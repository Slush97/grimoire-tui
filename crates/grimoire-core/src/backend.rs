//! The one place Grimoire writes anything to disk.
//!
//! Pages send a [`Request`]. The app's single worker thread owns the only
//! [`Backend`] and calls [`Backend::handle`] for each request in turn, then
//! sends every page a fresh [`Snapshot`], whether the request worked or not.
//! Every function in core that writes is `pub(crate)` and called only from
//! here, so the compiler keeps writes out of the UI.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use crate::game::{self, GameInstall};
use crate::gameinfo::{self, GameinfoStatus};
use crate::legacy::LegacySettings;
use crate::library::{self, mutate, Incoming, MetaKey, ModList};
use crate::settings::{Settings, SettingsChange};
use crate::{metadata, steam, Error, Result};

/// Where the TUI keeps its settings, and where the Electron client's data is.
#[derive(Debug, Clone)]
pub struct ConfigPaths {
    /// `~/.config/grimoire-tui`
    pub app: PathBuf,
    /// `~/.config/grimoire`, shared with the Electron client.
    pub legacy: PathBuf,
}

impl ConfigPaths {
    pub fn for_user() -> Option<Self> {
        let config = dirs::config_dir()?;
        Some(Self {
            app: config.join("grimoire-tui"),
            legacy: config.join("grimoire"),
        })
    }

    fn settings(&self) -> PathBuf {
        self.app.join("settings.json")
    }

    fn metadata(&self) -> PathBuf {
        self.legacy.join("mod-metadata.json")
    }

    fn journal(&self) -> PathBuf {
        self.app.join("reorder-journal.json")
    }
}

#[derive(Debug, Clone)]
pub enum Request {
    /// Re-read everything from disk.
    Rescan,
    // Mod files. Refused while Deadlock is running.
    Enable(MetaKey),
    Disable(MetaKey),
    SetGlobal(MetaKey, bool),
    /// Disable the first variant and give its slot to the second.
    SwapVariant(MetaKey, MetaKey),
    /// Every enabled non-Global mod, in the new order. Build it with
    /// [`library::enabled_order`] and friends, never from a partial list.
    Reorder(Vec<MetaKey>),
    Delete(Vec<MetaKey>),
    /// Copy VPKs into `.disabled/` with their metadata, then enable them if
    /// `enable` is set.
    Install {
        mods: Vec<Incoming>,
        enable: bool,
    },
    /// Clean up what an interrupted reorder left behind.
    Repair,
    // Game folder.
    SetGamePath(PathBuf),
    DetectGamePath,
    RepairGameinfo,
    // App settings.
    Settings(SettingsChange),
}

/// Everything the pages render from disk. A section a request didn't change
/// is the same `Arc` as in the previous snapshot, so pages skip work with
/// `Arc::ptr_eq`.
#[derive(Debug, Clone)]
pub struct Snapshot {
    pub settings: Arc<Settings>,
    pub game: GameState,
}

impl Snapshot {
    pub fn game(&self) -> Option<&Game> {
        match &self.game {
            GameState::Ready(game) => Some(game),
            _ => None,
        }
    }
}

#[derive(Debug, Clone)]
pub enum GameState {
    NotSet,
    /// The saved path stopped being a Deadlock install, like an unmounted drive.
    Invalid {
        path: PathBuf,
        reason: String,
    },
    Ready(Game),
}

/// Each section fails on its own, with a message for the user.
#[derive(Debug, Clone)]
pub struct Game {
    pub install: GameInstall,
    pub mods: Arc<Result<ModList, String>>,
    pub gameinfo: Arc<Result<GameinfoStatus, String>>,
}

impl Game {
    fn read(install: GameInstall, metadata: &Path) -> Self {
        Self {
            mods: Arc::new(read_mods(&install, metadata)),
            gameinfo: Arc::new(text(gameinfo::check(&install))),
            install,
        }
    }
}

#[derive(Debug)]
pub struct Backend {
    paths: ConfigPaths,
    settings: Arc<Settings>,
    /// Set when `settings.json` exists but won't parse. We run on defaults and
    /// refuse to overwrite the user's file.
    settings_error: Option<String>,
    game: GameState,
}

impl Backend {
    /// Reads everything, importing the Electron client's game path on the
    /// first run. Never fails outright: the error it returns is for the user
    /// to see once, and the backend still works.
    pub fn load(paths: ConfigPaths) -> (Self, Option<Error>) {
        let mut backend = Self {
            paths,
            settings: Arc::default(),
            settings_error: None,
            game: GameState::NotSet,
        };
        let problem = match Settings::load(&backend.paths.settings()) {
            Ok(Some(settings)) => {
                backend.settings = Arc::new(settings);
                backend.game = backend.open_game();
                None
            }
            Ok(None) => backend.import_legacy().err(),
            Err(err) => {
                backend.settings_error = Some(err.to_string());
                Some(err)
            }
        };
        (backend, problem)
    }

    pub fn snapshot(&self) -> Snapshot {
        Snapshot {
            settings: Arc::clone(&self.settings),
            game: self.game.clone(),
        }
    }

    pub fn handle(&mut self, request: Request) -> Result<()> {
        match request {
            Request::Rescan => {
                self.rescan();
                Ok(())
            }
            Request::Enable(key) => {
                self.change_mods(|install, meta, _| mutate::enable(install, meta, &key))
            }
            Request::Disable(key) => {
                self.change_mods(|install, meta, _| mutate::disable(install, meta, &key))
            }
            Request::SetGlobal(key, global) => {
                self.change_mods(|install, meta, _| mutate::set_global(install, meta, &key, global))
            }
            Request::Reorder(order) => self.change_mods(|install, meta, journal| {
                mutate::reorder(install, meta, journal, &order)
            }),
            Request::Delete(keys) => {
                self.change_mods(|install, meta, _| mutate::delete(install, meta, &keys))
            }
            Request::SwapVariant(off, on) => {
                self.change_mods(|install, meta, _| mutate::swap(install, meta, &off, &on))
            }
            Request::Install { mods, enable } => {
                self.change_mods(|install, meta, _| mutate::install(install, meta, &mods, enable))
            }
            Request::Repair => self.change_mods(mutate::repair),
            Request::SetGamePath(path) => self.set_game_path(path),
            Request::DetectGamePath => self.set_game_path(steam::detect()?),
            Request::RepairGameinfo => self.change_game(Self::refresh_gameinfo, |install, _, _| {
                gameinfo::repair(install)
            }),
            Request::Settings(change) => {
                let mut settings = (*self.settings).clone();
                settings.apply(change);
                self.save_settings(settings)
            }
        }
    }

    fn change_mods(
        &mut self,
        op: impl FnOnce(&GameInstall, &Path, &Path) -> Result<()>,
    ) -> Result<()> {
        if game::is_running() {
            return Err(Error::GameRunning);
        }
        self.change_game(Self::refresh_mods, op)
    }

    /// Runs `op` against the current install, then re-reads whatever it may
    /// have touched, even if it failed partway through.
    fn change_game(
        &mut self,
        refresh: fn(&mut Self),
        op: impl FnOnce(&GameInstall, &Path, &Path) -> Result<()>,
    ) -> Result<()> {
        let GameState::Ready(game) = &self.game else {
            return Err(Error::NoGame);
        };
        let result = op(&game.install, &self.paths.metadata(), &self.paths.journal());
        refresh(self);
        result
    }

    fn set_game_path(&mut self, path: PathBuf) -> Result<()> {
        // An invalid path is never saved.
        let install = GameInstall::open(&path)?;
        let mut settings = (*self.settings).clone();
        settings.game_path = Some(path);
        self.save_settings(settings)?;
        self.game = GameState::Ready(Game::read(install, &self.paths.metadata()));
        Ok(())
    }

    fn save_settings(&mut self, settings: Settings) -> Result<()> {
        if let Some(err) = &self.settings_error {
            return Err(Error::Refused(format!(
                "Grimoire couldn't read {}, so it won't overwrite it. Fix or delete that file. ({err})",
                self.paths.settings().display()
            )));
        }
        if settings != *self.settings {
            settings.save(&self.paths.settings())?;
            self.settings = Arc::new(settings);
        }
        Ok(())
    }

    /// First run: take the game path from the Electron client if present,
    /// then save so this only happens once.
    fn import_legacy(&mut self) -> Result<()> {
        // A legacy file that won't parse just means there's nothing to import.
        let legacy = LegacySettings::load(&self.paths.legacy).unwrap_or_default();
        let mut settings = Settings::default();
        settings.game_path = legacy.active_game_path().map(Path::to_owned);
        self.settings = Arc::new(settings);
        self.game = self.open_game();
        self.settings.save(&self.paths.settings())
    }

    fn open_game(&self) -> GameState {
        let Some(path) = &self.settings.game_path else {
            return GameState::NotSet;
        };
        match GameInstall::open(path) {
            Ok(install) => GameState::Ready(Game::read(install, &self.paths.metadata())),
            Err(err) => GameState::Invalid {
                path: path.clone(),
                reason: err.to_string(),
            },
        }
    }

    fn rescan(&mut self) {
        if matches!(self.game, GameState::Ready(_)) {
            self.refresh_mods();
        } else {
            self.game = self.open_game();
        }
    }

    /// Also rechecks `gameinfo.gi`, since enabling a mod can add an overflow
    /// root to it.
    fn refresh_mods(&mut self) {
        let metadata = self.paths.metadata();
        if let GameState::Ready(game) = &mut self.game {
            replace_if_changed(&mut game.mods, read_mods(&game.install, &metadata));
            replace_if_changed(&mut game.gameinfo, text(gameinfo::check(&game.install)));
        }
    }

    fn refresh_gameinfo(&mut self) {
        if let GameState::Ready(game) = &mut self.game {
            replace_if_changed(&mut game.gameinfo, text(gameinfo::check(&game.install)));
        }
    }
}

fn read_mods(install: &GameInstall, metadata: &Path) -> Result<ModList, String> {
    let metadata = metadata::load_map(metadata).map_err(|err| err.to_string())?;
    text(library::scan(install, &metadata))
}

fn text<T>(result: Result<T>) -> Result<T, String> {
    result.map_err(|err| err.to_string())
}

fn replace_if_changed<T: PartialEq>(slot: &mut Arc<T>, value: T) {
    if **slot != value {
        *slot = Arc::new(value);
    }
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::*;

    struct Fixture {
        _dir: tempfile::TempDir,
        paths: ConfigPaths,
        game: PathBuf,
    }

    fn fixture() -> Fixture {
        let dir = tempfile::tempdir().unwrap();
        let game = dir.path().join("Deadlock");
        let citadel = game.join("game/citadel");
        fs::create_dir_all(citadel.join("addons")).unwrap();
        fs::write(citadel.join("gameinfo.gi"), "").unwrap();
        fs::write(citadel.join("addons/pak01_dir.vpk"), "").unwrap();
        fs::write(citadel.join("addons/pak02_dir.vpk"), "").unwrap();

        let paths = ConfigPaths {
            app: dir.path().join("config/grimoire-tui"),
            legacy: dir.path().join("config/grimoire"),
        };
        fs::create_dir_all(&paths.legacy).unwrap();
        Fixture {
            _dir: dir,
            paths,
            game,
        }
    }

    #[test]
    fn first_run_imports_the_legacy_path_once() {
        let fx = fixture();
        fs::write(
            fx.paths.legacy.join("settings.json"),
            format!(r#"{{"deadlockPath":{:?}}}"#, fx.game),
        )
        .unwrap();

        let (backend, problem) = Backend::load(fx.paths.clone());

        assert!(problem.is_none());
        let snapshot = backend.snapshot();
        assert!(snapshot.game().is_some());
        let saved = Settings::load(&fx.paths.settings()).unwrap().unwrap();
        assert_eq!(saved, *snapshot.settings);
    }

    #[test]
    fn an_invalid_game_path_is_never_saved() {
        let fx = fixture();
        let (mut backend, _) = Backend::load(fx.paths.clone());

        let result = backend.handle(Request::SetGamePath(fx.paths.app.clone()));

        assert!(matches!(result, Err(Error::NotAGameInstall(_))));
        assert!(matches!(backend.snapshot().game, GameState::NotSet));
        assert_eq!(
            Settings::load(&fx.paths.settings())
                .unwrap()
                .unwrap()
                .game_path,
            None
        );
    }

    #[test]
    fn setting_the_game_path_scans_it() {
        let fx = fixture();
        let (mut backend, _) = Backend::load(fx.paths.clone());

        backend
            .handle(Request::SetGamePath(fx.game.clone()))
            .unwrap();

        let snapshot = backend.snapshot();
        let mods = snapshot.game().unwrap().mods.as_ref().as_ref().unwrap();
        assert_eq!(mods.mods.len(), 2);
    }

    #[test]
    fn unchanged_sections_keep_the_same_arc() {
        let fx = fixture();
        let (mut backend, _) = Backend::load(fx.paths.clone());
        backend
            .handle(Request::SetGamePath(fx.game.clone()))
            .unwrap();
        let before = backend.snapshot();

        backend.handle(Request::Rescan).unwrap();
        backend
            .handle(Request::Settings(SettingsChange::ShowNsfw(true)))
            .unwrap();

        let after = backend.snapshot();
        assert!(Arc::ptr_eq(
            &before.game().unwrap().mods,
            &after.game().unwrap().mods
        ));
        assert!(!Arc::ptr_eq(&before.settings, &after.settings));
    }

    #[test]
    fn corrupt_settings_are_reported_and_never_overwritten() {
        let fx = fixture();
        fs::create_dir_all(&fx.paths.app).unwrap();
        fs::write(fx.paths.settings(), "{ nope").unwrap();

        let (mut backend, problem) = Backend::load(fx.paths.clone());

        assert!(matches!(problem, Some(Error::Json { .. })));
        let result = backend.handle(Request::Settings(SettingsChange::ShowNsfw(true)));
        assert!(matches!(result, Err(Error::Refused(_))));
        assert_eq!(fs::read_to_string(fx.paths.settings()).unwrap(), "{ nope");
    }

    #[test]
    fn mod_requests_need_a_game() {
        let fx = fixture();
        let (mut backend, _) = Backend::load(fx.paths.clone());
        let result = backend.handle(Request::Enable(MetaKey::for_path(Path::new(
            "pak01_dir.vpk",
        ))));
        assert!(matches!(result, Err(Error::NoGame | Error::GameRunning)));
    }

    #[test]
    fn enable_and_disable_round_trip_through_handle() {
        let fx = fixture();
        let citadel = fx.game.join("game/citadel");
        fs::create_dir_all(citadel.join("addons/.disabled")).unwrap();
        fs::write(citadel.join("addons/.disabled/cool_dir.vpk"), "").unwrap();
        let (mut backend, _) = Backend::load(fx.paths.clone());
        backend
            .handle(Request::SetGamePath(fx.game.clone()))
            .unwrap();

        backend
            .handle(Request::Enable(MetaKey::for_path(Path::new(
                "cool_dir.vpk",
            ))))
            .unwrap();
        assert!(citadel.join("addons/pak03_dir.vpk").exists());

        backend
            .handle(Request::Disable(MetaKey::for_path(Path::new(
                "pak03_dir.vpk",
            ))))
            .unwrap();
        assert!(citadel.join("addons/.disabled/mod_dir.vpk").exists());
        let snapshot = backend.snapshot();
        let mods = snapshot.game().unwrap().mods.as_ref().as_ref().unwrap();
        let meta = mods
            .mods
            .iter()
            .find(|m| m.key.as_str() == "mod_dir.vpk")
            .unwrap()
            .metadata
            .as_ref()
            .unwrap();
        assert_eq!(meta.last_priority, Some(3));
    }
}
