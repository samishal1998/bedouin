//! The mark, in characters.
//!
//! `assets/icon/bedouin-mark-mono.svg` is two peaks over a wide slack base
//! with an arched entrance cut out of the middle — low and wide, a tent
//! pitched rather than a mountain. This is that, at the two sizes a terminal
//! has room for.

/// Eight lines. The empty states, where there is room to say something.
pub const TENT: &[&str] = &[
    r"        /\          /\        ",
    r"       /  \        /  \       ",
    r"      /    \      /    \      ",
    r"     /      \    /      \     ",
    r"    /        \__/        \    ",
    r"   /                      \   ",
    r"  /         ______         \  ",
    r" /_________/      \_________\ ",
];

/// How much of `TENT` to show, as the splash reveals it. Frame `n` draws the
/// bottom `n` rows — the tent goes up the way one is pitched, from the ground.
pub fn raising(frame: usize) -> Vec<&'static str> {
    let n = frame.min(TENT.len());
    TENT[TENT.len() - n..].to_vec()
}

pub const FRAMES: usize = TENT.len();

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_tent_is_rectangular() {
        // Every line the same width, or the peaks lean.
        let w = TENT[0].chars().count();
        for (i, l) in TENT.iter().enumerate() {
            assert_eq!(l.chars().count(), w, "line {i} is a different width: {l:?}");
        }
    }

    #[test]
    fn it_is_plain_ascii() {
        // A tent that renders as boxes on someone's terminal is not a mark.
        for l in TENT {
            assert!(l.is_ascii(), "{l:?}");
        }
    }

    #[test]
    fn raising_goes_from_the_ground_up_and_ends_whole() {
        assert!(raising(0).is_empty());
        assert_eq!(raising(1), vec![TENT[TENT.len() - 1]], "the base first");
        assert_eq!(raising(FRAMES), TENT.to_vec(), "and finishes complete");
        assert_eq!(raising(99), TENT.to_vec(), "past the end is still whole");
    }
}
