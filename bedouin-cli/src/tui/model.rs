//! What the TUI knows, and everything that changes it.
//!
//! The view is a pure function of this; nothing here draws.

use super::diff;
use bedouin_core::host::{Host, OsHost};
use bedouin_core::plan::{kind_label, Payload};
use bedouin_core::run;
use bedouin_core::{doctor, envfile, render};
use ratatui::widgets::ListState;
use std::path::Path;
use std::process::ExitCode;

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Section {
    Plan,
    Packages,
    Files,
    Repos,
    Links,
    Aliases,
    Languages,
    Doctor,
    Env,
}

impl Section {
    pub const ALL: [Section; 9] = [
        Section::Plan,
        Section::Packages,
        Section::Files,
        Section::Repos,
        Section::Links,
        Section::Aliases,
        Section::Languages,
        Section::Doctor,
        Section::Env,
    ];

    pub fn title(self) -> &'static str {
        match self {
            Section::Plan => "plan",
            Section::Packages => "packages",
            Section::Files => "files",
            Section::Repos => "repos",
            Section::Links => "links",
            Section::Aliases => "aliases",
            Section::Languages => "languages",
            Section::Doctor => "doctor",
            Section::Env => "env",
        }
    }
}

/// One line in the list, plus what acting on it would mean.
pub struct Row {
    pub sigil: char,
    pub kind: String,
    pub name: String,
    pub detail: String,
    /// Fields this row can be edited through a form. Empty means `e` only.
    pub fields: Vec<Field>,
    /// The plan item this row corresponds to, if any — what `d` diffs.
    pub plan_id: Option<String>,
}

#[derive(Clone)]
pub struct Field {
    pub label: String,
    /// Passed to `edit::set_field`; `None` targets an alias value instead.
    pub key: Option<String>,
    pub current: String,
}

pub struct Form {
    pub title: String,
    pub field: Field,
    pub value: String,
}

pub enum Mode {
    Browse,
    Confirm,
    Form(Form),
    Diff(DiffView),
}

pub struct DiffView {
    pub title: String,
    pub rows: Vec<diff::Row>,
    pub scroll: usize,
}

pub struct App {
    pub section: Section,
    pub mode: Mode,
    pub note: Option<String>,

    pub plan_rows: Vec<Row>,
    packages: Vec<Row>,
    files: Vec<Row>,
    repos: Vec<Row>,
    links: Vec<Row>,
    aliases: Vec<Row>,
    languages: Vec<Row>,
    doctor_rows: Vec<Row>,
    env_rows: Vec<Row>,

    /// One cursor per section, so moving between them keeps your place.
    state: [ListState; 9],
    pub warnings: Vec<String>,

    outcome: run::Outcome,
}

impl App {
    pub fn load(host: &dyn Host, config: Option<&Path>, cwd: &Path) -> Result<Self, String> {
        let outcome = run::plan(host, config, cwd).map_err(|e| e.to_string())?;
        let mut app = App {
            section: Section::Plan,
            mode: Mode::Browse,
            note: None,
            plan_rows: vec![],
            packages: vec![],
            files: vec![],
            repos: vec![],
            links: vec![],
            aliases: vec![],
            languages: vec![],
            doctor_rows: vec![],
            env_rows: vec![],
            state: std::array::from_fn(|_| ListState::default()),
            warnings: vec![],
            outcome,
        };
        app.rebuild(host);
        Ok(app)
    }

    pub fn replan(
        &mut self,
        host: &dyn Host,
        config: Option<&Path>,
        cwd: &Path,
    ) -> Result<(), String> {
        self.outcome = run::plan(host, config, cwd).map_err(|e| e.to_string())?;
        self.rebuild(host);
        Ok(())
    }

    fn rebuild(&mut self, host: &dyn Host) {
        let o = &self.outcome;
        self.warnings = o.plan.warnings.clone();

        self.plan_rows = o
            .plan
            .changes()
            .map(|i| Row {
                sigil: i.action.sigil(),
                kind: kind_label(i.kind).to_string(),
                name: i.name.clone(),
                detail: i.detail.clone(),
                fields: vec![],
                plan_id: Some(i.id.clone()),
            })
            .collect();

        self.packages = o
            .config
            .packages
            .iter()
            .map(|p| Row {
                sigil: plan_sigil(o, &format!("package/{}", p.name)),
                kind: p
                    .from
                    .first()
                    .map(|m| m.to_string())
                    .unwrap_or_else(|| "script".into()),
                name: p.name.clone(),
                detail: p.version.clone().unwrap_or_else(|| "latest".into()),
                fields: vec![Field {
                    label: "version".into(),
                    key: Some("version".into()),
                    current: p.version.clone().unwrap_or_default(),
                }],
                plan_id: Some(format!("package/{}", p.name)),
            })
            .collect();

        self.files = o
            .config
            .files
            .iter()
            .map(|f| Row {
                sigil: ' ',
                kind: "file".into(),
                name: f.dest.clone(),
                detail: f.src.clone(),
                fields: vec![],
                plan_id: o
                    .plan
                    .items
                    .iter()
                    .find(|i| {
                        matches!(&i.payload, Payload::File { dest, .. }
                        if dest.display().to_string().ends_with(f.dest.trim_start_matches('~')))
                    })
                    .map(|i| i.id.clone()),
            })
            .collect();

        self.repos = o
            .config
            .repos
            .iter()
            .map(|r| Row {
                sigil: ' ',
                kind: "repo".into(),
                name: r.dest.clone(),
                detail: r.url.clone(),
                fields: vec![],
                plan_id: None,
            })
            .collect();

        self.links = o
            .config
            .links
            .iter()
            .map(|l| Row {
                sigil: ' ',
                kind: "link".into(),
                name: l.dest.clone(),
                detail: format!("-> {}", l.src),
                fields: vec![],
                plan_id: None,
            })
            .collect();

        self.aliases = o
            .config
            .aliases
            .iter()
            .map(|(k, v)| Row {
                sigil: ' ',
                kind: "alias".into(),
                name: k.clone(),
                detail: v.clone(),
                fields: vec![Field {
                    label: "value".into(),
                    key: None,
                    current: v.clone(),
                }],
                plan_id: None,
            })
            .collect();

        self.languages = o
            .config
            .languages
            .iter()
            .map(|l| Row {
                sigil: plan_sigil(o, &format!("language/{}", l.name)),
                kind: l
                    .installer
                    .map(|m| m.to_string())
                    .unwrap_or_else(|| "default".into()),
                name: l.name.clone(),
                detail: l.version.clone().unwrap_or_else(|| "latest".into()),
                fields: vec![Field {
                    label: "version".into(),
                    key: Some("version".into()),
                    current: l.version.clone().unwrap_or_default(),
                }],
                plan_id: Some(format!("language/{}", l.name)),
            })
            .collect();

        self.doctor_rows = match doctor::check(&o.state, &o.config, &o.facts, host) {
            Ok(rep) => rep
                .drift
                .iter()
                .map(|d| Row {
                    sigil: '!',
                    kind: "drift".into(),
                    name: d.id().to_string(),
                    detail: d.to_string().trim().replace('\n', " "),
                    fields: vec![],
                    plan_id: Some(d.id().to_string()),
                })
                .collect(),
            Err(e) => vec![Row {
                sigil: 'x',
                kind: "error".into(),
                name: "doctor".into(),
                detail: e.to_string(),
                fields: vec![],
                plan_id: None,
            }],
        };

        self.env_rows = envfile::referenced(&o.loaded.raw, &o.facts.env, &o.loaded.root, host)
            .into_iter()
            .map(|r| Row {
                sigil: if r.set { ' ' } else { '?' },
                kind: if r.set { "set".into() } else { "unset".into() },
                name: r.name,
                detail: format!(
                    "{}{}",
                    r.site,
                    if r.match_key {
                        "   (a target)"
                    } else if r.has_default {
                        "   (has a default)"
                    } else {
                        ""
                    }
                ),
                fields: vec![],
                plan_id: None,
            })
            .collect();

        for (i, s) in Section::ALL.iter().enumerate() {
            let len = self.rows_of(*s).len();
            let sel = self.state[i].selected().unwrap_or(0);
            self.state[i].select((len > 0).then(|| sel.min(len - 1)));
        }
    }

    fn rows_of(&self, s: Section) -> &[Row] {
        match s {
            Section::Plan => &self.plan_rows,
            Section::Packages => &self.packages,
            Section::Files => &self.files,
            Section::Repos => &self.repos,
            Section::Links => &self.links,
            Section::Aliases => &self.aliases,
            Section::Languages => &self.languages,
            Section::Doctor => &self.doctor_rows,
            Section::Env => &self.env_rows,
        }
    }

    pub fn len_of(&self, s: Section) -> usize {
        self.rows_of(s).len()
    }

    pub fn rows(&self) -> &[Row] {
        self.rows_of(self.section)
    }

    fn idx(&self) -> usize {
        Section::ALL
            .iter()
            .position(|s| *s == self.section)
            .unwrap()
    }

    pub fn list_state(&mut self) -> &mut ListState {
        let i = self.idx();
        &mut self.state[i]
    }

    pub fn selected(&self) -> Option<&Row> {
        self.state[self.idx()]
            .selected()
            .and_then(|i| self.rows().get(i))
    }

    pub fn go(&mut self, s: Section) {
        self.section = s;
    }

    pub fn cycle_section(&mut self, d: isize) {
        let n = Section::ALL.len() as isize;
        let i = self.idx() as isize;
        self.section = Section::ALL[(((i + d) % n + n) % n) as usize];
    }

    pub fn select(&mut self, i: usize) {
        let len = self.rows().len();
        let s = self.list_state();
        s.select((len > 0).then(|| i.min(len.saturating_sub(1))));
    }

    pub fn move_by(&mut self, d: isize) {
        let len = self.rows().len();
        if len == 0 {
            return;
        }
        let cur = self.state[self.idx()].selected().unwrap_or(0) as isize;
        self.select((cur + d).clamp(0, len as isize - 1) as usize);
    }

    pub fn scroll_diff(&mut self, d: isize) {
        if let Mode::Diff(v) = &mut self.mode {
            let max = v.rows.len().saturating_sub(1);
            v.scroll = ((v.scroll as isize + d).max(0) as usize).min(max);
        }
    }

    // ---------------------------------------------------------------- edits

    pub fn open_form(&mut self) {
        let Some(row) = self.selected() else { return };
        let Some(field) = row.fields.first().cloned() else {
            self.note = Some("no editable field here — `e` opens the config".into());
            return;
        };
        self.mode = Mode::Form(Form {
            title: format!("{} · {}", row.name, field.label),
            value: field.current.clone(),
            field,
        });
    }

    /// Write the field, show what changed in the config, and re-plan.
    pub fn commit_field(
        &mut self,
        host: &dyn Host,
        field: &Field,
        value: &str,
    ) -> Result<(), String> {
        let Some(name) = self.selected().map(|r| r.name.clone()) else {
            return Ok(());
        };
        let path = self.outcome.loaded.entry.clone();
        let before = read(host, &path)?.ok_or_else(|| format!("{}: gone", path.display()))?;

        let edited = match &field.key {
            Some(key) => {
                let section = match self.section {
                    Section::Packages => bedouin_core::edit::Section::Packages,
                    Section::Languages => bedouin_core::edit::Section::Languages,
                    _ => return Err("this section has no editable fields".into()),
                };
                bedouin_core::edit::set_field(&before, section, &name, key, value)
            }
            None => bedouin_core::edit::set_alias(&before, None, &name, value),
        };
        let after = match edited {
            Ok(a) => a,
            // The common refusal by far: `- { name: jq, from: apt }` is a flow
            // mapping, and the text surgery only edits block style. It refuses
            // rather than corrupting, which is right -- but "did not find
            // expected '-' indicator" is not what the reader needs to hear.
            Err(e) if e.to_string().contains("no longer parses") => {
                return Err(format!(
                    "`{name}` is written inline (`- {{ … }}`), which a form cannot edit — press e"
                ))
            }
            Err(e) => return Err(e.to_string()),
        };

        if after == before {
            self.note = Some("no change".into());
            return Ok(());
        }
        host.write(&path, after.as_bytes(), 0o600)
            .map_err(|e| e.to_string())?;
        self.mode = Mode::Diff(DiffView {
            title: format!("{} — config", path.display()),
            rows: diff::lines(&before, &after, 2),
            scroll: 0,
        });
        self.note = Some("written — press r to re-plan".into());
        Ok(())
    }

    /// `$EDITOR` on the config, at the selected item's line where we can find
    /// it. Suspends the TUI; the caller has already left the alternate screen.
    pub fn edit_in_editor(
        &mut self,
        host: &OsHost,
        config: Option<&Path>,
        cwd: &Path,
    ) -> Result<(), String> {
        let path = self.outcome.loaded.entry.clone();
        let editor = std::env::var("VISUAL")
            .or_else(|_| std::env::var("EDITOR"))
            .unwrap_or_else(|_| "vi".into());
        let line = self
            .selected()
            .and_then(|r| line_of(host, &path, &r.name))
            .unwrap_or(1);

        let mut cmd = std::process::Command::new(&editor);
        // +N is understood by vi, vim, nvim, nano, emacs and helix. An editor
        // that does not take it opens at the top, which is not a failure.
        cmd.arg(format!("+{line}")).arg(&path);
        let status = cmd
            .status()
            .map_err(|e| format!("could not run `{editor}`: {e}"))?;
        if !status.success() {
            return Err(format!("`{editor}` exited {status}"));
        }
        self.replan(host, config, cwd)
    }

    // ----------------------------------------------------------------- diff

    /// What `d` shows, which depends on what is selected: drift for a doctor
    /// row, and otherwise the content apply would write against what is there.
    pub fn open_diff(&mut self, host: &dyn Host) -> Result<(), String> {
        let Some(row) = self.selected() else {
            return Ok(());
        };
        let Some(id) = row.plan_id.clone() else {
            self.note = Some("nothing to diff here".into());
            return Ok(());
        };
        let name = row.name.clone();

        let Some(item) = self.outcome.plan.items.iter().find(|i| i.id == id) else {
            self.note = Some("no plan step for this item".into());
            return Ok(());
        };

        let (title, before, after) = match &item.payload {
            Payload::RcBlock { file, content, .. } => {
                let on_disk = read(host, file)?.unwrap_or_default();
                // Absent block and empty block are the same thing to a diff.
                let now = bedouin_core::writers::extract_block(&on_disk, &item.name)
                    .unwrap_or_default()
                    .unwrap_or_default();
                (
                    format!("{} — what apply would write", file.display()),
                    now,
                    content.clone(),
                )
            }
            Payload::File { src, dest, .. } => {
                let template =
                    read(host, src)?.ok_or_else(|| format!("{}: not there", src.display()))?;
                let ctx = render::Context {
                    facts: &self.outcome.facts,
                    vars: &self.outcome.config.vars,
                };
                let rendered = render::render(&template.as_str().into(), &ctx)
                    .map_err(|e| format!("{}: {}", src.display(), e.message))?;
                let now = read(host, dest)?.unwrap_or_default();
                (
                    format!("{} — what apply would write", dest.display()),
                    now,
                    rendered,
                )
            }
            _ => {
                self.note = Some(format!("`{name}` has no content to diff"));
                return Ok(());
            }
        };

        let rows = diff::lines(&before, &after, 2);
        if rows.is_empty() {
            self.note = Some("identical — nothing would change".into());
            return Ok(());
        }
        self.mode = Mode::Diff(DiffView {
            title,
            rows,
            scroll: 0,
        });
        Ok(())
    }

    // ---------------------------------------------------------------- apply

    pub fn apply(
        &mut self,
        host: &OsHost,
        config: Option<&Path>,
        cwd: &Path,
        verbose: bool,
    ) -> Result<(), String> {
        self.mode = Mode::Browse;
        // Re-plan rather than reuse: `run_apply` takes the outcome by value,
        // and a plan is cheap next to what it is about to do.
        let o = run::plan(host, config, cwd).map_err(|e| e.to_string())?;
        let code = crate::run_apply(host, o, verbose, &Default::default());
        println!();
        if code == ExitCode::SUCCESS {
            println!("press any key to return to bedouin");
        } else {
            println!("the run did not finish cleanly — press any key to return");
        }
        let _ = crossterm::event::read();
        self.replan(host, config, cwd)
    }
}

/// Text through the Host, so a FakeHost run sees the same files the plan did.
fn read(host: &dyn Host, p: &Path) -> Result<Option<String>, String> {
    match host.read(p).map_err(|e| e.to_string())? {
        None => Ok(None),
        Some(b) => {
            Ok(Some(String::from_utf8(b).map_err(|_| {
                format!("{}: not valid UTF-8", p.display())
            })?))
        }
    }
}

fn plan_sigil(o: &run::Outcome, id: &str) -> char {
    o.plan
        .items
        .iter()
        .find(|i| i.id == id)
        .map(|i| i.action.sigil())
        .unwrap_or(' ')
}

/// Where a name appears in the config, for `$EDITOR +N`. A plain search: the
/// alternative is threading spans out of the loader, and being one line off
/// costs nothing here.
fn line_of(host: &dyn Host, path: &Path, name: &str) -> Option<usize> {
    let text = read(host, path).ok().flatten()?;
    let needles = [
        format!("name: {name}"),
        format!("name: \"{name}\""),
        format!("{name}:"),
    ];
    text.lines()
        .position(|l| {
            needles
                .iter()
                .any(|n| l.trim_start().starts_with(n.as_str()))
        })
        .map(|i| i + 1)
}
