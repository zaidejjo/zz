//! Doc-test extraction (`/// ```zz` fences → synthetic `@test` functions).
//!
//! A fence is a run of `///` comment lines:
//! ```text
//! /// ```zz
//! /// assert_eq(add(1, 2), 3)
//! /// ```
//! ```
//! The header accepts `,ignore` and `,should_panic` (mirroring `@test`
//! metadata). Body lines starting with `#` are compiled but hidden from
//! rendered docs (Rust convention); they become blank lines so remaining
//! lines keep their file line numbers.
//!
//! Design notes:
//! - Only explicit `zz` fences are extracted; plain ``` fences are prose.
//! - Extraction is pure text in/out. The test runner grafts parsed bodies
//!   as `Stmt::Func` nodes (line-exact via newline-padded parsing) and the
//!   AOT harness appends equivalent generated source. Both derive from
//!   [`Doctest::body_text`], so VM and AOT always agree.
//! - Unterminated fences and unknown header options are load errors (loud,
//!   with file position) — never silently skipped.

use crate::diag::{error_at, RawDiag};
use crate::span::Span;

/// One extracted fence.
#[derive(Debug, Clone, PartialEq)]
pub struct Doctest {
    /// Zero-based extraction order (stable synthetic name suffix).
    pub index: usize,
    /// Byte span of the opening fence line (for diagnostics).
    pub header_span: Span,
    /// 1-based file line of the first body line (for messages).
    pub first_line: usize,
    /// Body lines with original file line numbers preserved as blank
    /// padding: `lines[i]` is `Some(text)` for code or `None` for hidden
    /// (`#`) lines. Joining with `\n` after `first_line - 1` leading
    /// newlines reproduces file line numbers exactly.
    pub lines: Vec<Option<String>>,
    pub ignore: bool,
    pub should_panic: bool,
    /// Assert the snippet fails to compile (never executed).
    pub compile_fail: bool,
    /// Check the snippet but never execute it.
    pub no_run: bool,
    /// Shared setup: prepended to every test fence in the file, never a
    /// test itself and never named.
    pub is_prelude: bool,
    /// Test-only sequence (preludes excluded) driving synthetic names, so
    /// displays read `doctest m#0, m#1` with no gaps. `usize::MAX` for
    /// preludes (never named).
    pub test_index: usize,
}

impl Doctest {
    /// Synthetic function name: reserved `__zz_` prefix, no dots.
    pub fn func_name(&self) -> String {
        format!("__zz_doctest_{}", self.test_index)
    }

    /// Display name used by the runner (`--filter`, output, JSON).
    pub fn display_name(&self, stem: &str) -> String {
        format!("doctest {stem}#{}", self.test_index)
    }

    /// Body text with hidden lines blanked (line count preserved).
    pub fn body_text(&self) -> String {
        self.lines
            .iter()
            .map(|l| l.as_deref().unwrap_or(""))
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// Source that parses to a program whose statement spans sit on the
    /// original file lines: `first_line - 1` blank lines, then the body.
    /// Columns shift (the `/// ` prefix is stripped); lines are exact.
    pub fn padded_source(&self) -> String {
        let mut out = String::new();
        for _ in 1..self.first_line {
            out.push('\n');
        }
        out.push_str(&self.body_text());
        out.push('\n');
        out
    }

    /// Generated source-text twin for the AOT harness (positions are
    /// approximate there; AOT failures carry no file:line anyway).
    /// `prelude` (already-extracted shared setup text) is inlined at the
    /// top of the body.
    pub fn harness_func(&self, prelude: &str) -> String {
        let mut meta = String::new();
        if self.ignore {
            meta.push_str("(ignore = true)");
        } else if self.should_panic {
            meta.push_str("(should_panic = true)");
        }
        let body = if prelude.is_empty() {
            self.body_text()
        } else {
            format!("{prelude}\n{}", self.body_text())
        };
        format!(
            "@test{} func {}() {{\n{}\n}}\n",
            meta,
            self.func_name(),
            body
        )
    }
}

/// Scan `source` for fences. Returns doctests plus hard-error diagnostics
/// (unterminated fence, unknown header option). `path` is used only for
/// human-readable positions inside messages.
pub fn extract(source: &str, path: &str) -> (Vec<Doctest>, Vec<RawDiag>) {
    let mut diags = Vec::new();
    let mut out: Vec<Doctest> = Vec::new();
    let mut test_count = 0usize;
    // Byte offset of each line start (for header spans).
    let mut line_starts: Vec<usize> = vec![0];
    for (i, b) in source.bytes().enumerate() {
        if b == b'\n' {
            line_starts.push(i + 1);
        }
    }
    let lines: Vec<&str> = source.split('\n').collect();
    let mut i = 0;
    while i < lines.len() {
        let Some(content) = comment_content(lines[i]) else {
            i += 1;
            continue;
        };
        let content = content.trim_end_matches('\r');
        let Some(header) = content
            .strip_prefix("```")
            .map(str::trim_start)
            .filter(|h| h == &"zz" || h.starts_with("zz,") || h.starts_with("zz "))
        else {
            i += 1;
            continue;
        };
        // Parse header options after `zz`.
        let rest = header[2..].trim_start_matches([' ', '\t']);
        let rest = rest.strip_prefix(',').unwrap_or(rest);
        let mut ignore = false;
        let mut should_panic = false;
        let mut compile_fail = false;
        let mut no_run = false;
        let mut is_prelude = false;
        let mut bad_option: Option<&str> = None;
        for opt in rest.split(',').map(str::trim).filter(|s| !s.is_empty()) {
            // Allow a single space separator too (` ```zz ignore `).
            for word in opt.split_whitespace() {
                match word {
                    "ignore" => ignore = true,
                    "should_panic" => should_panic = true,
                    "compile_fail" => compile_fail = true,
                    "no_run" => no_run = true,
                    "prelude" => is_prelude = true,
                    other => bad_option = Some(other),
                }
            }
        }
        let header_start = line_starts.get(i).copied().unwrap_or(0);
        let header_span = Span::new(header_start as u32, (header_start + lines[i].len()) as u32);
        if no_run && compile_fail {
            diags.push(error_at(
                format!("doctest cannot combine `no_run` with `compile_fail` in {path}"),
                header_span,
            ));
            i += 1;
            continue;
        }
        if is_prelude && (ignore || should_panic || compile_fail || no_run) {
            diags.push(error_at(
                format!("doctest `prelude` takes no other options in {path}"),
                header_span,
            ));
            i += 1;
            continue;
        }
        if let Some(bad) = bad_option {
            diags.push(error_at(
                format!(
                    "unknown doctest option `{bad}` (expected `ignore`, `should_panic`, `compile_fail`, `no_run`, `prelude`) in {path}"
                ),
                header_span,
            ));
            i += 1;
            continue;
        }
        // Collect body until the closing fence.
        let mut body: Vec<Option<String>> = Vec::new();
        let mut j = i + 1;
        let mut closed = false;
        while j < lines.len() {
            let Some(c) = comment_content(lines[j]) else {
                break;
            };
            let c = c.trim_end_matches('\r');
            if c.trim() == "```" {
                closed = true;
                break;
            }
            let stripped = c.strip_prefix(' ').unwrap_or(c);
            if stripped == "#" || stripped.starts_with("# ") {
                body.push(None);
            } else {
                body.push(Some(stripped.to_string()));
            }
            j += 1;
        }
        if !closed {
            diags.push(error_at(
                format!("unterminated doctest fence in {path} (missing closing ```)"),
                header_span,
            ));
            i = j;
            continue;
        }
        let index = out.len();
        let test_index = if is_prelude {
            usize::MAX
        } else {
            let n = test_count;
            test_count += 1;
            n
        };
        out.push(Doctest {
            index,
            header_span,
            first_line: i + 2,
            lines: body,
            ignore,
            should_panic,
            compile_fail,
            no_run,
            is_prelude,
            test_index,
        });
        i = j + 1;
    }
    (out, diags)
}

/// Strip a `///` doc-comment prefix: optional leading whitespace, `///`,
/// optional single space. Returns `None` for non-doc lines.
fn comment_content(line: &str) -> Option<&str> {
    let trimmed = line.trim_start();
    let rest = trimmed.strip_prefix("///")?;
    Some(rest.strip_prefix(' ').unwrap_or(rest))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_basic_fence() {
        let src = "/// Add numbers.\n/// ```zz\n/// assert_eq(1 + 1, 2)\n/// ```\nfunc add() {}\n";
        let (docs, diags) = extract(src, "m.zz");
        assert!(diags.is_empty());
        assert_eq!(docs.len(), 1);
        assert_eq!(docs[0].body_text(), "assert_eq(1 + 1, 2)");
        assert_eq!(docs[0].first_line, 3);
        assert_eq!(docs[0].func_name(), "__zz_doctest_0");
        assert_eq!(docs[0].display_name("m"), "doctest m#0");
    }

    #[test]
    fn header_options() {
        let src = "/// ```zz,ignore\n/// x\n/// ```\n/// ```zz,should_panic\n/// y\n/// ```\n";
        let (docs, diags) = extract(src, "m.zz");
        assert!(diags.is_empty());
        assert_eq!(docs.len(), 2);
        assert!(docs[0].ignore);
        assert!(!docs[1].ignore && docs[1].should_panic);
        assert!(docs[0].harness_func("").contains("@test(ignore = true)"));
    }

    #[test]
    fn plain_fences_ignored() {
        let src = "/// ```\n/// not code\n/// ```\n/// ```python\n/// x\n/// ```\n";
        let (docs, diags) = extract(src, "m.zz");
        assert!(diags.is_empty());
        assert!(docs.is_empty());
    }

    #[test]
    fn hidden_lines_blanked() {
        let src = "/// ```zz\n/// # hidden setup\n/// visible()\n/// ```\n";
        let (docs, diags) = extract(src, "m.zz");
        assert!(diags.is_empty());
        assert_eq!(docs[0].body_text(), "\nvisible()");
        // Padded source keeps file lines: body line 3 is file line 3.
        let padded = docs[0].padded_source();
        assert_eq!(padded.lines().count(), 3);
        assert_eq!(padded.lines().nth(2), Some("visible()"));
    }

    #[test]
    fn unterminated_is_error() {
        let src = "/// ```zz\n/// x\nfunc f() {}\n";
        let (docs, diags) = extract(src, "m.zz");
        assert!(docs.is_empty());
        assert!(diags.iter().any(|d| d.message.contains("unterminated")));
    }

    #[test]
    fn unknown_option_is_error() {
        let src = "/// ```zz,ignroe\n/// x\n/// ```\n";
        let (docs, diags) = extract(src, "m.zz");
        assert!(docs.is_empty());
        assert!(diags
            .iter()
            .any(|d| d.message.contains("unknown doctest option")));
    }

    #[test]
    fn non_comment_line_ends_fence_as_unterminated() {
        let src = "/// ```zz\n/// x\nplain code\n/// ```\n";
        let (docs, diags) = extract(src, "m.zz");
        assert!(docs.is_empty());
        assert!(diags.iter().any(|d| d.message.contains("unterminated")));
    }

    #[test]
    fn new_options_and_conflicts() {
        let src = "/// ```zz,compile_fail\n/// x\n/// ```\n\
                   /// ```zz,no_run\n/// y\n/// ```\n\
                   /// ```zz,prelude\n/// z\n/// ```\n";
        let (docs, diags) = extract(src, "m.zz");
        assert!(diags.is_empty());
        assert_eq!(docs.len(), 3);
        assert!(docs[0].compile_fail && !docs[0].no_run && !docs[0].is_prelude);
        assert!(docs[1].no_run);
        assert!(docs[2].is_prelude);
        // Test numbering skips preludes.
        assert_eq!(docs[0].test_index, 0);
        assert_eq!(docs[1].test_index, 1);

        let bad = "/// ```zz,no_run,compile_fail\n/// x\n/// ```\n";
        let (_, diags) = extract(bad, "m.zz");
        assert!(diags.iter().any(|d| d.message.contains("cannot combine")));

        let bad2 = "/// ```zz,prelude,ignore\n/// x\n/// ```\n";
        let (_, diags) = extract(bad2, "m.zz");
        assert!(diags
            .iter()
            .any(|d| d.message.contains("takes no other options")));
    }
}
