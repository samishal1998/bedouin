//! The DAG, the diff, and the printed plan.
//!
//! Node order is dependency order:
//!
//! ```text
//! managers -> languages -> shell package -> packages -> files -> rc -> PATH
//! ```
//!
//! The shell's own package is pulled ahead of the general package stage so the
//! run that installs zsh can write into `~/.zshrc.d`. Beyond stage order the
//! edges come from `from:` (a cargo package needs the rust node) and from
//! `needs:` (build prerequisites nothing else can infer -- `from: cargo` does
//! not say the package wants a C toolchain). Within a stage with no edge to
//! separate them, declaration order: deterministic beats arbitrary, and it is
//! the order the user can see.

use crate::facts::{Facts, Manager, Shell};
use crate::host::Host;
use crate::loader::normalize;
use crate::schema::{Config, ConfigError, Result};
use crate::state::{ItemKind, State};
use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;

/// What `apply` must actually do.
///
/// Not merely "something differs": the executor does completely different work
/// for a version bump, a change of install method, and adopting a file that
/// was already there. Encoding the intent here is what keeps the plan a
/// faithful prediction -- otherwise the executor re-derives the diff and the
/// two can disagree.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    Create,
    /// Something unmanaged already sits at this path. Back it up, then write.
    Adopt,
    /// Same install method, different version.
    Upgrade { from: String, to: String },
    /// The install method changed. Remove via the old one, install via the new,
    /// rather than installing twice (§10).
    Reinstall { from_method: String, to_method: String },
    Remove,
    NoOp,
}

impl Action {
    pub fn sigil(&self) -> char {
        match self {
            Self::Create => '+',
            Self::Adopt | Self::Upgrade { .. } | Self::Reinstall { .. } => '~',
            Self::Remove => '-',
            Self::NoOp => ' ',
        }
    }

    /// Whether the executor has work to do.
    pub fn is_change(&self) -> bool {
        !matches!(self, Self::NoOp)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Item {
    pub id: String,
    pub kind: ItemKind,
    pub name: String,
    pub action: Action,
    /// The right-hand column: version, manager, source template.
    pub detail: String,
    pub needs_root: bool,
    /// Which conditional fields resolved to which arm, for `-v`.
    pub arms: BTreeMap<String, String>,
}

#[derive(Debug, Clone)]
pub struct Plan {
    pub items: Vec<Item>,
    pub pruned: Vec<String>,
    pub warnings: Vec<String>,
}

impl Plan {
    pub fn changes(&self) -> impl Iterator<Item = &Item> {
        self.items.iter().filter(|i| i.action != Action::NoOp)
    }

    pub fn has_changes(&self) -> bool {
        self.changes().next().is_some()
    }

    pub fn counts(&self) -> (usize, usize, usize) {
        let mut c = (0, 0, 0);
        for i in self.changes() {
            match i.action {
                Action::Create => c.0 += 1,
                Action::Adopt | Action::Upgrade { .. } | Action::Reinstall { .. } => c.1 += 1,
                Action::Remove => c.2 += 1,
                Action::NoOp => {}
            }
        }
        c
    }

    /// Exit code: 0 nothing pending, 2 changes pending. `plan` exiting 2 is
    /// what makes it usable as a CI drift check, which is the first thing
    /// anyone scripts it for.
    pub fn exit_code(&self) -> i32 {
        i32::from(self.has_changes()) * 2
    }

    pub fn render(&self, verbose: bool) -> String {
        let mut out = String::new();
        for w in &self.warnings {
            out.push_str(&format!("warning: {w}\n"));
        }
        if !self.warnings.is_empty() {
            out.push('\n');
        }
        if !self.has_changes() {
            out.push_str("No changes. The machine matches the config.\n");
            if verbose {
                self.push_verbose(&mut out);
            }
            return out;
        }

        out.push_str("Bedouin will make the following changes:\n\n");
        let kind_w = 8;
        let name_w = self
            .changes()
            .map(|i| i.name.chars().count())
            .max()
            .unwrap_or(0)
            .max(10);
        for i in self.changes() {
            out.push_str(&format!(
                "  {} {:<kw$}  {:<nw$}  {}\n",
                i.action.sigil(),
                kind_label(i.kind),
                i.name,
                i.detail,
                kw = kind_w,
                nw = name_w,
            ));
            if verbose {
                for (field, arm) in &i.arms {
                    out.push_str(&format!(
                        "      {:<kw$}  {field} = {arm}\n",
                        "",
                        kw = kind_w
                    ));
                }
            }
        }
        let (add, change, remove) = self.counts();
        out.push_str(&format!(
            "\nPlan: {add} to add, {change} to change, {remove} to remove.\n"
        ));
        if verbose {
            self.push_verbose(&mut out);
        }
        out
    }

    fn push_verbose(&self, out: &mut String) {
        if !self.pruned.is_empty() {
            out.push_str("\nNot on this machine:\n");
            for p in &self.pruned {
                out.push_str(&format!("  . {p}\n"));
            }
        }
    }
}

fn kind_label(k: ItemKind) -> &'static str {
    match k {
        ItemKind::Manager => "manager",
        ItemKind::Dir => "dir",
        ItemKind::Language => "language",
        ItemKind::Package => "package",
        ItemKind::File => "file",
        ItemKind::Rc => "rc",
        ItemKind::Path => "path",
    }
}

/// Minimal system directories every step's PATH starts from, before the state
/// manifest's recorded bin directories are added.
pub fn system_path(facts: &Facts) -> Vec<PathBuf> {
    let mut p: Vec<PathBuf> = ["/usr/local/bin", "/usr/bin", "/bin", "/usr/sbin", "/sbin"]
        .iter()
        .map(PathBuf::from)
        .collect();
    if facts.os == crate::facts::Os::Macos {
        p.insert(0, PathBuf::from("/opt/homebrew/bin"));
    }
    p
}

/// Bin directories a manager or language contributes, from its recipe rather
/// than from user configuration -- nobody should have to tell Bedouin where
/// rustup puts cargo.
pub fn recipe_bin_dirs(name: &str, facts: &Facts) -> Vec<PathBuf> {
    let home = &facts.home;
    match name {
        "rust" | "rustup" | "cargo" => vec![home.join(".cargo/bin")],
        "mise" => vec![
            home.join(".local/bin"),
            home.join(".local/share/mise/shims"),
        ],
        "brew" => vec![PathBuf::from(if facts.os == crate::facts::Os::Macos {
            "/opt/homebrew/bin"
        } else {
            "/home/linuxbrew/.linuxbrew/bin"
        })],
        _ => Vec::new(),
    }
}

/// The binary that proves a language toolchain is installed.
///
/// Not the language name: nothing on a machine with Rust is called `rust`.
pub fn recipe_probe_bin(language: &str) -> &str {
    match language {
        "rust" => "cargo",
        "python" => "python3",
        "golang" => "go",
        other => other,
    }
}

/// Order packages so a `needs:` edge always points backwards.
///
/// A `needs:` naming a package that `only:` pruned is not an error -- the
/// prerequisite genuinely does not exist on this machine, so the edge simply
/// goes away. `zellij needs build-essential` is right on Linux and meaningless
/// on macOS, and the same config has to say both. Only a `needs:` naming a
/// package that was never declared at all is a mistake.
fn topo(cfg: &Config) -> Result<Vec<usize>> {
    let index: BTreeMap<&str, usize> = cfg
        .packages
        .iter()
        .enumerate()
        .map(|(i, p)| (p.name.as_str(), i))
        .collect();

    // 0 unvisited, 1 on the stack, 2 done.
    let mut mark = vec![0u8; cfg.packages.len()];
    let mut order = Vec::new();
    // Iterative rather than recursive: `visit` recursing once per edge blows
    // the stack on a long chain, and a config is user input.
    let mut stack: Vec<(usize, usize)> = Vec::new(); // (package, next edge)

    for start in 0..cfg.packages.len() {
        if mark[start] == 2 {
            continue;
        }
        stack.push((start, 0));
        while let Some((i, edge)) = stack.pop() {
            if edge == 0 {
                if mark[i] == 2 {
                    continue;
                }
                mark[i] = 1;
            }
            let needs = &cfg.packages[i].needs;
            if edge >= needs.len() {
                mark[i] = 2;
                order.push(i);
                continue;
            }
            stack.push((i, edge + 1));
            let need = &needs[edge];
            let Some(&j) = index.get(need.as_str()) else {
                if cfg.pruned.iter().any(|q| q == &format!("package/{need}")) {
                    continue; // pruned here: the edge does not apply
                }
                return Err(ConfigError::new(format!(
                    "package `{}` needs `{need}`, which is not declared in this config",
                    cfg.packages[i].name
                )));
            };
            match mark[j] {
                2 => {}
                1 => {
                    let cycle: Vec<&str> = stack
                        .iter()
                        .filter(|(k, _)| mark[*k] == 1)
                        .map(|(k, _)| cfg.packages[*k].name.as_str())
                        .collect();
                    return Err(ConfigError::new(format!(
                        "`needs:` forms a cycle: {} -> {}",
                        cycle.join(" -> "),
                        cfg.packages[j].name
                    )));
                }
                _ => stack.push((j, 0)),
            }
        }
    }
    Ok(order)
}

fn arms_of(p: &crate::schema::Provenance) -> BTreeMap<String, String> {
    p.iter()
        .filter(|(_, w)| !matches!(w, crate::value::Winner::Literal))
        .map(|(k, w)| (k.clone(), w.to_string()))
        .collect()
}

/// Build the plan for this machine.
///
/// `config_root` is the directory holding the entry file; `src:` paths resolve
/// against it and may not escape it.
pub fn build(
    cfg: &Config,
    facts: &Facts,
    state: &State,
    host: &dyn Host,
    config_root: &std::path::Path,
) -> Result<Plan> {
    let mut items = Vec::new();
    let mut warnings = Vec::new();
    let mut declared_ids = BTreeSet::new();

    // Every manager that will exist by the time packages install: those already
    // on the machine, plus those this run will bootstrap.
    let mut available: BTreeSet<Manager> = facts.managers.iter().copied().collect();

    // ---- managers
    for m in &cfg.package_managers {
        let id = format!("manager/{m}");
        let present = facts.managers.contains(m);
        available.insert(*m);
        let action = if present {
            Action::NoOp
        } else if m.is_bootstrappable() {
            Action::Create
        } else {
            // apt and zypper are distro-provided and never installed by us.
            warnings.push(format!(
                "`{m}` is declared but is not installed and cannot be bootstrapped; \
                 packages that ask for it will fall back or fail"
            ));
            available.remove(m);
            continue;
        };
        items.push(Item {
            id: id.clone(),
            kind: ItemKind::Manager,
            name: m.to_string(),
            action,
            detail: if present {
                "already installed".into()
            } else {
                "not installed".into()
            },
            needs_root: false,
            arms: BTreeMap::new(),
        });
        declared_ids.insert(id);
    }

    // ---- languages, plus any implied by a `from: cargo` package
    let mut languages = cfg.languages.clone();
    let wants_cargo = cfg.packages.iter().any(|p| p.from.contains(&Manager::Cargo));
    if wants_cargo && !languages.iter().any(|l| l.name == "rust") {
        warnings.push(
            "a package installs `from: cargo` but no `rust` language is declared; \
             adding it implicitly"
                .into(),
        );
        languages.push(crate::schema::Language {
            name: "rust".into(),
            version: None,
            installer: Some(Manager::Rustup),
            resolved_from: Default::default(),
        });
    }
    for l in &languages {
        let id = format!("language/{}", l.name);
        let bin_dirs = recipe_bin_dirs(&l.name, facts);
        let probe = recipe_probe_bin(&l.name);
        let installed = !state.interrupted(&id)
            && (state.done(&id).is_some()
                || host.which(probe, &bin_dirs).is_some()
                || host.which(probe, &system_path(facts)).is_some());
        let installer = l.installer.unwrap_or(Manager::Mise);
        if installer == Manager::Cargo || installer == Manager::Rustup {
            available.insert(Manager::Cargo);
        }
        items.push(Item {
            id: id.clone(),
            kind: ItemKind::Language,
            name: l.name.clone(),
            action: if installed { Action::NoOp } else { Action::Create },
            detail: format!(
                "{}  {}",
                l.version.as_deref().unwrap_or("latest"),
                installer
            ),
            needs_root: false,
            arms: arms_of(&l.resolved_from),
        });
        declared_ids.insert(id);
    }

    // ---- packages, in `needs:` order
    let search: Vec<PathBuf> = state
        .bin_dirs()
        .into_iter()
        .chain(system_path(facts))
        .collect();
    for i in topo(cfg)? {
        let p = &cfg.packages[i];
        let id = format!("package/{}", p.name);
        if p.from.is_empty() {
            return Err(ConfigError::new(format!(
                "package `{}` has an empty `from:`, so there is no manager to \
                 install it with",
                p.name
            )));
        }
        let manager = p
            .from
            .iter()
            .find(|m| available.contains(m))
            .copied()
            .ok_or_else(|| {
                ConfigError::new(format!(
                    "package `{}` asks for {}, none of which will exist on this machine",
                    p.name,
                    p.from
                        .iter()
                        .map(|m| format!("`{m}`"))
                        .collect::<Vec<_>>()
                        .join(" or ")
                ))
            })?;

        // ponytail: presence only -- a binary on the search path counts as
        // installed. Version comparison needs the per-manager probe commands of
        // the installer recipe table, which lands with the executor in M1.
        let known = state.done(&id);
        let on_machine =
            !state.interrupted(&id) && (known.is_some() || host.which(&p.name, &search).is_some());
        let action = match (known, on_machine) {
            // The method moved, so the old install has to come out first --
            // installing twice would leave two copies and one unowned.
            (Some(s), _) if s.method.as_deref().is_some_and(|m| m != manager.as_str()) => {
                Action::Reinstall {
                    from_method: s.method.clone().unwrap_or_default(),
                    to_method: manager.to_string(),
                }
            }
            (Some(s), _) if p.version.is_some() && s.version.as_deref() != p.version.as_deref() => {
                Action::Upgrade {
                    from: s.version.clone().unwrap_or_else(|| "unknown".into()),
                    to: p.version.clone().unwrap_or_default(),
                }
            }
            (_, true) => Action::NoOp,
            (_, false) => Action::Create,
        };
        items.push(Item {
            id: id.clone(),
            kind: ItemKind::Package,
            name: p.name.clone(),
            action,
            detail: format!("{}  {manager}", p.version.as_deref().unwrap_or("latest")),
            needs_root: matches!(manager, Manager::Apt | Manager::Zypper | Manager::Dnf),
            arms: arms_of(&p.resolved_from),
        });
        declared_ids.insert(id);
    }

    // ---- files
    let mut seen_dest: BTreeMap<PathBuf, String> = BTreeMap::new();
    for f in &cfg.files {
        // Item identity is the normalised path, so `~/.gitconfig` and
        // `/home/u/.gitconfig` are one item rather than two. Dest is templated,
        // so this collision can only be checked once it has rendered.
        let dest = normalize(&f.dest, &facts.home, &facts.home);
        if let Some(first) = seen_dest.insert(dest.clone(), f.src.clone()) {
            return Err(ConfigError::new(format!(
                "two managed files resolve to the same destination {}\n  from: {first}\n  and:  {}",
                dest.display(),
                f.src
            )));
        }
        // A plan that names a source which is not there is not a prediction of
        // apply, it is a promise apply cannot keep -- and checking is free.
        let src = normalize(&f.src, &facts.home, config_root);
        // No exemption for absolute paths: `src: /etc/shadow` is the case the
        // rule exists for, and writing the same path absolutely must not be a
        // way around it.
        if !crate::loader::contained_in(&src, config_root) {
            return Err(ConfigError::new(format!(
                "managed file `{}` reaches outside the config root\n  resolved to: {}\n  root:       {}",
                f.src,
                src.display(),
                config_root.display()
            )));
        }
        if host
            .read(&src)
            .map_err(|e| ConfigError::new(e.to_string()))?
            .is_none()
        {
            return Err(ConfigError::new(format!(
                "managed file source `{}` does not exist\n  looked for: {}\n    `src:` resolves against the directory holding bedouin.yaml",
                f.src,
                src.display()
            )));
        }

        let id = format!("file/{}", dest.display());
        let exists = host
            .symlink_meta(&dest)
            .map_err(|e| ConfigError::new(e.to_string()))?
            .is_some();
        items.push(Item {
            id: id.clone(),
            kind: ItemKind::File,
            name: display_home(&dest, facts),
            action: if state.done(&id).is_some() {
                Action::NoOp
            } else if exists {
                // §9.1: back the user's file up before the first write. A
                // create and an adopt are different work, so they are
                // different actions.
                Action::Adopt
            } else {
                Action::Create
            },
            detail: format!("from {}", f.src),
            needs_root: !dest.starts_with(&facts.home),
            arms: arms_of(&f.resolved_from),
        });
        declared_ids.insert(id);
    }

    // ---- rc blocks and PATH, both of which need a shell Bedouin can write for
    let rc_capable = cfg.shell != Shell::Other;
    if !rc_capable
        && (cfg.packages.iter().any(|p| !p.rc.is_empty() || !p.path.is_empty()))
    {
        return Err(ConfigError::new(
            "this config writes rc blocks or PATH entries, but the shell is not one \
             Bedouin knows how to write for.\n  \
             Declare `shell: zsh` (or bash, or fish) at the top level",
        ));
    }
    if rc_capable {
        let writes_shell_files = cfg
            .packages
            .iter()
            .any(|p| !p.rc.is_empty() || !p.path.is_empty());
        if writes_shell_files {
            // §3.1: `plan` reports these; `apply` creates them.
            let dir = &facts.shell.rc_dir;
            let dir_id = format!("dir/{}", dir.display());
            let dir_exists = host
                .symlink_meta(dir)
                .map_err(|e| ConfigError::new(e.to_string()))?
                .is_some();
            items.push(Item {
                id: dir_id.clone(),
                kind: ItemKind::Dir,
                name: display_home(dir, facts),
                action: if dir_exists || state.done(&dir_id).is_some() {
                    Action::NoOp
                } else {
                    Action::Create
                },
                detail: format!("drop-in directory for {}", cfg.shell),
                needs_root: false,
                arms: BTreeMap::new(),
            });
            declared_ids.insert(dir_id);

            // fish sources conf.d natively and needs no block.
            if cfg.shell != Shell::Fish {
                let id = "rc/bedouin/source".to_string();
                items.push(Item {
                    id: id.clone(),
                    kind: ItemKind::Rc,
                    name: display_home(&facts.shell.rc_file, facts),
                    action: if state.done(&id).is_some() {
                        Action::NoOp
                    } else {
                        Action::Create
                    },
                    detail: format!("managed block: source {}", display_home(dir, facts)),
                    needs_root: false,
                    arms: BTreeMap::new(),
                });
                declared_ids.insert(id);
            }
        }

        for p in &cfg.packages {
            for block in &p.rc {
                let file = normalize(&block.file, &facts.home, &facts.home);
                let base = file
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_default();
                // The id carries the owning package: two packages may write
                // files of the same basename.
                let id = format!("rc/{}/{base}", p.name);
                items.push(Item {
                    id: id.clone(),
                    kind: ItemKind::Rc,
                    name: display_home(&file, facts),
                    action: if state.done(&id).is_some() {
                        Action::NoOp
                    } else {
                        Action::Create
                    },
                    detail: format!("owned by {}", p.name),
                    needs_root: false,
                    arms: BTreeMap::new(),
                });
                declared_ids.insert(id);
            }
        }

        let mut path_entries: Vec<(String, String)> = Vec::new();
        for p in &cfg.packages {
            for entry in &p.path {
                let norm = normalize(entry, &facts.home, &facts.home);
                let key = norm.display().to_string();
                if !path_entries.iter().any(|(k, _)| *k == key) {
                    path_entries.push((key, p.name.clone()));
                }
            }
        }
        for (entry, owner) in path_entries {
            let id = format!("path/{entry}");
            items.push(Item {
                id: id.clone(),
                kind: ItemKind::Path,
                name: display_home(std::path::Path::new(&entry), facts),
                action: if state.done(&id).is_some() {
                    Action::NoOp
                } else {
                    Action::Create
                },
                detail: format!("owned by {owner}"),
                needs_root: false,
                arms: BTreeMap::new(),
            });
            declared_ids.insert(id);
        }
    }

    // ---- one item, one id (§7.2): two nodes sharing a state key would
    // fight over it, and one of the two would become unowned and unremovable.
    {
        let mut by_id: BTreeMap<&str, &str> = BTreeMap::new();
        for i in &items {
            if let Some(first) = by_id.insert(&i.id, &i.name) {
                return Err(ConfigError::new(format!(
                    "two items resolve to the same id `{}`\n  {first}\n  {}",
                    i.id, i.name
                )));
            }
        }
    }

    // ---- removals: in state, owned by us, no longer declared
    for (id, item) in state.owned_by_bedouin() {
        if declared_ids.contains(id) {
            continue;
        }
        let name = id.split_once('/').map_or(id.as_str(), |(_, n)| n);
        items.push(Item {
            id: id.clone(),
            kind: item.kind,
            name: name.to_string(),
            action: Action::Remove,
            detail: match &item.method {
                Some(m) => format!("was: {m}, owner: bedouin"),
                None => "owner: bedouin".into(),
            },
            needs_root: false,
            arms: BTreeMap::new(),
        });
    }

    Ok(Plan {
        items,
        pruned: cfg.pruned.clone(),
        warnings,
    })
}

/// Print paths under `$HOME` with a `~`, which is how the user wrote them.
fn display_home(p: &std::path::Path, facts: &Facts) -> String {
    match p.strip_prefix(&facts.home) {
        Ok(rest) => format!("~/{}", rest.display()),
        Err(_) => p.display().to_string(),
    }
}
