//! Facts about the machine, resolved by the engine and never declared.
//!
//! The user asks "where is the rc dir" instead of writing conditionals about
//! it. Every enum here carries an `Other`/`None` arm on purpose: an
//! unrecognised machine must be *representable*, or Bedouin cannot run at all
//! somewhere it has not been taught about.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fmt;
use std::path::PathBuf;

macro_rules! str_enum {
    ($name:ident { $($variant:ident => $text:literal),+ $(,)? }) => {
        #[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
        #[serde(rename_all = "snake_case")]
        pub enum $name { $($variant),+ }

        impl $name {
            pub fn as_str(self) -> &'static str {
                match self { $(Self::$variant => $text),+ }
            }
            pub fn parse(s: &str) -> Option<Self> {
                match s { $($text => Some(Self::$variant),)+ _ => None }
            }
            pub const ALL: &'static [Self] = &[$(Self::$variant),+];
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str(self.as_str())
            }
        }
    };
}

str_enum!(Os { Macos => "macos", Linux => "linux" });

str_enum!(Distro {
    Ubuntu => "ubuntu",
    Debian => "debian",
    Fedora => "fedora",
    Opensuse => "opensuse",
    ArchLinux => "arch",
    Macos => "macos",
    Other => "other-distro",
});

str_enum!(DistroLike {
    Debian => "debian-like",
    Rhel => "rhel-like",
    Suse => "suse-like",
    Arch => "arch-like",
    None => "none-like",
});

str_enum!(Arch { X86_64 => "x86_64", Arm64 => "arm64" });

str_enum!(Shell { Zsh => "zsh", Bash => "bash", Fish => "fish", Other => "other" });

// How much authority Bedouin has to run privileged steps. Four-valued because
// a three-valued one cannot be computed: `sudo -n true` has two outcomes and
// cannot tell "no sudo rights" from "sudo needs a password". `Root` is not a
// curiosity -- containers run as root, and the smoke tests run in containers.
str_enum!(Privilege {
    Root => "root",
    Passwordless => "passwordless",
    Password => "password",
    Unavailable => "unavailable",
});

// Package managers and toolchain installers Bedouin knows how to drive.
str_enum!(Manager {
    Brew => "brew",
    Apt => "apt",
    Zypper => "zypper",
    Dnf => "dnf",
    Mise => "mise",
    Cargo => "cargo",
    Rustup => "rustup",
});

impl Manager {
    /// Managers Bedouin can install itself. apt and zypper are distro-provided
    /// and are never bootstrapped; declaring one on a machine that lacks it is
    /// a dropped DAG node, not an install step.
    pub fn is_bootstrappable(self) -> bool {
        matches!(self, Self::Brew | Self::Mise | Self::Cargo | Self::Rustup)
    }

    /// Whether this manager can exist on the given OS at all.
    pub fn runs_on(self, os: Os) -> bool {
        match self {
            Self::Apt | Self::Zypper | Self::Dnf => os == Os::Linux,
            _ => true,
        }
    }
}

/// Where a shell reads its startup configuration.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ShellFacts {
    /// The shell being *configured*. Defaults to `detected`, but a config may
    /// declare it, because on a fresh box Bedouin is usually installing the
    /// shell it is configuring and the detected one is the wrong answer.
    pub name: Shell,
    /// The pre-Bedouin login shell.
    pub detected: Shell,
    pub rc_file: PathBuf,
    /// The drop-in directory. Bedouin owns files inside it, never the rc file.
    pub rc_dir: PathBuf,
}

impl ShellFacts {
    /// Conventional rc file and drop-in directory for a shell under `home`.
    ///
    /// Returns `None` for [`Shell::Other`]: Bedouin will not guess at an
    /// unknown shell's syntax, and rc/PATH nodes become a plan-time error.
    pub fn paths_for(shell: Shell, home: &std::path::Path) -> Option<(PathBuf, PathBuf)> {
        let (rc, dir) = match shell {
            Shell::Zsh => (".zshrc", ".zshrc.d"),
            Shell::Bash => (".bashrc", ".bashrc.d"),
            Shell::Fish => (".config/fish/config.fish", ".config/fish/conf.d"),
            Shell::Other => return None,
        };
        Some((home.join(rc), home.join(dir)))
    }

    /// The extension Bedouin gives files it writes into the drop-in dir.
    pub fn rc_ext(&self) -> &'static str {
        match self.name {
            Shell::Zsh => "zsh",
            Shell::Bash => "bash",
            Shell::Fish => "fish",
            Shell::Other => "sh",
        }
    }
}

/// Everything the engine knows about the machine, resolved once per run.
///
/// Frozen into the plan artifact so a plan reviewed in one terminal applies
/// identically in another.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Facts {
    pub os: Os,
    pub distro: Distro,
    pub distro_like: DistroLike,
    pub distro_version: String,
    pub arch: Arch,
    pub home: PathBuf,
    pub user: String,
    pub hostname: String,
    pub shell: ShellFacts,
    pub privilege: Privilege,
    /// Only the variables the config actually references are ever frozen into
    /// an artifact; see `plan`. This map is the live environment.
    pub env: BTreeMap<String, String>,
    pub managers: Vec<Manager>,
}

impl Facts {
    /// A minimal fixture for tests and for `FakeHost`-driven runs.
    pub fn fixture(os: Os, distro: Distro, arch: Arch) -> Self {
        let home = PathBuf::from("/home/tester");
        let (rc_file, rc_dir) = ShellFacts::paths_for(Shell::Zsh, &home).unwrap();
        Facts {
            os,
            distro,
            distro_like: distro_like_of(distro),
            distro_version: "24.04".into(),
            arch,
            home,
            user: "tester".into(),
            hostname: "khaymah".into(),
            shell: ShellFacts {
                name: Shell::Zsh,
                detected: Shell::Bash,
                rc_file,
                rc_dir,
            },
            privilege: Privilege::Passwordless,
            env: BTreeMap::new(),
            managers: Vec::new(),
        }
    }
}

/// The `ID_LIKE` family a distro belongs to, used when `/etc/os-release` does
/// not say. Keeping this beside the enum means the arm lattice and the
/// resolver cannot disagree about what `ubuntu` implies.
pub fn distro_like_of(distro: Distro) -> DistroLike {
    match distro {
        Distro::Ubuntu | Distro::Debian => DistroLike::Debian,
        Distro::Fedora => DistroLike::Rhel,
        Distro::Opensuse => DistroLike::Suse,
        Distro::ArchLinux => DistroLike::Arch,
        Distro::Macos | Distro::Other => DistroLike::None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn enums_round_trip_through_their_text() {
        for os in Os::ALL {
            assert_eq!(Os::parse(os.as_str()), Some(*os));
        }
        for d in Distro::ALL {
            assert_eq!(Distro::parse(d.as_str()), Some(*d));
        }
        assert_eq!(Distro::parse("nope"), None);
    }

    #[test]
    fn apt_cannot_run_on_macos_and_is_never_bootstrapped() {
        assert!(!Manager::Apt.runs_on(Os::Macos));
        assert!(!Manager::Apt.is_bootstrappable());
        assert!(Manager::Brew.runs_on(Os::Macos));
        assert!(Manager::Brew.is_bootstrappable());
    }

    #[test]
    fn unknown_shell_has_no_guessed_paths() {
        let home = std::path::Path::new("/home/t");
        assert!(ShellFacts::paths_for(Shell::Other, home).is_none());
        let (rc, dir) = ShellFacts::paths_for(Shell::Zsh, home).unwrap();
        assert_eq!(rc, home.join(".zshrc"));
        assert_eq!(dir, home.join(".zshrc.d"));
    }
}
