//! GameBanana: listing, details, categories, and downloading a file to
//! unpack. Nothing is sent until the user opens Browse.
//!
//! API calls run on one thread, downloads on another so a big download
//! doesn't hold up browsing. Both share one gate that keeps requests a
//! second apart.

mod api;
mod archive;
pub mod html;

use std::fs::File;
use std::io::{Read, Write};
use std::path::Path;
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex, PoisonError};
use std::thread;
use std::time::{Duration, Instant};

use grimoire_core::library::Incoming;
use grimoire_core::metadata::ModMetadata;
use serde::de::DeserializeOwned;
use tempfile::TempDir;

pub use api::{Category, Details, ModFile, Sort, Submission};
use api::{API, DEADLOCK, DETAIL_FIELDS, LIST_FIELDS};

pub const PER_PAGE: usize = 20;

const USER_AGENT: &str = concat!(
    "Grimoire-TUI/",
    env!("CARGO_PKG_VERSION"),
    " (Deadlock mod manager; +https://grimoiremods.com)"
);
/// GameBanana bans clients that hammer it. One request a second is its
/// rule of thumb.
const MIN_GAP: Duration = Duration::from_secs(1);

/// What a listing asks for. Browse compares it with the reply's, so a reply
/// to a search the user already changed is dropped.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Query {
    /// Empty for the plain feed.
    pub text: String,
    pub sort: Sort,
    pub category: Option<u64>,
}

pub enum Job {
    List {
        query: Query,
        page: u32,
    },
    Categories,
    Details(Submission),
    Download {
        submission: Submission,
        details: Details,
        file: ModFile,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JobKind {
    List,
    Categories,
    Details,
    Download,
}

pub enum Reply {
    List {
        query: Query,
        page: u32,
        records: Vec<Submission>,
        last: bool,
    },
    Categories(Vec<Category>),
    Details {
        submission: Submission,
        details: Details,
    },
    Progress {
        done: u64,
        total: u64,
    },
    /// The VPKs live in `dir`, which is deleted when it's dropped.
    Downloaded {
        name: String,
        mods: Vec<Incoming>,
        dir: TempDir,
    },
    Failed {
        kind: JobKind,
        message: String,
    },
}

type Gate = Arc<Mutex<Option<Instant>>>;

pub struct Client {
    api: Sender<Job>,
    downloads: Sender<Job>,
    replies: Receiver<Reply>,
}

impl Client {
    pub fn spawn() -> Self {
        let (outbox, replies) = mpsc::channel();
        let gate = Gate::default();
        Self {
            api: Worker::spawn(Arc::clone(&gate), outbox.clone()),
            downloads: Worker::spawn(gate, outbox),
            replies,
        }
    }

    pub fn send(&self, job: Job) {
        let queue = match job {
            Job::Download { .. } => &self.downloads,
            _ => &self.api,
        };
        // Only fails once the worker is gone, and then there's nobody to tell.
        let _ = queue.send(job);
    }

    pub fn poll(&self) -> Vec<Reply> {
        self.replies.try_iter().collect()
    }
}

struct Worker {
    agent: ureq::Agent,
    gate: Gate,
    outbox: Sender<Reply>,
}

impl Worker {
    fn spawn(gate: Gate, outbox: Sender<Reply>) -> Sender<Job> {
        let (jobs, inbox) = mpsc::channel::<Job>();
        thread::spawn(move || {
            let mut worker = Self {
                agent: ureq::Agent::config_builder()
                    .user_agent(USER_AGENT)
                    .timeout_connect(Some(Duration::from_secs(15)))
                    .build()
                    .into(),
                gate,
                outbox,
            };
            for job in inbox {
                let kind = match job {
                    Job::List { .. } => JobKind::List,
                    Job::Categories => JobKind::Categories,
                    Job::Details(_) => JobKind::Details,
                    Job::Download { .. } => JobKind::Download,
                };
                let reply = worker
                    .handle(job)
                    .unwrap_or_else(|message| Reply::Failed { kind, message });
                if worker.outbox.send(reply).is_err() {
                    break;
                }
            }
        });
        jobs
    }

    fn handle(&mut self, job: Job) -> Result<Reply, String> {
        match job {
            Job::List { query, page } => {
                // Search ignores _sSort and always ranks by best match.
                let request = if query.text.is_empty() {
                    self.agent
                        .get(format!("{API}/Mod/Index"))
                        .query("_sSort", query.sort.token())
                } else {
                    self.agent
                        .get(format!("{API}/Util/Search/Results"))
                        .query("_sSearchString", &query.text)
                        .query("_sModelName", "Mod")
                        .query("_aFilters[itemtype]", "Mod")
                };
                let request = match query.category {
                    Some(id) => request.query("_aFilters[Generic_Category]", id.to_string()),
                    None => request,
                };
                let request = request
                    .query("_idGameRow", DEADLOCK)
                    .query("_aFilters[Generic_Game]", DEADLOCK)
                    .query("_nPerpage", PER_PAGE.to_string())
                    .query("_nPage", page.to_string())
                    .query("_csvProperties", LIST_FIELDS);
                let result: api::Page = self.json(request)?;
                Ok(Reply::List {
                    query,
                    page,
                    last: result.meta.complete || result.records.len() < PER_PAGE,
                    records: result.records,
                })
            }
            Job::Categories => {
                let request = self
                    .agent
                    .get(format!("{API}/Util/ModCategory/NestedStructure"))
                    .query("_idGameRow", DEADLOCK);
                let tree: serde_json::Value = self.json(request)?;
                let mut tree: Vec<Category> = api::lenient_list(tree)
                    .map_err(|err: serde_json::Error| format!("GameBanana: {err}"))?;
                tree.sort_by_key(|category| std::cmp::Reverse(category.items));
                Ok(Reply::Categories(tree))
            }
            Job::Details(submission) => {
                let request = self
                    .agent
                    .get(format!("{API}/Mod/{}", submission.id))
                    .query("_csvProperties", DETAIL_FIELDS);
                let details = self.json(request)?;
                Ok(Reply::Details {
                    submission,
                    details,
                })
            }
            Job::Download {
                submission,
                details,
                file,
            } => self.download(&submission, &details, &file),
        }
    }

    fn json<T: DeserializeOwned>(
        &self,
        request: ureq::RequestBuilder<ureq::typestate::WithoutBody>,
    ) -> Result<T, String> {
        self.wait_turn();
        request
            .config()
            .timeout_global(Some(Duration::from_secs(30)))
            .build()
            .call()
            .map_err(|err| format!("GameBanana: {err}"))?
            .body_mut()
            .read_json()
            .map_err(|err| format!("GameBanana sent something unexpected: {err}"))
    }

    fn wait_turn(&self) {
        let mut last = self.gate.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(rest) = last.and_then(|last| MIN_GAP.checked_sub(last.elapsed())) {
            thread::sleep(rest);
        }
        *last = Some(Instant::now());
    }

    fn download(
        &self,
        submission: &Submission,
        details: &Details,
        file: &ModFile,
    ) -> Result<Reply, String> {
        let failed =
            |err: &dyn std::fmt::Display| format!("Download of {} failed: {err}", file.name);
        let dir = tempfile::Builder::new()
            .prefix("grimoire-tui-")
            .tempdir()
            .map_err(|err| failed(&err))?;
        let saved = dir.path().join("download");

        self.wait_turn();
        let mut response = self
            .agent
            .get(&file.url)
            .config()
            .timeout_recv_body(Some(Duration::from_secs(600)))
            .build()
            .call()
            .map_err(|err| failed(&err))?;
        let total = response.body().content_length().unwrap_or(file.size);
        let mut body = response.body_mut().as_reader();
        let mut out = File::create(&saved).map_err(|err| failed(&err))?;
        let mut buf = vec![0; 1 << 16];
        let (mut done, mut reported) = (0, 0);
        let _ = self.outbox.send(Reply::Progress { done, total });
        loop {
            let read = body.read(&mut buf).map_err(|err| failed(&err))?;
            if read == 0 {
                break;
            }
            out.write_all(&buf[..read]).map_err(|err| failed(&err))?;
            done += read as u64;
            if done - reported >= 1 << 19 {
                reported = done;
                let _ = self.outbox.send(Reply::Progress { done, total });
            }
        }
        drop(out);

        let unpacked = dir.path().join("unpacked");
        let vpks = archive::vpks(&saved, &file.name, &unpacked)?;
        if vpks.is_empty() {
            return Err(format!("{} has no VPK files in it", file.name));
        }

        let description = file.description.trim();
        let base = ModMetadata {
            mod_name: Some(submission.name.clone()),
            author: Some(submission.author().to_owned()).filter(|a| !a.is_empty()),
            game_banana_id: Some(submission.id),
            game_banana_file_id: Some(file.id),
            category_name: details.category().map(str::to_owned),
            nsfw: details.nsfw.unwrap_or_else(|| submission.nsfw()),
            thumbnail_url: submission.thumbnail(),
            source_file_name: Some(stem(&file.name).to_owned()),
            file_description: Some(description.to_owned()).filter(|d| !d.is_empty()),
            ..ModMetadata::default()
        };
        let several = vpks.len() > 1;
        let mods = vpks
            .into_iter()
            .map(|vpk| {
                let mut metadata = base.clone();
                if several {
                    metadata.variant_label = Some(variant_label(&vpk, &unpacked));
                }
                Incoming { vpk, metadata }
            })
            .collect();
        Ok(Reply::Downloaded {
            name: submission.name.clone(),
            mods,
            dir,
        })
    }
}

fn stem(file: &str) -> &str {
    file.rsplit_once('.').map_or(file, |(stem, _)| stem)
}

/// For an archive with several VPKs: the folder a VPK sits in, which is how
/// authors name variants, else its own name.
fn variant_label(vpk: &Path, root: &Path) -> String {
    if let Some(folder) = vpk.parent().filter(|parent| *parent != root) {
        folder
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .into_owned()
    } else {
        let name = vpk.file_name().unwrap_or_default().to_string_lossy();
        let lower = name.to_ascii_lowercase();
        lower
            .strip_suffix("_dir.vpk")
            .map_or(name.to_string(), |s| name[..s.len()].to_owned())
    }
}
