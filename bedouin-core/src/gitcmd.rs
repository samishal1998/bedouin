//! One builder for every git command bedouin runs.
//!
//! Two things every one of them needs, and neither may be forgotten at a call
//! site:
//!
//! `GIT_TERMINAL_PROMPT=0`, always. Without it a private repository makes git
//! sit waiting for a username on a terminal nobody is watching -- ten silent
//! minutes until the command timeout, which in the reconcile daemon is not a
//! failure anyone sees, just a machine that quietly stopped converging.
//!
//! `gh` as a borrowed credential helper, when it is installed. `-c
//! credential.helper=!gh auth git-credential` is appended -- appended, not
//! replacing: clearing the helper list first would turn off a credential
//! store the user set up themselves, and their own configuration is not ours
//! to disable. gh answers for the hosts it is signed into and stays silent
//! for every other, so this is inert everywhere it does not help. SSH remotes
//! never consult credential helpers and are untouched.

use crate::host::{Cmd, Host};
use std::collections::BTreeMap;
use std::path::PathBuf;

/// A git command with bedouin's credential posture applied.
///
/// `args` is everything after `git`. `env` should already carry the PATH the
/// step runs with -- gh is looked for there, so a brew-installed gh works the
/// moment the manager step has run, and its absence costs one PATH scan.
pub fn git(host: &dyn Host, mut env: BTreeMap<String, String>, args: &[String]) -> Cmd {
    env.insert("GIT_TERMINAL_PROMPT".into(), "0".into());

    let path: Vec<PathBuf> = env
        .get("PATH")
        .map(|p| std::env::split_paths(p).collect())
        .unwrap_or_default();
    let mut argv: Vec<String> = vec!["git".into()];
    if host.which("gh", &path).is_some() {
        argv.push("-c".into());
        argv.push("credential.helper=!gh auth git-credential".into());
    }
    argv.extend(args.iter().cloned());

    let mut cmd = Cmd::new(argv);
    cmd.env = env;
    cmd
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::host::FakeHost;

    fn env() -> BTreeMap<String, String> {
        BTreeMap::from([("PATH".into(), "/usr/bin:/opt/tools".into())])
    }

    #[test]
    fn git_never_gets_to_ask_a_question() {
        // The failure this prevents is invisible: a private repo, a daemon,
        // and git waiting for a username until the timeout.
        let cmd = git(&FakeHost::new(), env(), &["clone".into(), "x".into()]);
        assert_eq!(
            cmd.env.get("GIT_TERMINAL_PROMPT").map(String::as_str),
            Some("0")
        );
    }

    #[test]
    fn gh_is_borrowed_when_present_and_not_missed_when_absent() {
        let with = FakeHost::new().with_binary("/opt/tools/gh");
        let cmd = git(&with, env(), &["clone".into(), "u".into(), "d".into()]);
        assert_eq!(
            cmd.argv,
            vec![
                "git",
                "-c",
                "credential.helper=!gh auth git-credential",
                "clone",
                "u",
                "d"
            ],
            "appended after `git`, before the subcommand -- and appended, not \
             replacing, so a helper the user configured still runs first"
        );

        let without = git(&FakeHost::new(), env(), &["pull".into()]);
        assert_eq!(without.argv, vec!["git", "pull"]);
    }
}

/// Where the clone behind a `subdir:` repo lives.
///
/// The destination holds an exported snapshot, so the repository itself needs
/// a home of its own -- one per remote, shared by every subdir taken from it.
pub fn store_dir(home: &std::path::Path, url: &str) -> PathBuf {
    let slug: String = url
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect::<String>()
        .trim_matches('-')
        .to_string();
    // The slug flattens every separator to `-`, so `a/b-c` and `a-b/c` would
    // share a directory and one remote's content would answer for the other.
    // A hash of the URL itself keeps them apart.
    let mut h: u64 = 0xcbf29ce484222325;
    for b in url.bytes() {
        h ^= u64::from(b);
        h = h.wrapping_mul(0x100000001b3);
    }
    home.join(".local/share/bedouin/repos")
        .join(format!("{slug}-{h:08x}"))
}

/// Single-quote a value for the one shell line the export pipe needs.
fn sq(s: &str) -> String {
    format!("'{}'", s.replace('\'', r"'\''"))
}

/// The commands that bring one `subdir:` repo up to date, in two halves:
/// the fetch half (clone the store if absent, fetch the ref) and the export.
///
/// Two halves so the caller can clear `dest` between them -- after the fetch
/// has succeeded, never before it. A snapshot is derived content, so files
/// the source deleted must go; but wiping it ahead of a fetch that then fails
/// on a dead network leaves an empty config directory, which is strictly
/// worse than a stale one.
///
/// The export is a pipe, because `git archive` writes a tar stream and
/// nothing else here touches a shell: the two ends meet in `sh -c`, with
/// every path quoted.
pub fn subdir_export(
    host: &dyn Host,
    env: BTreeMap<String, String>,
    url: &str,
    store: &std::path::Path,
    dest: &std::path::Path,
    reference: Option<&str>,
    subdir: &str,
) -> (Vec<Cmd>, Cmd) {
    let mut cmds = Vec::new();
    let store_s = store.display().to_string();

    let cloned = host
        .symlink_meta(&store.join("HEAD"))
        .ok()
        .flatten()
        .is_some();
    if !cloned {
        // A clone that died halfway leaves a directory with no HEAD, and git
        // refuses to clone into a non-empty directory -- so the store would
        // be bricked by the one interruption. Nothing in it is precious: it
        // is a cache of the remote.
        let _ = host.remove_dir_all(store);
        let mut args = vec![
            "clone".to_string(),
            "--bare".into(),
            "--depth".into(),
            "1".into(),
        ];
        if let Some(r) = reference {
            args.push("--branch".into());
            args.push(r.to_string());
        }
        args.push(url.to_string());
        args.push(store_s.clone());
        cmds.push(git(host, env.clone(), &args));
    }

    // Fetched every time, so `sync` on a moved branch or tag actually moves.
    // `--force` because a re-pointed tag is a thing remotes do.
    cmds.push(git(
        host,
        env.clone(),
        &[
            "-C".into(),
            store_s.clone(),
            "fetch".into(),
            "--depth".into(),
            "1".into(),
            "--force".into(),
            "origin".into(),
            reference.unwrap_or("HEAD").to_string(),
        ],
    ));

    // One normalised spelling of the subdir, used for the depth AND the
    // pathspec: `./nvim` counted `.` as a component, so tar stripped one
    // level too many and silently dropped every file.
    let sub: Vec<&str> = subdir
        .split('/')
        .filter(|c| !c.is_empty() && *c != ".")
        .collect();
    let sub = sub.join("/");
    let depth = sub.split('/').count();

    // Proof the subdir exists at that ref, as the last prep step: it runs
    // BEFORE any caller clears dest, so a typo'd subdir -- or one upstream
    // renamed away -- is a refusal with a sentence, not an emptied config
    // directory that the fetch-first ordering was supposed to prevent.
    //
    // tar is checked in the same breath and at the same moment, for the same
    // reason. openSUSE Leap's base image has no tar, and git does not pull one
    // in, so the export died mid-pipeline with a bare `tar: command not found`
    // and exit 127 -- accurate, but not a sentence that tells you to install
    // tar. Everything this needs to be true is asserted before dest is
    // touched.
    let mut check = Cmd::new([
        "sh".to_string(),
        "-c".into(),
        format!(
            "command -v tar >/dev/null 2>&1 || {{ echo \"subdir: exports with tar(1), which is not installed here\" >&2; exit 1; }}; git -C {} cat-file -e FETCH_HEAD:{} || {{ echo \"no directory {} at that ref -- check subdir: and ref:\" >&2; exit 1; }}",
            sq(&store_s),
            sq(&sub),
            sub
        ),
    ]);
    check.env = env.clone();
    cmds.push(check);

    let mut sh = Cmd::new([
        "sh".to_string(),
        "-c".into(),
        format!(
            "git -C {} archive FETCH_HEAD -- {} | tar -x --strip-components={depth} -C {}",
            sq(&store_s),
            sq(&sub),
            sq(&dest.display().to_string()),
        ),
    ]);
    sh.env = env;
    (cmds, sh)
}

/// A command that fails, with a sentence, if a working tree has uncommitted
/// changes. Run before anything that would delete the tree: what happens to
/// the user's commits is the user's call, and that call is not "silently
/// gone because the config changed a ref".
pub fn dirty_guard(env: BTreeMap<String, String>, dest: &std::path::Path) -> Cmd {
    let d = sq(&dest.display().to_string());
    // Three kinds of work a re-clone would destroy: uncommitted changes,
    // commits no remote has, and stashes. And fail CLOSED: a git that cannot
    // even report status -- a corrupt repo, an ownership refusal -- used to
    // read as clean, which meant "delete it" was the answer to "I don't
    // know what's in it".
    let script = format!(
        "cd {d} || exit 1; \
         st=$(git status --porcelain 2>&1) || {{ echo \"cannot read $(pwd): $st\" >&2; exit 1; }}; \
         [ -z \"$st\" ] || {{ echo \"$(pwd): uncommitted changes -- commit or stash them first\" >&2; exit 1; }}; \
         [ -z \"$(git log --branches --not --remotes -n 1 --format=x 2>/dev/null)\" ] || \
           {{ echo \"$(pwd): has commits no remote has -- push them first\" >&2; exit 1; }}; \
         [ -z \"$(git stash list 2>/dev/null)\" ] || \
           {{ echo \"$(pwd): has stashes -- apply or drop them first\" >&2; exit 1; }}"
    );
    let mut cmd = Cmd::new(["sh".to_string(), "-c".into(), script]);
    cmd.env = env;
    cmd
}

#[cfg(test)]
mod spec_tests {
    use crate::plan::repo_spec;

    #[test]
    fn a_ref_alone_spells_exactly_the_ref_state_already_recorded() {
        // State written before `subdir:` existed recorded `version = ref`.
        // If the spec for that same shape spelled anything else, every
        // existing repo would plan as "the pin moved" and re-clone on the
        // first run after upgrading bedouin.
        assert_eq!(repo_spec(&None, &None), None);
        assert_eq!(repo_spec(&Some("v1.2".into()), &None), Some("v1.2".into()));
        assert_eq!(
            repo_spec(&Some("main".into()), &Some("nvim".into())),
            Some("main #nvim".into())
        );
        assert_eq!(
            repo_spec(&None, &Some("nvim".into())),
            Some("@default #nvim".into())
        );
    }
}
