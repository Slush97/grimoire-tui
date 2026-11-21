//! The one [`Backend`], on its own thread.
//!
//! Every write goes through here as a [`Job`], and jobs run one at a time in
//! the order they were sent, so mod files never race. The UI keeps drawing
//! while a big install copies, and picks up the new snapshot when it lands.

use std::sync::mpsc::{self, Receiver, Sender};
use std::thread;

use grimoire_core::backend::{Backend, ConfigPaths, Request, Snapshot};
use grimoire_core::library::InstalledMod;
use tempfile::TempDir;

/// Requests that belong together. They run in order and stop at the first
/// failure.
pub struct Job {
    requests: Vec<Request>,
    /// Shown when every request worked.
    done: Option<String>,
    /// Kept alive until the job is over, like the download an install reads.
    hold: Option<TempDir>,
}

impl Job {
    pub fn new(request: Request) -> Self {
        Self::all(vec![request])
    }

    pub fn all(requests: Vec<Request>) -> Self {
        Self {
            requests,
            done: None,
            hold: None,
        }
    }

    pub fn done(mut self, message: impl Into<String>) -> Self {
        self.done = Some(message.into());
        self
    }

    pub fn hold(mut self, dir: TempDir) -> Self {
        self.hold = Some(dir);
        self
    }
}

/// What a finished job tells the UI.
pub enum Outcome {
    Done(Option<String>),
    Failed(String),
}

struct Finished {
    snapshot: Snapshot,
    outcome: Outcome,
}

pub struct Core {
    jobs: Sender<Job>,
    finished: Receiver<Finished>,
    pub snapshot: Snapshot,
    running: usize,
}

impl Core {
    /// Loads the backend here, so the first frame already has the mod list.
    /// The error is a problem to show once; the backend works regardless.
    pub fn start(paths: ConfigPaths) -> (Self, Option<String>) {
        let (backend, problem) = Backend::load(paths);
        let snapshot = backend.snapshot();
        let (jobs, inbox) = mpsc::channel::<Job>();
        let (outbox, finished) = mpsc::channel();
        thread::spawn(move || {
            let mut backend = backend;
            for job in inbox {
                let mut outcome = Outcome::Done(job.done);
                for request in job.requests {
                    if let Err(err) = backend.handle(request) {
                        outcome = Outcome::Failed(err.to_string());
                        break;
                    }
                }
                drop(job.hold);
                let snapshot = backend.snapshot();
                if outbox.send(Finished { snapshot, outcome }).is_err() {
                    break;
                }
            }
        });
        let core = Self {
            jobs,
            finished,
            snapshot,
            running: 0,
        };
        (core, problem.map(|err| err.to_string()))
    }

    pub fn send(&mut self, job: Job) {
        if self.jobs.send(job).is_ok() {
            self.running += 1;
        }
    }

    pub fn request(&mut self, request: Request) {
        self.send(Job::new(request));
    }

    /// Takes in finished jobs; the latest snapshot wins.
    pub fn poll(&mut self) -> Vec<Outcome> {
        let mut outcomes = Vec::new();
        while let Ok(finished) = self.finished.try_recv() {
            self.running = self.running.saturating_sub(1);
            self.snapshot = finished.snapshot;
            outcomes.push(finished.outcome);
        }
        outcomes
    }

    pub fn busy(&self) -> bool {
        self.running > 0
    }

    pub fn mods(&self) -> &[InstalledMod] {
        match self.snapshot.game().map(|game| &*game.mods) {
            Some(Ok(list)) => &list.mods,
            _ => &[],
        }
    }
}
