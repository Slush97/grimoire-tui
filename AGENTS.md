# Grimoire TUI

Ratatui terminal frontend for the Grimoire Deadlock mod manager. Linux first. All mod logic and every write to the game folder lives in `crates/grimoire-core`.

## Commands

```bash
cargo run                              # launches the TUI
cargo run -- --help                    # flags
cargo test --workspace                 # unit tests for TUI and core
cargo clippy --workspace --all-targets # pedantic, must be clean
cargo fmt
```

## Hard rules

- Never do filesystem work on the game folder in the TUI crate. Reads and writes both go through `grimoire-core`. If a page needs to know something about mods, it belongs in `grimoire-core` with a unit test there.
- Every write is a `Request` to the one `Backend`, through `core::Core`. Never rename a `pakNN` file anywhere else.
- Keep the Electron client's on-disk contract (pak slots, metadata keys, reserved priority slots) exactly.
- No telemetry. The only network traffic is GameBanana: Browse, downloads, and thumbnails of GameBanana mods.
- No em-dashes anywhere (code, comments, UI, commits).
- Commit subjects: imperative, lowercase (`add steam auto-detect`).

## Structure

Three tabs: Installed, Browse (GameBanana) and Settings. Core does every write; the TUI draws snapshots.

| Module | Owns |
|---|---|
| `crates/grimoire-core` | Mod library scanning/mutations, `gameinfo.gi`, Steam library auto-detect, Electron metadata/settings interop, crash-safe reorder journal. |
| `core` | The one `Backend`, on its own thread. Pages send a `Job` (requests that run in order and stop at the first failure) and get a fresh snapshot back. |
| `app` | The loop, the header and footer, the help popup, live config reload, handing the terminal to `$EDITOR`. It only draws when something changed. |
| `pages/` | One `Page` per tab. A page lists the actions it takes right now; the keymap, the footer hints and the help popup all read that list. A page big enough to be a folder keeps state and what keys do in `mod.rs` and drawing in `view.rs`. |
| `config/` | `~/.config/grimoire-tui/config.toml` and `themes/`. `default.toml` is the only source of defaults; the user's file is merged over it. |
| `gamebanana/` | apiv11 listing, details, categories and downloads, one request a second, plus archive unpacking into a temp dir. Installs go to core as `Request::Install`. |
| `thumbs` | Fetching, caching (`~/.cache/grimoire-tui/thumbnails`) and encoding thumbnails off the UI thread. Installed asks for them too, for mods that came from GameBanana. |
| `ui/` | Shared drawing pieces and the text field. Colors only ever come from theme roles. |

Mods are keyed by file name, and every move renames the file, so Installed follows a moved mod by what the job did (a new key, a place in the load order) instead of by its old key.
