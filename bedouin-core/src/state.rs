//! The record of what Bedouin did.
//!
//! `owner` is what makes uninstall safe: dropping an entry from the config
//! removes only `owner: bedouin` artifacts, so a `jq` that was already on the
//! machine survives.
//!
//! The durability rules matter more than the shape. Every one of them, absent,
//! degrades to "treat state as empty" -- which re-adopts every managed package
//! as `preexisting` and disables uninstall permanently. A *missing* file is an
//! empty state, because that is a first run; a corrupt one never is.

use crate::host::Host;
use crate::schema::{ConfigError, Provenance, Result};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

pub const SCHEMA_VERSION: u32 = 1;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Owner {
    /// Bedouin installed it, so Bedouin may remove it.
    Bedouin,
    /// It was already here. Adopted, never removed.
    Preexisting,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Status {
    /// Recorded *before* the work begins. An interrupt between "installed" and
    /// "flushed" would otherwise leave a Bedouin package looking preexisting --
    /// permanently un-removable, and silently so.
    Incomplete,
    Complete,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ItemKind {
    Manager,
    Dir,
    Language,
    Package,
    File,
    Rc,
    Path,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RcRecord {
    pub file: String,
    pub marker: String,
    pub hash: String,
    /// Text this block replaced, kept so a drifted block is not lost.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub superseded: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StateItem {
    pub kind: ItemKind,
    pub owner: Owner,
    pub status: Status,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    /// How it was installed, recorded rather than assumed, so a package moving
    /// from apt to cargo is removed and reinstalled rather than double-installed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub method: Option<String>,
    /// What this item contributes to the step environment. Languages and
    /// managers record theirs from the installer recipe, not from user config --
    /// nobody should have to tell Bedouin where rustup puts cargo.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub bin_dirs: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub path: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub rc_blocks: Vec<RcRecord>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hash: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub render_snapshot: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub backup: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mode: Option<String>,
    /// Which arm won for each conditional field, so `doctor` can say "this
    /// resolved differently than last apply".
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub resolved_from: Provenance,
}

impl StateItem {
    pub fn new(kind: ItemKind, owner: Owner) -> Self {
        Self {
            kind,
            owner,
            status: Status::Complete,
            version: None,
            method: None,
            bin_dirs: Vec::new(),
            path: Vec::new(),
            rc_blocks: Vec::new(),
            hash: None,
            render_snapshot: None,
            backup: None,
            mode: None,
            resolved_from: Provenance::new(),
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct State {
    #[serde(default)]
    pub schema_version: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_apply: Option<String>,
    #[serde(default)]
    pub items: BTreeMap<String, StateItem>,
}

impl State {
    pub fn empty() -> Self {
        Self {
            schema_version: SCHEMA_VERSION,
            last_apply: None,
            items: BTreeMap::new(),
        }
    }

    /// Every bin directory any completed item contributes, in item-id order.
    /// This is what the step environment's `PATH` is assembled from.
    pub fn bin_dirs(&self) -> Vec<PathBuf> {
        self.items
            .values()
            .filter(|i| i.status == Status::Complete)
            .flat_map(|i| i.bin_dirs.iter())
            .map(PathBuf::from)
            .collect()
    }

    /// The item as state records it -- but only if the step that wrote it
    /// finished. A `status` the diff ignores is a `status` that does nothing.
    pub fn done(&self, id: &str) -> Option<&StateItem> {
        self.items.get(id).filter(|i| i.status == Status::Complete)
    }

    /// A step recorded its intent and never flipped it. Such an item re-diffs
    /// as needing work (spec 8.3), and the machine probe may not overrule it:
    /// the probe is presence-only and cannot tell a half-install from a whole
    /// one. Re-running an installer is idempotent; skipping a half-install is
    /// a broken machine whose state stays wedged, because a no-op step never
    /// runs and so never flips the status to complete.
    pub fn interrupted(&self, id: &str) -> bool {
        self.items.get(id).is_some_and(|i| i.status == Status::Incomplete)
    }

    pub fn owned_by_bedouin(&self) -> impl Iterator<Item = (&String, &StateItem)> {
        self.items.iter().filter(|(_, i)| i.owner == Owner::Bedouin)
    }
}

pub fn default_path(home: &Path) -> PathBuf {
    home.join(".local/state/bedouin/state.json")
}

/// Read the state file.
///
/// Absent means a first run. Anything else that cannot be understood is a hard
/// error: continuing with an empty state would silently re-adopt every managed
/// item as `preexisting`.
pub fn load(host: &dyn Host, path: &Path) -> Result<State> {
    let Some(bytes) = host
        .read(path)
        .map_err(|e| ConfigError::new(e.to_string()))?
    else {
        return Ok(State::empty());
    };
    let text = String::from_utf8(bytes).map_err(|_| corrupt(path, "not valid UTF-8"))?;
    let state: State =
        serde_json::from_str(&text).map_err(|e| corrupt(path, &format!("cannot be parsed: {e}")))?;
    if state.schema_version > SCHEMA_VERSION {
        return Err(ConfigError::new(format!(
            "state file is version {} but this build understands {}.\n  \
             A newer bedouin wrote it. Upgrade, or move the file aside to start \
             over -- but note that starting over abandons every item bedouin owns:\n  {}",
            state.schema_version,
            SCHEMA_VERSION,
            path.display()
        )));
    }
    Ok(state)
}

fn corrupt(path: &Path, why: &str) -> ConfigError {
    ConfigError::new(format!(
        "the state file {why}.\n  \
         Refusing to continue with an empty state: that would re-adopt every \
         item bedouin installed as pre-existing and disable uninstall for good.\n  \
         Inspect or move aside: {}",
        path.display()
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::host::FakeHost;

    const P: &str = "/home/t/.local/state/bedouin/state.json";

    #[test]
    fn a_missing_state_file_is_a_first_run() {
        let h = FakeHost::new();
        let s = load(&h, Path::new(P)).unwrap();
        assert!(s.items.is_empty());
        assert_eq!(s.schema_version, SCHEMA_VERSION);
    }

    #[test]
    fn a_corrupt_state_file_is_never_an_empty_one() {
        // The failure that erases ownership. It must be loud.
        let h = FakeHost::new().with_file(P, "{ this is not json");
        let err = load(&h, Path::new(P)).unwrap_err();
        assert!(err.message.contains("Refusing to continue"), "{err}");
        assert!(err.message.contains("uninstall"), "{err}");
    }

    #[test]
    fn a_newer_state_file_is_refused_rather_than_downgraded() {
        let h = FakeHost::new().with_file(P, r#"{"schema_version": 99, "items": {}}"#);
        let err = load(&h, Path::new(P)).unwrap_err();
        assert!(err.message.contains("version 99"), "{err}");
    }

    #[test]
    fn bin_dirs_come_from_completed_items_only() {
        let mut s = State::empty();
        let mut rust = StateItem::new(ItemKind::Language, Owner::Bedouin);
        rust.bin_dirs = vec!["/home/t/.cargo/bin".into()];
        s.items.insert("language/rust".into(), rust);

        let mut half = StateItem::new(ItemKind::Manager, Owner::Bedouin);
        half.status = Status::Incomplete;
        half.bin_dirs = vec!["/opt/homebrew/bin".into()];
        s.items.insert("manager/brew".into(), half);

        assert_eq!(s.bin_dirs(), vec![PathBuf::from("/home/t/.cargo/bin")]);
    }

    #[test]
    fn a_state_file_round_trips() {
        let mut s = State::empty();
        let mut z = StateItem::new(ItemKind::Package, Owner::Bedouin);
        z.version = Some("0.40.1".into());
        z.method = Some("cargo".into());
        z.path = vec!["/home/t/.cargo/bin".into()];
        s.items.insert("package/zellij".into(), z);

        let text = serde_json::to_string(&s).unwrap();
        let h = FakeHost::new().with_file(P, &text);
        assert_eq!(load(&h, Path::new(P)).unwrap(), s);
    }
}
