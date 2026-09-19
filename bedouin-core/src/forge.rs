//! Packages from GitHub releases: which release, and which file in it.
//!
//! The half of `bedouin install` that has no side effects. Choosing an asset
//! is the subtlest thing this feature does and the worst place to be quietly
//! wrong -- helix publishes `helix-25.07.1-source.tar.xz` in the same release
//! as its binaries -- so the choosing is a pure function over a list of names,
//! testable against real releases without a network.
//!
//! Fetching lives in the caller, through `curl` on the `Host`, the way
//! `release.rs` and the manager bootstraps do. Nothing links an HTTP stack
//! into a binary that has to run on a bare machine.

use crate::facts::{Arch, DistroLike, Os};

/// One downloadable file attached to a release.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Asset {
    pub name: String,
    pub url: String,
}

/// A published release, reduced to what choosing needs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Release {
    pub tag: String,
    pub prerelease: bool,
    pub assets: Vec<Asset>,
}

/// The machine an asset has to run on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Target {
    pub os: Os,
    pub arch: Arch,
    /// Alpine and its derivatives have no glibc at all, so a `gnu` build is
    /// not merely worse there -- it does not run.
    pub musl_only: bool,
}

impl Target {
    pub fn of(os: Os, arch: Arch, like: DistroLike) -> Self {
        Self {
            os,
            arch,
            musl_only: like == DistroLike::Alpine,
        }
    }
}

/// Which asset to install, or why bedouin will not choose one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Pick {
    /// Exactly one asset scored highest.
    One(Asset),
    /// Several tied at the top. Naming them is more use than guessing.
    Ambiguous(Vec<String>),
    /// Nothing in the release runs on this machine.
    None,
}

/// Extensions that are never the thing to install.
const NEVER: &[&str] = &[
    // Signatures, sums and provenance sit beside the asset they describe.
    ".sha256",
    ".sha256sum",
    ".sha512",
    ".md5",
    ".sig",
    ".asc",
    ".pem",
    ".sbom",
    ".intoto.jsonl",
    // Metadata and update feeds.
    ".txt",
    ".json",
    ".yaml",
    ".yml",
    ".zsync",
    ".sbom.json",
    // Another manager's job. A .deb belongs to apt, and installing one by
    // unpacking it into ~/.local/bin would give you the files without the
    // package database entry that makes them removable.
    ".deb",
    ".rpm",
    ".apk",
    ".msi",
    ".pkg",
    ".snap",
    ".flatpak",
];

/// Name fragments that mark an asset as not-a-program.
const NOT_A_PROGRAM: &[&str] = &[
    "source",
    "-src",
    "_src",
    "sources",
    "debug",
    "symbols",
    "dbgsym",
    "vendor",
    "checksums",
    "sha256sums",
    "manifest",
];

fn arch_tokens(a: Arch) -> &'static [&'static str] {
    match a {
        Arch::X86_64 => &["x86_64", "x86-64", "amd64", "x64"],
        Arch::Arm64 => &["aarch64", "arm64"],
    }
}

/// Arch names that are definitely not this machine. `arm` is deliberately
/// absent: it is a prefix of `arm64`, and rejecting on it would reject the
/// asset an arm64 machine wants.
fn foreign_arch_tokens(a: Arch) -> &'static [&'static str] {
    match a {
        Arch::X86_64 => &[
            "aarch64", "arm64", "armv7", "armv6", "armhf", "riscv", "ppc64", "s390x", "i686",
            "i386", "386", "32-bit",
        ],
        Arch::Arm64 => &[
            "x86_64", "x86-64", "amd64", "riscv", "ppc64", "s390x", "i686", "i386", "386", "32-bit",
        ],
    }
}

fn os_tokens(o: Os) -> &'static [&'static str] {
    match o {
        Os::Linux => &["linux"],
        Os::Macos => &["darwin", "macos", "apple", "osx", "mac"],
    }
}

fn foreign_os_tokens(o: Os) -> &'static [&'static str] {
    match o {
        Os::Linux => &[
            "darwin", "macos", "apple", "osx", "windows", "win32", "win64", ".exe", ".dmg",
            "freebsd", "netbsd", "openbsd", "android",
        ],
        Os::Macos => &[
            "linux",
            "windows",
            "win32",
            "win64",
            ".exe",
            ".appimage",
            "freebsd",
            "netbsd",
            "openbsd",
            "android",
        ],
    }
}

/// Archive shapes bedouin can open, and the bare-binary case.
pub const ARCHIVE_EXTS: &[&str] = &[
    ".tar.gz", ".tgz", ".tar.xz", ".txz", ".tar.bz2", ".tbz2", ".zip", ".gz",
];

fn has(hay: &str, needle: &str) -> bool {
    hay.contains(needle)
}

/// Whether an asset is disqualified whatever else it scores.
fn rejected(name_lc: &str, t: Target) -> bool {
    if NEVER.iter().any(|e| name_lc.ends_with(e)) {
        return true;
    }
    // `.sha256` can also appear mid-name (`foo.tar.gz.sha256.txt`).
    if name_lc.contains(".sha256") || name_lc.contains(".asc") {
        return true;
    }
    if NOT_A_PROGRAM.iter().any(|w| has(name_lc, w)) {
        return true;
    }
    if foreign_os_tokens(t.os).iter().any(|w| has(name_lc, w)) {
        return true;
    }
    if foreign_arch_tokens(t.arch).iter().any(|w| has(name_lc, w)) {
        return true;
    }
    // On Alpine a gnu build does not run at all.
    if t.musl_only && has(name_lc, "gnu") {
        return true;
    }
    false
}

fn score(name_lc: &str, t: Target, repo: &str) -> i32 {
    let mut s = 0;
    if arch_tokens(t.arch).iter().any(|w| has(name_lc, w)) {
        s += 100;
    }
    if os_tokens(t.os).iter().any(|w| has(name_lc, w)) {
        s += 50;
    }
    if t.os == Os::Linux {
        // Static beats dynamic for a binary dropped onto an unknown machine,
        // which is the whole situation here. bedouin ships musl for the same
        // reason.
        if has(name_lc, "musl") {
            s += 30;
        } else if has(name_lc, "gnu") {
            s += 10;
        }
    }
    // A tarball beats a zip of the same build. Projects commonly publish both
    // -- eza ships `eza_x86_64-unknown-linux-musl.tar.gz` beside a `.zip` of
    // the identical binary -- and without a preference the two tie and the
    // whole release is refused as ambiguous. zip is the Windows-shaped choice,
    // and bedouin does not install on Windows; it stays a candidate because
    // some projects publish nothing else.
    if name_lc.ends_with(".zip") {
        s += 3;
    } else if ARCHIVE_EXTS.iter().any(|e| name_lc.ends_with(e)) {
        s += 5;
    }
    if t.os == Os::Linux && name_lc.ends_with(".appimage") {
        s += 5;
    }
    if !repo.is_empty() && has(name_lc, &repo.to_ascii_lowercase()) {
        s += 1;
    }
    s
}

/// Choose the asset to install, or refuse.
///
/// `repo` is the bare repository name (`fd` of `sharkdp/fd`), used only as a
/// tie-break: an asset named after the project is a better guess than one that
/// is not, and it is worth exactly one point.
pub fn pick(assets: &[Asset], t: Target, repo: &str) -> Pick {
    let mut best: Vec<(&Asset, i32)> = Vec::new();
    let mut top = 0;
    for a in assets {
        let lc = a.name.to_ascii_lowercase();
        if rejected(&lc, t) {
            continue;
        }
        let s = score(&lc, t, repo);
        // A survivor that matches nothing about this machine is not a
        // candidate; it is a file that happened not to be rejected.
        if s < 100 {
            continue;
        }
        if s > top {
            top = s;
            best.clear();
            best.push((a, s));
        } else if s == top {
            best.push((a, s));
        }
    }
    match best.len() {
        0 => Pick::None,
        1 => Pick::One(best[0].0.clone()),
        _ => Pick::Ambiguous(best.into_iter().map(|(a, _)| a.name.clone()).collect()),
    }
}

/// How the user asked for a version.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Selector {
    /// Newest non-prerelease. The default.
    Latest,
    /// Newest release, prereleases included.
    Prerelease,
    /// This tag exactly.
    Tag(String),
    /// Newest tag matching this pattern. For repositories that publish several
    /// products from one repo -- `tui-0.1.0` beside `cli-0.1.0` -- where
    /// "latest" is whichever was cut last and may be the wrong product.
    Pattern(String),
}

/// `org/repo`, `org/repo@v1.2.3`, `org/repo@latest`, `org/repo@/^tui-/`.
pub fn parse_spec(spec: &str) -> Result<(String, String, Selector), String> {
    let (repo_part, sel_part) = match spec.split_once('@') {
        Some((r, s)) => (r, Some(s)),
        None => (spec, None),
    };
    let (org, repo) = repo_part
        .split_once('/')
        .ok_or_else(|| format!("`{spec}` is not `org/repo`\n  For example: `sharkdp/fd`"))?;
    if org.is_empty() || repo.is_empty() || repo.contains('/') {
        return Err(format!(
            "`{spec}` is not `org/repo`\n  For example: `sharkdp/fd`"
        ));
    }
    let sel = match sel_part {
        None | Some("latest") => Selector::Latest,
        Some("prerelease") => Selector::Prerelease,
        Some(s) if s.len() >= 2 && s.starts_with('/') && s.ends_with('/') => {
            Selector::Pattern(s[1..s.len() - 1].to_string())
        }
        Some("") => Selector::Latest,
        Some(tag) => Selector::Tag(tag.to_string()),
    };
    Ok((org.to_string(), repo.to_string(), sel))
}

/// Does this tag match the pattern?
///
/// A deliberately small subset of regex: `^` and `$` anchors and literal text.
/// The mixed-tag case the feature exists for is `^tui-` and `^cli-`, and a
/// full engine would be a dependency and a parser for one character of value.
pub fn matches_pattern(tag: &str, pat: &str) -> bool {
    let (anchored_start, rest) = match pat.strip_prefix('^') {
        Some(r) => (true, r),
        None => (false, pat),
    };
    let (anchored_end, needle) = match rest.strip_suffix('$') {
        Some(r) => (true, r),
        None => (false, rest),
    };
    match (anchored_start, anchored_end) {
        (true, true) => tag == needle,
        (true, false) => tag.starts_with(needle),
        (false, true) => tag.ends_with(needle),
        (false, false) => tag.contains(needle),
    }
}

// ---------------------------------------------------------------- fetching

use crate::host::{Cmd, Host, Line};

#[derive(serde::Deserialize)]
struct ApiAsset {
    name: String,
    browser_download_url: String,
}

#[derive(serde::Deserialize)]
struct ApiRelease {
    tag_name: String,
    #[serde(default)]
    prerelease: bool,
    #[serde(default)]
    draft: bool,
    #[serde(default)]
    assets: Vec<ApiAsset>,
}

impl From<ApiRelease> for Release {
    fn from(r: ApiRelease) -> Self {
        Release {
            tag: r.tag_name,
            prerelease: r.prerelease,
            assets: r
                .assets
                .into_iter()
                .map(|a| Asset {
                    name: a.name,
                    url: a.browser_download_url,
                })
                .collect(),
        }
    }
}

/// A GitHub token, if the machine already has one.
///
/// Borrowed from `gh` rather than asking for one, exactly as `gitcmd` borrows
/// its credential helper. The difference it makes is 60 requests an hour
/// against 5000, which is the difference between "works" and "works until you
/// install a third thing".
pub fn token(host: &dyn Host, facts: &crate::facts::Facts) -> Option<String> {
    host.which("gh", &crate::plan::system_path(facts))?;
    let mut out = String::new();
    let mut cmd = Cmd::new(["gh", "auth", "token"]);
    cmd.env = host.env().clone();
    let status = host
        .run(&cmd, &mut |l| {
            if let Line::Out(s) = l {
                out.push_str(&s);
            }
        })
        .ok()?;
    let t = out.trim().to_string();
    (status.ok() && !t.is_empty()).then_some(t)
}

/// One GET against the GitHub API, returning the body.
fn api(host: &dyn Host, url: &str, token: Option<&str>) -> Result<String, String> {
    let mut argv = vec![
        "curl".to_string(),
        "-fsSL".into(),
        "-H".into(),
        "Accept: application/vnd.github+json".into(),
        "-H".into(),
        "X-GitHub-Api-Version: 2022-11-28".into(),
    ];
    if let Some(t) = token {
        argv.push("-H".into());
        argv.push(format!("Authorization: Bearer {t}"));
    }
    argv.push(url.to_string());
    let mut cmd = Cmd::new(argv);
    cmd.env = host.env().clone();
    let mut body = String::new();
    let mut err = String::new();
    let status = host
        .run(&cmd, &mut |l| match l {
            Line::Out(s) => body.push_str(&s),
            Line::Err(s) => err.push_str(&s),
            _ => {}
        })
        .map_err(|e| e.to_string())?;
    if !status.ok() {
        // 403 with nothing else to say is nearly always the anonymous limit.
        if token.is_none() && (err.contains("403") || err.contains("429")) {
            return Err(
                "GitHub is rate-limiting anonymous requests (60 an hour, shared by \
                 everyone behind your address)\n  `gh auth login` raises it to 5000, \
                 and bedouin will use that token"
                    .into(),
            );
        }
        if err.contains("404") {
            return Err(format!("no such repository or release: {url}"));
        }
        return Err(format!("GitHub request failed: {}", err.trim()));
    }
    Ok(body)
}

/// Find the release a selector names.
pub fn resolve(
    host: &dyn Host,
    facts: &crate::facts::Facts,
    org: &str,
    repo: &str,
    sel: &Selector,
) -> Result<Release, String> {
    let tok = token(host, facts);
    let t = tok.as_deref();
    let base = format!("https://api.github.com/repos/{org}/{repo}");
    match sel {
        Selector::Latest => {
            let body = api(host, &format!("{base}/releases/latest"), t)?;
            let r: ApiRelease = serde_json::from_str(&body)
                .map_err(|e| format!("GitHub returned something unreadable: {e}"))?;
            Ok(r.into())
        }
        Selector::Tag(tag) => {
            let body = api(host, &format!("{base}/releases/tags/{tag}"), t)?;
            let r: ApiRelease = serde_json::from_str(&body)
                .map_err(|e| format!("GitHub returned something unreadable: {e}"))?;
            Ok(r.into())
        }
        // Both of these need the list: a prerelease is by definition not what
        // /releases/latest returns, and a repository publishing several
        // products has one "latest" that may be the wrong product entirely.
        Selector::Prerelease | Selector::Pattern(_) => {
            let body = api(host, &format!("{base}/releases?per_page=50"), t)?;
            let all: Vec<ApiRelease> = serde_json::from_str(&body)
                .map_err(|e| format!("GitHub returned something unreadable: {e}"))?;
            let found = all
                .into_iter()
                .filter(|r| !r.draft)
                .find(|r| match sel {
                    Selector::Prerelease => true,
                    Selector::Pattern(p) => !r.prerelease && matches_pattern(&r.tag_name, p),
                    _ => unreachable!(),
                })
                .ok_or_else(|| match sel {
                    Selector::Pattern(p) => format!(
                        "no release in the last 50 has a tag matching `{p}`\n  \
                         Patterns support ^ and $ anchors and literal text"
                    ),
                    _ => "this repository has published no releases".to_string(),
                })?;
            Ok(found.into())
        }
    }
}

// ------------------------------------------------------------------ manifest

/// What a maintainer can say about their own tool.
///
/// Every field is optional and autodetect runs without any of this. A manifest
/// may never be the thing that makes a tool installable -- only the thing that
/// makes it better -- because a project that has never heard of bedouin has to
/// work anyway.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Manifest {
    /// What to call the installed binary. Defaults to the repository name.
    pub name: Option<String>,
    pub description: Option<String>,
    pub homepage: Option<String>,
    /// Where the binary is inside the archive. `{version}` and `{target}` are
    /// substituted, because that path usually carries them.
    pub bin: Option<String>,
    /// Asset names per target triple, overriding autodetect entirely. The
    /// escape hatch for a project whose naming no scoring can rank.
    #[serde(default)]
    pub assets: std::collections::BTreeMap<String, String>,
    /// Name the checksum file when it is not one bedouin recognises.
    pub checksums: Option<String>,
    #[serde(default)]
    pub completions: std::collections::BTreeMap<String, String>,
}

impl Manifest {
    fn parse(text: &str, json: bool) -> Result<Self, String> {
        if json {
            serde_json::from_str(text).map_err(|e| e.to_string())
        } else {
            serde_yaml_ng::from_str(text).map_err(|e| e.to_string())
        }
    }

    /// Substitute the placeholders a path inside an archive usually carries.
    pub fn bin_for(&self, version: &str, target: &str) -> Option<String> {
        self.bin.as_ref().map(|b| {
            b.replace("{version}", version)
                .replace("{version_no_v}", version.trim_start_matches('v'))
                .replace("{target}", target)
        })
    }

    pub fn asset_for(&self, version: &str, target: &str) -> Option<String> {
        self.assets.get(target).map(|a| {
            a.replace("{version}", version)
                .replace("{version_no_v}", version.trim_start_matches('v'))
                .replace("{target}", target)
        })
    }
}

/// Look for a manifest, in the two places the design names.
///
/// The repository copy is fetched from raw.githubusercontent, which is not the
/// API and so costs no rate limit. The release copy is already in the asset
/// list, so noticing it is free. Repository first: it is the one a maintainer
/// can fix without cutting a release.
pub fn manifest(host: &dyn Host, org: &str, repo: &str, rel: &Release) -> Option<Manifest> {
    let env: std::collections::BTreeMap<String, String> = host.env().clone();
    for (file, json) in [
        ("bedouin.yaml", false),
        ("bedouin.yml", false),
        ("bedouin.json", true),
    ] {
        let url = format!("https://raw.githubusercontent.com/{org}/{repo}/HEAD/{file}");
        if let Ok(body) = sh(host, &format!("curl -fsSL {}", sq(&url)), &env) {
            if !body.trim().is_empty() {
                match Manifest::parse(&body, json) {
                    Ok(m) => return Some(m),
                    // A manifest that does not parse is the maintainer's bug,
                    // and silently installing as though it were absent would
                    // hide it from them. Autodetect still runs.
                    Err(_) => continue,
                }
            }
        }
    }
    for a in &rel.assets {
        let lc = a.name.to_ascii_lowercase();
        let json = lc.ends_with(".json");
        if lc == "manifest.yaml" || lc == "manifest.yml" || lc == "manifest.json" {
            if let Ok(body) = sh(host, &format!("curl -fsSL {}", sq(&a.url)), &env) {
                if let Ok(m) = Manifest::parse(&body, json) {
                    return Some(m);
                }
            }
        }
    }
    None
}

// ------------------------------------------------------- download and place

use std::path::{Path, PathBuf};

/// The triple a manifest's `assets:` map is keyed by.
pub fn triple(facts: &crate::facts::Facts) -> String {
    let arch = match facts.arch {
        Arch::X86_64 => "x86_64",
        Arch::Arm64 => "aarch64",
    };
    match facts.os {
        Os::Macos => format!("{arch}-apple-darwin"),
        Os::Linux => format!("{arch}-unknown-linux-musl"),
    }
}

/// Where an installed tool lands. The user's own, so nothing here needs root.
pub fn bin_dir(facts: &crate::facts::Facts) -> PathBuf {
    facts.home.join(".local/bin")
}

fn sh(
    host: &dyn Host,
    script: &str,
    env: &std::collections::BTreeMap<String, String>,
) -> Result<String, String> {
    let mut cmd = Cmd::new(["sh".to_string(), "-c".into(), script.to_string()]);
    cmd.env = env.clone();
    let mut out = String::new();
    let mut err = String::new();
    let status = host
        .run(&cmd, &mut |l| match l {
            Line::Out(s) => out.push_str(&s),
            Line::Err(s) => {
                err.push_str(&s);
                err.push('\n');
            }
            _ => {}
        })
        .map_err(|e| e.to_string())?;
    if !status.ok() {
        return Err(err.trim().to_string());
    }
    Ok(out)
}

/// The checksum file covering an asset, if the release published one.
///
/// Two shapes are common and neither is a standard: a sidecar per asset, and
/// one file listing every asset. Both are looked for; neither is required.
pub fn checksum_for<'a>(rel: &'a Release, asset: &Asset) -> Option<&'a Asset> {
    let sidecar = format!("{}.sha256", asset.name.to_ascii_lowercase());
    if let Some(a) = rel
        .assets
        .iter()
        .find(|a| a.name.to_ascii_lowercase() == sidecar)
    {
        return Some(a);
    }
    const SHARED: &[&str] = &[
        "checksums.txt",
        "sha256sums",
        "sha256sums.txt",
        "checksums",
        "sha256sum.txt",
    ];
    rel.assets
        .iter()
        .find(|a| SHARED.contains(&a.name.to_ascii_lowercase().as_str()))
}

/// How an asset is opened. `Bare` covers a downloaded executable and an
/// AppImage alike: there is nothing to unpack, only a bit to set.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Form {
    TarGz,
    TarXz,
    TarBz2,
    Zip,
    Gz,
    Bare,
}

impl Form {
    /// The program that opens this shape, when tar does not do it alone.
    ///
    /// `tar -xJf` does not decompress xz itself: it execs `xz`, which a
    /// minimal image does not have. Finding that out afterwards, from
    /// `tar (child): xz: Cannot exec`, is a worse way to learn it than being
    /// told before the download starts.
    pub fn needs(self) -> Option<&'static str> {
        match self {
            Form::TarGz => Some("gzip"),
            Form::TarXz => Some("xz"),
            Form::TarBz2 => Some("bzip2"),
            Form::Zip => Some("unzip"),
            Form::Gz => Some("gunzip"),
            Form::Bare => None,
        }
    }
}

pub fn form_of(name: &str) -> Form {
    let n = name.to_ascii_lowercase();
    if n.ends_with(".tar.gz") || n.ends_with(".tgz") {
        Form::TarGz
    } else if n.ends_with(".tar.xz") || n.ends_with(".txz") {
        Form::TarXz
    } else if n.ends_with(".tar.bz2") || n.ends_with(".tbz2") {
        Form::TarBz2
    } else if n.ends_with(".zip") {
        Form::Zip
    } else if n.ends_with(".gz") {
        Form::Gz
    } else {
        Form::Bare
    }
}

/// Everything `bedouin install` needs to do after it has chosen.
pub struct Plan {
    pub org: String,
    pub repo: String,
    pub tag: String,
    pub asset: Asset,
    pub checksum: Option<Asset>,
    /// What to call the binary once it is in place, when the user or a
    /// manifest said. `None` means take the name the archive gives it --
    /// ripgrep's tarball contains `rg`, and installing that as `ripgrep`
    /// produces a working binary under a name nobody will type.
    pub bin_name: Option<String>,
    /// Where in the archive it is, when a manifest said.
    pub bin_path: Option<String>,
}

/// Download, verify, unpack, and put one binary on PATH.
///
/// Written as one shell script per step rather than a pipeline so that a
/// failure names the step that failed. Everything runs in a scratch directory
/// that is removed whatever happens -- a half-downloaded archive must never
/// be mistaken for a finished one on the next run.
pub fn install(
    host: &dyn Host,
    facts: &crate::facts::Facts,
    dest_dir: &Path,
    p: &Plan,
    env: &std::collections::BTreeMap<String, String>,
    mut note: impl FnMut(&str),
) -> Result<PathBuf, String> {
    let form = form_of(&p.asset.name);
    // Asked before anything is downloaded, in the same spirit as the tar check
    // in `gitcmd::subdir_export`: everything this needs is checked while the
    // cost of being wrong is still nothing.
    if let Some(tool) = form.needs() {
        let path = crate::plan::system_path(facts);
        if host.which(tool, &path).is_none() && host.which("busybox", &path).is_none() {
            return Err(format!(
                "`{}` needs `{tool}` to unpack, and this machine has none\n  \
                 Install {tool}, or choose another file with --asset",
                p.asset.name
            ));
        }
    }
    let hint = p.bin_name.clone().unwrap_or_else(|| p.repo.clone());
    let work = facts.home.join(".cache/bedouin/install");
    let _ = host.remove_dir_all(&work);
    host.mkdir_p(&work).map_err(|e| e.to_string())?;
    let w = work.display().to_string();
    let archive = format!("{w}/{}", p.asset.name);

    sh(
        host,
        &format!("curl -fsSL -o {} {}", sq(&archive), sq(&p.asset.url)),
        env,
    )
    .map_err(|e| format!("downloading {}: {e}", p.asset.name))?;

    match &p.checksum {
        Some(c) => {
            let sums = format!("{w}/{}", c.name);
            sh(
                host,
                &format!("curl -fsSL -o {} {}", sq(&sums), sq(&c.url)),
                env,
            )
            .map_err(|e| format!("downloading {}: {e}", c.name))?;
            verify(host, &archive, &sums, &p.asset.name, env)?;
            note(&format!("verified against {}", c.name));
        }
        None => note("no checksums published for this release"),
    }

    // Unpack into its own directory so what arrived is separable from what
    // curl wrote.
    let out = format!("{w}/unpacked");
    host.mkdir_p(Path::new(&out)).map_err(|e| e.to_string())?;
    let unpack = match form {
        Form::TarGz => format!("tar -xzf {} -C {}", sq(&archive), sq(&out)),
        Form::TarXz => format!("tar -xJf {} -C {}", sq(&archive), sq(&out)),
        Form::TarBz2 => format!("tar -xjf {} -C {}", sq(&archive), sq(&out)),
        Form::Zip => format!("unzip -oq {} -d {}", sq(&archive), sq(&out)),
        // A bare download has no name of its own inside an archive, so the
        // hint is all there is.
        Form::Gz => format!("gunzip -c {} > {}/{}", sq(&archive), sq(&out), sq(&hint)),
        Form::Bare => format!("cp {} {}/{}", sq(&archive), sq(&out), sq(&hint)),
    };
    sh(host, &unpack, env).map_err(|e| format!("unpacking {}: {e}", p.asset.name))?;

    let found = locate(host, Path::new(&out), p)?;
    // The archive is the authority on what the binary is called, unless
    // somebody said otherwise.
    let name = p.bin_name.clone().unwrap_or_else(|| {
        found
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| p.repo.clone())
    });
    let dest = dest_dir.join(&name);
    host.mkdir_p(dest_dir).map_err(|e| e.to_string())?;
    // Beside then move, as `release::install_over` does: an interrupted
    // install must never leave a half-written file on PATH.
    let staged = format!("{}.bedouin-new", dest.display());
    sh(
        host,
        &format!(
            "cp {} {} && chmod 755 {} && mv -f {} {}",
            sq(&found.display().to_string()),
            sq(&staged),
            sq(&staged),
            sq(&staged),
            sq(&dest.display().to_string())
        ),
        env,
    )
    .map_err(|e| format!("placing {name}: {e}"))?;
    let _ = host.remove_dir_all(&work);
    Ok(dest)
}

fn verify(
    host: &dyn Host,
    archive: &str,
    sums: &str,
    asset_name: &str,
    env: &std::collections::BTreeMap<String, String>,
) -> Result<(), String> {
    // One file per asset holds a bare digest; a shared file holds a line per
    // asset. Reduce both to "the digest that should match".
    let script = format!(
        "want=$(grep -F {name} {sums} 2>/dev/null | head -1 | tr -s ' ' | cut -d' ' -f1); \
         [ -n \"$want\" ] || want=$(head -1 {sums} | tr -s ' ' | cut -d' ' -f1); \
         got=$(sha256sum {arc} 2>/dev/null || shasum -a 256 {arc}); \
         got=${{got%% *}}; \
         [ \"$want\" = \"$got\" ] || {{ echo \"want $want, got $got\" >&2; exit 1; }}",
        name = sq(asset_name),
        sums = sq(sums),
        arc = sq(archive),
    );
    sh(host, &script, env).map(|_| ()).map_err(|e| {
        format!("checksum mismatch for {asset_name}: {e}\n  The download was discarded")
    })
}

/// The executable inside an unpacked archive.
fn locate(host: &dyn Host, root: &Path, p: &Plan) -> Result<PathBuf, String> {
    if let Some(rel) = &p.bin_path {
        let c = root.join(rel);
        if host.symlink_meta(&c).ok().flatten().is_some() {
            return Ok(c);
        }
        return Err(format!(
            "the manifest says the binary is at `{rel}`, and it is not in the archive"
        ));
    }
    let mut files = Vec::new();
    walk(host, root, &mut files, 0);
    // The name it will be installed as, then the repository name, then the
    // only executable there is.
    let hint = p.bin_name.clone().unwrap_or_else(|| p.repo.clone());
    for want in [hint.as_str(), p.repo.as_str()] {
        if let Some(f) = files.iter().find(|f| {
            f.file_name().map(|n| n.to_string_lossy().to_lowercase())
                == Some(want.to_ascii_lowercase())
        }) {
            return Ok(f.clone());
        }
    }
    let execs: Vec<&PathBuf> = files
        .iter()
        .filter(|f| {
            host.symlink_meta(f)
                .ok()
                .flatten()
                .is_some_and(|m| m.mode & 0o111 != 0)
        })
        .collect();
    match execs.len() {
        1 => Ok(execs[0].clone()),
        0 => Err(format!(
            "nothing executable in {}\n  Found: {}",
            p.asset.name,
            names(&files)
        )),
        _ => Err(format!(
            "several executables in {}, and none is named `{}`\n  Found: {}\n  \
             Name one with `bin:` in a bedouin.yaml, or install it by hand",
            p.asset.name,
            hint,
            names(&execs.iter().map(|p| (*p).clone()).collect::<Vec<_>>())
        )),
    }
}

fn names(v: &[PathBuf]) -> String {
    v.iter()
        .filter_map(|f| f.file_name().map(|n| n.to_string_lossy().into_owned()))
        .collect::<Vec<_>>()
        .join(", ")
}

fn walk(host: &dyn Host, dir: &Path, out: &mut Vec<PathBuf>, depth: usize) {
    // Release archives are one or two levels deep; anything more is not an
    // archive shape bedouin should be guessing about.
    if depth > 3 || out.len() > 500 {
        return;
    }
    let Ok(entries) = host.read_dir(dir) else {
        return;
    };
    for e in entries {
        match host.symlink_meta(&e) {
            Ok(Some(m)) if m.is_dir => walk(host, &e, out, depth + 1),
            Ok(Some(_)) => out.push(e),
            _ => {}
        }
    }
}

fn sq(s: &str) -> String {
    format!("'{}'", s.replace('\'', r"'\''"))
}

/// Fetch a small text file. Public because `generate` reads checksum files at
/// generate time so the script it emits does not have to.
pub fn fetch_text(host: &dyn Host, url: &str) -> Result<String, String> {
    sh(
        host,
        &format!("curl -fsSL {}", sq(url)),
        &host.env().clone(),
    )
}

/// The digest a release published for one asset, if it published one.
///
/// Read at generate time so the script that comes out needs no network beyond
/// its own download, and cannot be handed a different checksum later.
pub fn digest_for(host: &dyn Host, rel: &Release, asset: &Asset) -> Option<String> {
    let c = checksum_for(rel, asset)?;
    let body = fetch_text(host, &c.url).ok()?;
    for line in body.lines() {
        let mut parts = line.split_whitespace();
        let digest = parts.next()?;
        match parts.next() {
            // A shared file is `<digest>  <name>`; a sidecar is the digest
            // alone, sometimes followed by the name.
            Some(name) if name.trim_start_matches('*').ends_with(&asset.name) => {
                return Some(digest.to_string())
            }
            Some(_) => {}
            None if digest.len() == 64 => return Some(digest.to_string()),
            None => {}
        }
    }
    None
}

/// One line of the generated script's `case`: the uname pattern it matches,
/// the asset to fetch, and the digest to check it against.
pub type ScriptRow = (String, Asset, Option<String>);

/// The four machines a `uname -s`/`uname -m` pair can name, resolved once so
/// the emitted script does not have to.
pub fn script_rows(
    host: &dyn Host,
    rel: &Release,
    repo: &str,
) -> (Vec<ScriptRow>, Vec<&'static str>) {
    use crate::facts::{Arch, DistroLike, Os};
    let targets = [
        ("Linux-x86_64", Os::Linux, Arch::X86_64),
        ("Linux-aarch64|Linux-arm64", Os::Linux, Arch::Arm64),
        ("Darwin-x86_64", Os::Macos, Arch::X86_64),
        ("Darwin-arm64", Os::Macos, Arch::Arm64),
    ];
    let mut rows = Vec::new();
    let mut missing = Vec::new();
    for (uname, os, arch) in targets {
        match pick(&rel.assets, Target::of(os, arch, DistroLike::None), repo) {
            Pick::One(a) => {
                let sum = digest_for(host, rel, &a);
                rows.push((uname.to_string(), a, sum));
            }
            // A release with no build for a platform is ordinary, and a script
            // that says so beats one that pretends.
            _ => missing.push(uname),
        }
    }
    (rows, missing)
}

// ------------------------------------------------------------- script output

/// A `curl … | sh` installer for one resolved release.
///
/// Deliberately **not** a general asset matcher written in shell. The matcher
/// is the subtlest code in this feature and a second copy of it, in the
/// language least able to express it, would drift from the first within a
/// release or two. What this emits is a table: the assets that were chosen for
/// each target at generate time, and the checksum each one had.
///
/// That makes the script version-pinned, which is the honest shape -- it is
/// what bedouin's own `install.sh` is, and regenerating it is how a project
/// publishes a new version.
pub fn generate(org: &str, repo: &str, rel: &Release, rows: &[ScriptRow], bin: &str) -> String {
    let mut s = String::new();
    s.push_str("#!/bin/sh\n");
    s.push_str(&format!(
        "# {org}/{repo} {tag}\n\
         # Generated by `bedouin install generate`. Pinned to this release:\n\
         # regenerate it to publish a new one.\n\
         set -eu\n\n",
        tag = rel.tag
    ));
    s.push_str("BIN_DIR=\"${BEDOUIN_BIN_DIR:-$HOME/.local/bin}\"\n");
    s.push_str("case \"$(uname -s)-$(uname -m)\" in\n");
    for (uname, asset, sum) in rows {
        s.push_str(&format!("  {uname})\n"));
        s.push_str(&format!("    url={}\n", sq(&asset.url)));
        s.push_str(&format!("    file={}\n", sq(&asset.name)));
        match sum {
            Some(d) => s.push_str(&format!("    sum={}\n", sq(d))),
            None => s.push_str("    sum=\n"),
        }
        s.push_str("    ;;\n");
    }
    s.push_str(&format!(
        "  *)\n    echo \"{org}/{repo} {tag} has no build for $(uname -s) $(uname -m)\" >&2\n    exit 1\n    ;;\n",
        tag = rel.tag
    ));
    s.push_str("esac\n\n");
    s.push_str(&format!(
        r#"tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT
echo "fetching $file"
curl -fsSL -o "$tmp/$file" "$url"

if [ -n "$sum" ]; then
  got=$(sha256sum "$tmp/$file" 2>/dev/null || shasum -a 256 "$tmp/$file")
  got=${{got%% *}}
  if [ "$sum" != "$got" ]; then
    echo "checksum mismatch: wanted $sum, got $got" >&2
    exit 1
  fi
  echo "checksum ok"
else
  echo "this release publishes no checksums; nothing was verified" >&2
fi

case "$file" in
  *.tar.gz|*.tgz)  tar -xzf "$tmp/$file" -C "$tmp" ;;
  *.tar.xz|*.txz)  tar -xJf "$tmp/$file" -C "$tmp" ;;
  *.tar.bz2)       tar -xjf "$tmp/$file" -C "$tmp" ;;
  *.zip)           unzip -oq "$tmp/$file" -d "$tmp" ;;
  *)               cp "$tmp/$file" "$tmp/{bin}" ;;
esac

# The archive is the authority on what the binary is called: ripgrep's
# tarball contains `rg`. Look for the expected name first, then fall back to
# the one executable in the archive.
found=$(find "$tmp" -type f -name {bin} -print -quit)
if [ -z "$found" ]; then
  execs=$(find "$tmp" -type f -perm -u+x ! -name '*.txt' ! -name '*.md' -print)
  if [ "$(printf '%s\n' "$execs" | grep -c .)" = 1 ]; then
    found=$execs
  else
    echo "cannot tell which file in $file is the program:" >&2
    printf '  %s\n' $execs >&2
    exit 1
  fi
fi
name=$(basename "$found")

mkdir -p "$BIN_DIR"
install -m 755 "$found" "$BIN_DIR/$name"
echo "installed $BIN_DIR/$name"
case ":$PATH:" in
  *":$BIN_DIR:"*) ;;
  *) echo "note: $BIN_DIR is not on your PATH" >&2 ;;
esac
"#,
        bin = bin
    ));
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    fn assets(names: &[&str]) -> Vec<Asset> {
        names
            .iter()
            .map(|n| Asset {
                name: (*n).to_string(),
                url: format!("https://example/{n}"),
            })
            .collect()
    }

    fn linux64() -> Target {
        Target::of(Os::Linux, Arch::X86_64, DistroLike::Debian)
    }

    fn picked(a: &[Asset], t: Target, repo: &str) -> String {
        match pick(a, t, repo) {
            Pick::One(x) => x.name,
            Pick::Ambiguous(v) => panic!("ambiguous: {v:?}"),
            Pick::None => panic!("nothing picked"),
        }
    }

    // The four releases this matcher was designed against, copied from what
    // the GitHub API actually returned. Four projects, four naming schemes.

    #[test]
    fn ripgrep_rust_triples_with_sidecar_sums() {
        let a = assets(&[
            "ripgrep-15.2.0-aarch64-apple-darwin.tar.gz",
            "ripgrep-15.2.0-aarch64-apple-darwin.tar.gz.sha256",
            "ripgrep-15.2.0-aarch64-unknown-linux-gnu.tar.gz",
            "ripgrep-15.2.0-aarch64-unknown-linux-musl.tar.gz",
            "ripgrep-15.2.0-x86_64-unknown-linux-musl.tar.gz",
            "ripgrep-15.2.0-x86_64-unknown-linux-musl.tar.gz.sha256",
            "ripgrep-15.2.0-x86_64-pc-windows-msvc.zip",
            "ripgrep_15.2.0-1_amd64.deb",
        ]);
        assert_eq!(
            picked(&a, linux64(), "ripgrep"),
            "ripgrep-15.2.0-x86_64-unknown-linux-musl.tar.gz"
        );
        // The .deb is apt's job, and the .sha256 describes an asset rather
        // than being one.
        assert_eq!(
            picked(
                &a,
                Target::of(Os::Macos, Arch::Arm64, DistroLike::None),
                "ripgrep"
            ),
            "ripgrep-15.2.0-aarch64-apple-darwin.tar.gz"
        );
    }

    #[test]
    fn lazygit_go_style_names() {
        let a = assets(&[
            "checksums.txt",
            "lazygit_0.65.1_darwin_arm64.tar.gz",
            "lazygit_0.65.1_darwin_x86_64.tar.gz",
            "lazygit_0.65.1_linux_32-bit.tar.gz",
            "lazygit_0.65.1_linux_arm64.tar.gz",
            "lazygit_0.65.1_linux_x86_64.tar.gz",
            "lazygit_0.65.1_freebsd_x86_64.tar.gz",
            "lazygit_0.65.1_windows_x86_64.zip",
        ]);
        assert_eq!(
            picked(&a, linux64(), "lazygit"),
            "lazygit_0.65.1_linux_x86_64.tar.gz"
        );
        assert_eq!(
            picked(
                &a,
                Target::of(Os::Macos, Arch::Arm64, DistroLike::None),
                "lazygit"
            ),
            "lazygit_0.65.1_darwin_arm64.tar.gz"
        );
    }

    #[test]
    fn helix_ships_a_source_tarball_next_to_its_binaries() {
        // The reason the filter exists. `-source.tar.xz` sorts and scores
        // like a real asset and installing it would be a silent disaster.
        let a = assets(&[
            "helix-25.07.1-aarch64-linux.tar.xz",
            "helix-25.07.1-aarch64-macos.tar.xz",
            "helix-25.07.1-source.tar.xz",
            "helix-25.07.1-x86_64-linux.tar.xz",
            "helix-25.07.1-x86_64-macos.tar.xz",
            "helix-25.07.1-x86_64-windows.zip",
            "helix-25.07.1-x86_64.AppImage",
            "helix-25.07.1-x86_64.AppImage.zsync",
            "helix_25.7.1-1_amd64.deb",
        ]);
        let got = picked(&a, linux64(), "helix");
        assert_ne!(got, "helix-25.07.1-source.tar.xz", "installed the source!");
        assert_eq!(got, "helix-25.07.1-x86_64-linux.tar.xz");
    }

    #[test]
    fn fd_mixes_triples_with_differently_named_debs() {
        let a = assets(&[
            "fd-musl_10.5.0_amd64.deb",
            "fd-musl_10.5.0_arm64.deb",
            "fd-v10.5.0-aarch64-apple-darwin.tar.gz",
            "fd-v10.5.0-aarch64-unknown-linux-musl.tar.gz",
            "fd-v10.5.0-x86_64-unknown-linux-gnu.tar.gz",
            "fd-v10.5.0-x86_64-unknown-linux-musl.tar.gz",
            "fd-v10.5.0-x86_64-pc-windows-msvc.zip",
        ]);
        // musl over gnu: a static build is the safer thing to drop onto a
        // machine whose glibc you have not looked at.
        assert_eq!(
            picked(&a, linux64(), "fd"),
            "fd-v10.5.0-x86_64-unknown-linux-musl.tar.gz"
        );
        // The .deb named `amd64` must not win on an x86_64 box.
        assert!(!picked(&a, linux64(), "fd").ends_with(".deb"));
    }

    #[test]
    fn on_alpine_a_gnu_build_is_not_merely_worse() {
        let alpine = Target::of(Os::Linux, Arch::X86_64, DistroLike::Alpine);
        let only_gnu = assets(&["tool-1.0-x86_64-unknown-linux-gnu.tar.gz"]);
        // There is no glibc on Alpine, so this is a refusal rather than a
        // reluctant yes.
        assert_eq!(pick(&only_gnu, alpine, "tool"), Pick::None);

        let both = assets(&[
            "tool-1.0-x86_64-unknown-linux-gnu.tar.gz",
            "tool-1.0-x86_64-unknown-linux-musl.tar.gz",
        ]);
        assert_eq!(
            picked(&both, alpine, "tool"),
            "tool-1.0-x86_64-unknown-linux-musl.tar.gz"
        );
    }

    #[test]
    fn the_same_build_as_tarball_and_zip_is_not_a_tie() {
        // eza publishes both, and before there was a preference this refused
        // the whole release as ambiguous. A zip is the Windows-shaped choice
        // and bedouin does not install on Windows.
        let a = assets(&[
            "eza_x86_64-unknown-linux-musl.tar.gz",
            "eza_x86_64-unknown-linux-musl.zip",
        ]);
        assert_eq!(
            picked(&a, linux64(), "eza"),
            "eza_x86_64-unknown-linux-musl.tar.gz"
        );
        // But a zip on its own is still installable: some projects ship
        // nothing else.
        let only_zip = assets(&["tool-1.0-linux-x86_64.zip"]);
        assert_eq!(
            picked(&only_zip, linux64(), "tool"),
            "tool-1.0-linux-x86_64.zip"
        );
    }

    #[test]
    fn a_tie_is_refused_with_the_names_rather_than_guessed() {
        // Two assets that are equally good answers. Choosing one would be a
        // coin flip presented as a decision.
        let a = assets(&[
            "tool-1.0-linux-x86_64.tar.gz",
            "tool-1.0-linux-x86_64-alt.tar.gz",
        ]);
        match pick(&a, linux64(), "tool") {
            Pick::Ambiguous(mut v) => {
                v.sort();
                assert_eq!(v.len(), 2, "both must be named: {v:?}");
            }
            other => panic!("expected a refusal, got {other:?}"),
        }
    }

    #[test]
    fn a_release_with_nothing_for_this_machine_is_none() {
        let a = assets(&[
            "tool-1.0-windows-x86_64.zip",
            "tool-1.0-darwin-arm64.tar.gz",
        ]);
        assert_eq!(pick(&a, linux64(), "tool"), Pick::None);
    }

    #[test]
    fn arm_is_not_a_prefix_trap() {
        // `arm` matches inside `arm64`. Rejecting on it would reject the very
        // asset an arm64 machine wants.
        let a = assets(&[
            "tool-1.0-linux-arm64.tar.gz",
            "tool-1.0-linux-armv7.tar.gz",
            "tool-1.0-linux-x86_64.tar.gz",
        ]);
        assert_eq!(
            picked(
                &a,
                Target::of(Os::Linux, Arch::Arm64, DistroLike::Debian),
                "tool"
            ),
            "tool-1.0-linux-arm64.tar.gz"
        );
    }

    #[test]
    fn a_bare_binary_with_no_extension_is_installable() {
        let a = assets(&["tool-linux-amd64", "tool-darwin-amd64"]);
        assert_eq!(picked(&a, linux64(), "tool"), "tool-linux-amd64");
    }

    #[test]
    fn every_archive_shape_names_the_tool_that_opens_it() {
        // tar does not decompress xz or bzip2 itself, it execs them, and a
        // minimal image has neither. Checked before the download rather than
        // discovered from `tar (child): xz: Cannot exec` afterwards.
        assert_eq!(form_of("t-1.0.tar.xz").needs(), Some("xz"));
        assert_eq!(form_of("t-1.0.tar.bz2").needs(), Some("bzip2"));
        assert_eq!(form_of("t-1.0.zip").needs(), Some("unzip"));
        assert_eq!(form_of("t-1.0.tar.gz").needs(), Some("gzip"));
        // Nothing to unpack, so nothing to be missing.
        assert_eq!(form_of("t-1.0-linux-amd64").needs(), None);
        assert_eq!(form_of("t.AppImage").needs(), None);
        assert_eq!(form_of("t-1.0.tgz"), Form::TarGz);
        assert_eq!(form_of("t-1.0.txz"), Form::TarXz);
    }

    #[test]
    fn both_shapes_of_published_checksum_are_found() {
        let rel = Release {
            tag: "v1".into(),
            prerelease: false,
            assets: assets(&[
                "tool-linux.tar.gz",
                "tool-linux.tar.gz.sha256",
                "checksums.txt",
            ]),
        };
        let a = rel.assets[0].clone();
        // The sidecar is more specific than the shared file, so it wins.
        assert_eq!(
            checksum_for(&rel, &a).map(|c| c.name.as_str()),
            Some("tool-linux.tar.gz.sha256")
        );
        let shared = Release {
            assets: assets(&["tool-linux.tar.gz", "checksums.txt"]),
            ..rel.clone()
        };
        assert_eq!(
            checksum_for(&shared, &shared.assets[0].clone()).map(|c| c.name.as_str()),
            Some("checksums.txt")
        );
        // fd publishes none, and that is not an error.
        let none = Release {
            assets: assets(&["tool-linux.tar.gz"]),
            ..rel.clone()
        };
        assert_eq!(checksum_for(&none, &none.assets[0].clone()), None);
    }

    #[test]
    fn a_manifest_substitutes_the_placeholders_a_path_carries() {
        let m = Manifest {
            bin: Some("fd-{version}-{target}/fd".into()),
            ..Default::default()
        };
        assert_eq!(
            m.bin_for("v10.5.0", "x86_64-unknown-linux-musl").unwrap(),
            "fd-v10.5.0-x86_64-unknown-linux-musl/fd"
        );
        let m2 = Manifest {
            bin: Some("t-{version_no_v}/t".into()),
            ..Default::default()
        };
        assert_eq!(m2.bin_for("v1.2.3", "x").unwrap(), "t-1.2.3/t");
    }

    #[test]
    fn specs_parse_into_the_four_selectors() {
        assert_eq!(
            parse_spec("sharkdp/fd").unwrap(),
            ("sharkdp".into(), "fd".into(), Selector::Latest)
        );
        assert_eq!(
            parse_spec("o/r@v1.2.3").unwrap().2,
            Selector::Tag("v1.2.3".into())
        );
        assert_eq!(
            parse_spec("o/r@prerelease").unwrap().2,
            Selector::Prerelease
        );
        assert_eq!(
            parse_spec("o/r@/^tui-/").unwrap().2,
            Selector::Pattern("^tui-".into())
        );
        for bad in ["fd", "", "/fd", "sharkdp/", "a/b/c"] {
            assert!(parse_spec(bad).is_err(), "`{bad}` should not parse");
        }
    }

    #[test]
    fn patterns_pick_a_product_out_of_a_shared_repo() {
        // The case the selector exists for: one repository, two products,
        // and `/releases/latest` is whichever was cut last.
        let tags = ["tui-0.2.0", "cli-0.9.0", "tui-0.1.0"];
        let newest_tui = tags.iter().find(|t| matches_pattern(t, "^tui-"));
        assert_eq!(newest_tui, Some(&"tui-0.2.0"));
        assert!(matches_pattern("v1.2.3", "^v1"));
        assert!(matches_pattern("app-v1.2.3", "v1.2.3$"));
        assert!(matches_pattern("v1.2.3", "^v1.2.3$"));
        assert!(!matches_pattern("cli-0.9.0", "^tui-"));
    }
}
