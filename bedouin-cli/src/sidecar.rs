//! Finding, fetching and handing over to `bedouin-ui`.
//!
//! The web UI is a separate binary on purpose: it carries an HTTP stack and
//! the built assets, and the bootstrap binary must stay a small static thing
//! that runs on a bare machine. Nothing here links any of that in — the
//! download is `curl` through the `Host`, exactly as brew, mise and rustup
//! are bootstrapped, so this whole file costs the main binary a few hundred
//! bytes rather than a few megabytes.

use bedouin_core::host::{Cmd, Host, OsHost};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

const NAME: &str = "bedouin-ui";
const REPO: &str = "samishal1998/bedouin";

/// Where a fetched sidecar lives. Beside the binary would need write access
/// to wherever that is; the state directory is already ours.
fn store(home: &Path) -> PathBuf {
    home.join(".local/share/bedouin/bin")
}

/// The release asset for this machine, matching the names release.yml writes.
fn target() -> Option<&'static str> {
    Some(match (std::env::consts::OS, std::env::consts::ARCH) {
        ("linux", "x86_64") => "x86_64-unknown-linux-musl",
        ("linux", "aarch64") => "aarch64-unknown-linux-musl",
        ("macos", "x86_64") => "x86_64-apple-darwin",
        ("macos", "aarch64") => "aarch64-apple-darwin",
        _ => return None,
    })
}

/// A sidecar already on disk whose version matches ours, or `None`.
///
/// The version has to match: the sidecar links the same core, and a plan
/// rendered by a different one is a plan for a different program.
fn usable(host: &dyn Host, candidates: &[PathBuf]) -> Option<PathBuf> {
    let want = env!("CARGO_PKG_VERSION");
    for p in candidates {
        if host.symlink_meta(p).ok().flatten().is_none() {
            continue;
        }
        let mut out = String::new();
        let mut cmd = Cmd::new([p.display().to_string(), "--version".into()]);
        cmd.env = std::env::vars().collect();
        if host
            .run(&cmd, &mut |l| {
                if let bedouin_core::host::Line::Out(s) = l {
                    out.push_str(&s);
                }
            })
            .is_ok_and(|s| s.ok())
            && out.split_whitespace().nth(1) == Some(want)
        {
            return Some(p.clone());
        }
    }
    None
}

pub fn run(host: &OsHost, config: Option<&Path>, port: u16, yes: bool) -> ExitCode {
    let Some(target) = target() else {
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

    let beside = std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|d| d.join(NAME)));
    let stored = store(&home).join(NAME);
    let mut candidates: Vec<PathBuf> = beside.into_iter().collect();
    candidates.push(stored.clone());

    let found = match usable(host, &candidates) {
        Some(p) => p,
        None => {
            let version = env!("CARGO_PKG_VERSION");
            let url = format!(
                "https://github.com/{REPO}/releases/download/v{version}/{NAME}-{target}.tar.gz"
            );
            println!("{NAME} {version} is not installed.\n");
            println!("  fetch   {NAME} {version}");
            println!("  from    {url}");
            println!("  into    {}", stored.display());
            println!("  checked against the release's SHA256SUMS\n");
            if !yes && !confirm() {
                println!("Nothing fetched.");
                return ExitCode::SUCCESS;
            }
            match fetch(host, &url, &stored) {
                Ok(()) => stored,
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

fn confirm() -> bool {
    use std::io::Write;
    print!("Download it? [y/N] ");
    let _ = std::io::stdout().flush();
    let mut s = String::new();
    std::io::stdin().read_line(&mut s).is_ok() && matches!(s.trim(), "y" | "Y" | "yes")
}

/// curl, tar and shasum -- the same three tools the manager bootstraps rely
/// on, and no new dependency for the bootstrap binary.
fn fetch(host: &dyn Host, url: &str, dest: &Path) -> Result<(), String> {
    let tmp = std::env::temp_dir().join(format!("{NAME}.tar.gz"));
    let sums = std::env::temp_dir().join("bedouin-SHA256SUMS");
    let sums_url = url
        .rsplit_once('/')
        .map(|(base, _)| format!("{base}/SHA256SUMS"))
        .ok_or("malformed release url")?;

    sh(
        host,
        &["curl", "-fsSL", url, "-o", &tmp.display().to_string()],
    )?;

    // Verified, not trusted: this is a binary about to be executed. A release
    // without published sums is a release this refuses to install.
    sh(
        host,
        &[
            "curl",
            "-fsSL",
            &sums_url,
            "-o",
            &sums.display().to_string(),
        ],
    )
    .map_err(|e| format!("{e}\n  no SHA256SUMS for this release, so nothing to verify against"))?;

    let mut got = String::new();
    let mut cmd = Cmd::new(["shasum", "-a", "256", &tmp.display().to_string()]);
    cmd.env = std::env::vars().collect();
    host.run(&cmd, &mut |l| {
        if let bedouin_core::host::Line::Out(s) = l {
            got.push_str(&s);
        }
    })
    .map_err(|e| e.to_string())?;
    let digest = got.split_whitespace().next().unwrap_or_default();
    let published = host
        .read(&sums)
        .map_err(|e| e.to_string())?
        .map(|b| String::from_utf8_lossy(&b).into_owned())
        .unwrap_or_default();
    if digest.is_empty() || !published.contains(digest) {
        return Err(format!(
            "checksum mismatch for {NAME} -- refusing to install it\n  got: {digest}"
        ));
    }

    let dir = dest.parent().ok_or("no directory to install into")?;
    host.mkdir_p(dir).map_err(|e| e.to_string())?;
    sh(
        host,
        &[
            "tar",
            "xzf",
            &tmp.display().to_string(),
            "-C",
            &dir.display().to_string(),
        ],
    )?;
    sh(host, &["chmod", "+x", &dest.display().to_string()])?;
    let _ = host.remove(&tmp);
    let _ = host.remove(&sums);
    println!("Installed {}", dest.display());
    Ok(())
}

fn sh(host: &dyn Host, argv: &[&str]) -> Result<(), String> {
    let mut cmd = Cmd::new(argv.iter().map(|s| s.to_string()).collect::<Vec<_>>());
    cmd.env = std::env::vars().collect();
    let mut tail = Vec::new();
    let status = host
        .run(&cmd, &mut |l| {
            if let bedouin_core::host::Line::Err(s) = l {
                tail.push(s);
            }
        })
        .map_err(|e| e.to_string())?;
    if status.ok() {
        Ok(())
    } else {
        Err(format!(
            "`{}` failed\n  {}",
            argv.join(" "),
            tail.join("\n  ")
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_target_matches_the_names_release_yml_writes() {
        // If these drift apart the fetch 404s, and it 404s only on the
        // platform nobody developing on Linux would notice.
        let workflow = include_str!("../../.github/workflows/release.yml");
        for t in [
            "x86_64-unknown-linux-musl",
            "aarch64-unknown-linux-musl",
            "x86_64-apple-darwin",
            "aarch64-apple-darwin",
        ] {
            assert!(workflow.contains(t), "release.yml no longer builds {t}");
        }
        // And the asset name this constructs is the one it packages.
        assert!(
            workflow.contains("bedouin-ui-${{ matrix.target }}.tar.gz"),
            "release.yml does not package the sidecar under the expected name"
        );
    }

    #[test]
    fn this_machine_has_a_target() {
        assert!(
            target().is_some(),
            "no sidecar build for the platform running the tests"
        );
    }

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
