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
    Plan,
    /// Print the resolved facts for this machine.
    Facts,
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
        /// Show what would change and stop. Same as `plan`.
        #[arg(long)]
        dry_run: bool,
        /// Skip the confirmation prompt.
        #[arg(short = 'y', long)]
        yes: bool,
    },
}

fn run_apply(host: &OsHost, outcome: run::Outcome, _verbose: bool) -> ExitCode {
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

fn main() -> ExitCode {
    let cli = Cli::parse();
    let host = OsHost::new();
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));

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

        Command::Apply { dry_run, yes } => {
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
        Command::Plan => {
            print!("{}", outcome.plan.render(cli.verbose));
            // 0 nothing pending, 2 changes pending -- so a CI drift check can
            // just look at the exit status.
            ExitCode::from(outcome.plan.exit_code() as u8)
        }
    }
}
