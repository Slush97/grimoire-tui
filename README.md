# Grimoire TUI

Terminal mod manager for Deadlock (Linux only). Compatible with desktop [Grimoire](https://github.com/Slush97/grimoire): it imports your existing Deadlock path and shares `mod-metadata.json` and the same mod layout, so you can switch between both freely.

## Install

Requires [Rust](https://rustup.rs) 1.86+ and a C++ compiler (for `unrar`).

```bash
# Arch
sudo pacman -S --needed base-devel

# Debian / Ubuntu
sudo apt install build-essential
```

```bash
cargo install --git https://github.com/Slush97/grimoire-tui --locked
grimoire-tui
```

Run the same `cargo install` command again to update.

## Usage

On first run, Grimoire TUI detects Deadlock from your Steam libraries (or your existing Grimoire config). To set the path manually:

```bash
grimoire-tui ~/.local/share/Steam/steamapps/common/Deadlock
```

Tabs (`1`/`2`/`3` or `tab`):

- **Installed**: toggle mods (`space`/`enter`), reorder (`m` to grab/move, `K`/`J` to nudge), switch variants (`v`), toggle Global priority (`g`), delete (`d`), filter (`/` or `f`), repair an interrupted move (`R`), fix `gameinfo.gi` (`F`).
- **Browse**: search and filter GameBanana (`/` or `c`), sort (`s`), toggle list/card view (`v`), inspect details (`enter`), download and install zip/7z/rar/vpk mods (`i`).
- **Settings**: game folder, `gameinfo.gi`, auto-enable downloads, NSFW filter, theme, thumbnails, and opening the config in `$EDITOR` (`e`).

Press `?` anywhere for the full key list.

## Config

Created on first run at `~/.config/grimoire-tui/config.toml` and live-reloaded on save. Run `grimoire-tui --default-config` to print the defaults.

- **Themes**: `grimoire`, `paper`, or `terminal` (uses terminal ANSI colors), plus custom `.toml` themes in `~/.config/grimoire-tui/themes/`.
- **Keybinds**: every action is rebindable under `[keys]`.
- **Thumbnails**: auto-detects Kitty, Ghostty, WezTerm, foot, and iTerm2, falling back to halfblocks. Set `protocol = "off"` under `[images]` to disable.

## Network

Only contacts GameBanana for Browse listings, mod downloads, and thumbnails (cached in `~/.cache/grimoire-tui/thumbnails`). No telemetry or update checks.

## License

MIT
