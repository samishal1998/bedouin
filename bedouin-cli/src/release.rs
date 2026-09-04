//! The release channel: what is published, where, and how it is verified.
//!
//! Two callers want the same three things — find a version, download a
//! binary, prove it is the one that was published. `bedouin ui` fetching its
//! sidecar and `bedouin self upgrade` replacing this binary are the same
//! operation pointed at different assets, so they share the code rather than
//! each growing their own checksum step. A second download path is a second
//! place for the verification to be wrong.
//!
//! Everything here goes through `curl`, `tar` and `shasum` on the `Host`, the
//! way the manager bootstraps do. Nothing links an HTTP stack into the
//! binary that has to run on a bare machine.

use bedouin_core::host::{Cmd, Host, Line};
use std::path::Path;

pub const REPO: &str = "samishal1998/bedouin";

/// The release asset for this machine, matching the names release.yml writes.
pub fn target() -> Option<&'static str> {
    Some(match (std::env::consts::OS, std::env::consts::ARCH) {
        ("linux", "x86_64") => "x86_64-unknown-linux-musl",
        ("linux", "aarch64") => "aarch64-unknown-linux-musl",
        ("macos", "x86_64") => "x86_64-apple-darwin",
        ("macos", "aarch64") => "aarch64-apple-darwin",
        _ => return None,
    })
}

pub fn asset_url(name: &str, version: &str, target: &str) -> String {
    format!("https://github.com/{REPO}/releases/download/v{version}/{name}-{target}.tar.gz")
}

/// The newest published version, without asking the GitHub API.
///
/// `/releases/latest` redirects to `/releases/tag/vX.Y.Z`, so the tag is in
/// the URL curl lands on. That is one request, no token, no JSON, and — the
/// reason it is worth the trick — no 60-per-hour rate limit to share with
/// every other unauthenticated caller behind the same NAT.
pub fn latest(host: &dyn Host) -> Result<String, String> {
    let url = format!("https://github.com/{REPO}/releases/latest");
    let mut out = String::new();
    let mut cmd = Cmd::new([
        "curl",
        "-fsSLI",
        "-o",
        "/dev/null",
        "-w",
        "%{url_effective}\n",
        &url,
    ]);
    cmd.env = std::env::vars().collect();
    let status = host
        .run(&cmd, &mut |l| {
            if let Line::Out(s) = l {
                out.push_str(&s);
            }
        })
        .map_err(|e| e.to_string())?;
    if !status.ok() {
        return Err(format!(
            "could not reach the releases page\n  {url}\n  \
             This needs the network. `bedouin self version` works offline."
        ));
    }
    tag_version(out.trim()).map(str::to_string).ok_or_else(|| {
        format!(
            "could not read a version out of the release URL\n  {}",
            out.trim()
        )
    })
}

/// `.../releases/tag/v0.13.0` -> `0.13.0`. Returns `None` when the redirect
/// went somewhere else, which is how a signed-out redirect to a login page
/// stops being mistaken for a version.
fn tag_version(url: &str) -> Option<&str> {
    let tag = url.rsplit_once("/tag/")?.1;
    let v = tag.strip_prefix('v').unwrap_or(tag);
    parse(v).map(|_| v)
}

/// `0.13.0` -> `(0, 13, 0)`. Deliberately strict: anything that is not three
/// numbers is not a version this understands, and guessing at one is how you
/// talk someone into installing a release that does not exist.
pub fn parse(v: &str) -> Option<(u32, u32, u32)> {
    let mut p = v.split('.');
    let out = (
        p.next()?.parse().ok()?,
        p.next()?.parse().ok()?,
        p.next()?.parse().ok()?,
    );
    p.next().is_none().then_some(out)
}

/// Whether `candidate` is a version to move to. Compared as numbers, because
/// as text `0.9.0` sorts above `0.12.1` and every user of that bug is stuck
/// on the older build being told they are current.
pub fn is_newer(candidate: &str, current: &str) -> bool {
    match (parse(candidate), parse(current)) {
        (Some(a), Some(b)) => a > b,
        _ => false,
    }
}

/// What an installed binary says it is, or `None` if it is absent or will not
/// run — a sidecar built for another libc answers nothing useful.
pub fn installed_version(host: &dyn Host, bin: &Path) -> Option<String> {
    host.symlink_meta(bin).ok().flatten()?;
    let mut out = String::new();
    let mut cmd = Cmd::new([bin.display().to_string(), "--version".into()]);
    cmd.env = std::env::vars().collect();
    host.run(&cmd, &mut |l| {
        if let Line::Out(s) = l {
            out.push_str(&s);
        }
    })
    .ok()
    .filter(|s| s.ok())?;
    out.split_whitespace().nth(1).map(str::to_string)
}

/// Download, verify, unpack. `dest` is the binary that should exist when this
/// returns; the tarball is expected to hold a file of that name.
///
/// curl, tar and shasum -- the same three tools the manager bootstraps rely
/// on, and no new dependency for the bootstrap binary.
pub fn fetch(host: &dyn Host, url: &str, dest: &Path) -> Result<(), String> {
    let name = dest
        .file_name()
        .and_then(|n| n.to_str())
        .ok_or("no file name to install")?;
    let tmp = std::env::temp_dir().join(format!("{name}.tar.gz"));
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
        if let Line::Out(s) = l {
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
            "checksum mismatch for {name} -- refusing to install it\n  got: {digest}"
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
    Ok(())
}

/// Replace a binary that may be the one running this code.
///
/// Staged in a directory *beside* the target rather than in /tmp, for two
/// reasons: `mv` is then a rename within one filesystem, which is atomic and
/// cannot leave a half-written binary at the path; and creating the staging
/// directory fails immediately when the install directory is not ours, which
/// is a better error than downloading nine megabytes and then finding out.
///
/// Renaming over a running executable is fine on Linux and macOS — the kernel
/// holds the old inode open until this process exits. Writing *into* it would
/// not be; that is ETXTBSY.
pub fn install_over(host: &dyn Host, url: &str, dest: &Path) -> Result<(), String> {
    let dir = dest.parent().ok_or("no directory to install into")?;
    let staging = dir.join(".bedouin-upgrade");
    host.mkdir_p(&staging).map_err(|_| {
        format!(
            "cannot write to {}\n  \
             bedouin is installed somewhere that needs root. Re-run with:\n    \
             sudo {} self upgrade -y",
            dir.display(),
            dest.display()
        )
    })?;

    let staged = staging.join(dest.file_name().unwrap_or_default());
    let out = fetch(host, url, &staged).and_then(|()| {
        sh(
            host,
            &[
                "mv",
                &staged.display().to_string(),
                &dest.display().to_string(),
            ],
        )
    });
    let _ = host.remove_dir_all(&staging);
    out
}

/// Ask before doing something to someone's machine. Shared so the sidecar
/// fetch and a self upgrade ask the same way.
pub fn confirm(prompt: &str) -> bool {
    use std::io::Write;
    print!("{prompt} [y/N] ");
    let _ = std::io::stdout().flush();
    let mut s = String::new();
    std::io::stdin().read_line(&mut s).is_ok() && matches!(s.trim(), "y" | "Y" | "yes")
}

pub fn sh(host: &dyn Host, argv: &[&str]) -> Result<(), String> {
    let mut cmd = Cmd::new(argv.iter().map(|s| s.to_string()).collect::<Vec<_>>());
    cmd.env = std::env::vars().collect();
    let mut tail = Vec::new();
    let status = host
        .run(&cmd, &mut |l| {
            if let Line::Err(s) = l {
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
    use bedouin_core::host::{FakeHost, FakeRun};

    #[test]
    fn versions_compare_as_numbers_not_as_text() {
        // The whole reason this function exists. As text `0.9.0` sorts above
        // `0.12.1`, so a string compare tells everyone on 0.9.0 they are
        // current and quietly strands them there.
        assert!(is_newer("0.12.1", "0.9.0"));
        assert!(!is_newer("0.9.0", "0.12.1"));

        assert!(is_newer("0.13.0", "0.12.1"));
        assert!(is_newer("1.0.0", "0.99.99"));
        assert!(!is_newer("0.12.1", "0.12.1"));
        assert!(!is_newer("0.12.0", "0.12.1"));
    }

    #[test]
    fn a_version_that_is_not_three_numbers_is_not_a_version() {
        // Better to say "I could not read that" than to talk someone into
        // downloading a release that was never published.
        for bad in ["", "v1.2.3", "1.2", "1.2.3.4", "latest", "1.2.x", "nightly"] {
            assert!(parse(bad).is_none(), "{bad} parsed as a version");
        }
        assert_eq!(parse("0.12.1"), Some((0, 12, 1)));
        // Never newer than anything, rather than defaulting to "yes, upgrade".
        assert!(!is_newer("garbage", "0.12.1"));
        assert!(!is_newer("0.13.0", "garbage"));
    }

    #[test]
    fn the_version_comes_out_of_the_url_the_redirect_lands_on() {
        assert_eq!(
            tag_version("https://github.com/o/r/releases/tag/v0.13.0"),
            Some("0.13.0")
        );
        // Not every redirect is a release. A sign-in wall, a repo that moved,
        // a 404 page -- none of those are a version to offer someone.
        for other in [
            "https://github.com/login?return_to=%2Fo%2Fr",
            "https://github.com/o/r/releases",
            "https://github.com/o/r/releases/tag/nightly",
        ] {
            assert_eq!(tag_version(other), None, "{other} read as a version");
        }
    }

    #[test]
    fn the_asset_names_match_the_ones_release_yml_writes() {
        // If these drift apart the fetch 404s, and for the darwin targets it
        // 404s only on a platform nobody developing on Linux would notice.
        let workflow = include_str!("../../.github/workflows/release.yml");
        for t in [
            "x86_64-unknown-linux-musl",
            "aarch64-unknown-linux-musl",
            "x86_64-apple-darwin",
            "aarch64-apple-darwin",
        ] {
            assert!(workflow.contains(t), "release.yml no longer builds {t}");
        }
        // Both assets this can install are packaged under the names it builds.
        for a in [
            "bedouin-${{ matrix.target }}.tar.gz",
            "bedouin-ui-${{ matrix.target }}.tar.gz",
        ] {
            assert!(workflow.contains(a), "release.yml does not package {a}");
        }
        assert!(
            asset_url("bedouin-ui", "1.2.3", "x86_64-apple-darwin")
                .ends_with("/v1.2.3/bedouin-ui-x86_64-apple-darwin.tar.gz"),
            "the url this builds is not the asset that is published"
        );
    }

    #[test]
    fn this_machine_has_a_target() {
        assert!(
            target().is_some(),
            "no release build for the platform running the tests"
        );
    }

    #[test]
    fn latest_reads_the_redirect_and_reports_a_dead_network_plainly() {
        let argv = format!(
            "curl -fsSLI -o /dev/null -w %{{url_effective}}\n https://github.com/{REPO}/releases/latest"
        );
        let host = FakeHost::new().with_command(
            &argv,
            FakeRun::ok(&format!("https://github.com/{REPO}/releases/tag/v0.13.0")),
        );
        assert_eq!(latest(&host).unwrap(), "0.13.0");

        // An unscripted command is a machine without curl, which is also what
        // no network looks like from here.
        let err = latest(&FakeHost::new()).unwrap_err();
        assert!(err.contains("network"), "unhelpful failure: {err}");
    }
}
