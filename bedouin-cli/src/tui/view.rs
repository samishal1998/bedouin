//! Drawing. A pure function of `App`; it changes nothing.

use super::diff;
use super::model::{App, Mode, Section};
use ratatui::prelude::*;
use ratatui::widgets::{Block, Borders, Clear, List, ListItem, Paragraph, Tabs, Wrap};

pub fn draw(app: &mut App, f: &mut Frame) {
    let rows = Layout::vertical([
        Constraint::Length(1), // tabs
        Constraint::Min(3),    // list
        Constraint::Length(1), // footer
    ])
    .split(f.area());

    tabs(app, f, rows[0]);
    list(app, f, rows[1]);
    footer(app, f, rows[2]);

    // Modals last, over the top.
    match &app.mode {
        Mode::Diff(_) => diff_pane(app, f),
        Mode::Form(_) => form_pane(app, f),
        _ => {}
    }
}

fn tabs(app: &App, f: &mut Frame, area: Rect) {
    let titles: Vec<Line> = Section::ALL
        .iter()
        .map(|s| {
            let n = count(app, *s);
            Line::from(if n > 0 {
                format!("{} {}", s.title(), n)
            } else {
                s.title().to_string()
            })
        })
        .collect();
    let selected = Section::ALL.iter().position(|s| *s == app.section).unwrap();
    f.render_widget(
        Tabs::new(titles)
            .select(selected)
            .divider("·")
            .highlight_style(Style::new().bold().fg(Color::Cyan)),
        area,
    );
}

fn count(app: &App, s: Section) -> usize {
    app.len_of(s)
}

fn list(app: &mut App, f: &mut Frame, area: Rect) {
    let section = app.section;
    let empty = empty_note(app, section);
    if app.rows().is_empty() {
        f.render_widget(
            Paragraph::new(empty)
                .style(Style::new().fg(Color::DarkGray))
                .block(Block::default().borders(Borders::ALL).title(title(section)))
                .wrap(Wrap { trim: true }),
            area,
        );
        return;
    }

    let width = app.rows().iter().map(|r| r.name.len()).max().unwrap_or(4);
    let kindw = app.rows().iter().map(|r| r.kind.len()).max().unwrap_or(6);
    let items: Vec<ListItem> = app
        .rows()
        .iter()
        .map(|r| {
            ListItem::new(Line::from(vec![
                Span::styled(
                    format!("{} ", r.sigil),
                    Style::new().fg(sigil_colour(r.sigil)),
                ),
                Span::styled(
                    format!("{:<kindw$}  ", r.kind),
                    Style::new().fg(Color::Cyan),
                ),
                Span::raw(format!("{:<width$}  ", r.name)),
                Span::styled(r.detail.clone(), Style::new().fg(Color::DarkGray)),
            ]))
        })
        .collect();

    let block = Block::default().borders(Borders::ALL).title(title(section));
    let state = app.list_state();
    f.render_stateful_widget(
        List::new(items)
            .block(block)
            .highlight_style(Style::new().reversed()),
        area,
        state,
    );
}

fn title(s: Section) -> String {
    format!(" {} ", s.title())
}

fn empty_note(app: &App, s: Section) -> String {
    match s {
        Section::Plan => "No changes. The machine matches the config.".into(),
        Section::Doctor => "No drift. Everything managed matches what bedouin wrote.".into(),
        Section::Env => "This config reads no environment variables.".into(),
        _ => {
            let _ = app;
            format!("Nothing declared under `{}:`.", s.title())
        }
    }
}

fn sigil_colour(c: char) -> Color {
    match c {
        '+' => Color::Green,
        '-' => Color::Red,
        '~' => Color::Yellow,
        '!' | 'x' => Color::Red,
        '?' => Color::Yellow,
        _ => Color::DarkGray,
    }
}

fn footer(app: &App, f: &mut Frame, area: Rect) {
    let (text, style) = match &app.mode {
        Mode::Confirm => (
            "apply these changes?  y / n".to_string(),
            Style::new().fg(Color::Yellow).bold(),
        ),
        Mode::Form(_) => (
            "enter commit   esc cancel".to_string(),
            Style::new().fg(Color::Yellow),
        ),
        Mode::Diff(_) => (
            "j/k scroll   any other key closes".to_string(),
            Style::new().fg(Color::DarkGray),
        ),
        Mode::Browse => {
            if let Some(n) = &app.note {
                (n.clone(), Style::new().fg(Color::Yellow))
            } else if !app.warnings.is_empty() {
                (
                    format!("{} warning(s) — see `bedouin plan`", app.warnings.len()),
                    Style::new().fg(Color::Yellow),
                )
            } else {
                (
                    "tab section   j/k move   enter edit   e $EDITOR   d diff   a apply   q quit"
                        .to_string(),
                    Style::new().fg(Color::DarkGray),
                )
            }
        }
    };
    f.render_widget(
        Paragraph::new(text).style(style).wrap(Wrap { trim: true }),
        area,
    );
}

fn centred(area: Rect, pct_x: u16, pct_y: u16) -> Rect {
    let v = Layout::vertical([
        Constraint::Percentage((100 - pct_y) / 2),
        Constraint::Percentage(pct_y),
        Constraint::Percentage((100 - pct_y) / 2),
    ])
    .split(area);
    Layout::horizontal([
        Constraint::Percentage((100 - pct_x) / 2),
        Constraint::Percentage(pct_x),
        Constraint::Percentage((100 - pct_x) / 2),
    ])
    .split(v[1])[1]
}

fn diff_pane(app: &App, f: &mut Frame) {
    let Mode::Diff(v) = &app.mode else { return };
    let area = centred(f.area(), 84, 76);
    f.render_widget(Clear, area);

    let lines: Vec<Line> = v
        .rows
        .iter()
        .skip(v.scroll)
        .map(|r| match r {
            diff::Row::Same(s) => Line::styled(format!("  {s}"), Style::new().fg(Color::DarkGray)),
            diff::Row::Removed(s) => Line::styled(format!("- {s}"), Style::new().fg(Color::Red)),
            diff::Row::Added(s) => Line::styled(format!("+ {s}"), Style::new().fg(Color::Green)),
        })
        .collect();

    f.render_widget(
        Paragraph::new(lines).block(
            Block::default()
                .borders(Borders::ALL)
                .title(format!(" {} ", v.title)),
        ),
        area,
    );
}

fn form_pane(app: &App, f: &mut Frame) {
    let Mode::Form(form) = &app.mode else { return };
    let area = centred(f.area(), 60, 20);
    f.render_widget(Clear, area);
    let body = vec![
        Line::from(""),
        Line::from(vec![
            Span::styled(
                format!("{}: ", form.field.label),
                Style::new().fg(Color::Cyan),
            ),
            Span::styled(
                if form.value.is_empty() {
                    "(empty)".to_string()
                } else {
                    form.value.clone()
                },
                Style::new().bold(),
            ),
            Span::styled("▏", Style::new().fg(Color::Yellow)),
        ]),
    ];
    f.render_widget(
        Paragraph::new(body).block(
            Block::default()
                .borders(Borders::ALL)
                .title(format!(" {} ", form.title)),
        ),
        area,
    );
}
