//! The closed arm vocabulary and the implication lattice selection runs on.
//!
//! Arm names come from a closed set: built-ins plus the names declared under
//! `targets:`. The vocabulary does not depend on the machine, so a config is
//! valid or invalid *identically everywhere* -- only which arm wins varies. A
//! typo cannot become a branch that silently never matches on a machine you
//! are not sitting at.
//!
//! Arms are compared by the facts they **imply**, under subset inclusion. That
//! is the whole design:
//!
//! - `ubuntu` implies `{os=linux, distro=ubuntu, distro_like=debian}`
//! - `linux` implies `{os=linux}`
//!
//! so `ubuntu` strictly refines `linux` and wins where both are active. Under
//! a rule that merely counted pinned facts, both scored 1, "co-occurred", and
//! `{ubuntu: apt, linux: cargo}` was rejected at parse time with no fix the
//! user could express. Counting is also wrong in the other direction:
//! `debian-like` (2 facts) and `arm64` (1 fact) are *disjoint*, both true on a
//! Debian ARM box, and a size rule would silently pick the larger.

use crate::facts::{Arch, Distro, DistroLike, Facts, Os};
use std::collections::BTreeMap;
use std::fmt;
use std::sync::OnceLock;

/// The facts an arm pins. Absent fields are "don't care".
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct Implied {
    pub os: Option<Os>,
    pub distro: Option<Distro>,
    pub distro_like: Option<DistroLike>,
    pub arch: Option<Arch>,
}

impl Implied {
    /// Every fact `other` pins, self pins identically.
    pub fn contains(&self, other: &Implied) -> bool {
        fn ok<T: PartialEq>(mine: &Option<T>, theirs: &Option<T>) -> bool {
            match theirs {
                None => true,
                Some(t) => mine.as_ref() == Some(t),
            }
        }
        ok(&self.os, &other.os)
            && ok(&self.distro, &other.distro)
            && ok(&self.distro_like, &other.distro_like)
            && ok(&self.arch, &other.arch)
    }

    /// Self is strictly more specific than `other`.
    pub fn refines(&self, other: &Implied) -> bool {
        self != other && self.contains(other)
    }

    /// Two arms pin the same axis to different values, so no machine can
    /// satisfy both. `macos` and `ubuntu` conflict; `macos` and `arm64` do not.
    pub fn conflicts_with(&self, other: &Implied) -> bool {
        fn clash<T: PartialEq>(a: &Option<T>, b: &Option<T>) -> bool {
            matches!((a, b), (Some(x), Some(y)) if x != y)
        }
        clash(&self.os, &other.os)
            || clash(&self.distro, &other.distro)
            || clash(&self.distro_like, &other.distro_like)
            || clash(&self.arch, &other.arch)
    }

    /// Both arms can be active on one machine at once.
    pub fn can_cooccur(&self, other: &Implied) -> bool {
        !self.conflicts_with(other)
    }

    pub fn matches(&self, f: &Facts) -> bool {
        self.os.map_or(true, |v| v == f.os)
            && self.distro.map_or(true, |v| v == f.distro)
            && self.distro_like.map_or(true, |v| v == f.distro_like)
            && self.arch.map_or(true, |v| v == f.arch)
    }

    /// The arm that would pin everything both of these pin.
    pub fn union_with(&self, other: &Implied) -> Implied {
        self.union(*other)
    }

    fn union(self, other: Implied) -> Implied {
        Implied {
            os: self.os.or(other.os),
            distro: self.distro.or(other.distro),
            distro_like: self.distro_like.or(other.distro_like),
            arch: self.arch.or(other.arch),
        }
    }
}

/// An arm key: a built-in name, or a name declared under `targets:`.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ArmName(pub String);

impl ArmName {
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for ArmName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl From<&str> for ArmName {
    fn from(s: &str) -> Self {
        ArmName(s.to_owned())
    }
}

/// Base names, before the `{name}-{arch}` cross product is added.
fn base_arms() -> Vec<(&'static str, Implied)> {
    let linux = Implied {
        os: Some(Os::Linux),
        ..Default::default()
    };
    let d = |distro: Distro, like: DistroLike| Implied {
        os: Some(Os::Linux),
        distro: Some(distro),
        distro_like: Some(like),
        ..Default::default()
    };
    let like = |l: DistroLike| Implied {
        os: Some(Os::Linux),
        distro_like: Some(l),
        ..Default::default()
    };
    vec![
        // `macos` pins the distro too: on macOS the distro fact is always
        // `macos`, so the two are the same statement and collapsing them
        // avoids a duplicate arm name for one machine class.
        (
            "macos",
            Implied {
                os: Some(Os::Macos),
                distro: Some(Distro::Macos),
                ..Default::default()
            },
        ),
        ("linux", linux),
        ("ubuntu", d(Distro::Ubuntu, DistroLike::Debian)),
        ("debian", d(Distro::Debian, DistroLike::Debian)),
        ("fedora", d(Distro::Fedora, DistroLike::Rhel)),
        ("opensuse", d(Distro::Opensuse, DistroLike::Suse)),
        ("arch", d(Distro::ArchLinux, DistroLike::Arch)),
        (
            "other-distro",
            Implied {
                os: Some(Os::Linux),
                distro: Some(Distro::Other),
                ..Default::default()
            },
        ),
        ("debian-like", like(DistroLike::Debian)),
        ("rhel-like", like(DistroLike::Rhel)),
        ("suse-like", like(DistroLike::Suse)),
        ("arch-like", like(DistroLike::Arch)),
    ]
}

/// The full built-in vocabulary, including the `{name}-{arch}` cross product.
///
/// `DistroLike::None` is deliberately absent: branching on "belongs to no
/// family" is not a thing anyone means, and including it would make an arm
/// that is active on macOS and on every unrecognised Linux at once.
pub fn builtins() -> &'static BTreeMap<String, Implied> {
    static TABLE: OnceLock<BTreeMap<String, Implied>> = OnceLock::new();
    TABLE.get_or_init(|| {
        let mut map = BTreeMap::new();
        let arches = [
            (
                "x86_64",
                Implied {
                    arch: Some(Arch::X86_64),
                    ..Default::default()
                },
            ),
            (
                "arm64",
                Implied {
                    arch: Some(Arch::Arm64),
                    ..Default::default()
                },
            ),
        ];
        for (name, imp) in base_arms() {
            map.insert(name.to_string(), imp);
            for (aname, aimp) in &arches {
                map.insert(format!("{name}-{aname}"), imp.union(*aimp));
            }
        }
        for (aname, aimp) in arches {
            map.insert(aname.to_string(), aimp);
        }
        map
    })
}

pub fn builtin(name: &str) -> Option<Implied> {
    builtins().get(name).copied()
}

/// Closest known names to `input`, for a did-you-mean.
pub fn suggest(input: &str, known: impl Iterator<Item = String>) -> Vec<String> {
    let mut scored: Vec<(usize, String)> = known
        .map(|k| (edit_distance(input, &k), k))
        .filter(|(d, k)| *d <= 3.max(k.len() / 3))
        .collect();
    scored.sort();
    scored.into_iter().take(3).map(|(_, k)| k).collect()
}

/// Levenshtein, two-row. Small inputs; no need for anything cleverer.
fn edit_distance(a: &str, b: &str) -> usize {
    let (a, b): (Vec<char>, Vec<char>) = (a.chars().collect(), b.chars().collect());
    if a.is_empty() {
        return b.len();
    }
    let mut prev: Vec<usize> = (0..=b.len()).collect();
    let mut cur = vec![0usize; b.len() + 1];
    for (i, ca) in a.iter().enumerate() {
        cur[0] = i + 1;
        for (j, cb) in b.iter().enumerate() {
            let sub = prev[j] + usize::from(ca != cb);
            cur[j + 1] = sub.min(prev[j + 1] + 1).min(cur[j] + 1);
        }
        std::mem::swap(&mut prev, &mut cur);
    }
    prev[b.len()]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn imp(name: &str) -> Implied {
        builtin(name).unwrap_or_else(|| panic!("no built-in arm `{name}`"))
    }

    #[test]
    fn refinement_follows_the_distro_chain() {
        // The bug that made revision 1 unusable: `{ubuntu: apt, linux: cargo}`
        // must mean "apt on Ubuntu, cargo on other Linuxes", not a parse error.
        assert!(imp("ubuntu").refines(&imp("linux")));
        assert!(imp("ubuntu").refines(&imp("debian-like")));
        assert!(imp("debian-like").refines(&imp("linux")));
        assert!(!imp("linux").refines(&imp("ubuntu")));
    }

    #[test]
    fn a_conjunction_refines_both_of_its_halves() {
        assert!(imp("macos-arm64").refines(&imp("macos")));
        assert!(imp("macos-arm64").refines(&imp("arm64")));
        assert!(imp("ubuntu-x86_64").refines(&imp("ubuntu")));
        assert!(imp("ubuntu-x86_64").refines(&imp("x86_64")));
    }

    #[test]
    fn disjoint_axes_are_incomparable_even_when_sizes_differ() {
        // `debian-like` pins two facts and `arm64` pins one, but they are
        // disjoint and both true on a Debian ARM box. A size rule would pick
        // debian-like silently; subset inclusion makes it a tie.
        let (a, b) = (imp("debian-like"), imp("arm64"));
        assert!(!a.refines(&b) && !b.refines(&a));
        assert!(a.can_cooccur(&b), "both are true on a Debian ARM machine");

        let (m, r) = (imp("macos"), imp("arm64"));
        assert!(!m.refines(&r) && !r.refines(&m));
        assert!(m.can_cooccur(&r));
    }

    #[test]
    fn arms_pinning_one_axis_differently_can_never_both_be_active() {
        assert!(imp("macos").conflicts_with(&imp("ubuntu")));
        assert!(imp("ubuntu").conflicts_with(&imp("fedora")));
        assert!(imp("debian-like").conflicts_with(&imp("suse-like")));
        // ...so a value with both arms is fine: no machine sees a tie.
        assert!(!imp("macos").can_cooccur(&imp("ubuntu")));
    }

    #[test]
    fn matching_is_by_implied_facts_not_by_name() {
        let ubuntu = Facts::fixture(Os::Linux, Distro::Ubuntu, Arch::Arm64);
        assert!(imp("ubuntu").matches(&ubuntu));
        assert!(imp("debian-like").matches(&ubuntu));
        assert!(imp("linux").matches(&ubuntu));
        assert!(imp("arm64").matches(&ubuntu));
        assert!(imp("ubuntu-arm64").matches(&ubuntu));
        assert!(!imp("macos").matches(&ubuntu));
        assert!(!imp("x86_64").matches(&ubuntu));
        assert!(!imp("ubuntu-x86_64").matches(&ubuntu));
    }

    #[test]
    fn active_builtins_on_one_machine_are_totally_ordered_or_disjoint() {
        // Selection relies on this: if parse-time validation rejected every
        // incomparable co-occurring pair, the survivors that are active
        // together must be comparable, so there is a unique most-specific arm.
        let f = Facts::fixture(Os::Linux, Distro::Ubuntu, Arch::Arm64);
        let active: Vec<_> = builtins()
            .iter()
            .filter(|(_, i)| i.matches(&f))
            .map(|(n, i)| (n.clone(), *i))
            .collect();
        assert!(active.len() > 4, "expected several active arms");
        for (na, a) in &active {
            for (nb, b) in &active {
                assert!(
                    a == b || a.refines(b) || b.refines(a) || a.can_cooccur(b),
                    "`{na}` and `{nb}` are active together yet unrelated"
                );
            }
        }
        let most = active
            .iter()
            .find(|(_, a)| active.iter().all(|(_, b)| a == b || a.refines(b)));
        assert_eq!(most.map(|(n, _)| n.as_str()), Some("ubuntu-arm64"));
    }

    #[test]
    fn none_like_is_not_addressable() {
        assert!(builtin("none-like").is_none());
        assert!(builtin("mcaos").is_none());
    }

    #[test]
    fn did_you_mean_finds_the_near_miss() {
        let names = || builtins().keys().cloned();
        assert_eq!(suggest("mcaos", names()).first().map(String::as_str), Some("macos"));
        assert!(suggest("ubunut", names()).contains(&"ubuntu".to_string()));
        assert!(suggest("zzzzzzzzzz", names()).is_empty());
    }
}
