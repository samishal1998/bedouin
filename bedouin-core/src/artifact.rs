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

/// Every environment variable the config could read.
///
/// Scanned on the RAW config, before rendering: rendering substitutes the
/// values and throws the references away, so afterwards there is nothing left
/// to find.
pub fn referenced_env(raw: &RawConfig) -> BTreeSet<String> {
    let mut names = BTreeSet::new();

    // `match: { env: { NAME: value } }` on every declared target.
    for t in &raw.targets {
        if let Some(env) = &t.r#match.env {
            names.extend(env.keys().cloned());
        }
    }

    // `{{ env.NAME }}` anywhere a template can appear.
    fn scan(s: &str, into: &mut BTreeSet<String>) {
        let mut rest = s;
        while let Some(i) = rest.find("env.") {
            let name: String = rest[i + 4..]
                .chars()
                .take_while(|c| c.is_ascii_alphanumeric() || *c == '_')
                .collect();
            if !name.is_empty() {
                into.insert(name);
            }
            rest = &rest[i + 4..];
        }
    }
    fn walk(v: &crate::value::Value<crate::value::Tmpl>, into: &mut BTreeSet<String>) {
        for t in v.payloads() {
            scan(t.as_str(), into);
        }
    }
    fn walk_list(
        v: &crate::value::Value<crate::value::OneOrMany<crate::value::Tmpl>>,
        into: &mut BTreeSet<String>,
    ) {
        for many in v.payloads() {
            for t in many.iter() {
                scan(t.as_str(), into);
            }
        }
    }

    for v in raw.vars.values() {
        walk(v, &mut names);
    }
    for v in raw.aliases.values() {
        walk(v, &mut names);
    }
    for t in &raw.targets {
        for v in t.vars.values() {
            walk(v, &mut names);
        }
    }
    for p in &raw.packages {
        for v in p.version.iter() {
            walk(v, &mut names);
        }
        for v in p.aliases.values() {
            walk(v, &mut names);
        }
        for b in &p.rc {
            walk(&b.file, &mut names);
            walk(&b.content, &mut names);
        }
        if let Some(v) = &p.path {
            walk_list(v, &mut names);
        }
        walk_list(&p.from, &mut names);
        if let Some(c) = &p.completions {
            walk_list(&c.generate, &mut names);
        }
    }
    for f in &raw.files {
        walk(&f.src, &mut names);
        walk(&f.dest, &mut names);
        if let Some(m) = &f.mode {
            walk(m, &mut names);
        }
    }
    for l in &raw.languages {
        for v in l.version.iter() {
            walk(v, &mut names);
        }
    }
    names
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
) -> Artifact {
    let wanted = referenced_env(raw);
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
        let found = referenced_env(&r);
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
    fn a_config_that_reads_nothing_freezes_nothing() {
        let r = raw("version: 0\npackages: [{name: jq, from: apt}]\n");
        assert!(referenced_env(&r).is_empty());
    }
}
