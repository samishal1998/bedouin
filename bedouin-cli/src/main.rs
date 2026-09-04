//! The only thing that runs on a fresh machine.

mod sidecar;
#[cfg(feature = "tui")]
mod tui;

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

#[derive(Subcommand, Clone)]
enum DaemonAction {
    /// Write the unit file and print how to enable it.
    Install {
        /// Seconds between reconciles.
        #[arg(long, default_value_t = 900)]
        interval: u64,
    },
    /// Remove the unit file.
    Uninstall,
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
    /// Set an alias in the config, then apply.
    Alias {
        /// `name=value`, e.g. `gs=git status`.
        spec: String,
        /// Scope it to a package instead of making it global.
        #[arg(long, value_name = "PACKAGE")]
        package: Option<String>,
        #[arg(long)]
        no_apply: bool,
        #[arg(short = 'y', long)]
        yes: bool,
    },
    /// Set a package's completion generator, then apply.
    Completions {
        /// The package the completions belong to.
        package: String,
        /// The command that prints them, e.g. `-- zellij setup --dump-completion {{ shell.name }}`.
        #[arg(last = true, required = true)]
        generate: Vec<String>,
        #[arg(long)]
        no_apply: bool,
        #[arg(short = 'y', long)]
        yes: bool,
    },
    /// Add a package to the config, then apply.
    Add {
        /// `manager:package` or `manager:package@version`, e.g. `cargo:zellij@0.40.1`.
        spec: String,
        /// Also give it an alias: `--alias z=zellij`. Repeatable.
        #[arg(long, value_name = "NAME=VALUE")]
        alias: Vec<String>,
        /// Also set its completion generator, as one command string.
        #[arg(long, value_name = "COMMAND")]
        completions: Option<String>,
        /// Also add a PATH entry it owns. Repeatable.
        #[arg(long, value_name = "DIR")]
        path: Vec<String>,
        #[arg(long)]
        no_apply: bool,
        #[arg(short = 'y', long)]
        yes: bool,
    },
    /// Apply once if anything drifted, then exit. What the timer runs.
    Reconcile {
        /// Stay resident and re-check on an interval instead of exiting.
        #[arg(long)]
        watch: bool,
        /// Seconds between checks under --watch.
        #[arg(long, default_value_t = 900)]
        interval: u64,
    },
    /// Write the unit file that runs `reconcile` unattended.
    Daemon {
        #[command(subcommand)]
        action: DaemonAction,
    },
    /// Lift hand edits of managed content back into the config.
    Absorb {
        /// Absorb everything absorbable without asking.
        #[arg(short = 'y', long)]
        yes: bool,
    },
    /// List the environment variables this config reads.
    Env {
        /// Write a commented .env.bedouin beside the config.
        #[arg(long)]
        write: bool,
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
        /// Steps not to run, by id (`package/jq`) or name (`jq`), comma
        /// separated. For the one step a machine cannot do yet -- a package
        /// from a repository you have not added -- so it does not strand the
        /// rest of the run.
        #[arg(long, value_name = "STEP", value_delimiter = ',')]
        skip: Vec<String>,
    },
    /// Show the plan on screen, with a key to apply it.
    #[cfg(feature = "tui")]
    Tui,
    /// Serve the web UI. Runs `bedouin-ui`, fetching it once if it is absent:
    /// the HTTP stack and the built assets live there, not in this binary.
    Ui {
        #[arg(short, long, default_value_t = 7777)]
        port: u16,
        /// Fetch `bedouin-ui` without asking, if it is missing.
        #[arg(short = 'y', long)]
        yes: bool,
    },
    /// Print bedouin's own completion script. Hidden: the plan runs it for
    /// you, and `bedouin completions` is the one that takes a package.
    #[command(hide = true)]
    CompletionScript {
        /// bash, zsh or fish.
        shell: String,
    },
}

fn run_apply(
    host: &OsHost,
    outcome: run::Outcome,
    _verbose: bool,
    skip: &std::collections::BTreeSet<String>,
) -> ExitCode {
    // Exclusive for the length of the run: two applies sharing one state file
    // is how an item ends up owned by neither.
    let _lock = match bedouin_core::host::StateLock::acquire(&bedouin_core::state::default_path(
        &outcome.facts.home,
    )) {
        Ok(l) => l,
        Err(e) => {
            eprintln!("bedouin: {e}");
            return ExitCode::FAILURE;
        }
    };

    // sudo's timestamp expires after 15 minutes by default and a real apply can
    // outlast that, so "one prompt, up front" needs refreshing to stay true.
    let _keepalive = (outcome.facts.privilege == bedouin_core::facts::Privilege::Password
        && outcome.plan.changes().any(|i| i.needs_root))
    .then(bedouin_core::host::SudoKeepalive::start);

    println!();
    let report = match bedouin_core::apply::apply(
        &outcome.plan,
        &outcome.config,
        &outcome.facts,
        outcome.state,
        host,
        skip,
        &mut print_line,
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

/// How a step's output looks. Command chatter is dimmed and indented so the
/// headings between steps are what the eye catches; a heading is bold, and
/// stderr is red.
fn print_line(line: bedouin_core::host::Line) {
    use bedouin_core::host::Line;
    use bedouin_core::style;
    match line {
        Line::Step { index, total, id } => println!(
            "\n{} {}",
            style::blue("::"),
            style::bold(&format!("[{index}/{total}] {id}"))
        ),
        // The CLI already shows failure in the report; a tick per step would
        // double every line for no gain.
        Line::StepEnd { .. } => {}
        Line::Out(s) => println!("   {}", style::dim(&s)),
        Line::Err(s) => eprintln!("   {}", style::red(&s)),
    }
}

fn confirm_absorb() -> bool {
    use std::io::Write;
    print!("  Lift this into the config? [y/N] ");
    let _ = std::io::stdout().flush();
    let mut a = String::new();
    if std::io::stdin().read_line(&mut a).is_err() {
        return false;
    }
    matches!(a.trim().to_ascii_lowercase().as_str(), "y" | "yes")
}

/// Write an edited config, then prove it still loads.
///
/// The edit was written before anything checked it could still be read, and
/// `main` loads the config before dispatching ANY subcommand -- so a bad edit
/// bricked the tool: plan, doctor, facts and remove itself all died at load,
/// and the only way out was a text editor. Back up, write, verify, restore on
/// failure.
fn write_config_verified(
    host: &OsHost,
    entry: &std::path::Path,
    text: &str,
    cli_config: Option<&std::path::Path>,
    cwd: &std::path::Path,
) -> Result<run::Outcome, String> {
    let original = std::fs::read_to_string(entry).map_err(|e| e.to_string())?;
    let backup = PathBuf::from(format!("{}.bedouin-bak", entry.display()));
    std::fs::write(&backup, &original).map_err(|e| format!("{}: {e}", backup.display()))?;
    std::fs::write(entry, text).map_err(|e| format!("{}: {e}", entry.display()))?;

    match run::plan(host, cli_config, cwd) {
        Ok(o) => {
            let _ = std::fs::remove_file(&backup);
            Ok(o)
        }
        Err(e) => {
            // Put it back exactly as it was, rather than leaving the user with
            // a config the tool itself can no longer read.
            let _ = std::fs::write(entry, &original);
            let _ = std::fs::remove_file(&backup);
            Err(format!(
                "{e}\n  The edit would have left a config bedouin cannot load, so \
                 {} has been restored unchanged.",
                entry.display()
            ))
        }
    }
}

/// `bedouin facts`, on the loaded document and the probe alone.
fn cmd_facts(host: &OsHost, config: Option<&std::path::Path>, cwd: &std::path::Path) -> ExitCode {
    let (_, facts) = match run::load_only(host, config, cwd) {
        Ok(x) => x,
        Err(e) => {
            eprintln!("bedouin: {e}");
            return ExitCode::FAILURE;
        }
    };
    // Names only. The values are the same secrets class the plan artifact
    // withholds, and this output ends up in bug reports.
    let mut facts = facts;
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

fn cmd_env(
    host: &OsHost,
    config: Option<&std::path::Path>,
    cwd: &std::path::Path,
    write: bool,
) -> ExitCode {
    let (loaded, facts) = match run::load_only(host, config, cwd) {
        Ok(x) => x,
        Err(e) => {
            eprintln!("bedouin: {e}");
            return ExitCode::FAILURE;
        }
    };
    use bedouin_core::envfile;
    let refs = envfile::referenced(&loaded.raw, &facts.env, &loaded.root, host);
    if refs.is_empty() {
        println!("This config reads no environment variables.");
        return ExitCode::SUCCESS;
    }

    println!("Variables this config reads:\n");
    let w = refs.iter().map(|r| r.name.len()).max().unwrap_or(4);
    for r in &refs {
        // Names and set/unset, never values -- this output lands in
        // bug reports.
        println!(
            "  {:<w$}  {:<7}  {}{}",
            r.name,
            if r.set { "set" } else { "not set" },
            r.site,
            // A `match:` on an unset variable is not a failure -- the target
            // simply does not match. Different from a template guarded by
            // `| default(...)`, and worth saying differently.
            //
            // From `match_key`, not from the site string: `targets.work.vars.x`
            // starts with `targets.` too, and is an ordinary template read that
            // fails like any other.
            if r.match_key {
                "   (a target; unset just means it will not match)"
            } else if r.has_default {
                "   (has a default)"
            } else {
                ""
            },
            w = w
        );
    }
    let unset: Vec<&_> = refs.iter().filter(|r| !r.set).collect();
    let risky = unset.iter().filter(|r| !r.has_default).count();
    println!();
    if unset.is_empty() {
        println!("All set.");
    } else {
        println!("{} of {} unset.", unset.len(), refs.len());
        if risky > 0 {
            let (s_, v) = if risky == 1 {
                ("", "has")
            } else {
                ("s", "have")
            };
            println!("{risky} of those{s_} {v} no default and will fail to resolve.");
        }
    }

    if write {
        let path = envfile::path_beside(&loaded.root);
        if path.exists() {
            eprintln!(
                "bedouin: {} already exists. Refusing to overwrite it",
                path.display()
            );
            return ExitCode::FAILURE;
        }
        if let Err(e) = std::fs::write(&path, envfile::scaffold(&refs)) {
            eprintln!("bedouin: {}: {e}", path.display());
            return ExitCode::FAILURE;
        }
        println!("\nWrote {}", path.display());
        println!("Bedouin reads it before resolving facts, so what you put there takes effect.");

        // It holds values; it does not belong in the repository.
        let gi = loaded.root.join(".gitignore");
        if gi.exists() {
            let cur = std::fs::read_to_string(&gi).unwrap_or_default();
            if !cur.lines().any(|l| l.trim() == envfile::FILE_NAME) {
                let sep = if cur.ends_with('\n') || cur.is_empty() {
                    ""
                } else {
                    "\n"
                };
                if std::fs::write(&gi, format!("{cur}{sep}{}\n", envfile::FILE_NAME)).is_ok() {
                    println!("Added it to {}", gi.display());
                }
            }
        } else {
            println!("It holds values -- add it to your .gitignore.");
        }
    }
    ExitCode::SUCCESS
}

/// Set a package's `completions.generate`, as a YAML argv list.
fn set_completions(
    text: &str,
    package: &str,
    argv: &[String],
) -> Result<String, bedouin_core::schema::ConfigError> {
    let list = argv
        .iter()
        .map(|a| format!("\"{}\"", a.replace('\\', "\\\\").replace('"', "\\\"")))
        .collect::<Vec<_>>()
        .join(", ");
    // `generate:` sits under `completions:`, so the field is the whole block.
    bedouin_core::edit::set_field(
        text,
        bedouin_core::edit::Section::Packages,
        package,
        "completions",
        &format!("{{ generate: [{list}] }}"),
    )
}

/// Split a command string into argv.
///
/// Whitespace separates words, except inside `{{ ... }}` -- a naive split turns
/// `{{ shell.name }}` into three arguments and the template stops being one.
/// Quotes group too, so a generator with a real argument survives. Anything
/// with actual shell syntax should use the `--` form, which needs no guessing.
fn shell_words(s: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut depth = 0usize;
    let mut quote: Option<char> = None;
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '{' if chars.peek() == Some(&'{') => {
                depth += 1;
                cur.push(c);
                cur.push(chars.next().unwrap_or('{'));
            }
            '}' if chars.peek() == Some(&'}') && depth > 0 => {
                depth -= 1;
                cur.push(c);
                cur.push(chars.next().unwrap_or('}'));
            }
            '\'' | '"' if quote.is_none() => quote = Some(c),
            c if Some(c) == quote => quote = None,
            c if c.is_whitespace() && depth == 0 && quote.is_none() => {
                if !cur.is_empty() {
                    out.push(std::mem::take(&mut cur));
                }
            }
            c => cur.push(c),
        }
    }
    if !cur.is_empty() {
        out.push(cur);
    }
    out
}

/// Edit the config, verify it still loads, then apply -- the shape every
/// config-editing command shares.
#[allow(clippy::too_many_arguments)]
fn edit_then_apply(
    host: &OsHost,
    outcome: &run::Outcome,
    cli_config: Option<&std::path::Path>,
    verbose: bool,
    cwd: &std::path::Path,
    no_apply: bool,
    yes: bool,
    edit: impl FnOnce(&str) -> Result<String, bedouin_core::schema::ConfigError>,
    done: &str,
) -> ExitCode {
    let entry = &outcome.loaded.entry;
    let text = match std::fs::read_to_string(entry) {
        Ok(t) => t,
        Err(e) => {
            eprintln!("bedouin: {}: {e}", entry.display());
            return ExitCode::FAILURE;
        }
    };
    let edited = match edit(&text) {
        Ok(t) => t,
        Err(e) => {
            eprintln!("bedouin: {e}");
            return ExitCode::FAILURE;
        }
    };
    let after = match write_config_verified(host, entry, &edited, cli_config, cwd) {
        Ok(o) => o,
        Err(e) => {
            eprintln!("bedouin: {e}");
            return ExitCode::FAILURE;
        }
    };
    println!("{done}");
    if no_apply {
        return ExitCode::SUCCESS;
    }
    if !after.plan.has_changes() {
        println!("Nothing to do on this machine.");
        return ExitCode::SUCCESS;
    }
    print!("{}", after.plan.render(verbose));
    if !yes && !confirm() {
        println!("Config edited, nothing applied.");
        return ExitCode::SUCCESS;
    }
    run_apply(host, after, verbose, &Default::default())
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
    # A mapping means branches. `default:` is what runs on a machine none of
    # the named arms match. Without one, such a machine is an error rather than
    # a fallback -- deliberately: a missing default means you have not decided
    # yet, and bedouin would rather say so than guess on your behalf.
    from: { macos: brew, debian-like: apt, suse-like: zypper, default: apt }

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
    // Decided once, here: a pipe or a CI log gets plain text, and NO_COLOR is
    // honoured because output this long ends up in files and bug reports.
    bedouin_core::style::set_enabled(
        std::io::IsTerminal::is_terminal(&std::io::stdout())
            && std::env::var_os("NO_COLOR").is_none()
            && std::env::var("TERM").map(|t| t != "dumb").unwrap_or(true),
    );
    let host = OsHost::new();
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));

    // `init` has no config to load yet, so it runs before the pipeline.
    if matches!(cli.command, Command::Init) {
        let target = cli
            .config
            .clone()
            .unwrap_or_else(|| cwd.join("bedouin.yaml"));
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

    // `env` diagnoses configs that will not resolve, so it must not need
    // resolution -- it runs on the loaded document and the facts alone.
    if let Command::Env { write } = cli.command {
        return cmd_env(&host, cli.config.as_deref(), &cwd, write);
    }

    // `facts` answers "what does bedouin think this machine is", which is a
    // question about the MACHINE, not the config. Computing it from a full
    // plan meant a config that would not resolve -- a package with no arm for
    // this platform, say -- took `facts` down with it, on exactly the
    // unsupported box where it is the first thing you would reach for.
    if matches!(cli.command, Command::Facts) {
        return cmd_facts(&host, cli.config.as_deref(), &cwd);
    }

    // Emits a script and exits; it needs no config, and the plan step that
    // runs it must work before anything is resolved.
    if let Command::CompletionScript { shell } = &cli.command {
        use clap::CommandFactory;
        let Some(gen) = (match shell.as_str() {
            "bash" => Some(clap_complete::Shell::Bash),
            "zsh" => Some(clap_complete::Shell::Zsh),
            "fish" => Some(clap_complete::Shell::Fish),
            _ => None,
        }) else {
            eprintln!("bedouin: no completion for `{shell}`\n  known: bash, zsh, fish");
            return ExitCode::FAILURE;
        };
        clap_complete::generate(gen, &mut Cli::command(), "bedouin", &mut std::io::stdout());
        return ExitCode::SUCCESS;
    }

    // Hands over to another binary; it needs no config resolved here, and
    // the config path is passed straight through.
    if let Command::Ui { port, yes } = cli.command {
        return sidecar::run(&host, cli.config.as_deref(), port, yes);
    }

    // `tui` plans for itself, and re-plans after each apply.
    #[cfg(feature = "tui")]
    if matches!(cli.command, Command::Tui) {
        return tui::run(&host, cli.config.as_deref(), &cwd, cli.verbose);
    }

    // `apply -f` takes facts AND config from the artifact, so like `env` it
    // must not need the live config to resolve first. Planning here defeated
    // the frozen environment in precisely the case it exists for: applying a
    // reviewed plan in a session where the variable is not exported, which
    // died on `undefined value` before the artifact was ever opened.
    if let Command::Apply {
        plan: Some(file),
        skip,
        ..
    } = &cli.command
    {
        let skip: std::collections::BTreeSet<String> = skip.iter().cloned().collect();
        println!("Applying the plan in {}.\n", file.display());
        let report = match run::apply_artifact(&host, file, &skip, &mut print_line) {
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

    // Captured before the match moves out of `cli.command`.
    let cli_config = cli.config.clone();
    let verbose = cli.verbose;

    let outcome = match run::plan(&host, cli.config.as_deref(), &cwd) {
        Ok(o) => o,
        Err(e) => {
            eprintln!("bedouin: {e}");
            return ExitCode::FAILURE;
        }
    };

    match cli.command {
        Command::Sync { yes } => {
            let root = &outcome.loaded.root;
            // A dirty tree means uncommitted local edits; a pull would either
            // fail or bury them. Neither is ours to decide.
            match git(root, &["status", "--porcelain"]) {
                Err(e) => {
                    eprintln!(
                        "bedouin: {} is not a git repository, or git failed: {e}",
                        root.display()
                    );
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
            // Declared repos are pulled here rather than on every apply: a
            // repo that is present is done, and this is the command whose job
            // is "go and get what changed".
            for repo in &outcome.config.repos {
                let dest = bedouin_core::loader::normalize(
                    &repo.dest,
                    &outcome.facts.home,
                    &outcome.facts.home,
                );
                if !dest.join(".git").exists() {
                    continue;
                }
                match git(&dest, &["pull", "--ff-only"]) {
                    Ok(out) => {
                        println!("{}: {}", dest.display(), out.lines().next().unwrap_or("ok"))
                    }
                    Err(e) => {
                        // Not fatal: one repo that diverged should not stop
                        // the rest, and it is the user's call what to do.
                        eprintln!("bedouin: {}: {e}", dest.display());
                    }
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
            run_apply(&host, after, cli.verbose, &Default::default())
        }

        Command::Add {
            spec,
            alias,
            completions,
            path: path_entries,
            no_apply,
            yes,
        } => {
            let Some((manager, rest)) = spec.split_once(':') else {
                eprintln!(
                    "bedouin: `{spec}` is not `manager:package`.\n  For example: `bedouin add apt:ripgrep` or `bedouin add cargo:zellij@0.40.1`"
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
                eprintln!(
                    "bedouin: unknown manager `{manager}`\n  known: {}",
                    known.join(", ")
                );
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
            let mut edited = match bedouin_core::edit::add_package(&text, name, manager, version) {
                Ok(t) => t,
                Err(e) => {
                    eprintln!("bedouin: {e}");
                    return ExitCode::FAILURE;
                }
            };
            // The extras, applied to the same text before any of it is
            // written -- so a bad `--alias` leaves nothing half-added.
            for a in &alias {
                let Some((k, v)) = a.split_once('=') else {
                    eprintln!("bedouin: `--alias {a}` is not `name=value`");
                    return ExitCode::FAILURE;
                };
                match bedouin_core::edit::set_alias(&edited, Some(name), k, v) {
                    Ok(t) => edited = t,
                    Err(e) => {
                        eprintln!("bedouin: {e}");
                        return ExitCode::FAILURE;
                    }
                }
            }
            if let Some(cmd) = &completions {
                match set_completions(&edited, name, &shell_words(cmd)) {
                    Ok(t) => edited = t,
                    Err(e) => {
                        eprintln!("bedouin: {e}");
                        return ExitCode::FAILURE;
                    }
                }
            }
            if !path_entries.is_empty() {
                let list = path_entries
                    .iter()
                    .map(|p| format!("\"{p}\""))
                    .collect::<Vec<_>>()
                    .join(", ");
                match bedouin_core::edit::set_field(
                    &edited,
                    bedouin_core::edit::Section::Packages,
                    name,
                    "path",
                    &format!("[{list}]"),
                ) {
                    Ok(t) => edited = t,
                    Err(e) => {
                        eprintln!("bedouin: {e}");
                        return ExitCode::FAILURE;
                    }
                }
            }
            let after =
                match write_config_verified(&host, entry, &edited, cli.config.as_deref(), &cwd) {
                    Ok(o) => o,
                    Err(e) => {
                        eprintln!("bedouin: {e}");
                        return ExitCode::FAILURE;
                    }
                };
            println!("Added `{name}` from `{manager}` to {}.", entry.display());
            if no_apply {
                return ExitCode::SUCCESS;
            }
            if !after.plan.has_changes() {
                println!("Already on this machine; nothing to do.");
                return ExitCode::SUCCESS;
            }
            print!("{}", after.plan.render(cli.verbose));
            if !yes && !confirm() {
                println!("Config edited, nothing applied.");
                return ExitCode::SUCCESS;
            }
            run_apply(&host, after, cli.verbose, &Default::default())
        }

        // `init` has no config yet; `env` and `facts` answer questions that
        // must survive a config which does not resolve. All three return
        // before the pipeline above.
        Command::Ui { .. } => unreachable!("handed over above"),
        Command::Init | Command::Env { .. } | Command::Facts => {
            unreachable!("handled before the config is resolved")
        }
        #[cfg(feature = "tui")]
        Command::Tui => unreachable!("handled above"),
        Command::CompletionScript { .. } => unreachable!("handled above"),

        Command::Reconcile { watch, interval } => {
            let mut first = Some(outcome);
            loop {
                let o = match first.take() {
                    Some(o) => o,
                    None => match run::plan(&host, cli.config.as_deref(), &cwd) {
                        Ok(o) => o,
                        Err(e) => {
                            // Under --watch a bad config is temporary: someone
                            // is editing it. Say so and keep waiting rather
                            // than dying and needing a restart.
                            eprintln!("bedouin: {e}");
                            if !watch {
                                return ExitCode::FAILURE;
                            }
                            std::thread::sleep(std::time::Duration::from_secs(interval));
                            continue;
                        }
                    },
                };
                if o.plan.has_changes() {
                    println!("{} change(s) pending; applying.", o.plan.changes().count());
                    let code = run_apply(&host, o, cli.verbose, &Default::default());
                    if !watch {
                        return code;
                    }
                } else if !watch {
                    println!("Nothing to reconcile.");
                    return ExitCode::SUCCESS;
                }
                // ponytail: polling, not inotify. inotify is a dependency and
                // another platform split; a reconcile loop is not latency
                // sensitive. Revisit if seconds ever matter.
                std::thread::sleep(std::time::Duration::from_secs(interval));
            }
        }

        Command::Daemon { action } => {
            let exe = std::env::current_exe()
                .map(|p| p.display().to_string())
                .unwrap_or_else(|_| "bedouin".into());
            let config = outcome.loaded.entry.display().to_string();
            match action {
                DaemonAction::Install { interval } => {
                    let mut units = vec![bedouin_core::daemon::unit_for(
                        &outcome.facts,
                        &exe,
                        &config,
                        interval,
                    )];
                    units.extend(bedouin_core::daemon::service_for(
                        &outcome.facts,
                        &exe,
                        &config,
                    ));
                    for u in &units {
                        if let Some(d) = u.path.parent() {
                            let _ = std::fs::create_dir_all(d);
                        }
                        if let Err(e) = std::fs::write(&u.path, &u.contents) {
                            eprintln!("bedouin: {}: {e}", u.path.display());
                            return ExitCode::FAILURE;
                        }
                        println!("Wrote {}", u.path.display());
                    }
                    let enable: Vec<&String> = units.iter().flat_map(|u| u.enable.iter()).collect();
                    if !enable.is_empty() {
                        // Printed, not run: enabling a background service that
                        // mutates the machine is the user's decision.
                        println!("\nTo start it:");
                        for c in enable {
                            println!("  {c}");
                        }
                    }
                    ExitCode::SUCCESS
                }
                DaemonAction::Uninstall => {
                    let mut units = vec![bedouin_core::daemon::unit_for(
                        &outcome.facts,
                        &exe,
                        &config,
                        900,
                    )];
                    units.extend(bedouin_core::daemon::service_for(
                        &outcome.facts,
                        &exe,
                        &config,
                    ));
                    for u in &units {
                        match std::fs::remove_file(&u.path) {
                            Ok(()) => println!("Removed {}", u.path.display()),
                            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                            Err(e) => eprintln!("bedouin: {}: {e}", u.path.display()),
                        }
                    }
                    println!("Disable it with your service manager if it is still loaded.");
                    ExitCode::SUCCESS
                }
            }
        }

        Command::Absorb { yes } => {
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
            let edited: Vec<&bedouin_core::doctor::Drift> = report
                .drift
                .iter()
                .filter(|d| matches!(d, bedouin_core::doctor::Drift::Edited { .. }))
                .collect();
            if edited.is_empty() {
                println!("Nothing to absorb: no managed content has been edited by hand.");
                return ExitCode::SUCCESS;
            }

            let entry = outcome.loaded.entry.clone();
            let mut text = match std::fs::read_to_string(&entry) {
                Ok(t) => t,
                Err(e) => {
                    eprintln!("bedouin: {}: {e}", entry.display());
                    return ExitCode::FAILURE;
                }
            };
            let mut absorbed = 0usize;

            for d in edited {
                let bedouin_core::doctor::Drift::Edited { id, file } = d else {
                    continue;
                };
                // `rc/{package}/{basename}` -- the block is between markers
                // bedouin owns, so its current text IS the new content.
                let Some(rest) = id.strip_prefix("rc/") else {
                    println!("  ? {id}\n      not absorbable yet; edit the config by hand");
                    continue;
                };
                let Some((package, basename)) = rest.split_once('/') else {
                    continue;
                };
                if package == "bedouin" {
                    println!(
                        "  ? {id}\n      bedouin owns this block itself; nothing to absorb into"
                    );
                    continue;
                }
                let current = match std::fs::read_to_string(file) {
                    Ok(t) => t,
                    Err(e) => {
                        eprintln!("  ! {file}: {e}");
                        continue;
                    }
                };
                let Ok(Some(block)) = bedouin_core::writers::extract_block(&current, package)
                else {
                    println!("  ? {id}\n      could not read the block back; edit by hand");
                    continue;
                };

                println!("\n{id}\n  from {file}:");
                for l in block.lines() {
                    println!("    | {l}");
                }
                if !yes && !confirm_absorb() {
                    println!("  skipped");
                    continue;
                }
                match bedouin_core::edit::set_rc_content(&text, package, basename, &block) {
                    Ok(t) => {
                        text = t;
                        absorbed += 1;
                    }
                    Err(e) => eprintln!("  ! {e}"),
                }
            }

            if absorbed == 0 {
                println!("\nNothing absorbed.");
                return ExitCode::SUCCESS;
            }
            if let Err(e) = write_config_verified(&host, &entry, &text, cli.config.as_deref(), &cwd)
            {
                eprintln!("bedouin: {e}");
                return ExitCode::FAILURE;
            }
            println!("\nAbsorbed {absorbed} edit(s) into {}.", entry.display());
            // The config now matches the disk, but state still records the old
            // hash, so doctor keeps reporting drift until apply re-records it.
            // Applying writes the same bytes back -- it is the bookkeeping that
            // moves, not the machine.
            println!("Run `bedouin apply` to record it, then commit the config.");
            ExitCode::SUCCESS
        }

        Command::Alias {
            spec,
            package,
            no_apply,
            yes,
        } => {
            let Some((name, value)) = spec.split_once('=') else {
                eprintln!("bedouin: `{spec}` is not `name=value`.");
                eprintln!("  For example: `bedouin alias gs='git status'`");
                return ExitCode::FAILURE;
            };
            edit_then_apply(
                &host,
                &outcome,
                cli_config.as_deref(),
                verbose,
                &cwd,
                no_apply,
                yes,
                |text| bedouin_core::edit::set_alias(text, package.as_deref(), name, value),
                &match &package {
                    Some(p) => format!("Set alias `{name}` on package `{p}`."),
                    None => format!("Set global alias `{name}`."),
                },
            )
        }

        Command::Completions {
            package,
            generate,
            no_apply,
            yes,
        } => edit_then_apply(
            &host,
            &outcome,
            cli_config.as_deref(),
            verbose,
            &cwd,
            no_apply,
            yes,
            |text| set_completions(text, &package, &generate),
            &format!("Set completions for `{package}`: `{}`.", generate.join(" ")),
        ),

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
            let id = format!("{}/{name}", if language { "language" } else { "package" });
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
            // Re-plan against the edited config: the removal is a plan outcome
            // like any other, not a special path. Verified before it sticks.
            let after =
                match write_config_verified(&host, entry, &edited, cli.config.as_deref(), &cwd) {
                    Ok(o) => o,
                    Err(e) => {
                        eprintln!("bedouin: {e}");
                        return ExitCode::FAILURE;
                    }
                };
            println!(
                "Removed {} `{name}` from {}.",
                section.label(),
                entry.display()
            );
            if no_apply {
                println!("Config edited only. Run `bedouin apply` when ready.");
                return ExitCode::SUCCESS;
            }
            if !after.plan.has_changes() {
                println!("Nothing to undo on this machine.");
                return ExitCode::SUCCESS;
            }
            print!("{}", after.plan.render(cli.verbose));
            if !yes && !confirm() {
                println!("Config edited, nothing applied.");
                return ExitCode::SUCCESS;
            }
            run_apply(&host, after, cli.verbose, &Default::default())
        }

        // The `plan: Some(..)` case returned above, before the live plan.
        Command::Apply {
            dry_run, yes, skip, ..
        } => {
            let skip: std::collections::BTreeSet<String> = skip.into_iter().collect();
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
            run_apply(&host, outcome, cli.verbose, &skip)
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
                    &host,
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

#[cfg(test)]
mod tests {
    use super::shell_words;

    #[test]
    fn a_template_survives_being_split_into_argv() {
        // A naive whitespace split turns `{{ shell.name }}` into three
        // arguments, and the template stops being one.
        assert_eq!(
            shell_words("zellij setup --dump-completion {{ shell.name }}"),
            ["zellij", "setup", "--dump-completion", "{{ shell.name }}"]
        );
        assert_eq!(
            shell_words("kubectl completion {{ shell.name }}"),
            ["kubectl", "completion", "{{ shell.name }}"]
        );
    }

    #[test]
    fn quotes_group_and_plain_words_do_not() {
        assert_eq!(
            shell_words("gh completion -s zsh"),
            ["gh", "completion", "-s", "zsh"]
        );
        assert_eq!(
            shell_words("tool --flag 'two words'"),
            ["tool", "--flag", "two words"]
        );
        assert_eq!(shell_words("   spaced   out  "), ["spaced", "out"]);
        assert!(shell_words("").is_empty());
    }
}
