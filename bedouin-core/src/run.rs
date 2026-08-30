//! Wiring the stages together, so the CLI and the tests drive the same path.

use crate::facts::{Arch, Facts, Os, Shell};
use crate::host::Host;
use crate::loader::{self, Loaded};
use crate::plan::{self, Plan};
use crate::probe;
use crate::schema::{self, Config, ConfigError, Result};
use crate::state::{self, State};
use std::path::{Path, PathBuf};

#[derive(Debug)]
pub struct Outcome {
    pub loaded: Loaded,
    pub facts: Facts,
    pub config: Config,
    pub state: State,
    pub plan: Plan,
}

/// Resolve facts, load and resolve the config, diff against state, build a plan.
///
/// Nothing here mutates the machine.
pub fn plan(host: &dyn Host, explicit: Option<&Path>, cwd: &Path) -> Result<Outcome> {
    let (os, arch) = probe::host_platform();
    plan_for(host, explicit, cwd, os, arch)
}

/// As [`plan`], for a stated platform. Lets a test drive a fresh macOS from a
/// Linux runner.
pub fn plan_for(
    host: &dyn Host,
    explicit: Option<&Path>,
    cwd: &Path,
    os: Os,
    arch: Arch,
) -> Result<Outcome> {
    let home = PathBuf::from(
        host.env()
            .get("HOME")
            .ok_or_else(|| ConfigError::new("$HOME is not set, so there is no home to configure"))?,
    );

    let entry = loader::locate(explicit, host, cwd, &home)?;
    let loaded = loader::load(&entry, host)?;

    // The declared shell has to reach fact resolution, because `shell.rc_dir`
    // and the PATH file name hang off it -- and on a fresh box the detected
    // shell is usually the one being replaced.
    let declared = match &loaded.raw.shell {
        None => None,
        Some(s) => Some(Shell::parse(s).ok_or_else(|| {
            ConfigError::new(format!("`shell: {s}` is not a shell Bedouin knows"))
        })?),
    };
    let facts = probe::facts_for(host, declared, os, arch)?;
    let config = schema::resolve(&loaded.raw, &loaded.vocab, &facts)?;
    let state = state::load(host, &state::default_path(&facts.home))?;
    let plan = plan::build(&config, &facts, &state, host, &loaded.root)?;

    Ok(Outcome {
        loaded,
        facts,
        config,
        state,
        plan,
    })
}
