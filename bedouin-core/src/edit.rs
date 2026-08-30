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

/// The line that declares a top-level section.
///
/// Column 0 specifically: a block scalar's content is always indented deeper
/// than its own key, so nothing inside one can sit at column 0. Matching
/// `trim_start()` instead found `packages:` written *inside* a `vars:` banner
/// and scanned the wrong region of the file entirely.
fn section_head(lines: &[&str], section: Section) -> Option<usize> {
    lines
        .iter()
        .position(|l| *l == section.key())
}

/// The line ending the file actually uses.
///
/// `str::lines()` strips `\r`, so re-joining with `\n` silently rewrites every
/// line of a CRLF config -- turning a one-entry edit into a whole-file diff.
fn newline_of(text: &str) -> &'static str {
    if text.contains("\r\n") {
        "\r\n"
    } else {
        "\n"
    }
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

    let Some(head) = section_head(&lines, section) else {
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
    let nl = newline_of(text);
    let mut out = kept.join(nl);
    if text.ends_with('\n') {
        out.push_str(nl);
    }

    // Verify by re-parsing rather than trusting the surgery. Nothing here is
    // worth a mangled config.
    let parsed: serde_yaml_ng::Value = serde_yaml_ng::from_str(&out).map_err(|e| {
        ConfigError::new(format!(
            "removing `{name}` would leave a file that no longer parses: {e}\n  \
             Refusing to write it. Remove the entry by hand"
        ))
    })?;
    let before: serde_yaml_ng::Value = serde_yaml_ng::from_str(text)
        .map_err(|e| ConfigError::new(format!("this config does not parse: {e}")))?;
    let key = section.key().trim_end_matches(':');
    let mut parsed = parsed;
    // Emptying the list leaves a bare `packages:` -- null, not `[]`.
    if parsed.get(key).is_some_and(serde_yaml_ng::Value::is_null) {
        parsed[key] = serde_yaml_ng::Value::Sequence(vec![]);
    }
    // The edit is allowed to produce exactly one thing: the document minus that
    // one entry. Asking only "is the target gone" let the text scan delete
    // lines out of an unrelated entry -- a `- name: x` inside a block scalar is
    // prose, not structure, and nothing downstream could tell.
    if parsed != expected_after(&before, section, name) {
        return Err(ConfigError::new(format!(
            "could not cleanly remove `{name}`: the edit would change more than \
             that one entry.\n  Refusing to write it. Remove the entry by hand"
        )));
    }
    Ok(out)
}

/// The document the edit is allowed to produce: `before` minus exactly the
/// first `section` element named `name`.
fn expected_after(
    before: &serde_yaml_ng::Value,
    section: Section,
    name: &str,
) -> serde_yaml_ng::Value {
    let key = section.key().trim_end_matches(':');
    let mut doc = before.clone();
    let Some(items) = doc.get_mut(key).and_then(|v| v.as_sequence_mut()) else {
        return doc;
    };
    if let Some(i) = items
        .iter()
        .position(|i| i.get("name").is_some_and(|n| scalar_is(n, name)))
    {
        items.remove(i);
    }
    doc
}

/// `- name: 8` and `- name: true` are scalars too, and `as_str()` returns None
/// for both -- which made the old guard pass unconditionally for them.
fn scalar_is(v: &serde_yaml_ng::Value, name: &str) -> bool {
    match v {
        serde_yaml_ng::Value::String(s) => s == name,
        serde_yaml_ng::Value::Number(n) => n.to_string() == name,
        serde_yaml_ng::Value::Bool(b) => b.to_string() == name,
        _ => false,
    }
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
    let out = match section_head(&lines, Section::Packages) {
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
    out.push_str(newline_of(text));

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

/// Replace the `content:` of one package's rc block.
///
/// The one edit `absorb` needs. Finds the package, finds the rc entry whose
/// `file:` mentions `file_hint`, and rewrites its `content:` -- as a block
/// scalar, which is the form that survives multi-line shell code.
pub fn set_rc_content(
    text: &str,
    package: &str,
    file_hint: &str,
    new_content: &str,
) -> Result<String> {
    let lines: Vec<&str> = text.lines().collect();
    let head = section_head(&lines, Section::Packages)
        .ok_or_else(|| ConfigError::new("this config has no `packages:` section"))?;

    let start = (head + 1..lines.len())
        .take_while(|i| lines[*i].trim().is_empty() || indent_of(lines[*i]) > indent_of(lines[head]))
        .find(|i| entry_names(&lines, *i, package))
        .ok_or_else(|| ConfigError::new(format!("no package named `{package}` in this config")))?;
    let (from, to) = entry_span(&lines, start);

    // The rc entry whose `file:` names this file, then the `content:` under it.
    let file_line = (from..to)
        .find(|i| {
            let t = lines[*i].trim_start();
            (t.starts_with("- file:") || t.starts_with("file:")) && t.contains(file_hint)
        })
        .ok_or_else(|| {
            ConfigError::new(format!(
                "package `{package}` has no rc block for `{file_hint}`"
            ))
        })?;
    let content_line = (file_line..to)
        .find(|i| lines[*i].trim_start().starts_with("content:"))
        .ok_or_else(|| {
            ConfigError::new(format!("the rc block for `{file_hint}` has no `content:`"))
        })?;

    // The value runs to the next line indented no deeper than `content:`.
    let content_indent = indent_of(lines[content_line]);
    let mut value_end = content_line + 1;
    while value_end < to {
        let l = lines[value_end];
        if !l.trim().is_empty() && indent_of(l) <= content_indent {
            break;
        }
        value_end += 1;
    }

    let body_indent = " ".repeat(content_indent + 2);
    let mut out: Vec<String> = lines[..content_line].iter().map(|s| (*s).to_string()).collect();
    out.push(format!("{}content: |", " ".repeat(content_indent)));
    for l in new_content.trim_end_matches('\n').lines() {
        out.push(if l.trim().is_empty() {
            String::new()
        } else {
            format!("{body_indent}{l}")
        });
    }
    out.extend(lines[value_end..].iter().map(|s| (*s).to_string()));
    let mut out = out.join("\n");
    if text.ends_with('\n') {
        out.push('\n');
    }

    // Verify by reparsing, and verify the value actually landed -- absorb
    // writes the file the user trusts.
    let doc: serde_yaml_ng::Value = serde_yaml_ng::from_str(&out).map_err(|e| {
        ConfigError::new(format!(
            "absorbing that edit would leave a file that no longer parses: {e}\n  \
             Refusing to write it. Copy the change across by hand"
        ))
    })?;
    let landed = doc
        .get("packages")
        .and_then(|p| p.as_sequence())
        .and_then(|ps| {
            ps.iter()
                .find(|p| p.get("name").and_then(|n| n.as_str()) == Some(package))
        })
        .and_then(|p| p.get("rc"))
        .and_then(|rc| rc.as_sequence())
        .and_then(|rcs| {
            rcs.iter().find(|b| {
                b.get("file")
                    .and_then(|f| f.as_str())
                    .is_some_and(|f| f.contains(file_hint))
            })
        })
        .and_then(|b| b.get("content"))
        .and_then(|c| c.as_str())
        .map(|c| c.trim_end().to_string());
    if landed.as_deref() != Some(new_content.trim_end()) {
        return Err(ConfigError::new(format!(
            "could not cleanly absorb into `{package}`. Refusing to guess -- \
             copy the change across by hand"
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
                .any(|i| i.get("name").is_some_and(|n| scalar_is(n, name)))
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
    fn absorbing_rewrites_only_that_blocks_content() {
        let out = set_rc_content(
            CFG,
            "zellij",
            "70-zellij.zsh",
            "eval \"$(zellij setup --generate-auto-start zsh)\"\nexport ZELLIJ_AUTO_EXIT=true",
        )
        .unwrap();
        let doc: serde_yaml_ng::Value = serde_yaml_ng::from_str(&out).unwrap();
        let content = doc["packages"][1]["rc"][0]["content"].as_str().unwrap();
        assert!(content.contains("ZELLIJ_AUTO_EXIT=true"), "the edit landed");
        assert!(content.contains("generate-auto-start"));
        // Everything else in the file is untouched.
        assert!(out.contains("# a comment worth keeping"));
        assert!(out.contains("- { name: fd, from: apt }"));
        assert!(out.contains("dest: ~/.gitconfig"));
        assert_eq!(doc["packages"].as_sequence().unwrap().len(), 3);
    }

    #[test]
    fn absorbing_into_something_that_is_not_there_is_refused() {
        assert!(set_rc_content(CFG, "nope", "x", "y").is_err());
        assert!(set_rc_content(CFG, "jq", "70-zellij.zsh", "y").is_err());
    }

    #[test]
    fn absorbed_content_round_trips_through_yaml() {
        // Shell code is exactly the text most likely to break a naive writer:
        // quotes, dollars, backslashes, blank lines.
        let tricky = "alias x='it'\''s fine'\nexport P=\"$(echo \\$HOME)\"\n\nfunction f() { :; }";
        let out = set_rc_content(CFG, "zellij", "70-zellij.zsh", tricky).unwrap();
        let doc: serde_yaml_ng::Value = serde_yaml_ng::from_str(&out).unwrap();
        assert_eq!(
            doc["packages"][1]["rc"][0]["content"].as_str().unwrap().trim_end(),
            tricky
        );
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

#[cfg(test)]
mod review_regressions {
    use super::*;

    /// A `- name:` inside a block scalar is prose, not structure.
    const PROSE: &str = r#"version: 0
shell: zsh

packages:
  - name: zellij
    from: cargo
    rc:
      - file: "{{ shell.rc_dir }}/70-zellij.zsh"
        content: |
          # my carefully tuned zellij setup
          eval "$(zellij setup)"
          # NOTE: if you want ripgrep too, add:
          - name: ripgrep
            from: apt

  - name: jq
    from: apt
"#;

    #[test]
    fn a_name_inside_a_block_scalar_is_not_an_entry() {
        // This used to report success and delete zellij's entire rc block --
        // silently, with exit 0 and no backup, for a package that was never in
        // the config at all.
        let e = remove_entry(PROSE, Section::Packages, "ripgrep").unwrap_err();
        assert!(
            e.message.contains("no package named") || e.message.contains("more than that one entry"),
            "{e}"
        );
        // The real entries still come out cleanly.
        let out = remove_entry(PROSE, Section::Packages, "jq").unwrap();
        assert!(out.contains("carefully tuned zellij setup"), "prose survives");
        assert!(out.contains("- name: ripgrep"), "and so does the note about it");
    }

    #[test]
    fn a_packages_key_inside_a_block_scalar_is_not_the_section() {
        let cfg = "version: 0\nvars:\n  banner: |\n    packages:\n      - name: fd\n        from: apt\npackages:\n  - name: jq\n    from: apt\n";
        let e = remove_entry(cfg, Section::Packages, "fd").unwrap_err();
        assert!(e.message.contains("no package named") || e.message.contains("more than"), "{e}");
        // And the banner is untouched by a legitimate removal.
        let out = remove_entry(cfg, Section::Packages, "jq").unwrap();
        assert!(out.contains("      - name: fd"), "the banner survives: {out}");
    }

    #[test]
    fn a_name_yaml_reads_as_a_number_or_bool_is_still_guarded() {
        // `as_str()` returns None for these, which made the old guard vacuous.
        let cfg = "version: 0\npackages:\n  - name: 8\n    from: apt\n  - name: jq\n    from: apt\n";
        let out = remove_entry(cfg, Section::Packages, "8").unwrap();
        let doc: serde_yaml_ng::Value = serde_yaml_ng::from_str(&out).unwrap();
        assert_eq!(doc["packages"].as_sequence().unwrap().len(), 1);
        assert_eq!(doc["packages"][0]["name"].as_str(), Some("jq"));
    }

    #[test]
    fn a_crlf_config_keeps_its_line_endings() {
        // Re-joining with \n turned a one-entry removal into a whole-file diff.
        let cfg = "version: 0\r\npackages:\r\n  - name: jq\r\n    from: apt\r\n  - name: fd\r\n    from: apt\r\n";
        let out = remove_entry(cfg, Section::Packages, "fd").unwrap();
        assert!(out.contains("\r\n"), "CRLF preserved");
        assert!(!out.replace("\r\n", "").contains('\n'), "no bare LF introduced: {out:?}");
        let doc: serde_yaml_ng::Value = serde_yaml_ng::from_str(&out).unwrap();
        assert_eq!(doc["packages"].as_sequence().unwrap().len(), 1);
    }
}
