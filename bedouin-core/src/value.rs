//! Conditional values.
//!
//! One sentence: **a YAML mapping where a value is expected means branches;
//! anything else is the literal value.** There is no `Value` keyword, no
//! `select:`, no `when:`, no `matcher:` -- the shape of the YAML carries the
//! meaning, so the common case stays one line and the conditional case stays
//! four.
//!
//! `Deserialize` is hand-written rather than `#[serde(untagged)]`. Untagged
//! enums emit `data did not match any variant of untagged enum`, and a config
//! tool's error messages are its user interface.

use crate::arm::{self, ArmName};
use crate::facts::Facts;
use crate::target::{ArmKind, Vocabulary};
use serde::de::{self, MapAccess, SeqAccess, Visitor};
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use std::cell::RefCell;
use std::collections::BTreeSet;
use std::fmt;
use std::sync::Arc;

// ---------------------------------------------------------------- known arms

thread_local! {
    static KNOWN: RefCell<Option<Arc<BTreeSet<String>>>> = const { RefCell::new(None) };
}

struct KnownGuard(Option<Arc<BTreeSet<String>>>);

impl Drop for KnownGuard {
    fn drop(&mut self) {
        KNOWN.with(|k| *k.borrow_mut() = self.0.take());
    }
}

/// Run `f` with `names` in scope as the legal arm keys.
///
/// Name validation has to happen *inside* deserialization, while the
/// `file:line:col` of the offending key is still available -- which is why the
/// known set is threaded through a thread-local rather than passed as an
/// argument serde has no way to carry.
pub fn with_known_arms<R>(names: Arc<BTreeSet<String>>, f: impl FnOnce() -> R) -> R {
    let prev = KNOWN.with(|k| k.borrow_mut().replace(names));
    let _restore = KnownGuard(prev);
    f()
}

fn arm_is_known(name: &str) -> bool {
    KNOWN.with(|k| match &*k.borrow() {
        Some(set) => set.contains(name),
        // No scope established: built-ins only. Keeps unit tests honest without
        // making every one of them set up a vocabulary.
        None => arm::builtin(name).is_some(),
    })
}

fn known_names() -> Vec<String> {
    KNOWN.with(|k| match &*k.borrow() {
        Some(set) => set.iter().cloned().collect(),
        None => arm::builtins().keys().cloned().collect(),
    })
}

/// Keys that were considered and deliberately rejected. Reporting them as
/// merely "unknown" would leave the user guessing; each names its replacement.
fn rejected_key_hint(key: &str) -> Option<&'static str> {
    Some(match key {
        "fromEnv" | "from_env" => {
            "`fromEnv` is not a key. The environment is a fact: write \
             `\"{{ env.NAME | default('latest') }}\"`"
        }
        "fromScript" | "from_script" => {
            "`fromScript` is not a key. Nothing the config supplies runs during \
             `plan`: on a fresh machine the script runs before Bedouin has \
             installed anything, so its fallback would always be the real value. \
             Pin the version, or use `{{ env.NAME }}`"
        }
        "script" | "exitCode" | "exit_code" => {
            "matcher scripts are not a key. Declare a target under `targets:` \
             instead, and match on facts"
        }
        "matcher" => "`matcher` is not a key. Arm keys are target names directly: `{ macos: brew, default: apt }`",
        "fallback" => "`fallback` is not a key. The catch-all arm is `default:`",
        _ => return None,
    })
}

// ---------------------------------------------------------------- OneOrMany

/// A scalar or a list of them. `from: brew` and `from: [brew, apt]` are both
/// legal and mean the same shape downstream.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(untagged)]
pub enum OneOrMany<T> {
    One(T),
    Many(Vec<T>),
}

impl<T> OneOrMany<T> {
    pub fn iter(&self) -> Box<dyn Iterator<Item = &T> + '_> {
        match self {
            Self::One(t) => Box::new(std::iter::once(t)),
            Self::Many(v) => Box::new(v.iter()),
        }
    }

    pub fn into_vec(self) -> Vec<T> {
        match self {
            Self::One(t) => vec![t],
            Self::Many(v) => v,
        }
    }

    pub fn len(&self) -> usize {
        match self {
            Self::One(_) => 1,
            Self::Many(v) => v.len(),
        }
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

impl<'de, T: serde::de::DeserializeOwned> Deserialize<'de> for OneOrMany<T> {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        struct V<T>(std::marker::PhantomData<T>);

        impl<'de, T: serde::de::DeserializeOwned> Visitor<'de> for V<T> {
            type Value = OneOrMany<T>;

            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("a value or a list of them")
            }

            fn visit_seq<A: SeqAccess<'de>>(self, mut a: A) -> Result<Self::Value, A::Error> {
                let mut v = Vec::new();
                while let Some(item) = a.next_element()? {
                    v.push(item);
                }
                Ok(OneOrMany::Many(v))
            }

            fn visit_map<A: MapAccess<'de>>(self, _: A) -> Result<Self::Value, A::Error> {
                Err(de::Error::custom("expected a value or a list, found a mapping"))
            }

            fn visit_some<D: Deserializer<'de>>(self, d: D) -> Result<Self::Value, D::Error> {
                OneOrMany::deserialize(d)
            }

            // Scalars defer to `T`, which is where `Tmpl` applies its rules.
            fn visit_str<E: de::Error>(self, v: &str) -> Result<Self::Value, E> {
                one(yaml_str(v))
            }
            fn visit_i64<E: de::Error>(self, v: i64) -> Result<Self::Value, E> {
                one(yaml_num(v.into()))
            }
            fn visit_u64<E: de::Error>(self, v: u64) -> Result<Self::Value, E> {
                one(yaml_num(v.into()))
            }
            fn visit_f64<E: de::Error>(self, v: f64) -> Result<Self::Value, E> {
                one(yaml_num(v.into()))
            }
            fn visit_bool<E: de::Error>(self, v: bool) -> Result<Self::Value, E> {
                one(serde_yaml_ng::Value::Bool(v))
            }
        }

        fn one<T: serde::de::DeserializeOwned, E: de::Error>(
            v: serde_yaml_ng::Value,
        ) -> Result<OneOrMany<T>, E> {
            T::deserialize(v).map(OneOrMany::One).map_err(de::Error::custom)
        }

        d.deserialize_any(V::<T>(std::marker::PhantomData))
    }
}

fn yaml_str(s: &str) -> serde_yaml_ng::Value {
    serde_yaml_ng::Value::String(s.to_owned())
}

fn yaml_num(n: serde_yaml_ng::Number) -> serde_yaml_ng::Value {
    serde_yaml_ng::Value::Number(n)
}

// --------------------------------------------------------------------- Tmpl

/// Raw minijinja source, rendered later against frozen facts and vars.
///
/// The coercion rules live here rather than on `Value` so they apply on both
/// the literal path *and* every arm payload -- revision 1 of the spec put them
/// on the literal path only, which let `{macos: 1.80, default: latest}`
/// through.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
pub struct Tmpl(pub String);

impl Tmpl {
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Whether rendering could change this. A literal needs no template pass.
    pub fn is_literal(&self) -> bool {
        !self.0.contains("{{") && !self.0.contains("{%")
    }
}

impl From<&str> for Tmpl {
    fn from(s: &str) -> Self {
        Tmpl(s.to_owned())
    }
}

impl fmt::Display for Tmpl {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

const UNQUOTED_NUMBER: &str = "expected a string, found an unquoted number. \
Quote version numbers -- YAML reads 1.80 as the float 1.8, and by the time \
Bedouin sees it the trailing zero is gone, so it would install 1.8 instead";

impl<'de> Deserialize<'de> for Tmpl {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        struct V;

        impl Visitor<'_> for V {
            type Value = Tmpl;

            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("a string, optionally containing {{ ... }}")
            }

            fn visit_str<E: de::Error>(self, s: &str) -> Result<Tmpl, E> {
                Ok(Tmpl(s.to_owned()))
            }

            // Integers are unambiguous: `version: 3` survives the round trip.
            fn visit_i64<E: de::Error>(self, v: i64) -> Result<Tmpl, E> {
                Ok(Tmpl(v.to_string()))
            }

            fn visit_u64<E: de::Error>(self, v: u64) -> Result<Tmpl, E> {
                Ok(Tmpl(v.to_string()))
            }

            fn visit_f64<E: de::Error>(self, _: f64) -> Result<Tmpl, E> {
                Err(E::custom(UNQUOTED_NUMBER))
            }

            fn visit_bool<E: de::Error>(self, _: bool) -> Result<Tmpl, E> {
                Err(E::custom(
                    "expected a string, found a boolean. Quote it if you meant \
                     the text `true` or `false`",
                ))
            }
        }

        d.deserialize_any(V)
    }
}

// -------------------------------------------------------------------- Value

/// A value that may vary by machine.
///
/// Depth is 1 by construction: the arm payload is `T`, never `Value<T>`, so
/// nested conditionals are unrepresentable rather than forbidden.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Value<T> {
    Const(T),
    ByTarget {
        arms: Vec<(ArmName, T)>,
        default: Option<T>,
    },
}

impl<T> Value<T> {
    pub fn constant(t: T) -> Self {
        Self::Const(t)
    }

    /// Every payload, whether or not it can ever win. Used by the renderer and
    /// by validation, never by selection.
    pub fn payloads(&self) -> impl Iterator<Item = &T> {
        match self {
            Self::Const(t) => vec![t].into_iter(),
            Self::ByTarget { arms, default } => arms
                .iter()
                .map(|(_, t)| t)
                .chain(default.iter())
                .collect::<Vec<_>>()
                .into_iter(),
        }
    }

    pub fn is_conditional(&self) -> bool {
        matches!(self, Self::ByTarget { .. })
    }
}

/// Why a value could not be reduced to one payload.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SelectError {
    /// No arm matched and there is no `default:`. A missing default means the
    /// author did not decide -- which is different from `only:`, where the
    /// author decided the item does not exist here.
    NoMatch {
        arms: Vec<String>,
        active: Vec<String>,
    },
}

impl fmt::Display for SelectError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NoMatch { arms, active } => {
                write!(
                    f,
                    "no arm matches this machine and there is no `default:`.\n  \
                     arms declared: {}\n  \
                     arms true here: {}\n  \
                     Add a `default:`, or `only:` on the item if it should not exist on this machine",
                    arms.join(", "),
                    if active.is_empty() {
                        "none".to_string()
                    } else {
                        active.join(", ")
                    }
                )
            }
        }
    }
}

/// What won, for `plan -v` and for the state file's `resolved_from`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Winner {
    Literal,
    Arm(String),
    Default,
}

impl fmt::Display for Winner {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Literal => f.write_str("literal"),
            Self::Arm(a) => write!(f, "{a}"),
            Self::Default => f.write_str("default"),
        }
    }
}

impl<T> Value<T> {
    /// Reduce to the winning payload.
    ///
    /// Three rules, in order:
    ///   1. a declared target beats every built-in;
    ///   2. among declared targets, declaration order wins;
    ///   3. among built-ins, the arm whose implied fact set strictly contains
    ///      the others wins.
    ///
    /// Rule 3 relies on [`validate_arms`] having rejected incomparable arms
    /// that can co-occur, which is what makes the winner unique.
    pub fn select(&self, vocab: &Vocabulary, facts: &Facts) -> Result<(&T, Winner), SelectError> {
        let (arms, default) = match self {
            Self::Const(t) => return Ok((t, Winner::Literal)),
            Self::ByTarget { arms, default } => (arms, default),
        };

        let active: Vec<(&ArmName, &T, ArmKind)> = arms
            .iter()
            .filter_map(|(name, payload)| {
                let kind = vocab.classify(name.as_str())?;
                vocab
                    .matches(name.as_str(), facts)
                    .then_some((name, payload, kind))
            })
            .collect();

        // Rule 1 and 2: any declared target wins, earliest declaration first.
        let declared = active
            .iter()
            .filter_map(|(n, p, k)| match k {
                ArmKind::Declared(i) => Some((*i, *n, *p)),
                ArmKind::Builtin(_) => None,
            })
            .min_by_key(|(i, _, _)| *i);
        if let Some((_, name, payload)) = declared {
            return Ok((payload, Winner::Arm(name.0.clone())));
        }

        // Rule 3: the unique most specific built-in.
        let builtins: Vec<_> = active
            .iter()
            .filter_map(|(n, p, k)| match k {
                ArmKind::Builtin(i) => Some((*n, *p, *i)),
                ArmKind::Declared(_) => None,
            })
            .collect();
        if let Some((name, payload, _)) = builtins
            .iter()
            .find(|(_, _, a)| builtins.iter().all(|(_, _, b)| a == b || a.refines(b)))
        {
            return Ok((payload, Winner::Arm(name.0.clone())));
        }

        match default {
            Some(d) => Ok((d, Winner::Default)),
            None => Err(SelectError::NoMatch {
                arms: arms.iter().map(|(n, _)| n.0.clone()).collect(),
                active: active.iter().map(|(n, _, _)| n.0.clone()).collect(),
            }),
        }
    }
}

/// An ambiguity that must be reported at parse time, not discovered on the one
/// machine where both arms happen to be true.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AmbiguousArms {
    pub a: String,
    pub b: String,
    /// The conjunction that resolves it, when the vocabulary has one.
    pub conjunction: Option<String>,
}

impl fmt::Display for AmbiguousArms {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "arms `{}` and `{}` can both be true on one machine and neither is \
             more specific, so which one wins would be arbitrary",
            self.a, self.b
        )?;
        match &self.conjunction {
            Some(c) => write!(f, ".\n  Write `{c}:` for the machine where both hold"),
            None => f.write_str(
                ".\n  There is no built-in name for that combination -- declare a \
                 target under `targets:` that pins both",
            ),
        }
    }
}

/// Reject arm pairs that are incomparable yet can co-occur.
///
/// This is what a closed vocabulary buys beyond typo-catching: co-occurrence is
/// decidable over built-ins, so the ambiguity is caught on the machine you are
/// sitting at rather than on the one you ship to. Declared targets are exempt --
/// their predicates are open, co-occurrence is undecidable in general, and rule
/// 2 gives them a total order by declaration anyway.
pub fn validate_arms<T>(value: &Value<T>, vocab: &Vocabulary) -> Result<(), AmbiguousArms> {
    let Value::ByTarget { arms, .. } = value else {
        return Ok(());
    };
    let builtins: Vec<_> = arms
        .iter()
        .filter_map(|(n, _)| match vocab.classify(n.as_str()) {
            Some(ArmKind::Builtin(i)) => Some((n.0.clone(), i)),
            _ => None,
        })
        .collect();

    for (i, (na, a)) in builtins.iter().enumerate() {
        for (nb, b) in builtins.iter().skip(i + 1) {
            if a.refines(b) || b.refines(a) || a == b || !a.can_cooccur(b) {
                continue;
            }
            let want = a.union_with(b);
            let union = arm::builtins()
                .iter()
                .find(|(_, imp)| **imp == want)
                .map(|(n, _)| n.clone());
            return Err(AmbiguousArms {
                a: na.clone(),
                b: nb.clone(),
                conjunction: union,
            });
        }
    }
    Ok(())
}

// ------------------------------------------------------------- Deserialize

impl<'de, T: serde::de::DeserializeOwned> Deserialize<'de> for Value<T> {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        struct V<T>(std::marker::PhantomData<T>);

        impl<'de, T: serde::de::DeserializeOwned> Visitor<'de> for V<T> {
            type Value = Value<T>;

            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("a value, or a mapping of target names to values")
            }

            // A mapping always means arms: `Value<T>` is only ever used where
            // `T` is a scalar or a list, so no literal is map-shaped. Reading
            // it through MapAccess rather than buffering into a YAML mapping
            // is what makes a repeated arm key visible instead of collapsed.
            fn visit_map<A: MapAccess<'de>>(self, mut m: A) -> Result<Self::Value, A::Error> {
                let mut arms: Vec<(ArmName, T)> = Vec::new();
                let mut default = None;

                while let Some(key) = m.next_key::<String>()? {
                    if key == "default" {
                        if default.is_some() {
                            return Err(de::Error::custom("`default:` is given twice"));
                        }
                        default = Some(m.next_value()?);
                        continue;
                    }
                    if let Some(hint) = rejected_key_hint(&key) {
                        return Err(de::Error::custom(hint));
                    }
                    if !arm_is_known(&key) {
                        let suggestions = arm::suggest(&key, known_names().into_iter());
                        let tail = if suggestions.is_empty() {
                            String::from(
                                "\n  Arm keys are built-in names or a target you declared under `targets:`",
                            )
                        } else {
                            format!("\n  did you mean: {}?", suggestions.join(", "))
                        };
                        return Err(de::Error::custom(format!("unknown arm `{key}`{tail}")));
                    }
                    if arms.iter().any(|(n, _)| n.as_str() == key) {
                        return Err(de::Error::custom(format!(
                            "arm `{key}` is given twice; the second would silently win"
                        )));
                    }
                    arms.push((ArmName(key), m.next_value()?));
                }

                if arms.is_empty() && default.is_none() {
                    return Err(de::Error::custom(
                        "an empty mapping declares no arms and no `default:`",
                    ));
                }
                Ok(Value::ByTarget { arms, default })
            }

            fn visit_seq<A: SeqAccess<'de>>(self, mut a: A) -> Result<Self::Value, A::Error> {
                let mut items = Vec::new();
                while let Some(item) = a.next_element::<serde_yaml_ng::Value>()? {
                    items.push(item);
                }
                konst(serde_yaml_ng::Value::Sequence(items))
            }

            fn visit_some<D: Deserializer<'de>>(self, d: D) -> Result<Self::Value, D::Error> {
                Value::deserialize(d)
            }

            fn visit_str<E: de::Error>(self, v: &str) -> Result<Self::Value, E> {
                konst(yaml_str(v))
            }
            fn visit_i64<E: de::Error>(self, v: i64) -> Result<Self::Value, E> {
                konst(yaml_num(v.into()))
            }
            fn visit_u64<E: de::Error>(self, v: u64) -> Result<Self::Value, E> {
                konst(yaml_num(v.into()))
            }
            fn visit_f64<E: de::Error>(self, v: f64) -> Result<Self::Value, E> {
                konst(yaml_num(v.into()))
            }
            fn visit_bool<E: de::Error>(self, v: bool) -> Result<Self::Value, E> {
                konst(serde_yaml_ng::Value::Bool(v))
            }
        }

        fn konst<T: serde::de::DeserializeOwned, E: de::Error>(
            v: serde_yaml_ng::Value,
        ) -> Result<Value<T>, E> {
            T::deserialize(v).map(Value::Const).map_err(de::Error::custom)
        }

        d.deserialize_any(V::<T>(std::marker::PhantomData))
    }
}

impl<T: Serialize> Serialize for Value<T> {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeMap;
        match self {
            Self::Const(t) => t.serialize(s),
            Self::ByTarget { arms, default } => {
                let mut m = s.serialize_map(Some(arms.len() + usize::from(default.is_some())))?;
                for (n, v) in arms {
                    m.serialize_entry(n.as_str(), v)?;
                }
                if let Some(d) = default {
                    m.serialize_entry("default", d)?;
                }
                m.end()
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::facts::{Arch, Distro, Os};
    use crate::target::{MatchSpec, Target};

    fn vocab(targets: Vec<Target>) -> Vocabulary {
        Vocabulary::new(targets).expect("test vocabulary is well formed")
    }

    fn target(name: &str, m: MatchSpec) -> Target {
        Target {
            name: name.into(),
            r#match: m,
        }
    }

    fn parse(yaml: &str, v: &Vocabulary) -> Result<Value<Tmpl>, String> {
        let names: Arc<BTreeSet<String>> = Arc::new(v.all_names().collect());
        with_known_arms(names, || {
            serde_yaml_ng::from_str::<Value<Tmpl>>(yaml).map_err(|e| e.to_string())
        })
    }

    fn parse_list(yaml: &str, v: &Vocabulary) -> Result<Value<OneOrMany<Tmpl>>, String> {
        let names: Arc<BTreeSet<String>> = Arc::new(v.all_names().collect());
        with_known_arms(names, || {
            serde_yaml_ng::from_str::<Value<OneOrMany<Tmpl>>>(yaml).map_err(|e| e.to_string())
        })
    }

    fn won(v: &Value<Tmpl>, vc: &Vocabulary, f: &Facts) -> String {
        v.select(vc, f).expect("selection succeeds").0 .0.clone()
    }

    // ------------------------------------------------------------ shape

    #[test]
    fn a_scalar_is_a_literal_and_a_mapping_is_branches() {
        let v = vocab(vec![]);
        assert_eq!(parse("latest", &v).unwrap(), Value::Const(Tmpl::from("latest")));
        assert!(parse("{ macos: brew, default: apt }", &v)
            .unwrap()
            .is_conditional());
    }

    #[test]
    fn a_list_stays_a_literal_value() {
        // `from: [brew, apt, zypper]` is a fallback order, not branches.
        let v = vocab(vec![]);
        let parsed = parse_list("[brew, apt, zypper]", &v).unwrap();
        match parsed {
            Value::Const(OneOrMany::Many(items)) => assert_eq!(items.len(), 3),
            other => panic!("expected a literal list, got {other:?}"),
        }
        // ...and a list is still just a payload inside an arm.
        let branched = parse_list("{ macos: brew, default: [apt, zypper] }", &v).unwrap();
        assert!(branched.is_conditional());
    }

    // ------------------------------------------------------------ errors

    #[test]
    fn a_misspelled_arm_names_the_arm_it_meant() {
        let v = vocab(vec![]);
        let err = parse("{ mcaos: nightly, default: stable }", &v).unwrap_err();
        assert!(err.contains("unknown arm `mcaos`"), "{err}");
        assert!(err.contains("macos"), "should suggest the near miss: {err}");
        // Never serde's untagged message, which is what a derived impl gives.
        assert!(!err.contains("did not match any variant"), "{err}");
    }

    #[test]
    fn a_declared_target_is_a_legal_arm_key_and_an_undeclared_one_is_not() {
        let m = MatchSpec {
            hostname: Some(OneOrMany::One("khaymah".into())),
            ..Default::default()
        };
        let v = vocab(vec![target("laptop", m)]);
        assert!(parse("{ laptop: nightly, default: stable }", &v).is_ok());
        let err = parse("{ desktop: nightly, default: stable }", &v).unwrap_err();
        assert!(err.contains("unknown arm `desktop`"), "{err}");
    }

    #[test]
    fn rejected_keys_name_their_replacement_rather_than_reading_as_unknown() {
        let v = vocab(vec![]);
        let env = parse("{ fromEnv: ZELLIJ_VERSION, fallback: latest }", &v).unwrap_err();
        assert!(env.contains("env.NAME"), "must point at the replacement: {env}");

        let script = parse("{ fromScript: \"doctor determine-version\" }", &v).unwrap_err();
        assert!(
            script.contains("fresh machine") || script.contains("before Bedouin"),
            "must explain the ordering reason, not just refuse: {script}"
        );

        let matcher = parse("{ matcher: default, value: latest }", &v).unwrap_err();
        assert!(matcher.contains("target names directly"), "{matcher}");
    }

    #[test]
    fn an_unquoted_version_is_refused_on_both_paths() {
        // YAML reads 1.80 as 1.8, and the trailing zero is gone before Bedouin
        // sees it. Revision 1 of the spec caught this only on the literal path.
        let v = vocab(vec![]);
        let literal = parse("1.80", &v).unwrap_err();
        assert!(literal.contains("Quote version numbers"), "{literal}");

        let in_arm = parse("{ macos: 1.80, default: latest }", &v).unwrap_err();
        assert!(
            in_arm.contains("Quote version numbers"),
            "an arm payload gets the same rule: {in_arm}"
        );

        // Integers are unambiguous and survive.
        assert_eq!(parse("3", &v).unwrap(), Value::Const(Tmpl::from("3")));
    }

    #[test]
    fn yaml_booleans_are_refused_where_a_string_is_meant() {
        let v = vocab(vec![]);
        // YAML 1.2 reads only `true`/`false` as booleans -- `on` and `no` stay
        // strings, so they need no special handling.
        let err = parse("{ macos: true, default: latest }", &v).unwrap_err();
        assert!(err.contains("boolean"), "{err}");
        assert_eq!(
            parse("{ macos: on, default: latest }", &v).unwrap(),
            Value::ByTarget {
                arms: vec![(ArmName::from("macos"), Tmpl::from("on"))],
                default: Some(Tmpl::from("latest")),
            }
        );
    }

    #[test]
    fn duplicate_arms_and_empty_mappings_are_refused() {
        let v = vocab(vec![]);
        assert!(parse("{ macos: a, macos: b }", &v)
            .unwrap_err()
            .contains("twice"));
        assert!(parse("{}", &v).unwrap_err().contains("no arms"));
    }

    // --------------------------------------------------------- selection

    #[test]
    fn a_refining_arm_beats_the_arm_it_refines() {
        // The bug that made revision 1 unusable. "apt on Ubuntu, cargo on other
        // Linuxes" was a parse error with no expressible fix.
        let v = vocab(vec![]);
        let val = parse("{ ubuntu: apt, linux: cargo, default: brew }", &v).unwrap();
        assert!(validate_arms(&val, &v).is_ok(), "this must not be ambiguous");

        let ubuntu = Facts::fixture(Os::Linux, Distro::Ubuntu, Arch::X86_64);
        assert_eq!(won(&val, &v, &ubuntu), "apt");

        let fedora = Facts::fixture(Os::Linux, Distro::Fedora, Arch::X86_64);
        assert_eq!(won(&val, &v, &fedora), "cargo");

        let mac = Facts::fixture(Os::Macos, Distro::Macos, Arch::Arm64);
        assert_eq!(won(&val, &v, &mac), "brew");
    }

    #[test]
    fn written_order_of_arms_never_changes_the_answer() {
        let v = vocab(vec![]);
        let f = Facts::fixture(Os::Macos, Distro::Macos, Arch::Arm64);
        // Under first-match-wins the `macos-arm64` arm would be unreachable on
        // every machine, and sorting the file would silently change meaning.
        let a = parse("{ macos: stable, macos-arm64: nightly, default: x }", &v).unwrap();
        let b = parse("{ macos-arm64: nightly, macos: stable, default: x }", &v).unwrap();
        assert_eq!(won(&a, &v, &f), "nightly");
        assert_eq!(won(&b, &v, &f), "nightly");
    }

    #[test]
    fn a_declared_target_beats_every_builtin() {
        let work = MatchSpec {
            env: Some([("BEDOUIN_PROFILE".to_string(), "work".to_string())].into()),
            ..Default::default()
        };
        let v = vocab(vec![target("work", work)]);
        let val = parse("{ work: \"0.9.5\", ubuntu: apt, default: latest }", &v).unwrap();

        let mut f = Facts::fixture(Os::Linux, Distro::Ubuntu, Arch::X86_64);
        assert_eq!(won(&val, &v, &f), "apt", "target inactive, builtin wins");
        f.env.insert("BEDOUIN_PROFILE".into(), "work".into());
        assert_eq!(won(&val, &v, &f), "0.9.5", "you named it, so you meant it");
    }

    #[test]
    fn among_declared_targets_declaration_order_decides() {
        // Co-occurrence of open predicates is undecidable, so Bedouin does not
        // pretend to decide it -- it uses the order written at the top of the
        // file, which also restores the handoff's first-match-wins.
        let host = MatchSpec {
            hostname: Some(OneOrMany::One("khaymah".into())),
            ..Default::default()
        };
        let env = MatchSpec {
            env: Some([("P".to_string(), "1".to_string())].into()),
            ..Default::default()
        };
        let first = vocab(vec![
            target("laptop", host.clone()),
            target("work", env.clone()),
        ]);
        let second = vocab(vec![target("work", env), target("laptop", host)]);

        let mut f = Facts::fixture(Os::Linux, Distro::Ubuntu, Arch::X86_64);
        f.env.insert("P".into(), "1".into()); // both targets now active

        assert_eq!(
            won(&parse("{ laptop: a, work: b }", &first).unwrap(), &first, &f),
            "a"
        );
        assert_eq!(
            won(&parse("{ laptop: a, work: b }", &second).unwrap(), &second, &f),
            "b"
        );
    }

    #[test]
    fn a_missing_default_is_an_error_that_names_what_was_available() {
        // "the author did not decide" must not look like "the author decided
        // this item does not exist here" -- that is what `only:` is for.
        let v = vocab(vec![]);
        let val = parse("{ macos: latest }", &v).unwrap();
        let linux = Facts::fixture(Os::Linux, Distro::Ubuntu, Arch::X86_64);
        let err = val.select(&v, &linux).unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("no `default:`"), "{msg}");
        assert!(msg.contains("macos"), "names the arms declared: {msg}");
        assert!(msg.contains("only:"), "points at the other tool: {msg}");
    }

    #[test]
    fn the_winner_is_reported_for_plan_v_and_the_state_file() {
        let v = vocab(vec![]);
        let f = Facts::fixture(Os::Linux, Distro::Ubuntu, Arch::Arm64);
        let val = parse("{ ubuntu: apt, default: brew }", &v).unwrap();
        assert_eq!(val.select(&v, &f).unwrap().1, Winner::Arm("ubuntu".into()));

        let plain = parse("latest", &v).unwrap();
        assert_eq!(plain.select(&v, &f).unwrap().1, Winner::Literal);

        let fallback = parse("{ macos: x, default: brew }", &v).unwrap();
        assert_eq!(fallback.select(&v, &f).unwrap().1, Winner::Default);
    }

    // -------------------------------------------------------- ambiguity

    #[test]
    fn orthogonal_arms_are_rejected_at_parse_time_with_the_conjunction() {
        let v = vocab(vec![]);
        let val = parse("{ macos: \"1.80\", arm64: nightly, default: stable }", &v).unwrap();
        let err = validate_arms(&val, &v).unwrap_err();
        assert_eq!(err.conjunction.as_deref(), Some("macos-arm64"));
        assert!(err.to_string().contains("macos-arm64"), "{err}");
    }

    #[test]
    fn the_suggested_conjunction_never_narrows_what_the_user_wrote() {
        // `debian-like` pins two facts and `arm64` one, so a size rule would
        // pick debian-like silently. The conjunction must pin exactly the union:
        // `ubuntu-arm64` also contains both, but suggesting it would quietly
        // drop Debian from an arm the user wrote for the whole family.
        let v = vocab(vec![]);
        let val = parse("{ debian-like: apt, arm64: cargo, default: brew }", &v).unwrap();
        let err = validate_arms(&val, &v).unwrap_err();
        assert_eq!(err.conjunction.as_deref(), Some("debian-like-arm64"));
        assert!(!err.to_string().contains("ubuntu"), "must not narrow: {err}");
    }

    #[test]
    fn arms_that_can_never_co_occur_are_not_ambiguous() {
        let v = vocab(vec![]);
        for yaml in [
            "{ macos: a, ubuntu: b, default: c }",   // different os
            "{ ubuntu: a, fedora: b, default: c }",  // different distro
            "{ ubuntu: a, linux: b, default: c }",   // comparable
            "{ x86_64: a, arm64: b, default: c }",   // different arch
        ] {
            let val = parse(yaml, &v).unwrap();
            assert!(validate_arms(&val, &v).is_ok(), "{yaml} should be fine");
        }
    }

    #[test]
    fn declared_targets_are_exempt_from_the_ambiguity_check() {
        // Their predicates are open, so co-occurrence is undecidable; rule 2
        // gives them a total order by declaration instead.
        let m = |h: &str| MatchSpec {
            hostname: Some(OneOrMany::One(h.into())),
            ..Default::default()
        };
        let v = vocab(vec![target("a", m("h1")), target("b", m("h2"))]);
        let val = parse("{ a: x, b: y, default: z }", &v).unwrap();
        assert!(validate_arms(&val, &v).is_ok());
    }
}
