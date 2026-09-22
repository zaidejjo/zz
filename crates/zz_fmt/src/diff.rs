//! Unified diff renderer used by `zz fmt --check` and the LSP.
//!
//! Wraps the `similar` crate to produce a colored unified diff with
//! 3 lines of context, suitable for direct terminal output. The
//! coloring is ANSI; callers can disable it by using `unified_diff_plain`.

use crate::FmtError;
use similar::{ChangeTag, TextDiff};

/// Old-side start index and length covered by `op`.
fn op_old_range(op: &similar::DiffOp) -> (usize, usize) {
    match *op {
        similar::DiffOp::Equal { old_index, len, .. } => (old_index, len),
        similar::DiffOp::Delete {
            old_index, old_len, ..
        } => (old_index, old_len),
        similar::DiffOp::Insert { old_index, .. } => (old_index, 0),
        similar::DiffOp::Replace {
            old_index, old_len, ..
        } => (old_index, old_len),
    }
}

/// New-side start index and length covered by `op`.
fn op_new_range(op: &similar::DiffOp) -> (usize, usize) {
    match *op {
        similar::DiffOp::Equal { new_index, len, .. } => (new_index, len),
        similar::DiffOp::Insert {
            new_index, new_len, ..
        } => (new_index, new_len),
        similar::DiffOp::Delete { new_index, .. } => (new_index, 0),
        similar::DiffOp::Replace {
            new_index, new_len, ..
        } => (new_index, new_len),
    }
}

/// Compute a colored ANSI unified diff between `original` and
/// `formatted`. `path` is shown in the `---`/`+++` headers. Returns
/// an empty string when the inputs are identical.
pub fn unified_diff(path: &str, original: &str, formatted: &str) -> Result<String, FmtError> {
    if original == formatted {
        return Ok(String::new());
    }

    let diff = TextDiff::from_lines(original, formatted);
    let mut out = String::new();
    out.push_str(&format!("--- {path}\n+++ {path}\n"));

    // `grouped_ops(3)` returns a list of hunks, each a list of
    // line-level DiffOps with up to 3 lines of context.
    for (idx, hunk) in diff.grouped_ops(3).iter().enumerate() {
        if idx > 0 {
            out.push('\n');
        }
        // Hunk header ranges come from the first and last ops in
        // the hunk. Use the first op to anchor the start line.
        let first = &hunk[0];
        let last = &hunk[hunk.len() - 1];
        let (old_start, old_len) = op_old_range(first);
        let (new_start, new_len) = op_new_range(last);
        // `old_index`/`new_index` are 0-based; emit 1-based.
        out.push_str(&format!(
            "@@ -{},{} +{},{} @@\n",
            old_start + 1,
            old_len,
            new_start + 1,
            new_len
        ));

        for op in hunk {
            // `iter_changes(op)` yields Change values for the
            // individual added/removed/equal lines that make up
            // the op. We re-emit each one with the right sign and
            // ANSI color.
            for change in diff.iter_changes(op) {
                let (sign, prefix) = match change.tag() {
                    ChangeTag::Equal => (' ', "\x1b[0m"),
                    ChangeTag::Insert => ('+', "\x1b[32m"), // green
                    ChangeTag::Delete => ('-', "\x1b[31m"), // red
                };
                out.push_str(prefix);
                out.push(sign);
                out.push_str(change.value());
                if !change.value().ends_with('\n') {
                    out.push('\n');
                }
                out.push_str("\x1b[0m");
            }
        }
    }

    Ok(out)
}

/// Compute a unified diff without ANSI color codes. Suitable for
/// piping to a file, LSP, or non-tty output.
pub fn unified_diff_plain(path: &str, original: &str, formatted: &str) -> Result<String, FmtError> {
    let colored = unified_diff(path, original, formatted)?;
    Ok(strip_ansi(&colored))
}

/// Strip ANSI escape sequences from a string.
pub fn strip_ansi(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\x1b' {
            // Skip until we see a letter (terminator).
            while let Some(&next) = chars.peek() {
                chars.next();
                if next.is_ascii_alphabetic() {
                    break;
                }
            }
        } else {
            out.push(c);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_diff_for_identical_inputs() {
        let d = unified_diff("a.zz", "x := 1\n", "x := 1\n").unwrap();
        assert!(d.is_empty());
    }

    #[test]
    fn produces_diff_for_changed_line() {
        let d = unified_diff("a.zz", "x := 1\n", "x := 2\n").unwrap();
        assert!(d.contains("--- a.zz"));
        assert!(d.contains("+++ a.zz"));
        assert!(d.contains("-x := 1"));
        assert!(d.contains("+x := 2"));
    }

    #[test]
    fn plain_strips_ansi() {
        let d = unified_diff("a.zz", "x := 1\n", "x := 2\n").unwrap();
        let plain = unified_diff_plain("a.zz", "x := 1\n", "x := 2\n").unwrap();
        assert!(!plain.contains('\x1b'));
        assert!(plain.contains("-x := 1"));
        assert!(d.contains('\x1b'));
    }

    #[test]
    fn strip_ansi_handles_empty() {
        assert_eq!(strip_ansi(""), "");
        assert_eq!(strip_ansi("hello"), "hello");
        assert_eq!(strip_ansi("\x1b[31mred\x1b[0m"), "red");
    }

    #[test]
    fn multi_hunk_diff() {
        let original = "a := 1\nb := 2\nc := 3\nd := 4\ne := 5\nf := 6\n";
        let formatted = "a := 1\nb := 22\nc := 3\nd := 4\ne := 55\nf := 6\n";
        let d = unified_diff("a.zz", original, formatted).unwrap();
        assert!(d.contains("--- a.zz"));
        assert!(d.contains("@@"));
        let hunk_count = d.matches("@@").count();
        assert!(hunk_count >= 2, "got {hunk_count} hunks in: {d}");
    }
}
