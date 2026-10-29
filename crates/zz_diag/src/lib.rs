//! Diagnostics infrastructure.
//!
//! The frontend produces `RawDiag`s (message + span + severity + fixits,
//! decoupled from any file store). Rendering produces Rust-quality colored
//! output with source context, carets, and inline suggestions.
//!
//! Transport types live in `zz_diag_core` so the checker and other pipeline
//! crates can emit diagnostics without depending on the renderer.

use std::io::Write;

use codespan_reporting::diagnostic::{Diagnostic as CsDiagnostic, Label, Severity as CsSeverity};
use codespan_reporting::files::SimpleFiles;
use colored::Colorize;

// Re-exported so every existing `zz_frontend::diag::…` path keeps working.
pub use zz_diag_core::*;

/// `SimpleFiles` uses `usize` as its file id.
pub type FileId = usize;

/// A rendered diagnostic bound to a file.
pub type Diag = CsDiagnostic<FileId>;

/// Source files known to the diagnostic renderer.
pub type Files = SimpleFiles<String, String>;

/// Render a fixit replacement for display: backtick-quoted, except a
/// replacement made solely of backticks (e.g. a raw-string closer),
/// which would render ambiguously as ``` — spell those by name (#414).
pub(crate) fn render_replacement(rep: &str) -> String {
    if !rep.is_empty() && rep.chars().all(|c| c == '`') {
        if rep.len() == 1 {
            "a backtick".to_string()
        } else {
            format!("{} backticks", rep.len())
        }
    } else {
        format!("`{rep}`")
    }
}

/// Bind a raw diagnostic to a file, producing a renderable [`Diag`].
///
/// A trait rather than an inherent method because [`RawDiag`] is defined in
/// `zz_diag_core`; callers bring `RawDiagBind` into scope.
pub trait RawDiagBind {
    fn bind(&self, file_id: FileId) -> Diag;
}

impl RawDiagBind for RawDiag {
    fn bind(&self, file_id: FileId) -> Diag {
        let cs_severity = match self.severity {
            Severity::Error => CsSeverity::Error,
            Severity::Warning => CsSeverity::Warning,
            Severity::Help => CsSeverity::Help,
        };

        let mut d = CsDiagnostic::new(cs_severity).with_message(self.message.clone());

        let mut labels = Vec::new();
        if let Some(span) = self.span {
            labels.push(Label::primary(file_id, span.to_range()));
        }
        if let Some(sec) = &self.secondary {
            labels.push(
                Label::secondary(file_id, sec.span.to_range()).with_message(sec.message.clone()),
            );
        }

        for fixit in &self.fixits {
            labels.push(
                Label::secondary(file_id, fixit.span.to_range())
                    .with_message(format!("help: {} → `{}`", fixit.message, fixit.replacement)),
            );
        }

        if !labels.is_empty() {
            d = d.with_labels(labels);
        }

        let mut notes = self.notes.clone();
        for fixit in &self.fixits {
            notes.push(format!(
                "{}: replace with {}",
                fixit.message,
                render_replacement(&fixit.replacement)
            ));
        }

        if !notes.is_empty() {
            d = d.with_notes(notes);
        }

        d
    }
}

// --- Rust-quality colorized renderer ----------------------------------------

/// Compute line number (1-based) and column (0-based) for a byte offset.
fn line_col_for(source: &str, byte_offset: usize) -> (usize, usize) {
    let mut line = 1;
    let mut col = 0;
    for (i, ch) in source.char_indices() {
        if i >= byte_offset {
            break;
        }
        if ch == '\n' {
            line += 1;
            col = 0;
        } else {
            col += 1;
        }
    }
    (line, col)
}

/// Find the start byte of the line containing `offset`.
fn line_start_for(source: &str, offset: usize) -> usize {
    match source[..offset].rfind('\n') {
        Some(p) => p + 1,
        None => 0,
    }
}

/// Find the end byte (exclusive) of the line containing `offset`.
fn line_end_for(source: &str, offset: usize) -> usize {
    source[offset..]
        .find('\n')
        .map(|p| offset + p)
        .unwrap_or(source.len())
}

/// Render a single diagnostic with source context and ANSI colors.
///
/// Output format (matches rustc):
/// ```text
/// error: undefined variable `prntlnn`
///   --> file.zz:5:1
///    |
///  5 | prntlnn("hello")
///    | ^^^^^^^ help: replace with `println`
///    |
///    = did you mean `println`?
/// ```
fn render_one_colored(files: &Files, file_id: FileId, raw: &RawDiag) -> String {
    use std::fmt::Write;

    let mut out = String::new();

    // --- Severity tag ---
    let tag = match raw.severity {
        Severity::Error => "error".red().bold(),
        Severity::Warning => "warning".yellow().bold(),
        Severity::Help => "help".cyan().bold(),
    };
    let _ = write!(out, "{}: {}", tag, raw.message.bold());

    // When the secondary span points outside the current source (e.g. a const
    // defined in an earlier REPL snippet), we emit it as a note instead.
    let mut secondary_note: Option<String> = None;

    // --- Location + source context ---
    if let Some(span) = raw.span {
        if let Ok(file) = files.get(file_id) {
            let name = file.name();
            let source: &str = file.source().as_ref();
            let start = span.start as usize;
            let end = span.end as usize;
            // A span that doesn't fit the loaded source (e.g. a runtime
            // error rendered against a fallback/empty source) must not
            // panic the renderer: emit the message without context.
            if start > end
                || end > source.len()
                || !source.is_char_boundary(start)
                || !source.is_char_boundary(end)
            {
                let (line_num, col) = line_col_for(source, start.min(source.len()));
                let _ = write!(out, "\n  --> {}:{line_num}:{}", name, col + 1);
            } else {
                let (line_num, col) = line_col_for(source, start);
                let _ = write!(out, "\n  --> {}:{line_num}:{}", name, col + 1);

                // Gutter width must fit the widest line number shown (the
                // secondary label may sit on a different line than the primary).
                let mut gutter = format!("{line_num:>4}").len();
                if let Some(sec) = &raw.secondary {
                    if sec.span.end as usize <= source.len() {
                        let (sec_line_num, _) = line_col_for(source, sec.span.start as usize);
                        gutter = gutter.max(format!("{sec_line_num:>4}").len());
                    }
                }
                let _ = write!(out, "\n    |");

                // Secondary label first: source line + `-` carets + message.
                // When the span points outside the current source (e.g. a const
                // defined in an earlier REPL snippet), fall back to a note.
                if let Some(sec) = &raw.secondary {
                    if sec.span.end as usize <= source.len() {
                        let (sec_line_num, _) = line_col_for(source, sec.span.start as usize);
                        let sec_lstart = line_start_for(source, sec.span.start as usize);
                        let sec_lend = line_end_for(source, sec.span.start as usize);
                        let sec_line_text = &source[sec_lstart..sec_lend];
                        let sec_pad = format!("{sec_line_num:>4}");
                        let _ = write!(out, "\n {sec_pad} | {sec_line_text}");
                        let sec_col_offset = sec.span.start as usize - sec_lstart;
                        let sec_len = (sec.span.end - sec.span.start) as usize;
                        let sec_spaces = " ".repeat(sec_col_offset);
                        let sec_dashes = "-".repeat(sec_len);
                        let _ = write!(
                            out,
                            "\n {:>width$} | {sec_spaces}{sec_dashes} {}",
                            "",
                            sec.message.cyan(),
                            width = gutter,
                        );
                    } else {
                        secondary_note = Some(sec.message.clone());
                    }
                }

                // Source line
                let lstart = line_start_for(source, start);
                let lend = line_end_for(source, start);
                let line_text = &source[lstart..lend];
                let pad = format!("{line_num:>4}");
                let _ = write!(out, "\n {pad} | {line_text}");

                // Carets under the span + inline fixit hint.
                // Multi-line spans only underline the visible first line —
                // a 200-char underline for a triple-quoted string is noise.
                let col_offset = start - lstart;
                let first_line_len = lend
                    .saturating_sub(lstart)
                    .saturating_sub(col_offset)
                    .max(1);
                let len = (end - start).max(1).min(first_line_len);
                let spaces = " ".repeat(col_offset);
                let carets = "^".repeat(len);
                let colored_carets = match raw.severity {
                    Severity::Error => carets.red().bold(),
                    Severity::Warning => carets.yellow().bold(),
                    Severity::Help => carets.cyan().bold(),
                };
                // Inline the first fixit hint directly under the carets
                // Use blank padding matching the source line prefix width
                let blank_pad = format!("{:>width$}", "", width = gutter);
                if let Some(fixit) = raw.fixits.first() {
                    // Deletions render as "remove this", not "replace with ``".
                    let hint = if fixit.replacement.is_empty() {
                        " help: remove this".to_string()
                    } else {
                        format!(
                            " help: replace with {}",
                            render_replacement(&fixit.replacement)
                        )
                    };
                    let _ = write!(
                        out,
                        "\n {blank_pad} | {spaces}{colored_carets}{}",
                        hint.cyan()
                    );
                } else {
                    let _ = write!(out, "\n {blank_pad} | {spaces}{colored_carets}");
                }
            }
        }
    }

    // --- Notes ---
    for note_text in &raw.notes {
        let _ = write!(out, "\n    {} {}", "=".bold(), note_text.cyan());
    }
    if let Some(note_text) = &secondary_note {
        let _ = write!(
            out,
            "\n    {} {} (defined in a previous snippet)",
            "=".bold(),
            note_text.cyan(),
        );
    }

    // --- Additional FixIt suggestions (beyond the first, which is inlined) ---
    for fixit in raw.fixits.iter().skip(1) {
        let _ = write!(
            out,
            "\n    {} {}: replace with {}",
            "=".bold(),
            "help".cyan().bold(),
            render_replacement(&fixit.replacement).green()
        );
    }

    out
}

/// Render diagnostics to an in-memory string (tests, REPL capture).
/// Returns ANSI-colored output.
pub fn render_to_string(files: &Files, file_id: FileId, diags: &[RawDiag]) -> String {
    let mut out = String::new();
    for raw in diags {
        out.push_str(&render_one_colored(files, file_id, raw));
        out.push('\n');
    }
    out
}

/// Render diagnostics to stderr with ANSI colors (when stderr is a tty).
pub fn render_to_stderr(files: &Files, file_id: FileId, diags: &[RawDiag]) {
    let use_color = std::io::IsTerminal::is_terminal(&std::io::stderr());
    let mut writer = std::io::stderr();

    for raw in diags {
        if use_color {
            let rendered = render_one_colored(files, file_id, raw);
            let _ = writer.write_all(rendered.as_bytes());
        } else {
            // Fallback: plain text rendering without ANSI codes.
            let mut line = String::new();
            let prefix = match raw.severity {
                Severity::Error => "error",
                Severity::Warning => "warning",
                Severity::Help => "help",
            };
            use std::fmt::Write;
            let _ = write!(line, "{prefix}: {}", raw.message);
            if let Some(span) = raw.span {
                if let Ok(file) = files.get(file_id) {
                    let name = file.name();
                    let source: &str = file.source().as_ref();
                    let (ln, col) = line_col_for(source, span.start as usize);
                    let _ = write!(line, "\n  --> {name}:{ln}:{}", col + 1);
                    let _ = write!(line, "\n    |");
                    // Secondary label first (source line + `-` carets).
                    if let Some(sec) = &raw.secondary {
                        if sec.span.end as usize <= source.len() {
                            let (sec_ln, _) = line_col_for(source, sec.span.start as usize);
                            let sec_lstart = line_start_for(source, sec.span.start as usize);
                            let sec_lend = line_end_for(source, sec.span.start as usize);
                            let sec_line_text = &source[sec_lstart..sec_lend];
                            let _ = write!(line, "\n {:>4} | {sec_line_text}", sec_ln);
                            let sec_col_off = sec.span.start as usize - sec_lstart;
                            let sec_len = (sec.span.end - sec.span.start) as usize;
                            let _ = write!(line, "\n {:>4} |", "");
                            let _ = write!(
                                line,
                                " {}{} {}",
                                " ".repeat(sec_col_off),
                                "-".repeat(sec_len),
                                sec.message,
                            );
                        } else {
                            let _ = write!(
                                line,
                                "\n    = {} (defined in a previous snippet)",
                                sec.message,
                            );
                        }
                    }
                    let lstart = line_start_for(source, span.start as usize);
                    let lend = line_end_for(source, span.start as usize);
                    let line_text = &source[lstart..lend];
                    let _ = write!(line, "\n {:>4} | {line_text}", ln);
                    let col_off = span.start as usize - lstart;
                    let len = ((span.end - span.start).max(1)) as usize;
                    let _ = write!(line, "\n {:>4} |", "");
                    let _ = write!(line, " {}{}", " ".repeat(col_off), "^".repeat(len));
                }
            }
            for note_text in &raw.notes {
                let _ = write!(line, "\n    = {note_text}");
            }
            let _ = writer.write_all(line.as_bytes());
        }
        let _ = writer.write_all(b"\n");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use zz_span_ast::Span;

    #[test]
    fn render_replacement_spells_backticks_by_name() {
        // #414: all-backtick insertions would render ambiguously as ```
        // when wrapped in backticks — spell them out instead.
        assert_eq!(render_replacement("`"), "a backtick");
        assert_eq!(render_replacement("```"), "3 backticks");
        // Everything else keeps the backtick-quoted rendering.
        assert_eq!(render_replacement("println"), "`println`");
        assert_eq!(render_replacement("{"), "`{`");
        assert_eq!(render_replacement(""), "``");
    }

    #[test]
    fn renders_caret_diagnostic() {
        let mut files = Files::new();
        let id = files.add("test.zz".to_string(), "x := 1 +\n".to_string());
        let diag = error_at("expected expression", Span::new(8, 9));
        let out = render_to_string(&files, id, &[diag]);
        assert!(out.contains("error"), "missing error keyword: {out}");
        assert!(
            out.contains("expected expression"),
            "missing message: {out}"
        );
        assert!(out.contains("^"), "missing caret: {out}");
    }

    #[test]
    fn renders_warning_with_fixit() {
        let mut files = Files::new();
        let id = files.add("test.zz".to_string(), "x := 1\ny := 2\n".to_string());
        let diag = warning_at("unused variable `y`", Span::new(7, 8))
            .with_note("consider prefixing with `_`")
            .with_fixit(FixIt::safe(Span::new(7, 8), "_y", "rename to"));
        let out = render_to_string(&files, id, &[diag]);
        assert!(out.contains("warning"), "missing warning keyword: {out}");
        assert!(out.contains("unused variable"), "missing message: {out}");
        // Fixit should be inlined under the caret
        assert!(
            out.contains("help: replace with `_y`"),
            "missing inline fixit: {out}"
        );
    }

    #[test]
    fn warning_severity_includes_keyword() {
        let mut files = Files::new();
        let id = files.add("test.zz".to_string(), "x\n".to_string());
        let diag = warning_at("unused variable `x`", Span::new(0, 1));
        let out = render_to_string(&files, id, &[diag]);
        assert!(out.contains("warning"), "missing warning keyword: {out}");
    }

    #[test]
    fn help_severity_includes_keyword() {
        let mut files = Files::new();
        let id = files.add("test.zz".to_string(), "x\n".to_string());
        let diag = note("did you mean `y`?");
        let out = render_to_string(&files, id, &[diag]);
        assert!(out.contains("help"), "missing help keyword: {out}");
    }

    #[test]
    fn shows_source_context_with_line_numbers() {
        let mut files = Files::new();
        let id = files.add(
            "test.zz".to_string(),
            "line 1\nline 2\nx := bad\nline 4\n".to_string(),
        );
        let diag = error_at("undefined variable", Span::new(14, 17));
        let out = render_to_string(&files, id, &[diag]);
        assert!(out.contains("3"), "missing line number: {out}");
        assert!(out.contains("bad"), "missing source text: {out}");
    }

    #[test]
    fn fixit_safe_constructor() {
        let fixit = FixIt::safe(Span::new(0, 3), "_x".to_string(), "prefix with _");
        assert_eq!(fixit.replacement, "_x");
        assert!(matches!(fixit.safety, FixSafety::Safe));
    }

    #[test]
    fn fixit_ambiguous_constructor() {
        let fixit = FixIt::ambiguous(
            Span::new(5, 10),
            "println",
            "replace function",
            vec!["println".to_string(), "println!".to_string()],
        );
        assert_eq!(fixit.replacement, "println");
        assert_eq!(fixit.alternatives.len(), 2);
        assert!(matches!(fixit.safety, FixSafety::Ambiguous));
    }
}
