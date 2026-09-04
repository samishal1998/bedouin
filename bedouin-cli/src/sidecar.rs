//! Finding, fetching and handing over to `bedouin-ui`.
//!
//! The web UI is a separate binary on purpose: it carries an HTTP stack and
//! the built assets, and the bootstrap binary must stay a small static thing
//! that runs on a bare machine. Nothing here links any of that in — the
//! download goes through `release`, which is `curl` on the `Host`, exactly as
//! brew, mise and rustup are bootstrapped.

use crate::release;
use bedouin_core::host::{Host, OsHost};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

pub const NAME: &str = "bedouin-ui";

/// Where a fetched sidecar lives. Beside the binary would need write access
/// to wherever that is; the state directory is already ours.
pub fn store(home: &Path) -> PathBuf {
    home.join(".local/share/bedouin/bin")
}

/// Every place a sidecar could be, nearest first: beside this binary (how a
/// release tarball or a `cargo build` leaves it), then the state directory
/// (where a fetch puts it).
pub fn candidates(home: &Path) -> Vec<PathBuf> {
    let beside = std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|d| d.join(NAME)));
    let mut out: Vec<PathBuf> = beside.into_iter().collect();
    out.push(store(home).join(NAME));
    out
}

/// A sidecar already on disk whose version matches ours, or `None`.
///
/// The version has to match: the sidecar links the same core, and a plan
/// rendered by a different one is a plan for a different program.
fn usable(host: &dyn Host, candidates: &[PathBuf]) -> Option<PathBuf> {
    let want = env!("CARGO_PKG_VERSION");
    candidates
        .iter()
        .find(|p| release::installed_version(host, p).as_deref() == Some(want))
        .cloned()
}

pub fn run(host: &OsHost, config: Option<&Path>, hostname: &str, port: u16, yes: bool) -> ExitCode {
    let Some(target) = release::target() else {
        eprintln!(
            "bedouin: no {NAME} build for {}/{}",
            std::env::consts::OS,
            std::env::consts::ARCH
        );
        return ExitCode::FAILURE;
    };
    let Some(home) = host.env().get("HOME").map(PathBuf::from) else {
        eprintln!("bedouin: $HOME is not set");
        return ExitCode::FAILURE;
    };

    let stored = store(&home).join(NAME);
    let found = match usable(host, &candidates(&home)) {
        Some(p) => p,
        None => {
            let version = env!("CARGO_PKG_VERSION");
            let url = release::asset_url(NAME, version, target);
            println!("{NAME} {version} is not installed.\n");
            println!("  fetch   {NAME} {version}");
            println!("  from    {url}");
            println!("  into    {}", stored.display());
            println!("  checked against the release's SHA256SUMS\n");
            if !yes && !release::confirm("Download it?") {
                println!("Nothing fetched.");
                return ExitCode::SUCCESS;
            }
            match release::fetch(host, &url, &stored) {
                Ok(()) => {
                    println!("Installed {}", stored.display());
                    stored
                }
                Err(e) => {
                    eprintln!("bedouin: {e}");
                    return ExitCode::FAILURE;
                }
            }
        }
    };

    // Replace this process rather than spawning one. `apply` runs `sudo -v`
    // with inherited stdin, so the server has to own the terminal you started
    // it from -- that is what lets sudo prompt here instead of needing a
    // password to cross an HTTP boundary.
    let mut cmd = std::process::Command::new(&found);
    cmd.arg("--port").arg(port.to_string());
    cmd.arg("--hostname").arg(hostname);
    if let Some(c) = config {
        cmd.arg("--config").arg(c);
    }
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        let e = cmd.exec();
        eprintln!("bedouin: could not run {}: {e}", found.display());
        ExitCode::FAILURE
    }
    #[cfg(not(unix))]
    match cmd.status() {
        Ok(s) if s.success() => ExitCode::SUCCESS,
        Ok(_) => ExitCode::FAILURE,
        Err(e) => {
            eprintln!("bedouin: could not run {}: {e}", found.display());
            ExitCode::FAILURE
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_sidecar_is_stored_under_the_state_directory() {
        // Beside the binary would need write access to wherever that is --
        // /usr/local/bin for a system install.
        let p = store(Path::new("/home/t")).join(NAME);
        assert_eq!(
            p,
            Path::new("/home/t/.local/share/bedouin/bin/bedouin-ui"),
            "somewhere already ours to write to"
        );
    }
}
