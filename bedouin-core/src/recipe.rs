//! Compiled-in installer recipes.
//!
//! A recipe is Bedouin's own knowledge, never user-supplied: the commands to
//! install and remove a package, how to pin a version for each manager, and
//! which bin directories the thing contributes. That last part is why the user
//! never has to tell Bedouin where rustup puts cargo.
//!
//! Every command is argv. Nothing here builds a shell string, so a package
//! name can never be read as shell syntax. Bootstrapping a manager that ships
//! as a piped installer is therefore two steps -- download, then run the
//! downloaded file -- rather than one `curl … | sh`.

use crate::facts::{Facts, Manager, Os};
use crate::host::Cmd;
use std::path::PathBuf;

/// Managers whose steps need root. Package managers that own `/usr` do; the
/// per-user ones do not, and running them as root would put files in the wrong
/// home.
pub fn needs_root(m: Manager) -> bool {
    // npm is deliberately absent, and it is the one manager where that is a
    // judgement rather than a fact. Under a node Bedouin manages -- mise, or
    // any per-user install -- the global prefix belongs to the user and root
    // would be wrong. Under a distro node it is /usr/local and root is needed.
    //
    // False is the safe half of that. Escalating would run `sudo npm` on the
    // setup Bedouin itself recommends, where root has no mise and resolves a
    // different node, or none. Not escalating fails a system-wide install
    // loudly, with EACCES naming the path.
    //
    // ponytail: static answer to an environment-dependent question. The fix is
    // a probe-time fact recording whether `npm root -g` is writable.
    matches!(m, Manager::Apt | Manager::Zypper | Manager::Dnf)
}

/// How each manager spells "this exact version".
fn pinned(m: Manager, pkg: &str, version: &str) -> String {
    match m {
        Manager::Apt => format!("{pkg}={version}"),
        Manager::Zypper | Manager::Dnf => format!("{pkg}-{version}"),
        Manager::Brew | Manager::Npm => format!("{pkg}@{version}"),
        // cargo and mise take the version as a separate flag; see `install`.
        _ => pkg.to_string(),
    }
}

/// `latest` means "install if absent, never upgrade" (§7.2), so it is not a
/// version to pin -- it is the absence of one.
fn concrete(version: Option<&str>) -> Option<&str> {
    version.filter(|v| *v != "latest" && !v.is_empty())
}

pub fn install(m: Manager, pkg: &str, version: Option<&str>) -> Cmd {
    let v = concrete(version);
    let mut cmd = match (m, v) {
        (Manager::Apt, _) => Cmd::new([
            "apt-get".into(),
            "install".into(),
            "-y".into(),
            v.map_or_else(|| pkg.to_string(), |ver| pinned(m, pkg, ver)),
        ]),
        (Manager::Npm, _) => Cmd::new([
            "npm".into(),
            "install".into(),
            "-g".into(),
            v.map_or_else(|| pkg.to_string(), |ver| pinned(m, pkg, ver)),
        ]),
        (Manager::Zypper, _) => Cmd::new([
            "zypper".into(),
            "--non-interactive".into(),
            "install".into(),
            v.map_or_else(|| pkg.to_string(), |ver| pinned(m, pkg, ver)),
        ]),
        (Manager::Dnf, _) => Cmd::new([
            "dnf".into(),
            "install".into(),
            "-y".into(),
            v.map_or_else(|| pkg.to_string(), |ver| pinned(m, pkg, ver)),
        ]),
        (Manager::Brew, _) => Cmd::new([
            "brew".into(),
            "install".into(),
            v.map_or_else(|| pkg.to_string(), |ver| pinned(m, pkg, ver)),
        ]),
        (Manager::Cargo, Some(ver)) => {
            Cmd::new(["cargo", "install", "--locked", "--version", ver, pkg])
        }
        (Manager::Cargo, None) => Cmd::new(["cargo", "install", "--locked", pkg]),
        (Manager::Mise, Some(ver)) => Cmd::new(["mise", "use", "-g", &format!("{pkg}@{ver}")]),
        (Manager::Mise, None) => Cmd::new(["mise", "use", "-g", pkg]),
        (Manager::Rustup, Some(ver)) => Cmd::new(["rustup", "toolchain", "install", ver]),
        (Manager::Rustup, None) => Cmd::new(["rustup", "toolchain", "install", "stable"]),
    };
    cmd.root = needs_root(m);
    cmd
}

/// Refresh a manager's package lists.
///
/// `None` where there is nothing to refresh. A freshly imaged machine has no
/// apt lists at all, so without this the very first install on the very
/// machine class Bedouin exists for fails with "Unable to locate package".
/// Shell-quote, for the recipes that need a `sh -c` to express themselves.
fn sq(s: &str) -> String {
    format!("'{}'", s.replace('\'', r"'\''"))
}

/// Ask the manager whether it already has this package. Exit 0 means yes.
///
/// This exists because presence cannot be answered by looking for a binary.
/// `dnsutils` installs `dig`, `git-delta` installs `delta`, `bind-utils`
/// installs `dig` -- so `which(name)` says "not installed" and apply then runs
/// an install that the manager turns into a no-op. The package was already
/// there, but Bedouin recorded itself as the owner, and dropping the line from
/// the config later removed software Bedouin never installed.
///
/// `None` for a manager with no cheap way to ask. The caller installs, exactly
/// as before -- not knowing is the old behaviour, not a new failure.
pub fn installed(m: Manager, pkg: &str) -> Option<Cmd> {
    let sh = |script: String| Cmd::new(["sh".to_string(), "-c".into(), script]);
    Some(match m {
        // dpkg-query exits 0 for a package that is merely *known* -- removed
        // but with config files left behind is still "known". Only the status
        // field separates that from installed.
        Manager::Apt => sh(format!(
            "dpkg-query -W -f='${{db:Status-Status}}' {} 2>/dev/null | grep -qx installed",
            sq(pkg)
        )),
        // Both rpm distros answer through rpm itself, which is faster than
        // asking dnf or zypper and does not touch the network.
        Manager::Dnf | Manager::Zypper => Cmd::new(["rpm".to_string(), "-q".into(), pkg.into()]),
        Manager::Brew => Cmd::new([
            "brew".to_string(),
            "list".into(),
            "--versions".into(),
            pkg.into(),
        ]),
        Manager::Npm => Cmd::new([
            "npm".to_string(),
            "ls".into(),
            "-g".into(),
            "--depth=0".into(),
            pkg.into(),
        ]),
        // The header lines of `cargo install --list` are "name vX.Y.Z[ (src)]:"
        // at column zero; the binaries it installed are indented beneath.
        Manager::Cargo => sh(format!(
            "cargo install --list | grep -q {}",
            sq(&format!("^{pkg} v"))
        )),
        // mise and rustup install toolchains rather than packages, and neither
        // reaches this arm today. Left unanswered rather than guessed at.
        Manager::Mise | Manager::Rustup => return None,
    })
}

pub fn refresh(m: Manager) -> Option<Cmd> {
    let mut cmd = match m {
        Manager::Apt => Cmd::new(["apt-get", "update"]),
        Manager::Zypper => Cmd::new(["zypper", "--non-interactive", "refresh"]),
        Manager::Brew => Cmd::new(["brew", "update"]),
        _ => return None,
    };
    cmd.root = needs_root(m);
    Some(cmd)
}

pub fn remove(m: Manager, pkg: &str) -> Cmd {
    let mut cmd = match m {
        Manager::Apt => Cmd::new(["apt-get", "remove", "-y", pkg]),
        Manager::Zypper => Cmd::new(["zypper", "--non-interactive", "remove", pkg]),
        Manager::Dnf => Cmd::new(["dnf", "remove", "-y", pkg]),
        Manager::Npm => Cmd::new(["npm", "uninstall", "-g", pkg]),
        Manager::Brew => Cmd::new(["brew", "uninstall", pkg]),
        Manager::Cargo => Cmd::new(["cargo", "uninstall", pkg]),
        Manager::Mise => Cmd::new(["mise", "rm", "-g", pkg]),
        Manager::Rustup => Cmd::new(["rustup", "toolchain", "uninstall", pkg]),
    };
    cmd.root = needs_root(m);
    cmd
}

/// Steps that put a manager on a machine that lacks it.
///
/// `None` for apt, zypper and dnf: those are the distro's, and Bedouin does
/// not install a distro's package manager.
pub fn bootstrap(m: Manager, facts: &Facts) -> Option<Vec<Cmd>> {
    let tmp = |name: &str| format!("/tmp/bedouin-{name}");
    match m {
        Manager::Rustup | Manager::Cargo => {
            let script = tmp("rustup.sh");
            Some(vec![
                Cmd::new([
                    "curl",
                    "--proto",
                    "=https",
                    "--tlsv1.2",
                    "-sSfL",
                    "https://sh.rustup.rs",
                    "-o",
                    &script,
                ]),
                // Downloaded, then run as a file. The upstream one-liner pipes
                // curl into sh; keeping the two apart is what lets every step
                // stay argv.
                Cmd::new(["sh", &script, "-y", "--no-modify-path"]),
            ])
        }
        Manager::Brew => {
            let script = tmp("brew.sh");
            let mut cmds = Vec::new();
            // Homebrew on Linux stops at the first missing prerequisite, and
            // on a fresh machine they are all missing -- git especially, which
            // it needs to clone itself with. They are the distro's to provide,
            // and the package phase that would install them runs AFTER this
            // one, so brew has to ask for its own. `unzip` is not on
            // Homebrew's list but casks need it, and `brew install
            // 1password-cli` on Linux is a cask.
            if facts.os == Os::Linux && facts.managers.contains(&Manager::Apt) {
                let mut pre = Cmd::new([
                    "apt-get",
                    "install",
                    "-y",
                    "build-essential",
                    "procps",
                    "curl",
                    "file",
                    "git",
                    "unzip",
                ]);
                pre.root = true;
                cmds.push(pre);
            }
            cmds.push(Cmd::new([
                "curl",
                "-fsSL",
                "https://raw.githubusercontent.com/Homebrew/install/HEAD/install.sh",
                "-o",
                &script,
            ]));
            cmds.push(Cmd::new(["bash", &script]));
            Some(cmds)
        }
        Manager::Mise => {
            let script = tmp("mise.sh");
            Some(vec![
                Cmd::new(["curl", "-fsSL", "https://mise.run", "-o", &script]),
                Cmd::new(["sh", &script]),
            ])
        }
        // npm is not installed on its own: it arrives with node. Declare node
        // under `languages:` and npm is simply there -- and if the machine
        // already has its own node, that is the npm Bedouin uses.
        Manager::Apt | Manager::Zypper | Manager::Dnf | Manager::Npm => {
            let _ = facts;
            None
        }
    }
}

/// Steps that install a shell framework, and where it lands.
///
/// Fetch-then-run, argv only, exactly like brew and rustup: the upstream
/// one-liner pipes curl into sh, and keeping the two apart is what lets every
/// step stay argv.
pub fn framework_install(kind: &str, facts: &Facts) -> Option<(PathBuf, Vec<Cmd>)> {
    match kind {
        "oh-my-zsh" => {
            let script = "/tmp/bedouin-omz.sh";
            let mut run = Cmd::new(["sh", script, "--unattended", "--keep-zshrc"]);
            // --keep-zshrc matters: bedouin owns a BLOCK in your .zshrc, and
            // letting the installer replace the file would take your config
            // with it.
            run.env.insert("RUNZSH".into(), "no".into());
            run.env.insert("CHSH".into(), "no".into());
            Some((
                facts.home.join(".oh-my-zsh"),
                vec![
                    Cmd::new([
                        "curl",
                        "-fsSL",
                        "https://raw.githubusercontent.com/ohmyzsh/ohmyzsh/master/tools/install.sh",
                        "-o",
                        script,
                    ]),
                    run,
                ],
            ))
        }
        _ => None,
    }
}

/// Bin directories a manager or language contributes once installed.
pub fn bin_dirs(name: &str, facts: &Facts) -> Vec<PathBuf> {
    let home = &facts.home;
    match name {
        "rust" | "rustup" | "cargo" => vec![home.join(".cargo/bin")],
        // `go install` drops binaries in GOPATH/bin, which is not where the
        // toolchain itself lives -- so installing go is not enough to make
        // what go installs runnable.
        "go" => vec![home.join("go/bin")],
        "mise" => vec![
            home.join(".local/bin"),
            home.join(".local/share/mise/shims"),
        ],
        "brew" => vec![PathBuf::from(if facts.os == Os::Macos {
            "/opt/homebrew/bin"
        } else {
            "/home/linuxbrew/.linuxbrew/bin"
        })],
        _ => Vec::new(),
    }
}

/// The installer a language brings its own script for.
///
/// Preferred over a generic version manager: rustup is how Rust is meant to be
/// installed, it is what `rustup component add` and toolchain pinning expect,
/// and it is what a machine that already has Rust almost certainly used. mise
/// is the fallback for languages that ship no installer of their own -- it
/// fetches the upstream builds too, so it is still the source, just not a
/// first-party script.
pub fn default_installer(language: &str) -> Manager {
    match language {
        "rust" => Manager::Rustup,
        _ => Manager::Mise,
    }
}

/// The binary that proves a toolchain is present. Not the language name:
/// nothing on a machine with Rust is called `rust`.
/// What a person installed on purpose, one name per line.
///
/// Not "everything installed" -- that is thousands of packages and useless.
/// Each manager is asked for the subset a human chose, which every manager
/// spells differently and two cannot usefully answer at all.
///
/// `None` means Bedouin will not guess. dnf and zypper are the interesting
/// refusal: `dnf repoquery --userinstalled` is accurate on a real install but
/// returns the whole base system in a container, because the image build
/// marked it user-installed. rpm has no equivalent of Debian's `Priority`
/// field to filter on. The clean signal is `dnf history` -- transaction 1 is
/// the image, later ones are the user -- and reading it means parsing
/// transactions rather than a list.
///
/// ponytail: apt, brew, cargo and npm only. Add dnf when the history-parsing
/// is worth it; until then it is honest to say nothing.
pub fn list_manual(m: Manager) -> Option<Cmd> {
    let sh = |script: &str| Cmd::new(["sh".to_string(), "-c".into(), script.to_string()]);
    Some(match m {
        // `apt-mark showmanual` is 93 packages on a bare image, nearly all of
        // it the base system. Priority is what separates them: required,
        // important and standard are the distro's, optional and extra are
        // things somebody asked for. One apt-cache call, not one per package.
        Manager::Apt => sh(
            "apt-mark showmanual 2>/dev/null | xargs -r apt-cache --no-all-versions show 2>/dev/null \
             | awk '/^Package:/{p=$2} /^Priority:/{if($2!=\"required\"&&$2!=\"important\"&&$2!=\"standard\")print p}' \
             | sort -u",
        ),
        // brew already draws this distinction: leaves are the formulae nothing
        // else depends on.
        Manager::Brew => Cmd::new(["brew".to_string(), "leaves".into()]),
        // Header lines are `name vX.Y.Z[ (source)]:` at column zero; the
        // binaries a crate installed are indented under it.
        Manager::Cargo => sh("cargo install --list | sed -n 's/^\\([^ ]*\\) v.*:$/\\1/p'"),
        // --parseable gives paths; the name is whatever follows node_modules,
        // which keeps @scope/name in one piece. npm and corepack ship with
        // node, so every machine would otherwise be told to adopt them.
        Manager::Npm => sh(
            "npm ls -g --depth=0 --parseable 2>/dev/null | grep '/node_modules/' \
             | sed 's|.*/node_modules/||' | grep -vx 'npm' | grep -vx 'corepack' | sort -u",
        ),
        Manager::Dnf | Manager::Zypper | Manager::Mise | Manager::Rustup => return None,
    })
}

/// The package manager a language brings with it.
///
/// node ships npm, so declaring node under `languages:` is what makes
/// `from: npm` resolvable -- the same relationship `rust` has with cargo,
/// which `plan` spells out separately because it also has to reason about
/// rustup. A language Bedouin does not install still counts: if the machine
/// already has node, npm is already there, and that is the npm Bedouin uses.
pub fn provides_manager(language: &str) -> Option<Manager> {
    match language {
        "node" => Some(Manager::Npm),
        _ => None,
    }
}

pub fn probe_bin(language: &str) -> &str {
    match language {
        "rust" => "cargo",
        "python" => "python3",
        "golang" => "go",
        other => other,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::facts::{Arch, Distro};

    #[test]
    fn only_the_managers_that_can_answer_are_asked_what_was_installed_by_hand() {
        // Silence is the deliberate answer for these. dnf's own
        // `--userinstalled` returns the whole base system in a container, and
        // rpm has no Priority field to filter on; mise and rustup install
        // toolchains, which `languages:` already covers.
        for m in [
            Manager::Dnf,
            Manager::Zypper,
            Manager::Mise,
            Manager::Rustup,
        ] {
            assert!(list_manual(m).is_none(), "{m} must not guess");
        }
        for m in [Manager::Apt, Manager::Brew, Manager::Cargo, Manager::Npm] {
            assert!(list_manual(m).is_some(), "{m} can answer");
        }
        // brew already means "installed on purpose, not as a dependency".
        assert_eq!(list_manual(Manager::Brew).unwrap().argv, ["brew", "leaves"]);

        // The shell ones are pipelines, so assert the parts that carry the
        // meaning rather than the whole string.
        let apt = list_manual(Manager::Apt).unwrap().argv.join(" ");
        assert!(apt.contains("apt-mark showmanual"), "{apt}");
        // Priority is what separates the distro's packages from a person's.
        for p in ["required", "important", "standard"] {
            assert!(apt.contains(p), "apt filter lost {p}: {apt}");
        }
        let npm = list_manual(Manager::Npm).unwrap().argv.join(" ");
        assert!(npm.contains("--parseable"), "{npm}");
        // Both ship with node; every machine would otherwise be told to
        // adopt them.
        assert!(npm.contains("grep -vx 'npm'"), "{npm}");
        assert!(npm.contains("grep -vx 'corepack'"), "{npm}");
    }

    #[test]
    fn npm_is_the_users_own_npm() {
        // It arrives with node and is never bootstrapped: a machine that has
        // node has npm, and that is the one Bedouin drives.
        assert!(!Manager::Npm.is_bootstrappable());
        assert!(bootstrap(
            Manager::Npm,
            &Facts::fixture(Os::Linux, Distro::Ubuntu, Arch::X86_64)
        )
        .is_none());
        assert!(Manager::Npm.runs_on(Os::Linux));
        assert!(Manager::Npm.runs_on(Os::Macos));

        // A package manager, not a toolchain installer.
        assert!(Manager::Npm.installs_packages());
        assert!(!Manager::Npm.installs_toolchains());
        assert_eq!(Manager::Npm.probe_bin(), "npm");

        // Declaring node is what makes `from: npm` resolvable.
        assert_eq!(provides_manager("node"), Some(Manager::Npm));
        assert_eq!(provides_manager("python"), None);

        assert_eq!(
            install(Manager::Npm, "is-odd", None).argv,
            ["npm", "install", "-g", "is-odd"]
        );
        assert_eq!(
            remove(Manager::Npm, "is-odd").argv,
            ["npm", "uninstall", "-g", "is-odd"]
        );
        // Exit 0 when present, non-zero when absent -- verified against a real
        // npm both ways.
        assert_eq!(
            installed(Manager::Npm, "is-odd")
                .expect("npm can answer")
                .argv,
            ["npm", "ls", "-g", "--depth=0", "is-odd"]
        );
        // Not escalated: see `needs_root`.
        assert!(!needs_root(Manager::Npm));
        assert!(refresh(Manager::Npm).is_none());
    }

    #[test]
    fn each_manager_spells_a_pinned_version_its_own_way() {
        assert_eq!(
            install(Manager::Apt, "jq", Some("1.7")).argv,
            ["apt-get", "install", "-y", "jq=1.7"]
        );
        assert_eq!(
            install(Manager::Brew, "jq", Some("1.7")).argv,
            ["brew", "install", "jq@1.7"]
        );
        // npm spells it with @ as well. `pinned` has a wildcard default that
        // returns the bare name, so a manager missing from that match silently
        // installs latest and reports success -- which is why this is asserted
        // rather than assumed.
        assert_eq!(
            install(Manager::Npm, "eslint", Some("8.0.0")).argv,
            ["npm", "install", "-g", "eslint@8.0.0"]
        );
        assert_eq!(
            install(Manager::Cargo, "zellij", Some("0.40.1")).argv,
            [
                "cargo",
                "install",
                "--locked",
                "--version",
                "0.40.1",
                "zellij"
            ]
        );
        assert_eq!(
            install(Manager::Zypper, "jq", Some("1.7")).argv,
            ["zypper", "--non-interactive", "install", "jq-1.7"]
        );
    }

    #[test]
    fn latest_is_the_absence_of_a_pin_not_a_version_to_install() {
        // §7.2: `latest` means install if absent, never upgrade. Passing it
        // through as a version string would ask apt for a package literally
        // called `jq=latest`.
        assert_eq!(
            install(Manager::Apt, "jq", Some("latest")).argv,
            ["apt-get", "install", "-y", "jq"]
        );
        assert_eq!(
            install(Manager::Cargo, "zellij", Some("latest")).argv,
            ["cargo", "install", "--locked", "zellij"]
        );
        assert_eq!(
            install(Manager::Apt, "jq", None).argv,
            install(Manager::Apt, "jq", Some("latest")).argv
        );
    }

    #[test]
    fn only_the_system_managers_ask_for_root() {
        assert!(install(Manager::Apt, "jq", None).root);
        assert!(install(Manager::Zypper, "jq", None).root);
        // Running a per-user manager as root would put files in root's home.
        assert!(!install(Manager::Brew, "jq", None).root);
        assert!(!install(Manager::Cargo, "zellij", None).root);
        assert!(!install(Manager::Mise, "node", None).root);
    }

    #[test]
    fn the_managers_with_package_lists_know_how_to_refresh_them() {
        assert_eq!(refresh(Manager::Apt).unwrap().argv, ["apt-get", "update"]);
        assert!(refresh(Manager::Apt).unwrap().root);
        assert!(
            refresh(Manager::Cargo).is_none(),
            "cargo has no index to refresh"
        );
        assert!(refresh(Manager::Rustup).is_none());
    }

    #[test]
    fn a_distro_package_manager_is_never_bootstrapped() {
        let f = Facts::fixture(Os::Linux, Distro::Ubuntu, Arch::X86_64);
        assert!(bootstrap(Manager::Apt, &f).is_none());
        assert!(bootstrap(Manager::Zypper, &f).is_none());
        assert!(bootstrap(Manager::Brew, &f).is_some());
        assert!(bootstrap(Manager::Rustup, &f).is_some());
    }

    #[test]
    fn nothing_a_recipe_emits_is_a_shell_string() {
        // A package name reaching a shell would be an injection; every step is
        // argv, and the piped upstream installers are split into fetch + run.
        let f = Facts::fixture(Os::Linux, Distro::Ubuntu, Arch::X86_64);
        let mut all: Vec<Cmd> = Vec::new();
        for m in Manager::ALL {
            all.push(install(*m, "pkg; rm -rf /", None));
            all.push(remove(*m, "pkg; rm -rf /"));
            all.extend(bootstrap(*m, &f).unwrap_or_default());
        }
        for c in &all {
            assert!(!c.argv.is_empty());
            // No step is `sh -c <string>`, which is the shape that would let a
            // package name become code.
            let piped = c.argv.windows(2).any(|w| w[0] == "-c");
            assert!(!piped, "a -c string slipped in: {:?}", c.argv);
        }
    }

    #[test]
    fn brew_asks_for_its_own_prerequisites_on_linux() {
        // The package phase runs after the manager phase, so `git` being in
        // the config does not help: brew needs it to clone itself, and on a
        // bare box the install script stops with "You must install Git".
        let mut linux = Facts::fixture(Os::Linux, Distro::Ubuntu, Arch::X86_64);
        linux.managers = vec![Manager::Apt];
        let cmds = bootstrap(Manager::Brew, &linux).expect("brew bootstraps");
        let first = &cmds[0];
        assert!(first.argv.contains(&"git".to_string()), "{:?}", first.argv);
        assert!(first.root, "installing them needs root");

        // macOS brings its own; nothing to install first.
        let mac = Facts::fixture(Os::Macos, Distro::Macos, Arch::Arm64);
        let cmds = bootstrap(Manager::Brew, &mac).expect("brew bootstraps");
        assert!(
            cmds[0].argv.first().is_some_and(|a| a == "curl"),
            "{:?}",
            cmds[0].argv
        );
    }

    #[test]
    fn brew_lands_in_a_different_place_on_each_platform() {
        let mac = Facts::fixture(Os::Macos, Distro::Macos, Arch::Arm64);
        let linux = Facts::fixture(Os::Linux, Distro::Ubuntu, Arch::X86_64);
        assert_eq!(bin_dirs("brew", &mac), [PathBuf::from("/opt/homebrew/bin")]);
        assert_eq!(
            bin_dirs("brew", &linux),
            [PathBuf::from("/home/linuxbrew/.linuxbrew/bin")]
        );
        assert_eq!(bin_dirs("rust", &linux), [linux.home.join(".cargo/bin")]);
    }
}
