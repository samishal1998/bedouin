//! The plan artifact: a plan you reviewed, applied later, unchanged.
//!
//! Its whole job is to carry the *environment* forward. Env is process-scoped
//! and otherwise unpersisted, so without this a plan reviewed in one terminal
//! applies differently in another — exactly the divergence §6.5 rejects
//! plan-time scripts to avoid.
//!
//! Only the variables the config actually references are frozen. Freezing the
//! whole environment writes every secret in the user's shell into a file that
//! exists to be read and shared.

use crate::facts::Facts;
use crate::plan::Plan;
use crate::schema::{Config, ConfigError, RawConfig, Result};
use crate::state::State;
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

pub const ARTIFACT_VERSION: u32 = 1;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Artifact {
    pub artifact_version: u32,
    pub config_root: PathBuf,
    pub facts: Facts,
    pub config: Config,
    /// What the state looked like when this was computed. `apply -f` refuses a
    /// plan built against a different machine state rather than applying a
    /// stale one.
    pub state_digest: String,
    pub items: Vec<ItemSummary>,
}

/// The plan as printed, for a human reading the artifact.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ItemSummary {
    pub id: String,
    pub action: String,
    pub name: String,
    pub detail: String,
}

/// One environment read found in a template.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EnvRef {
    pub name: String,
    /// Whether THIS read cannot fail: guarded by `| default(<literal>)`, or an
    /// `is defined` test.
    pub guarded: bool,
}

/// Every environment read inside a template expression in `s`.
///
/// Only INSIDE `{{ … }}` or `{% … %}`. Searching the whole string for `env.`
/// reads ordinary text as a reference: the rc file name `40-direnv.zsh`
/// contains one, and `bedouin env` duly reported a variable named `zsh`.
/// `{# … #}` comments and `{% raw %}` bodies are skipped for the same reason:
/// they are not evaluated, so nothing in them is read.
///
/// One scanner, because there were two. `envfile::sites` kept its own copy and
/// the pair drifted -- the duplicate is how a fix to one silently left the
/// other wrong.
pub fn scan_refs(s: &str) -> Vec<EnvRef> {
    let mut out = Vec::new();
    let b = s.as_bytes();
    let mut i = 0;
    while i + 1 < b.len() {
        if b[i] != b'{' {
            i += 1;
            continue;
        }
        match b[i + 1] {
            // A comment is not evaluated.
            b'#' => i = find_close(s, i + 2, "#}").1,
            b'{' | b'%' => {
                let close = if b[i + 1] == b'{' { "}}" } else { "%}" };
                let (body, after) = find_close(s, i + 2, close);
                // `{% raw %}` protects its body from the template engine, so
                // an `{{ env.X }}` inside it is literal text, not a read.
                if body.trim() == "raw" {
                    i = s[after..]
                        .find("{% endraw %}")
                        .map(|e| after + e + "{% endraw %}".len())
                        .unwrap_or(s.len());
                    continue;
                }
                scan_expr(body, &mut out);
                i = after;
            }
            _ => i += 1,
        }
    }
    out
}

/// The text between `from` and the next `close` that is not inside a string
/// literal, and the offset just past that delimiter.
///
/// Quote-aware because `default('}}')` is a legal fallback, and stopping at
/// that `}}` truncated the expression -- hiding every later read in it from
/// both the warning and the artifact freeze.
fn find_close<'a>(s: &'a str, from: usize, close: &str) -> (&'a str, usize) {
    let b = s.as_bytes();
    let mut i = from;
    let mut quote: Option<u8> = None;
    while i < b.len() {
        match quote {
            Some(q) => {
                if b[i] == q {
                    quote = None;
                }
            }
            None => {
                if b[i] == b'\'' || b[i] == b'"' {
                    quote = Some(b[i]);
                } else if b[i..].starts_with(close.as_bytes()) {
                    // Byte comparison, not `s[i..]`: `i` walks bytes, and
                    // slicing at one inside a multibyte character panics.
                    // `close` is ASCII, so a match here IS a char boundary.
                    return (&s[from..i], i + close.len());
                }
            }
        }
        i += 1;
    }
    (&s[from..], s.len())
}

/// `env.NAME` and `env['NAME']` within one already-delimited expression.
fn scan_expr(expr: &str, out: &mut Vec<EnvRef>) {
    let b = expr.as_bytes();
    let mut from = 0;
    while let Some(rel) = expr[from..].find("env") {
        let i = from + rel;
        from = i + 3;
        // `vars.myenv.x` ends in `env` too; only a standalone `env` is the
        // environment.
        let joined = i > 0 && {
            let c = b[i - 1];
            c.is_ascii_alphanumeric() || c == b'_' || c == b'.'
        };
        if joined {
            continue;
        }
        let rest = &expr[i + 3..];
        // `env.NAME` and `env['NAME']` are the same read -- minijinja takes
        // both, and a name containing `-` can only be written the second way.
        // Missing the subscript form froze nothing, so `plan --out` succeeded
        // and `apply -f` then died on `undefined value`.
        let (name, tail) = match rest.as_bytes().first() {
            Some(b'.') => {
                let r = &rest[1..];
                let n: String = r
                    .chars()
                    .take_while(|c| c.is_ascii_alphanumeric() || *c == '_')
                    .collect();
                let t = &r[n.len()..];
                (n, t)
            }
            Some(b'[') => {
                let r = rest[1..].trim_start();
                // A dynamic key -- `env[vars.k]` -- is not statically knowable.
                let Some(q) = r.chars().next().filter(|c| *c == '\'' || *c == '"') else {
                    continue;
                };
                let Some(end) = r[1..].find(q) else { continue };
                let after = r[1 + end + 1..].trim_start();
                (
                    r[1..1 + end].to_string(),
                    after.strip_prefix(']').unwrap_or(after),
                )
            }
            _ => continue,
        };
        if name.is_empty() {
            continue;
        }
        out.push(EnvRef {
            name,
            guarded: is_guarded(tail),
        });
    }
}

/// Whether what follows a read makes it safe when the variable is unset.
///
/// Guarded means THIS read pipes into `default`, not merely that the word
/// appears later in the expression: substring-matching called
/// `{{ env.CONFIG_DIR ~ '/defaults.toml' }}` guarded, and
/// `{% if env.PROFILE == 'default' %}` too -- both fail to render, and calling
/// them safe suppresses the warning that says so.
fn is_guarded(tail: &str) -> bool {
    let tail = tail.trim_start();
    // `{% if env.X is defined %}` cannot fail; it is the test, not a read.
    if let Some(rest) = tail.strip_prefix("is ") {
        let rest = rest.trim_start();
        let rest = rest.strip_prefix("not ").unwrap_or(rest).trim_start();
        if rest.starts_with("defined") || rest.starts_with("undefined") {
            return true;
        }
    }
    let Some(rest) = tail.strip_prefix('|') else {
        return false;
    };
    let rest = rest.trim_start();
    // `d` is minijinja's documented alias for `default`.
    let arg = if let Some(a) = rest.strip_prefix("default") {
        a
    } else if let Some(a) = rest.strip_prefix('d') {
        // ...but not the prefix of some other filter name.
        if a.starts_with(|c: char| c.is_ascii_alphanumeric() || c == '_') {
            return false;
        }
        a
    } else {
        return false;
    };
    let arg = arg.trim_start();
    let Some(inner) = arg.strip_prefix('(') else {
        // `| default` with no argument falls back to the empty string.
        return true;
    };
    // `default(x)` returns `x` unchanged when the read is undefined, so the
    // fallback has to be something that IS defined. A literal always is. So
    // is a fact -- `default(user)` is the common spelling and always renders.
    //
    // `vars.` and `env.` are the two that need not: inside a `vars:` entry the
    // vars map is empty by design, and a fallback reading another unset
    // variable fails exactly when the first one does.
    let inner = inner.trim_start();
    let literal = inner.starts_with('\'')
        || inner.starts_with('"')
        || inner.starts_with(|c: char| c.is_ascii_digit())
        || inner.starts_with("true")
        || inner.starts_with("false")
        || inner.starts_with(')');
    literal || !(inner.starts_with("vars.") || inner.starts_with("env."))
}

/// One environment read somewhere in the config.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConfigRead {
    pub name: String,
    /// Where, in config terms, e.g. `packages.zellij`.
    pub site: String,
    pub guarded: bool,
    /// A `match: { env: … }` key. Unset means the target does not match, which
    /// is not a failure -- but only while that is ALL the variable does, so
    /// the distinction has to travel rather than be guessed from `site`.
    pub match_key: bool,
}

/// Every environment read in the config:
/// Every environment read in the config: `(name, site, guarded)`.
///
/// Scanned on the RAW config, before rendering: rendering substitutes the
/// values and throws the references away, so afterwards there is nothing left
/// to find.
///
/// ONE walk, feeding both the artifact's frozen set and `bedouin env`'s
/// site/guard columns. They used to be two walks over different field sets,
/// and they disagreed: a variable the artifact froze could still be reported
/// as read by `(unknown)` and unguarded, warning that it would fail when it
/// would not.
///
/// `config_root` and `host` are needed because a `files:` entry's TEMPLATE is
/// as much a part of the config's surface as the YAML is. Scanning only the
/// YAML missed `{{ env.GIT_USER_NAME }}` inside `gitconfig.j2`, so the
/// variable was neither reported by `bedouin env` nor frozen into the
/// artifact -- and a plan reviewed with one value applied with another.
pub fn env_refs(
    raw: &RawConfig,
    config_root: &Path,
    host: &dyn crate::host::Host,
) -> Vec<ConfigRead> {
    let mut out: Vec<ConfigRead> = Vec::new();

    let scan = |s: &str, site: &str, out: &mut Vec<ConfigRead>| {
        for r in scan_refs(s) {
            out.push(ConfigRead {
                name: r.name,
                site: site.to_string(),
                guarded: r.guarded,
                match_key: false,
            });
        }
    };
    macro_rules! walk {
        ($v:expr, $site:expr) => {
            for t in $v.payloads() {
                scan(t.as_str(), &$site, &mut out);
            }
        };
    }
    macro_rules! walk_list {
        ($v:expr, $site:expr) => {
            for many in $v.payloads() {
                for t in many.iter() {
                    scan(t.as_str(), &$site, &mut out);
                }
            }
        };
    }

    for t in &raw.targets {
        // A `match:` on an unset variable is not a failure; it simply does
        // not match. So: guarded.
        if let Some(env) = &t.r#match.env {
            for k in env.keys() {
                out.push(ConfigRead {
                    name: k.clone(),
                    site: format!("targets.{}", t.name),
                    guarded: true,
                    match_key: true,
                });
            }
        }
        for (k, v) in &t.vars {
            walk!(v, format!("targets.{}.vars.{k}", t.name));
        }
    }
    for (k, v) in &raw.vars {
        walk!(v, format!("vars.{k}"));
    }
    for (k, v) in &raw.aliases {
        walk!(v, format!("aliases.{k}"));
    }
    for p in &raw.packages {
        let site = format!("packages.{}", p.name);
        for v in p.version.iter() {
            walk!(v, site);
        }
        for v in p.aliases.values() {
            walk!(v, site);
        }
        for b in &p.rc {
            walk!(&b.file, site);
            walk!(&b.content, site);
        }
        if let Some(v) = &p.path {
            walk_list!(v, site);
        }
        if let Some(f) = &p.from {
            walk_list!(f, site);
        }
        if let Some(sc) = &p.script {
            walk!(sc, site);
        }
        if let Some(c) = &p.completions {
            walk_list!(&c.generate, site);
        }
    }
    for f in &raw.files {
        let hint = f
            .dest
            .payloads()
            .next()
            .map(|t| t.as_str().to_string())
            .unwrap_or_default();
        let site = format!("files.{hint}");
        walk!(&f.src, site);
        walk!(&f.dest, site);
        if let Some(m) = &f.mode {
            walk!(m, site);
        }
        // The file this points at is rendered with the same environment, so
        // what it references, the config references.
        for src in f.src.payloads() {
            let rel = src.as_str();
            // A templated `src:` cannot be resolved before rendering, and
            // rendering is what this scan feeds. Rare; skipped, not guessed.
            if rel.contains("{{") {
                continue;
            }
            // Same root rule `plan::build` enforces. Without it `bedouin env`
            // -- which never builds a plan -- reads `src: /etc/shadow`, since
            // `join` drops the base for an absolute path.
            let src = config_root.join(rel);
            if !crate::loader::contained_in(&src, config_root) {
                continue;
            }
            if let Ok(Some(bytes)) = host.read(&src) {
                if let Ok(text) = String::from_utf8(bytes) {
                    scan(&text, &format!("files.{rel}"), &mut out);
                }
            }
        }
    }
    for l in &raw.languages {
        let site = format!("languages.{}", l.name);
        for v in l.version.iter() {
            walk!(v, site);
        }
        // Missed once: an installer chosen by the environment was read at
        // resolve time and frozen nowhere.
        for v in l.installer.iter() {
            walk!(v, site);
        }
    }
    for r in &raw.repos {
        let hint = r.url.payloads().next().map(|t| t.as_str()).unwrap_or("");
        let site = format!("repos.{hint}");
        walk!(&r.url, site);
        walk!(&r.dest, site);
        if let Some(v) = &r.r#ref {
            walk!(v, site);
        }
    }
    for l in &raw.links {
        let hint = l.dest.payloads().next().map(|t| t.as_str()).unwrap_or("");
        let site = format!("links.{hint}");
        walk!(&l.src, site);
        walk!(&l.dest, site);
    }
    if let Some(pm) = &raw.package_managers {
        walk_list!(pm, "package_managers");
    }
    out
}

/// Every environment variable the config could read.
pub fn referenced_env(
    raw: &RawConfig,
    config_root: &Path,
    host: &dyn crate::host::Host,
) -> BTreeSet<String> {
    env_refs(raw, config_root, host)
        .into_iter()
        .map(|r| r.name)
        .collect()
}

pub fn digest_state(state: &State) -> String {
    let text = serde_json::to_string(state).unwrap_or_default();
    crate::writers::digest(&text)
}

/// Build an artifact, freezing only the environment the config references.
pub fn build(
    plan: &Plan,
    cfg: &Config,
    facts: &Facts,
    raw: &RawConfig,
    state: &State,
    config_root: &Path,
    host: &dyn crate::host::Host,
) -> Artifact {
    let wanted = referenced_env(raw, config_root, host);
    let mut facts = facts.clone();
    facts.env.retain(|k, _| wanted.contains(k));

    Artifact {
        artifact_version: ARTIFACT_VERSION,
        config_root: config_root.to_path_buf(),
        facts,
        config: cfg.clone(),
        state_digest: digest_state(state),
        items: plan
            .changes()
            .map(|i| ItemSummary {
                id: i.id.clone(),
                action: format!("{:?}", i.action),
                name: i.name.clone(),
                detail: i.detail.clone(),
            })
            .collect(),
    }
}

pub fn write(a: &Artifact, host: &dyn crate::host::Host, to: &Path) -> Result<()> {
    let json = serde_json::to_string_pretty(a)
        .map_err(|e| ConfigError::new(format!("serialising the plan: {e}")))?;
    // 0600 regardless: it carries whichever environment variables the config
    // reads, and those are frequently tokens.
    host.write(to, json.as_bytes(), 0o600)
        .map_err(|e| ConfigError::new(e.to_string()))
}

pub fn read(host: &dyn crate::host::Host, from: &Path) -> Result<Artifact> {
    let bytes = host
        .read(from)
        .map_err(|e| ConfigError::new(e.to_string()))?
        .ok_or_else(|| ConfigError::new(format!("{}: no such plan", from.display())))?;
    let a: Artifact = serde_json::from_slice(&bytes)
        .map_err(|e| ConfigError::new(format!("{}: not a bedouin plan: {e}", from.display())))?;
    if a.artifact_version > ARTIFACT_VERSION {
        return Err(ConfigError::new(format!(
            "this plan was written by a newer bedouin (artifact version {} vs {})",
            a.artifact_version, ARTIFACT_VERSION
        )));
    }
    Ok(a)
}

/// Refuse a plan whose assumptions no longer hold.
///
/// Checks the state digest and the facts — but NOT the environment. The
/// environment is what the artifact exists to carry forward; re-checking it
/// would reject precisely the case it is for.
pub fn check_still_valid(a: &Artifact, facts: &Facts, state: &State) -> Result<()> {
    if digest_state(state) != a.state_digest {
        return Err(ConfigError::new(
            "the machine has changed since this plan was made.\n  \
             Re-run `bedouin plan` -- applying a stale plan would act on \
             assumptions that no longer hold",
        ));
    }
    let (was, now) = (&a.facts, facts);
    for (what, l, r) in [
        ("os", was.os.as_str(), now.os.as_str()),
        ("distro", was.distro.as_str(), now.distro.as_str()),
        ("arch", was.arch.as_str(), now.arch.as_str()),
        ("hostname", was.hostname.as_str(), now.hostname.as_str()),
    ] {
        if l != r {
            return Err(ConfigError::new(format!(
                "this plan was made for a different machine: `{what}` was `{l}` and is now `{r}`"
            )));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::host::FakeHost;

    fn raw(yaml: &str) -> RawConfig {
        serde_yaml_ng::from_str(yaml).expect("test config parses")
    }

    #[test]
    fn only_the_variables_the_config_reads_are_found() {
        let r = raw(r#"
version: 0
vars:
  v: "{{ env.EDITOR_CHOICE }}"
aliases:
  a: "echo {{ env.GREETING }}"
targets:
  - name: work
    match: { env: { BEDOUIN_PROFILE: work } }
packages:
  - name: zellij
    from: cargo
    version: "{{ env.ZELLIJ_VERSION | default('latest') }}"
    path: ["{{ env.EXTRA_BIN }}"]
files:
  - src: templates/g.j2
    dest: "{{ env.CONFIG_HOME }}/git"
"#);
        let found = referenced_env(&r, Path::new("/cfg"), &FakeHost::new());
        for want in [
            "EDITOR_CHOICE",
            "GREETING",
            "BEDOUIN_PROFILE",
            "ZELLIJ_VERSION",
            "EXTRA_BIN",
            "CONFIG_HOME",
        ] {
            assert!(found.contains(want), "missed {want}: {found:?}");
        }
        // And nothing else: the point is to freeze these and not the shell's
        // whole environment, which is where the secrets are.
        assert_eq!(found.len(), 6, "{found:?}");
    }

    #[test]
    fn env_outside_a_template_expression_is_not_a_reference() {
        // `40-direnv.zsh` contains the substring `env.`. Reading it as one
        // made `bedouin env` report a variable named `zsh`.
        let r = raw(r#"
version: 0
packages:
  - name: direnv
    from: cargo
    rc:
      - file: "~/.zshrc.d/40-direnv.zsh"
        content: |
          eval "$(direnv hook zsh)"
files:
  - src: templates/env.example
    dest: "~/.config/env.d/defaults"
"#);
        let found = referenced_env(&r, Path::new("/cfg"), &FakeHost::new());
        assert!(found.is_empty(), "read plain text as env refs: {found:?}");
    }

    #[test]
    fn an_identifier_merely_ending_in_env_is_not_the_environment() {
        let r = raw("version: 0\nvars:\n  a: \"{{ vars.myenv.HOME }}\"\n");
        let found = referenced_env(&r, Path::new("/cfg"), &FakeHost::new());
        assert!(found.is_empty(), "{found:?}");
    }

    #[test]
    fn a_template_files_own_contents_are_scanned() {
        // The variable is read at render time, so it must be reported AND
        // frozen. Scanning only the YAML missed it, and a plan reviewed with
        // one value applied with another.
        let r = raw(r#"
version: 0
files:
  - src: templates/gitconfig.j2
    dest: "~/.gitconfig"
"#);
        let host = FakeHost::new().with_file(
            "/cfg/templates/gitconfig.j2",
            "[user]\n\tname = {{ env.GIT_USER_NAME | default(user) }}\n\
             \temail = {{ env.GIT_USER_EMAIL }}\n",
        );
        let found = referenced_env(&r, Path::new("/cfg"), &host);
        assert!(found.contains("GIT_USER_NAME"), "{found:?}");
        assert!(found.contains("GIT_USER_EMAIL"), "{found:?}");
    }

    #[test]
    fn repos_and_links_are_scanned_too() {
        let r = raw(r#"
version: 0
repos:
  - url: "https://{{ env.GIT_HOST }}/me/dotfiles"
    dest: "~/store"
    ref: "{{ env.DOTFILES_REF }}"
links:
  - src: "~/store/nvim"
    dest: "{{ env.XDG_CONFIG_HOME }}/nvim"
"#);
        let found = referenced_env(&r, Path::new("/cfg"), &FakeHost::new());
        for want in ["GIT_HOST", "DOTFILES_REF", "XDG_CONFIG_HOME"] {
            assert!(found.contains(want), "missed {want}: {found:?}");
        }
    }

    #[test]
    fn a_statement_block_reads_the_environment_too() {
        let r = raw("version: 0\nvars:\n  a: \"{% if env.CI %}x{% endif %}\"\n");
        let found = referenced_env(&r, Path::new("/cfg"), &FakeHost::new());
        assert!(found.contains("CI"), "{found:?}");
    }

    #[test]
    fn the_scanner_survives_pathological_input() {
        // Byte-indexed, so multibyte text and half-open delimiters are the
        // shapes that would panic or spin. None of these may do either.
        for s in [
            "",
            "{",
            "{{",
            "{%",
            "{{ env.",
            "{{ env.A", // unterminated, real ref
            "}} {{ }}{{",
            "café {{ env.CAFÉ_VAR }} café", // multibyte around and inside
            "日本語 env.NOT_A_REF 日本語",
            "{{ 日本語 env.INNER }}",
            "{{{{ env.A }}}}",
            "{% if env.CI %}{{ env.B }}{% endif %}",
            "{{ env.A }}{{ env.B }}",
        ] {
            let found = scan_refs(s); // must return, not panic or hang
            if s.contains("env.A") && s.contains("{{") {
                assert!(
                    found.iter().any(|r| r.name == "A"),
                    "lost a real reference in {s:?}: {found:?}"
                );
            }
        }
        // The plain-text one really is not a reference.
        assert!(scan_refs("日本語 env.NOT_A_REF 日本語").is_empty());
        // And a multibyte-adjacent real one still is.
        assert_eq!(scan_refs("café {{ env.CAFE_VAR }}")[0].name, "CAFE_VAR");
    }

    #[test]
    fn guarded_means_this_read_pipes_into_default() {
        // Each case cross-checked against what minijinja actually does with
        // the variable unset: `true` here must mean the template renders.
        for (tmpl, want) in [
            ("{{ env.X | default('y') }}", vec![("X", true)]),
            ("{{ env.X|default('y') }}", vec![("X", true)]),
            // The word `default` appearing for another reason is not a guard.
            (
                "{{ env.CONFIG_DIR ~ '/defaults.toml' }}",
                vec![("CONFIG_DIR", false)],
            ),
            (
                "{% if env.PROFILE == 'default' %}a{% endif %}",
                vec![("PROFILE", false)],
            ),
            (
                "{{ env.THEME ~ vars.default_theme }}",
                vec![("THEME", false)],
            ),
            // A guard on a LATER read does not cover an earlier one.
            (
                "{{ env.A }}{{ env.B | default('x') }}",
                vec![("A", false), ("B", true)],
            ),
            (
                "{{ env.ROOT ~ (env.SUB | default('src')) }}",
                vec![("ROOT", false), ("SUB", true)],
            ),
        ] {
            assert_eq!(
                scan_refs(tmpl)
                    .into_iter()
                    .map(|r| (r.name, r.guarded))
                    .collect::<Vec<_>>(),
                want.into_iter()
                    .map(|(n, g)| (n.to_string(), g))
                    .collect::<Vec<_>>(),
                "for {tmpl}"
            );
        }
    }

    #[test]
    fn every_templatable_field_is_walked() {
        // One missed field is one variable read at resolve time and frozen
        // nowhere, which is how a reviewed plan applies differently.
        let r = raw(r#"
version: 0
package_managers: ["{{ env.PM }}"]
vars:
  v: "{{ env.V }}"
aliases:
  a: "{{ env.A }}"
targets:
  - name: t
    match: { distro: ubuntu }
    vars:
      tv: "{{ env.TV }}"
languages:
  - name: rust
    version: "{{ env.LV }}"
    installer: "{{ env.LI }}"
packages:
  - name: p
    from: "{{ env.FROM }}"
    version: "{{ env.PV }}"
    path: ["{{ env.PP }}"]
    aliases: { x: "{{ env.PA }}" }
    completions:
      generate: ["{{ env.PC }}"]
    rc:
      - file: "{{ env.RF }}"
        content: "{{ env.RC }}"
files:
  - src: "{{ env.FS }}"
    dest: "{{ env.FD }}"
    mode: "{{ env.FM }}"
repos:
  - url: "{{ env.RU }}"
    dest: "{{ env.RD }}"
    ref: "{{ env.RR }}"
links:
  - src: "{{ env.LS }}"
    dest: "{{ env.LD }}"
"#);
        let found = referenced_env(&r, Path::new("/cfg"), &FakeHost::new());
        for want in [
            "PM", "V", "A", "TV", "LV", "LI", "FROM", "PV", "PP", "PA", "PC", "RF", "RC", "FS",
            "FD", "FM", "RU", "RD", "RR", "LS", "LD",
        ] {
            assert!(
                found.contains(want),
                "unwalked field lost {want}: {found:?}"
            );
        }
    }

    #[test]
    fn the_subscript_spelling_is_the_same_read() {
        // `env['NAME']` renders; missing it froze nothing, so `plan --out`
        // succeeded and `apply -f` then died on `undefined value`. A name
        // containing `-` can only be written this way.
        for t in [
            "{{ env['NPM_TOKEN'] }}",
            "{{ env[\"NPM_TOKEN\"] }}",
            "{{ env[ 'NPM_TOKEN' ] }}",
        ] {
            let f = scan_refs(t);
            assert_eq!(f.len(), 1, "for {t}: {f:?}");
            assert_eq!(f[0].name, "NPM_TOKEN", "for {t}");
        }
        assert!(scan_refs("{{ env['X'] | default('y') }}")[0].guarded);
        // A dynamic key is not statically knowable, so claim nothing.
        assert!(scan_refs("{{ env[vars.k] }}").is_empty());
    }

    #[test]
    fn a_quoted_delimiter_does_not_end_the_expression() {
        // `default('}}')` is a legal fallback. Stopping at that `}}` hid every
        // later read from both the warning and the artifact freeze.
        let f = scan_refs("{{ env.A | default('}}') ~ env.B }}");
        let names: Vec<_> = f.iter().map(|r| r.name.as_str()).collect();
        assert_eq!(names, ["A", "B"], "{f:?}");
    }

    #[test]
    fn unevaluated_regions_are_not_read() {
        // `{% raw %}` and `{# … #}` are never evaluated, so nothing in them is
        // a reference -- inventing one warns about a variable nothing reads.
        assert!(scan_refs("{% raw %}{{ env.LITERAL }}{% endraw %}").is_empty());
        assert!(scan_refs("{# was: {{ env.LEGACY }} #}").is_empty());
        // ...and a real read after the skipped region is still found.
        let f = scan_refs("{# c #}{{ env.REAL }}");
        assert_eq!(f[0].name, "REAL", "{f:?}");
    }

    #[test]
    fn a_fallback_that_is_itself_undefined_is_not_a_guard() {
        // `default(x)` returns `x` unchanged when the read is undefined, so an
        // undefined `x` still fails -- and inside `vars:` the vars map is
        // empty by design, making that fallback undefined every time.
        assert!(!scan_refs("{{ env.E | default(vars.other) }}")[0].guarded);
        assert!(!scan_refs("{{ env.E | default(env.OTHER) }}")[0].guarded);
        // ...but a fact always resolves, and `default(user)` is the common
        // spelling. Calling it unguarded warned about a config that works.
        assert!(scan_refs("{{ env.E | default(user) }}")[0].guarded);
        assert!(scan_refs("{{ env.E | default(home) }}")[0].guarded);
        // A literal fallback really is one.
        for t in [
            "{{ env.E | default('v') }}",
            "{{ env.E | default(\"v\") }}",
            "{{ env.E | default(8080) }}",
            "{{ env.E | d('v') }}",
            "{{ env.E | default }}",
        ] {
            assert!(scan_refs(t)[0].guarded, "for {t}");
        }
        // `| dirname(...)` is not `| d(...)`.
        assert!(!scan_refs("{{ env.E | dirname('x') }}")[0].guarded);
    }

    #[test]
    fn the_is_defined_idiom_is_a_guard() {
        for t in [
            "{% if env.OPT is defined %}x{% endif %}",
            "{% if env.OPT is not defined %}x{% endif %}",
        ] {
            assert!(scan_refs(t)[0].guarded, "for {t}");
        }
    }

    #[test]
    fn a_config_that_reads_nothing_freezes_nothing() {
        let r = raw("version: 0\npackages: [{name: jq, from: apt}]\n");
        assert!(referenced_env(&r, Path::new("/cfg"), &FakeHost::new()).is_empty());
    }
}
