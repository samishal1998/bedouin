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
    /// Label/value pairs for the aside pane. Built here so the view stays a
    /// pure function of this and never reaches back into the config.
    pub details: Vec<(String, String)>,
}

#[derive(Clone)]
pub struct Field {
    pub label: String,
    /// Passed to `edit::set_field`; `None` targets an alias value instead.
    pub key: Option<String>,
    pub current: String,
}

pub struct Form {
    /// A new entry rather than an edit of the selected one. `enter` on the
    /// last field creates it; the fields are the ones `add` needs.
    pub creating: bool,
    pub title: String,
    /// Every field this item can be edited through, not just the first.
    pub fields: Vec<Field>,
    pub idx: usize,
    /// The edit buffer for the field at `idx`.
    pub value: String,
}

impl Form {
    /// Moving between fields abandons the buffer for the one being left --
    /// committing is `enter`, and a half-typed value silently carried into
    /// the next field would be worse than losing it.
    pub fn move_by(&mut self, d: isize) {
        let n = self.fields.len() as isize;
        self.idx = (((self.idx as isize + d) % n + n) % n) as usize;
        self.value = self.fields[self.idx].current.clone();
    }
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
    /// The config as written. Forms seed from this, never from the resolved
    /// value: `from: { macos: brew, default: apt }` resolves to `apt` on this
    /// machine, and writing that back would flatten the condition away.
    text: String,
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
            text: String::new(),
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
        self.text = read(host, &self.outcome.loaded.entry)
            .ok()
            .flatten()
            .unwrap_or_default();
        let o = &self.outcome;
        let text = self.text.clone();
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
                details: {
                    let mut d = vec![
                        ("id".into(), i.id.clone()),
                        ("kind".into(), kind_label(i.kind).to_string()),
                        ("action".into(), action_label(&i.action)),
                        ("detail".into(), i.detail.clone()),
                    ];
                    if i.needs_root {
                        d.push(("needs root".into(), "yes".into()));
                    }
                    for (field, arm) in &i.arms {
                        d.push((format!("arm · {field}"), arm.clone()));
                    }
                    d.extend(payload_details(&i.payload));
                    d
                },
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
                fields: PKG_KEYS
                    .iter()
                    .map(|k| Field {
                        label: (*k).into(),
                        key: Some((*k).to_string()),
                        current: raw_field(&text, "packages:", &p.name, k).unwrap_or_default(),
                    })
                    .collect(),
                plan_id: Some(format!("package/{}", p.name)),
                details: {
                    let mut d = vec![(
                        "from".into(),
                        if p.from.is_empty() {
                            "script".into()
                        } else {
                            p.from
                                .iter()
                                .map(|m| m.to_string())
                                .collect::<Vec<_>>()
                                .join(", ")
                        },
                    )];
                    d.push((
                        "version".into(),
                        p.version.clone().unwrap_or_else(|| "latest".into()),
                    ));
                    if let Some(sc) = &p.script {
                        d.push(("script".into(), sc.trim().to_string()));
                    }
                    if !p.needs.is_empty() {
                        d.push(("needs".into(), p.needs.join(", ")));
                    }
                    if !p.path.is_empty() {
                        d.push(("path".into(), p.path.join(", ")));
                    }
                    for (k, v) in &p.aliases {
                        d.push((format!("alias · {k}"), v.clone()));
                    }
                    if let Some(c) = &p.completions {
                        d.push(("completions".into(), c.join(" ")));
                    }
                    for b in &p.rc {
                        d.push((format!("rc · {}", b.file), b.content.trim().to_string()));
                    }
                    d
                },
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
                details: vec![
                    ("src".into(), f.src.clone()),
                    ("dest".into(), f.dest.clone()),
                    (
                        "mode".into(),
                        f.mode.clone().unwrap_or_else(|| "0644".into()),
                    ),
                ],
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
                details: {
                    let mut d = vec![
                        ("url".into(), r.url.clone()),
                        ("dest".into(), r.dest.clone()),
                    ];
                    if let Some(rf) = &r.r#ref {
                        d.push(("ref".into(), rf.clone()));
                    }
                    d
                },
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
                details: vec![
                    ("points at".into(), l.src.clone()),
                    ("link".into(), l.dest.clone()),
                ],
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
                details: vec![
                    ("alias".into(), k.clone()),
                    ("expands to".into(), v.clone()),
                ],
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
                fields: LANG_KEYS
                    .iter()
                    .map(|k| Field {
                        label: (*k).into(),
                        key: Some((*k).to_string()),
                        current: raw_field(&text, "languages:", &l.name, k).unwrap_or_default(),
                    })
                    .collect(),
                plan_id: Some(format!("language/{}", l.name)),
                details: vec![
                    (
                        "installer".into(),
                        l.installer
                            .map(|m| m.to_string())
                            .unwrap_or_else(|| "its own (default)".into()),
                    ),
                    (
                        "version".into(),
                        l.version.clone().unwrap_or_else(|| "latest".into()),
                    ),
                ],
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
                    details: drift_details(d),
                })
                .collect(),
            Err(e) => vec![Row {
                sigil: 'x',
                kind: "error".into(),
                name: "doctor".into(),
                detail: e.to_string(),
                fields: vec![],
                plan_id: None,
                details: vec![],
            }],
        };

        self.env_rows = envfile::referenced(&o.loaded.raw, &o.facts.env, &o.loaded.root, host)
            .into_iter()
            .map(|r| {
                let consequence = if r.match_key {
                    "the target simply does not match"
                } else if r.has_default {
                    "falls back to its `| default(…)`"
                } else {
                    "resolve fails — there is nothing to fall back to"
                };
                Row {
                    sigil: if r.set { ' ' } else { '?' },
                    kind: if r.set { "set".into() } else { "unset".into() },
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
                    details: vec![
                        ("read at".into(), r.site.clone()),
                        (
                            "state".into(),
                            if r.set { "set" } else { "not set" }.to_string(),
                        ),
                        ("if unset".into(), consequence.to_string()),
                    ],
                    name: r.name,
                }
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
        if row.fields.is_empty() {
            self.note = Some("no editable field here — `e` opens the config".into());
            return;
        }
        self.mode = Mode::Form(Form {
            creating: false,
            title: row.name.clone(),
            value: row.fields[0].current.clone(),
            fields: row.fields.clone(),
            idx: 0,
        });
    }

    /// `n`. Only where there is a way to add: `edit::add_package` for
    /// packages, and `set_alias` which creates the alias if it is absent.
    /// Everywhere else `e` is the answer, and it says so.
    pub fn open_new(&mut self) {
        let fields = match self.section {
            Section::Packages => vec![("name", ""), ("from", "apt"), ("version", "")],
            Section::Aliases => vec![("name", ""), ("value", "")],
            s => {
                self.note = Some(format!(
                    "bedouin cannot add a `{}` entry for you — press e",
                    s.title()
                ));
                return;
            }
        };
        let fields: Vec<Field> = fields
            .into_iter()
            .map(|(label, seed)| Field {
                label: label.into(),
                key: Some(label.into()),
                current: seed.into(),
            })
            .collect();
        self.mode = Mode::Form(Form {
            creating: true,
            title: format!("new {}", self.section.title().trim_end_matches('s')),
            value: fields[0].current.clone(),
            fields,
            idx: 0,
        });
    }

    /// Create the entry the form describes. Separate from `commit_field`
    /// because adding takes every field at once, where editing takes one.
    pub fn commit_new(
        &mut self,
        host: &dyn Host,
        fields: &[Field],
        idx: usize,
        value: &str,
    ) -> Result<(), String> {
        let get = |label: &str| -> String {
            fields
                .iter()
                .enumerate()
                .find(|(_, f)| f.label == label)
                .map(|(i, f)| {
                    if i == idx {
                        value.to_string()
                    } else {
                        f.current.clone()
                    }
                })
                .unwrap_or_default()
        };
        let name = get("name");
        if name.trim().is_empty() {
            return Err("a name is required".into());
        }

        let path = self.outcome.loaded.entry.clone();
        let before = read(host, &path)?.ok_or_else(|| format!("{}: gone", path.display()))?;

        let after = match self.section {
            Section::Packages => {
                let from = get("from");
                if from.trim().is_empty() {
                    return Err("`from` is required — which manager installs it?".into());
                }
                let v = get("version");
                let version = (!v.trim().is_empty()).then_some(v);
                bedouin_core::edit::add_package(&before, &name, &from, version.as_deref())
                    .map_err(|e| e.to_string())?
            }
            Section::Aliases => bedouin_core::edit::set_alias(&before, None, &name, &get("value"))
                .map_err(|e| e.to_string())?,
            _ => return Err("nothing here can be added".into()),
        };

        host.write(&path, after.as_bytes(), 0o600)
            .map_err(|e| e.to_string())?;
        self.mode = Mode::Diff(DiffView {
            title: format!("{} — added `{name}`", path.display()),
            rows: diff::lines(&before, &after, 2),
            scroll: 0,
        });
        self.note = Some(format!("added `{name}` — press r to re-plan"));
        Ok(())
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

/// Everything `edit::set_field` can set on a package, in the order they read.
const PKG_KEYS: &[&str] = &["from", "version", "only", "needs", "path", "script"];
const LANG_KEYS: &[&str] = &["installer", "version", "only"];

/// The value of `key:` inside `name`'s entry, exactly as written.
///
/// `None` when the entry is inline (`- { … }`) or the value opens a nested
/// block: neither is a single scalar a one-line form can round-trip, and
/// guessing would flatten it.
fn raw_field(text: &str, section: &str, name: &str, key: &str) -> Option<String> {
    let lines: Vec<&str> = text.lines().collect();
    let start = lines
        .iter()
        .position(|l| l.trim_start().starts_with(section))?;
    let mut entry = None;
    for (i, l) in lines.iter().enumerate().skip(start + 1) {
        let t = l.trim_start();
        // Out of the section entirely.
        if !l.starts_with(' ') && !t.is_empty() && !t.starts_with('#') {
            break;
        }
        if t.starts_with("- ") || t.starts_with('-') {
            let is_ours = t.contains(&format!("name: {name}")) && {
                // `jq` must not match `jq-extra`.
                let after = t.split(&format!("name: {name}")).nth(1).unwrap_or("");
                after.is_empty() || after.starts_with([',', ' ', '}', '\n'])
            };
            entry = if is_ours { Some(i) } else { None };
            // An inline entry holds everything on one line; a form cannot
            // round-trip it, and `commit_field` says so when you try.
            if is_ours && t.starts_with("- {") {
                return None;
            }
            if is_ours && t.starts_with("- name:") {
                continue;
            }
        }
        let Some(e) = entry else { continue };
        if i == e {
            continue;
        }
        if let Some(v) = t.strip_prefix(&format!("{key}:")) {
            let v = v.trim();
            // A block value (`key:` then indented lines) is not a scalar.
            return if v.is_empty() {
                None
            } else {
                Some(v.to_string())
            };
        }
    }
    None
}

fn action_label(a: &bedouin_core::plan::Action) -> String {
    use bedouin_core::plan::Action::*;
    match a {
        Create => "create".into(),
        Adopt => "adopt (backs up what is there)".into(),
        Upgrade { from, to } => format!("upgrade {from} -> {to}"),
        Reinstall {
            from_method,
            to_method,
        } => {
            format!("reinstall via {from_method} -> {to_method}")
        }
        Remove => "remove".into(),
        NoOp => "already matches".into(),
    }
}

fn payload_details(p: &Payload) -> Vec<(String, String)> {
    use Payload::*;
    match p {
        Manager(m) => vec![("manager".into(), m.to_string())],
        Language {
            installer, version, ..
        } => vec![
            ("installer".into(), installer.to_string()),
            (
                "version".into(),
                version.clone().unwrap_or_else(|| "latest".into()),
            ),
        ],
        Package {
            manager, version, ..
        } => vec![
            ("manager".into(), manager.to_string()),
            (
                "version".into(),
                version.clone().unwrap_or_else(|| "latest".into()),
            ),
        ],
        ScriptPackage { script, .. } => vec![("script".into(), script.trim().to_string())],
        Dir(d) => vec![("directory".into(), d.display().to_string())],
        File { src, dest, mode } => vec![
            ("src".into(), src.display().to_string()),
            ("dest".into(), dest.display().to_string()),
            ("mode".into(), format!("{mode:o}")),
        ],
        RcBlock {
            file,
            marker,
            content,
        } => vec![
            ("file".into(), file.display().to_string()),
            ("block".into(), marker.clone()),
            ("content".into(), content.trim().to_string()),
        ],
        Completions { argv, dest } => vec![
            ("generator".into(), argv.join(" ")),
            ("writes".into(), dest.display().to_string()),
        ],
        Repo { url, dest, .. } => vec![
            ("url".into(), url.clone()),
            ("dest".into(), dest.display().to_string()),
        ],
        Link { src, dest } => vec![
            ("points at".into(), src.display().to_string()),
            ("link".into(), dest.display().to_string()),
        ],
        _ => vec![],
    }
}

fn drift_details(d: &doctor::Drift) -> Vec<(String, String)> {
    use doctor::Drift::*;
    match d {
        Edited { file, .. } => vec![
            ("what".into(), "managed content was changed by hand".into()),
            ("file".into(), file.clone()),
            ("apply would".into(), "overwrite it".into()),
        ],
        Missing { file, .. } => vec![
            ("what".into(), "bedouin wrote it and it is gone".into()),
            ("file".into(), file.clone()),
            ("apply would".into(), "put it back".into()),
        ],
        Resolved {
            field, was, now, ..
        } => vec![
            ("what".into(), "the config resolves differently now".into()),
            ("field".into(), field.clone()),
            ("was".into(), was.clone()),
            ("now".into(), now.clone()),
        ],
        Unterminated { file, why, .. } => vec![
            (
                "what".into(),
                "a block bedouin owns was never closed".into(),
            ),
            ("file".into(), file.clone()),
            ("why".into(), why.clone()),
            ("apply would".into(), "refuse to touch this file".into()),
        ],
        Incomplete { .. } => vec![
            (
                "what".into(),
                "a step recorded intent and never finished".into(),
            ),
            ("apply would".into(), "redo it".into()),
        ],
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
