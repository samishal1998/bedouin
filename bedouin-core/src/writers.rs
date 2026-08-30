//! Turning desired content into file content.
//!
//! Everything here is a pure function of `(existing text, desired text)`. §9's
//! marker rules are string logic and the interesting cases -- a start marker
//! with no end, rendered content that itself contains a marker -- are far
//! easier to pin down here than through a filesystem.

use crate::facts::Shell;

/// A content hash. Not cryptographic -- it answers "did this change", which is
/// all the drift check asks of it. Lives here so the planner and the executor
/// cannot disagree about whether something needs rewriting.
//
// ponytail: FNV-1a keeps the binary dependency-free. Swap for SHA-256 if state
// files ever need comparing across machines.
pub fn digest(s: &str) -> String {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in s.as_bytes() {
        h ^= u64::from(*b);
        h = h.wrapping_mul(0x1000_0000_01b3);
    }
    format!("fnv1a:{h:016x}")
}

/// The hash of a block's content, normalised the way the file stores it.
///
/// `upsert_block` writes the content as lines between markers, so a trailing
/// newline in the config does not survive the round trip. Hashing the raw
/// string in one place and the extracted lines in the other made every block
/// look edited the moment it was written.
pub fn block_digest(content: &str) -> String {
    digest(content.trim_end_matches('\n'))
}

pub fn start_marker(id: &str) -> String {
    format!("# >>> bedouin: {id} >>>")
}

pub fn end_marker(id: &str) -> String {
    format!("# <<< bedouin: {id} <<<")
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BlockError {
    /// A start marker with no matching end. Bedouin does not guess where the
    /// block ended and does not rewrite the file.
    Unterminated { id: String, line: usize },
    /// Rendered content carrying a marker line would split or capture a
    /// neighbouring block.
    MarkerInContent { id: String, line: String },
}

impl std::fmt::Display for BlockError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Unterminated { id, line } => write!(
                f,
                "the bedouin block `{id}` opens at line {line} and never closes.\n  \
                 Refusing to guess where it ends -- close it with `{}`, or delete \
                 the opening marker",
                end_marker(id)
            ),
            Self::MarkerInContent { id, line } => write!(
                f,
                "the content for `{id}` contains a bedouin marker line:\n    {line}\n  \
                 That would split or capture a neighbouring block. Remove it"
            ),
        }
    }
}

fn is_marker(line: &str) -> bool {
    let t = line.trim();
    t.starts_with("# >>> bedouin:") || t.starts_with("# <<< bedouin:")
}

/// Where a block sits in `text`, as inclusive line indices.
fn find_block(text: &str, id: &str) -> Result<Option<(usize, usize)>, BlockError> {
    let (start, end) = (start_marker(id), end_marker(id));
    let lines: Vec<&str> = text.lines().collect();
    let Some(from) = lines.iter().position(|l| l.trim() == start) else {
        return Ok(None);
    };
    match lines.iter().skip(from + 1).position(|l| l.trim() == end) {
        Some(offset) => Ok(Some((from, from + 1 + offset))),
        None => Err(BlockError::Unterminated {
            id: id.to_string(),
            line: from + 1,
        }),
    }
}

/// The content between a block's markers, if the block is there.
///
/// This is what `doctor` compares against the hash state recorded: it must see
/// exactly what `upsert_block` would have written, and nothing around it.
pub fn extract_block(text: &str, id: &str) -> Result<Option<String>, BlockError> {
    let lines: Vec<&str> = text.lines().collect();
    Ok(find_block(text, id)?.map(|(from, to)| lines[from + 1..to].join("\n")))
}

/// What replacing a block displaced, so nothing edited by hand is lost.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Upsert {
    pub text: String,
    /// The previous content between the markers, when it differed.
    pub superseded: Option<String>,
    pub changed: bool,
}

/// Insert or replace the block owned by `id`.
pub fn upsert_block(existing: &str, id: &str, content: &str) -> Result<Upsert, BlockError> {
    if let Some(line) = content.lines().find(|l| is_marker(l)) {
        return Err(BlockError::MarkerInContent {
            id: id.to_string(),
            line: line.trim().to_string(),
        });
    }

    let body: Vec<String> = std::iter::once(start_marker(id))
        .chain(content.lines().map(str::to_string))
        .chain(std::iter::once(end_marker(id)))
        .collect();

    let lines: Vec<&str> = existing.lines().collect();
    match find_block(existing, id)? {
        Some((from, to)) => {
            let previous = lines[from + 1..to].join("\n");
            if previous == content.trim_end_matches('\n') {
                return Ok(Upsert {
                    text: existing.to_string(),
                    superseded: None,
                    changed: false,
                });
            }
            let mut out: Vec<String> = lines[..from].iter().map(|s| (*s).to_string()).collect();
            out.extend(body);
            out.extend(lines[to + 1..].iter().map(|s| (*s).to_string()));
            Ok(Upsert {
                text: join(out),
                superseded: Some(previous),
                changed: true,
            })
        }
        None => {
            let mut out: Vec<String> = lines.iter().map(|s| (*s).to_string()).collect();
            // Keep one blank line between the existing content and the block.
            if !out.is_empty() && out.last().is_some_and(|l| !l.trim().is_empty()) {
                out.push(String::new());
            }
            out.extend(body);
            Ok(Upsert {
                text: join(out),
                superseded: None,
                changed: true,
            })
        }
    }
}

/// Remove the block owned by `id`, leaving everything else alone.
pub fn remove_block(existing: &str, id: &str) -> Result<Upsert, BlockError> {
    let lines: Vec<&str> = existing.lines().collect();
    match find_block(existing, id)? {
        None => Ok(Upsert {
            text: existing.to_string(),
            superseded: None,
            changed: false,
        }),
        Some((from, to)) => {
            let previous = lines[from + 1..to].join("\n");
            let mut out: Vec<String> = lines[..from].iter().map(|s| (*s).to_string()).collect();
            out.extend(lines[to + 1..].iter().map(|s| (*s).to_string()));
            while out.last().is_some_and(|l| l.trim().is_empty()) {
                out.pop();
            }
            Ok(Upsert {
                text: join(out),
                superseded: Some(previous),
                changed: true,
            })
        }
    }
}

fn join(lines: Vec<String>) -> String {
    if lines.is_empty() {
        return String::new();
    }
    let mut s = lines.join("\n");
    s.push('\n');
    s
}

/// The one file Bedouin renders from the structured `path:` entries.
///
/// PATH is never string-edited: provenance and removal are automatic because
/// this file is generated whole.
pub fn path_file(entries: &[String], shell: Shell) -> String {
    let mut out = String::from("# Generated by bedouin. Edits here are overwritten.\n");
    for e in entries {
        out.push_str(&match shell {
            // fish's own helper is idempotent and order-aware; exporting a
            // colon-joined string by hand is not the fish way.
            Shell::Fish => format!("fish_add_path {e}\n"),
            _ => format!("export PATH=\"{e}:$PATH\"\n"),
        });
    }
    out
}

/// Quote an alias value for a shell.
///
/// Alias values are user text landing in a file the shell evaluates, so this is
/// load-bearing rather than cosmetic. Single quotes because nothing inside them
/// is expanded; the only hard case is an embedded quote, and posix shells and
/// fish escape it differently.
pub fn quote_for(value: &str, shell: Shell) -> String {
    match shell {
        // A posix shell cannot escape `'` inside `'...'`: close, emit an
        // escaped quote, reopen.
        Shell::Fish => format!("'{}'", value.replace('\\', "\\\\").replace('\'', "\\'")),
        _ => format!("'{}'", value.replace('\'', "'\\''")),
    }
}

/// Alias declarations, in the shell's own syntax.
pub fn alias_lines(aliases: &std::collections::BTreeMap<String, String>, shell: Shell) -> String {
    aliases
        .iter()
        .map(|(name, value)| match shell {
            // fish's `alias` defines a function; the name and value are
            // separate words rather than an `=` pair.
            Shell::Fish => format!("alias {name} {}", quote_for(value, shell)),
            _ => format!("alias {name}={}", quote_for(value, shell)),
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Where a shell looks for completion files Bedouin generates.
pub fn completions_dir(shell: Shell, rc_dir: &std::path::Path, home: &std::path::Path) -> std::path::PathBuf {
    match shell {
        // fish reads this natively; nothing needs wiring.
        Shell::Fish => home.join(".config/fish/completions"),
        _ => rc_dir.join("completions"),
    }
}

/// The file one tool's completions are written to.
pub fn completions_file(shell: Shell, dir: &std::path::Path, name: &str) -> std::path::PathBuf {
    match shell {
        Shell::Zsh => dir.join(format!("_{name}")),
        Shell::Fish => dir.join(format!("{name}.fish")),
        _ => dir.join(format!("{name}.bash")),
    }
}

/// The block that makes a shell's rc file read the drop-in directory.
pub fn source_dir_snippet(rc_dir: &str, shell: Shell) -> String {
    let comp = format!("{rc_dir}/completions");
    match shell {
        Shell::Fish => String::new(), // conf.d and completions are both native
        // zsh finds completions through fpath, and fpath must be set before
        // compinit runs -- which is why this sits in the rc file rather than a
        // drop-in that may be sourced afterwards.
        Shell::Zsh => format!(
            "fpath=(\"{comp}\" $fpath)\n\
             if [ -d \"{rc_dir}\" ]; then\n  \
             for f in \"{rc_dir}\"/*.zsh; do [ -r \"$f\" ] && . \"$f\"; done\nfi"
        ),
        _ => format!(
            "if [ -d \"{rc_dir}\" ]; then\n  \
             for f in \"{rc_dir}\"/*; do [ -r \"$f\" ] && . \"$f\"; done\nfi\n\
             if [ -d \"{comp}\" ]; then\n  \
             for f in \"{comp}\"/*; do [ -r \"$f\" ] && . \"$f\"; done\nfi"
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const RC: &str = "export EDITOR=vi\nalias ll='ls -l'\n";

    #[test]
    fn a_new_block_is_appended_and_leaves_existing_content_alone() {
        let u = upsert_block(RC, "zellij", "eval \"$(zellij setup)\"").unwrap();
        assert!(u.changed);
        assert!(u.text.starts_with(RC), "existing content is untouched");
        assert!(u.text.contains("# >>> bedouin: zellij >>>"));
        assert!(u.text.contains("# <<< bedouin: zellij <<<"));
        assert_eq!(u.superseded, None);
    }

    #[test]
    fn rewriting_a_block_keeps_what_it_displaced() {
        let first = upsert_block(RC, "zellij", "old line").unwrap().text;
        let second = upsert_block(&first, "zellij", "new line").unwrap();
        assert!(second.changed);
        assert_eq!(second.superseded.as_deref(), Some("old line"));
        assert!(second.text.contains("new line"));
        assert!(!second.text.contains("old line"));
        // Only one block, not two.
        assert_eq!(second.text.matches("# >>> bedouin: zellij").count(), 1);
        assert!(second.text.starts_with(RC));
    }

    #[test]
    fn writing_the_same_content_twice_changes_nothing() {
        // Idempotence is the whole point: a second apply must be a no-op.
        let once = upsert_block(RC, "zellij", "same").unwrap().text;
        let twice = upsert_block(&once, "zellij", "same").unwrap();
        assert!(!twice.changed);
        assert_eq!(twice.text, once);
        assert_eq!(twice.superseded, None);
    }

    #[test]
    fn blocks_do_not_disturb_each_other() {
        let a = upsert_block(RC, "alpha", "A").unwrap().text;
        let b = upsert_block(&a, "beta", "B").unwrap().text;
        let a2 = upsert_block(&b, "alpha", "A2").unwrap();
        assert!(a2.text.contains("A2") && a2.text.contains('B'));
        let gone = remove_block(&a2.text, "alpha").unwrap();
        assert!(gone.changed);
        assert!(!gone.text.contains("A2"));
        assert!(gone.text.contains('B'), "beta survives alpha's removal");
        assert!(gone.text.starts_with(RC));
    }

    #[test]
    fn an_unterminated_block_is_refused_rather_than_guessed_at() {
        // Rewriting here would swallow everything after the opening marker.
        let broken = format!("{RC}{}\nsomething\n", start_marker("zellij"));
        let e = upsert_block(&broken, "zellij", "x").unwrap_err();
        assert!(matches!(e, BlockError::Unterminated { .. }));
        assert!(e.to_string().contains("never closes"), "{e}");
        assert!(remove_block(&broken, "zellij").is_err());
    }

    #[test]
    fn content_carrying_a_marker_line_is_refused() {
        // Otherwise a template could split or capture a neighbouring block.
        let e = upsert_block(RC, "evil", "echo hi\n# <<< bedouin: zellij <<<\necho bye")
            .unwrap_err();
        assert!(matches!(e, BlockError::MarkerInContent { .. }));
        assert!(e.to_string().contains("neighbouring"), "{e}");
    }

    #[test]
    fn removing_a_block_that_is_not_there_does_nothing() {
        let u = remove_block(RC, "absent").unwrap();
        assert!(!u.changed);
        assert_eq!(u.text, RC);
    }

    #[test]
    fn removing_the_only_block_restores_the_original_file() {
        let with = upsert_block(RC, "only", "x").unwrap().text;
        let without = remove_block(&with, "only").unwrap();
        assert_eq!(without.text, RC, "no stray blank lines left behind");
    }

    #[test]
    fn a_blocks_content_can_be_read_back_out_of_the_file() {
        // What `doctor` compares against the recorded hash, so it must round
        // trip exactly through upsert.
        let content = "eval \"$(zellij setup)\"\nexport X=1";
        let text = upsert_block(RC, "zellij", content).unwrap().text;
        assert_eq!(extract_block(&text, "zellij").unwrap().as_deref(), Some(content));
        assert_eq!(extract_block(&text, "absent").unwrap(), None);
        assert_eq!(digest(&extract_block(&text, "zellij").unwrap().unwrap()), digest(content));
    }

    #[test]
    fn a_block_hashes_the_same_whether_from_config_or_from_disk() {
        // The round trip drops a trailing newline, so hashing the raw config
        // string and the extracted block must still agree -- otherwise every
        // block reads as drifted the instant it is written.
        for content in ["alias j=jq\n", "alias j=jq", "a\nb\n", "a\nb"] {
            let text = upsert_block(RC, "x", content).unwrap().text;
            let back = extract_block(&text, "x").unwrap().unwrap();
            assert_eq!(
                block_digest(content),
                block_digest(&back),
                "content {content:?} did not round trip"
            );
        }
    }

    #[test]
    fn a_hand_edit_inside_the_markers_changes_the_hash() {
        let text = upsert_block(RC, "zellij", "original").unwrap().text;
        let edited = text.replace("original", "someone changed this");
        assert_ne!(
            digest(&extract_block(&edited, "zellij").unwrap().unwrap()),
            digest("original"),
            "drift must be detectable"
        );
    }

    #[test]
    fn the_path_file_uses_each_shell_own_syntax() {
        let entries = vec!["/home/t/.cargo/bin".to_string(), "/home/t/.local/bin".to_string()];
        let zsh = path_file(&entries, Shell::Zsh);
        assert!(zsh.contains("export PATH=\"/home/t/.cargo/bin:$PATH\""));
        assert!(zsh.contains("Generated by bedouin"));

        let fish = path_file(&entries, Shell::Fish);
        assert!(fish.contains("fish_add_path /home/t/.cargo/bin"));
        assert!(!fish.contains("export PATH"), "fish does not export PATH");

        // Order is the order given: it is the config's declaration order.
        let first = zsh.find(".cargo").unwrap();
        let second = zsh.find(".local").unwrap();
        assert!(first < second);
    }

    #[test]
    fn an_empty_path_file_is_still_a_valid_file() {
        let out = path_file(&[], Shell::Zsh);
        assert!(out.contains("Generated by bedouin"));
        assert!(!out.contains("export PATH"));
    }

    #[test]
    fn the_source_snippet_is_a_no_op_for_fish() {
        assert!(source_dir_snippet("/home/t/.config/fish/conf.d", Shell::Fish).is_empty());
        let zsh = source_dir_snippet("/home/t/.zshrc.d", Shell::Zsh);
        assert!(zsh.contains("/home/t/.zshrc.d"));
        // Guarded, so an rc file survives the directory not existing yet.
        assert!(zsh.contains("-d"));
    }

    #[test]
    fn a_block_can_be_written_into_an_empty_file() {
        let u = upsert_block("", "first", "x").unwrap();
        assert!(u.text.starts_with("# >>> bedouin: first >>>"));
        assert!(u.text.ends_with("<<<\n"));
    }
}
