//! Resolving facts from a real machine, through [`Host`] so it stays testable.
//!
//! Everything here is read-only. That is not in tension with "nothing runs
//! during `plan`": the prohibition is on executing *user-supplied* code from
//! the config, which is unreviewable and, on a fresh box, always falls through
//! to its default. These commands are fixed, auditable, and degrade to "not
//! installed" when the tool is absent.

use crate::facts::{
    distro_like_of, Arch, Distro, DistroLike, Facts, Manager, Os, Privilege, Shell, ShellFacts,
};
use crate::host::{Cmd, Host, Line};
use crate::schema::Result;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

fn capture(host: &dyn Host, argv: &[&str], path: &[PathBuf]) -> Option<String> {
    let mut cmd = Cmd::new(argv.iter().copied());
    cmd.env.insert(
        "PATH".into(),
        path.iter()
            .map(|p| p.display().to_string())
            .collect::<Vec<_>>()
            .join(":"),
    );
    let mut out = String::new();
    let status = host
        .run(&cmd, &mut |l| {
            if let Line::Out(s) = l {
                out.push_str(&s);
                out.push('\n');
            }
        })
        .ok()?;
    status.ok().then(|| out.trim().to_string())
}

/// `KEY=value` and `KEY="value"` from an os-release file.
fn os_release(text: &str) -> BTreeMap<String, String> {
    text.lines()
        .filter_map(|l| l.split_once('='))
        .map(|(k, v)| {
            (
                k.trim().to_string(),
                v.trim().trim_matches('"').to_string(),
            )
        })
        .collect()
}

fn shell_from_path(p: &str) -> Shell {
    Path::new(p)
        .file_name()
        .and_then(|n| Shell::parse(&n.to_string_lossy()))
        .unwrap_or(Shell::Other)
}

/// Which of the managers Bedouin knows are present.
fn managers(host: &dyn Host, path: &[PathBuf]) -> Vec<Manager> {
    Manager::ALL
        .iter()
        .copied()
        .filter(|m| {
            let bin = match m {
                Manager::Apt => "apt-get",
                other => other.as_str(),
            };
            host.which(bin, path).is_some()
        })
        .collect()
}

/// Four-valued, because a three-valued classification cannot be computed:
/// `sudo -n true` has two outcomes and cannot tell "no sudo rights" from "sudo
/// needs a password".
fn privilege(host: &dyn Host, path: &[PathBuf]) -> Privilege {
    if capture(host, &["id", "-u"], path).as_deref() == Some("0") {
        return Privilege::Root;
    }
    if capture(host, &["sudo", "-n", "true"], path).is_some() {
        return Privilege::Passwordless;
    }
    // `sudo -n -l` succeeds when rules exist but a password is wanted for this
    // command; it fails outright when the user is not a sudoer at all.
    if capture(host, &["sudo", "-n", "-l"], path).is_some() {
        return Privilege::Password;
    }
    Privilege::Unavailable
}

/// This machine's os and arch, from compile-time constants.
pub fn host_platform() -> (Os, Arch) {
    let os = match std::env::consts::OS {
        "macos" => Os::Macos,
        _ => Os::Linux,
    };
    let arch = match std::env::consts::ARCH {
        "aarch64" | "arm64" => Arch::Arm64,
        _ => Arch::X86_64,
    };
    (os, arch)
}

/// Resolve every fact. `declared_shell` overrides the detected one, because on
/// a fresh box Bedouin is usually installing the shell it configures.
pub fn facts(host: &dyn Host, declared_shell: Option<Shell>) -> Result<Facts> {
    let (os, arch) = host_platform();
    facts_for(host, declared_shell, os, arch)
}

/// Resolve facts for a stated platform.
///
/// The platform is a parameter rather than a constant so a test can present a
/// fresh macOS to a Linux test runner -- otherwise the one machine class this
/// tool exists for could only ever be exercised on the machine you happen to
/// have.
pub fn facts_for(
    host: &dyn Host,
    declared_shell: Option<Shell>,
    os: Os,
    arch: Arch,
) -> Result<Facts> {
    let env: BTreeMap<String, String> = host.env().clone();
    let home = PathBuf::from(env.get("HOME").cloned().unwrap_or_else(|| "/".into()));

    let search: Vec<PathBuf> = env
        .get("PATH")
        .map(|p| p.split(':').map(PathBuf::from).collect())
        .unwrap_or_else(|| crate::plan::system_path(&Facts::fixture(os, Distro::Other, arch)));

    let (distro, distro_like, distro_version) = if os == Os::Macos {
        (
            Distro::Macos,
            DistroLike::None,
            capture(host, &["sw_vers", "-productVersion"], &search).unwrap_or_default(),
        )
    } else {
        let text = host
            .read(Path::new("/etc/os-release"))
            .ok()
            .flatten()
            .map(|b| String::from_utf8_lossy(&b).into_owned())
            .unwrap_or_default();
        let kv = os_release(&text);
        let id = kv.get("ID").map(String::as_str).unwrap_or("");
        let distro = Distro::parse(id).unwrap_or(Distro::Other);
        // ID_LIKE is authoritative where present; otherwise infer, so the arm
        // lattice and the resolver cannot disagree about what `ubuntu` implies.
        let like = kv
            .get("ID_LIKE")
            .and_then(|l| l.split_whitespace().find_map(|w| match w {
                "debian" => Some(DistroLike::Debian),
                "rhel" | "fedora" => Some(DistroLike::Rhel),
                "suse" | "opensuse" => Some(DistroLike::Suse),
                "arch" => Some(DistroLike::Arch),
                _ => None,
            }))
            .unwrap_or_else(|| distro_like_of(distro));
        (
            distro,
            like,
            kv.get("VERSION_ID").cloned().unwrap_or_default(),
        )
    };

    // $SHELL is preferred because it reflects what the user actually uses; the
    // passwd lookup exists because $SHELL is absent under some CI and container
    // invocations.
    let detected = env
        .get("SHELL")
        .map(|s| shell_from_path(s))
        .or_else(|| {
            let user = env.get("USER")?;
            let line = capture(host, &["getent", "passwd", user], &search)?;
            Some(shell_from_path(line.rsplit(':').next()?))
        })
        .unwrap_or(Shell::Other);
    let name = declared_shell.unwrap_or(detected);
    let (rc_file, rc_dir) = ShellFacts::paths_for(name, &home)
        .unwrap_or_else(|| (home.join(".profile"), home.join(".profile.d")));

    let hostname = env
        .get("HOSTNAME")
        .cloned()
        .or_else(|| {
            host.read(Path::new("/etc/hostname"))
                .ok()
                .flatten()
                .map(|b| String::from_utf8_lossy(&b).trim().to_string())
        })
        .or_else(|| capture(host, &["hostname", "-s"], &search))
        .unwrap_or_default();

    Ok(Facts {
        os,
        distro,
        distro_like,
        distro_version,
        arch,
        home,
        user: env.get("USER").cloned().unwrap_or_default(),
        hostname,
        shell: ShellFacts {
            name,
            detected,
            rc_file,
            rc_dir,
        },
        privilege: privilege(host, &search),
        env,
        managers: managers(host, &search),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::host::{FakeHost, FakeRun};

    fn base() -> FakeHost {
        FakeHost::new()
            .with_env("HOME", "/home/t")
            .with_env("USER", "t")
            .with_env("PATH", "/usr/bin:/bin")
    }

    #[test]
    fn os_release_is_parsed_with_and_without_quotes() {
        let kv = os_release("ID=ubuntu\nID_LIKE=debian\nVERSION_ID=\"24.04\"\nPRETTY=\"a=b\"");
        assert_eq!(kv["ID"], "ubuntu");
        assert_eq!(kv["VERSION_ID"], "24.04");
        assert_eq!(kv["PRETTY"], "a=b");
    }

    #[test]
    fn id_like_is_believed_over_the_built_in_table() {
        // A derivative Bedouin has never heard of still lands in the right
        // family, which is the point of `distro: other` being representable.
        let h = base().with_file("/etc/os-release", "ID=pop\nID_LIKE=ubuntu debian\nVERSION_ID=22.04\n");
        let f = facts(&h, None).unwrap();
        assert_eq!(f.distro, Distro::Other);
        assert_eq!(f.distro_like, DistroLike::Debian);
        assert_eq!(f.distro_version, "22.04");
    }

    #[test]
    fn a_declared_shell_overrides_the_detected_one() {
        // The fresh-box case: running from bash, installing and configuring zsh.
        let h = base().with_env("SHELL", "/bin/bash");
        let detected = facts(&h, None).unwrap();
        assert_eq!(detected.shell.name, Shell::Bash);
        assert_eq!(detected.shell.rc_dir, PathBuf::from("/home/t/.bashrc.d"));

        let declared = facts(&h, Some(Shell::Zsh)).unwrap();
        assert_eq!(declared.shell.name, Shell::Zsh);
        assert_eq!(declared.shell.detected, Shell::Bash);
        assert_eq!(declared.shell.rc_dir, PathBuf::from("/home/t/.zshrc.d"));
    }

    #[test]
    fn the_shell_falls_back_to_passwd_when_shell_is_unset() {
        let h = base().with_command(
            "getent passwd t",
            FakeRun::ok("t:x:1000:1000::/home/t:/usr/bin/zsh"),
        );
        assert_eq!(facts(&h, None).unwrap().shell.detected, Shell::Zsh);
    }

    #[test]
    fn privilege_distinguishes_all_four_machines() {
        let root = base().with_command("id -u", FakeRun::ok("0"));
        assert_eq!(facts(&root, None).unwrap().privilege, Privilege::Root);

        let free = base()
            .with_command("id -u", FakeRun::ok("1000"))
            .with_command("sudo -n true", FakeRun::ok(""));
        assert_eq!(facts(&free, None).unwrap().privilege, Privilege::Passwordless);

        let asks = base()
            .with_command("id -u", FakeRun::ok("1000"))
            .with_command("sudo -n true", FakeRun::fails(1, "a password is required"))
            .with_command("sudo -n -l", FakeRun::ok("(ALL) ALL"));
        assert_eq!(facts(&asks, None).unwrap().privilege, Privilege::Password);

        // Nothing scripted: no sudo at all.
        assert_eq!(
            facts(&base().with_command("id -u", FakeRun::ok("1000")), None)
                .unwrap()
                .privilege,
            Privilege::Unavailable
        );
    }

    #[test]
    fn only_managers_actually_present_are_reported() {
        let h = base()
            .with_binary("/usr/bin/apt-get")
            .with_binary("/usr/bin/mise");
        let f = facts(&h, None).unwrap();
        assert!(f.managers.contains(&Manager::Apt), "{:?}", f.managers);
        assert!(f.managers.contains(&Manager::Mise));
        assert!(!f.managers.contains(&Manager::Brew), "a fresh box has no brew");
    }
}
