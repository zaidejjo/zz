//! Stub: unified diff via the `similar` crate. M3 work.

use crate::FmtError;

pub fn unified_diff(path: &str, original: &str, formatted: &str) -> Result<String, FmtError> {
    use similar::{ChangeTag, TextDiff};
    let diff = TextDiff::from_lines(original, formatted);
    let mut out = String::new();
    out.push_str(&format!("--- {path}\n+++ {path}\n"));
    for change in diff.iter_all_changes() {
        let sign = match change.tag() {
            ChangeTag::Equal => ' ',
            ChangeTag::Insert => '+',
            ChangeTag::Delete => '-',
        };
        out.push(sign);
        out.push_str(change.value());
        if !change.value().ends_with('\n') {
            out.push('\n');
        }
    }
    Ok(out)
}
