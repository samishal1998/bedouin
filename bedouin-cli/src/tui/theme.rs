//! The mark's palette, in a terminal.
//!
//! Taken from `assets/icon/bedouin-mark-*.svg` and the names the site already
//! uses in `docs-site/src/styles/bedouin.css` — madder is the pigment, and it
//! is the one colour the project has.
//!
//! RGB rather than the sixteen ANSI slots: those are whatever the user's
//! theme says they are, and a brand colour that becomes someone's "bright
//! red" is not a brand colour. Terminals without truecolor degrade to their
//! nearest, which is the right failure.

use ratatui::style::{Color, Modifier, Style};

pub const MADDER: Color = Color::Rgb(0xA8, 0x2A, 0x24);
pub const MADDER_LIFT: Color = Color::Rgb(0xD4, 0x44, 0x3C);
pub const PALE: Color = Color::Rgb(0xF0, 0xB8, 0xB4);
pub const SAND: Color = Color::Rgb(0xF2, 0xEF, 0xE9);
pub const DIM: Color = Color::Rgb(0x8B, 0x85, 0x7D);
/// Only for text laid ON madder, where sand-on-madder is the mark's own pairing.
pub const ON_ACCENT: Color = Color::Rgb(0xF2, 0xEF, 0xE9);

/// The active tab, the focused border, the thing being pointed at.
pub fn accent() -> Style {
    Style::new().fg(MADDER_LIFT).add_modifier(Modifier::BOLD)
}

pub fn quiet() -> Style {
    Style::new().fg(DIM)
}

pub fn label() -> Style {
    Style::new().fg(PALE)
}

pub fn body() -> Style {
    Style::new().fg(SAND)
}

/// Selection reads as the mark: sand on madder, the pairing the logo uses.
pub fn selected() -> Style {
    Style::new()
        .bg(MADDER)
        .fg(ON_ACCENT)
        .add_modifier(Modifier::BOLD)
}

/// What a plan sigil means. Additions stay green because that convention is
/// older and louder than any brand, and a palette that makes "will install"
/// and "will remove" the same hue is a palette that lies. Everything else
/// sits in the madder family.
pub fn sigil(c: char) -> Color {
    match c {
        '+' => Color::Rgb(0x6F, 0x8F, 0x5E), // a green warm enough to sit beside madder
        '~' => MADDER_LIFT,
        '-' => MADDER,
        '!' | 'x' => MADDER,
        '?' => PALE,
        _ => DIM,
    }
}

/// Diff rows. Removals are madder; additions borrow the same green as `+`,
/// for the same reason.
pub fn added() -> Style {
    Style::new().fg(Color::Rgb(0x6F, 0x8F, 0x5E))
}

pub fn removed() -> Style {
    Style::new().fg(MADDER_LIFT)
}
