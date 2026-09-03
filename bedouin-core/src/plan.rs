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
use serde::Serialize;
use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;

/// What `apply` must actually do.
///
/// Not merely "something differs": the executor does completely different work
/// for a version bump, a change of install method, and adopting a file that
/// was already there. Encoding the intent here is what keeps the plan a
/// faithful prediction -- otherwise the executor re-derives the diff and the
/// two can disagree.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Action {
    Create,
    /// Something unmanaged already sits at this path. Back it up, then write.
    Adopt,
    /// Same install method, different version.
    Upgrade {
        from: String,
        to: String,
    },
    /// The install method changed. Remove via the old one, install via the new,
    /// rather than installing twice (§10).
    Reinstall {
        from_method: String,
        to_method: String,
    },
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

/// Everything `apply` needs to carry out one item.
///
/// The plan is self-contained: the executor reads this and nothing else, so it
/// cannot reach a different conclusion than the plan the user reviewed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Payload {
    Manager(Manager),
    Language {
        installer: Manager,
        version: Option<String>,
        bin_dirs: Vec<PathBuf>,
    },
    Package {
        manager: Manager,
        version: Option<String>,
        /// Set for `Reinstall`: the manager the old copy came from.
        previous: Option<Manager>,
    },
    /// Installed by running a script the config supplies. Bedouin runs it and
    /// records that it did; it cannot uninstall the result, so it never claims
    /// to own it.
    ScriptPackage {
        name: String,
        script: String,
    },
    Dir(PathBuf),
    File {
        src: PathBuf,
        dest: PathBuf,
        mode: u32,
    },
    /// A block Bedouin owns inside a file. The file may be a drop-in Bedouin
    /// created or the user's own rc file -- either way Bedouin owns the block
    /// and never the file, which is what §9 actually says.
    RcBlock {
        file: PathBuf,
        marker: String,
        content: String,
    },
    PathFile {
        file: PathBuf,
        entries: Vec<String>,
    },
    /// Install a shell framework, if it is not already there.
    Framework {
        kind: String,
        home: PathBuf,
    },
    /// A block that must sit above the line which reads it.
    AnchoredBlock {
        file: PathBuf,
        marker: String,
        content: String,
        anchor: String,
    },
    /// A symlink Bedouin owns.
    Link {
        src: PathBuf,
        dest: PathBuf,
    },
    /// Clone or fast-forward a git repository.
    Repo {
        url: String,
        dest: PathBuf,
        reference: Option<String>,
    },
    /// Run a tool's own completion generator and write its stdout. The output
    /// is a file Bedouin wholly owns: no sentinels, never evaluated.
    Completions {
        argv: Vec<String>,
        dest: PathBuf,
    },
    /// Nothing to execute -- a removal of an item whose kind the executor
    /// handles from state alone.
    None,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
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
    pub payload: Payload,
}

#[derive(Debug, Clone, Serialize)]
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
        use crate::style::{bold, cyan, dim, green, red, yellow};
        let mut out = String::new();
        for w in &self.warnings {
            out.push_str(&format!("{} {w}\n", yellow("warning:")));
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

        out.push_str(&bold("Bedouin will make the following changes:"));
        out.push_str("\n\n");
        let kind_w = 8;
        let name_w = self
            .changes()
            .map(|i| i.name.chars().count())
            .max()
            .unwrap_or(0)
            .max(10);
        for i in self.changes() {
            // The sigil carries the meaning, so it carries the colour:
            // adding is green, changing amber, removing red.
            let g = i.action.sigil().to_string();
            let sigil = match i.action {
                Action::Create => green(&g),
                Action::Adopt | Action::Upgrade { .. } | Action::Reinstall { .. } => yellow(&g),
                Action::Remove => red(&g),
                Action::NoOp => dim(&g),
            };
            // Pad BEFORE colouring: the escape codes are bytes too, and
            // padding the coloured string counts them toward the width, which
            // knocks every column out by the length of the sequence.
            let kind = format!("{:<kw$}", kind_label(i.kind), kw = kind_w);
            out.push_str(&format!(
                "  {} {}  {:<nw$}  {}\n",
                sigil,
                cyan(&kind),
                i.name,
                dim(&i.detail),
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
        for i in &self.items {
            if let crate::plan::Payload::PathFile { entries, .. } = &i.payload {
                if i.action.is_change() {
                    out.push_str("\nPATH entries:\n");
                    for e in entries {
                        out.push_str(&format!("  {e}\n"));
                    }
                }
            }
        }
        if !self.pruned.is_empty() {
            out.push_str("\nNot on this machine:\n");
            for p in &self.pruned {
                out.push_str(&format!("  . {p}\n"));
            }
        }
    }
}

pub fn kind_label(k: ItemKind) -> &'static str {
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

/// The action for a piece of managed content, three ways.
///
/// Config versus state answers "did the user change what they want". Disk
/// versus state answers "did something change it behind our back". Comparing
/// only the first two made `doctor` report drift that `apply` then refused to
/// fix -- a promise the tool did not keep.
fn content_action(recorded: Option<&str>, want: &str, on_disk: Option<String>) -> Action {
    match recorded {
        None => Action::Create,
        Some(had) if had != want => Action::Upgrade {
            from: "config changed".into(),
            to: "current".into(),
        },
        Some(had) => match on_disk {
            Some(found) if found == had => Action::NoOp,
            Some(_) => Action::Upgrade {
                from: "edited on disk".into(),
                to: "managed".into(),
            },
            None => Action::Create, // it was here and is not any more
        },
    }
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

    // ---- languages are needed first: what they install WITH is a manager
    // this run has to have, and the manager loop is right below.
    let mut languages = cfg.languages.clone();
    let wants_cargo = cfg
        .packages
        .iter()
        .any(|p| p.from.contains(&Manager::Cargo));
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

    // Declaring `installer: rustup` or `from: cargo` IS declaring you need
    // that manager. Making the user also list it in `package_managers:` meant
    // a fresh machine ran `rustup toolchain install` against a rustup nothing
    // had installed -- which is the one thing a bootstrap tool must not do.
    //
    // Only bootstrappable ones are added: apt and zypper come with the distro,
    // and a package asking for a missing one already errors by name.
    let mut managers: Vec<Manager> = cfg.package_managers.clone();
    let want = |m: Manager, into: &mut Vec<Manager>| {
        if m.is_bootstrappable() && m.runs_on(facts.os) && !into.contains(&m) {
            into.push(m);
        }
    };
    for l in &languages {
        let m = l
            .installer
            .unwrap_or_else(|| crate::recipe::default_installer(&l.name));
        // cargo is rustup's own output, so rustup is what gets installed.
        want(
            if m == Manager::Cargo {
                Manager::Rustup
            } else {
                m
            },
            &mut managers,
        );
    }
    for p in &cfg.packages {
        for m in &p.from {
            want(
                if *m == Manager::Cargo {
                    Manager::Rustup
                } else {
                    *m
                },
                &mut managers,
            );
        }
    }

    // ---- managers
    for m in &managers {
        let id = format!("manager/{m}");
        let present = facts.managers.contains(m) || state.done(&id).is_some();
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
            payload: Payload::Manager(*m),
        });
        declared_ids.insert(id);
    }

    // ---- languages
    for l in &languages {
        let id = format!("language/{}", l.name);
        let bin_dirs = crate::recipe::bin_dirs(&l.name, facts);
        let probe = crate::recipe::probe_bin(&l.name);
        let installed = !state.interrupted(&id)
            && (state.done(&id).is_some()
                || host.which(probe, &bin_dirs).is_some()
                || host.which(probe, &system_path(facts)).is_some());
        let installer = l
            .installer
            .unwrap_or_else(|| crate::recipe::default_installer(&l.name));
        if installer == Manager::Cargo || installer == Manager::Rustup {
            available.insert(Manager::Cargo);
        }
        items.push(Item {
            id: id.clone(),
            kind: ItemKind::Language,
            name: l.name.clone(),
            action: if installed {
                Action::NoOp
            } else {
                Action::Create
            },
            detail: format!(
                "{}  {}",
                l.version.as_deref().unwrap_or("latest"),
                installer
            ),
            needs_root: false,
            arms: arms_of(&l.resolved_from),
            payload: Payload::Language {
                installer,
                version: l.version.clone(),
                bin_dirs: bin_dirs.clone(),
            },
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
        if let Some(script) = &p.script {
            // Presence is the binary being there. There is no manager to ask,
            // and re-running an installer that is already satisfied is the
            // kind of surprise a plan exists to prevent.
            let known = state.done(&id);
            let on_machine = !state.interrupted(&id)
                && (known.is_some() || host.which(&p.name, &search).is_some());
            items.push(Item {
                id: id.clone(),
                kind: ItemKind::Package,
                name: p.name.clone(),
                action: if on_machine {
                    Action::NoOp
                } else {
                    Action::Create
                },
                detail: "script".into(),
                needs_root: false,
                arms: arms_of(&p.resolved_from),
                payload: Payload::ScriptPackage {
                    name: p.name.clone(),
                    script: script.clone(),
                },
            });
            declared_ids.insert(id);
            continue;
        }
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
        let previous = match &action {
            Action::Reinstall { from_method, .. } => Manager::parse(from_method),
            _ => None,
        };
        items.push(Item {
            id: id.clone(),
            kind: ItemKind::Package,
            name: p.name.clone(),
            action,
            detail: format!("{}  {manager}", p.version.as_deref().unwrap_or("latest")),
            needs_root: crate::recipe::needs_root(manager),
            arms: arms_of(&p.resolved_from),
            payload: Payload::Package {
                manager,
                version: p.version.clone(),
                previous,
            },
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

        // Render now, so the plan can tell whether the file would actually
        // change. Comparing ids alone made every managed file write-once:
        // editing the template or a `vars:` value printed "No changes" forever.
        let template = String::from_utf8(
            host.read(&src)
                .map_err(|e| ConfigError::new(e.to_string()))?
                .unwrap_or_default(),
        )
        .map_err(|_| ConfigError::new(format!("{} is not valid UTF-8", src.display())))?;
        let want = crate::writers::digest(
            &crate::render::render(
                &crate::value::Tmpl(template),
                &crate::render::Context {
                    facts,
                    vars: &cfg.vars,
                },
            )
            .map_err(|e| ConfigError::new(e.to_string()))?,
        );

        items.push(Item {
            id: id.clone(),
            kind: ItemKind::File,
            name: display_home(&dest, facts),
            action: match state.done(&id) {
                // §9.1: back the user's file up before the first write. A
                // create and an adopt are different work, so they are
                // different actions.
                None if exists => Action::Adopt,
                None => Action::Create,
                Some(st) => content_action(st.hash.as_deref(), &want, read_digest(host, &dest)?),
            },
            detail: format!("from {}", f.src),
            needs_root: !dest.starts_with(&facts.home),
            arms: arms_of(&f.resolved_from),
            payload: Payload::File {
                src: src.clone(),
                dest: dest.clone(),
                // 0600 under ~/.ssh and ~/.gnupg: a 0644 private key is a
                // quiet way to break someone's day.
                mode: f
                    .mode
                    .as_deref()
                    .and_then(|m| u32::from_str_radix(m, 8).ok())
                    .unwrap_or(
                        if dest.starts_with(facts.home.join(".ssh"))
                            || dest.starts_with(facts.home.join(".gnupg"))
                        {
                            0o600
                        } else {
                            0o644
                        },
                    ),
            },
        });
        declared_ids.insert(id);
    }

    // ---- links, after repos: a link into a repo the run clones is normal
    // ---- (see below; this block is emitted after the repo loop)

    // ---- the shell framework, before repos: a repo may clone INTO the
    // framework's own directory (a plugin under ~/.oh-my-zsh/custom), and
    // creating that directory first makes the framework's installer refuse.
    if let Some(fw) = &cfg.framework {
        if let Some((home, _)) = crate::recipe::framework_install(&fw.kind, facts) {
            let id = format!("framework/{}", fw.kind);
            let present = host
                .symlink_meta(&home)
                .map_err(|e| ConfigError::new(e.to_string()))?
                .is_some();
            items.push(Item {
                id: id.clone(),
                kind: ItemKind::Manager,
                name: fw.kind.clone(),
                action: if present || state.done(&id).is_some() {
                    Action::NoOp
                } else {
                    Action::Create
                },
                detail: format!("shell framework for {}", cfg.shell),
                needs_root: false,
                arms: BTreeMap::new(),
                payload: Payload::Framework {
                    kind: fw.kind.clone(),
                    home,
                },
            });
            declared_ids.insert(id);

            // Only for plugins with no repo declared for them: naming one the
            // user has already handled is noise, and noise trains people to stop
            // reading warnings.
            for p in crate::writers::unbundled_plugins(&fw.plugins) {
                let handled = cfg
                    .repos
                    .iter()
                    .any(|r| r.dest.trim_end_matches('/').ends_with(p.as_str()));
                if handled {
                    continue;
                }
                warnings.push(format!(
                    "plugin `{p}` is not bundled with {}, so listing it does nothing on its own. \
                 Add a repo: url https://github.com/zsh-users/{p}, dest \
                 \"{{{{ home }}}}/.oh-my-zsh/custom/plugins/{p}\"",
                    fw.kind
                ));
            }
        }
    }

    // ---- repos: config that lives in a git repository
    for repo in &cfg.repos {
        let dest = normalize(&repo.dest, &facts.home, &facts.home);
        if !dest.starts_with(&facts.home) {
            return Err(ConfigError::new(format!(
                "repo `{}` would clone to {}, which is outside your home directory",
                repo.url,
                dest.display()
            )));
        }
        let id = format!("repo/{}", dest.display());
        let on_disk = host
            .symlink_meta(&dest)
            .map_err(|e| ConfigError::new(e.to_string()))?
            .is_some();
        let known = state.done(&id);
        let action = match (known, on_disk) {
            // A different remote at the same path is not an update.
            (Some(st), _) if st.method.as_deref() != Some(repo.url.as_str()) => Action::Reinstall {
                from_method: st.method.clone().unwrap_or_default(),
                to_method: repo.url.clone(),
            },
            // Present and ours is done. Pulling on every apply would mean
            // `plan` never converges and would make it claim a change it
            // cannot know about without going to the network -- the same rule
            // as `version: latest` (§7.2). `bedouin sync` is what pulls.
            (Some(_), true) => Action::NoOp,
            (Some(_), false) => Action::Create,
            // Already there and not ours: adopted, never touched. Clobbering
            // someone's hand-managed config is the data-loss class §14b is
            // about.
            (None, true) => Action::NoOp,
            (None, false) => Action::Create,
        };
        items.push(Item {
            id: id.clone(),
            kind: ItemKind::Dir,
            name: display_home(&dest, facts),
            action,
            detail: match (&repo.r#ref, known.is_none() && on_disk) {
                (_, true) => "already here; adopted, not touched".to_string(),
                (Some(r), _) => format!("{} @ {r}", repo.url),
                (None, _) => repo.url.clone(),
            },
            needs_root: false,
            arms: arms_of(&repo.resolved_from),
            payload: Payload::Repo {
                url: repo.url.clone(),
                dest,
                reference: repo.r#ref.clone(),
            },
        });
        declared_ids.insert(id);
    }

    // ---- links: symlinks Bedouin owns. After repos, since a link into a
    // repository this run clones is the normal case.
    for link in &cfg.links {
        let dest = normalize(&link.dest, &facts.home, &facts.home);
        let src = normalize(&link.src, &facts.home, &facts.home);
        if !dest.starts_with(&facts.home) {
            return Err(ConfigError::new(format!(
                "link `{}` would be created outside your home directory",
                dest.display()
            )));
        }
        let id = format!("link/{}", dest.display());
        let current = host
            .read_link(&dest)
            .map_err(|e| ConfigError::new(e.to_string()))?;
        let exists = host
            .symlink_meta(&dest)
            .map_err(|e| ConfigError::new(e.to_string()))?
            .is_some();
        let ours = state.done(&id).is_some();

        // Anything at `dest` that bedouin did not make is refused, not
        // replaced. §9.1 exists because a first apply once destroyed a
        // ~/.gitconfig; a link is no different.
        if exists && !ours && current.is_none() {
            return Err(ConfigError::new(format!(
                "{} already exists and is not a link bedouin made.\n  \
                 Refusing to replace it -- move it aside if you want bedouin to \
                 own that path",
                display_home(&dest, facts)
            )));
        }
        if exists && !ours && current.is_some() {
            return Err(ConfigError::new(format!(
                "{} is already a symlink, and not one bedouin made.\n  \
                 Refusing to repoint it -- remove it if you want bedouin to own it",
                display_home(&dest, facts)
            )));
        }

        let action = match (&current, ours) {
            (Some(t), _) if *t == src => Action::NoOp,
            (Some(t), true) => Action::Upgrade {
                from: t.display().to_string(),
                to: src.display().to_string(),
            },
            _ => Action::Create,
        };
        items.push(Item {
            id: id.clone(),
            kind: ItemKind::File,
            name: display_home(&dest, facts),
            action,
            detail: format!("-> {}", display_home(&src, facts)),
            needs_root: false,
            arms: arms_of(&link.resolved_from),
            payload: Payload::Link { src, dest },
        });
        declared_ids.insert(id);
    }

    // ---- rc blocks and PATH, both of which need a shell Bedouin can write for
    let rc_capable = cfg.shell != Shell::Other;
    if !rc_capable
        && (!cfg.aliases.is_empty()
            || cfg.packages.iter().any(|p| {
                !p.rc.is_empty()
                    || !p.path.is_empty()
                    || !p.aliases.is_empty()
                    || p.completions.is_some()
            }))
    {
        return Err(ConfigError::new(
            "this config writes rc blocks or PATH entries, but the shell is not one \
             Bedouin knows how to write for.\n  \
             Declare `shell: zsh` (or bash, or fish) at the top level",
        ));
    }
    if rc_capable {
        let writes_shell_files = !cfg.aliases.is_empty()
            || cfg.packages.iter().any(|p| {
                !p.rc.is_empty()
                    || !p.path.is_empty()
                    || !p.aliases.is_empty()
                    || p.completions.is_some()
            });
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
                payload: Payload::Dir(dir.clone()),
            });
            declared_ids.insert(dir_id);

            // fish sources conf.d natively and needs no block.
            if cfg.shell != Shell::Fish {
                let id = "rc/bedouin/source".to_string();
                let snippet =
                    crate::writers::source_dir_snippet(&dir.display().to_string(), cfg.shell);
                let want = crate::writers::block_digest(&snippet);
                items.push(Item {
                    id: id.clone(),
                    kind: ItemKind::Rc,
                    name: display_home(&facts.shell.rc_file, facts),
                    action: match state.done(&id) {
                        None => Action::Create,
                        Some(st) => content_action(
                            st.rc_blocks.first().map(|b| b.hash.as_str()),
                            &want,
                            read_block_digest(host, &facts.shell.rc_file, "source")?,
                        ),
                    },
                    detail: format!("managed block: source {}", display_home(dir, facts)),
                    needs_root: false,
                    arms: BTreeMap::new(),
                    payload: Payload::RcBlock {
                        file: facts.shell.rc_file.clone(),
                        marker: "source".into(),
                        content: crate::writers::source_dir_snippet(
                            &dir.display().to_string(),
                            cfg.shell,
                        ),
                    },
                });
                declared_ids.insert(id);
            }
        }

        // The framework block: the settings it reads as it loads.
        if let Some(fw) = &cfg.framework {
            let content = crate::writers::framework_block(fw.theme.as_deref(), &fw.plugins);
            if !content.is_empty() {
                let id = "rc/bedouin/framework".to_string();
                let want = crate::writers::block_digest(&content);
                items.push(Item {
                    id: id.clone(),
                    kind: ItemKind::Rc,
                    name: display_home(&facts.shell.rc_file, facts),
                    action: match state.done(&id) {
                        None => Action::Create,
                        Some(st) => content_action(
                            st.rc_blocks.first().map(|b| b.hash.as_str()),
                            &want,
                            read_block_digest(host, &facts.shell.rc_file, "framework")?,
                        ),
                    },
                    detail: match &fw.theme {
                        Some(t) => format!("theme {t}, {} plugin(s)", fw.plugins.len()),
                        None => format!("{} plugin(s)", fw.plugins.len()),
                    },
                    needs_root: false,
                    arms: BTreeMap::new(),
                    payload: Payload::AnchoredBlock {
                        file: facts.shell.rc_file.clone(),
                        marker: "framework".into(),
                        content,
                        anchor: crate::writers::OMZ_ANCHOR.to_string(),
                    },
                });
                declared_ids.insert(id);
            }
        }

        // Global aliases: one block Bedouin owns.
        if !cfg.aliases.is_empty() {
            let file = facts
                .shell
                .rc_dir
                .join(format!("10-bedouin-aliases.{}", facts.shell.rc_ext()));
            let content = crate::writers::alias_lines(&cfg.aliases, cfg.shell);
            let id = "rc/bedouin/aliases".to_string();
            let want = crate::writers::block_digest(&content);
            items.push(Item {
                id: id.clone(),
                kind: ItemKind::Rc,
                name: display_home(&file, facts),
                action: match state.done(&id) {
                    None => Action::Create,
                    Some(st) => content_action(
                        st.rc_blocks.first().map(|b| b.hash.as_str()),
                        &want,
                        read_block_digest(host, &file, "aliases")?,
                    ),
                },
                detail: format!("{} global aliases", cfg.aliases.len()),
                needs_root: false,
                arms: BTreeMap::new(),
                payload: Payload::RcBlock {
                    file,
                    marker: "aliases".into(),
                    content,
                },
            });
            declared_ids.insert(id);
        }

        // A package's aliases live with the package, so dropping it takes them
        // with it through machinery that already converges.
        for p in &cfg.packages {
            if p.aliases.is_empty() {
                continue;
            }
            let file =
                facts
                    .shell
                    .rc_dir
                    .join(format!("30-{}-aliases.{}", p.name, facts.shell.rc_ext()));
            let content = crate::writers::alias_lines(&p.aliases, cfg.shell);
            let id = format!("rc/{}/aliases", p.name);
            let want = crate::writers::block_digest(&content);
            items.push(Item {
                id: id.clone(),
                kind: ItemKind::Rc,
                name: display_home(&file, facts),
                action: match state.done(&id) {
                    None => Action::Create,
                    Some(st) => content_action(
                        st.rc_blocks.first().map(|b| b.hash.as_str()),
                        &want,
                        read_block_digest(host, &file, &p.name)?,
                    ),
                },
                detail: format!("{} aliases for {}", p.aliases.len(), p.name),
                needs_root: false,
                arms: BTreeMap::new(),
                payload: Payload::RcBlock {
                    file,
                    marker: p.name.clone(),
                    content,
                },
            });
            declared_ids.insert(id);
        }

        // Completions: generated by the tool itself, after it is installed.
        let comp_dir = crate::writers::completions_dir(cfg.shell, &facts.shell.rc_dir, &facts.home);
        for p in &cfg.packages {
            let Some(argv) = &p.completions else { continue };
            let dest = crate::writers::completions_file(cfg.shell, &comp_dir, &p.name);
            let id = format!("completion/{}", p.name);
            // Content-addressed on the COMMAND, not on its output: the output
            // cannot be known until it runs, and plan does not run it. Editing
            // `generate:` therefore takes effect, and a package upgrade
            // re-runs it because output can differ by version.
            let want = crate::writers::digest(&argv.join(" "));
            let pkg_changing = items
                .iter()
                .any(|i| i.id == format!("package/{}", p.name) && i.action.is_change());
            items.push(Item {
                id: id.clone(),
                kind: ItemKind::File,
                name: display_home(&dest, facts),
                action: match state.done(&id) {
                    None => Action::Create,
                    Some(_) if pkg_changing => Action::Upgrade {
                        from: "package changed".into(),
                        to: "regenerated".into(),
                    },
                    // Two questions, and they need different answers. Did the
                    // COMMAND change (state.method) -- plan can see that. Did
                    // the FILE change -- plan can see that too, by hashing it
                    // against what apply recorded. What plan cannot see is
                    // whether re-running would now produce different output.
                    Some(st) if st.method.as_deref() != Some(want.as_str()) => Action::Upgrade {
                        from: "generator changed".into(),
                        to: "regenerated".into(),
                    },
                    Some(st) => match (st.hash.as_deref(), read_digest(host, &dest)?) {
                        (_, None) => Action::Create,
                        (Some(had), Some(found)) if had != found => Action::Upgrade {
                            from: "edited on disk".into(),
                            to: "regenerated".into(),
                        },
                        _ => Action::NoOp,
                    },
                },
                detail: format!("{} completions from `{}`", cfg.shell, argv.join(" ")),
                needs_root: false,
                arms: BTreeMap::new(),
                payload: Payload::Completions {
                    argv: argv.clone(),
                    dest,
                },
            });
            declared_ids.insert(id);
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
                let want = crate::writers::block_digest(&block.content);
                items.push(Item {
                    id: id.clone(),
                    kind: ItemKind::Rc,
                    name: display_home(&file, facts),
                    // Content-addressed, so editing `content:` in the config
                    // takes effect, and so does a hand edit inside the markers.
                    action: match state.done(&id) {
                        None => Action::Create,
                        Some(st) => content_action(
                            st.rc_blocks.first().map(|b| b.hash.as_str()),
                            &want,
                            read_block_digest(host, &file, &p.name)?,
                        ),
                    },
                    detail: format!("owned by {}", p.name),
                    needs_root: false,
                    arms: BTreeMap::new(),
                    payload: Payload::RcBlock {
                        file: file.clone(),
                        marker: p.name.clone(),
                        content: block.content.clone(),
                    },
                });
                declared_ids.insert(id);
            }
        }

        let mut path_entries: Vec<(String, String)> = Vec::new();
        // Everything bedouin installs a toolchain INTO. The run itself already
        // sees these -- `step_env` builds them -- but the user's shell never
        // did, so mise would install neovim and go, and the shell would report
        // neither as present. Installing a thing without making it runnable is
        // not installing it.
        for m in &managers {
            for d in crate::recipe::bin_dirs(m.as_str(), facts) {
                let key = d.display().to_string();
                if !path_entries.iter().any(|(k, _)| *k == key) {
                    path_entries.push((key, m.to_string()));
                }
            }
        }
        for l in &languages {
            for d in crate::recipe::bin_dirs(&l.name, facts) {
                let key = d.display().to_string();
                if !path_entries.iter().any(|(k, _)| *k == key) {
                    path_entries.push((key, l.name.clone()));
                }
            }
        }
        for p in &cfg.packages {
            for entry in &p.path {
                let norm = normalize(entry, &facts.home, &facts.home);
                let key = norm.display().to_string();
                if !path_entries.iter().any(|(k, _)| *k == key) {
                    path_entries.push((key, p.name.clone()));
                }
            }
        }
        let all_path_entries: Vec<String> = path_entries.iter().map(|(k, _)| k.clone()).collect();
        if !all_path_entries.is_empty() {
            let file = facts
                .shell
                .rc_dir
                .join(format!("00-bedouin-path.{}", facts.shell.rc_ext()));
            let id = format!("path/{}", file.display());
            let want =
                crate::writers::digest(&crate::writers::path_file(&all_path_entries, cfg.shell));
            let owners: Vec<&str> = {
                let mut o: Vec<&str> = path_entries.iter().map(|(_, p)| p.as_str()).collect();
                o.dedup();
                o
            };
            items.push(Item {
                id: id.clone(),
                kind: ItemKind::Path,
                name: display_home(&file, facts),
                action: match state.done(&id) {
                    None => Action::Create,
                    Some(st) => {
                        content_action(st.hash.as_deref(), &want, read_digest(host, &file)?)
                    }
                },
                detail: format!(
                    "{} {} from {}",
                    all_path_entries.len(),
                    if all_path_entries.len() == 1 {
                        "entry"
                    } else {
                        "entries"
                    },
                    owners.join(", ")
                ),
                needs_root: false,
                arms: BTreeMap::new(),
                payload: Payload::PathFile {
                    file,
                    entries: all_path_entries,
                },
            });
            declared_ids.insert(id);
        }
    }

    // ---- one item, one id (§7.2): two nodes sharing a state key would fight
    // over it, and one of the two would become unowned and unremovable.
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
        // Prefer the path we recorded: `rc/jq/70-demo.zsh` is an id, not a
        // thing the user recognises.
        let name = item
            .owned_files
            .first()
            .or_else(|| item.rc_blocks.first().map(|b| &b.file))
            .map(|f| display_home(std::path::Path::new(f), facts))
            .unwrap_or_else(|| {
                let tail = id.split_once('/').map_or(id.as_str(), |(_, n)| n);
                match item.kind {
                    ItemKind::Dir | ItemKind::Path => {
                        display_home(std::path::Path::new(tail), facts)
                    }
                    _ => tail.to_string(),
                }
            });
        items.push(Item {
            id: id.clone(),
            kind: item.kind,
            name,
            action: Action::Remove,
            detail: match &item.method {
                Some(m) => format!("was: {m}, owner: bedouin"),
                None => "owner: bedouin".into(),
            },
            needs_root: false,
            arms: BTreeMap::new(),
            payload: Payload::None,
        });
    }

    Ok(Plan {
        items,
        pruned: cfg.pruned.clone(),
        warnings,
    })
}

/// Hash of a whole file as it stands, or `None` if it is not there.
fn read_digest(host: &dyn Host, p: &std::path::Path) -> Result<Option<String>> {
    Ok(host
        .read(p)
        .map_err(|e| ConfigError::new(e.to_string()))?
        .map(|b| crate::writers::digest(&String::from_utf8_lossy(&b))))
}

/// Hash of one owned block inside a file, or `None` if the block is not there.
fn read_block_digest(host: &dyn Host, p: &std::path::Path, marker: &str) -> Result<Option<String>> {
    let Some(bytes) = host.read(p).map_err(|e| ConfigError::new(e.to_string()))? else {
        return Ok(None);
    };
    // An unterminated block is not "absent"; leave it to the executor, which
    // refuses rather than guessing where it ended.
    Ok(
        crate::writers::extract_block(&String::from_utf8_lossy(&bytes), marker)
            .ok()
            .flatten()
            .map(|c| crate::writers::block_digest(&c)),
    )
}

/// Print paths under `$HOME` with a `~`, which is how the user wrote them.
fn display_home(p: &std::path::Path, facts: &Facts) -> String {
    match p.strip_prefix(&facts.home) {
        Ok(rest) => format!("~/{}", rest.display()),
        Err(_) => p.display().to_string(),
    }
}
