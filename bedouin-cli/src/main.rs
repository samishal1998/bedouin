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
            match serde_json::to_string_pretty(&outcome.facts) {
                Ok(j) => println!("{j}"),
                Err(e) => {
                    eprintln!("bedouin: {e}");
                    return ExitCode::FAILURE;
                }
            }
            ExitCode::SUCCESS
        }
        Command::Plan => {
            print!("{}", outcome.plan.render(cli.verbose));
            // 0 nothing pending, 2 changes pending -- so a CI drift check can
            // just look at the exit status.
            ExitCode::from(outcome.plan.exit_code() as u8)
        }
    }
}
