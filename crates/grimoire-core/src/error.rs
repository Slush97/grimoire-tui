use std::io;
use std::path::PathBuf;

pub type Result<T, E = Error> = std::result::Result<T, E>;

/// Every message here can end up in a toast, so write it for the user.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("{0} is not a Deadlock install (no game/citadel/gameinfo.gi)")]
    NotAGameInstall(PathBuf),

    #[error("Set your Deadlock folder in Settings first")]
    NoGame,

    #[error("Close Deadlock first. Mods can't move while the game has them open")]
    GameRunning,

    /// A request that made sense for the page but not for the disk as it is
    /// now, like a full root or a mod that moved since the last scan.
    #[error("{0}")]
    Refused(String),

    #[error("could not read {path}: {source}")]
    Io { path: PathBuf, source: io::Error },

    #[error("could not parse {path}: {source}")]
    Json {
        path: PathBuf,
        source: serde_json::Error,
    },
}

impl Error {
    pub(crate) fn io(path: impl Into<PathBuf>) -> impl FnOnce(io::Error) -> Self {
        let path = path.into();
        move |source| Self::Io { path, source }
    }
}
