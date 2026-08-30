//! `bedouin.yaml` as parsed, and as resolved.
//!
//! Two shapes: [`RawConfig`] is what YAML deserializes into, with every
//! evaluatable leaf still a [`Value`]. [`Config`] is what the planner sees, and
//! it contains no conditionals at all -- that is what keeps arms out of the
//! diff, the DAG, and the state file.
//!
//! [`resolve`] is the bridge, and its internal order is load-bearing:
//! **prune `only:`, then select arms, then render.**

use crate::facts::{Facts, Manager, Shell};
use crate::render::{self, Context, RenderError};
use crate::target::Vocabulary;
use crate::value::{validate_arms, OneOrMany, SelectError, Tmpl, Value, Winner};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

// ------------------------------------------------------------------- errors

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConfigError {
    /// `file:line:col` where known.
    pub at: Option<String>,
    /// The item being resolved, e.g. `packages[2] "zellij"`.
    pub item: Option<String>,
    pub message: String,
}

impl ConfigError {
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            at: None,
            item: None,
            message: message.into(),
        }
    }

    pub fn in_item(mut self, item: impl Into<String>) -> Self {
        self.item = Some(item.into());
        self
    }

    pub fn at(mut self, at: impl Into<String>) -> Self {
        self.at = Some(at.into());
        self
    }
}

impl std::fmt::Display for ConfigError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if let Some(at) = &self.at {
            write!(f, "{at}: ")?;
        }
        if let Some(item) = &self.item {
            write!(f, "{item}: ")?;
        }
        f.write_str(&self.message)
    }
}

impl std::error::Error for ConfigError {}

pub type Result<T> = std::result::Result<T, ConfigError>;

// ---------------------------------------------------------------- raw shapes

type Val = Value<Tmpl>;
type ValList = Value<OneOrMany<Tmpl>>;

/// `bedouin.yaml` as written.
///
/// Unknown keys are refused. A config tool that silently ignores a misspelled
/// key silently does the wrong thing.
//
// ponytail: serde's derived unknown-field error already lists the expected
// keys, which is most of the value. A hand-rolled check would add a
// did-you-mean; do it when the derived message actually grates.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RawConfig {
    #[serde(default)]
    pub version: u32,
    #[serde(default)]
    pub includes: Vec<String>,
    /// The shell being configured. Not evaluatable, and not a fact: on a fresh
    /// box Bedouin is usually installing the shell it configures, so the
    /// detected one is the wrong answer.
    #[serde(default)]
    pub shell: Option<String>,
    #[serde(default)]
    pub vars: BTreeMap<String, Val>,
    /// Shell aliases that belong to no package.
    #[serde(default)]
    pub aliases: BTreeMap<String, Val>,
    #[serde(default)]
    pub targets: Vec<crate::target::Target>,
    #[serde(default)]
    pub package_managers: Option<ValList>,
    #[serde(default)]
    pub languages: Vec<RawLanguage>,
    #[serde(default)]
    pub packages: Vec<RawPackage>,
    #[serde(default)]
    pub files: Vec<RawFile>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RawPackage {
    /// Not evaluatable: this is the state-file identity. A name that varies by
    /// machine is an identity that varies by machine, and uninstall stops
    /// working.
    pub name: String,
    pub from: ValList,
    #[serde(default)]
    pub version: Option<Val>,
    /// Membership, not value. Arms choose *between* values; they cannot make an
    /// item not exist, and without this one config cannot cover Ubuntu and
    /// macOS -- which is the product premise.
    #[serde(default)]
    pub only: Option<OneOrMany<String>>,
    /// Explicit DAG edges for build prerequisites the engine cannot infer.
    /// Nothing in `from: cargo` says the package needs a C toolchain.
    #[serde(default)]
    pub needs: Vec<String>,
    #[serde(default)]
    pub path: Option<ValList>,
    #[serde(default)]
    pub rc: Vec<RawRcBlock>,
    /// Aliases scoped to this package: they live in its rc block, so dropping
    /// the package removes them through machinery that already converges.
    #[serde(default)]
    pub aliases: BTreeMap<String, Val>,
    #[serde(default)]
    pub completions: Option<RawCompletions>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RawCompletions {
    /// argv, run at apply time after this package is installed. Its stdout is
    /// written to the shell's completions directory and never evaluated.
    pub generate: ValList,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RawRcBlock {
    pub file: Val,
    pub content: Val,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RawLanguage {
    pub name: String,
    #[serde(default)]
    pub version: Option<Val>,
    #[serde(default)]
    pub installer: Option<Val>,
    #[serde(default)]
    pub only: Option<OneOrMany<String>>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RawFile {
    pub src: Val,
    pub dest: Val,
    #[serde(default)]
    pub mode: Option<Val>,
    #[serde(default)]
    pub only: Option<OneOrMany<String>>,
}

// ----------------------------------------------------------- resolved shapes

/// Which arm won for each conditional field. Nearly free to record, and it is
/// what lets `doctor` say "this resolved differently than last apply" -- the
/// failure a conditional config otherwise makes invisible.
pub type Provenance = BTreeMap<String, Winner>;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Package {
    pub name: String,
    pub from: Vec<Manager>,
    pub version: Option<String>,
    pub needs: Vec<String>,
    pub path: Vec<String>,
    pub rc: Vec<RcBlock>,
    pub aliases: BTreeMap<String, String>,
    pub completions: Option<Vec<String>>,
    pub resolved_from: Provenance,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RcBlock {
    pub file: String,
    pub content: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Language {
    pub name: String,
    pub version: Option<String>,
    pub installer: Option<Manager>,
    pub resolved_from: Provenance,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileSpec {
    pub src: String,
    pub dest: String,
    pub mode: Option<String>,
    pub resolved_from: Provenance,
}

/// The planner's input. Contains no conditionals.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Config {
    pub shell: Shell,
    pub vars: BTreeMap<String, String>,
    pub aliases: BTreeMap<String, String>,
    pub package_managers: Vec<Manager>,
    pub languages: Vec<Language>,
    pub packages: Vec<Package>,
    pub files: Vec<FileSpec>,
    /// Items dropped by `only:`, for `plan -v`.
    pub pruned: Vec<String>,
}

// -------------------------------------------------------------------- resolve

struct Resolver<'a> {
    vocab: &'a Vocabulary,
    facts: &'a Facts,
    vars: BTreeMap<String, String>,
}

impl Resolver<'_> {
    fn ctx(&self) -> Context<'_> {
        Context {
            facts: self.facts,
            vars: &self.vars,
        }
    }

    /// Select then render, recording which arm won.
    fn one(&self, v: &Val, field: &str, prov: &mut Provenance) -> Result<String> {
        validate_arms(v, self.vocab).map_err(|e| ConfigError::new(e.to_string()))?;
        let (payload, winner) = v
            .select(self.vocab, self.facts)
            .map_err(|e| self.select_err(e, field))?;
        prov.insert(field.to_string(), winner);
        render::render(payload, &self.ctx()).map_err(Self::render_err)
    }

    fn many(&self, v: &ValList, field: &str, prov: &mut Provenance) -> Result<Vec<String>> {
        validate_arms(v, self.vocab).map_err(|e| ConfigError::new(e.to_string()))?;
        let (payload, winner) = v
            .select(self.vocab, self.facts)
            .map_err(|e| self.select_err(e, field))?;
        prov.insert(field.to_string(), winner);
        payload
            .iter()
            .map(|t| render::render(t, &self.ctx()).map_err(Self::render_err))
            .collect()
    }

    fn select_err(&self, e: SelectError, field: &str) -> ConfigError {
        ConfigError::new(format!("`{field}`: {e}"))
    }

    fn render_err(e: RenderError) -> ConfigError {
        ConfigError::new(e.to_string())
    }

    fn managers(&self, names: &[String], field: &str) -> Result<Vec<Manager>> {
        names
            .iter()
            .map(|n| {
                Manager::parse(n).ok_or_else(|| {
                    let known: Vec<_> = Manager::ALL.iter().map(|m| m.as_str()).collect();
                    ConfigError::new(format!(
                        "`{field}`: unknown package manager `{n}`\n  known: {}",
                        known.join(", ")
                    ))
                })
            })
            .collect()
    }
}

/// Does this item exist on this machine?
///
/// Evaluated first, before any of the item's other fields are touched. Without
/// that ordering the key does not work: `only: [ubuntu, opensuse]` on an entry
/// whose `from:` is `{ubuntu: apt, opensuse: zypper}` would still trip the
/// no-default error on macOS, and nothing would be fixed.
fn keeps(only: &Option<OneOrMany<String>>, vocab: &Vocabulary, facts: &Facts) -> Result<bool> {
    let Some(names) = only else { return Ok(true) };
    if names.is_empty() {
        return Err(ConfigError::new(
            "`only: []` names no machine, so the item would never exist anywhere. \
             Drop the item, or name the arms it applies to",
        ));
    }
    let mut any = false;
    for name in names.iter() {
        if !vocab.is_known(name) {
            let s = crate::arm::suggest(name, vocab.all_names());
            let tail = if s.is_empty() {
                String::new()
            } else {
                format!("\n  did you mean: {}?", s.join(", "))
            };
            return Err(ConfigError::new(format!(
                "`only:` names an unknown arm `{name}`{tail}"
            )));
        }
        any |= vocab.matches(name, facts);
    }
    Ok(any)
}

/// Reduce a parsed config to a concrete one for this machine.
pub fn resolve(raw: &RawConfig, vocab: &Vocabulary, facts: &Facts) -> Result<Config> {
    if raw.version != 0 {
        return Err(ConfigError::new(format!(
            "unsupported `version: {}`; this build understands schema version 0",
            raw.version
        )));
    }

    // Variables first: everything else may reference them, and they may not
    // reference each other.
    let mut vars = BTreeMap::new();
    for (k, v) in &raw.vars {
        let mut prov = Provenance::new();
        validate_arms(v, vocab).map_err(|e| ConfigError::new(e.to_string()))?;
        let (payload, w) = v
            .select(vocab, facts)
            .map_err(|e| ConfigError::new(format!("`vars.{k}`: {e}")))?;
        prov.insert(k.clone(), w);
        vars.insert(
            k.clone(),
            render::render_var(payload, facts).map_err(|e| ConfigError::new(e.to_string()))?,
        );
    }

    // Active targets' vars fold in per key, first-declared winning, and
    // override the base block. Per key rather than wholesale, so a target that
    // sets only `editor` does not drop another target's unrelated `proxy`.
    let mut from_targets: BTreeMap<String, String> = BTreeMap::new();
    for t in vocab.declared() {
        if !t.r#match.matches(facts) {
            continue;
        }
        for (k, v) in &t.vars {
            if from_targets.contains_key(k) {
                continue;
            }
            // Every other evaluatable leaf validates before selecting. Skipping
            // it here made the same arm pair a hard error in the base `vars:`
            // block and a silent fallthrough to `default:` inside a target.
            validate_arms(v, vocab)
                .map_err(|e| ConfigError::new(format!("target `{}` var `{k}`: {e}", t.name)))?;
            let (payload, _) = v
                .select(vocab, facts)
                .map_err(|e| ConfigError::new(format!("target `{}` var `{k}`: {e}", t.name)))?;
            from_targets.insert(
                k.clone(),
                render::render_var(payload, facts).map_err(|e| ConfigError::new(e.to_string()))?,
            );
        }
    }
    vars.extend(from_targets);

    let r = Resolver { vocab, facts, vars };
    let mut pruned = Vec::new();

    let package_managers = match &raw.package_managers {
        None => Vec::new(),
        Some(v) => {
            let mut prov = Provenance::new();
            let names = r.many(v, "package_managers", &mut prov)?;
            let declared = r.managers(&names, "package_managers")?;
            // A manager that cannot exist on this OS is dropped rather than
            // planned. No applicability table is needed beyond `runs_on`: if
            // dropping leaves a package with no viable manager, the planner
            // already errors, naming the package and what it asked for.
            declared
                .into_iter()
                .filter(|m| {
                    let ok = m.runs_on(facts.os);
                    if !ok {
                        pruned.push(format!("manager/{m} (cannot run on {})", facts.os));
                    }
                    ok
                })
                .collect()
        }
    };

    let mut languages = Vec::new();
    for l in &raw.languages {
        if !keeps(&l.only, vocab, facts).map_err(|e| e.in_item(format!("language `{}`", l.name)))? {
            pruned.push(format!("language/{}", l.name));
            continue;
        }
        let item = format!("language `{}`", l.name);
        let mut prov = Provenance::new();
        let version = match &l.version {
            Some(v) => Some(r.one(v, "version", &mut prov).map_err(|e| e.in_item(&item))?),
            None => None,
        };
        let installer = match &l.installer {
            Some(v) => {
                let name = r.one(v, "installer", &mut prov).map_err(|e| e.in_item(&item))?;
                let m = r
                    .managers(std::slice::from_ref(&name), "installer")
                    .map_err(|e| e.in_item(&item))?[0];
                // Only these two install toolchains. Accepting any known
                // manager would let `installer: apt` reach an executor path
                // that does not exist, and fail at apply rather than at parse.
                if !matches!(m, Manager::Rustup | Manager::Mise) {
                    return Err(ConfigError::new(format!(
                        "`installer: {m}` is not a toolchain installer\n  supported: rustup, mise"
                    ))
                    .in_item(&item));
                }
                Some(m)
            }
            None => None,
        };
        languages.push(Language {
            name: l.name.clone(),
            version,
            installer,
            resolved_from: prov,
        });
    }

    let mut packages = Vec::new();
    for p in &raw.packages {
        if !keeps(&p.only, vocab, facts).map_err(|e| e.in_item(format!("package `{}`", p.name)))? {
            pruned.push(format!("package/{}", p.name));
            continue;
        }
        let item = format!("package `{}`", p.name);
        let mut prov = Provenance::new();
        let from_names = r.many(&p.from, "from", &mut prov).map_err(|e| e.in_item(&item))?;
        let from = r.managers(&from_names, "from").map_err(|e| e.in_item(&item))?;
        // rustup installs toolchains, not packages: `from: rustup` would have
        // run `rustup toolchain install`, ignoring the package name entirely
        // and reporting success.
        if let Some(bad) = from.iter().find(|m| **m == Manager::Rustup) {
            return Err(ConfigError::new(format!(
                "`from: {bad}` installs toolchains, not packages\n  For a Rust toolchain use `languages:`; for a crate use `from: cargo`"
            ))
            .in_item(&item));
        }
        let version = match &p.version {
            Some(v) => Some(r.one(v, "version", &mut prov).map_err(|e| e.in_item(&item))?),
            None => None,
        };
        let path = match &p.path {
            Some(v) => r.many(v, "path", &mut prov).map_err(|e| e.in_item(&item))?,
            None => Vec::new(),
        };
        let mut rc = Vec::new();
        for (i, block) in p.rc.iter().enumerate() {
            let file = r
                .one(&block.file, &format!("rc[{i}].file"), &mut prov)
                .map_err(|e| e.in_item(&item))?;
            let content = r
                .one(&block.content, &format!("rc[{i}].content"), &mut prov)
                .map_err(|e| e.in_item(&item))?;
            rc.push(RcBlock { file, content });
        }
        let mut aliases = BTreeMap::new();
        for (k, v) in &p.aliases {
            aliases.insert(
                k.clone(),
                r.one(v, &format!("aliases.{k}"), &mut prov)
                    .map_err(|e| e.in_item(&item))?,
            );
        }
        let completions = match &p.completions {
            None => None,
            Some(c) => Some(
                r.many(&c.generate, "completions.generate", &mut prov)
                    .map_err(|e| e.in_item(&item))?,
            ),
        };
        if completions.as_ref().is_some_and(Vec::is_empty) {
            return Err(ConfigError::new(
                "`completions.generate` is empty, so there is no command to run",
            )
            .in_item(&item));
        }
        packages.push(Package {
            name: p.name.clone(),
            aliases,
            completions,
            from,
            version,
            needs: p.needs.clone(),
            path,
            rc,
            resolved_from: prov,
        });
    }

    let mut files = Vec::new();
    for f in &raw.files {
        let mut prov = Provenance::new();
        // `dest` is not known until it renders, so name the item by `src`.
        let item = format!("file `{}`", tmpl_hint(&f.src));
        if !keeps(&f.only, vocab, facts).map_err(|e| e.in_item(&item))? {
            pruned.push(format!("file/{}", tmpl_hint(&f.src)));
            continue;
        }
        let src = r.one(&f.src, "src", &mut prov).map_err(|e| e.in_item(&item))?;
        let dest = r.one(&f.dest, "dest", &mut prov).map_err(|e| e.in_item(&item))?;
        let mode = match &f.mode {
            Some(v) => Some(r.one(v, "mode", &mut prov).map_err(|e| e.in_item(&item))?),
            None => None,
        };
        files.push(FileSpec {
            src,
            dest,
            mode,
            resolved_from: prov,
        });
    }

    let shell = match &raw.shell {
        None => facts.shell.name,
        Some(s) => Shell::parse(s).ok_or_else(|| {
            let known: Vec<_> = Shell::ALL
                .iter()
                .filter(|s| **s != Shell::Other)
                .map(|s| s.as_str())
                .collect();
            ConfigError::new(format!(
                "`shell: {s}` is not a shell Bedouin knows\n  known: {}",
                known.join(", ")
            ))
        })?,
    };

    let mut global_aliases = BTreeMap::new();
    for (k, v) in &raw.aliases {
        let mut prov = Provenance::new();
        global_aliases.insert(k.clone(), r.one(v, &format!("aliases.{k}"), &mut prov)?);
    }

    Ok(Config {
        shell,
        aliases: global_aliases,
        vars: r.vars,
        package_managers,
        languages,
        packages,
        files,
        pruned,
    })
}

/// A readable stand-in for an unresolved value, used only in error text.
fn tmpl_hint(v: &Val) -> String {
    match v {
        Value::Const(t) => t.0.clone(),
        Value::ByTarget { arms, default } => arms
            .first()
            .map(|(_, t)| t.0.clone())
            .or_else(|| default.as_ref().map(|t| t.0.clone()))
            .unwrap_or_else(|| "?".into()),
    }
}
