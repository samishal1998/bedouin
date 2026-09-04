//! `bedouin tui` — the config and the plan, navigable, editable, diffable.
//!
//! Applying SUSPENDS the terminal rather than rendering progress in a widget.
//! That is not a shortcut: `apply` runs `sudo -v` with inherited stdin, and
//! inside a raw-mode alternate screen that prompt is invisible and the run
//! hangs. Out of the alternate screen sudo works, and the run is literally
//! `bedouin apply` — same function, same renderer, same colours. `$EDITOR`
//! is suspended the same way and for the same reason.

mod diff;
mod model;
mod view;

use bedouin_core::host::OsHost;
use crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers};
use crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
};
use crossterm::ExecutableCommand;
use model::{App, Mode, Section};
use ratatui::prelude::*;
use std::path::Path;
use std::process::ExitCode;
use std::time::Duration;

/// What the key handler asks the outer loop to do. Anything that needs the
/// terminal given back lives here rather than inside the handler, so there is
/// exactly one place that suspends and restores.
enum Step {
    Stay,
    Quit,
    Apply,
    Edit,
}

pub fn run(host: &OsHost, config: Option<&Path>, cwd: &Path, verbose: bool) -> ExitCode {
    let mut app = match App::load(host, config, cwd) {
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
        if term.draw(|f| view::draw(&mut app, f)).is_err() {
            break ExitCode::FAILURE;
        }
        let step = match key(&mut app, host) {
            Ok(s) => s,
            Err(e) => {
                app.note = Some(e);
                Step::Stay
            }
        };
        match step {
            Step::Stay => {}
            Step::Quit => break ExitCode::SUCCESS,
            Step::Apply | Step::Edit => {
                let _ = leave(&mut term);
                let outcome = match step {
                    Step::Apply => app.apply(host, config, cwd, verbose),
                    _ => app.edit_in_editor(host, config, cwd),
                };
                match enter() {
                    Ok(t) => term = t,
                    Err(e) => {
                        eprintln!("bedouin: {e}");
                        break ExitCode::FAILURE;
                    }
                }
                if let Err(e) = outcome {
                    app.note = Some(e);
                }
            }
        }
    };

    let _ = leave(&mut term);
    code
}

fn key(app: &mut App, host: &dyn bedouin_core::host::Host) -> Result<Step, String> {
    if !event::poll(Duration::from_millis(200)).map_err(|e| e.to_string())? {
        return Ok(Step::Stay);
    }
    let Event::Key(k) = event::read().map_err(|e| e.to_string())? else {
        return Ok(Step::Stay);
    };
    if k.kind != KeyEventKind::Press {
        return Ok(Step::Stay);
    }
    app.note = None;

    match &mut app.mode {
        // A form field being typed into. Everything is literal except the
        // three keys that end it.
        Mode::Form(form) => {
            match k.code {
                KeyCode::Esc => app.mode = Mode::Browse,
                KeyCode::Enter => {
                    let (field, value) = (form.field.clone(), form.value.clone());
                    app.mode = Mode::Browse;
                    app.commit_field(host, &field, &value)?;
                }
                KeyCode::Backspace => {
                    form.value.pop();
                }
                KeyCode::Char(c) => form.value.push(c),
                _ => {}
            }
            Ok(Step::Stay)
        }

        Mode::Confirm => Ok(match k.code {
            KeyCode::Char('y') | KeyCode::Char('Y') => Step::Apply,
            _ => {
                app.mode = Mode::Browse;
                Step::Stay
            }
        }),

        Mode::Diff(_) => {
            match k.code {
                KeyCode::Down | KeyCode::Char('j') => app.scroll_diff(1),
                KeyCode::Up | KeyCode::Char('k') => app.scroll_diff(-1),
                _ => app.mode = Mode::Browse,
            }
            Ok(Step::Stay)
        }

        Mode::Browse => match k.code {
            KeyCode::Char('q') | KeyCode::Esc => Ok(Step::Quit),
            KeyCode::Char('c') if k.modifiers.contains(KeyModifiers::CONTROL) => Ok(Step::Quit),
            KeyCode::Tab | KeyCode::Right | KeyCode::Char('l') => {
                app.cycle_section(1);
                Ok(Step::Stay)
            }
            KeyCode::BackTab | KeyCode::Left | KeyCode::Char('h') => {
                app.cycle_section(-1);
                Ok(Step::Stay)
            }
            KeyCode::Down | KeyCode::Char('j') => {
                app.move_by(1);
                Ok(Step::Stay)
            }
            KeyCode::Up | KeyCode::Char('k') => {
                app.move_by(-1);
                Ok(Step::Stay)
            }
            KeyCode::Char('g') => {
                app.select(0);
                Ok(Step::Stay)
            }
            KeyCode::Char('G') => {
                let last = app.rows().len().saturating_sub(1);
                app.select(last);
                Ok(Step::Stay)
            }
            KeyCode::Char('d') => {
                app.open_diff(host)?;
                Ok(Step::Stay)
            }
            KeyCode::Enter => {
                app.open_form();
                Ok(Step::Stay)
            }
            KeyCode::Char('e') => Ok(Step::Edit),
            KeyCode::Char('r') => {
                app.note = Some("re-planned".into());
                Ok(Step::Stay)
            }
            KeyCode::Char('a') => {
                if app.plan_rows.is_empty() {
                    app.note = Some("nothing to apply".into());
                } else {
                    app.mode = Mode::Confirm;
                }
                Ok(Step::Stay)
            }
            KeyCode::Char('1') => {
                app.go(Section::Plan);
                Ok(Step::Stay)
            }
            _ => Ok(Step::Stay),
        },
    }
}

type Term = Terminal<CrosstermBackend<std::io::Stdout>>;

fn enter() -> std::io::Result<Term> {
    enable_raw_mode()?;
    let mut out = std::io::stdout();
    out.execute(EnterAlternateScreen)?;
    let mut t = Terminal::new(CrosstermBackend::new(out))?;
    t.clear()?;
    Ok(t)
}

fn leave(term: &mut Term) -> std::io::Result<()> {
    disable_raw_mode()?;
    term.backend_mut().execute(LeaveAlternateScreen)?;
    term.show_cursor()
}

#[cfg(test)]
mod tests {
    use super::model::{App, Mode, Section};
    use super::view;
    use bedouin_core::host::{FakeHost, FakeRun};
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;
    use std::path::Path;

    const CONFIG: &str = r#"
version: 0
shell: bash
aliases:
  ll: ls -alh
languages:
  - { name: rust, installer: rustup }
packages:
  - name: jq
    from: apt
    version: "1.7"
files:
  - { src: templates/g.j2, dest: "~/.gitconfig" }
repos:
  - { url: "https://example.invalid/r", dest: "~/r" }
links:
  - { src: "~/r/x", dest: "~/x" }
"#;

    fn host() -> FakeHost {
        FakeHost::new()
            .with_env("HOME", "/home/t")
            .with_env("PATH", "/usr/bin:/bin")
            .with_env("BEDOUIN_EXE", "bedouin")
            .with_command("id -u", FakeRun::ok("1000"))
            .with_file(
                "/etc/os-release",
                "ID=ubuntu\nID_LIKE=debian\nVERSION_ID=\"24.04\"\n",
            )
            .with_binary("/usr/bin/apt-get")
            .with_file("/cfg/bedouin.yaml", CONFIG)
            .with_file("/cfg/templates/g.j2", "[user]\n\tname = {{ user }}\n")
    }

    fn app() -> App {
        App::load(
            &host(),
            Some(Path::new("/cfg/bedouin.yaml")),
            Path::new("/cfg"),
        )
        .expect("load")
    }

    fn screen(a: &mut App) -> String {
        let mut t = Terminal::new(TestBackend::new(100, 20)).expect("backend");
        t.draw(|f| view::draw(a, f)).expect("draw");
        t.backend()
            .buffer()
            .content()
            .iter()
            .map(|c| c.symbol())
            .collect()
    }

    #[test]
    fn every_section_draws_and_names_itself() {
        let mut a = app();
        for s in Section::ALL {
            a.go(s);
            let out = screen(&mut a);
            assert!(
                out.contains(s.title()),
                "section `{}` does not name itself:\n{out}",
                s.title()
            );
        }
    }

    #[test]
    fn the_config_sections_show_what_the_config_declares() {
        let mut a = app();
        for (s, needle) in [
            (Section::Packages, "jq"),
            (Section::Files, ".gitconfig"),
            (Section::Repos, "example.invalid"),
            (Section::Links, "~/x"),
            (Section::Aliases, "ll"),
            (Section::Languages, "rust"),
        ] {
            a.go(s);
            let out = screen(&mut a);
            assert!(
                out.contains(needle),
                "`{needle}` missing from {}:\n{out}",
                s.title()
            );
        }
    }

    #[test]
    fn tab_moves_between_sections_and_wraps() {
        let mut a = app();
        assert!(a.section == Section::Plan);
        a.cycle_section(1);
        assert!(a.section == Section::Packages);
        a.cycle_section(-1);
        assert!(a.section == Section::Plan);
        // Backwards off the front lands on the last, not out of bounds.
        a.cycle_section(-1);
        assert!(a.section == Section::Env);
    }

    #[test]
    fn each_section_keeps_its_own_cursor() {
        let mut a = app();
        a.go(Section::Packages);
        a.move_by(1);
        let pkg = a.list_state().selected();
        a.go(Section::Aliases);
        a.select(0);
        a.go(Section::Packages);
        assert_eq!(a.list_state().selected(), pkg, "moving away lost the place");
    }

    #[test]
    fn the_cursor_cannot_leave_a_list() {
        let mut a = app();
        a.go(Section::Packages);
        for _ in 0..99 {
            a.move_by(1);
        }
        let last = a.rows().len() - 1;
        assert_eq!(a.list_state().selected(), Some(last));
        for _ in 0..99 {
            a.move_by(-1);
        }
        assert_eq!(a.list_state().selected(), Some(0));
    }

    #[test]
    fn a_form_opens_on_an_editable_field_and_not_otherwise() {
        let mut a = app();
        a.go(Section::Packages);
        a.select(0);
        a.open_form();
        match &a.mode {
            Mode::Form(f) => assert_eq!(f.value, "1.7", "seeded with the current value"),
            _ => panic!("a package version should be editable"),
        }
        assert!(screen(&mut a).contains("version"), "the form is drawn");

        a.mode = Mode::Browse;
        a.go(Section::Repos);
        a.select(0);
        a.open_form();
        assert!(
            matches!(a.mode, Mode::Browse),
            "a repo has no form field, so nothing opens"
        );
        assert!(a.note.is_some(), "and it says why");
    }

    #[test]
    fn a_diff_of_a_file_shows_what_apply_would_write() {
        let mut a = app();
        a.go(Section::Plan);
        // Find the gitconfig row rather than assuming an index.
        let i = a
            .rows()
            .iter()
            .position(|r| r.name.contains(".gitconfig"))
            .expect("the template is in the plan");
        a.select(i);
        a.open_diff(&host()).expect("diff");
        match &a.mode {
            Mode::Diff(v) => assert!(!v.rows.is_empty(), "a new file is all additions"),
            _ => panic!("expected a diff, note={:?}", a.note),
        }
        assert!(
            screen(&mut a).contains("name ="),
            "the rendered text is shown"
        );
    }

    #[test]
    fn editing_a_field_writes_it_and_shows_the_config_diff() {
        let h = host();
        let mut a =
            App::load(&h, Some(Path::new("/cfg/bedouin.yaml")), Path::new("/cfg")).expect("load");
        a.go(Section::Packages);
        a.select(0);
        a.open_form();
        let Mode::Form(f) = &a.mode else {
            panic!("no form")
        };
        let field = f.field.clone();
        a.mode = Mode::Browse;
        a.commit_field(&h, &field, "1.8").expect("commit");

        // The config on the (fake) disk really changed...
        let written = String::from_utf8(
            bedouin_core::host::Host::read(&h, Path::new("/cfg/bedouin.yaml"))
                .unwrap()
                .unwrap(),
        )
        .unwrap();
        assert!(
            written.contains("1.8"),
            "the new value was written:\n{written}"
        );

        // ...and what changed is on screen, both sides.
        match &a.mode {
            Mode::Diff(v) => {
                let text = format!("{:?}", v.rows);
                assert!(text.contains("1.7"), "the old value is shown as removed");
                assert!(text.contains("1.8"), "the new value is shown as added");
            }
            _ => panic!("an edit should show its diff, note={:?}", a.note),
        }
    }

    #[test]
    fn an_inline_entry_says_to_use_the_editor_rather_than_a_yaml_error() {
        // Most real configs write packages as `- { name: jq, from: apt }`, and
        // the text surgery only edits block style. It refuses rather than
        // corrupting -- but the refusal has to be readable.
        const FLOW: &str = "version: 0\nshell: bash\npackages:\n  \
                            - { name: jq, from: apt, version: \"1.7\" }\n";
        let h = host().with_file("/cfg/bedouin.yaml", FLOW);
        let mut a =
            App::load(&h, Some(Path::new("/cfg/bedouin.yaml")), Path::new("/cfg")).expect("load");
        a.go(Section::Packages);
        a.select(0);
        a.open_form();
        let Mode::Form(f) = &a.mode else {
            panic!("no form")
        };
        let field = f.field.clone();
        a.mode = Mode::Browse;

        let err = a.commit_field(&h, &field, "1.8").expect_err("must refuse");
        assert!(err.contains("press e"), "points at the editor: {err}");
        assert!(!err.contains("indicator"), "not a raw yaml error: {err}");
    }

    #[test]
    fn confirming_is_asked_for_before_applying() {
        let mut a = app();
        assert!(matches!(a.mode, Mode::Browse));
        a.mode = Mode::Confirm;
        assert!(screen(&mut a).contains("y / n"), "it asks first");
    }
}
