//! `bedouin install org/repo` -- a package manager whose repository is GitHub.
//!
//! The resolving and choosing live in `bedouin_core::forge`, which has no side
//! effects and is tested against real releases. This file is the part that
//! talks to a person: what to print, what to refuse, and what to write into
//! the config afterwards.

use bedouin_core::facts::Facts;
use bedouin_core::forge::{self, Pick};
use bedouin_core::host::{Host, OsHost};
use bedouin_core::style;
use std::process::ExitCode;

pub fn run(
    host: &OsHost,
    facts: &Facts,
    spec: &str,
    asset_override: Option<&str>,
    bin_override: Option<&str>,
    yes: bool,
) -> ExitCode {
    let (org, repo, sel) = match forge::parse_spec(spec) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("bedouin: {e}");
            return ExitCode::FAILURE;
        }
    };

    let rel = match forge::resolve(host, facts, &org, &repo, &sel) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("bedouin: {e}");
            return ExitCode::FAILURE;
        }
    };
    if rel.assets.is_empty() {
        eprintln!(
            "bedouin: {org}/{repo} {} has no downloadable files\n  \
             Some projects publish only source archives, which bedouin cannot install",
            rel.tag
        );
        return ExitCode::FAILURE;
    }

    let man = forge::manifest(host, &org, &repo, &rel).unwrap_or_default();
    let triple = forge::triple(facts);

    // Named asset wins, then the manifest's map, then autodetect.
    let asset = if let Some(name) = asset_override {
        match rel.assets.iter().find(|a| a.name == name) {
            Some(a) => a.clone(),
            None => {
                eprintln!("bedouin: {org}/{repo} {} has no asset `{name}`", rel.tag);
                list_assets(&rel);
                return ExitCode::FAILURE;
            }
        }
    } else if let Some(name) = man.asset_for(&rel.tag, &triple) {
        match rel.assets.iter().find(|a| a.name == name) {
            Some(a) => a.clone(),
            None => {
                eprintln!(
                    "bedouin: the manifest names `{name}` for {triple}, and the release \
                     does not have it"
                );
                return ExitCode::FAILURE;
            }
        }
    } else {
        let t = forge::Target::of(facts.os, facts.arch, facts.distro_like);
        match forge::pick(&rel.assets, t, &repo) {
            Pick::One(a) => a,
            // Refusing is the point. Two equally good answers is a coin flip,
            // and presenting a coin flip as a decision is how the wrong binary
            // ends up on somebody's PATH.
            Pick::Ambiguous(names) => {
                eprintln!(
                    "bedouin: {org}/{repo} {} has {} files that fit this machine equally well:",
                    rel.tag,
                    names.len()
                );
                for n in &names {
                    eprintln!("    {n}");
                }
                eprintln!("  Pick one with `--asset <name>`");
                return ExitCode::FAILURE;
            }
            Pick::None => {
                eprintln!(
                    "bedouin: nothing in {org}/{repo} {} runs on {} {}",
                    rel.tag, facts.os, facts.arch
                );
                list_assets(&rel);
                eprintln!("  Name one anyway with `--asset <name>`");
                return ExitCode::FAILURE;
            }
        }
    };

    // Only an explicit request fixes the name; otherwise the archive decides,
    // which is how `BurntSushi/ripgrep` installs as `rg`.
    let bin_name = bin_override
        .map(str::to_string)
        .or_else(|| man.name.clone());

    println!("  {org}/{repo}  {}", style::bold(&rel.tag));
    if let Some(d) = &man.description {
        println!("  {}", style::dim(d));
    }
    println!("  asset   {}", asset.name);
    match &bin_name {
        Some(n) => println!("  binary  ~/.local/bin/{n}"),
        None => println!("  binary  ~/.local/bin/ (named by the archive)"),
    }
    if rel.prerelease {
        println!("  {}", style::dim("this release is marked prerelease"));
    }
    if !yes && !crate::release::confirm("Install it?") {
        println!("Nothing done.");
        return ExitCode::SUCCESS;
    }

    let checksum = man
        .checksums
        .as_ref()
        .and_then(|want| rel.assets.iter().find(|a| &a.name == want).cloned())
        .or_else(|| forge::checksum_for(&rel, &asset).cloned());

    let plan = forge::Plan {
        org: org.clone(),
        repo: repo.clone(),
        tag: rel.tag.clone(),
        asset,
        checksum,
        bin_name: bin_name.clone(),
        bin_path: man.bin_for(&rel.tag, &triple),
    };
    let env: std::collections::BTreeMap<String, String> = host.env().clone();
    match forge::install(host, facts, &plan, &env, |m| {
        println!("  {}", style::dim(m))
    }) {
        Ok(path) => {
            println!("{} {}", style::green("✓"), path.display());
            if bedouin_core::plan::system_path(facts)
                .iter()
                .all(|p| p != &forge::bin_dir(facts))
            {
                println!(
                    "  {}",
                    style::dim("~/.local/bin is not on this shell's PATH; open a new shell")
                );
            }
            println!(
                "  {}",
                style::dim(&format!(
                    "To keep it: bedouin add github:{org}/{repo}@{}",
                    rel.tag
                ))
            );
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("bedouin: {e}");
            ExitCode::FAILURE
        }
    }
}

fn list_assets(rel: &forge::Release) {
    eprintln!("  The release has:");
    for a in &rel.assets {
        eprintln!("    {}", a.name);
    }
}
