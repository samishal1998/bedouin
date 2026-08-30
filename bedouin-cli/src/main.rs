//! The only thing that runs on a fresh machine.

use bedouin_core::host::OsHost;
use bedouin_core::run;
use clap::{Parser, Subcommand};
use std::path::PathBuf;
use std::process::ExitCode;

#[derive(Parser)]
#[command(name = "bedouin", version, about = "Declarative environment manager")]
struct Cli {
    /// Config file. Otherwise $BEDOUIN_CONFIG, ./bedouin.yaml, then
    /// ~/.config/bedouin/bedouin.yaml.
    #[arg(short, long, global = true)]
    config: Option<PathBuf>,
    /// Show which arm won for each conditional value, and what `only:` pruned.
    #[arg(short, long, global = true)]
    verbose: bool,
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Show what apply would do. Exits 2 when changes are pending.
    Plan {
        /// Also write the plan, so `apply -f` can run exactly this one later.
        #[arg(short, long, value_name = "FILE")]
        out: Option<PathBuf>,
    },
    /// Print the resolved facts for this machine.
    Facts,
    /// Write a starter config here.
    Init,
    /// Pull the config repository, then apply what changed.
    Sync {
        #[arg(short = 'y', long)]
        yes: bool,
    },
    /// Add a package to the config, then apply.
    Add {
        /// `manager:package` or `manager:package@version`, e.g. `cargo:zellij@0.40.1`.
        spec: String,
        #[arg(long)]
        no_apply: bool,
        #[arg(short = 'y', long)]
        yes: bool,
    },
    /// Report managed content that changed since the last apply.
    Doctor,
    /// Drop a package or language from the config, then apply.
    Remove {
        /// The package name. Use --language for a toolchain.
        name: String,
        /// Remove from `languages:` rather than `packages:`.
        #[arg(long)]
        language: bool,
        /// Edit the config and stop, without applying.
        #[arg(long)]
        no_apply: bool,
        #[arg(short = 'y', long)]
        yes: bool,
    },
    /// Make the machine match the config.
    Apply {
        /// Apply a plan written earlier by `plan -o`, rather than re-planning.
        #[arg(short = 'f', long, value_name = "FILE")]
        plan: Option<PathBuf>,
        /// Show what would change and stop. Same as `plan`.
        #[arg(long)]
        dry_run: bool,
        /// Skip the confirmation prompt.
        #[arg(short = 'y', long)]
        yes: bool,
    },
}

fn run_apply(host: &OsHost, outcome: run::Outcome, _verbose: bool) -> ExitCode {
    // Exclusive for the length of the run: two applies sharing one state file
    // is how an item ends up owned by neither.
    let _lock = match bedouin_core::host::StateLock::acquire(
        &bedouin_core::state::default_path(&outcome.facts.home),
    ) {
        Ok(l) => l,
        Err(e) => {
            eprintln!("bedouin: {e}");
            return ExitCode::FAILURE;
        }
    };

    // sudo's timestamp expires after 15 minutes by default and a real apply can
    // outlast that, so "one prompt, up front" needs refreshing to stay true.
    let _keepalive = (outcome.facts.privilege
        == bedouin_core::facts::Privilege::Password
        && outcome.plan.changes().any(|i| i.needs_root))
    .then(bedouin_core::host::SudoKeepalive::start);

    println!();
    let report = match bedouin_core::apply::apply(
        &outcome.plan,
        &outcome.config,
        &outcome.facts,
        outcome.state,
        host,
        &mut |line| match line {
            bedouin_core::host::Line::Out(s) => println!("  {s}"),
            bedouin_core::host::Line::Err(s) => eprintln!("  {s}"),
        },
    ) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("bedouin: {e}");
            return ExitCode::FAILURE;
        }
    };
    print!("{}", report.render());
    if report.ok() {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    }
}

/// Applying changes a machine, so say so and wait.
fn confirm() -> bool {
    use std::io::Write;
    print!("\nApply these changes? [y/N] ");
    let _ = std::io::stdout().flush();
    let mut answer = String::new();
    if std::io::stdin().read_line(&mut answer).is_err() {
        return false;
    }
    matches!(answer.trim().to_ascii_lowercase().as_str(), "y" | "yes")
}

const STARTER: &str = r#"# bedouin.yaml -- one config, every machine.
#
# `bedouin plan` shows what would change; `bedouin apply` makes it so.
# Anything conditional is written as a mapping: `{ macos: brew, default: apt }`.
version: 0

# The shell you are configuring. On a fresh box this is usually NOT the shell
# you are running, which is why it is declared rather than detected.
shell: zsh

vars:
  editor: nvim

# Named conditions. Use one wherever a built-in arm name is not enough --
# a distro version, a hostname, an environment variable.
# targets:
#   - name: work
#     match: { env: { BEDOUIN_PROFILE: work } }

aliases:
  ll: ls -alh

packages:
  - name: jq
    from: { macos: brew, debian-like: apt, suse-like: zypper }

# files:
#   - src: templates/gitconfig.j2
#     dest: ~/.gitconfig
"#;

fn git(root: &std::path::Path, args: &[&str]) -> Result<String, String> {
    let out = std::process::Command::new("git")
        .arg("-C")
        .arg(root)
        .args(args)
        .output()
        .map_err(|e| format!("git: {e}"))?;
    if !out.status.success() {
        return Err(String::from_utf8_lossy(&out.stderr).trim().to_string());
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    let host = OsHost::new();
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));

    // `init` has no config to load yet, so it runs before the pipeline.
    if matches!(cli.command, Command::Init) {
        let target = cli.config.clone().unwrap_or_else(|| cwd.join("bedouin.yaml"));
        if target.exists() {
            eprintln!(
                "bedouin: {} already exists. Refusing to overwrite it",
                target.display()
            );
            return ExitCode::FAILURE;
        }
        if let Some(parent) = target.parent() {
            let _ = std::fs::create_dir_all(parent);
            let _ = std::fs::create_dir_all(parent.join("templates"));
        }
        if let Err(e) = std::fs::write(&target, STARTER) {
            eprintln!("bedouin: {}: {e}", target.display());
            return ExitCode::FAILURE;
        }
        println!("Wrote {}", target.display());
        println!("Next: edit it, then `bedouin plan`.");
        return ExitCode::SUCCESS;
    }

    let outcome = match run::plan(&host, cli.config.as_deref(), &cwd) {
        Ok(o) => o,
        Err(e) => {
            eprintln!("bedouin: {e}");
            return ExitCode::FAILURE;
        }
    };

    match cli.command {
        Command::Facts => {
            // Names only. The values are the same secrets class the plan
            // artifact withholds, and this output ends up in bug reports.
            let mut facts = outcome.facts.clone();
            facts.env = facts
                .env
                .keys()
                .map(|k| (k.clone(), "<set>".to_string()))
                .collect();
            match serde_json::to_string_pretty(&facts) {
                Ok(j) => println!("{j}"),
                Err(e) => {
                    eprintln!("bedouin: {e}");
                    return ExitCode::FAILURE;
                }
            }
            ExitCode::SUCCESS
        }
        Command::Sync { yes } => {
            let root = &outcome.loaded.root;
            // A dirty tree means uncommitted local edits; a pull would either
            // fail or bury them. Neither is ours to decide.
            match git(root, &["status", "--porcelain"]) {
                Err(e) => {
                    eprintln!("bedouin: {} is not a git repository, or git failed: {e}", root.display());
                    return ExitCode::FAILURE;
                }
                Ok(s) if !s.is_empty() => {
                    eprintln!("bedouin: {} has uncommitted changes:", root.display());
                    eprintln!("{s}");
                    eprintln!("  Commit or stash them first -- sync will not decide what happens to your edits");
                    return ExitCode::FAILURE;
                }
                Ok(_) => {}
            }
            // --ff-only: sync pulls, it does not merge. A divergence is a
            // decision for the user.
            match git(root, &["pull", "--ff-only"]) {
                Ok(out) => println!("{out}"),
                Err(e) => {
                    eprintln!("bedouin: git pull failed: {e}");
                    return ExitCode::FAILURE;
                }
            }
            // Re-plan against what was just pulled. The plan IS the diff.
            let after = match run::plan(&host, cli.config.as_deref(), &cwd) {
                Ok(o) => o,
                Err(e) => {
                    eprintln!("bedouin: {e}");
                    return ExitCode::FAILURE;
                }
            };
            if !after.plan.has_changes() {
                println!("Up to date; nothing to apply.");
                return ExitCode::SUCCESS;
            }
            print!("{}", after.plan.render(cli.verbose));
            if !yes && !confirm() {
                println!("Pulled, nothing applied.");
                return ExitCode::SUCCESS;
            }
            run_apply(&host, after, cli.verbose)
        }

        Command::Add {
            spec,
            no_apply,
            yes,
        } => {
            let Some((manager, rest)) = spec.split_once(':') else {
                eprintln!(
                    "bedouin: `{spec}` is not `manager:package`.\n                       For example: `bedouin add apt:ripgrep` or `bedouin add cargo:zellij@0.40.1`"
                );
                return ExitCode::FAILURE;
            };
            let (name, version) = match rest.split_once('@') {
                Some((n, v)) => (n, Some(v)),
                None => (rest, None),
            };
            if bedouin_core::facts::Manager::parse(manager).is_none() {
                let known: Vec<&str> = bedouin_core::facts::Manager::ALL
                    .iter()
                    .map(|m| m.as_str())
                    .collect();
                eprintln!("bedouin: unknown manager `{manager}`\n  known: {}", known.join(", "));
                return ExitCode::FAILURE;
            }

            let entry = &outcome.loaded.entry;
            let text = match std::fs::read_to_string(entry) {
                Ok(t) => t,
                Err(e) => {
                    eprintln!("bedouin: {}: {e}", entry.display());
                    return ExitCode::FAILURE;
                }
            };
            let edited = match bedouin_core::edit::add_package(&text, name, manager, version) {
                Ok(t) => t,
                Err(e) => {
                    eprintln!("bedouin: {e}");
                    return ExitCode::FAILURE;
                }
            };
            if let Err(e) = std::fs::write(entry, &edited) {
                eprintln!("bedouin: {}: {e}", entry.display());
                return ExitCode::FAILURE;
            }
            println!("Added `{name}` from `{manager}` to {}.", entry.display());
            if no_apply {
                return ExitCode::SUCCESS;
            }
            let after = match run::plan(&host, cli.config.as_deref(), &cwd) {
                Ok(o) => o,
                Err(e) => {
                    eprintln!("bedouin: {e}\n  The config was edited; fix it or run `bedouin remove {name}`.");
                    return ExitCode::FAILURE;
                }
            };
            if !after.plan.has_changes() {
                println!("Already on this machine; nothing to do.");
                return ExitCode::SUCCESS;
            }
            print!("{}", after.plan.render(cli.verbose));
            if !yes && !confirm() {
                println!("Config edited, nothing applied.");
                return ExitCode::SUCCESS;
            }
            run_apply(&host, after, cli.verbose)
        }

        Command::Init => unreachable!("handled before the config is loaded"),

        Command::Doctor => {
            let report = match bedouin_core::doctor::check(
                &outcome.state,
                &outcome.config,
                &outcome.facts,
                &host,
            ) {
                Ok(r) => r,
                Err(e) => {
                    eprintln!("bedouin: {e}");
                    return ExitCode::FAILURE;
                }
            };
            print!("{}", report.render(cli.verbose));
            // 0 clean, 2 drift -- mirroring `plan`, so a CI check reads the same.
            ExitCode::from(report.exit_code() as u8)
        }

        Command::Remove {
            name,
            language,
            no_apply,
            yes,
        } => {
            use bedouin_core::edit::{remove_entry, Section};
            let section = if language {
                Section::Languages
            } else {
                Section::Packages
            };
            let entry = &outcome.loaded.entry;

            // A package that was already on the machine is adopted, never
            // owned -- dropping it from the config must not read as a promise
            // to uninstall it.
            let id = format!(
                "{}/{name}",
                if language { "language" } else { "package" }
            );
            if outcome
                .state
                .items
                .get(&id)
                .is_some_and(|i| i.owner == bedouin_core::state::Owner::Preexisting)
            {
                eprintln!(
                    "bedouin: `{name}` was already on this machine when bedouin first ran,
                       so bedouin does not own it and will not uninstall it.
                       Removing it from the config is safe; use your package manager to                      uninstall it."
                );
            }

            let text = match std::fs::read_to_string(entry) {
                Ok(t) => t,
                Err(e) => {
                    eprintln!("bedouin: {}: {e}", entry.display());
                    return ExitCode::FAILURE;
                }
            };
            let edited = match remove_entry(&text, section, &name) {
                Ok(t) => t,
                Err(e) => {
                    eprintln!("bedouin: {e}");
                    return ExitCode::FAILURE;
                }
            };
            if let Err(e) = std::fs::write(entry, &edited) {
                eprintln!("bedouin: {}: {e}", entry.display());
                return ExitCode::FAILURE;
            }
            println!("Removed {} `{name}` from {}.", section.label(), entry.display());
            if no_apply {
                println!("Config edited only. Run `bedouin apply` when ready.");
                return ExitCode::SUCCESS;
            }

            // Re-plan against the edited config: the removal is a plan outcome
            // like any other, not a special path.
            let after = match run::plan(&host, cli.config.as_deref(), &cwd) {
                Ok(o) => o,
                Err(e) => {
                    eprintln!("bedouin: {e}");
                    return ExitCode::FAILURE;
                }
            };
            if !after.plan.has_changes() {
                println!("Nothing to undo on this machine.");
                return ExitCode::SUCCESS;
            }
            print!("{}", after.plan.render(cli.verbose));
            if !yes && !confirm() {
                println!("Config edited, nothing applied.");
                return ExitCode::SUCCESS;
            }
            run_apply(&host, after, cli.verbose)
        }

        Command::Apply {
            plan: from_file,
            dry_run,
            yes,
        } => {
            if let Some(file) = from_file {
                // Facts and config come from the artifact, so the environment
                // it froze is the environment this run sees.
                println!("Applying the plan in {}.\n", file.display());
                let report = match run::apply_artifact(&host, &file, &mut |line| match line {
                    bedouin_core::host::Line::Out(s) => println!("  {s}"),
                    bedouin_core::host::Line::Err(s) => eprintln!("  {s}"),
                }) {
                    Ok(r) => r,
                    Err(e) => {
                        eprintln!("bedouin: {e}");
                        return ExitCode::FAILURE;
                    }
                };
                print!("{}", report.render());
                return if report.ok() {
                    ExitCode::SUCCESS
                } else {
                    ExitCode::FAILURE
                };
            }
            if dry_run {
                print!("{}", outcome.plan.render(cli.verbose));
                return ExitCode::from(outcome.plan.exit_code() as u8);
            }
            if !outcome.plan.has_changes() {
                println!("No changes. The machine already matches the config.");
                return ExitCode::SUCCESS;
            }
            print!("{}", outcome.plan.render(cli.verbose));
            if !yes && !confirm() {
                println!("Nothing applied.");
                return ExitCode::SUCCESS;
            }
            run_apply(&host, outcome, cli.verbose)
        }
        Command::Plan { out } => {
            print!("{}", outcome.plan.render(cli.verbose));
            if let Some(to) = out {
                let a = bedouin_core::artifact::build(
                    &outcome.plan,
                    &outcome.config,
                    &outcome.facts,
                    &outcome.loaded.raw,
                    &outcome.state,
                    &outcome.loaded.root,
                );
                if let Err(e) = bedouin_core::artifact::write(&a, &host, &to) {
                    eprintln!("bedouin: {e}");
                    return ExitCode::FAILURE;
                }
                println!("\nPlan written to {} (mode 0600).", to.display());
            }
            // 0 nothing pending, 2 changes pending -- so a CI drift check can
            // just look at the exit status.
            ExitCode::from(outcome.plan.exit_code() as u8)
        }
    }
}
