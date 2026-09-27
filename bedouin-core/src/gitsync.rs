//! Keeping the config repository in step with edits bedouin makes to it.
//!
//! `add`, `remove`, `alias`, `absorb` and the web UI edit bedouin.yaml; without
//! this the edit sat uncommitted, and `sync` then refused the dirty tree. On by
//! default. Off is a per-clone setting -- `git config bedouin.autosync false`,
//! which `bedouin sync --auto off` writes -- for batching several edits and
//! running `bedouin sync` once at the end. Stored in git's own config rather
//! than bedouin.yaml so turning it off is not itself an edit to commit.

use crate::host::{Host, Line};
use std::path::Path;

/// What happened after an edit, for the caller to say in one line.
#[derive(Debug, PartialEq, Eq)]
pub enum Synced {
    NotARepo,
    Off,
    Committed,
    Pushed,
    /// Committed, but the push did not go through. The commit is kept.
    PushFailed(String),
    Failed(String),
}

fn run(host: &dyn Host, root: &Path, args: &[&str]) -> Result<String, String> {
    let mut full = vec!["-C".to_string(), root.display().to_string()];
    full.extend(args.iter().map(|s| s.to_string()));
    let cmd = crate::gitcmd::git(host, host.env().clone(), &full);
    let (mut out, mut err) = (String::new(), String::new());
    let status = host
        .run(&cmd, &mut |l| match l {
            Line::Out(s) => {
                out.push_str(&s);
                out.push('\n');
            }
            Line::Err(s) => {
                err.push_str(&s);
                err.push('\n');
            }
            _ => {}
        })
        .map_err(|e| e.to_string())?;
    if status.ok() {
        Ok(out.trim().to_string())
    } else {
        Err(err.trim().to_string())
    }
}

pub fn is_repo(host: &dyn Host, root: &Path) -> bool {
    run(host, root, &["rev-parse", "--is-inside-work-tree"]).is_ok_and(|o| o == "true")
}

/// On unless this clone says otherwise.
pub fn enabled(host: &dyn Host, root: &Path) -> bool {
    run(host, root, &["config", "--get", "bedouin.autosync"]).map_or(true, |v| v != "false")
}

pub fn set_enabled(host: &dyn Host, root: &Path, on: bool) -> Result<(), String> {
    let v = if on { "true" } else { "false" };
    run(host, root, &["config", "bedouin.autosync", v]).map(|_| ())
}

fn has_upstream(host: &dyn Host, root: &Path) -> bool {
    run(host, root, &["rev-parse", "--abbrev-ref", "@{u}"]).is_ok()
}

/// Commit everything changed under the config root. `Ok(false)` when there
/// was nothing to commit. Everything, not just bedouin.yaml: `absorb` writes
/// templates too, and .gitignore is what keeps .env.bedouin out.
pub fn commit_all(host: &dyn Host, root: &Path, msg: &str) -> Result<bool, String> {
    run(host, root, &["add", "-A"])?;
    if run(host, root, &["diff", "--cached", "--quiet"]).is_ok() {
        return Ok(false);
    }
    run(host, root, &["commit", "-q", "-m", msg])?;
    Ok(true)
}

/// Push when there is somewhere to push to. `Ok(false)` without an upstream.
pub fn push(host: &dyn Host, root: &Path) -> Result<bool, String> {
    if !has_upstream(host, root) {
        return Ok(false);
    }
    run(host, root, &["push", "-q"])?;
    Ok(true)
}

/// Pull, putting local commits on top. A conflict is not bedouin's to settle:
/// the rebase is undone and the repository left as it was before the pull.
pub fn pull(host: &dyn Host, root: &Path) -> Result<String, String> {
    if !has_upstream(host, root) {
        return Ok(String::new());
    }
    run(host, root, &["pull", "--rebase", "-q"]).map_err(|e| {
        let _ = run(host, root, &["rebase", "--abort"]);
        format!("{e}\n  The pull was undone; resolve it in the repository by hand")
    })
}

/// After an edit: commit it, and push it when the clone has an upstream.
pub fn after_edit(host: &dyn Host, root: &Path, msg: &str) -> Synced {
    if !is_repo(host, root) {
        return Synced::NotARepo;
    }
    if !enabled(host, root) {
        return Synced::Off;
    }
    match commit_all(host, root, msg) {
        Err(e) => Synced::Failed(e),
        Ok(false) => Synced::Committed,
        Ok(true) => match push(host, root) {
            Ok(true) => Synced::Pushed,
            Ok(false) => Synced::Committed,
            Err(e) => Synced::PushFailed(e),
        },
    }
}
