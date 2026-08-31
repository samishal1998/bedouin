//! ANSI styling.
//!
//! Whether to colour is a property of the terminal the process is attached to,
//! not of any one render call -- and `render` is called from a dozen places.
//! So it is decided once, by the CLI, and read here. A library run (tests,
//! `FakeHost`) leaves it off and gets plain text, which is also what makes
//! golden-output assertions possible.

use std::sync::atomic::{AtomicBool, Ordering};

static ON: AtomicBool = AtomicBool::new(false);

/// Called once by the CLI, after deciding from the tty and `NO_COLOR`.
pub fn set_enabled(on: bool) {
    ON.store(on, Ordering::Relaxed);
}

pub fn enabled() -> bool {
    ON.load(Ordering::Relaxed)
}

fn paint(code: &str, s: &str) -> String {
    if enabled() {
        format!("\x1b[{code}m{s}\x1b[0m")
    } else {
        s.to_string()
    }
}

pub fn bold(s: &str) -> String {
    paint("1", s)
}
pub fn dim(s: &str) -> String {
    paint("2", s)
}
pub fn red(s: &str) -> String {
    paint("31", s)
}
pub fn green(s: &str) -> String {
    paint("32", s)
}
pub fn yellow(s: &str) -> String {
    paint("33", s)
}
pub fn blue(s: &str) -> String {
    paint("34", s)
}
pub fn cyan(s: &str) -> String {
    paint("36", s)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn styling_is_off_by_default_so_output_stays_assertable() {
        // Not `set_enabled(false)` first: the point is the default.
        assert_eq!(green("ok"), "ok");
        assert!(!bold("x").contains('\x1b'));
    }
}
