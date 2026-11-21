# Grimoire TUI

A terminal mod manager for Deadlock. It's the terminal version of [Grimoire](https://github.com/Slush97/grimoire).

It picks up your existing Grimoire setup, so there's nothing to migrate. It works on the same mod files in your Deadlock folder, so you can switch between this and Grimoire whenever you want.

Linux only for now.

It has three tabs:

- **Installed**: turn mods on and off, change the load order, pick between variants of a mod, delete what you don't want.
- **Browse**: search GameBanana and install mods straight from it. Zip, 7z and rar all work.
- **Settings**: your Deadlock folder, theme, thumbnails and so on.

## Install

You need [Rust](https://rustup.rs) 1.86 or newer and a C++ compiler (rar support builds a bit of C++).

```bash
# Arch
sudo pacman -S --needed base-devel

# Debian / Ubuntu
sudo apt install build-essential
```

Then:

```bash
cargo install --git https://github.com/Slush97/grimoire-tui --locked
grimoire-tui
```

Run the same `cargo install` again to update.

## First run

It looks through your Steam libraries for Deadlock on its own. If it can't find it, give it the folder that has `game/citadel` inside:

```bash
grimoire-tui ~/.local/share/Steam/steamapps/common/Deadlock
```

It remembers that folder, so you only do this once. You can change it later in Settings.

## Keys

Press `?` anywhere to see every key that works right there. The footer shows the common ones. `j`/`k` or the arrow keys move around, `enter` confirms, `esc` backs out.

Everywhere:

| Key | What it does |
|---|---|
| `1` `2` `3` or `tab` | switch tabs |
| `/` | search |
| `?` | help |
| `q` | quit |

Installed:

| Key | What it does |
|---|---|
| `space` or `enter` | turn the mod on or off |
| `m` | pick a mod up, move it with `j`/`k`, drop it with `enter` (`esc` puts it back) |
| `K` / `J` | load it one step earlier / later |
| `v` | switch to another variant of the same mod |
| `g` | make it Global, so it loads before everything else |
| `d` | delete it |
| `f` | show on, off or all mods |
| `o` | open its GameBanana page |
| `O` | open the mods folder |

Browse:

| Key | What it does |
|---|---|
| `enter` | details |
| `i` | install |
| `s` | sort |
| `c` | pick a category |
| `x` | clear filters |
| `v` | list or thumbnail cards |
| `o` | open on GameBanana |

Settings: `enter` changes the selected row, `d` finds Deadlock again, `e` opens the config file in `$EDITOR`.

Two keys for when things go wrong. If a move got cut off halfway (crash, power cut), Installed tells you and `R` finishes it. If mods stop loading after a Deadlock update, `F` fixes `gameinfo.gi`.

## Config

The config file lives at `~/.config/grimoire-tui/config.toml`. It's created on first run with every option in it and a comment on each one. Save it and the change shows up right away, no restart. If you break it, `grimoire-tui --default-config` prints the original.

In there you can rebind any key, pick a theme (`grimoire`, `paper`, or `terminal` to use your terminal's own colors), override single colors, change the border style, and choose list or cards for Browse. Your own themes go in `themes/` next to the config file.

Thumbnails show up properly in terminals that can draw images (kitty, Ghostty, WezTerm, foot and others). Everywhere else you get a blocky version. Set `protocol = "off"` under `[images]` if you don't want them.

## What goes over the network

Only GameBanana: browsing, downloads, and thumbnails for mods you got from there. No tracking, no analytics, no update checks. Thumbnails are cached in `~/.cache/grimoire-tui/thumbnails`.

## License

MIT
