//! Everything Grimoire knows about Deadlock mods, with no UI attached.
//!
//! Frontends render the [`backend::Snapshot`]s this crate produces and send
//! user intent back as [`backend::Request`]s.

pub mod backend;
pub mod error;
mod fsx;
pub mod game;
pub mod gameinfo;
pub mod legacy;
pub mod library;
pub mod metadata;
pub mod settings;
mod steam;

pub use error::{Error, Result};
