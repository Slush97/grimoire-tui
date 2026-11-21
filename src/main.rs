//! Terminal frontend for Grimoire: Installed, Browse and Settings.
//!
//! `core` owns the one `Backend` on a worker thread, `gamebanana` and
//! `thumbs` do network work on theirs, and `app` runs the loop that draws
//! the pages. Pages only read snapshots and send jobs; see `pages`.

mod app;
mod config;
mod core;
mod gamebanana;
mod pages;
mod thumbs;
mod ui;

use std::io::{self, stdout};
use std::path::PathBuf;

use grimoire_core::backend::{ConfigPaths, GameState, Request};
use ratatui::crossterm::event::{DisableFocusChange, EnableFocusChange};
use ratatui::crossterm::execute;
use ratatui::DefaultTerminal;

use app::App;
use config::Config;
use thumbs::Thumbs;

const USAGE: &str = "\
Usage: grimoire-tui [options] [deadlock folder]

  [deadlock folder]      use and save this Deadlock install
  --config-dir <dir>     read config.toml and themes/ from <dir>
  --default-config       print the default config.toml
  -h, --help             show this
";

fn main() -> io::Result<()> {
    let mut game_path = None;
    let mut config_dir = None;
    let mut args = std::env::args_os().skip(1);
    while let Some(arg) = args.next() {
        match arg.to_str() {
            Some("-h" | "--help") => {
                print!("{USAGE}");
                return Ok(());
            }
            Some("--default-config") => {
                print!("{}", config::DEFAULT);
                return Ok(());
            }
            Some("--config-dir") => {
                let dir = args
                    .next()
                    .ok_or_else(|| io::Error::other("--config-dir needs a folder"))?;
                config_dir = Some(PathBuf::from(dir));
            }
            Some(flag) if flag.starts_with('-') => {
                return Err(io::Error::other(format!(
                    "unknown option {flag}\n\n{USAGE}"
                )));
            }
            _ => game_path = Some(PathBuf::from(arg)),
        }
    }

    let core_paths =
        ConfigPaths::for_user().ok_or_else(|| io::Error::other("no config directory"))?;
    let paths = match config_dir {
        Some(dir) => config::Paths::in_dir(&dir),
        None => config::Paths::for_user().ok_or_else(|| io::Error::other("no config directory"))?,
    };
    let mut problems = Vec::new();
    problems.extend(paths.ensure().err());
    let (config, problem) = Config::load(&paths);
    problems.extend(problem);
    let (mut core, problem) = core::Core::start(core_paths);
    problems.extend(problem);
    match game_path {
        Some(path) => core.request(Request::SetGamePath(path)),
        None if matches!(core.snapshot.game, GameState::NotSet) => {
            core.request(Request::DetectGamePath);
        }
        None => {}
    }

    let mut terminal = init_terminal();
    // Asks the terminal about images, so it has to come after raw mode.
    let thumbs = Thumbs::new(config.images, config.theme.bg);
    let problem = (!problems.is_empty()).then(|| problems.join(". "));
    let result = App::new(core, thumbs, config, paths, problem).run(&mut terminal);
    restore_terminal();
    result
}

pub fn init_terminal() -> DefaultTerminal {
    let terminal = ratatui::init();
    // Focus events let Installed rescan when you come back from elsewhere.
    // Terminals without them just never send one.
    let _ = execute!(stdout(), EnableFocusChange);
    terminal
}

pub fn restore_terminal() {
    let _ = execute!(stdout(), DisableFocusChange);
    ratatui::restore();
}
