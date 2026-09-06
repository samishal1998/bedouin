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
