//! Drift: the difference between what Bedouin last wrote and what is there now.
//!
//! Everything here is read-only. It works by comparing the hashes the executor
//! records against what the machine actually holds, which is why those hashes
//! were worth recording even before anything read them.
//!
//! Two kinds of drift matter and they are not the same. **Edited** means
//! someone changed managed content by hand -- their edit is real and Bedouin
//! would overwrite it on the next apply. **Resolved differently** means the
//! config now picks a different arm than it did last time: nothing was edited,
//! but the machine moved, and a conditional config otherwise makes that
//! invisible.

use crate::facts::Facts;
use crate::host::Host;
use crate::schema::{ConfigError, Config, Result};
use crate::state::{ItemKind, Owner, State};
use crate::writers;
use std::path::Path;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Drift {
    /// Managed content was changed by hand. The next apply would overwrite it.
    Edited { id: String, file: String },
    /// Bedouin wrote it and it is not there any more.
    Missing { id: String, file: String },
    /// The config resolves to a different arm than it did last apply.
    Resolved {
        id: String,
        field: String,
        was: String,
        now: String,
    },
    /// A block Bedouin owns was opened and never closed, so nothing can safely
    /// rewrite that file.
    Unterminated { id: String, file: String, why: String },
    /// A step recorded its intent and never finished. The next apply redoes it.
    Incomplete { id: String },
}

impl Drift {
    pub fn id(&self) -> &str {
        match self {
            Self::Edited { id, .. }
            | Self::Missing { id, .. }
            | Self::Resolved { id, .. }
            | Self::Unterminated { id, .. }
            | Self::Incomplete { id } => id,
        }
    }
}

impl std::fmt::Display for Drift {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Edited { id, file } => write!(
                f,
                "  ~ {id}\n      {file} was edited by hand; `apply` would overwrite it"
            ),
            Self::Missing { id, file } => write!(
                f,
                "  - {id}\n      {file} is gone; `apply` would put it back"
            ),
            Self::Resolved { id, field, was, now } => write!(
                f,
                "  ! {id}\n      `{field}` resolved to `{was}` last apply and to `{now}` now"
            ),
            Self::Unterminated { id, file, why } => write!(
                f,
                "  x {id}\n      {file}: {why}"
            ),
            Self::Incomplete { id } => write!(
                f,
                "  ! {id}\n      a previous run started this and did not finish; \
                 `apply` will redo it"
            ),
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct Report {
    pub drift: Vec<Drift>,
    pub checked: usize,
    /// Items Bedouin adopted rather than installed. Reported because they look
    /// managed and are not.
    pub preexisting: Vec<String>,
}

impl Report {
    pub fn is_clean(&self) -> bool {
        self.drift.is_empty()
    }

    /// 0 clean, 2 drift, mirroring `plan` so a CI check can read the status.
    pub fn exit_code(&self) -> i32 {
        i32::from(!self.is_clean()) * 2
    }

    pub fn render(&self, verbose: bool) -> String {
        let mut out = String::new();
        if self.is_clean() {
            out.push_str(&format!(
                "No drift. {} managed items match what bedouin last wrote.\n",
                self.checked
            ));
        } else {
            out.push_str(&format!("Drift in {} of {} items:\n\n", self.drift.len(), self.checked));
            for d in &self.drift {
                out.push_str(&format!("{d}\n"));
            }
            out.push_str("\nRun `bedouin plan` to see what apply would do about it.\n");
        }
        if verbose && !self.preexisting.is_empty() {
            out.push_str("\nAdopted, not installed by bedouin (never removed):\n");
            for p in &self.preexisting {
                out.push_str(&format!("  {p}\n"));
            }
        }
        out
    }
}

/// Compare what state records against what the machine holds.
pub fn check(state: &State, cfg: &Config, facts: &Facts, host: &dyn Host) -> Result<Report> {
    let mut report = Report::default();

    for (id, item) in &state.items {
        if item.owner == Owner::Preexisting {
            report.preexisting.push(id.clone());
            continue;
        }
        // An item left `incomplete` by a failed or interrupted run is not
        // clean, whatever its hashes say -- and doctor exiting 0 while `plan`
        // exits 2 is the two of them disagreeing about the same machine.
        if item.status == crate::state::Status::Incomplete {
            report.drift.push(Drift::Incomplete { id: id.clone() });
            report.checked += 1;
            continue;
        }

        // Only items whose content doctor can actually verify are counted;
        // otherwise "N managed items match what bedouin last wrote" claims more
        // than was checked. A package's presence is `plan`'s question.
        if item.hash.is_none() && item.rc_blocks.is_empty() {
            continue;
        }
        report.checked += 1;

        // A block inside a file: compare only what lies between the markers, so
        // the user's own edits elsewhere in their rc file are not drift.
        for block in &item.rc_blocks {
            let Some(bytes) = host
                .read(Path::new(&block.file))
                .map_err(|e| ConfigError::new(e.to_string()))?
            else {
                report.drift.push(Drift::Missing {
                    id: id.clone(),
                    file: block.file.clone(),
                });
                continue;
            };
            let text = String::from_utf8_lossy(&bytes);
            match writers::extract_block(&text, &block.marker) {
                Err(e) => report.drift.push(Drift::Unterminated {
                    id: id.clone(),
                    file: block.file.clone(),
                    why: e.to_string(),
                }),
                Ok(None) => report.drift.push(Drift::Missing {
                    id: id.clone(),
                    file: block.file.clone(),
                }),
                Ok(Some(found)) if writers::block_digest(&found) != block.hash => {
                    report.drift.push(Drift::Edited {
                        id: id.clone(),
                        file: block.file.clone(),
                    });
                }
                Ok(Some(_)) => {}
            }
        }

        // A file Bedouin wholly owns: the whole thing is the managed content.
        if item.rc_blocks.is_empty() {
            if let (Some(hash), Some(file)) = (&item.hash, item.owned_files.first()) {
                match host
                    .read(Path::new(file))
                    .map_err(|e| ConfigError::new(e.to_string()))?
                {
                    None => report.drift.push(Drift::Missing {
                        id: id.clone(),
                        file: file.clone(),
                    }),
                    Some(bytes) => {
                        let text = String::from_utf8_lossy(&bytes);
                        if writers::digest(&text) != *hash {
                            report.drift.push(Drift::Edited {
                                id: id.clone(),
                                file: file.clone(),
                            });
                        }
                    }
                }
            }
        }
    }

    // Which arm won, last time versus now. Nothing was edited and nothing is
    // missing -- the machine moved, and a conditional config makes that
    // invisible without this.
    let now = resolutions(cfg);
    for (id, item) in &state.items {
        for (field, was) in &item.resolved_from {
            // `Winner::Literal` is not an arm; comparing its display form
            // ("literal") against a real arm name invented permanent drift for
            // every field that is simply not conditional.
            if matches!(was, crate::value::Winner::Literal) {
                continue;
            }
            if let Some(current) = now.get(&(id.clone(), field.clone())) {
                let was = was.to_string();
                if *current != was {
                    report.drift.push(Drift::Resolved {
                        id: id.clone(),
                        field: field.clone(),
                        was,
                        now: current.clone(),
                    });
                }
            }
        }
    }
    let _ = facts;

    report.drift.sort_by(|a, b| a.id().cmp(b.id()));
    Ok(report)
}

/// Which arm each conditional field resolves to under the config as it stands.
fn resolutions(cfg: &Config) -> std::collections::BTreeMap<(String, String), String> {
    let mut out = std::collections::BTreeMap::new();
    for p in &cfg.packages {
        for (field, winner) in &p.resolved_from {
            if matches!(winner, crate::value::Winner::Literal) {
                continue;
            }
            out.insert(
                (format!("package/{}", p.name), field.clone()),
                winner.to_string(),
            );
        }
    }
    for l in &cfg.languages {
        for (field, winner) in &l.resolved_from {
            if matches!(winner, crate::value::Winner::Literal) {
                continue;
            }
            out.insert(
                (format!("language/{}", l.name), field.clone()),
                winner.to_string(),
            );
        }
    }
    out
}

/// The kinds `doctor` can speak about, for a `--only` filter later.
pub fn kinds() -> &'static [ItemKind] {
    &[
        ItemKind::File,
        ItemKind::Rc,
        ItemKind::Path,
        ItemKind::Package,
        ItemKind::Language,
    ]
}
