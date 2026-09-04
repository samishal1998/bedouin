//! A line diff, small enough to read.
//!
//! Trims the common head and tail, then shows what is left as removals
//! followed by additions. Not Myers: the texts here are a config file and a
//! rendered rc block, and the interesting change is almost always contiguous.
//! `ponytail: prefix/suffix trim, swap in a real LCS if a reordering diff ever
//! reads badly.`

#[derive(Debug, PartialEq, Eq)]
pub enum Row {
    Same(String),
    Removed(String),
    Added(String),
}

/// `context` lines of unchanged text are kept either side of the change.
pub fn lines(before: &str, after: &str, context: usize) -> Vec<Row> {
    let a: Vec<&str> = before.lines().collect();
    let b: Vec<&str> = after.lines().collect();

    let head = a
        .iter()
        .zip(&b)
        .take_while(|(x, y)| x == y)
        .count()
        .min(a.len().min(b.len()));
    let tail = a[head..]
        .iter()
        .rev()
        .zip(b[head..].iter().rev())
        .take_while(|(x, y)| x == y)
        .count();

    let mut out = Vec::new();
    if head == a.len() && head == b.len() {
        return out; // identical
    }

    let lead = head.saturating_sub(context);
    for l in &a[lead..head] {
        out.push(Row::Same((*l).to_string()));
    }
    for l in &a[head..a.len() - tail] {
        out.push(Row::Removed((*l).to_string()));
    }
    for l in &b[head..b.len() - tail] {
        out.push(Row::Added((*l).to_string()));
    }
    let trail = (a.len() - tail + context).min(a.len());
    for l in &a[a.len() - tail..trail] {
        out.push(Row::Same((*l).to_string()));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn render(rows: &[Row]) -> String {
        rows.iter()
            .map(|r| match r {
                Row::Same(s) => format!("  {s}"),
                Row::Removed(s) => format!("- {s}"),
                Row::Added(s) => format!("+ {s}"),
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[test]
    fn identical_text_has_no_rows() {
        assert!(lines("a\nb\n", "a\nb\n", 2).is_empty());
    }

    #[test]
    fn a_changed_line_shows_both_sides_with_context() {
        let d = lines("a\nb\nc\n", "a\nB\nc\n", 1);
        assert_eq!(render(&d), "  a\n- b\n+ B\n  c");
    }

    #[test]
    fn an_addition_has_no_removal() {
        let d = lines("a\nc\n", "a\nb\nc\n", 1);
        assert_eq!(render(&d), "  a\n+ b\n  c");
    }

    #[test]
    fn a_removal_has_no_addition() {
        let d = lines("a\nb\nc\n", "a\nc\n", 1);
        assert_eq!(render(&d), "  a\n- b\n  c");
    }

    #[test]
    fn one_side_empty_is_all_of_the_other() {
        assert_eq!(render(&lines("", "x\ny\n", 2)), "+ x\n+ y");
        assert_eq!(render(&lines("x\ny\n", "", 2)), "- x\n- y");
    }

    #[test]
    fn context_is_bounded_not_the_whole_file() {
        let before: String = (0..40).map(|i| format!("l{i}\n")).collect();
        let after = before.replace("l20\n", "CHANGED\n");
        let d = lines(&before, &after, 2);
        // 2 leading + 1 removed + 1 added + 2 trailing, not 40.
        assert_eq!(d.len(), 6, "{}", render(&d));
    }
}
