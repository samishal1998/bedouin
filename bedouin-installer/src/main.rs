//! `bedouin-installer` -- packages from GitHub releases, without the rest of
//! bedouin.
//!
//! Everything here is a thin shell over `bedouin_core::forge`, which is what
//! `bedouin install` uses too. The point of the separate binary is that it
//! needs no config, no state file and no opinion about your machine beyond
//! what it has to know to pick a file.

use bedouin_core::facts::Facts;
use bedouin_core::forge::{self, Pick};
use bedouin_core::host::{Host, OsHost};
use clap::Parser;
use std::process::ExitCode;

#[derive(Parser)]
#[command(
    name = "bedouin-installer",
    version,
    about = "Install a binary from a GitHub release",
    long_about = "Install a binary from a GitHub release.\n\n\
                  `org/repo`, optionally `@tag`, `@prerelease`, or `@/regex/` \
                  for repositories that publish several products from one place."
)]
struct Cli {
    /// `org/repo[@selector]`, e.g. `sharkdp/fd`.
    spec: String,
    /// Use this release file instead of choosing one.
    #[arg(long)]
    asset: Option<String>,
    /// Call the installed binary this.
    #[arg(long)]
    bin: Option<String>,
    /// Write a `curl … | sh` installer to stdout instead of installing.
    #[arg(long)]
    generate: bool,
    /// Where to put it. Defaults to ~/.local/bin.
    #[arg(long)]
    bin_dir: Option<std::path::PathBuf>,
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    let host = OsHost::new();
    let facts = match bedouin_core::probe::facts(&host, None) {
        Ok(f) => f,
        Err(e) => {
            eprintln!("bedouin-installer: {e}");
            return ExitCode::FAILURE;
        }
    };
    run(&host, &facts, &cli)
}

fn run(host: &OsHost, facts: &Facts, cli: &Cli) -> ExitCode {
    let (org, repo, sel) = match forge::parse_spec(&cli.spec) {
        Ok(v) => v,
        Err(e) => return fail(&e),
    };
    let rel = match forge::resolve(host, facts, &org, &repo, &sel) {
        Ok(r) => r,
        Err(e) => return fail(&e),
    };
    let man = forge::manifest(host, &org, &repo, &rel).unwrap_or_default();
    let triple = forge::triple(facts);

    if cli.generate {
        let bin = cli
            .bin
            .clone()
            .or_else(|| man.name.clone())
            .unwrap_or_else(|| repo.clone());
        let (rows, missing) = forge::script_rows(host, &rel, &repo);
        if rows.is_empty() {
            return fail(&format!("{org}/{repo} {} has nothing installable", rel.tag));
        }
        if !missing.is_empty() {
            eprintln!(
                "bedouin-installer: no build for {}; the script will say so",
                missing.join(", ")
            );
        }
        print!("{}", forge::generate(&org, &repo, &rel, &rows, &bin));
        return ExitCode::SUCCESS;
    }

    let asset = match cli.asset.as_deref() {
        Some(name) => match rel.assets.iter().find(|a| a.name == name) {
            Some(a) => a.clone(),
            None => return fail(&format!("{org}/{repo} {} has no asset `{name}`", rel.tag)),
        },
        None => match man
            .asset_for(&rel.tag, &triple)
            .and_then(|n| rel.assets.iter().find(|a| a.name == n).cloned())
        {
            Some(a) => a,
            None => {
                let t = forge::Target::of(facts.os, facts.arch, facts.distro_like);
                match forge::pick(&rel.assets, t, &repo) {
                    Pick::One(a) => a,
                    Pick::Ambiguous(names) => {
                        eprintln!(
                            "bedouin-installer: {} files fit this machine equally well:",
                            names.len()
                        );
                        for n in &names {
                            eprintln!("    {n}");
                        }
                        return fail("Pick one with --asset <name>");
                    }
                    Pick::None => {
                        eprintln!(
                            "bedouin-installer: nothing in {org}/{repo} {} runs here:",
                            rel.tag
                        );
                        for a in &rel.assets {
                            eprintln!("    {}", a.name);
                        }
                        return ExitCode::FAILURE;
                    }
                }
            }
        },
    };

    let checksum = man
        .checksums
        .as_ref()
        .and_then(|w| rel.assets.iter().find(|a| &a.name == w).cloned())
        .or_else(|| forge::checksum_for(&rel, &asset).cloned());

    let dest = cli.bin_dir.clone().unwrap_or_else(|| forge::bin_dir(facts));

    println!("{}/{}  {}", org, repo, rel.tag);
    println!("  {}", asset.name);
    let plan = forge::Plan {
        org,
        repo,
        tag: rel.tag.clone(),
        asset,
        checksum,
        bin_name: cli.bin.clone().or_else(|| man.name.clone()),
        bin_path: man.bin_for(&rel.tag, &triple),
    };
    let env: std::collections::BTreeMap<String, String> = host.env().clone();
    match forge::install(host, facts, &dest, &plan, &env, |m| println!("  {m}")) {
        Ok(p) => {
            println!("installed {}", p.display());
            ExitCode::SUCCESS
        }
        Err(e) => fail(&e),
    }
}

fn fail(msg: &str) -> ExitCode {
    eprintln!("bedouin-installer: {msg}");
    ExitCode::FAILURE
}
