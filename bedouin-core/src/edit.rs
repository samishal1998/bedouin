//! Editing the config file the user maintains.
//!
//! Text surgery, not a serde round trip. The config lives in the user's git
//! repository with their comments, their ordering and their formatting;
//! re-serialising it would silently reflow the whole file and drop every
//! comment. So an edit removes exactly the lines it means to and leaves the
//! rest byte-for-byte alone.
//!
//! Every edit verifies itself by re-parsing the result. If the file no longer
//! parses, or still contains what was supposed to go, the edit is refused and
//! the user is told to make the change by hand. A config tool that mangles the
//! config is worse than one that cannot edit it.

use crate::schema::{ConfigError, Result};

/// Which top-level list an item lives in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Section {
    Packages,
    Languages,
}

impl Section {
    fn key(self) -> &'static str {
        match self {
            Self::Packages => "packages:",
            Self::Languages => "languages:",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Packages => "package",
            Self::Languages => "language",
        }
    }
}

/// Does the text after a `name:` denote exactly `name`?
///
/// Has to survive an inline comment (`- name: jq   # keep this`), quotes, and
/// the flow form's trailing `,` or `}`.
fn value_is(raw: &str, name: &str) -> bool {
    let mut v = raw.trim();
    // A comment starts at an unquoted `#` preceded by whitespace.
    if !v.starts_with(['"', '\'']) {
        if let Some(i) = v.find(" #") {
            v = v[..i].trim();
        }
    }
    let v = v
        .trim_start_matches(['"', '\''])
        .split([',', '}', '"', '\''])
        .next()
        .unwrap_or("")
        .trim();
    v == name
}

fn indent_of(line: &str) -> usize {
    line.len() - line.trim_start().len()
}

/// Does this line begin a list entry naming `name`?
///
/// Handles both the block form (`- name: jq`, with `name:` on the same line or
/// the next) and the flow form (`- { name: jq, from: apt }`).
fn entry_names(lines: &[&str], at: usize, name: &str) -> bool {
    let line = lines[at].trim_start();
    let Some(rest) = line.strip_prefix("- ") else {
        return false;
    };
    let rest = rest.trim();
    let matches_here = |s: &str| {
        s.split_once("name:").is_some_and(|(_, v)| value_is(v, name))
    };
    if rest.contains("name:") {
        return matches_here(rest);
    }
    // `- ` alone, with the fields on following lines.
    let base = indent_of(lines[at]);
    lines[at + 1..]
        .iter()
        .take_while(|l| l.trim().is_empty() || indent_of(l) > base)
        .any(|l| matches_here(l.trim()))
}

/// The half-open line range one list entry occupies.
fn entry_span(lines: &[&str], start: usize) -> (usize, usize) {
    let base = indent_of(lines[start]);
    let mut end = start + 1;
    while end < lines.len() {
        let l = lines[end];
        if l.trim().is_empty() {
            end += 1;
            continue;
        }
        if indent_of(l) <= base {
            break;
        }
        end += 1;
    }
    // Do not swallow blank lines that separate this entry from the next.
    while end > start + 1 && lines[end - 1].trim().is_empty() {
        end -= 1;
    }
    (start, end)
}

/// Remove one named entry from `packages:` or `languages:`.
pub fn remove_entry(text: &str, section: Section, name: &str) -> Result<String> {
    let lines: Vec<&str> = text.lines().collect();

    let Some(head) = lines.iter().position(|l| l.trim_start() == section.key()) else {
        return Err(ConfigError::new(format!(
            "this config has no `{}` section, so there is no `{name}` to remove",
            section.key()
        )));
    };
    let head_indent = indent_of(lines[head]);

    let mut found = None;
    let mut i = head + 1;
    while i < lines.len() {
        let l = lines[i];
        if !l.trim().is_empty() && indent_of(l) <= head_indent {
            break; // out of this section
        }
        if entry_names(&lines, i, name) {
            found = Some(i);
            break;
        }
        i += 1;
    }

    let Some(start) = found else {
        return Err(ConfigError::new(format!(
            "no {} named `{name}` in this config",
            section.label()
        )));
    };

    let (from, mut to) = entry_span(&lines, start);
    // An entry with a blank line on each side leaves two behind. Take one with
    // it, so removing from a spaced-out list does not open a growing gap.
    let blank_before = from > 0 && lines[from - 1].trim().is_empty();
    let blank_after = lines.get(to).is_some_and(|l| l.trim().is_empty());
    if blank_after && (blank_before || from == 0) {
        to += 1;
    }
    let mut kept: Vec<&str> = Vec::with_capacity(lines.len());
    kept.extend_from_slice(&lines[..from]);
    kept.extend_from_slice(&lines[to..]);
    let mut out = kept.join("\n");
    if text.ends_with('\n') {
        out.push('\n');
    }

    // Verify by re-parsing rather than trusting the surgery. Nothing here is
    // worth a mangled config.
    let parsed: serde_yaml_ng::Value = serde_yaml_ng::from_str(&out).map_err(|e| {
        ConfigError::new(format!(
            "removing `{name}` would leave a file that no longer parses: {e}\n  \
             Refusing to write it. Remove the entry by hand"
        ))
    })?;
    if still_present(&parsed, section, name) {
        return Err(ConfigError::new(format!(
            "could not cleanly remove `{name}` -- it is still there after the edit.\n  \
             Refusing to guess. Remove the entry by hand"
        )));
    }
    Ok(out)
}

/// Append a package to `packages:`, creating the section if it is absent.
///
/// The insert mirror of [`remove_entry`], with the same verify-by-reparsing:
/// this writes the file the user keeps in git, so it appends and touches
/// nothing else.
pub fn add_package(text: &str, name: &str, from: &str, version: Option<&str>) -> Result<String> {
    if serde_yaml_ng::from_str::<serde_yaml_ng::Value>(text)
        .ok()
        .is_some_and(|d| still_present(&d, Section::Packages, name))
    {
        return Err(ConfigError::new(format!(
            "`{name}` is already in this config"
        )));
    }

    let entry = match version {
        Some(v) => format!("  - name: {name}\n    from: {from}\n    version: \"{v}\"\n"),
        None => format!("  - name: {name}\n    from: {from}\n"),
    };

    let lines: Vec<&str> = text.lines().collect();
    let out = match lines.iter().position(|l| l.trim_start() == "packages:") {
        // Append to the end of the existing section rather than the file: a
        // `packages:` block followed by `files:` must not swallow the latter.
        Some(head) => {
            let mut end = head + 1;
            let head_indent = indent_of(lines[head]);
            let mut last_content = head;
            while end < lines.len() {
                let l = lines[end];
                if !l.trim().is_empty() && indent_of(l) <= head_indent {
                    break;
                }
                if !l.trim().is_empty() {
                    last_content = end;
                }
                end += 1;
            }
            let mut kept: Vec<String> = lines[..=last_content].iter().map(|s| (*s).to_string()).collect();
            kept.push(entry.trim_end().to_string());
            kept.extend(lines[last_content + 1..].iter().map(|s| (*s).to_string()));
            kept.join("\n")
        }
        None => {
            let mut base = text.trim_end().to_string();
            base.push_str("\n\npackages:\n");
            base.push_str(entry.trim_end());
            base
        }
    };
    let mut out = out;
    out.push('\n');

    let parsed: serde_yaml_ng::Value = serde_yaml_ng::from_str(&out).map_err(|e| {
        ConfigError::new(format!(
            "adding `{name}` would leave a file that no longer parses: {e}\n  \
             Refusing to write it. Add the entry by hand"
        ))
    })?;
    if !still_present(&parsed, Section::Packages, name) {
        return Err(ConfigError::new(format!(
            "could not cleanly add `{name}`. Refusing to guess -- add it by hand"
        )));
    }
    Ok(out)
}

fn still_present(doc: &serde_yaml_ng::Value, section: Section, name: &str) -> bool {
    let key = section.key().trim_end_matches(':');
    doc.get(key)
        .and_then(|v| v.as_sequence())
        .is_some_and(|items| {
            items
                .iter()
                .any(|i| i.get("name").and_then(|n| n.as_str()) == Some(name))
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    const CFG: &str = r#"version: 0
shell: zsh

# The tools I actually use
packages:
  - name: jq          # a comment worth keeping
    from: apt

  - name: zellij
    from: cargo
    rc:
      - file: "{{ shell.rc_dir }}/70-zellij.zsh"
        content: |
          eval "$(zellij setup)"

  - { name: fd, from: apt }

files:
  - src: templates/gitconfig.j2
    dest: ~/.gitconfig
"#;

    #[test]
    fn removing_an_entry_leaves_everything_else_byte_for_byte() {
        let out = remove_entry(CFG, Section::Packages, "zellij").unwrap();
        assert!(!out.contains("zellij"));
        // The neighbours, their comments and the later sections all survive.
        assert!(out.contains("# a comment worth keeping"));
        assert!(out.contains("# The tools I actually use"));
        assert!(out.contains("- { name: fd, from: apt }"));
        assert!(out.contains("dest: ~/.gitconfig"));
        assert!(out.contains("shell: zsh"));
        // And no growing gap where the entry used to be.
        assert!(!out.contains("\n\n\n"), "left a double blank line:\n{out}");
    }

    #[test]
    fn a_flow_style_entry_is_removable_too() {
        let out = remove_entry(CFG, Section::Packages, "fd").unwrap();
        assert!(!out.contains("name: fd"));
        assert!(out.contains("name: jq"));
        assert!(out.contains("name: zellij"));
    }

    #[test]
    fn removing_the_first_entry_does_not_eat_the_second() {
        let out = remove_entry(CFG, Section::Packages, "jq").unwrap();
        assert!(!out.contains("name: jq"));
        assert!(out.contains("name: zellij"), "the next entry survives");
        assert!(out.contains("eval \"$(zellij setup)\""), "and its nested block");
    }

    #[test]
    fn the_result_still_parses_and_still_has_the_others() {
        let out = remove_entry(CFG, Section::Packages, "zellij").unwrap();
        let doc: serde_yaml_ng::Value = serde_yaml_ng::from_str(&out).unwrap();
        let names: Vec<&str> = doc["packages"]
            .as_sequence()
            .unwrap()
            .iter()
            .map(|p| p["name"].as_str().unwrap())
            .collect();
        assert_eq!(names, ["jq", "fd"]);
    }

    #[test]
    fn removing_something_absent_says_so_rather_than_editing() {
        let e = remove_entry(CFG, Section::Packages, "ripgrep").unwrap_err();
        assert!(e.message.contains("no package named `ripgrep`"), "{e}");
        let e = remove_entry("version: 0\n", Section::Packages, "jq").unwrap_err();
        assert!(e.message.contains("no `packages:` section"), "{e}");
    }

    #[test]
    fn adding_a_package_appends_to_its_section_and_nothing_else() {
        let out = add_package(CFG, "ripgrep", "apt", None).unwrap();
        let doc: serde_yaml_ng::Value = serde_yaml_ng::from_str(&out).unwrap();
        let names: Vec<&str> = doc["packages"]
            .as_sequence()
            .unwrap()
            .iter()
            .map(|p| p["name"].as_str().unwrap())
            .collect();
        assert_eq!(names, ["jq", "zellij", "fd", "ripgrep"]);
        // The section after `packages:` must not be swallowed.
        assert!(out.contains("dest: ~/.gitconfig"));
        assert!(out.contains("# a comment worth keeping"));
    }

    #[test]
    fn adding_with_a_version_pins_it() {
        let out = add_package(CFG, "zoxide", "cargo", Some("0.9.4")).unwrap();
        let doc: serde_yaml_ng::Value = serde_yaml_ng::from_str(&out).unwrap();
        let last = doc["packages"].as_sequence().unwrap().last().unwrap();
        assert_eq!(last["name"].as_str(), Some("zoxide"));
        assert_eq!(last["version"].as_str(), Some("0.9.4"), "quoted, so 1.80 stays 1.80");
    }

    #[test]
    fn adding_something_already_there_is_refused() {
        let e = add_package(CFG, "jq", "apt", None).unwrap_err();
        assert!(e.message.contains("already in this config"), "{e}");
    }

    #[test]
    fn a_config_with_no_packages_section_gains_one() {
        let out = add_package("version: 0\nshell: zsh\n", "jq", "apt", None).unwrap();
        let doc: serde_yaml_ng::Value = serde_yaml_ng::from_str(&out).unwrap();
        assert_eq!(doc["packages"][0]["name"].as_str(), Some("jq"));
        assert_eq!(doc["shell"].as_str(), Some("zsh"));
    }

    #[test]
    fn add_then_remove_returns_the_file_to_where_it_started() {
        let added = add_package(CFG, "ripgrep", "apt", None).unwrap();
        let back = remove_entry(&added, Section::Packages, "ripgrep").unwrap();
        assert_eq!(back, CFG, "a round trip must not reflow the file");
    }

    #[test]
    fn a_name_is_matched_past_comments_and_quoting() {
        assert!(value_is("jq", "jq"));
        assert!(value_is("jq   # a comment worth keeping", "jq"));
        assert!(value_is("\"jq\"", "jq"));
        assert!(value_is(" fd, from: apt }", "fd"));
        // ...and does not match a different package whose name is a prefix.
        assert!(!value_is("jq-extra", "jq"));
        assert!(!value_is("ripgrep", "jq"));
    }

    #[test]
    fn a_language_comes_out_of_its_own_section() {
        let cfg = "version: 0\nlanguages:\n  - name: rust\n    installer: rustup\n  - name: go\n    installer: mise\npackages:\n  - name: rust\n    from: apt\n";
        let out = remove_entry(cfg, Section::Languages, "rust").unwrap();
        let doc: serde_yaml_ng::Value = serde_yaml_ng::from_str(&out).unwrap();
        assert_eq!(doc["languages"].as_sequence().unwrap().len(), 1);
        // The package that happens to share the name is untouched.
        assert_eq!(doc["packages"].as_sequence().unwrap().len(), 1);
    }

    #[test]
    fn removing_the_only_entry_leaves_a_parseable_file() {
        let cfg = "version: 0\npackages:\n  - name: jq\n    from: apt\nfiles: []\n";
        let out = remove_entry(cfg, Section::Packages, "jq").unwrap();
        let doc: serde_yaml_ng::Value = serde_yaml_ng::from_str(&out).unwrap();
        assert!(doc["packages"].is_null() || doc["packages"].as_sequence().is_none_or(|s| s.is_empty()));
    }
}
