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

/// Write a config, then prove bedouin can still read it.
///
/// A backup, the new text, a re-plan -- and if that plan fails, the original
/// goes back. The alternative is handing someone a config their own tool can
/// no longer load, from an edit their own tool made.
///
/// Shared rather than owned by whoever writes first: `bedouin add`, the TUI's
/// editor and the web UI all mutate this file, and a second copy of this
/// function is a second chance to skip the restore.
pub fn write_verified(
    host: &dyn Host,
    entry: &Path,
    text: &str,
    explicit: Option<&Path>,
    cwd: &Path,
) -> Result<Outcome> {
    let original = host
        .read(entry)
        .map_err(|e| ConfigError::new(e.to_string()))?
        .map(|b| String::from_utf8_lossy(&b).into_owned())
        .ok_or_else(|| ConfigError::new(format!("{} is not there to edit", entry.display())))?;

    // The file's own mode, carried through every write below. `Host::write`
    // always sets one, so picking a number here would quietly re-permission a
    // config that is checked into git -- a mode change in `git status` that
    // the user did not ask for and cannot explain.
    let mode = host
        .symlink_meta(entry)
        .ok()
        .flatten()
        .map(|m| m.mode & 0o7777)
        .unwrap_or(0o644);

    // Written before the change, not after: if this process dies mid-write the
    // previous text is still on disk beside the file.
    let backup = PathBuf::from(format!("{}.bedouin-bak", entry.display()));
    host.write(&backup, original.as_bytes(), mode)
        .map_err(|e| ConfigError::new(format!("{}: {e}", backup.display())))?;
    host.write(entry, text.as_bytes(), mode)
        .map_err(|e| ConfigError::new(format!("{}: {e}", entry.display())))?;

    match plan(host, explicit, cwd) {
        Ok(o) => {
            let _ = host.remove(&backup);
            Ok(o)
        }
        Err(e) => {
            let _ = host.write(entry, original.as_bytes(), mode);
            let _ = host.remove(&backup);
            Err(ConfigError::new(format!(
                "{e}\n  The edit would have left a config bedouin cannot load, so \
                 {} has been restored unchanged.",
                entry.display()
            )))
        }
    }
}

/// The shell a config declares, if it declares one.
fn declared_shell(loaded: &Loaded) -> Result<Option<Shell>> {
    let Some(s) = loaded.raw.shell.as_ref().and_then(|s| s.name.as_ref()) else {
        return Ok(None);
    };
    Shell::parse(s)
        .map(Some)
        .ok_or_else(|| ConfigError::new(format!("`shell: {s}` is not a shell Bedouin knows")))
}

/// Load and resolve facts, but do not resolve the config.
///
/// `bedouin env` exists to diagnose a config that will not resolve -- often
/// *because* a variable is missing -- so it must not need resolution to run.
pub fn load_only(host: &dyn Host, explicit: Option<&Path>, cwd: &Path) -> Result<(Loaded, Facts)> {
    let home =
        PathBuf::from(host.env().get("HOME").ok_or_else(|| {
            ConfigError::new("$HOME is not set, so there is no home to configure")
        })?);
    let entry = loader::locate(explicit, host, cwd, &home)?;
    let loaded = loader::load(&entry, host)?;
    let declared = declared_shell(&loaded)?;
    let (os, arch) = probe::host_platform();
    let mut facts = probe::facts_for(host, declared, os, arch)?;
    for (k, v) in crate::envfile::load(host, &loaded.root)? {
        facts.env.entry(k).or_insert(v);
    }
    Ok((loaded, facts))
}

/// Apply a previously written plan artifact.
///
/// Facts and config come from the artifact, not from this machine: that is
/// what makes the plan you reviewed the plan that runs.
pub fn apply_artifact(
    host: &dyn Host,
    artifact_path: &Path,
    skip: &std::collections::BTreeSet<String>,
    out: &mut dyn FnMut(crate::host::Line),
) -> Result<crate::apply::Report> {
    let a = crate::artifact::read(host, artifact_path)?;
    let (os, arch) = probe::host_platform();
    let live = probe::facts_for(host, Some(a.facts.shell.name), os, arch)?;
    let state = state::load(host, &state::default_path(&a.facts.home))?;
    crate::artifact::check_still_valid(&a, &live, &state)?;

    let plan = plan::build(&a.config, &a.facts, &state, host, &a.config_root)?;
    crate::apply::apply(&plan, &a.config, &a.facts, state, host, skip, out)
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
    let home =
        PathBuf::from(host.env().get("HOME").ok_or_else(|| {
            ConfigError::new("$HOME is not set, so there is no home to configure")
        })?);

    let entry = loader::locate(explicit, host, cwd, &home)?;
    let loaded = loader::load(&entry, host)?;

    // The declared shell has to reach fact resolution, because `shell.rc_dir`
    // and the PATH file name hang off it -- and on a fresh box the detected
    // shell is usually the one being replaced.
    let declared = declared_shell(&loaded)?;
    let mut facts = probe::facts_for(host, declared, os, arch)?;

    // `.env.bedouin` beside the config, if it is there. The process
    // environment wins on a collision: what you exported for this command is
    // more specific than what the file says in general.
    for (k, v) in crate::envfile::load(host, &loaded.root)? {
        facts.env.entry(k).or_insert(v);
    }
    let config = schema::resolve(&loaded.raw, &loaded.vocab, &facts)?;
    let state = state::load(host, &state::default_path(&facts.home))?;
    let mut plan = plan::build(&config, &facts, &state, host, &loaded.root)?;

    // A referenced variable that is unset AND unguarded resolves to nothing
    // useful; saying so at plan time beats failing at apply.
    for r in crate::envfile::referenced(&loaded.raw, &facts.env, &loaded.root, host) {
        if !r.set && !r.has_default {
            plan.warnings.push(format!(
                "`{}` is read by {} but is not set. Set it, give it a \
                 `| default(...)`, or put it in {}",
                r.name,
                r.site,
                crate::envfile::FILE_NAME
            ));
        }
    }

    Ok(Outcome {
        loaded,
        facts,
        config,
        state,
        plan,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::host::OsHost;
    use std::fs;

    /// A real config on a real disk, because this function's whole job is
    /// what is left on disk afterwards. A fake here would test the fake.
    fn fixture(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("bedouin-write-verified-{name}"));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        fs::write(
            dir.join("bedouin.yaml"),
            "version: 0\nshell: bash\npackages:\n  - name: jq\n    from: apt\naliases:\n  ll: ls -alh\n",
        )
        .unwrap();
        dir
    }

    #[test]
    fn a_config_that_will_not_load_is_put_back_exactly_as_it_was() {
        let dir = fixture("restore");
        let entry = dir.join("bedouin.yaml");
        let before = fs::read_to_string(&entry).unwrap();

        let err = write_verified(
            &OsHost::new(),
            &entry,
            "packages:\n  - this is not\n   valid: yaml: at all\n",
            Some(&entry),
            &dir,
        )
        .expect_err("a config bedouin cannot load must not be left on disk");

        assert!(
            err.to_string().contains("restored unchanged"),
            "the error must say the file was put back: {err}"
        );
        assert_eq!(
            fs::read_to_string(&entry).unwrap(),
            before,
            "the original config did not come back"
        );
        assert!(
            !dir.join("bedouin.yaml.bedouin-bak").exists(),
            "the backup was left lying beside the config"
        );
    }

    #[test]
    fn the_files_own_permissions_survive_the_edit() {
        // `Host::write` always sets a mode, so this function has to carry the
        // existing one through. Picking a number instead silently
        // re-permissions a config that is checked into git, which shows up as
        // a mode change the user did not make and cannot explain.
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let dir = fixture("mode");
            let entry = dir.join("bedouin.yaml");
            fs::set_permissions(&entry, fs::Permissions::from_mode(0o644)).unwrap();

            write_verified(
                &OsHost::new(),
                &entry,
                "version: 0\nshell: bash\npackages:\n  - name: jq\n    from: apt\n\
                 aliases:\n  ll: ls -alh\n  gs: git status\n",
                Some(&entry),
                &dir,
            )
            .expect("a valid edit");

            let mode = fs::metadata(&entry).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode, 0o644, "the edit changed the config's permissions");
            assert!(fs::read_to_string(&entry)
                .unwrap()
                .contains("gs: git status"));
        }
    }
}
