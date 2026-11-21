//! The `SearchPaths` block in `gameinfo.gi`, which decides which folders the
//! engine loads mods from. See docs/spec/on-disk-format.md#gameinfogi-searchpaths.
//!
//! Grimoire owns the whole block and rewrites it canonically. Everything else
//! in the file (including the old client's performance-config lines) is left
//! alone. Repairs keep the file's own line endings and the block's indentation
//! so the diff stays minimal.

use std::fmt::Write as _;
use std::fs;
use std::path::Path;

use crate::game::GameInstall;
use crate::{fsx, Error, Result};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GameinfoStatus {
    Configured,
    /// Why it needs fixing, for the user.
    NeedsFix(String),
    /// `gameinfo.gi` is gone.
    Missing,
    /// No `FileSystem` section to put `SearchPaths` in. Only Steam's
    /// "verify game files" can fix it.
    Unrepairable,
}

pub(crate) fn check(install: &GameInstall) -> Result<GameinfoStatus> {
    let path = install.gameinfo();
    let text = match fs::read_to_string(&path) {
        Ok(text) => text,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            return Ok(GameinfoStatus::Missing)
        }
        Err(err) => return Err(Error::io(&path)(err)),
    };
    let Some(block) = find_block(&text) else {
        return Ok(if has_filesystem(&text) {
            GameinfoStatus::NeedsFix("gameinfo.gi has no SearchPaths block".to_owned())
        } else {
            GameinfoStatus::Unrepairable
        });
    };
    let mut missing = Vec::new();
    for want in ["citadel/grimoire", "citadel/addons"] {
        if !has_game_line(&text[block.start..block.end], want) {
            missing.push(want.to_owned());
        }
    }
    for root in install.addon_roots()? {
        if root.index == 0 {
            continue;
        }
        let want = format!("citadel/addons{}", root.index);
        if !has_game_line(&text[block.start..block.end], &want) {
            missing.push(want);
        }
    }
    if missing.is_empty() {
        Ok(GameinfoStatus::Configured)
    } else {
        Ok(GameinfoStatus::NeedsFix(format!(
            "SearchPaths is missing {}",
            missing.join(", ")
        )))
    }
}

/// Rewrites `SearchPaths` to the canonical block, with a line for every
/// overflow root on disk. Also called by `library` right after it creates a
/// new overflow root, before any VPK goes in it.
pub(crate) fn repair(install: &GameInstall) -> Result<()> {
    let path = install.gameinfo();
    let text = match fs::read_to_string(&path) {
        Ok(text) => text,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            return Err(Error::Refused(
                "gameinfo.gi is missing. Verify your game files in Steam, then try again."
                    .to_owned(),
            ));
        }
        Err(err) => return Err(Error::io(&path)(err)),
    };
    let ending = if text.contains("\r\n") { "\r\n" } else { "\n" };
    let roots: Vec<u32> = install
        .addon_roots()?
        .iter()
        .filter_map(|root| (root.index > 0).then_some(root.index))
        .collect();
    let deadworks = install.citadel().join("deadworks_addons/vpks").exists();

    let edited = if let Some(block) = find_block(&text) {
        let mut edited = String::with_capacity(text.len() + 512);
        edited.push_str(&text[..block.start]);
        edited.push_str(&render(&block.indent, ending, &roots, deadworks));
        edited.push_str(&text[block.end..]);
        edited
    } else if let Some(insert) = filesystem_insert(&text) {
        let mut edited = String::with_capacity(text.len() + 512);
        edited.push_str(&text[..insert.at]);
        edited.push_str(&render(&insert.indent, ending, &roots, deadworks));
        edited.push_str(&text[insert.at..]);
        edited
    } else {
        return Err(Error::Refused(
            "gameinfo.gi has no FileSystem section. Verify your game files in Steam, then try again."
                .to_owned(),
        ));
    };

    back_up(&path)?;
    fsx::write_atomic(&path, edited.as_bytes())
}

/// Copies the original aside before the first edit. The backup is never
/// overwritten, so it stays a way back to before Grimoire ever touched the file.
fn back_up(path: &Path) -> Result<()> {
    let mut backup = path.as_os_str().to_owned();
    backup.push(".grimoire-bak");
    if Path::new(&backup).exists() {
        return Ok(());
    }
    fs::copy(path, &backup).map_err(Error::io(&backup))?;
    Ok(())
}

/// An active `Game <want>` line inside the block. Commented-out lines don't
/// count, and matching is case-insensitive so oddly-cased files aren't mangled.
fn has_game_line(block: &str, want: &str) -> bool {
    block.lines().any(|line| {
        let line = line.split("//").next().unwrap_or("").trim();
        let mut parts = line.split_whitespace();
        parts
            .next()
            .is_some_and(|key| key.eq_ignore_ascii_case("game"))
            && parts
                .next()
                .is_some_and(|path| path.trim_matches('"').eq_ignore_ascii_case(want))
    })
}

fn has_filesystem(text: &str) -> bool {
    text.lines().any(|line| is_section(line, "filesystem"))
}

fn is_section(line: &str, name: &str) -> bool {
    line.split("//")
        .next()
        .unwrap_or("")
        .trim()
        .trim_matches('"')
        .eq_ignore_ascii_case(name)
}

/// The full lines to replace, with the `SearchPaths` line's own indentation.
struct Block {
    start: usize,
    end: usize,
    indent: String,
}

fn find_block(text: &str) -> Option<Block> {
    let (start, line) =
        lines_from(text, 0).find(|(_, line)| line.trim().eq_ignore_ascii_case("searchpaths"))?;
    let indent: String = line.chars().take_while(|c| c.is_whitespace()).collect();
    let close = matching_brace(text, start + line.len())?;
    Some(Block {
        start,
        end: line_end(text, close),
        indent,
    })
}

struct Insert {
    /// Byte offset to insert at: right after the `FileSystem` opening brace.
    at: usize,
    /// One tab deeper than the `FileSystem` line, like a hand-written file.
    indent: String,
}

fn filesystem_insert(text: &str) -> Option<Insert> {
    let (start, line) = lines_from(text, 0).find(|(_, line)| is_section(line, "filesystem"))?;
    let mut indent: String = line.chars().take_while(|c| c.is_whitespace()).collect();
    indent.push('\t');
    let open = opening_brace(text, start + line.len())?;
    Some(Insert {
        at: line_end(text, open),
        indent,
    })
}

/// Lines with their byte offsets. `str::lines` doesn't say where a line was.
fn lines_from(text: &str, from: usize) -> impl Iterator<Item = (usize, &str)> {
    let mut at = from;
    text[from..].split_inclusive('\n').map(move |line| {
        let start = at;
        at += line.len();
        (start, line)
    })
}

/// End of the line containing `at`, through its newline, or the file end.
fn line_end(text: &str, at: usize) -> usize {
    text[at..].find('\n').map_or(text.len(), |end| at + end + 1)
}

/// The `{` opening the block whose header ends at `from`. A `}` first means
/// the file is malformed, and forcing a block into it would only hide that.
fn opening_brace(text: &str, from: usize) -> Option<usize> {
    let bytes = text.as_bytes();
    let mut i = from;
    loop {
        i = skip_trivia(text, i);
        if i >= bytes.len() {
            return None;
        }
        match bytes[i] {
            b'{' => return Some(i),
            b'}' => return None,
            _ => i += 1,
        }
    }
}

/// Byte index of the `}` closing the first `{` at or after `from`.
fn matching_brace(text: &str, from: usize) -> Option<usize> {
    let bytes = text.as_bytes();
    let mut i = from;
    let mut depth = 0;
    while i < bytes.len() {
        i = skip_trivia(text, i);
        if i >= bytes.len() {
            return None;
        }
        match bytes[i] {
            b'{' => depth += 1,
            b'}' => {
                if depth <= 1 {
                    return (depth == 1).then_some(i);
                }
                depth -= 1;
            }
            _ => {}
        }
        i += 1;
    }
    None
}

/// Skips quoted strings and `//` comments so braces inside them don't count.
fn skip_trivia(text: &str, mut i: usize) -> usize {
    let bytes = text.as_bytes();
    while i < bytes.len() {
        if bytes[i] == b'"' {
            i += 1;
            while i < bytes.len() {
                if bytes[i] == b'\\' {
                    i += 2;
                } else {
                    i += 1;
                    if bytes[i - 1] == b'"' {
                        break;
                    }
                }
            }
        } else if bytes.get(i + 1) == Some(&b'/') && bytes[i] == b'/' {
            i = text[i..].find('\n').map_or(bytes.len(), |end| i + end + 1);
        } else {
            break;
        }
    }
    i
}

fn render(base: &str, ending: &str, roots: &[u32], deadworks: bool) -> String {
    let mut entries = vec![
        ("Game", "citadel/grimoire".to_owned()),
        ("Game", "citadel/addons".to_owned()),
    ];
    for root in roots {
        entries.push(("Game", format!("citadel/addons{root}")));
    }
    if deadworks {
        entries.push(("Game", "citadel/deadworks_addons/vpks".to_owned()));
    }
    entries.extend(
        [
            ("Mod", "citadel"),
            ("Write", "citadel"),
            ("Game", "citadel"),
            ("Write", "core"),
            ("Mod", "core"),
            ("Game", "core"),
            ("AddonRoot", "citadel_addons"),
            ("OfficialAddonRoot", "citadel_community_addons"),
        ]
        .map(|(key, value)| (key, value.to_owned())),
    );

    let mut out = format!("{base}SearchPaths{ending}{base}\t{{{ending}");
    for (key, value) in entries {
        // Tab counts, not column math: short keys get more tabs so the values
        // line up the way the old client wrote them.
        let tabs = match key {
            "Game" | "Mod" | "Write" => "\t\t\t\t",
            "AddonRoot" => "\t\t\t",
            _ => "\t\t",
        };
        let _ = write!(out, "{base}\t\t{key}{tabs}{value}{ending}");
    }
    let _ = write!(out, "{base}\t}}{ending}");
    out
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::*;

    const FILESYSTEM: &str = "\"GameInfo\"\n{\n\tFileSystem\n\t{\n\t\t// a comment\n";

    fn install_with(gameinfo: &str) -> (tempfile::TempDir, GameInstall) {
        let dir = tempfile::tempdir().unwrap();
        let citadel = dir.path().join("game/citadel");
        fs::create_dir_all(citadel.join("addons")).unwrap();
        fs::write(citadel.join("gameinfo.gi"), gameinfo).unwrap();
        let install = GameInstall::open(dir.path()).unwrap();
        (dir, install)
    }

    fn block(entries: &str) -> String {
        format!("{FILESYSTEM}\t\tSearchPaths\n\t\t{{\n{entries}\t\t}}\n\t\t\"UserSettingsPathID\"\t\"USRLOCAL\"\n\t}}\n}}\n")
    }

    fn game_line(path: &str) -> String {
        format!("\t\tGame\t\t\t\t{path}\n")
    }

    #[test]
    fn check_passes_a_configured_block() {
        let (_dir, install) = install_with(&block(&format!(
            "{}{}",
            game_line("citadel/grimoire"),
            game_line("citadel/addons")
        )));
        assert_eq!(check(&install).unwrap(), GameinfoStatus::Configured);
    }

    #[test]
    fn check_ignores_commented_lines_and_requires_overflow_roots() {
        let (_dir, install) = install_with(&block(&format!(
            "//Game citadel/grimoire\n{}",
            game_line("citadel/addons")
        )));
        fs::create_dir(install.citadel().join("addons2")).unwrap();

        assert_eq!(
            check(&install).unwrap(),
            GameinfoStatus::NeedsFix(
                "SearchPaths is missing citadel/grimoire, citadel/addons2".to_owned()
            )
        );
    }

    #[test]
    fn check_reports_a_missing_file_block_and_section() {
        let dir = tempfile::tempdir().unwrap();
        let citadel = dir.path().join("game/citadel");
        fs::create_dir_all(citadel.join("addons")).unwrap();
        fs::write(citadel.join("gameinfo.gi"), "").unwrap();
        let install = GameInstall::open(dir.path()).unwrap();

        fs::remove_file(install.gameinfo()).unwrap();
        assert_eq!(check(&install).unwrap(), GameinfoStatus::Missing);

        fs::write(install.gameinfo(), FILESYSTEM).unwrap();
        assert!(matches!(
            check(&install).unwrap(),
            GameinfoStatus::NeedsFix(_)
        ));

        fs::write(install.gameinfo(), "\"GameInfo\"\n{\n}\n").unwrap();
        assert_eq!(check(&install).unwrap(), GameinfoStatus::Unrepairable);
    }

    #[test]
    fn repair_rewrites_only_the_block_and_keeps_a_backup() {
        let (_dir, install) = install_with(&block(&format!(
            "{}\t\tMod\t\t\t\tcitadel\n",
            game_line("citadel/addons")
        )));
        fs::create_dir(install.citadel().join("addons1")).unwrap();

        repair(&install).unwrap();

        let text = fs::read_to_string(install.gameinfo()).unwrap();
        assert!(text.contains("citadel/grimoire"));
        assert!(text.contains("citadel/addons1"));
        assert!(text.contains("\"UserSettingsPathID\""));
        assert_eq!(check(&install).unwrap(), GameinfoStatus::Configured);

        let backup = install.citadel().join("gameinfo.gi.grimoire-bak");
        assert!(backup.is_file());
        fs::write(&backup, "user copy").unwrap();
        repair(&install).unwrap();
        assert_eq!(fs::read_to_string(&backup).unwrap(), "user copy");
    }

    #[test]
    fn repair_inserts_into_filesystem_and_keeps_crlf() {
        let (_dir, install) = install_with(&FILESYSTEM.replace('\n', "\r\n"));
        fs::create_dir_all(install.citadel().join("deadworks_addons/vpks")).unwrap();

        repair(&install).unwrap();

        let text = fs::read_to_string(install.gameinfo()).unwrap();
        assert!(text.contains("\t\tSearchPaths\r\n"));
        assert!(text.contains("citadel/deadworks_addons/vpks"));
        assert!(!text.contains("\n\n"));
        assert_eq!(check(&install).unwrap(), GameinfoStatus::Configured);
    }

    #[test]
    fn repair_without_a_filesystem_section_is_refused() {
        let (_dir, install) = install_with("\"GameInfo\"\n{\n}\n");
        let result = repair(&install);
        assert!(matches!(result, Err(Error::Refused(_))));
    }
}
