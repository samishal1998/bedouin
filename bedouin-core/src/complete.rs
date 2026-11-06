//! Completions a tool can print for itself, found without being told.
//!
//! Most CLIs worth completing ship a generator -- `gh completion`, `rustup
//! completions`, `delta --generate-completion` -- and every one of them used to
//! need a `completions:` line before bedouin would run it. Detection asks the
//! tool instead: its `--help` names the subcommand or flag, and then the forms
//! tools actually use are tried until one prints a script for the shell being
//! configured.
//!
//! Exit status proves nothing here. `gh completion zsh` exits 0 and prints a
//! *bash* script -- gh wants `--shell zsh` and quietly ignores the word -- so
//! every answer is checked for the shape that shell loads.
//!
//! The answer is kept in a lock beside the state file, keyed on the binary
//! itself, so the question is asked once per binary rather than once per run.

use crate::host::Host;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

pub const LOCK_NAME: &str = "completions.lock";

/// What was learned about one package's binary.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Entry {
    /// The shell the answer is for: a generator that works for zsh may not
    /// for fish.
    pub shell: String,
    /// [`Host::fingerprint`] of the binary when it was asked. A different one
    /// is a different binary -- usually an upgrade, which may have added a
    /// generator or changed its output -- so the question is asked again.
    pub binary: String,
    /// The tool's own `--version`, for a person reading the lock. The
    /// fingerprint is what decides; this is what explains.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    /// The command that prints its completions, or `None` when it has none.
    /// A "no" is kept as carefully as a "yes": it is what stops the question
    /// being asked on every run. Only a real "no", though -- a probe that
    /// timed out never gets here.
    pub generate: Option<Vec<String>>,
    /// The same form, with another shell's name, printed that shell's script.
    /// Only then is the shell really an argument, and only then can
    /// `--adopt` write it as `{{ shell.name }}`: gh ignores a bare
    /// `completion bash` and prints bash whatever it is asked, which on this
    /// machine is right and in a shared config would be wrong everywhere else.
    #[serde(default)]
    pub portable: bool,
}

/// Package name -> what its binary answered.
pub type Lock = BTreeMap<String, Entry>;

pub fn lock_path(home: &Path) -> PathBuf {
    crate::state::default_path(home).with_file_name(LOCK_NAME)
}

/// The lock, or an empty one. It is a cache: missing or unreadable only means
/// the questions get asked again, which is never wrong, only slower.
pub fn load(host: &dyn Host, home: &Path) -> Lock {
    host.read(&lock_path(home))
        .ok()
        .flatten()
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or_default()
}

pub fn save(host: &dyn Host, home: &Path, lock: &Lock) -> Result<(), String> {
    let json = serde_json::to_string_pretty(lock).map_err(|e| e.to_string())?;
    host.write(&lock_path(home), json.as_bytes(), 0o644)
        .map_err(|e| e.to_string())
}

/// Subcommands that print completions, as tools spell them.
const SUBCOMMANDS: &[&str] = &[
    "completion",
    "completions",
    "autocomplete",
    "auto-complete",
    "autocompletion",
    "autocompletions",
    "auto-completion",
    "auto-completions",
    "gen-completion",
    "gen-completions",
    "generate-completion",
    "generate-completions",
    "generate-shell-completion",
    "generate-shell-completions",
    "shell-completion",
    "shell-completions",
];

/// The same, as a top-level flag.
const FLAGS: &[&str] = &[
    "--completion",
    "--completions",
    "--generate-completion",
    "--generate-completions",
    "--generate-shell-completion",
    "--shell-completion",
    "--shell-completions",
];

/// What a `--help` says the tool has: each a prefix of the command to try.
///
/// A subcommand counts only where a help page lists subcommands: a name, then
/// a gap or a colon, then its description (`  completion   Generate ...`, gh's
/// `  completion:  ...`), or inside a bare comma list (npm's). Prose does not
/// look like that, and prose wraps -- node's help has a line that is just the
/// word `completion` -- and a tool run with `completion` as an argument it
/// never offered may take it as a file to run or a pattern to search.
pub fn advertised(help: &str) -> Vec<Vec<String>> {
    let mut out: Vec<Vec<String>> = Vec::new();
    let mut push = |v: Vec<String>| {
        if !out.contains(&v) {
            out.push(v);
        }
    };
    for line in help.lines() {
        let words: Vec<&str> = line.split_whitespace().collect();
        let Some(first) = words.first() else { continue };
        let bare = |w: &str| w.trim_end_matches([':', ',']).to_string();
        // `access, adduser, audit, ...`: every word but the last ends in a
        // comma, so the whole line is a list of names.
        let list = words.len() > 1 && words[..words.len() - 1].iter().all(|w| w.ends_with(','));
        let names: Vec<String> = if list {
            words.iter().map(|w| bare(w)).collect()
        } else if listed(line) {
            vec![bare(first)]
        } else {
            Vec::new()
        };
        for n in names {
            if SUBCOMMANDS.contains(&n.as_str()) {
                push(vec![n]);
            }
        }
        // Flags appear mid-line (`-c, --completions <SHELL>`), and a flag name
        // is not a word prose uses, so they are matched anywhere.
        for w in &words {
            let flag = w
                .trim_end_matches([',', ':'])
                .split(['=', '<', '['])
                .next()
                .unwrap_or("");
            if FLAGS.contains(&flag) {
                push(vec![flag.to_string()]);
            }
        }
    }
    out
}

/// `name: description` or `name  description`: how help pages list commands.
fn listed(line: &str) -> bool {
    let t = line.trim_start();
    let Some(end) = t.find(char::is_whitespace) else {
        return false;
    };
    let (name, rest) = t.split_at(end);
    if rest.trim().is_empty() {
        return false;
    }
    name.ends_with(':') || rest.starts_with("  ") || rest.starts_with('\t')
}

/// The shell a found form is checked against as well, to know whether the
/// shell is really an argument to it.
pub fn other_shell(shell: &str) -> &'static str {
    if shell == "zsh" {
        "bash"
    } else {
        "zsh"
    }
}

/// Whether the binary on PATH is one program standing in for many -- a mise
/// shim, a rustup proxy -- by its real name differing from the one it was
/// called by. Its fingerprint then stays the same when the tool behind it is
/// upgraded, so the fingerprint alone cannot say the answer is still good.
pub fn shim(fingerprint: &str, bin: &str) -> bool {
    // `<real path> <size> <mtime>`: the path may hold spaces, the rest cannot.
    let real = fingerprint.rsplitn(3, ' ').last().unwrap_or(fingerprint);
    Path::new(real)
        .file_name()
        .is_some_and(|n| n.to_string_lossy() != bin)
}

/// The ways tools take the shell, in the order worth trying. Positional first,
/// because it is the most common and the least likely to mean anything else.
pub fn forms(prefix: &[String], shell: &str) -> Vec<Vec<String>> {
    let with = |rest: &[&str]| -> Vec<String> {
        prefix
            .iter()
            .cloned()
            .chain(rest.iter().map(|s| (*s).to_string()))
            .collect()
    };
    let flag_form = prefix.first().is_some_and(|p| p.starts_with("--"));
    if flag_form {
        let last = prefix.last().cloned().unwrap_or_default();
        let mut eq = prefix.to_vec();
        if let Some(l) = eq.last_mut() {
            *l = format!("{last}={shell}");
        }
        vec![with(&[shell]), eq]
    } else {
        vec![
            with(&[shell]),
            with(&["--shell", shell]),
            with(&["-s", shell]),
            with(&[&format!("--{shell}")]),
        ]
    }
}

/// Whether `text` is a completion script `shell` can load the way bedouin
/// installs it.
///
/// zsh reads a file on `fpath` as the body of an autoloaded function, and
/// compinit looks for `#compdef` on its FIRST line only -- pip's script opens
/// with a comment and would sit on fpath unread. A script written to be
/// *sourced* (npm's, which calls `compdef` at the top level) would do nothing
/// from there either, so neither counts. bash sources what it is given,
/// so a `complete -F`/`-C` registration is the mark. fish's is `complete -c`.
pub fn looks_like(shell: &str, text: &str) -> bool {
    let mut lines = text.lines().map(str::trim).filter(|l| !l.is_empty());
    match shell {
        "zsh" => text
            .lines()
            .next()
            .is_some_and(|l| l.starts_with("#compdef")),
        "bash" => {
            lines.any(|l| l.starts_with("complete ") && (l.contains(" -F ") || l.contains(" -C ")))
        }
        "fish" => {
            !text.contains("COMPREPLY")
                && lines.any(|l| {
                    l.starts_with("complete ") && (l.contains(" -c ") || l.contains("--command"))
                })
        }
        _ => false,
    }
}

/// `argv` as config, with the shell made a template, so one line serves zsh
/// on this machine and fish on the next.
pub fn template(argv: &[String], shell: &str) -> Vec<String> {
    const T: &str = "{{ shell.name }}";
    argv.iter()
        .map(|a| {
            if a == shell {
                T.to_string()
            } else if *a == format!("--{shell}") {
                format!("--{T}")
            } else if let Some(head) = a.strip_suffix(&format!("={shell}")) {
                format!("{head}={T}")
            } else {
                a.clone()
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(v: &[&str]) -> Vec<String> {
        v.iter().map(|x| (*x).to_string()).collect()
    }

    #[test]
    fn help_pages_as_real_tools_write_them() {
        // Each line is copied from the tool's own --help.
        assert_eq!(
            advertised("  completion:  Generate shell completion scripts"),
            [s(&["completion"])],
            "gh, with its colon"
        );
        assert_eq!(
            advertised("  completions  Generate tab-completion scripts for your shell"),
            [s(&["completions"])],
            "rustup"
        );
        assert_eq!(
            advertised(
                "  completion    Generate shell completions [aliases: complete, completions]"
            ),
            [s(&["completion"])],
            "mise: the aliases are not tried as well"
        );
        assert_eq!(
            advertised(
                "    access, adduser, audit, bugs, cache, ci,\n    completion, config, dedupe,"
            ),
            [s(&["completion"])],
            "npm's comma list"
        );
        assert_eq!(
            advertised("      --generate-completion <GENERATE_COMPLETION>\n          Print completion file"),
            [s(&["--generate-completion"])],
            "delta's flag"
        );
        assert_eq!(
            advertised("  -c, --completions <SHELL>  Print shell completion script"),
            [s(&["--completions"])],
            "a flag behind its short form"
        );
    }

    #[test]
    fn prose_about_completion_is_not_a_command() {
        // ripgrep's help says "complete" and "completely" a dozen times, and
        // `rg completion zsh` is a search, not a question.
        let rg =
            "        performed. One can also specify none which will then completely disable\n\
                  verbose help output has complete documentation, where as the condensed\n\
                  Shell completion is available.";
        assert!(advertised(rg).is_empty(), "{:?}", advertised(rg));
        // node's is bash-only and spelled differently; not one of ours.
        assert!(
            advertised("  --completion-bash           print source-able bash completion")
                .is_empty()
        );
        // node's help also wraps a description onto a line of its own, and
        // `node completion` would run ./completion.js.
        assert!(advertised(
            "  --heapsnapshot-signal=...   Generate heap snapshot on\n                              completion"
        )
        .is_empty());
        assert!(
            advertised("  completion of the build is reported").is_empty(),
            "prose, single-spaced"
        );
    }

    #[test]
    fn forms_cover_how_tools_take_the_shell() {
        assert_eq!(
            forms(&s(&["completion"]), "zsh"),
            [
                s(&["completion", "zsh"]),
                s(&["completion", "--shell", "zsh"]),
                s(&["completion", "-s", "zsh"]),
                s(&["completion", "--zsh"]),
            ]
        );
        assert_eq!(
            forms(&s(&["--completions"]), "fish"),
            [s(&["--completions", "fish"]), s(&["--completions=fish"])]
        );
    }

    #[test]
    fn output_is_judged_by_the_shell_that_will_load_it() {
        let cobra_bash = "# bash completion V2 for gh\n__gh_debug() {\n}\ncomplete -o default -F __start_gh gh\n";
        let cobra_zsh = "#compdef gh\ncompdef _gh gh\n_gh() {\n}\n";
        let clap_fish = "# Print an optspec for argparse\ncomplete -c rustup -n \"__fish_use_subcommand\" -s v\n";
        let npm = "###-begin-npm-completion-###\nif type complete &>/dev/null; then\n  complete -o default -F _npm_completion npm\nelif type compdef &>/dev/null; then\n  compdef _npm_completion npm\nfi\n";

        // The gh trap: asked for zsh, answered in bash.
        assert!(!looks_like("zsh", cobra_bash));
        assert!(looks_like("zsh", cobra_zsh));
        assert!(looks_like("bash", cobra_bash));
        assert!(!looks_like("bash", cobra_zsh));
        assert!(looks_like("fish", clap_fish));
        assert!(!looks_like("fish", cobra_bash));
        // npm's works only sourced: fine for bash, nothing on zsh's fpath.
        assert!(looks_like("bash", npm));
        assert!(!looks_like("zsh", npm));
        assert!(!looks_like("fish", npm));
        // compinit reads line one; a comment above #compdef hides it (pip).
        assert!(!looks_like(
            "zsh",
            "# pip zsh completion start\n#compdef -P pip\n"
        ));
        assert!(!looks_like("zsh", "\n#compdef gh\n"));
        // Help text, an error, or nothing.
        assert!(!looks_like("zsh", ""));
        assert!(!looks_like("zsh", "Usage: gh completion -s <shell>"));
    }

    #[test]
    fn the_shell_becomes_a_template_and_nothing_else_does() {
        let t = |v: &[&str]| template(&s(v), "zsh");
        assert_eq!(
            t(&["gh", "completion", "--shell", "zsh"]),
            s(&["gh", "completion", "--shell", "{{ shell.name }}"])
        );
        assert_eq!(
            t(&["pip", "completion", "--zsh"]),
            s(&["pip", "completion", "--{{ shell.name }}"])
        );
        assert_eq!(
            t(&["just", "--completions=zsh"]),
            s(&["just", "--completions={{ shell.name }}"])
        );
        // A word that merely contains the shell's name is left alone.
        assert_eq!(
            t(&["zshtool", "completion", "zsh"]),
            s(&["zshtool", "completion", "{{ shell.name }}"])
        );
    }

    #[test]
    fn a_shim_is_known_by_its_real_name() {
        assert!(
            shim("/home/t/.local/bin/mise 41000 1.0", "node"),
            "a mise shim"
        );
        assert!(
            shim("/home/t/.cargo/bin/rustup 9 1.0", "cargo"),
            "a rustup proxy"
        );
        assert!(!shim("/usr/bin/gh 45 1.0", "gh"));
        assert!(!shim("/opt/my tools/gh 45 1.0", "gh"), "spaces in the path");
        assert!(!shim("/usr/bin/gh", "gh"), "a test host's bare path");
    }

    #[test]
    fn a_missing_or_corrupt_lock_is_an_empty_one() {
        let h = crate::host::FakeHost::new();
        assert!(load(&h, Path::new("/home/t")).is_empty());
        let h = h.with_file("/home/t/.local/state/bedouin/completions.lock", "{not json");
        assert!(load(&h, Path::new("/home/t")).is_empty());
    }
}
