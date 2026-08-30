//! Declared targets: the only match language in the file, and the escape
//! hatch for every axis a compiled-in enum cannot know.
//!
//! `distro_version` is why the vocabulary cannot be closed-and-only-closed.
//! The most common reason a bootstrap config needs a conditional at all is
//! "Ubuntu 22.04 ships an unusably old X", and versions are not enumerable.
//! Declaring a target keeps arm *names* closed while leaving the *axes* open,
//! and needs no Bedouin release to address a new machine class.

use crate::arm::{self, Implied};
use crate::facts::Facts;
use crate::value::OneOrMany;
use serde::{Deserialize, Serialize};
use std::cmp::Ordering;
use std::collections::BTreeMap;

/// A condition over facts. Scalars match exactly, lists match any, and a
/// `distro_version` beginning with an operator compares as a version.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MatchSpec {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub os: Option<OneOrMany<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub distro: Option<OneOrMany<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub distro_like: Option<OneOrMany<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub distro_version: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub arch: Option<OneOrMany<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hostname: Option<OneOrMany<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub env: Option<BTreeMap<String, String>>,
}

impl MatchSpec {
    /// How many facts this pins. Not used for selection -- declared targets are
    /// ordered by declaration, not specificity -- but it is what tells the user
    /// an empty `match: {}` is empty.
    pub fn key_count(&self) -> usize {
        usize::from(self.os.is_some())
            + usize::from(self.distro.is_some())
            + usize::from(self.distro_like.is_some())
            + usize::from(self.distro_version.is_some())
            + usize::from(self.arch.is_some())
            + usize::from(self.hostname.is_some())
            + self.env.as_ref().map_or(0, BTreeMap::len)
    }

    pub fn is_empty(&self) -> bool {
        self.key_count() == 0
    }

    pub fn matches(&self, f: &Facts) -> bool {
        fn any(spec: &Option<OneOrMany<String>>, actual: &str) -> bool {
            match spec {
                None => true,
                Some(o) => o.iter().any(|s| s == actual),
            }
        }
        any(&self.os, f.os.as_str())
            && any(&self.distro, f.distro.as_str())
            && any(&self.distro_like, f.distro_like.as_str())
            && any(&self.arch, f.arch.as_str())
            && any(&self.hostname, &f.hostname)
            && self
                .distro_version
                .as_deref()
                .map_or(true, |req| version_matches(req, &f.distro_version))
            && self.env.as_ref().map_or(true, |want| {
                want.iter()
                    .all(|(k, v)| f.env.get(k).map(String::as_str) == Some(v.as_str()))
            })
    }
}

/// `">=24.04"`, `"<22"`, or a bare `"24.04"` meaning equality.
pub fn version_matches(requirement: &str, actual: &str) -> bool {
    let req = requirement.trim();
    let (op, want) = if let Some(r) = req.strip_prefix(">=") {
        ("ge", r)
    } else if let Some(r) = req.strip_prefix("<=") {
        ("le", r)
    } else if let Some(r) = req.strip_prefix('>') {
        ("gt", r)
    } else if let Some(r) = req.strip_prefix('<') {
        ("lt", r)
    } else {
        ("eq", req)
    };
    let ord = cmp_versions(actual, want.trim());
    match op {
        "ge" => ord != Ordering::Less,
        "le" => ord != Ordering::Greater,
        "gt" => ord == Ordering::Greater,
        "lt" => ord == Ordering::Less,
        _ => ord == Ordering::Equal,
    }
}

/// Compare dotted versions component-wise, numerically where both components
/// are numeric. `24.04` and `24.4` compare equal, which is what a user writing
/// `>=24.4` against Ubuntu's `24.04` means.
pub fn cmp_versions(a: &str, b: &str) -> Ordering {
    let mut ai = a.split(['.', '-', '_']);
    let mut bi = b.split(['.', '-', '_']);
    loop {
        match (ai.next(), bi.next()) {
            (None, None) => return Ordering::Equal,
            // A missing trailing component is zero: 24 == 24.0
            (x, y) => {
                let (x, y) = (x.unwrap_or("0"), y.unwrap_or("0"));
                let ord = match (x.parse::<u64>(), y.parse::<u64>()) {
                    (Ok(m), Ok(n)) => m.cmp(&n),
                    _ => x.cmp(y),
                };
                if ord != Ordering::Equal {
                    return ord;
                }
            }
        }
    }
}

/// A named condition. Its `name` doubles as an arm key everywhere below.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Target {
    pub name: String,
    #[serde(default)]
    pub r#match: MatchSpec,
    /// Overrides folded into the base `vars:` block when this target is active.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub vars: BTreeMap<String, crate::value::Value<crate::value::Tmpl>>,
}

/// What an arm key turned out to be.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArmKind {
    /// A built-in name and the facts it implies.
    Builtin(Implied),
    /// A declared target, carrying its declaration index. Lower wins.
    Declared(usize),
}

/// Built-in names plus the names declared under `targets:`.
#[derive(Debug, Clone, Default)]
pub struct Vocabulary {
    declared: Vec<Target>,
    index: BTreeMap<String, usize>,
}

#[derive(Debug, PartialEq, Eq)]
pub enum VocabError {
    /// A declared name shadows a built-in, so the same key would mean two things.
    ShadowsBuiltin(String),
    Duplicate(String),
    EmptyMatch(String),
}

impl std::fmt::Display for VocabError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::ShadowsBuiltin(n) => write!(
                f,
                "target `{n}` collides with a built-in arm name; pick another name"
            ),
            Self::Duplicate(n) => write!(f, "target `{n}` is declared twice"),
            Self::EmptyMatch(n) => write!(
                f,
                "target `{n}` has an empty `match:`, so it matches everything; write `default:` instead"
            ),
        }
    }
}

impl Vocabulary {
    pub fn new(declared: Vec<Target>) -> Result<Self, VocabError> {
        let mut index = BTreeMap::new();
        for (i, t) in declared.iter().enumerate() {
            if arm::builtin(&t.name).is_some() {
                return Err(VocabError::ShadowsBuiltin(t.name.clone()));
            }
            if t.r#match.is_empty() {
                return Err(VocabError::EmptyMatch(t.name.clone()));
            }
            if index.insert(t.name.clone(), i).is_some() {
                return Err(VocabError::Duplicate(t.name.clone()));
            }
        }
        Ok(Self { declared, index })
    }

    pub fn classify(&self, name: &str) -> Option<ArmKind> {
        if let Some(i) = self.index.get(name) {
            return Some(ArmKind::Declared(*i));
        }
        arm::builtin(name).map(ArmKind::Builtin)
    }

    pub fn is_known(&self, name: &str) -> bool {
        self.classify(name).is_some()
    }

    /// Is this arm true on this machine?
    pub fn matches(&self, name: &str, f: &Facts) -> bool {
        match self.classify(name) {
            Some(ArmKind::Builtin(i)) => i.matches(f),
            Some(ArmKind::Declared(i)) => self.declared[i].r#match.matches(f),
            None => false,
        }
    }

    pub fn declared(&self) -> &[Target] {
        &self.declared
    }

    /// Every legal arm key, for did-you-mean and for `--list-arms`.
    pub fn all_names(&self) -> impl Iterator<Item = String> + '_ {
        arm::builtins()
            .keys()
            .cloned()
            .chain(self.declared.iter().map(|t| t.name.clone()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::facts::{Arch, Distro, Os};

    fn t(name: &str, m: MatchSpec) -> Target {
        Target {
            name: name.into(),
            r#match: m,
            ..Default::default()
        }
    }

    #[test]
    fn versions_compare_component_wise_not_lexically() {
        assert_eq!(cmp_versions("24.04", "24.04"), Ordering::Equal);
        // Lexically "24.04" < "24.4"; numerically they are the same release.
        assert_eq!(cmp_versions("24.04", "24.4"), Ordering::Equal);
        // Lexically "9" > "10"; numerically it is not.
        assert_eq!(cmp_versions("9", "10"), Ordering::Less);
        assert_eq!(cmp_versions("24", "24.0"), Ordering::Equal);
        assert_eq!(cmp_versions("22.04", "24.04"), Ordering::Less);
    }

    #[test]
    fn version_operators_do_what_they_say() {
        assert!(version_matches(">=24.04", "24.04"));
        assert!(version_matches(">=24.04", "25.10"));
        assert!(!version_matches(">=24.04", "22.04"));
        assert!(version_matches("<24.04", "22.04"));
        assert!(version_matches("24.04", "24.04"));
        assert!(!version_matches("24.04", "22.04"));
    }

    #[test]
    fn the_noble_target_from_the_spec_matches_only_new_enough_ubuntu() {
        let noble = MatchSpec {
            distro: Some(OneOrMany::One("ubuntu".into())),
            distro_version: Some(">=24.04".into()),
            ..Default::default()
        };
        let mut f = Facts::fixture(Os::Linux, Distro::Ubuntu, Arch::X86_64);
        assert!(noble.matches(&f));
        f.distro_version = "22.04".into();
        assert!(!noble.matches(&f), "22.04 is the case noble exists to exclude");
        f.distro_version = "24.04".into();
        f.distro = Distro::Debian;
        assert!(!noble.matches(&f));
    }

    #[test]
    fn env_matching_needs_every_named_variable() {
        let work = MatchSpec {
            env: Some([("BEDOUIN_PROFILE".to_string(), "work".to_string())].into()),
            ..Default::default()
        };
        let mut f = Facts::fixture(Os::Linux, Distro::Ubuntu, Arch::X86_64);
        assert!(!work.matches(&f), "unset variable must not match");
        f.env.insert("BEDOUIN_PROFILE".into(), "home".into());
        assert!(!work.matches(&f));
        f.env.insert("BEDOUIN_PROFILE".into(), "work".into());
        assert!(work.matches(&f));
    }

    #[test]
    fn a_declared_name_may_not_shadow_a_builtin() {
        let m = MatchSpec {
            hostname: Some(OneOrMany::One("x".into())),
            ..Default::default()
        };
        assert_eq!(
            Vocabulary::new(vec![t("ubuntu", m.clone())]).unwrap_err(),
            VocabError::ShadowsBuiltin("ubuntu".into())
        );
        assert_eq!(
            Vocabulary::new(vec![t("work", m.clone()), t("work", m.clone())]).unwrap_err(),
            VocabError::Duplicate("work".into())
        );
        assert_eq!(
            Vocabulary::new(vec![t("everything", MatchSpec::default())]).unwrap_err(),
            VocabError::EmptyMatch("everything".into())
        );
    }

    #[test]
    fn classification_keeps_declaration_order() {
        let m = |h: &str| MatchSpec {
            hostname: Some(OneOrMany::One(h.into())),
            ..Default::default()
        };
        let v = Vocabulary::new(vec![t("work", m("khaymah")), t("laptop", m("khaymah"))]).unwrap();
        assert_eq!(v.classify("work"), Some(ArmKind::Declared(0)));
        assert_eq!(v.classify("laptop"), Some(ArmKind::Declared(1)));
        assert!(matches!(v.classify("ubuntu"), Some(ArmKind::Builtin(_))));
        assert_eq!(v.classify("nope"), None);
    }
}
