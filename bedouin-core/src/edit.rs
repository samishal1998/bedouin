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
    Files,
    Repos,
    Links,
}

impl Section {
    fn key(self) -> &'static str {
        match self {
            Self::Packages => "packages:",
            Self::Languages => "languages:",
            Self::Files => "files:",
            Self::Repos => "repos:",
            Self::Links => "links:",
        }
    }

    /// The field that says which entry this is.
    ///
    /// A package and a language have names. A file, a repo and a link are
    /// identified by where they land -- there is nothing else unique about
    /// them, and two links to the same source are a normal thing to write.
    pub fn id_field(self) -> &'static str {
        match self {
            Self::Packages | Self::Languages => "name",
            Self::Files | Self::Repos | Self::Links => "dest",
        }
    }

    /// Parse a section name as the config spells it. Closed vocabulary: an
    /// unknown one is an error rather than an edit aimed at nowhere.
    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "packages" => Self::Packages,
            "languages" => Self::Languages,
            "files" => Self::Files,
            "repos" => Self::Repos,
            "links" => Self::Links,
            _ => return None,
        })
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Packages => "package",
            Self::Languages => "language",
            Self::Files => "file",
            Self::Repos => "repo",
            Self::Links => "link",
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
    lines.iter().position(|l| *l == section.key())
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

/// Does this line begin a list entry identified by `name`?
///
/// `field` is the section's identifying key -- `name:` for a package,
/// `dest:` for a file. Handles both the block form (`- name: jq`, with the
/// key on the same line or the next) and the flow form
/// (`- { name: jq, from: apt }`).
fn entry_names(lines: &[&str], at: usize, field: &str, name: &str) -> bool {
    let line = lines[at].trim_start();
    let Some(rest) = line.strip_prefix("- ") else {
        return false;
    };
    let rest = rest.trim();
    let key = format!("{field}:");
    let matches_here = |s: &str| s.split_once(&key).is_some_and(|(_, v)| value_is(v, name));
    if rest.contains(&key) {
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
        if entry_names(&lines, i, section.id_field(), name) {
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
    if let Some(i) = items.iter().position(|i| {
        i.get(section.id_field())
            .is_some_and(|n| scalar_is(n, name))
    }) {
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

/// Whether this text declares that entry at all.
///
/// `includes:` means the config a plan is built from is the merge of several
/// files, while every edit here rewrites one. An entry that came from an
/// included file is not editable through this text -- the edits refuse it
/// safely, but a UI wants to know before offering the button.
pub fn has_entry(text: &str, section: Section, name: &str) -> bool {
    let lines: Vec<&str> = text.lines().collect();
    locate_entry(&lines, section, name).is_ok()
}

/// The value of `key:` inside one entry, exactly as the file writes it.
///
/// The seed for an edit form, and it has to be the raw text rather than the
/// resolved value. `from: { macos: brew, default: apt }` RESOLVES to `apt` on
/// Linux; a form seeded from that shows `apt`, and saving it writes `apt` over
/// the mapping and deletes the macOS arm. The file still parses, the edit's
/// own checks pass, and the breakage is invisible on the machine that caused
/// it.
///
/// `None` when the entry is inline (`- { … }`) or the value opens a nested
/// block: neither is a single scalar a one-line form can round-trip, and
/// guessing would flatten it.
pub fn raw_field(text: &str, section: Section, name: &str, key: &str) -> Option<String> {
    let lines: Vec<&str> = text.lines().collect();
    let id = section.id_field();
    let start = lines
        .iter()
        .position(|l| l.trim_start().starts_with(section.key()))?;
    let mut entry = None;
    for (i, l) in lines.iter().enumerate().skip(start + 1) {
        let t = l.trim_start();
        // Out of the section entirely.
        if !l.starts_with(' ') && !t.is_empty() && !t.starts_with('#') {
            break;
        }
        if t.starts_with("- ") || t.starts_with('-') {
            let needle = format!("{id}: {name}");
            let is_ours = t.contains(&needle) && {
                // `jq` must not match `jq-extra`.
                let after = t.split(&needle).nth(1).unwrap_or("");
                after.is_empty() || after.starts_with([',', ' ', '}', '\n'])
            };
            entry = if is_ours { Some(i) } else { None };
            // An inline entry holds everything on one line; a form cannot
            // round-trip it, and the commit says so when you try.
            if is_ours && t.starts_with("- {") {
                return None;
            }
            if is_ours && t.starts_with(&format!("- {id}:")) {
                continue;
            }
        }
        let Some(e) = entry else { continue };
        if i == e {
            continue;
        }
        if let Some(v) = t.strip_prefix(&format!("{key}:")) {
            let v = v.trim();
            // A block value (`key:` then indented lines) is not a scalar.
            return if v.is_empty() {
                None
            } else {
                Some(v.to_string())
            };
        }
    }
    None
}

/// How a value has to be written to survive a YAML round trip.
///
/// Left bare when it is an unambiguous plain scalar, quoted otherwise --
/// because `dest: ~/.config/nvim` is fine but `version: 1.80` is a float,
/// `mode: 0644` is an int, and a value that merely *starts* `~` is null.
/// Quoting everything would be safe and would also make every entry this
/// writes look unlike every entry the user wrote by hand.
fn scalar(v: &str) -> String {
    let plain = !v.is_empty()
        && v.starts_with(|c: char| c.is_ascii_alphabetic() || c == '_')
        && v.chars()
            .all(|c| c.is_ascii_alphanumeric() || "._+-/".contains(c))
        && !matches!(
            v.to_ascii_lowercase().as_str(),
            "true" | "false" | "null" | "yes" | "no" | "on" | "off"
        );
    if plain {
        v.to_string()
    } else {
        format!("\"{}\"", v.replace('\\', "\\\\").replace('"', "\\\""))
    }
}

/// Append an entry to one of the list sections, creating the section if it is
/// absent.
///
/// The insert mirror of [`remove_entry`], with the same verify-by-reparsing:
/// this writes the file the user keeps in git, so it appends and touches
/// nothing else. `fields` is written in the order given, so the result reads
/// the way a person would have typed it.
pub fn add_entry(text: &str, section: Section, fields: &[(&str, &str)]) -> Result<String> {
    let key = section.id_field();
    let name = fields
        .iter()
        .find(|(k, _)| *k == key)
        .map(|(_, v)| *v)
        .filter(|v| !v.trim().is_empty())
        .ok_or_else(|| {
            ConfigError::new(format!(
                "a {} needs a `{key}:` -- that is what names it",
                section.label()
            ))
        })?;

    if serde_yaml_ng::from_str::<serde_yaml_ng::Value>(text)
        .ok()
        .is_some_and(|d| still_present(&d, section, name))
    {
        return Err(ConfigError::new(format!(
            "`{name}` is already in this config"
        )));
    }

    let entry: String = fields
        .iter()
        .filter(|(_, v)| !v.trim().is_empty())
        .enumerate()
        .map(|(i, (k, v))| {
            let lead = if i == 0 { "  - " } else { "    " };
            format!("{lead}{k}: {}\n", scalar(v))
        })
        .collect();

    let lines: Vec<&str> = text.lines().collect();
    let out = match section_head(&lines, section) {
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
            let mut kept: Vec<String> = lines[..=last_content]
                .iter()
                .map(|s| (*s).to_string())
                .collect();
            kept.push(entry.trim_end().to_string());
            kept.extend(lines[last_content + 1..].iter().map(|s| (*s).to_string()));
            kept.join("\n")
        }
        None => {
            let mut base = text.trim_end().to_string();
            base.push_str("\n\n");
            base.push_str(section.key());
            base.push('\n');
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
    if !still_present(&parsed, section, name) {
        return Err(ConfigError::new(format!(
            "could not cleanly add `{name}`. Refusing to guess -- add it by hand"
        )));
    }
    Ok(out)
}

/// Append a package to `packages:`. The shape `bedouin add` reduces to.
pub fn add_package(text: &str, name: &str, from: &str, version: Option<&str>) -> Result<String> {
    let mut fields = vec![("name", name), ("from", from)];
    if let Some(v) = version {
        fields.push(("version", v));
    }
    add_entry(text, Section::Packages, &fields)
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
        .take_while(|i| {
            lines[*i].trim().is_empty() || indent_of(lines[*i]) > indent_of(lines[head])
        })
        .find(|i| entry_names(&lines, *i, "name", package))
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
    let mut out: Vec<String> = lines[..content_line]
        .iter()
        .map(|s| (*s).to_string())
        .collect();
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

/// Set a simple `key: value` field on one named entry, adding it if absent.
///
/// The shape every `bedouin add --…` convenience reduces to. Verified the same
/// way as everything else here: the parsed result must equal the parsed
/// original plus exactly this change.
pub fn set_field(
    text: &str,
    section: Section,
    name: &str,
    key: &str,
    value: &str,
) -> Result<String> {
    let lines: Vec<&str> = text.lines().collect();
    let (from, to) = locate_entry(&lines, section, name)?;
    let nl = newline_of(text);

    // The entry's own field indent, taken from a sibling rather than assumed.
    let indent = (from + 1..to)
        .find(|i| !lines[*i].trim().is_empty())
        .map(|i| indent_of(lines[i]))
        .unwrap_or(indent_of(lines[from]) + 2);
    let line = format!("{}{key}: {value}", " ".repeat(indent));

    let existing = (from..to).find(|i| {
        let t = lines[*i].trim_start();
        indent_of(lines[*i]) == indent && t.starts_with(&format!("{key}:"))
    });

    let mut out: Vec<String> = lines.iter().map(|s| (*s).to_string()).collect();
    match existing {
        Some(i) => out[i] = line,
        // After the entry's last content line, so it reads in declaration order.
        None => {
            let at = (from..to)
                .rev()
                .find(|i| !lines[*i].trim().is_empty())
                .unwrap_or(from);
            out.insert(at + 1, line);
        }
    }
    let mut out = out.join(nl);
    if text.ends_with('\n') {
        out.push_str(nl);
    }
    verify_only_change(text, &out, |doc| {
        entry_of(doc, section, name)
            .and_then(|e| e.get(key))
            .is_some()
    })?;
    Ok(out)
}

/// Remove an alias, globally or from one package.
///
/// The mirror of [`set_alias`]. Emptying the block takes the block with it:
/// a bare `aliases:` is null rather than an empty map, and leaving one behind
/// turns a delete into a config that fails to resolve.
pub fn remove_alias(text: &str, package: Option<&str>, alias: &str) -> Result<String> {
    let lines: Vec<&str> = text.lines().collect();
    let nl = newline_of(text);

    // The `aliases:` block this alias would live in -- the top-level one, or
    // the one nested under a named package.
    let head = match package {
        None => lines
            .iter()
            .position(|l| *l == "aliases:")
            .ok_or_else(|| ConfigError::new("this config has no `aliases:` section"))?,
        Some(pkg) => {
            let (from, to) = locate_entry(&lines, Section::Packages, pkg)?;
            (from..to)
                .find(|i| lines[*i].trim_start() == "aliases:")
                .ok_or_else(|| ConfigError::new(format!("`{pkg}` has no aliases in this config")))?
        }
    };
    let (_, end) = block_span(&lines, head);

    let key = format!("{alias}:");
    let at = (head + 1..end)
        .find(|i| lines[*i].trim_start().starts_with(&key))
        .ok_or_else(|| {
            ConfigError::new(match package {
                Some(p) => format!("`{p}` has no alias `{alias}`"),
                None => format!("there is no alias `{alias}` in this config"),
            })
        })?;

    // The last entry in the block: the block goes too.
    let only = (head + 1..end)
        .filter(|i| !lines[*i].trim().is_empty())
        .count()
        == 1;
    let (from, to) = if only { (head, end) } else { (at, at + 1) };

    let mut kept: Vec<&str> = Vec::with_capacity(lines.len());
    kept.extend_from_slice(&lines[..from]);
    kept.extend_from_slice(&lines[to..]);
    let mut out = kept.join(nl);
    if text.ends_with('\n') {
        out.push_str(nl);
    }

    // The document is allowed to become exactly itself minus that one alias.
    let before: serde_yaml_ng::Value = serde_yaml_ng::from_str(text)
        .map_err(|e| ConfigError::new(format!("this config does not parse: {e}")))?;
    let parsed: serde_yaml_ng::Value = serde_yaml_ng::from_str(&out).map_err(|e| {
        ConfigError::new(format!(
            "removing `{alias}` would leave a file that no longer parses: {e}\n  \
             Refusing to write it. Remove it by hand"
        ))
    })?;
    let mut expected = before.clone();
    let map = match package {
        None => expected.get_mut("aliases"),
        Some(pkg) => expected
            .get_mut("packages")
            .and_then(|v| v.as_sequence_mut())
            .and_then(|items| {
                items
                    .iter_mut()
                    .find(|e| e.get("name").is_some_and(|n| scalar_is(n, pkg)))
            })
            .and_then(|e| e.get_mut("aliases")),
    };
    if let Some(m) = map.and_then(|m| m.as_mapping_mut()) {
        m.remove(serde_yaml_ng::Value::String(alias.to_string()));
        if m.is_empty() {
            // An emptied block was deleted above, so the parsed document has
            // no key at all -- match that rather than an empty mapping.
            match package {
                None => {
                    if let Some(d) = expected.as_mapping_mut() {
                        d.remove(serde_yaml_ng::Value::String("aliases".into()));
                    }
                }
                Some(pkg) => {
                    if let Some(e) = expected
                        .get_mut("packages")
                        .and_then(|v| v.as_sequence_mut())
                        .and_then(|items| {
                            items
                                .iter_mut()
                                .find(|e| e.get("name").is_some_and(|n| scalar_is(n, pkg)))
                        })
                        .and_then(|e| e.as_mapping_mut())
                    {
                        e.remove(serde_yaml_ng::Value::String("aliases".into()));
                    }
                }
            }
        }
    }
    if parsed != expected {
        return Err(ConfigError::new(format!(
            "could not cleanly remove `{alias}`: the edit would change more than \
             that one alias.\n  Refusing to write it. Remove it by hand"
        )));
    }
    Ok(out)
}

/// Take a `key:` back out of one entry.
///
/// Not the same as setting it empty. `version:` with nothing after it is
/// null, and null is a value -- an absent `version:` means "latest", while a
/// present-but-null one is a config saying something it does not mean. A form
/// that clears a field wants this, not `set_field(.., "")`.
pub fn unset_field(text: &str, section: Section, name: &str, key: &str) -> Result<String> {
    let lines: Vec<&str> = text.lines().collect();
    let (from, to) = locate_entry(&lines, section, name)?;
    let want = format!("{key}:");

    let at = (from..to)
        .find(|i| {
            let t = lines[*i].trim_start();
            t == want || t.starts_with(&format!("{key}: ")) || t.starts_with(&format!("{key}:\t"))
        })
        .ok_or_else(|| ConfigError::new(format!("`{name}` has no `{key}:` to remove",)))?;

    // A value that opens a nested block takes its block with it.
    let indent = indent_of(lines[at]);
    let mut end = at + 1;
    while end < to && (lines[end].trim().is_empty() || indent_of(lines[end]) > indent) {
        end += 1;
    }
    while end > at + 1 && lines[end - 1].trim().is_empty() {
        end -= 1;
    }

    let mut kept: Vec<&str> = Vec::with_capacity(lines.len());
    kept.extend_from_slice(&lines[..at]);
    kept.extend_from_slice(&lines[end..]);
    let nl = newline_of(text);
    let mut out = kept.join(nl);
    if text.ends_with('\n') {
        out.push_str(nl);
    }

    verify_only_change(text, &out, |doc| {
        entry_of(doc, section, name).is_some_and(|e| e.get(key).is_none())
    })?;
    Ok(out)
}

/// Add an alias, globally or scoped to a package.
pub fn set_alias(text: &str, package: Option<&str>, alias: &str, value: &str) -> Result<String> {
    let lines: Vec<&str> = text.lines().collect();
    let nl = newline_of(text);
    // Quoted, because an alias value is shell and routinely contains `:` and
    // `#`, either of which would change what the YAML means.
    let entry_value = format!("{alias}: {}", yaml_quote(value));

    let (block_start, block_end, indent) = match package {
        None => match lines.iter().position(|l| *l == "aliases:") {
            Some(head) => {
                let (_, end) = block_span(&lines, head);
                let indent = (head + 1..end)
                    .find(|i| !lines[*i].trim().is_empty())
                    .map(|i| indent_of(lines[i]))
                    .unwrap_or(2);
                (head, end, indent)
            }
            // No `aliases:` block yet: make one, before `packages:` so the
            // file keeps reading top-down.
            None => {
                let at = section_head(&lines, Section::Packages).unwrap_or(lines.len());
                let mut out: Vec<String> = lines.iter().map(|s| (*s).to_string()).collect();
                out.insert(at, String::new());
                out.insert(at + 1, "aliases:".into());
                out.insert(at + 2, format!("  {entry_value}"));
                let mut joined = out.join(nl);
                if text.ends_with('\n') {
                    joined.push_str(nl);
                }
                return verify_only_change(text, &joined, |doc| {
                    doc.get("aliases")
                        .and_then(|a| a.get(alias))
                        .and_then(|v| v.as_str())
                        == Some(value)
                })
                .map(|()| joined);
            }
        },
        Some(pkg) => {
            let (from, to) = locate_entry(&lines, Section::Packages, pkg)?;
            let field_indent = (from + 1..to)
                .find(|i| !lines[*i].trim().is_empty())
                .map(|i| indent_of(lines[i]))
                .unwrap_or(indent_of(lines[from]) + 2);
            match (from..to).find(|i| {
                indent_of(lines[*i]) == field_indent && lines[*i].trim_start() == "aliases:"
            }) {
                Some(head) => {
                    let (_, end) = block_span(&lines, head);
                    (head, end, field_indent + 2)
                }
                None => {
                    // No `aliases:` on this package yet.
                    let at = (from..to)
                        .rev()
                        .find(|i| !lines[*i].trim().is_empty())
                        .unwrap_or(from);
                    let mut out: Vec<String> = lines.iter().map(|s| (*s).to_string()).collect();
                    out.insert(at + 1, format!("{}aliases:", " ".repeat(field_indent)));
                    out.insert(
                        at + 2,
                        format!("{}{entry_value}", " ".repeat(field_indent + 2)),
                    );
                    let mut joined = out.join(nl);
                    if text.ends_with('\n') {
                        joined.push_str(nl);
                    }
                    return verify_only_change(text, &joined, |doc| {
                        entry_of(doc, Section::Packages, pkg)
                            .and_then(|e| e.get("aliases"))
                            .and_then(|a| a.get(alias))
                            .and_then(|v| v.as_str())
                            == Some(value)
                    })
                    .map(|()| joined);
                }
            }
        }
    };

    let mut out: Vec<String> = lines.iter().map(|s| (*s).to_string()).collect();
    let existing = (block_start + 1..block_end)
        .find(|i| lines[*i].trim_start().starts_with(&format!("{alias}:")));
    let line = format!("{}{entry_value}", " ".repeat(indent));
    match existing {
        Some(i) => out[i] = line,
        None => {
            let at = (block_start..block_end)
                .rev()
                .find(|i| !lines[*i].trim().is_empty())
                .unwrap_or(block_start);
            out.insert(at + 1, line);
        }
    }
    let mut joined = out.join(nl);
    if text.ends_with('\n') {
        joined.push_str(nl);
    }
    let check = |doc: &serde_yaml_ng::Value| match package {
        None => {
            doc.get("aliases")
                .and_then(|a| a.get(alias))
                .and_then(|v| v.as_str())
                == Some(value)
        }
        Some(pkg) => {
            entry_of(doc, Section::Packages, pkg)
                .and_then(|e| e.get("aliases"))
                .and_then(|a| a.get(alias))
                .and_then(|v| v.as_str())
                == Some(value)
        }
    };
    verify_only_change(text, &joined, check)?;
    Ok(joined)
}

/// Quote a value that is going into YAML as a scalar.
///
/// Alias values are shell, and shell is full of `:` and `#` -- both of which
/// change what a bare YAML scalar means.
fn yaml_quote(v: &str) -> String {
    if v.contains('\'') {
        format!("\"{}\"", v.replace('\\', "\\\\").replace('"', "\\\""))
    } else {
        format!("'{v}'")
    }
}

/// The half-open line range of a `key:` block, given its header line.
fn block_span(lines: &[&str], head: usize) -> (usize, usize) {
    let base = indent_of(lines[head]);
    let mut end = head + 1;
    while end < lines.len() {
        let l = lines[end];
        if !l.trim().is_empty() && indent_of(l) <= base {
            break;
        }
        end += 1;
    }
    while end > head + 1 && lines[end - 1].trim().is_empty() {
        end -= 1;
    }
    (head, end)
}

fn locate_entry(lines: &[&str], section: Section, name: &str) -> Result<(usize, usize)> {
    let head = section_head(lines, section).ok_or_else(|| {
        ConfigError::new(format!("this config has no `{}` section", section.key()))
    })?;
    let head_indent = indent_of(lines[head]);
    let start = (head + 1..lines.len())
        .take_while(|i| lines[*i].trim().is_empty() || indent_of(lines[*i]) > head_indent)
        .find(|i| entry_names(lines, *i, section.id_field(), name))
        .ok_or_else(|| {
            ConfigError::new(format!(
                "no {} named `{name}` in this config",
                section.label()
            ))
        })?;
    Ok(entry_span(lines, start))
}

fn entry_of<'a>(
    doc: &'a serde_yaml_ng::Value,
    section: Section,
    name: &str,
) -> Option<&'a serde_yaml_ng::Value> {
    doc.get(section.key().trim_end_matches(':'))?
        .as_sequence()?
        .iter()
        .find(|e| {
            e.get(section.id_field())
                .is_some_and(|n| scalar_is(n, name))
        })
}

/// The guard every edit here shares: the result parses, the change landed, and
/// nothing else moved.
fn verify_only_change(
    before_text: &str,
    after_text: &str,
    landed: impl Fn(&serde_yaml_ng::Value) -> bool,
) -> Result<()> {
    let after: serde_yaml_ng::Value = serde_yaml_ng::from_str(after_text).map_err(|e| {
        ConfigError::new(format!(
            "that edit would leave a file that no longer parses: {e}\n  \
             Refusing to write it. Make the change by hand"
        ))
    })?;
    if !landed(&after) {
        return Err(ConfigError::new(
            "could not cleanly make that edit. Refusing to guess -- make it by hand",
        ));
    }

    // And nothing else moved. `landed` only proves the change arrived; it says
    // nothing about what else the text surgery hit on the way. Every edit that
    // comes through here sets one value, so the documents are allowed to
    // differ in one place at most.
    let before: serde_yaml_ng::Value = serde_yaml_ng::from_str(before_text)
        .map_err(|e| ConfigError::new(format!("this config does not parse: {e}")))?;
    // At most one, not exactly one: committing a value back unchanged is a
    // legitimate thing to do from a form, and `landed` has already proved the
    // value is there.
    if differences(&before, &after) > 1 {
        return Err(ConfigError::new(
            "could not cleanly make that edit: it would change more than the one \
             value it was meant to.\n  Refusing to write it. Make the change by hand",
        ));
    }
    Ok(())
}

/// How many places two documents differ, counted to a maximum of two.
///
/// Two is all any caller needs: one is the edit, more than one is a refusal.
/// Stopping there also keeps a whole-file rewrite from being counted leaf by
/// leaf.
fn differences(a: &serde_yaml_ng::Value, b: &serde_yaml_ng::Value) -> usize {
    use serde_yaml_ng::Value as V;
    match (a, b) {
        (V::Mapping(x), V::Mapping(y)) => {
            let mut n = 0;
            for (k, v) in x {
                match y.get(k) {
                    Some(w) => n += differences(v, w),
                    None => n += 1, // a key this edit dropped
                }
                if n > 1 {
                    return n;
                }
            }
            // Keys the edit introduced.
            n += y.iter().filter(|(k, _)| x.get(k).is_none()).count();
            n.min(2)
        }
        (V::Sequence(x), V::Sequence(y)) if x.len() == y.len() => {
            let mut n = 0;
            for (v, w) in x.iter().zip(y) {
                n += differences(v, w);
                if n > 1 {
                    return n;
                }
            }
            n
        }
        // A list that changed length is not something any edit through here
        // does, so it is more than one change by definition.
        (V::Sequence(_), V::Sequence(_)) => 2,
        _ => usize::from(a != b),
    }
}

fn still_present(doc: &serde_yaml_ng::Value, section: Section, name: &str) -> bool {
    let key = section.key().trim_end_matches(':');
    doc.get(key)
        .and_then(|v| v.as_sequence())
        .is_some_and(|items| {
            items.iter().any(|i| {
                i.get(section.id_field())
                    .is_some_and(|n| scalar_is(n, name))
            })
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Files, repos and links are keyed by where they land rather than by a
    /// name, which is the whole reason `Section::id_field` exists.
    const DEST_KEYED: &str = r#"version: 0
shell: bash

packages:
  - name: jq
    from: apt

# where my dotfiles go
files:
  - { src: templates/gitconfig.j2, dest: "~/.gitconfig" }
  - src: templates/starship.j2
    dest: "~/.config/starship.toml"
"#;

    #[test]
    fn one_change_is_one_change_and_two_is_a_refusal() {
        // The guard `verify_only_change` leans on. It used to end with
        // `let _ = before_text;` -- the "nothing else moved" half of its own
        // doc comment was never implemented, so a set_field whose surgery hit
        // a second place would have been written out.
        let doc = |y: &str| serde_yaml_ng::from_str::<serde_yaml_ng::Value>(y).unwrap();
        let base = doc("a: 1\nb: 2\nps:\n  - name: jq\n    from: apt\n");

        // Zero matters: a form that commits a value back unchanged is an
        // edit that must be allowed through, not refused as suspicious.
        assert_eq!(differences(&base, &base), 0);
        assert_eq!(
            differences(
                &base,
                &doc("a: 1\nb: 3\nps:\n  - name: jq\n    from: apt\n")
            ),
            1,
            "one scalar"
        );
        assert_eq!(
            differences(
                &base,
                &doc("a: 1\nb: 2\nps:\n  - name: jq\n    from: cargo\n")
            ),
            1,
            "one value inside a list entry"
        );
        assert_eq!(
            differences(
                &base,
                &doc("a: 1\nb: 2\nc: 3\nps:\n  - name: jq\n    from: apt\n")
            ),
            1,
            "one added key"
        );
        assert_eq!(
            differences(
                &base,
                &doc("a: 9\nb: 9\nps:\n  - name: jq\n    from: apt\n")
            ),
            2,
            "two scalars must not pass for one"
        );
        assert_eq!(
            differences(&base, &doc("a: 1\nps:\n  - name: jq\n    from: apt\n")),
            1,
            "one dropped key"
        );
        assert_eq!(
            differences(&base, &doc("a: 1\nb: 2\nps: []\n")),
            2,
            "a list that changed length is never one edit"
        );
    }

    #[test]
    fn an_entry_from_an_included_file_is_not_in_this_text() {
        // `includes:` merges several files into one config while every edit
        // here rewrites one. The edits already refuse what they cannot find;
        // this is so a UI can stop offering the button first.
        assert!(has_entry(CFG, Section::Packages, "jq"));
        assert!(
            has_entry(CFG, Section::Packages, "fd"),
            "the flow form counts"
        );
        assert!(!has_entry(CFG, Section::Packages, "ripgrep"));
        assert!(!has_entry(CFG, Section::Languages, "jq"), "wrong section");
    }

    #[test]
    fn clearing_a_field_removes_it_rather_than_nulling_it() {
        // `version:` with nothing after it parses as null, and null is a
        // value. An absent version means "latest"; a null one is a config
        // saying something nobody typed.
        let cfg = "version: 0\nshell: bash\npackages:\n  - name: jq          # a comment worth keeping\n    from: apt\n    version: \"1.7\"\n";
        let out = unset_field(cfg, Section::Packages, "jq", "version").unwrap();
        let doc: serde_yaml_ng::Value = serde_yaml_ng::from_str(&out).unwrap();
        let jq = doc["packages"]
            .as_sequence()
            .unwrap()
            .iter()
            .find(|e| e["name"].as_str() == Some("jq"))
            .unwrap();
        assert!(jq.get("version").is_none(), "still there:\n{out}");
        assert_eq!(
            jq["from"].as_str(),
            Some("apt"),
            "it took a sibling with it"
        );
        assert!(
            out.contains("# a comment worth keeping"),
            "the comment was collateral"
        );

        let e = unset_field(cfg, Section::Packages, "jq", "nosuch").unwrap_err();
        assert!(e.to_string().contains("no `nosuch:`"), "{e}");
    }

    #[test]
    fn an_alias_can_be_taken_back_out_again() {
        let cfg = "version: 0\nshell: bash\n\naliases:\n  ll: ls -alh\n  gs: git status\n\npackages:\n  - name: jq\n    from: apt\n";
        let out = remove_alias(cfg, None, "gs").unwrap();
        assert!(!out.contains("git status"));
        assert!(out.contains("ll: ls -alh"), "it took the wrong one");
        assert!(out.contains("packages:"));

        let e = remove_alias(cfg, None, "nope").unwrap_err();
        assert!(e.to_string().contains("no alias `nope`"), "{e}");
    }

    #[test]
    fn emptying_an_alias_block_removes_the_block_with_it() {
        // A bare `aliases:` is null, not an empty map. Leaving one behind
        // turns a delete into a config that will not resolve.
        let cfg = "version: 0\nshell: bash\n\naliases:\n  ll: ls -alh\n\npackages:\n  - name: jq\n    from: apt\n";
        let out = remove_alias(cfg, None, "ll").unwrap();
        assert!(
            !out.contains("aliases:"),
            "a bare `aliases:` was left:\n{out}"
        );
        let doc: serde_yaml_ng::Value = serde_yaml_ng::from_str(&out).unwrap();
        assert!(doc.get("aliases").is_none());
        assert!(doc.get("packages").is_some(), "it took the packages too");
    }

    #[test]
    fn a_package_scoped_alias_is_removed_from_that_package_only() {
        let cfg = "version: 0\nshell: bash\n\naliases:\n  ll: ls -alh\n\npackages:\n  - name: fd-find\n    from: apt\n    aliases:\n      fd: fd-find\n      f: fd-find\n";
        let out = remove_alias(cfg, Some("fd-find"), "f").unwrap();
        assert!(out.contains("fd: fd-find"), "it took the wrong one:\n{out}");
        assert!(out.contains("ll: ls -alh"), "it reached the global block");
        let doc: serde_yaml_ng::Value = serde_yaml_ng::from_str(&out).unwrap();
        assert!(doc["packages"][0]["aliases"].get("f").is_none());
    }

    #[test]
    fn an_entry_identified_by_dest_can_be_found_and_removed() {
        // `locate_entry` used to look for `name:` and nothing else, so a file
        // was invisible to every edit in this module.
        let out = remove_entry(DEST_KEYED, Section::Files, "~/.gitconfig").unwrap();
        assert!(
            !out.contains("gitconfig.j2"),
            "the flow-form entry survived"
        );
        assert!(out.contains("starship.toml"), "it took the wrong entry");
        assert!(
            out.contains("# where my dotfiles go"),
            "the comment above the section was collateral"
        );

        // The block form too, not just the flow form.
        let out = remove_entry(DEST_KEYED, Section::Files, "~/.config/starship.toml").unwrap();
        assert!(!out.contains("starship.toml"));
        assert!(out.contains("gitconfig.j2"));
    }

    #[test]
    fn a_section_that_does_not_exist_yet_is_created_to_hold_the_first_entry() {
        let out = add_entry(
            DEST_KEYED,
            Section::Links,
            &[
                ("src", "~/projects/dotfiles/nvim"),
                ("dest", "~/.config/nvim"),
            ],
        )
        .unwrap();
        assert!(out.contains("links:"), "no links section was created");
        // `~` alone is null in YAML and `~/...` is not, but quoting is what
        // keeps that from being a thing anyone has to know.
        assert!(out.contains(r#"dest: "~/.config/nvim""#), "got:\n{out}");
        let doc: serde_yaml_ng::Value = serde_yaml_ng::from_str(&out).unwrap();
        assert_eq!(
            doc["links"][0]["dest"].as_str(),
            Some("~/.config/nvim"),
            "it did not round trip as a string"
        );
        assert!(out.contains("packages:"), "it clobbered what was there");
    }

    #[test]
    fn adding_something_that_is_already_there_is_refused_not_duplicated() {
        let e = add_entry(
            DEST_KEYED,
            Section::Files,
            &[("src", "x.j2"), ("dest", "~/.gitconfig")],
        )
        .unwrap_err();
        assert!(e.to_string().contains("already in this config"), "{e}");
    }

    #[test]
    fn a_value_is_quoted_when_leaving_it_bare_would_change_what_it_means() {
        // Bare where bare is unambiguous, quoted where YAML would read it as
        // something other than the text the user typed.
        assert_eq!(scalar("apt"), "apt");
        assert_eq!(scalar("stable"), "stable");
        assert_eq!(scalar("templates/git.j2"), "templates/git.j2");
        for v in ["1.80", "0644", "~/.config/nvim", "no", "true", "~", ""] {
            assert!(
                scalar(v).starts_with('"'),
                "`{v}` must be quoted or it is not a string any more"
            );
        }
        assert_eq!(
            scalar("https://github.com/o/r"),
            r#""https://github.com/o/r""#
        );
    }

    #[test]
    fn an_entry_with_no_identifying_field_is_refused() {
        let e = add_entry(DEST_KEYED, Section::Files, &[("src", "x.j2")]).unwrap_err();
        assert!(e.to_string().contains("`dest:`"), "{e}");
    }

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
        assert!(
            out.contains("eval \"$(zellij setup)\""),
            "and its nested block"
        );
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
        assert_eq!(
            last["version"].as_str(),
            Some("0.9.4"),
            "quoted, so 1.80 stays 1.80"
        );
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
            doc["packages"][1]["rc"][0]["content"]
                .as_str()
                .unwrap()
                .trim_end(),
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
        assert!(
            doc["packages"].is_null() || doc["packages"].as_sequence().is_none_or(|s| s.is_empty())
        );
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
            e.message.contains("no package named")
                || e.message.contains("more than that one entry"),
            "{e}"
        );
        // The real entries still come out cleanly.
        let out = remove_entry(PROSE, Section::Packages, "jq").unwrap();
        assert!(
            out.contains("carefully tuned zellij setup"),
            "prose survives"
        );
        assert!(
            out.contains("- name: ripgrep"),
            "and so does the note about it"
        );
    }

    #[test]
    fn a_packages_key_inside_a_block_scalar_is_not_the_section() {
        let cfg = "version: 0\nvars:\n  banner: |\n    packages:\n      - name: fd\n        from: apt\npackages:\n  - name: jq\n    from: apt\n";
        let e = remove_entry(cfg, Section::Packages, "fd").unwrap_err();
        assert!(
            e.message.contains("no package named") || e.message.contains("more than"),
            "{e}"
        );
        // And the banner is untouched by a legitimate removal.
        let out = remove_entry(cfg, Section::Packages, "jq").unwrap();
        assert!(
            out.contains("      - name: fd"),
            "the banner survives: {out}"
        );
    }

    #[test]
    fn a_name_yaml_reads_as_a_number_or_bool_is_still_guarded() {
        // `as_str()` returns None for these, which made the old guard vacuous.
        let cfg =
            "version: 0\npackages:\n  - name: 8\n    from: apt\n  - name: jq\n    from: apt\n";
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
        assert!(
            !out.replace("\r\n", "").contains('\n'),
            "no bare LF introduced: {out:?}"
        );
        let doc: serde_yaml_ng::Value = serde_yaml_ng::from_str(&out).unwrap();
        assert_eq!(doc["packages"].as_sequence().unwrap().len(), 1);
    }
}

#[cfg(test)]
mod field_tests {
    use super::*;

    const CFG: &str = r#"version: 0
shell: zsh

aliases:
  ll: 'ls -alh'

packages:
  - name: jq          # keep me
    from: apt

  - name: zellij
    from: cargo
    aliases:
      z: 'zellij'
"#;

    fn doc(s: &str) -> serde_yaml_ng::Value {
        serde_yaml_ng::from_str(s).expect("parses")
    }

    #[test]
    fn a_field_is_added_to_the_right_entry_and_nothing_else_moves() {
        let out = set_field(CFG, Section::Packages, "jq", "version", "\"1.7\"").unwrap();
        let d = doc(&out);
        assert_eq!(d["packages"][0]["version"].as_str(), Some("1.7"));
        assert_eq!(
            d["packages"][1].get("version"),
            None,
            "the sibling is untouched"
        );
        assert!(out.contains("# keep me"), "comments survive");
    }

    #[test]
    fn setting_a_field_that_is_already_there_replaces_it() {
        let once = set_field(CFG, Section::Packages, "jq", "version", "\"1.7\"").unwrap();
        let twice = set_field(&once, Section::Packages, "jq", "version", "\"1.8\"").unwrap();
        assert_eq!(doc(&twice)["packages"][0]["version"].as_str(), Some("1.8"));
        // Replaced in place, not appended -- `version: 0` at the top is the
        // schema version, so count the indented ones.
        assert_eq!(
            twice
                .lines()
                .filter(|l| l.trim_start().starts_with("version:") && l.starts_with(' '))
                .count(),
            1
        );
    }

    #[test]
    fn a_global_alias_joins_the_existing_block() {
        let out = set_alias(CFG, None, "gs", "git status").unwrap();
        let d = doc(&out);
        assert_eq!(d["aliases"]["gs"].as_str(), Some("git status"));
        assert_eq!(
            d["aliases"]["ll"].as_str(),
            Some("ls -alh"),
            "the neighbour survives"
        );
    }

    #[test]
    fn a_config_with_no_aliases_block_gains_one() {
        let bare = "version: 0\nshell: zsh\npackages:\n  - name: jq\n    from: apt\n";
        let out = set_alias(bare, None, "ll", "ls -alh").unwrap();
        let d = doc(&out);
        assert_eq!(d["aliases"]["ll"].as_str(), Some("ls -alh"));
        assert_eq!(
            d["packages"][0]["name"].as_str(),
            Some("jq"),
            "packages survive"
        );
    }

    #[test]
    fn a_package_alias_lands_on_that_package_only() {
        let out = set_alias(CFG, Some("zellij"), "zj", "zellij attach").unwrap();
        let d = doc(&out);
        assert_eq!(
            d["packages"][1]["aliases"]["zj"].as_str(),
            Some("zellij attach")
        );
        assert_eq!(
            d["packages"][1]["aliases"]["z"].as_str(),
            Some("zellij"),
            "kept"
        );
        assert_eq!(d["aliases"].get("zj"), None, "not global");
    }

    #[test]
    fn a_package_with_no_aliases_block_gains_one() {
        let out = set_alias(CFG, Some("jq"), "j", "jq").unwrap();
        assert_eq!(
            doc(&out)["packages"][0]["aliases"]["j"].as_str(),
            Some("jq")
        );
        assert!(out.contains("# keep me"));
    }

    #[test]
    fn alias_values_full_of_shell_survive_the_yaml_round_trip() {
        // `:` and `#` in a bare scalar change what the YAML means, and shell is
        // full of both.
        for v in [
            "git log --oneline | head -20",
            "echo a: b",
            "grep --color=auto # always",
            "echo 'it'\\''s fine'",
            "curl -s https://x.dev/y && echo",
        ] {
            let out = set_alias(CFG, None, "t", v).unwrap();
            assert_eq!(
                doc(&out)["aliases"]["t"].as_str(),
                Some(v),
                "round trip of {v:?}"
            );
        }
    }

    #[test]
    fn editing_something_absent_is_refused_rather_than_invented() {
        assert!(set_field(CFG, Section::Packages, "nope", "version", "1").is_err());
        assert!(set_alias(CFG, Some("nope"), "x", "y").is_err());
    }
}
