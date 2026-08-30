//! Finding, reading, and merging `bedouin.yaml`.
//!
//! Seven stages. Two orderings are load-bearing:
//!
//! **Parse per file before merging.** Merging YAML *text* first would destroy
//! the `file:line:col` every error message depends on. Each file is parsed from
//! its own text, twice: once permissively to collect `targets:`, then again
//! with the resulting names in scope. Two text parses per file; files are small,
//! and `serde_yaml_ng` only reports a location when it deserializes from text,
//! so buffering into a `Value` and typing it later would lose the span anyway.
//!
//! **Collect target names across all files before typing any of them.** A
//! target declared in `conf.d/10-targets.yaml` must be in scope when
//! `conf.d/20-packages.yaml` is deserialized; collecting per file would reject
//! configs that are correct as a whole.

use crate::host::Host;
use crate::schema::{ConfigError, RawConfig, Result};
use crate::target::{Target, Vocabulary};
use crate::value::with_known_arms;
use serde::Deserialize;
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;

/// Just enough of the file to run stage 3, ignoring everything else.
#[derive(Deserialize, Default)]
struct TargetsOnly {
    #[serde(default)]
    targets: Vec<Target>,
    #[serde(default)]
    includes: Vec<String>,
    #[serde(flatten)]
    _rest: BTreeMap<String, serde_yaml_ng::Value>,
}

#[derive(Debug)]
pub struct Loaded {
    /// The directory containing the entry file. `includes:` and `src:` resolve
    /// against this, never the process working directory, so `bedouin apply`
    /// behaves identically from any cwd.
    pub root: PathBuf,
    pub entry: PathBuf,
    pub files: Vec<PathBuf>,
    pub raw: RawConfig,
    pub vocab: Vocabulary,
}

/// `--config` -> `$BEDOUIN_CONFIG` -> `./bedouin.yaml` -> `~/.config/bedouin/`.
pub fn locate(
    explicit: Option<&Path>,
    host: &dyn Host,
    cwd: &Path,
    home: &Path,
) -> Result<PathBuf> {
    let mut tried = Vec::new();
    let mut candidates: Vec<PathBuf> = Vec::new();
    if let Some(p) = explicit {
        candidates.push(if p.is_absolute() { p.into() } else { cwd.join(p) });
    } else if let Some(p) = host.env().get("BEDOUIN_CONFIG") {
        candidates.push(PathBuf::from(p));
    } else {
        candidates.push(cwd.join("bedouin.yaml"));
        candidates.push(home.join(".config/bedouin/bedouin.yaml"));
    }
    for c in candidates {
        match host.read(&c) {
            Ok(Some(_)) => return Ok(c),
            Ok(None) => tried.push(c),
            // Present but unreadable is not absent. Reporting "no config file
            // found" while naming a path that exists sends the user to `ls`.
            Err(e) => {
                return Err(ConfigError::new(format!(
                    "{} exists but could not be read: {e}",
                    c.display()
                )))
            }
        }
    }
    Err(ConfigError::new(format!(
        "no config file found. Looked in:\n{}\n  \
         Point at one with `--config <path>` or `$BEDOUIN_CONFIG`",
        tried
            .iter()
            .map(|p| format!("  {}", p.display()))
            .collect::<Vec<_>>()
            .join("\n")
    )))
}

fn read_text(host: &dyn Host, p: &Path) -> Result<String> {
    let bytes = host
        .read(p)
        .map_err(|e| ConfigError::new(e.to_string()))?
        .ok_or_else(|| ConfigError::new(format!("{}: no such file", p.display())))?;
    String::from_utf8(bytes)
        .map_err(|_| ConfigError::new(format!("{}: not valid UTF-8", p.display())))
}

/// Attach `file:line:col` to a YAML error. `serde_yaml_ng` knows the position
/// but not which file it was reading, so the loader supplies that half.
fn yaml_err(file: &Path, e: &serde_yaml_ng::Error) -> ConfigError {
    yaml_err_in(file, None, e)
}

/// Locate a YAML error, refining the line where possible.
///
/// serde reports where the *mapping* began, not where the offending key sits,
/// so an unknown-arm error inside `{ macos: x, mcaos: y }` points at `macos`.
/// When the message names a key in backticks and the file text is available,
/// scan forward for the line that actually declares it.
fn yaml_err_in(file: &Path, text: Option<&str>, e: &serde_yaml_ng::Error) -> ConfigError {
    let msg_full = e.to_string();
    let mut line_col = e.location().map(|l| (l.line(), l.column()));
    if let (Some(text), Some((line, _))) = (text, line_col) {
        if let Some(key) = msg_full
            .split('`')
            .nth(1)
            .filter(|k| !k.is_empty() && !k.contains(' '))
        {
            let needle = format!("{key}:");
            if let Some((n, l)) = text
                .lines()
                .enumerate()
                .skip(line.saturating_sub(1))
                .find(|(_, l)| l.trim_start().starts_with(&needle))
            {
                line_col = Some((n + 1, l.len() - l.trim_start().len() + 1));
            }
        }
    }
    let at = match line_col {
        Some((l, c)) => format!("{}:{l}:{c}", file.display()),
        None => file.display().to_string(),
    };
    // serde_yaml_ng appends its own " at line N column M". The location is
    // already the prefix, so printing it twice is just noise.
    let msg = msg_full;
    let trimmed = msg
        .rfind(" at line ")
        .map_or(msg.as_str(), |i| &msg[..i])
        .trim_end();
    ConfigError::new(trimmed.to_string()).at(at)
}

/// One path segment matched against `*` and `?`. No `**`.
//
// ponytail: single-segment globs only. `conf.d/*.yaml` is the documented idiom
// and it is what a drop-in directory needs; add recursive matching if someone
// actually nests them.
fn glob_segment(pattern: &str, name: &str) -> bool {
    // Two-pointer with a single backtrack point: linear in practice, where the
    // obvious recursive version is exponential on patterns like `*a*a*a*b`.
    let (p, n) = (pattern.as_bytes(), name.as_bytes());
    let (mut pi, mut ni) = (0usize, 0usize);
    let (mut star, mut resume) = (None, 0usize);
    while ni < n.len() {
        match p.get(pi) {
            Some(b'*') => {
                star = Some(pi);
                resume = ni;
                pi += 1;
            }
            Some(b'?') => {
                pi += 1;
                ni += 1;
            }
            Some(c) if *c == n[ni] => {
                pi += 1;
                ni += 1;
            }
            _ => match star {
                Some(s) => {
                    pi = s + 1;
                    resume += 1;
                    ni = resume;
                }
                None => return false,
            },
        }
    }
    p[pi..].iter().all(|c| *c == b'*')
}

/// Expand one `includes:` entry against the config root, lexicographically.
///
/// Sorted rather than filesystem order, which is what makes `10-` / `20-`
/// prefixes mean what they look like.
fn expand(host: &dyn Host, root: &Path, pattern: &str) -> Result<Vec<PathBuf>> {
    let rel = Path::new(pattern);
    let (dir, file_pat) = match rel.parent() {
        Some(d) if !d.as_os_str().is_empty() => (
            root.join(d),
            rel.file_name().unwrap_or_default().to_string_lossy().into_owned(),
        ),
        _ => (root.to_path_buf(), pattern.to_string()),
    };
    if !file_pat.contains('*') && !file_pat.contains('?') {
        let p = dir.join(&file_pat);
        return match host.read(&p) {
            Ok(Some(_)) => Ok(vec![p]),
            _ => Err(ConfigError::new(format!(
                "`includes:` names `{pattern}`, which does not exist ({})",
                p.display()
            ))),
        };
    }
    let mut out: Vec<PathBuf> = host
        .read_dir(&dir)
        .map_err(|e| ConfigError::new(e.to_string()))?
        .into_iter()
        .filter(|p| {
            p.file_name()
                .is_some_and(|n| glob_segment(&file_pat, &n.to_string_lossy()))
        })
        .collect();
    if out.is_empty() {
        // Expanding to nothing in silence is the worst outcome available: every
        // package in that drop-in vanishes from the config, and anything
        // already in state as `owner: bedouin` is then planned for REMOVAL. A
        // one-character typo would read as "uninstall all of this".
        return Err(ConfigError::new(format!(
            "`includes:` pattern `{pattern}` matches no files\n  looked in: {}\n               Remove the pattern, or fix it -- an include that matches nothing \
             would silently drop every item it was meant to add",
            dir.display()
        )));
    }
    out.sort();
    Ok(out)
}

/// Collapse `.`/`..`, expand a leading `~`, and make the result absolute.
///
/// Item ids are built from the normalised path, so `~/.gitconfig` and
/// `/home/u/.gitconfig` are one item rather than two.
pub fn normalize(raw: &str, home: &Path, base: &Path) -> PathBuf {
    let expanded = if raw == "~" {
        home.to_path_buf()
    } else if let Some(rest) = raw.strip_prefix("~/") {
        home.join(rest)
    } else {
        let p = Path::new(raw);
        if p.is_absolute() {
            p.to_path_buf()
        } else {
            base.join(p)
        }
    };
    let mut out = PathBuf::new();
    for c in expanded.components() {
        match c {
            Component::ParentDir => {
                out.pop();
            }
            Component::CurDir => {}
            other => out.push(other.as_os_str()),
        }
    }
    out
}

/// True when `p` is inside `base` after normalisation.
pub fn contained_in(p: &Path, base: &Path) -> bool {
    // `Path::starts_with` is component-wise and purely lexical, so an
    // uncollapsed `/cfg/../evil/x.yaml` "starts with" `/cfg`. Collapse both
    // sides first. Symlinks out of the root are not chased: §1.1 trusts the
    // config's own tree, and this guard is about accidents, not adversaries.
    fn collapse(p: &Path) -> PathBuf {
        let mut out = PathBuf::new();
        for c in p.components() {
            match c {
                Component::ParentDir => {
                    out.pop();
                }
                Component::CurDir => {}
                other => out.push(other.as_os_str()),
            }
        }
        out
    }
    collapse(p).starts_with(collapse(base))
}

pub fn load(entry: &Path, host: &dyn Host) -> Result<Loaded> {
    let root = entry.parent().unwrap_or(Path::new(".")).to_path_buf();

    // 2. read: the entry file, then its includes.
    let entry_text = read_text(host, entry)?;
    let head: TargetsOnly = serde_yaml_ng::from_str(&entry_text)
        .map_err(|e| yaml_err(entry, &e))?;

    let mut files = vec![entry.to_path_buf()];
    let mut texts = vec![entry_text];
    for pattern in &head.includes {
        for p in expand(host, &root, pattern)? {
            if !contained_in(&p, &root) {
                return Err(ConfigError::new(format!(
                    "`includes:` reaches outside the config root: {}",
                    p.display()
                )));
            }
            if files.contains(&p) {
                continue;
            }
            texts.push(read_text(host, &p)?);
            files.push(p);
        }
    }

    // 3+4. collect every `targets:` across every file, then build the
    // vocabulary once. Nested `includes:` are not followed -- one level keeps
    // the ordering statement in a single place.
    let mut targets: Vec<Target> = Vec::new();
    for (path, text) in files.iter().zip(&texts) {
        let t: TargetsOnly =
            serde_yaml_ng::from_str(text).map_err(|e| yaml_err(path, &e))?;
        targets.extend(t.targets);
    }
    let vocab = Vocabulary::new(targets).map_err(|e| ConfigError::new(e.to_string()))?;
    let names: Arc<BTreeSet<String>> = Arc::new(vocab.all_names().collect());

    // 5. type each document with the arm names in scope, from its own text so
    // the span survives.
    let mut parsed: Vec<(PathBuf, RawConfig)> = Vec::new();
    for (path, text) in files.iter().zip(&texts) {
        let cfg = with_known_arms(names.clone(), || {
            serde_yaml_ng::from_str::<RawConfig>(text)
                .map_err(|e| yaml_err_in(path, Some(text), &e))
        })?;
        parsed.push((path.clone(), cfg));
    }

    // 6. merge. Lists concatenate; a repeated item id is an error naming both
    // files, because two nodes would otherwise compete for one state key.
    let mut merged = RawConfig::default();
    let mut seen_pkg: BTreeMap<String, PathBuf> = BTreeMap::new();
    let mut seen_lang: BTreeMap<String, PathBuf> = BTreeMap::new();
    for (path, cfg) in parsed {
        if cfg.version != 0 {
            return Err(ConfigError::new(format!(
                "unsupported `version: {}`; this build understands schema version 0",
                cfg.version
            ))
            .at(path.display().to_string()));
        }
        for p in &cfg.packages {
            if let Some(first) = seen_pkg.insert(p.name.clone(), path.clone()) {
                return Err(duplicate("package", &p.name, &first, &path));
            }
        }
        for l in &cfg.languages {
            if let Some(first) = seen_lang.insert(l.name.clone(), path.clone()) {
                return Err(duplicate("language", &l.name, &first, &path));
            }
        }
        merged.vars.extend(cfg.vars);
        merged.aliases.extend(cfg.aliases);
        merged.targets.extend(cfg.targets);
        merged.languages.extend(cfg.languages);
        merged.packages.extend(cfg.packages);
        merged.files.extend(cfg.files);
        if cfg.shell.is_some() {
            merged.shell = cfg.shell;
        }
        if cfg.package_managers.is_some() {
            merged.package_managers = cfg.package_managers;
        }
    }
    merged.includes = head.includes;

    if merged.packages.is_empty() && merged.languages.is_empty() && merged.files.is_empty() {
        return Err(ConfigError::new(
            "nothing declared: no packages, languages, or files.\n  \
             Refusing to plan an empty run against an existing state, which \
             would propose removing everything Bedouin manages",
        )
        .at(entry.display().to_string()));
    }

    Ok(Loaded {
        root,
        entry: entry.to_path_buf(),
        files,
        raw: merged,
        vocab,
    })
}

fn duplicate(kind: &str, name: &str, first: &Path, second: &Path) -> ConfigError {
    ConfigError::new(format!(
        "{kind} `{name}` is declared twice, so two nodes would compete for one \
         state entry\n  first:  {}\n  again:  {}",
        first.display(),
        second.display()
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::host::FakeHost;

    fn host_with(files: &[(&str, &str)]) -> FakeHost {
        files
            .iter()
            .fold(FakeHost::new(), |h, (p, c)| h.with_file(*p, c))
    }

    #[test]
    fn globs_match_within_one_segment() {
        assert!(glob_segment("*.yaml", "10-targets.yaml"));
        assert!(glob_segment("*", "anything"));
        assert!(glob_segment("2?-*.yaml", "20-packages.yaml"));
        assert!(!glob_segment("*.yaml", "notes.md"));
        assert!(!glob_segment("2?-*.yaml", "10-targets.yaml"));
    }

    #[test]
    fn paths_normalise_to_one_identity() {
        let home = Path::new("/home/u");
        let base = Path::new("/cfg");
        let want = PathBuf::from("/home/u/.gitconfig");
        assert_eq!(normalize("~/.gitconfig", home, base), want);
        assert_eq!(normalize("/home/u/.gitconfig", home, base), want);
        assert_eq!(normalize("/home/u/./x/../.gitconfig", home, base), want);
        // Relative paths resolve against the config root, not the cwd.
        assert_eq!(
            normalize("templates/gitconfig.j2", home, base),
            PathBuf::from("/cfg/templates/gitconfig.j2")
        );
    }

    #[test]
    fn the_search_order_is_flag_then_env_then_cwd_then_xdg() {
        let cwd = Path::new("/work");
        let home = Path::new("/home/u");
        let h = host_with(&[("/home/u/.config/bedouin/bedouin.yaml", "version: 0")]);
        assert_eq!(
            locate(None, &h, cwd, home).unwrap(),
            PathBuf::from("/home/u/.config/bedouin/bedouin.yaml")
        );

        let h = h.with_file("/work/bedouin.yaml", "version: 0");
        assert_eq!(
            locate(None, &h, cwd, home).unwrap(),
            PathBuf::from("/work/bedouin.yaml"),
            "cwd beats the XDG location"
        );

        let h = h.with_env("BEDOUIN_CONFIG", "/home/u/.config/bedouin/bedouin.yaml");
        assert_eq!(
            locate(None, &h, cwd, home).unwrap(),
            PathBuf::from("/home/u/.config/bedouin/bedouin.yaml"),
            "the environment beats both"
        );
    }

    #[test]
    fn nothing_found_says_where_it_looked() {
        let h = FakeHost::new();
        let err = locate(None, &h, Path::new("/work"), Path::new("/home/u")).unwrap_err();
        assert!(err.message.contains("/work/bedouin.yaml"), "{err}");
        assert!(err.message.contains("--config"), "{err}");
    }

    #[test]
    fn a_target_declared_in_one_include_is_in_scope_in_another() {
        // The ordering bug: collecting names per file rejects a config that is
        // correct as a whole.
        let h = host_with(&[
            ("/cfg/bedouin.yaml", "version: 0\nincludes: [conf.d/*.yaml]\n"),
            (
                "/cfg/conf.d/10-targets.yaml",
                "targets:\n  - name: work\n    match: { hostname: khaymah }\n",
            ),
            (
                "/cfg/conf.d/20-packages.yaml",
                "packages:\n  - name: neovim\n    from: apt\n    version: { work: \"0.9.5\", default: latest }\n",
            ),
        ]);
        let loaded = load(Path::new("/cfg/bedouin.yaml"), &h).expect("loads");
        assert_eq!(loaded.files.len(), 3);
        assert_eq!(loaded.packages_len(), 1);
        assert!(loaded.vocab.is_known("work"));
    }

    #[test]
    fn includes_expand_in_sorted_order_not_filesystem_order() {
        let h = host_with(&[
            ("/cfg/bedouin.yaml", "version: 0\nincludes: [conf.d/*.yaml]\n"),
            ("/cfg/conf.d/30-c.yaml", "packages:\n  - { name: c, from: apt }\n"),
            ("/cfg/conf.d/10-a.yaml", "packages:\n  - { name: a, from: apt }\n"),
            ("/cfg/conf.d/20-b.yaml", "packages:\n  - { name: b, from: apt }\n"),
        ]);
        let loaded = load(Path::new("/cfg/bedouin.yaml"), &h).unwrap();
        let names: Vec<_> = loaded.raw.packages.iter().map(|p| p.name.clone()).collect();
        assert_eq!(names, ["a", "b", "c"]);
    }

    #[test]
    fn the_same_package_in_two_files_is_an_error_naming_both() {
        let h = host_with(&[
            ("/cfg/bedouin.yaml", "version: 0\nincludes: [conf.d/*.yaml]\n"),
            ("/cfg/conf.d/10-a.yaml", "packages:\n  - { name: jq, from: apt }\n"),
            ("/cfg/conf.d/20-b.yaml", "packages:\n  - { name: jq, from: brew }\n"),
        ]);
        let err = load(Path::new("/cfg/bedouin.yaml"), &h).unwrap_err();
        assert!(err.message.contains("declared twice"), "{err}");
        assert!(err.message.contains("10-a.yaml"), "{err}");
        assert!(err.message.contains("20-b.yaml"), "{err}");
    }

    #[test]
    fn a_yaml_error_carries_the_file_and_the_line() {
        let h = host_with(&[(
            "/cfg/bedouin.yaml",
            "version: 0\npackages:\n  - name: zellij\n    frm: cargo\n",
        )]);
        let err = load(Path::new("/cfg/bedouin.yaml"), &h).unwrap_err();
        let at = err.at.expect("a location");
        assert!(at.contains("/cfg/bedouin.yaml"), "{at}");
        assert!(at.contains(':'), "expected line:col in {at}");
        assert!(err.message.contains("frm"), "{}", err.message);
    }

    #[test]
    fn an_unknown_arm_inside_an_include_still_reports_its_own_file() {
        let h = host_with(&[
            ("/cfg/bedouin.yaml", "version: 0\nincludes: [conf.d/*.yaml]\n"),
            (
                "/cfg/conf.d/10-p.yaml",
                "packages:\n  - name: fd\n    from: { mcaos: brew, default: apt }\n",
            ),
        ]);
        let err = load(Path::new("/cfg/bedouin.yaml"), &h).unwrap_err();
        assert!(err.at.as_deref().unwrap_or("").contains("10-p.yaml"), "{err}");
        assert!(err.message.contains("macos"), "{}", err.message);
    }

    #[test]
    fn an_include_cannot_climb_out_of_the_config_root() {
        let h = host_with(&[
            ("/cfg/bedouin.yaml", "version: 0\nincludes: [\"../evil/x.yaml\"]\n"),
            // FakeHost is a literal map; the real filesystem resolves `..` for us.
            ("/cfg/../evil/x.yaml", "packages:\n  - { name: outside, from: apt }\n"),
        ]);
        let err = load(Path::new("/cfg/bedouin.yaml"), &h).unwrap_err();
        assert!(err.message.contains("reaches outside the config root"), "{err}");
        assert!(contained_in(Path::new("/cfg/conf.d/../a.yaml"), Path::new("/cfg")));
    }

    #[test]
    fn an_empty_config_is_refused_rather_than_planned() {
        let h = host_with(&[("/cfg/bedouin.yaml", "version: 0\nvars: { editor: nvim }\n")]);
        let err = load(Path::new("/cfg/bedouin.yaml"), &h).unwrap_err();
        assert!(err.message.contains("nothing declared"), "{err}");
    }

    #[test]
    fn an_unsupported_schema_version_is_refused() {
        let h = host_with(&[(
            "/cfg/bedouin.yaml",
            "version: 1\npackages:\n  - { name: jq, from: apt }\n",
        )]);
        let err = load(Path::new("/cfg/bedouin.yaml"), &h).unwrap_err();
        assert!(err.message.contains("version: 1"), "{err}");
    }

    impl Loaded {
        fn packages_len(&self) -> usize {
            self.raw.packages.len()
        }
    }
}
