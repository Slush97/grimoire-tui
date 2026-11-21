//! Mod thumbnails. A background thread fetches them from GameBanana's image
//! server (or the disk cache), decodes them and encodes them for the
//! terminal at the size they'll be drawn, so drawing one is only copying
//! cells and the UI never waits on an image.

use std::collections::{HashMap, HashSet, VecDeque};
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use base64::Engine as _;
use image::imageops::FilterType;
use image::Rgba;
use ratatui::layout::{Rect, Size};
use ratatui::style::Color;
use ratatui::text::Line;
use ratatui::Frame;
use ratatui_image::picker::{Picker, ProtocolType};
use ratatui_image::protocol::Protocol;
use ratatui_image::{Image, Resize};

use crate::config::theme::Theme;
use crate::config::ImageProtocol;

/// Encoded thumbnails kept in memory.
const KEEP: usize = 96;
/// Thumbnails kept on disk, newest first.
const KEEP_ON_DISK: usize = 600;
const WORKERS: usize = 4;
const MAX_BYTES: u64 = 16 << 20;
const USER_AGENT: &str = concat!(
    "Grimoire-TUI/",
    env!("CARGO_PKG_VERSION"),
    " (Deadlock mod manager; +https://grimoiremods.com)"
);
static TEMP_SEQ: AtomicU64 = AtomicU64::new(0);

pub struct Thumbs {
    protocol: ImageProtocol,
    background: Option<Rgba<u8>>,
    /// What the terminal said it can do. Asked once, on first need.
    queried: Option<Picker>,
    picker: Option<Picker>,
    slots: HashMap<String, Slot>,
    /// Least recently drawn first.
    order: VecDeque<String>,
    jobs: Sender<Load>,
    loaded: Receiver<Loaded>,
    wanted: Arc<Mutex<HashSet<String>>>,
    requested: Vec<String>,
    /// Images drawn this frame and the last, as url and where.
    drawn: Vec<(String, Rect)>,
    last_drawn: Vec<(String, Rect)>,
}

enum Slot {
    Pending(Size),
    Ready(Size, Protocol),
    Failed,
}

struct Load {
    url: String,
    size: Size,
    picker: Picker,
}

struct Loaded {
    url: String,
    size: Size,
    /// `None` when the load was dropped from the queue.
    result: Option<Result<Protocol, String>>,
}

impl Thumbs {
    /// This may ask the terminal about itself, so call it once the terminal
    /// is in raw mode and before reading any events.
    pub fn new(protocol: ImageProtocol, background: Color) -> Self {
        let (jobs, inbox) = mpsc::channel();
        let (outbox, loaded) = mpsc::channel();
        let wanted = Arc::new(Mutex::new(HashSet::new()));
        let inbox = Arc::new(Mutex::new(inbox));
        for worker in 0..WORKERS {
            let inbox = Arc::clone(&inbox);
            let outbox = outbox.clone();
            let wanted = Arc::clone(&wanted);
            thread::spawn(move || load_forever(&inbox, &outbox, &wanted, worker == 0));
        }
        let mut thumbs = Self {
            protocol,
            background: rgba(background),
            queried: None,
            picker: None,
            slots: HashMap::new(),
            order: VecDeque::new(),
            jobs,
            loaded,
            wanted,
            requested: Vec::new(),
            drawn: Vec::new(),
            last_drawn: Vec::new(),
        };
        thumbs.rebuild();
        thumbs
    }

    pub fn set_protocol(&mut self, protocol: ImageProtocol) {
        if protocol != self.protocol {
            self.protocol = protocol;
            self.rebuild();
        }
    }

    /// The theme's background, to fill around an image that doesn't fill
    /// its box. Without it the gaps draw black.
    pub fn set_background(&mut self, color: Color) {
        let background = rgba(color);
        if background != self.background {
            self.background = background;
            self.rebuild();
        }
    }

    fn rebuild(&mut self) {
        let forced = match self.protocol {
            ImageProtocol::Off => None,
            ImageProtocol::Auto if speaks_graphics() => Some(self.query().protocol_type()),
            ImageProtocol::Auto | ImageProtocol::Halfblocks => Some(ProtocolType::Halfblocks),
            ImageProtocol::Kitty => Some(ProtocolType::Kitty),
            ImageProtocol::Sixel => Some(ProtocolType::Sixel),
            ImageProtocol::Iterm2 => Some(ProtocolType::Iterm2),
        };
        self.picker = forced.map(|forced| {
            let mut picker = if forced == ProtocolType::Halfblocks {
                Picker::halfblocks()
            } else {
                self.query().clone()
            };
            picker.set_protocol_type(forced);
            picker.set_background_color(self.background);
            picker
        });
        self.forget();
    }

    /// Asks the terminal what it draws and its cell size, once.
    fn query(&mut self) -> &Picker {
        self.queried.get_or_insert_with(|| {
            Picker::from_query_stdio().unwrap_or_else(|_| Picker::halfblocks())
        })
    }

    /// What's drawing images right now, for Settings.
    pub fn describe(&self) -> &'static str {
        match self.picker.as_ref().map(Picker::protocol_type) {
            None => "off",
            Some(ProtocolType::Kitty) => "kitty",
            Some(ProtocolType::Sixel) => "sixel",
            Some(ProtocolType::Iterm2) => "iterm2",
            Some(ProtocolType::Halfblocks) => "halfblocks",
        }
    }

    pub fn enabled(&self) -> bool {
        self.picker.is_some()
    }

    /// Drops every encoded image, for when the terminal lost them (after
    /// handing it to an editor, say).
    pub fn forget(&mut self) {
        self.slots.clear();
        self.order.clear();
        self.requested.clear();
        if let Ok(mut wanted) = self.wanted.lock() {
            wanted.clear();
        }
    }

    /// Takes in finished loads. True when something new can be drawn.
    pub fn poll(&mut self) -> bool {
        let mut changed = false;
        while let Ok(loaded) = self.loaded.try_recv() {
            let still_wanted = matches!(
                self.slots.get(&loaded.url),
                Some(Slot::Pending(size)) if *size == loaded.size
            );
            if !still_wanted {
                continue;
            }
            match loaded.result {
                Some(Ok(protocol)) => {
                    self.slots
                        .insert(loaded.url, Slot::Ready(loaded.size, protocol));
                }
                Some(Err(_)) => {
                    self.slots.insert(loaded.url, Slot::Failed);
                }
                None => {
                    self.slots.remove(&loaded.url);
                }
            }
            changed = true;
        }
        changed
    }

    /// Rows a `width` wide thumbnail takes, for GameBanana's 16:9 previews.
    pub fn rows_for(&self, width: u16) -> u16 {
        let Some(picker) = &self.picker else {
            return 0;
        };
        let font = picker.font_size();
        let pixels = u32::from(width) * u32::from(font.width) * 9 / 16;
        let rows = pixels.div_ceil(u32::from(font.height.max(1)));
        u16::try_from(rows).unwrap_or(u16::MAX)
    }

    /// Draws the thumbnail at `url` centered in `area`, or asks for it.
    pub fn draw(&mut self, frame: &mut Frame, area: Rect, url: &str, theme: &Theme) {
        let Some(picker) = &self.picker else {
            return;
        };
        if area.is_empty() {
            return;
        }
        if !self.requested.iter().any(|requested| requested == url) {
            self.requested.push(url.to_owned());
            if let Ok(mut wanted) = self.wanted.lock() {
                wanted.insert(url.to_owned());
            }
        }
        let size = area.as_size();
        match self.slots.get_mut(url) {
            Some(Slot::Ready(ready, protocol)) if *ready == size => {
                let drawn = protocol.size();
                let x = area.x + (area.width.saturating_sub(drawn.width)) / 2;
                let y = area.y + (area.height.saturating_sub(drawn.height)) / 2;
                let at = Rect::new(x, y, drawn.width, drawn.height).intersection(area);
                frame.render_widget(Image::new(protocol), at);
                self.drawn.push((url.to_owned(), at));
                self.touch(url);
                return;
            }
            Some(Slot::Failed) => return,
            Some(Slot::Pending(pending)) if *pending == size => {}
            _ => {
                let load = Load {
                    url: url.to_owned(),
                    size,
                    picker: picker.clone(),
                };
                if self.jobs.send(load).is_ok() {
                    self.slots.insert(url.to_owned(), Slot::Pending(size));
                    self.touch(url);
                }
            }
        }
        let dots = Line::from("\u{b7} \u{b7} \u{b7}")
            .style(theme.faint())
            .centered();
        let middle = Rect::new(area.x, area.y + area.height / 2, area.width, 1);
        frame.render_widget(dots, middle);
    }

    /// Whether the frame just drawn changed which images are on screen.
    /// kitty only shows an image's placeholder cells reliably once the
    /// whole screen is redrawn, so the app redraws everything after one.
    pub fn changed_on_screen(&mut self) -> bool {
        let changed = self.drawn != self.last_drawn;
        self.last_drawn = std::mem::take(&mut self.drawn);
        if let Ok(mut wanted) = self.wanted.lock() {
            *wanted = self.requested.drain(..).collect();
        } else {
            self.requested.clear();
        }
        let kitty = self
            .picker
            .as_ref()
            .is_some_and(|picker| picker.protocol_type() == ProtocolType::Kitty);
        changed && kitty
    }

    pub fn loading(&self) -> bool {
        self.slots
            .values()
            .any(|slot| matches!(slot, Slot::Pending(_)))
    }

    fn touch(&mut self, url: &str) {
        if let Some(at) = self.order.iter().position(|u| u == url) {
            self.order.remove(at);
        }
        self.order.push_back(url.to_owned());
        while self.order.len() > KEEP {
            if let Some(old) = self.order.pop_front() {
                self.slots.remove(&old);
            }
        }
    }
}

/// Whether the terminal says in its environment that it draws images.
/// Those answer the capability query right away. Anything else, and tmux
/// (which needs passthrough that may be off), might never answer, and an
/// unanswered query stalls startup and swallows the next key press.
fn rgba(color: Color) -> Option<Rgba<u8>> {
    match color {
        Color::Rgb(r, g, b) => Some(Rgba([r, g, b, 255])),
        _ => None,
    }
}

fn speaks_graphics() -> bool {
    let var = |name: &str| std::env::var(name).unwrap_or_default();
    let term = var("TERM");
    let program = var("TERM_PROGRAM");
    var("TMUX").is_empty()
        && (!var("KITTY_WINDOW_ID").is_empty()
            || term == "xterm-kitty"
            || term == "xterm-ghostty"
            || term.starts_with("foot")
            || program.eq_ignore_ascii_case("ghostty")
            || program == "WezTerm"
            || program == "iTerm.app")
}

fn load_forever(
    inbox: &Mutex<Receiver<Load>>,
    outbox: &Sender<Loaded>,
    wanted: &Mutex<HashSet<String>>,
    prune_cache: bool,
) {
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .user_agent(USER_AGENT)
        .timeout_global(Some(Duration::from_secs(20)))
        .build()
        .into();
    let cache = dirs::cache_dir().map(|dir| dir.join("grimoire-tui").join("thumbnails"));
    if prune_cache {
        if let Some(cache) = &cache {
            prune(cache);
        }
    }
    loop {
        let load = match inbox.lock() {
            Ok(inbox) => match inbox.recv() {
                Ok(load) => load,
                Err(_) => return,
            },
            Err(_) => return,
        };
        if !is_wanted(&load.url, wanted) {
            if outbox
                .send(Loaded {
                    url: load.url,
                    size: load.size,
                    result: None,
                })
                .is_err()
            {
                return;
            }
            continue;
        }
        let result = bytes(&agent, cache.as_deref(), &load.url).and_then(|bytes| {
            let image = image::load_from_memory(&bytes).map_err(|err| err.to_string())?;
            load.picker
                .new_protocol(image, load.size, Resize::Fit(Some(FilterType::Triangle)))
                .map_err(|err| err.to_string())
        });
        let loaded = Loaded {
            url: load.url,
            size: load.size,
            result: Some(result),
        };
        if outbox.send(loaded).is_err() {
            return;
        }
    }
}

fn is_wanted(url: &str, wanted: &Mutex<HashSet<String>>) -> bool {
    wanted.lock().is_ok_and(|urls| urls.contains(url))
}

/// The image's bytes: inline for `data:` URLs (the Electron client stores
/// local mods' thumbnails that way), else from the cache or the network.
fn bytes(agent: &ureq::Agent, cache: Option<&Path>, url: &str) -> Result<Vec<u8>, String> {
    if let Some(data) = url.strip_prefix("data:") {
        let (_, encoded) = data.split_once(";base64,").ok_or("not base64 data")?;
        return base64::engine::general_purpose::STANDARD
            .decode(encoded.trim())
            .map_err(|err| err.to_string());
    }
    let cached = cache.map(|dir| dir.join(cache_name(url)));
    if let Some(bytes) = cached.as_ref().and_then(|path| fs::read(path).ok()) {
        return Ok(bytes);
    }
    let mut bytes = Vec::new();
    agent
        .get(url)
        .call()
        .map_err(|err| err.to_string())?
        .body_mut()
        .as_reader()
        .take(MAX_BYTES)
        .read_to_end(&mut bytes)
        .map_err(|err| err.to_string())?;
    if let Some(path) = cached {
        // A cache that can't be written only costs a refetch next time.
        let _ = save(&path, &bytes);
    }
    Ok(bytes)
}

fn save(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir)?;
    }
    let name = path.file_name().unwrap_or_default().to_string_lossy();
    let seq = TEMP_SEQ.fetch_add(1, Ordering::Relaxed);
    let temp = path.with_file_name(format!(".{name}.{}.{seq}.part", std::process::id()));
    if let Err(err) = fs::write(&temp, bytes) {
        let _ = fs::remove_file(&temp);
        return Err(err);
    }
    if let Err(err) = fs::rename(&temp, path) {
        let _ = fs::remove_file(&temp);
        return Err(err);
    }
    Ok(())
}

/// GameBanana image names are unique, so the URL itself, made file-safe,
/// is the key.
fn cache_name(url: &str) -> String {
    let rest = url.split_once("://").map_or(url, |(_, rest)| rest);
    let name: String = rest
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_') {
                c
            } else {
                '_'
            }
        })
        .collect();
    let keep = name.len().saturating_sub(150);
    name[keep..].to_owned()
}

fn prune(dir: &Path) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    let mut files: Vec<(std::time::SystemTime, PathBuf)> = entries
        .flatten()
        .filter(|entry| entry.path().extension().is_none_or(|ext| ext != "part"))
        .filter_map(|entry| Some((entry.metadata().ok()?.modified().ok()?, entry.path())))
        .collect();
    if files.len() <= KEEP_ON_DISK {
        return;
    }
    files.sort_by_key(|(modified, _)| std::cmp::Reverse(*modified));
    for (_, path) in files.drain(KEEP_ON_DISK..) {
        let _ = fs::remove_file(path);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cache_names_are_file_safe() {
        assert_eq!(
            cache_name("https://images.gamebanana.com/img/ss/mods/530-90_68c1b.jpg"),
            "images.gamebanana.com_img_ss_mods_530-90_68c1b.jpg"
        );
        assert!(cache_name(&format!("https://x/{}", "a".repeat(400))).len() <= 150);
    }

    #[test]
    fn data_urls_decode_without_the_network() {
        let agent = ureq::Agent::new_with_defaults();
        let bytes = bytes(&agent, None, "data:image/png;base64,aGk=").unwrap();
        assert_eq!(bytes, b"hi");
    }

    #[test]
    fn queued_loads_only_survive_while_their_urls_are_on_screen() {
        let wanted = Mutex::new(HashSet::from(["visible".to_owned()]));
        assert!(is_wanted("visible", &wanted));
        assert!(!is_wanted("scrolled-away", &wanted));
    }
}
