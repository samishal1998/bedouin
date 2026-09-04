//! Drawing. A pure function of `App`; it changes nothing.

use super::art;
use super::diff;
use super::model::{App, Mode, Section};
use super::theme;
use ratatui::prelude::*;
use ratatui::widgets::{Block, Borders, Clear, List, ListItem, Paragraph, Tabs, Wrap};

/// Below this the aside would leave neither half readable, so the list takes
/// the whole width and `d` is how you see detail.
const ASIDE_MIN_COLS: u16 = 90;

pub fn draw(app: &mut App, f: &mut Frame) {
    let rows = Layout::vertical([
        Constraint::Length(1), // tabs
        Constraint::Min(3),    // body
        Constraint::Length(1), // footer
    ])
    .split(f.area());

    tabs(app, f, rows[0]);

    if rows[1].width >= ASIDE_MIN_COLS {
        let cols = Layout::horizontal([Constraint::Percentage(58), Constraint::Percentage(42)])
            .split(rows[1]);
        list(app, f, cols[0]);
        aside(app, f, cols[1]);
    } else {
        list(app, f, rows[1]);
    }

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
            let n = app.len_of(*s);
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
            .divider(Span::styled("·", theme::quiet()))
            .style(theme::quiet())
            .highlight_style(theme::accent()),
        area,
    );
}

fn framed(title: &str) -> Block<'static> {
    Block::default()
        .borders(Borders::ALL)
        .border_style(theme::quiet())
        .title(Span::styled(format!(" {title} "), theme::accent()))
}

fn list(app: &mut App, f: &mut Frame, area: Rect) {
    let section = app.section;
    if app.rows().is_empty() {
        f.render_widget(
            empty_pane(section, area).block(framed(section.title())),
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
                    Style::new().fg(theme::sigil(r.sigil)),
                ),
                Span::styled(format!("{:<kindw$}  ", r.kind), theme::label()),
                Span::styled(format!("{:<width$}  ", r.name), theme::body()),
                Span::styled(r.detail.clone(), theme::quiet()),
            ]))
        })
        .collect();

    let block = framed(section.title());
    let state = app.list_state();
    f.render_stateful_widget(
        List::new(items)
            .block(block)
            .highlight_style(theme::selected()),
        area,
        state,
    );
}

/// The selected item, opened out. Everything here is already on the row; the
/// view never reaches back into the config for it.
fn aside(app: &App, f: &mut Frame, area: Rect) {
    let Some(row) = app.selected() else {
        f.render_widget(
            Paragraph::new("nothing selected")
                .style(theme::quiet())
                .block(framed("details")),
            area,
        );
        return;
    };

    let mut lines = vec![Line::from(vec![
        Span::styled(
            format!("{} ", row.sigil),
            Style::new().fg(theme::sigil(row.sigil)),
        ),
        Span::styled(row.name.clone(), theme::accent()),
    ])];

    if row.details.is_empty() {
        lines.push(Line::from(""));
        lines.push(Line::styled(row.detail.clone(), theme::quiet()));
    }

    let pad = row
        .details
        .iter()
        .map(|(k, _)| k.len())
        .max()
        .unwrap_or(0)
        .min(14);

    for (k, v) in &row.details {
        lines.push(Line::from(""));
        // A value with newlines is content, not a field: give it its own
        // lines rather than crushing it onto one.
        if v.contains('\n') {
            lines.push(Line::styled(format!("{k}:"), theme::label()));
            for l in v.lines() {
                lines.push(Line::styled(format!("  {l}"), theme::body()));
            }
        } else {
            lines.push(Line::from(vec![
                Span::styled(format!("{k:<pad$}  "), theme::label()),
                Span::styled(v.clone(), theme::body()),
            ]));
        }
    }

    if !row.fields.is_empty() {
        lines.push(Line::from(""));
        lines.push(Line::styled("enter to edit", theme::quiet()));
    }

    f.render_widget(
        Paragraph::new(lines)
            .block(framed("details"))
            .wrap(Wrap { trim: false }),
        area,
    );
}

/// The tent going up while the first plan is computed.
pub fn splash(f: &mut Frame, frame: usize) {
    let rows = art::raising(frame);
    let h = art::FRAMES as u16 + 2;
    let w = art::TENT[0].chars().count() as u16;
    let area = f.area();
    if area.height < h || area.width < w {
        return;
    }
    // Anchored so the base stays put and the peaks rise into place.
    let top = area.height / 2 - h / 2 + (art::FRAMES - rows.len()) as u16;
    let rect = Rect {
        x: area.width / 2 - w / 2,
        y: top,
        width: w,
        height: rows.len() as u16 + 2,
    };
    let mut lines: Vec<Line> = rows
        .iter()
        .map(|l| Line::styled(*l, Style::new().fg(theme::MADDER_LIFT)))
        .collect();
    if rows.len() == art::FRAMES {
        lines.push(Line::from(""));
        lines.push(Line::styled(
            format!("{:^w$}", "bedouin", w = w as usize),
            theme::quiet(),
        ));
    }
    f.render_widget(Paragraph::new(lines), rect);
}

/// The empty state gets the mark: there is room, and nothing else to say.
fn empty_pane(s: Section, area: Rect) -> Paragraph<'static> {
    let mut lines: Vec<Line> = Vec::new();
    if area.height >= 14 && area.width >= 34 {
        for l in art::TENT {
            lines.push(Line::styled(*l, theme::quiet()));
        }
        lines.push(Line::from(""));
    }
    lines.push(Line::styled(empty_note(s), theme::quiet()));
    Paragraph::new(lines).alignment(Alignment::Center)
}

fn empty_note(s: Section) -> String {
    match s {
        Section::Plan => "No changes. The machine matches the config.".into(),
        Section::Doctor => "No drift. Everything managed matches what bedouin wrote.".into(),
        Section::Env => "This config reads no environment variables.".into(),
        _ => format!("Nothing declared under `{}:`.", s.title()),
    }
}

fn footer(app: &App, f: &mut Frame, area: Rect) {
    let (text, style) = match &app.mode {
        Mode::Confirm => ("apply these changes?  y / n".to_string(), theme::accent()),
        Mode::Form(_) => (
            "↑/↓ field   enter commit   esc cancel".to_string(),
            theme::label(),
        ),
        Mode::Diff(_) => (
            "j/k scroll   any other key closes".to_string(),
            theme::quiet(),
        ),
        Mode::Browse => {
            if let Some(n) = &app.note {
                (n.clone(), theme::label())
            } else if !app.warnings.is_empty() {
                (
                    format!("{} warning(s) — see `bedouin plan`", app.warnings.len()),
                    theme::label(),
                )
            } else {
                (
                    "tab section   j/k move   enter edit   n new   e $EDITOR   d diff   a apply   q quit"
                        .to_string(),
                    theme::quiet(),
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
            diff::Row::Same(s) => Line::styled(format!("  {s}"), theme::quiet()),
            diff::Row::Removed(s) => Line::styled(format!("- {s}"), theme::removed()),
            diff::Row::Added(s) => Line::styled(format!("+ {s}"), theme::added()),
        })
        .collect();

    f.render_widget(Paragraph::new(lines).block(framed(&v.title)), area);
}

fn form_pane(app: &App, f: &mut Frame) {
    let Mode::Form(form) = &app.mode else { return };
    let area = centred(f.area(), 68, 60);
    f.render_widget(Clear, area);

    let pad = form.fields.iter().map(|x| x.label.len()).max().unwrap_or(8);

    let mut body = vec![Line::from("")];
    for (i, field) in form.fields.iter().enumerate() {
        let active = i == form.idx;
        let shown = if active { &form.value } else { &field.current };
        let mut spans = vec![
            Span::styled(
                if active { "› " } else { "  " },
                Style::new().fg(theme::MADDER_LIFT),
            ),
            Span::styled(
                format!("{:<pad$}  ", field.label),
                if active {
                    theme::accent()
                } else {
                    theme::label()
                },
            ),
            Span::styled(
                if shown.is_empty() {
                    "(unset)".to_string()
                } else {
                    shown.clone()
                },
                if active {
                    theme::body().add_modifier(Modifier::BOLD)
                } else {
                    theme::quiet()
                },
            ),
        ];
        if active {
            spans.push(Span::styled("▏", theme::accent()));
        }
        body.push(Line::from(spans));
    }
    f.render_widget(
        Paragraph::new(body)
            .block(framed(&form.title))
            .wrap(Wrap { trim: false }),
        area,
    );
}
