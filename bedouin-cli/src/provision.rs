//! Putting bedouin on a machine that does not have it yet.
//!
//! Two roads to the same place. `bedouin ssh` walks there interactively: over
//! your ssh connection, with agent forwarding, so the machine borrows your
//! keys for the first clone and stores none of them. `bedouin cloudinit`
//! writes the walk down for a machine that boots unattended: a user-data file
//! carrying a read-only deploy key, age-encrypted at rest because that file
//! otherwise sits in a shell history or a git repo with a private key inside.
//!
//! Neither invents a second bootstrap. Both run the same install.sh every
//! README instructs a person to run, then `git clone`, then bedouin itself.

use bedouin_core::host::{Host, OsHost};
use bedouin_core::run;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode, Stdio};

const INSTALL_URL: &str = "https://samishal1998.github.io/bedouin/install.sh";

/// The config repository this machine's own config came from -- the default
/// for both roads, because "put my setup on that box" almost always means
/// this setup.
fn local_repo_url(host: &OsHost, config: Option<&Path>, cwd: &Path) -> Result<String, String> {
    let (loaded, _) = run::load_only(host, config, cwd).map_err(|e| e.to_string())?;
    let out = Command::new("git")
        .arg("-C")
        .arg(&loaded.root)
        .args(["remote", "get-url", "origin"])
        .output()
        .map_err(|e| format!("git: {e}"))?;
    if !out.status.success() {
        return Err(format!(
            "{} has no `origin` remote, so there is no repository to hand the \
             machine.\n  Pass one with --repo",
            loaded.root.display()
        ));
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

/// Get git and curl onto a box that may have neither. Best effort across the
/// package managers bedouin already speaks; a box with none of them gets a
/// sentence, not a guess.
const BOOTSTRAP_TOOLS: &str = r#"
set -e
# Only what is actually missing. RHEL 9 (Rocky, Alma) ships `curl-minimal`,
# which provides curl(1) but conflicts with the full `curl` package -- so
# asking for `git curl` there fails the whole transaction over the half we
# already have.
need=""
command -v git >/dev/null 2>&1 || need="$need git"
command -v curl >/dev/null 2>&1 || need="$need curl"
# `subdir:` repos export through tar, and Leap's base image has none.
command -v tar >/dev/null 2>&1 || need="$need tar"
if [ -n "$need" ]; then
  # A freshly provisioned machine is usually root, and a minimal image has no
  # sudo to speak of -- so sudo only when we are not root.
  SUDO=""; [ "$(id -u)" = 0 ] || SUDO="sudo"
  if command -v apt-get >/dev/null 2>&1; then $SUDO apt-get update -qq && $SUDO apt-get install -y -qq $need
  elif command -v dnf >/dev/null 2>&1; then $SUDO dnf install -y -q $need
  elif command -v pacman >/dev/null 2>&1; then $SUDO pacman -Sy --noconfirm --quiet $need
  elif command -v zypper >/dev/null 2>&1; then $SUDO zypper --quiet install -y $need
  else echo "bedouin: this machine has no package manager I recognise; install git and curl first" >&2; exit 1
  fi
fi
"#;

/// `bedouin ssh user@host` -- install, clone, apply, over one forwarded
/// agent. Nothing credential-shaped lands on the machine.
pub fn ssh(
    host: &OsHost,
    config: Option<&Path>,
    cwd: &Path,
    target: &str,
    repo: Option<String>,
    yes: bool,
    ssh_args: &[String],
) -> ExitCode {
    let repo = match repo
        .map(Ok)
        .unwrap_or_else(|| local_repo_url(host, config, cwd))
    {
        Ok(r) => r,
        Err(e) => {
            eprintln!("bedouin: {e}");
            return ExitCode::FAILURE;
        }
    };

    // The promise on the next line is agent forwarding, and an agent only
    // answers ssh remotes. GitHub https rewrites cleanly; anything else https
    // gets a warning instead of a clone that hangs asking for a password.
    let repo = match to_ssh_remote(&repo) {
        Some(r) => r,
        None => {
            println!("  note    {repo} is not an ssh remote; the clone will need");
            println!("          its own credentials on the machine");
            repo
        }
    };
    println!("  target  {target}");
    println!("  repo    {repo}");
    println!("  keys    forwarded for the clone, stored nowhere\n");
    if !yes && !crate::release::confirm("Provision it?") {
        println!("Nothing done.");
        return ExitCode::SUCCESS;
    }

    // Four stages, each its own ssh: the failure names the stage, and a
    // person watching sees the walk rather than one long silence.
    let stages: [(&str, String); 4] = [
        ("tools", BOOTSTRAP_TOOLS.to_string()),
        (
            "install bedouin",
            format!(
                "command -v bedouin >/dev/null 2>&1 || [ -x \"$HOME/.local/bin/bedouin\" ] || \
                 curl -fsSL {INSTALL_URL} | sh"
            ),
        ),
        (
            "clone config",
            format!(
                "[ -e \"$HOME/.config/bedouin/bedouin.yaml\" ] || \
                 GIT_TERMINAL_PROMPT=0 git clone {} \"$HOME/.config/bedouin\"",
                sh_quote(&repo)
            ),
        ),
        (
            "apply",
            // sync, not apply: on a machine provisioned before, this pulls
            // what changed first, so `bedouin ssh` twice is pull-and-apply
            // rather than apply-the-clone-from-last-time. On a fresh clone
            // the pull is a no-op. PATH: a non-interactive shell has not
            // read the rc files the install just wrote; the tty is for sudo.
            "export PATH=\"$HOME/.local/bin:$PATH\"; bedouin sync -y".to_string(),
        ),
    ];

    for (name, script) in &stages {
        println!(":: {name}");
        // -A: the machine borrows this terminal's agent for exactly this
        // long. -t: sudo and git may need to ask a human something.
        let status = Command::new("ssh")
            .args(["-A", "-t"])
            .args(ssh_args)
            .args([target, script.as_str()])
            .stdin(Stdio::inherit())
            .stdout(Stdio::inherit())
            .stderr(Stdio::inherit())
            .status();
        match status {
            Ok(s) if s.success() => {}
            Ok(s) => {
                eprintln!(
                    "bedouin: stage `{name}` failed on {target} (exit {})",
                    s.code().unwrap_or(-1)
                );
                return ExitCode::FAILURE;
            }
            Err(e) => {
                eprintln!("bedouin: could not run ssh: {e}");
                return ExitCode::FAILURE;
            }
        }
    }
    println!("\nProvisioned. `bedouin ssh {target}` again is a re-apply; `bedouin sync` on the machine pulls.");
    ExitCode::SUCCESS
}

/// `bedouin cloudinit` -- the same walk, written down for a machine that
/// boots unattended, with a read-only deploy key as its only credential.
pub fn cloudinit(
    host: &OsHost,
    config: Option<&Path>,
    cwd: &Path,
    repo: Option<String>,
    out: &Path,
    plain: bool,
) -> ExitCode {
    let repo = match repo
        .map(Ok)
        .unwrap_or_else(|| local_repo_url(host, config, cwd))
    {
        Ok(r) => r,
        Err(e) => {
            eprintln!("bedouin: {e}");
            return ExitCode::FAILURE;
        }
    };
    // A deploy key authenticates over ssh; an https remote would sit there
    // asking a question nobody is present to answer.
    let repo = match to_ssh_remote(&repo) {
        Some(r) => r,
        None => {
            eprintln!(
                "bedouin: {repo} is not something a deploy key can clone.\n  \
                 Use an ssh remote (git@github.com:you/config.git)"
            );
            return ExitCode::FAILURE;
        }
    };

    if !plain && Host::which(host, "age", &path_dirs()).is_none() {
        eprintln!(
            "bedouin: `age` is not installed, and this file will contain a private key.\n  \
             Install age, or pass --plain to write it unencrypted -- and then \
             treat the file like the key it is"
        );
        return ExitCode::FAILURE;
    }

    // The machine's identity, made here and never used here: the private half
    // goes only into the user-data, the public half goes to the forge as a
    // read-only deploy key.
    let keydir = std::env::temp_dir().join(format!("bedouin-cloudinit-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&keydir);
    let mut mk = std::fs::DirBuilder::new();
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        mk.mode(0o700);
    }
    // create, not create_all: if something already sits at this exact path
    // after the cleanup above, that is a race worth refusing, not joining.
    if mk.create(&keydir).is_err() {
        eprintln!("bedouin: cannot create {}", keydir.display());
        return ExitCode::FAILURE;
    }
    let keyfile = keydir.join("deploy_key");
    let gen = Command::new("ssh-keygen")
        .args([
            "-t",
            "ed25519",
            "-N",
            "",
            "-C",
            "bedouin-deploy",
            "-q",
            "-f",
        ])
        .arg(&keyfile)
        .status();
    if !matches!(gen, Ok(s) if s.success()) {
        eprintln!("bedouin: ssh-keygen failed");
        return ExitCode::FAILURE;
    }
    let private = std::fs::read_to_string(&keyfile).unwrap_or_default();
    let public = std::fs::read_to_string(keyfile.with_extension("pub"))
        .unwrap_or_default()
        .trim()
        .to_string();
    let _ = std::fs::remove_dir_all(&keydir);

    let user_data = render_user_data(&repo, &private);

    let written = if plain {
        write_private(out, user_data.as_bytes())
    } else {
        encrypt_with_age(&user_data, out)
    };
    if let Err(e) = written {
        eprintln!("bedouin: {e}");
        return ExitCode::FAILURE;
    }

    println!("Wrote {}.", out.display());
    println!("\nAdd this as a READ-ONLY deploy key on {repo}:");
    println!("\n  {public}\n");
    if plain {
        println!(
            "WARNING: {} holds a PRIVATE KEY in plain text.",
            out.display()
        );
        println!("Do not commit it. Delete it once the machine is up.");
    } else {
        println!("The file is age-encrypted; decrypt it on the way in, e.g.");
        println!(
            "  age -d {} | <your cloud's create command> --user-data -",
            out.display()
        );
    }
    println!("\nThe machine will: install bedouin, clone the config, apply.");
    ExitCode::SUCCESS
}

fn render_user_data(repo: &str, private_key: &str) -> String {
    use std::fmt::Write;
    let mut s = String::from("#cloud-config\n");
    s.push_str("# Written by `bedouin cloudinit`. The write_files entry is a read-only\n");
    s.push_str("# deploy key for the config repository and nothing else.\n");
    s.push_str("packages:\n  - git\n  - curl\n  - tar\n");
    s.push_str("write_files:\n");
    s.push_str("  - path: /root/.ssh/bedouin_deploy\n");
    s.push_str("    permissions: \"0600\"\n");
    s.push_str("    owner: root:root\n");
    s.push_str("    content: |\n");
    for line in private_key.lines() {
        let _ = writeln!(s, "      {line}");
    }
    s.push_str("runcmd:\n");
    let _ = writeln!(s, "  - curl -fsSL {INSTALL_URL} | HOME=/root sh");
    let _ = writeln!(
        s,
        "  - GIT_SSH_COMMAND='ssh -i /root/.ssh/bedouin_deploy -o StrictHostKeyChecking=accept-new' \
         git clone {} /root/.config/bedouin",
        sh_quote(repo)
    );
    // Remembered by the clone itself, so `bedouin sync` next month can still
    // pull -- a deploy key that worked exactly once turns the config into a
    // snapshot nobody meant to take.
    s.push_str(
        "  - git -C /root/.config/bedouin config core.sshCommand \
         'ssh -i /root/.ssh/bedouin_deploy -o StrictHostKeyChecking=accept-new'\n",
    );
    s.push_str("  - HOME=/root PATH=/root/.local/bin:$PATH bedouin apply -y\n");
    s
}

/// `https://github.com/o/r(.git)` -> `git@github.com:o/r.git`; ssh remotes
/// pass through. Anything else is not a deploy-key clone.
fn to_ssh_remote(url: &str) -> Option<String> {
    if url.starts_with("git@") || url.starts_with("ssh://") {
        return Some(url.to_string());
    }
    let rest = url.strip_prefix("https://github.com/")?;
    let rest = rest.trim_end_matches('/').trim_end_matches(".git");
    Some(format!("git@github.com:{rest}.git"))
}

/// Written 0600 from the first byte: this file holds a private key, and a
/// default-umask write leaves it world-readable for as long as it exists.
fn write_private(out: &Path, data: &[u8]) -> Result<(), String> {
    use std::io::Write;
    let mut f = std::fs::OpenOptions::new();
    f.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        f.mode(0o600);
    }
    f.open(out)
        .and_then(|mut f| f.write_all(data))
        .map_err(|e| format!("{}: {e}", out.display()))
}

fn encrypt_with_age(data: &str, out: &Path) -> Result<(), String> {
    use std::io::Write;
    // -p: a passphrase, asked on the tty by age itself. The person is present
    // at generation time; that is the whole reason this can be encrypted at
    // all when the boot cannot be.
    let mut child = Command::new("age")
        .arg("-p")
        .arg("-o")
        .arg(out)
        .stdin(Stdio::piped())
        .spawn()
        .map_err(|e| format!("age: {e}"))?;
    child
        .stdin
        .take()
        .ok_or("age: no stdin")?
        .write_all(data.as_bytes())
        .map_err(|e| format!("age: {e}"))?;
    let status = child.wait().map_err(|e| format!("age: {e}"))?;
    if status.success() {
        Ok(())
    } else {
        Err("age did not encrypt the file".into())
    }
}

fn path_dirs() -> Vec<PathBuf> {
    std::env::var("PATH")
        .map(|p| std::env::split_paths(&p).collect())
        .unwrap_or_default()
}

fn sh_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', r"'\''"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_remotes_a_deploy_key_can_use_survive() {
        assert_eq!(
            to_ssh_remote("https://github.com/o/config").as_deref(),
            Some("git@github.com:o/config.git")
        );
        assert_eq!(
            to_ssh_remote("https://github.com/o/config.git").as_deref(),
            Some("git@github.com:o/config.git")
        );
        assert_eq!(
            to_ssh_remote("git@github.com:o/config.git").as_deref(),
            Some("git@github.com:o/config.git")
        );
        assert_eq!(
            to_ssh_remote("ssh://git@gitlab.com/o/r.git").as_deref(),
            Some("ssh://git@gitlab.com/o/r.git")
        );
        // An https remote to a forge this cannot rewrite is refused rather
        // than emitted as a clone that will hang asking for a password.
        assert_eq!(to_ssh_remote("https://gitlab.com/o/r"), None);
    }

    #[test]
    fn the_user_data_is_valid_yaml_and_carries_no_surprises() {
        let key =
            "-----BEGIN OPENSSH PRIVATE KEY-----\nabc\ndef\n-----END OPENSSH PRIVATE KEY-----\n";
        let ud = render_user_data("git@github.com:o/config.git", key);
        assert!(ud.starts_with("#cloud-config\n"));
        let doc: serde_yaml_ng::Value = serde_yaml_ng::from_str(&ud).expect("parses as YAML");
        assert_eq!(
            doc["write_files"][0]["path"].as_str(),
            Some("/root/.ssh/bedouin_deploy")
        );
        // The key survives the block-scalar indentation byte for byte.
        assert_eq!(
            doc["write_files"][0]["content"].as_str().map(str::trim_end),
            Some(key.trim_end())
        );
        assert_eq!(doc["write_files"][0]["permissions"].as_str(), Some("0600"));
        let cmds = doc["runcmd"].as_sequence().expect("runcmd");
        assert_eq!(
            cmds.len(),
            4,
            "install, clone, remember-the-key, apply -- nothing else"
        );
        assert!(
            cmds[2]
                .as_str()
                .is_some_and(|c| c.contains("core.sshCommand")),
            "the clone must remember its deploy key, or the first sync is the last"
        );
    }
}
