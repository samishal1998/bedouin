//! The executor: the half that changes the machine.
//!
//! Three invariants hold the whole thing together.
//!
//! **Intent before work.** A step writes `status: incomplete` and flushes
//! *before* it begins, and flips to `complete` when it succeeds. Recording only
//! on success leaves a run interrupted between "installed" and "flushed" with a
//! package Bedouin installed that looks pre-existing -- permanently
//! un-removable, and silently so.
//!
//! **The environment is constructed, never inherited.** `PATH` comes from the
//! bin directories the state manifest records, so the step that installs a
//! cargo package finds the cargo the previous step installed. This is the fix
//! for the Ansible reload problem and the reason one run can do both.
//!
//! **Stop on the first failure.** Not transactional, and not pretending to be:
//! rolling back a package manager is not something Bedouin can do honestly. A
//! half-configured machine that reports success is worse than one that reports
//! where it broke, and re-running resumes because `plan` re-diffs.

use crate::facts::{Facts, Manager, Privilege};
use crate::host::{Cmd, Host, Line};
use crate::plan::{Action, Item, Payload, Plan};
use crate::recipe;
use crate::render::{self, Context};
use crate::schema::{Config, ConfigError, Result};
use crate::state::{self, ItemKind, Owner, State, StateItem, Status};
use crate::value::Tmpl;
use crate::writers;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Failure {
    pub id: String,
    pub message: String,
    pub output_tail: Vec<String>,
}

#[derive(Debug, Clone, Default)]
pub struct Report {
    pub completed: Vec<String>,
    pub failure: Option<Failure>,
    /// Steps after the failure. Naming them is the difference between "it
    /// broke" and "here is what did not happen".
    pub not_attempted: Vec<String>,
}

impl Report {
    pub fn ok(&self) -> bool {
        self.failure.is_none()
    }

    pub fn render(&self) -> String {
        let mut out = String::new();
        match &self.failure {
            None if self.completed.is_empty() => {
                out.push_str("Nothing to do. The machine already matches the config.\n");
            }
            None => out.push_str(&format!("Applied {} changes.\n", self.completed.len())),
            Some(f) => {
                out.push_str(&format!("\nFailed at {}: {}\n", f.id, f.message));
                for l in &f.output_tail {
                    out.push_str(&format!("    {l}\n"));
                }
                out.push_str(&format!(
                    "\n{} applied before the failure.\n",
                    self.completed.len()
                ));
                if !self.not_attempted.is_empty() {
                    out.push_str("Not attempted:\n");
                    for id in &self.not_attempted {
                        out.push_str(&format!("  {id}\n"));
                    }
                }
                out.push_str("\nFix the cause and re-run: completed items diff as no-ops.\n");
            }
        }
        out
    }
}

/// The environment every step is spawned with.
///
/// Constructed from the state manifest plus a minimal system base -- never the
/// parent shell's, which is the entire point.
fn step_env(state: &State, facts: &Facts) -> BTreeMap<String, String> {
    let mut path: Vec<String> = state
        .bin_dirs()
        .into_iter()
        // Every place bedouin's own managers install to, whether or not state
        // records one yet. Building PATH from state alone made a toolchain
        // bedouin did NOT install invisible to every step -- so `installer:
        // rustup` on a box that already had rustup failed with "No such file
        // or directory", which is the confusing half of the very problem a
        // constructed environment exists to solve. Naming the directories
        // rather than trusting the ambient PATH is the point.
        .chain(
            crate::facts::Manager::ALL
                .iter()
                .flat_map(|m| crate::recipe::bin_dirs(m.as_str(), facts)),
        )
        .chain(crate::plan::system_path(facts))
        .map(|p| p.display().to_string())
        .collect();
    path.dedup();
    let mut env = BTreeMap::new();
    env.insert("PATH".into(), path.join(":"));
    env.insert("HOME".into(), facts.home.display().to_string());
    env.insert("USER".into(), facts.user.clone());
    // Package managers get noisy or interactive without these.
    env.insert("DEBIAN_FRONTEND".into(), "noninteractive".into());
    env.insert("LC_ALL".into(), "C".into());
    for keep in [
        "TERM",
        "SSL_CERT_FILE",
        "SSL_CERT_DIR",
        "HTTPS_PROXY",
        "HTTP_PROXY",
        "NO_PROXY",
    ] {
        if let Some(v) = facts.env.get(keep) {
            env.insert(keep.into(), v.clone());
        }
    }
    env
}

struct Executor<'a> {
    /// Managers whose lists have been refreshed this run. Once each, not once
    /// per package.
    refreshed: std::collections::BTreeSet<Manager>,
    host: &'a dyn Host,
    facts: &'a Facts,
    cfg: &'a Config,
    state: State,
    state_path: PathBuf,
    out: &'a mut dyn FnMut(Line),
}

impl Executor<'_> {
    /// Persist state. Called before and after every step, so an interrupted run
    /// leaves a truthful record rather than a stale one.
    fn flush(&self) -> Result<()> {
        let json = serde_json::to_string_pretty(&self.state)
            .map_err(|e| ConfigError::new(format!("serialising state: {e}")))?;
        self.host
            .write(&self.state_path, json.as_bytes(), 0o600)
            .map_err(|e| ConfigError::new(e.to_string()))
    }

    /// Prepend `sudo -n` where a step declares it needs root.
    ///
    /// Escalation lives here rather than inside `Host` so it is visible in the
    /// argv a test asserts on -- a Host that silently rewrote commands would
    /// hide the one thing worth checking.
    fn escalate(&self, mut cmd: Cmd) -> Cmd {
        if cmd.root && self.facts.privilege != Privilege::Root {
            let mut argv = vec!["sudo".to_string(), "-n".to_string()];
            argv.append(&mut cmd.argv);
            cmd.argv = argv;
        }
        cmd
    }

    /// Refresh a manager's package lists, at most once per run.
    fn refresh(&mut self, m: Manager) -> std::result::Result<(), (String, Vec<String>)> {
        if !self.refreshed.insert(m) {
            return Ok(());
        }
        let Some(cmd) = recipe::refresh(m) else {
            return Ok(());
        };
        let mut cmd = self.escalate(cmd);
        cmd.env = step_env(&self.state, self.facts);
        self.run(&cmd)
    }

    fn run(&mut self, cmd: &Cmd) -> std::result::Result<(), (String, Vec<String>)> {
        let mut tail: Vec<String> = Vec::new();
        let status = match self.host.run(cmd, &mut |l| {
            if tail.len() >= 20 {
                tail.remove(0);
            }
            tail.push(l.text().to_string());
            (self.out)(l);
        }) {
            Ok(s) => s,
            Err(e) => return Err((e.to_string(), tail)),
        };
        if status.timed_out {
            return Err((format!("`{}` timed out", cmd.display()), tail));
        }
        if !status.ok() {
            return Err((format!("`{}` exited {}", cmd.display(), status.code), tail));
        }
        Ok(())
    }

    fn write_text(
        &self,
        p: &Path,
        text: &str,
        mode: u32,
    ) -> std::result::Result<(), (String, Vec<String>)> {
        self.host
            .write(p, text.as_bytes(), mode)
            .map_err(|e| (e.to_string(), Vec::new()))
    }

    /// Writing through a symlink puts content somewhere the config does not
    /// name -- and `OsHost::write` renames over the path, which would silently
    /// sever a dotfiles-repo symlink.
    fn refuse_symlink(&self, p: &Path) -> std::result::Result<(), (String, Vec<String>)> {
        let meta = self
            .host
            .symlink_meta(p)
            .map_err(|e| (e.to_string(), Vec::new()))?;
        if meta.is_some_and(|m| m.is_symlink) {
            return Err((
                format!(
                    "{} is a symlink; refusing to write through it -- that would \
                     put content somewhere the config does not name, and replace \
                     the link with a regular file",
                    p.display()
                ),
                Vec::new(),
            ));
        }
        Ok(())
    }

    /// Read a file that Bedouin is about to rewrite.
    ///
    /// Non-UTF-8 is an error rather than a lossy decode: `from_utf8_lossy`
    /// turns every stray byte into U+FFFD, and the result was written straight
    /// back -- corrupting the user's live rc file, and corrupting the backup an
    /// adopt takes of it.
    fn read_text(&self, p: &Path) -> std::result::Result<String, (String, Vec<String>)> {
        match self.host.read(p) {
            Ok(Some(b)) => String::from_utf8(b).map_err(|_| {
                (
                    format!(
                        "{} is not valid UTF-8. Refusing to rewrite it: decoding \
                         it would replace those bytes and write the damage back",
                        p.display()
                    ),
                    Vec::new(),
                )
            }),
            Ok(None) => Ok(String::new()),
            Err(e) => Err((e.to_string(), Vec::new())),
        }
    }

    /// Read a file that must exist. Used on the restore path, where treating a
    /// missing backup as empty would write a zero-byte file over the user's data.
    fn read_existing(&self, p: &Path) -> std::result::Result<String, (String, Vec<String>)> {
        match self.host.read(p) {
            Ok(Some(b)) => String::from_utf8(b)
                .map_err(|_| (format!("{} is not valid UTF-8", p.display()), Vec::new())),
            Ok(None) => Err((
                format!(
                    "{} has gone missing, so there is nothing to restore",
                    p.display()
                ),
                Vec::new(),
            )),
            Err(e) => Err((e.to_string(), Vec::new())),
        }
    }

    /// Carry out one item and return the state entry it produced.
    fn step(&mut self, item: &Item) -> std::result::Result<StateItem, (String, Vec<String>)> {
        let mut rec = StateItem::new(item.kind, Owner::Bedouin);
        rec.status = Status::Complete;
        rec.resolved_from = item
            .arms
            .iter()
            .map(|(k, v)| (k.clone(), crate::value::Winner::Arm(v.clone())))
            .collect();

        match (&item.action, &item.payload) {
            (Action::Remove, _) => {
                // Only what state says we own, undone the way it went in.
                let Some(prev) = self.state.items.get(&item.id).cloned() else {
                    return Ok(rec);
                };

                if item.kind == ItemKind::Package {
                    if let Some(m) = prev.method.as_deref().and_then(Manager::parse) {
                        let mut cmd = self.escalate(recipe::remove(m, &item.name));
                        cmd.env = step_env(&self.state, self.facts);
                        // A package already gone by other means must not wedge
                        // every future apply: stop-on-first-failure plus "drop
                        // the entry only on success" would rerun the same
                        // doomed command forever, and nothing after it would
                        // ever run again.
                        if let Err((msg, tail)) = self.run(&cmd) {
                            (self.out)(Line::Err(format!(
                                "could not uninstall {}: {msg}. Dropping bedouin's \
                                 record of it -- the package, if still present, is \
                                 now yours to remove",
                                item.name
                            )));
                            for l in tail {
                                (self.out)(Line::Err(l));
                            }
                        }
                    }
                }

                // A block inside a file that is the user's: take the block out
                // and leave the rest of their file alone.
                for block in &prev.rc_blocks {
                    let path = PathBuf::from(&block.file);
                    if prev.owned_files.contains(&block.file) {
                        continue; // handled below, as a whole file
                    }
                    let existing = self.read_text(&path)?;
                    let cleaned = writers::remove_block(&existing, &block.marker)
                        .map_err(|e| (e.to_string(), Vec::new()))?;
                    // A drop-in file that is now empty is litter Bedouin made,
                    // so tidy it -- but only inside the drop-in directory. The
                    // same rule applied to `~/.zshrc` would delete a user's own
                    // file for the crime of being blank.
                    if cleaned.text.trim().is_empty() && path.starts_with(&self.facts.shell.rc_dir)
                    {
                        self.host
                            .remove(&path)
                            .map_err(|e| (e.to_string(), Vec::new()))?;
                    } else {
                        self.write_text(&path, &cleaned.text, 0o644)?;
                    }
                }

                // Files Bedouin created outright.
                for f in &prev.owned_files {
                    self.host
                        .remove(Path::new(f))
                        .map_err(|e| (e.to_string(), Vec::new()))?;
                }

                // A managed file that displaced one of the user's gives it
                // back. The destination comes from state, not from the plan
                // payload: a removal is built from what state records, so the
                // payload is empty and reading it here silently skipped the
                // restore -- deleting the file and keeping the backup, which is
                // the user's own content lost in all but name.
                if let (Some(backup), Some(dest)) = (&prev.backup, prev.owned_files.first()) {
                    let saved = self.read_existing(Path::new(backup))?;
                    let mode = prev
                        .mode
                        .as_deref()
                        .and_then(|m| u32::from_str_radix(m, 8).ok())
                        .unwrap_or(0o644);
                    self.write_text(Path::new(dest), &saved, mode)?;
                    self.host
                        .remove(Path::new(backup))
                        .map_err(|e| (e.to_string(), Vec::new()))?;
                }

                if item.kind == ItemKind::Dir {
                    // Only if empty: Bedouin created the directory, but it does
                    // not follow that everything now inside it is ours.
                    let dir = item.id.trim_start_matches("dir/");
                    self.host
                        .remove_dir(Path::new(dir))
                        .map_err(|e| (e.to_string(), Vec::new()))?;
                }
            }

            (_, Payload::Manager(m)) => {
                let Some(steps) = recipe::bootstrap(*m, self.facts) else {
                    return Err((
                        format!("`{m}` is not installed and cannot be bootstrapped"),
                        Vec::new(),
                    ));
                };
                for cmd in steps {
                    let mut cmd = self.escalate(cmd);
                    cmd.env = step_env(&self.state, self.facts);
                    self.run(&cmd)?;
                }
                rec.method = Some(m.to_string());
                rec.bin_dirs = recipe::bin_dirs(m.as_str(), self.facts)
                    .iter()
                    .map(|p| p.display().to_string())
                    .collect();
            }

            (
                _,
                Payload::Language {
                    installer,
                    version,
                    bin_dirs,
                },
            ) => {
                let mut cmd =
                    self.escalate(recipe::install(*installer, &item.name, version.as_deref()));
                cmd.env = step_env(&self.state, self.facts);
                self.run(&cmd)?;
                rec.version = version.clone();
                rec.method = Some(installer.to_string());
                rec.bin_dirs = bin_dirs.iter().map(|p| p.display().to_string()).collect();
            }

            (
                action,
                Payload::Package {
                    manager,
                    version,
                    previous,
                },
            ) => {
                // A changed method means the old copy comes out first;
                // installing twice would leave two, one of them unowned.
                if let (Action::Reinstall { .. }, Some(old)) = (action, previous) {
                    let mut cmd = self.escalate(recipe::remove(*old, &item.name));
                    cmd.env = step_env(&self.state, self.facts);
                    self.run(&cmd)?;
                }
                self.refresh(*manager)?;
                let mut cmd =
                    self.escalate(recipe::install(*manager, &item.name, version.as_deref()));
                cmd.env = step_env(&self.state, self.facts);
                self.run(&cmd)?;
                rec.version = version.clone();
                rec.method = Some(manager.to_string());
            }

            (_, Payload::Dir(d)) => {
                self.host
                    .mkdir_p(d)
                    .map_err(|e| (e.to_string(), Vec::new()))?;
            }

            (action, Payload::File { src, dest, mode }) => {
                let template = self.read_text(src)?;
                let rendered = render::render(
                    &Tmpl(template),
                    &Context {
                        facts: self.facts,
                        vars: &self.cfg.vars,
                    },
                )
                .map_err(|e| (e.to_string(), Vec::new()))?;

                self.refuse_symlink(dest)?;
                if self
                    .host
                    .symlink_meta(dest)
                    .map_err(|e| (e.to_string(), Vec::new()))?
                    .is_some()
                    && *action == Action::Adopt
                {
                    // `with_extension` REPLACES the extension, so
                    // `init.lua` backed up to `init.bedouin-bak` and two
                    // managed files sharing a stem collided on one backup.
                    let backup = PathBuf::from(format!("{}.bedouin-bak", dest.display()));
                    // Never overwrite a backup that already exists: on a
                    // re-adopt the "existing" content is bedouin's own
                    // render, and saving that over the real backup destroys
                    // the only copy of the user's file.
                    let already = self
                        .host
                        .symlink_meta(&backup)
                        .map_err(|e| (e.to_string(), Vec::new()))?
                        .is_some();
                    if !already {
                        let existing = self.read_text(dest)?;
                        self.write_text(&backup, &existing, *mode)?;
                    }
                    rec.backup = Some(backup.display().to_string());
                }
                self.write_text(dest, &rendered, *mode)?;
                rec.owned_files = vec![dest.display().to_string()];
                rec.hash = Some(writers::digest(&rendered));
                // Kept so M4's three-way absorb has an original to compare
                // against; unreconstructible later.
                rec.render_snapshot = Some(rendered);
                rec.mode = Some(format!("{mode:o}"));
            }

            (
                _,
                Payload::RcBlock {
                    file,
                    marker,
                    content,
                },
            ) => {
                // ALWAYS read what is there. Writing a drop-in from an empty
                // base truncated whatever the path pointed at: an rc block
                // aimed at the user's own ~/.zshrc replaced it with a single
                // bedouin block and no backup, and two packages sharing one
                // drop-in file silently lost the first one's block. §9 owns the
                // *block*, never the file.
                self.refuse_symlink(file)?;
                let existing = self.read_text(file)?;
                let u = writers::upsert_block(&existing, marker, content)
                    .map_err(|e| (e.to_string(), Vec::new()))?;
                self.write_text(file, &u.text, 0o644)?;
                rec.rc_blocks = vec![state::RcRecord {
                    file: file.display().to_string(),
                    marker: marker.clone(),
                    hash: writers::block_digest(content),
                    superseded: u.superseded,
                }];
            }

            (_, Payload::Completions { argv, dest }) => {
                let mut cmd = Cmd::new(argv.iter().cloned());
                cmd.env = step_env(&self.state, self.facts);
                // Capture stdout rather than streaming it: it IS the file.
                let mut captured = String::new();
                let mut stderr_tail: Vec<String> = Vec::new();
                let status = self
                    .host
                    .run(&cmd, &mut |l| match l {
                        Line::Out(s) => {
                            captured.push_str(&s);
                            captured.push('\n');
                        }
                        Line::Err(s) => stderr_tail.push(s),
                    })
                    .map_err(|e| (e.to_string(), Vec::new()))?;
                if !status.ok() {
                    return Err((
                        format!(
                            "`{}` exited {} while generating completions",
                            cmd.display(),
                            status.code
                        ),
                        stderr_tail,
                    ));
                }
                if captured.trim().is_empty() {
                    return Err((
                        format!("`{}` produced no completions", cmd.display()),
                        stderr_tail,
                    ));
                }
                // Tool output, not managed text: no sentinels, and no UTF-8
                // refusal on the way in -- but the file is Bedouin's, so
                // removal deletes it.
                self.refuse_symlink(dest)?;
                self.write_text(dest, &captured, 0o644)?;
                rec.owned_files = vec![dest.display().to_string()];
                rec.hash = Some(writers::digest(&captured));
                // The command is what the diff is addressed on (§16.2).
                rec.method = Some(writers::digest(&argv.join(" ")));
            }

            (_, Payload::PathFile { file, entries }) => {
                self.refuse_symlink(file)?;
                let text = writers::path_file(entries, self.cfg.shell);
                self.write_text(file, &text, 0o644)?;
                rec.path = entries.clone();
                rec.owned_files = vec![file.display().to_string()];
                rec.hash = Some(writers::digest(&text));
            }

            (_, Payload::None) => {}
        }
        Ok(rec)
    }
}

/// Execute a plan.
pub fn apply(
    plan: &Plan,
    cfg: &Config,
    facts: &Facts,
    state: State,
    host: &dyn Host,
    out: &mut dyn FnMut(Line),
) -> Result<Report> {
    let changes: Vec<&Item> = plan.changes().collect();
    if changes.is_empty() {
        return Ok(Report::default());
    }

    // Refuse to start rather than fail partway: a run that dies at step
    // nineteen of twenty because it cannot escalate is worse than one that
    // never began.
    let root_steps: Vec<&str> = changes
        .iter()
        .filter(|i| i.needs_root)
        .map(|i| i.id.as_str())
        .collect();
    if !root_steps.is_empty() && facts.privilege == Privilege::Unavailable {
        return Err(ConfigError::new(format!(
            "{} steps need root and this machine offers no way to escalate:\n{}\n  \
             Run as root, add the user to a sudo group, or drop those items from \
             the config",
            root_steps.len(),
            root_steps
                .iter()
                .map(|s| format!("  {s}"))
                .collect::<Vec<_>>()
                .join("\n")
        )));
    }

    // The state lock and the sudo keepalive are real-process concerns and live
    // in the CLI, not here: they touch the actual filesystem and spawn actual
    // processes, neither of which a `FakeHost` run should do.
    if !root_steps.is_empty() && facts.privilege == Privilege::Password {
        // Prompt once, before the work, naming what needs it -- not halfway
        // through, after twenty minutes of compiling.
        out(Line::Out(format!(
            "{} steps need root; validating sudo once up front.",
            root_steps.len()
        )));
        let mut cmd = Cmd::new(["sudo", "-v"]);
        cmd.env = step_env(&state, facts);
        let status = host
            .run(&cmd, out)
            .map_err(|e| ConfigError::new(e.to_string()))?;
        if !status.ok() {
            return Err(ConfigError::new(
                "sudo could not be validated, so the privileged steps would fail \
                 partway through. Nothing has been changed",
            ));
        }
    }

    let state_path = state::default_path(&facts.home);
    let mut ex = Executor {
        refreshed: std::collections::BTreeSet::new(),
        host,
        facts,
        cfg,
        state,
        state_path,
        out,
    };

    let mut report = Report::default();
    for (i, item) in changes.iter().enumerate() {
        // Intent first: if the run dies here, the record says so.
        if item.action != Action::Remove {
            // Flip the status on whatever is already recorded. Replacing the
            // record wholesale discarded `method`, `backup`, `owned_files` and
            // `rc_blocks`, so a step that then failed left bedouin amnesiac
            // about a package it had installed -- permanently unowned, and its
            // backup unreachable.
            let pending = ex
                .state
                .items
                .entry(item.id.clone())
                .or_insert_with(|| StateItem::new(item.kind, Owner::Bedouin));
            pending.status = Status::Incomplete;
            pending.owner = Owner::Bedouin;
            ex.flush()?;
        }

        match ex.step(item) {
            Ok(rec) => {
                if item.action == Action::Remove {
                    ex.state.items.remove(&item.id);
                } else {
                    // Carry forward the backup an earlier adopt recorded: a
                    // later rewrite does not take a new one, and losing the
                    // pointer strands the user's original file.
                    let mut rec = rec;
                    if rec.backup.is_none() {
                        rec.backup = ex.state.items.get(&item.id).and_then(|p| p.backup.clone());
                    }
                    ex.state.items.insert(item.id.clone(), rec);
                }
                ex.flush()?;
                report.completed.push(item.id.clone());
            }
            Err((message, output_tail)) => {
                ex.flush()?;
                report.failure = Some(Failure {
                    id: item.id.clone(),
                    message,
                    output_tail,
                });
                report.not_attempted = changes[i + 1..].iter().map(|x| x.id.clone()).collect();
                break;
            }
        }
    }

    // Anything already on the machine that Bedouin did not install is adopted
    // rather than claimed: it must survive being dropped from the config.
    for item in &plan.items {
        if item.action == Action::NoOp && !ex.state.items.contains_key(&item.id) {
            let mut rec = StateItem::new(item.kind, Owner::Preexisting);
            // Adopt its bin directories as well as its existence: a toolchain
            // that was already here must still reach later steps' PATH without
            // re-probing the machine every run.
            rec.bin_dirs = match &item.payload {
                Payload::Language { bin_dirs, .. } => {
                    bin_dirs.iter().map(|p| p.display().to_string()).collect()
                }
                Payload::Manager(m) => crate::recipe::bin_dirs(m.as_str(), facts)
                    .iter()
                    .map(|p| p.display().to_string())
                    .collect(),
                _ => Vec::new(),
            };
            ex.state.items.insert(item.id.clone(), rec);
        }
    }
    ex.flush()?;

    Ok(report)
}
