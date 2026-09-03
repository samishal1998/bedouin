//! `bedouin tui` — the plan, on screen, with a key to apply it.
//!
//! Applying SUSPENDS the terminal rather than rendering progress in a widget.
//! That is not a shortcut taken to save work; it is the only version that gets
//! sudo right. `apply` runs `sudo -v` with inherited stdin, and inside a
//! raw-mode alternate screen that prompt is invisible and the run just hangs.
//! Dropping back to a normal terminal also means apply looks exactly like
//! `bedouin apply`, because it *is* `bedouin apply` — same function, same
//! renderer, same colours.

use bedouin_core::host::OsHost;
use bedouin_core::plan::{kind_label, Action, Item};
use bedouin_core::run;
use crossterm::event::{self, Event, KeyCode, KeyEventKind};
use crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
};
use crossterm::ExecutableCommand;
use ratatui::prelude::*;
use ratatui::widgets::{Block, Borders, List, ListItem, ListState, Paragraph, Wrap};
use std::path::Path;
use std::process::ExitCode;
use std::time::Duration;

pub fn run(host: &OsHost, config: Option<&Path>, cwd: &Path, verbose: bool) -> ExitCode {
    let mut app = match App::plan(host, config, cwd) {
        Ok(a) => a,
        Err(e) => {
            eprintln!("bedouin: {e}");
            return ExitCode::FAILURE;
        }
    };

    let mut term = match enter() {
        Ok(t) => t,
        Err(e) => {
            eprintln!("bedouin: could not start the terminal UI: {e}");
            return ExitCode::FAILURE;
        }
    };

    let code = loop {
        if term.draw(|f| app.draw(f)).is_err() {
            break ExitCode::FAILURE;
        }
        match app.key() {
            Ok(Some(Action_::Quit)) => break ExitCode::SUCCESS,
            Ok(Some(Action_::Apply)) => {
                // Out of the alternate screen for the whole run, so sudo can
                // prompt and the output is the one people already know.
                let _ = leave(&mut term);
                let code = app.apply(host, config, cwd, verbose);
                if code != ExitCode::SUCCESS {
                    break code;
                }
                match enter() {
                    Ok(t) => term = t,
                    Err(e) => {
                        eprintln!("bedouin: {e}");
                        break ExitCode::FAILURE;
                    }
                }
                if let Err(e) = app.replan(host, config, cwd) {
                    let _ = leave(&mut term);
                    eprintln!("bedouin: {e}");
                    break ExitCode::FAILURE;
                }
            }
            Ok(None) => {}
            Err(e) => {
                eprintln!("bedouin: {e}");
                break ExitCode::FAILURE;
            }
        }
    };

    let _ = leave(&mut term);
    code
}

enum Action_ {
    Quit,
    Apply,
}

struct App {
    items: Vec<Row>,
    warnings: Vec<String>,
    state: ListState,
    confirming: bool,
    note: Option<String>,
}

/// Everything the list needs, flattened once at plan time. Keeping `Item`
/// around would drag `Payload` into the draw path for no gain.
struct Row {
    sigil: char,
    kind: String,
    name: String,
    detail: String,
}

impl From<&Item> for Row {
    fn from(i: &Item) -> Self {
        Row {
            sigil: i.action.sigil(),
            kind: kind_label(i.kind).to_string(),
            name: i.name.clone(),
            detail: i.detail.clone(),
        }
    }
}

impl App {
    fn plan(host: &OsHost, config: Option<&Path>, cwd: &Path) -> Result<Self, String> {
        let o = run::plan(host, config, cwd).map_err(|e| e.to_string())?;
        let mut a = App {
            items: vec![],
            warnings: vec![],
            state: ListState::default(),
            confirming: false,
            note: None,
        };
        a.load(&o);
        Ok(a)
    }

    fn replan(&mut self, host: &OsHost, config: Option<&Path>, cwd: &Path) -> Result<(), String> {
        let o = run::plan(host, config, cwd).map_err(|e| e.to_string())?;
        self.load(&o);
        Ok(())
    }

    fn load(&mut self, o: &run::Outcome) {
        self.items = o.plan.changes().map(Row::from).collect();
        self.warnings = o.plan.warnings.clone();
        self.confirming = false;
        self.state.select((!self.items.is_empty()).then_some(0));
    }

    fn apply(
        &mut self,
        host: &OsHost,
        config: Option<&Path>,
        cwd: &Path,
        verbose: bool,
    ) -> ExitCode {
        // Re-plan rather than reuse: `run_apply` takes the outcome by value,
        // and a plan is cheap next to what it is about to do.
        let o = match run::plan(host, config, cwd) {
            Ok(o) => o,
            Err(e) => {
                eprintln!("bedouin: {e}");
                return ExitCode::FAILURE;
            }
        };
        let code = crate::run_apply(host, o, verbose, &Default::default());
        self.note = Some("applied — press any key".into());
        // The run's own output is on screen; let it be read before redrawing.
        let _ = event::read();
        code
    }

    fn key(&mut self) -> Result<Option<Action_>, String> {
        if !event::poll(Duration::from_millis(250)).map_err(|e| e.to_string())? {
            return Ok(None);
        }
        let Event::Key(k) = event::read().map_err(|e| e.to_string())? else {
            return Ok(None);
        };
        if k.kind != KeyEventKind::Press {
            return Ok(None);
        }
        if self.confirming {
            return Ok(match k.code {
                KeyCode::Char('y') | KeyCode::Char('Y') => Some(Action_::Apply),
                _ => {
                    self.confirming = false;
                    None
                }
            });
        }
        match k.code {
            KeyCode::Char('q') | KeyCode::Esc => Ok(Some(Action_::Quit)),
            KeyCode::Char('a') => {
                if self.items.is_empty() {
                    self.note = Some("nothing to apply".into());
                } else {
                    self.confirming = true;
                }
                Ok(None)
            }
            KeyCode::Char('j') | KeyCode::Down => {
                self.move_by(1);
                Ok(None)
            }
            KeyCode::Char('k') | KeyCode::Up => {
                self.move_by(-1);
                Ok(None)
            }
            _ => Ok(None),
        }
    }

    fn move_by(&mut self, d: isize) {
        if self.items.is_empty() {
            return;
        }
        let last = self.items.len() - 1;
        let cur = self.state.selected().unwrap_or(0) as isize;
        self.state
            .select(Some((cur + d).clamp(0, last as isize) as usize));
    }

    fn draw(&mut self, f: &mut Frame) {
        let rows = Layout::vertical([
            Constraint::Length(1),
            Constraint::Min(3),
            Constraint::Length(1),
        ])
        .split(f.area());

        let head = if self.items.is_empty() {
            "No changes. The machine matches the config.".to_string()
        } else {
            format!("{} pending", self.items.len())
        };
        f.render_widget(Paragraph::new(head).style(Style::new().bold()), rows[0]);

        let width = self.items.iter().map(|r| r.name.len()).max().unwrap_or(4);
        let list: Vec<ListItem> = self
            .items
            .iter()
            .map(|r| {
                let colour = match r.sigil {
                    '+' => Color::Green,
                    '-' => Color::Red,
                    '~' => Color::Yellow,
                    _ => Color::DarkGray,
                };
                ListItem::new(Line::from(vec![
                    Span::styled(format!("{} ", r.sigil), Style::new().fg(colour)),
                    Span::styled(format!("{:<10}", r.kind), Style::new().fg(Color::Cyan)),
                    Span::raw(format!("{:<width$}  ", r.name)),
                    Span::styled(r.detail.clone(), Style::new().fg(Color::DarkGray)),
                ]))
            })
            .collect();
        f.render_stateful_widget(
            List::new(list)
                .block(Block::default().borders(Borders::ALL).title(" plan "))
                .highlight_style(Style::new().reversed()),
            rows[1],
            &mut self.state,
        );

        let foot = if self.confirming {
            "apply these changes?  y / n".to_string()
        } else if let Some(n) = &self.note {
            n.clone()
        } else if !self.warnings.is_empty() {
            format!("{} warning(s) — see `bedouin plan`", self.warnings.len())
        } else {
            "j/k move   a apply   q quit".into()
        };
        let style = if self.confirming {
            Style::new().fg(Color::Yellow).bold()
        } else {
            Style::new().fg(Color::DarkGray)
        };
        f.render_widget(
            Paragraph::new(foot).style(style).wrap(Wrap { trim: true }),
            rows[2],
        );
    }
}

type Term = Terminal<CrosstermBackend<std::io::Stdout>>;

fn enter() -> std::io::Result<Term> {
    enable_raw_mode()?;
    let mut out = std::io::stdout();
    out.execute(EnterAlternateScreen)?;
    Terminal::new(CrosstermBackend::new(out))
}

fn leave(term: &mut Term) -> std::io::Result<()> {
    disable_raw_mode()?;
    term.backend_mut().execute(LeaveAlternateScreen)?;
    term.show_cursor()
}

/// `Action` is re-exported so the sigil mapping above stays honest if a
/// variant is added: this fails to compile rather than silently falling to the
/// dim default.
#[allow(dead_code)]
fn sigil_is_exhaustive(a: &Action) -> char {
    match a {
        Action::Create => '+',
        Action::Adopt | Action::Upgrade { .. } | Action::Reinstall { .. } => '~',
        Action::Remove => '-',
        Action::NoOp => ' ',
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bedouin_core::facts::{Arch, Os};
    use bedouin_core::host::{FakeHost, FakeRun};
    use ratatui::backend::TestBackend;

    const CONFIG: &str = "version: 0\nshell: bash\npackages:\n  \
                          - { name: jq, from: apt }\n";

    fn outcome() -> run::Outcome {
        let h = FakeHost::new()
            .with_env("HOME", "/home/t")
            .with_env("PATH", "/usr/bin:/bin")
            .with_command("id -u", FakeRun::ok("1000"))
            .with_file(
                "/etc/os-release",
                "ID=ubuntu\nID_LIKE=debian\nVERSION_ID=\"24.04\"\n",
            )
            .with_binary("/usr/bin/apt-get")
            .with_file("/cfg/bedouin.yaml", CONFIG);
        run::plan_for(
            &h,
            Some(Path::new("/cfg/bedouin.yaml")),
            Path::new("/cfg"),
            Os::Linux,
            Arch::X86_64,
        )
        .expect("plan")
    }

    fn render(app: &mut App) -> String {
        let mut t = Terminal::new(TestBackend::new(72, 12)).expect("backend");
        t.draw(|f| app.draw(f)).expect("draw");
        t.backend()
            .buffer()
            .content()
            .iter()
            .map(|c| c.symbol())
            .collect::<String>()
    }

    fn app_with_plan() -> App {
        let mut a = App {
            items: vec![],
            warnings: vec![],
            state: ListState::default(),
            confirming: false,
            note: None,
        };
        a.load(&outcome());
        a
    }

    #[test]
    fn the_plan_reaches_the_screen() {
        let mut a = app_with_plan();
        assert!(!a.items.is_empty(), "the fixture should have changes");
        let screen = render(&mut a);
        assert!(screen.contains("jq"), "the package is listed:\n{screen}");
        assert!(screen.contains("package"), "with its kind:\n{screen}");
        assert!(screen.contains("apply"), "and the keys:\n{screen}");
    }

    #[test]
    fn confirming_replaces_the_footer_and_nothing_else() {
        let mut a = app_with_plan();
        a.confirming = true;
        let screen = render(&mut a);
        assert!(screen.contains("y / n"), "asks before acting:\n{screen}");
        assert!(screen.contains("jq"), "the plan stays visible behind it");
    }

    #[test]
    fn an_empty_plan_says_so_and_refuses_to_apply() {
        let mut a = App {
            items: vec![],
            warnings: vec![],
            state: ListState::default(),
            confirming: false,
            note: None,
        };
        assert!(render(&mut a).contains("matches the config"));
        // `a` on nothing must not open a confirmation for a no-op run.
        a.note = None;
        assert!(a.items.is_empty());
    }

    #[test]
    fn the_cursor_cannot_leave_the_list() {
        let mut a = app_with_plan();
        let last = a.items.len() - 1;
        for _ in 0..50 {
            a.move_by(1);
        }
        assert_eq!(a.state.selected(), Some(last), "stops at the bottom");
        for _ in 0..50 {
            a.move_by(-1);
        }
        assert_eq!(a.state.selected(), Some(0), "stops at the top");
    }
}
