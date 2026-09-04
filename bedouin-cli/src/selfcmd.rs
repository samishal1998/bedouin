//! `bedouin self` — this binary, and the pieces released alongside it.
//!
//! Deliberately not `bedouin upgrade`. bedouin's whole subject is installing
//! and upgrading *packages*, so an unqualified `upgrade` would read as "bring
//! my machine's software up to date", which is a thing this tool pointedly
//! does not do (§7.2: `latest` means install if absent, never upgrade). The
//! `self` namespace is the difference between the tool and its subject.

use crate::{release, sidecar};
use bedouin_core::host::{Host, OsHost};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

/// Something a release can replace. `name` is both the binary's name and the
/// name of its release asset, which is why the two must never diverge.
struct Piece {
    name: &'static str,
    path: PathBuf,
    version: String,
}

/// What is installed on this machine right now, without touching the network.
///
/// The sidecar is listed only when it is already here. Someone who has never
/// run `bedouin ui` does not get a web server pushed onto their machine by an
/// upgrade; `bedouin ui` still fetches it on first use.
fn installed(host: &dyn Host) -> Vec<Piece> {
    let mut out = vec![Piece {
        name: "bedouin",
        path: std::env::current_exe().unwrap_or_else(|_| PathBuf::from("bedouin")),
        version: env!("CARGO_PKG_VERSION").to_string(),
    }];
    if let Some(home) = host.env().get("HOME").map(PathBuf::from) {
        // Whichever copy `bedouin ui` would actually run, so an upgrade fixes
        // the one that is in use rather than a shadowed one further down.
        if let Some((path, version)) = sidecar::candidates(&home)
            .into_iter()
            .find_map(|p| release::installed_version(host, &p).map(|v| (p, v)))
        {
            out.push(Piece {
                name: sidecar::NAME,
                path,
                version,
            });
        }
    }
    out
}

/// `/home/you/.local/bin` -> `~/.local/bin`, so a path fits on a line and
/// reads as the place you know rather than a string you have to parse.
fn short(host: &dyn Host, p: &Path) -> String {
    let s = p.display().to_string();
    match host.env().get("HOME") {
        Some(h) if !h.is_empty() => s
            .strip_prefix(h)
            .map(|rest| format!("~{rest}"))
            .unwrap_or(s),
        _ => s,
    }
}

/// `bedouin self version` — what is on this machine. No network: this is the
/// command that still answers on a box that cannot reach GitHub.
pub fn version(host: &OsHost) -> ExitCode {
    let pieces = installed(host);
    let w = pieces.iter().map(|p| p.name.len()).max().unwrap_or(7);
    for p in &pieces {
        println!(
            "  {:w$}  {:8}  {}",
            p.name,
            p.version,
            short(host, &p.path),
            w = w
        );
    }
    if pieces.len() == 1 {
        println!(
            "  {:w$}  not installed — `bedouin ui` fetches it on first use",
            sidecar::NAME,
            w = w
        );
    }
    println!("\n`bedouin self upgrade --check` looks for a newer release.");
    ExitCode::SUCCESS
}

/// `bedouin self upgrade`. Exits 2 under `--check` when something is out of
/// date, so a timer or a CI step can gate on it the way it gates on `plan`.
pub fn upgrade(host: &OsHost, check: bool, yes: bool) -> ExitCode {
    let Some(target) = release::target() else {
        eprintln!(
            "bedouin: no release build for {}/{}",
            std::env::consts::OS,
            std::env::consts::ARCH
        );
        return ExitCode::FAILURE;
    };
    let newest = match release::latest(host) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("bedouin: {e}");
            return ExitCode::FAILURE;
        }
    };

    let pieces = installed(host);
    let stale: Vec<&Piece> = pieces
        .iter()
        .filter(|p| release::is_newer(&newest, &p.version))
        .collect();

    if stale.is_empty() {
        let running = env!("CARGO_PKG_VERSION");
        println!("bedouin {running} is the latest release.");
        // Everything else on the machine is at the same version, or it would
        // be in `stale` -- but say so, because "up to date" that quietly means
        // "I only checked one thing" is the kind of reassurance that rots.
        for p in pieces.iter().skip(1) {
            println!("  {} {} is current too.", p.name, p.version);
        }
        return ExitCode::SUCCESS;
    }

    let w = stale.iter().map(|p| p.name.len()).max().unwrap_or(7);
    println!();
    for p in &stale {
        println!("  {:w$}  {}  ->  {newest}", p.name, p.version, w = w);
    }

    if check {
        println!("\n`bedouin self upgrade` installs it.");
        // 2, not 1: the same "there is work pending" that `plan` and `doctor`
        // use. 1 stays what it has always been -- something went wrong.
        return ExitCode::from(2);
    }

    println!("\n  from    github.com/{}", release::REPO);
    for p in &stale {
        println!("  into    {}", short(host, &p.path));
    }
    println!("  checked against the release's SHA256SUMS\n");
    if !yes && !release::confirm("Upgrade?") {
        println!("Nothing changed.");
        return ExitCode::SUCCESS;
    }

    // Each piece is replaced on its own. A sidecar that fails to install does
    // not undo an upgraded bedouin -- and must not be reported as if it had,
    // because the next `bedouin ui` will re-fetch it anyway.
    let mut failed = false;
    for p in &stale {
        let url = release::asset_url(p.name, &newest, target);
        match release::install_over(host, &url, &p.path) {
            Ok(()) => println!("  {} {} -> {newest}", p.name, p.version),
            Err(e) => {
                eprintln!("bedouin: {} could not be upgraded\n  {e}", p.name);
                failed = true;
            }
        }
    }
    if failed {
        return ExitCode::FAILURE;
    }
    println!("\nUpgraded. `bedouin --version` to confirm.");
    ExitCode::SUCCESS
}

#[cfg(test)]
mod tests {
    use super::*;
    use bedouin_core::host::FakeHost;

    #[test]
    fn a_machine_without_the_sidecar_lists_only_bedouin() {
        // The upgrade must not install a web server on someone who has never
        // asked for one, and this is where that decision is made.
        let host = FakeHost::new().with_env("HOME", "/home/t");
        let pieces = installed(&host);
        assert_eq!(pieces.len(), 1);
        assert_eq!(pieces[0].name, "bedouin");
        assert_eq!(pieces[0].version, env!("CARGO_PKG_VERSION"));
    }

    #[test]
    fn paths_are_shown_against_home() {
        let host = FakeHost::new().with_env("HOME", "/home/t");
        assert_eq!(
            short(&host, Path::new("/home/t/.local/bin/bedouin")),
            "~/.local/bin/bedouin"
        );
        // A system install is not under HOME and must not be mangled into
        // looking like it is.
        assert_eq!(
            short(&host, Path::new("/usr/local/bin/bedouin")),
            "/usr/local/bin/bedouin"
        );
    }
}
