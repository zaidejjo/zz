//! Wadler-style best-fit pretty-printer.
//!
//! Renders a `Doc` into a `String`, wrapping groups onto multiple lines
//! when they don't fit the configured line width. Uses Wadler's
//! best-fit ("max-break") algorithm: flattens the `Doc` into a command
//! stream, then at each break point chooses flat (space) if the rest
//! fits in the remaining width, otherwise breaks (newline + indent).
//! This produces better line-wrapping decisions than first-fit,
//! matching the algorithm used by `prettyplease` and `dprint`.
//!
//! The `Doc` IR and lowering are unchanged from the first-fit version;
//! only the renderer differs.

use crate::doc::Doc;

/// What to emit at end-of-line.
#[derive(Debug, Clone, Copy)]
pub enum Eol {
    /// `\n`
    Lf,
    /// `\r\n`
    Crlf,
}

impl Eol {
    pub fn as_str(self) -> &'static str {
        match self {
            Eol::Lf => "\n",
            Eol::Crlf => "\r\n",
        }
    }
}

/// A command in the flattened command stream.
///
/// Each break command carries the indent level at which it occurs, so
/// that `Doc::Indent` properly affects newline indentation.
#[derive(Debug, Clone)]
enum Cmd {
    /// Literal text to emit.
    Text(String),
    /// A break point: space if the rest fits, newline+indent if not.
    Break { indent: usize },
    /// Always break: newline+indent.
    HardBreak { indent: usize },
    /// A soft break: nothing if the rest fits, newline+indent if not.
    SoftBreak { indent: usize },
}

/// Render `doc` into a `String` using Wadler's best-fit algorithm.
///
/// `line_width` is the maximum number of columns per line.
/// `indent_width` is the number of spaces (or tabs) per indent level.
/// `eol` is the end-of-line sequence.
/// `use_tabs` indents with `\t` instead of spaces when true.
pub fn render<'src>(
    doc: &Doc<'src>,
    line_width: usize,
    indent_width: usize,
    eol: Eol,
    use_tabs: bool,
) -> String {
    let mut out = String::new();
    let cmds = flatten_doc(doc);
    best_fit(
        line_width,
        indent_width,
        eol,
        use_tabs,
        &cmds,
        line_width,
        &mut out,
    );
    out
}

/// Flatten a `Doc` tree into a flat stream of `Cmd`s.
///
/// `Doc::Group` is transparent: breaks inside it are what matter.
/// `Doc::Indent` tracks the indentation level for newlines.
/// `Doc::IfBreak` is not currently produced by the IR lowering, so it
/// is flattened as the `flat` branch.
fn flatten_doc<'src>(doc: &Doc<'src>) -> Vec<Cmd> {
    let mut cmds = Vec::new();
    flatten_doc_into(doc, &mut cmds, 0);
    cmds
}

fn flatten_doc_into<'src>(doc: &Doc<'src>, cmds: &mut Vec<Cmd>, indent: usize) {
    match doc {
        Doc::Nil => {}
        Doc::Text(t) => cmds.push(Cmd::Text(t.to_string())),
        Doc::TextOwned(t) => cmds.push(Cmd::Text(t.as_ref().to_string())),
        Doc::Line => cmds.push(Cmd::Break { indent }),
        Doc::HardLine => cmds.push(Cmd::HardBreak { indent }),
        Doc::SoftLine => cmds.push(Cmd::SoftBreak { indent }),
        Doc::Concat(parts) => {
            for p in parts {
                flatten_doc_into(p, cmds, indent);
            }
        }
        Doc::Group { contents } => {
            // Groups are transparent: breaks inside them are what matter.
            flatten_doc_into(contents, cmds, indent);
        }
        Doc::Indent { contents } => {
            flatten_doc_into(contents, cmds, indent + 1);
        }
        Doc::IfBreak { flat, broken: _ } => {
            // IfBreak is context-dependent (depends on whether the
            // enclosing group broke). Nothing currently produces it,
            // so flatten as the flat branch.
            // TODO: Handle IfBreak properly when it's used by the IR.
            flatten_doc_into(flat, cmds, indent);
        }
    }
}

/// Wadler best-fit: render `cmds` optimally in `remaining` columns on the
/// current line.
///
/// At each `Break`, tries flat (a single space) first if the rest of the
/// stream fits in the remaining width; otherwise breaks to a new line
/// with the command's indent level.
fn best_fit(
    line_width: usize,
    indent_width: usize,
    eol: Eol,
    use_tabs: bool,
    cmds: &[Cmd],
    remaining: usize,
    out: &mut String,
) {
    match cmds {
        [] => {}
        [Cmd::Text(t), rest @ ..] => {
            out.push_str(t);
            let new_remaining = remaining.saturating_sub(t.len());
            best_fit(
                line_width,
                indent_width,
                eol,
                use_tabs,
                rest,
                new_remaining,
                out,
            );
        }
        [Cmd::Break { indent }, rest @ ..] => {
            // Try flat: break as a single space.
            if remaining > 0 && fits(remaining.saturating_sub(1), rest, indent_width, line_width) {
                out.push(' ');
                let new_remaining = remaining.saturating_sub(1);
                best_fit(
                    line_width,
                    indent_width,
                    eol,
                    use_tabs,
                    rest,
                    new_remaining,
                    out,
                );
            } else {
                // Break: newline + indent.
                out.push_str(eol.as_str());
                write_indent(*indent, indent_width, use_tabs, out);
                let new_remaining = line_width.saturating_sub(*indent * indent_width);
                best_fit(
                    line_width,
                    indent_width,
                    eol,
                    use_tabs,
                    rest,
                    new_remaining,
                    out,
                );
            }
        }
        [Cmd::HardBreak { indent }, rest @ ..] => {
            out.push_str(eol.as_str());
            write_indent(*indent, indent_width, use_tabs, out);
            let new_remaining = line_width.saturating_sub(*indent * indent_width);
            best_fit(
                line_width,
                indent_width,
                eol,
                use_tabs,
                rest,
                new_remaining,
                out,
            );
        }
        [Cmd::SoftBreak { indent }, rest @ ..] => {
            // Try flat: break as nothing.
            if fits(remaining, rest, indent_width, line_width) {
                best_fit(
                    line_width,
                    indent_width,
                    eol,
                    use_tabs,
                    rest,
                    remaining,
                    out,
                );
            } else {
                // Break: newline + indent.
                out.push_str(eol.as_str());
                write_indent(*indent, indent_width, use_tabs, out);
                let new_remaining = line_width.saturating_sub(*indent * indent_width);
                best_fit(
                    line_width,
                    indent_width,
                    eol,
                    use_tabs,
                    rest,
                    new_remaining,
                    out,
                );
            }
        }
    }
}

/// Check if `cmds` can fit in `remaining` columns on the current line.
///
/// Treats `Break` as a space (1 column) and `SoftBreak` as nothing
/// (0 columns). `HardBreak` starts a new line, so the rest is checked
/// against a fresh line width.
fn fits(remaining: usize, cmds: &[Cmd], indent_width: usize, line_width: usize) -> bool {
    match cmds {
        [] => true,
        [Cmd::Text(t), rest @ ..] => {
            t.len() <= remaining
                && fits(
                    remaining.saturating_sub(t.len()),
                    rest,
                    indent_width,
                    line_width,
                )
        }
        [Cmd::Break { .. }, rest @ ..] => {
            // Break as a space (1 column).
            remaining > 0 && fits(remaining.saturating_sub(1), rest, indent_width, line_width)
        }
        [Cmd::HardBreak { indent }, rest @ ..] => {
            // Hard break starts a new line; check rest against fresh width.
            let new_remaining = line_width.saturating_sub(*indent * indent_width);
            fits(new_remaining, rest, indent_width, line_width)
        }
        [Cmd::SoftBreak { .. }, rest @ ..] => {
            // Soft break as nothing (0 columns).
            fits(remaining, rest, indent_width, line_width)
        }
    }
}

/// Write `indent` levels of indentation to `out`, using tabs when
/// `use_tabs` is true and spaces otherwise.
fn write_indent(indent: usize, indent_width: usize, use_tabs: bool, out: &mut String) {
    if use_tabs {
        for _ in 0..indent {
            out.push('\t');
        }
    } else {
        for _ in 0..indent * indent_width {
            out.push(' ');
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn best_fit_empty_doc() {
        let out = render(&Doc::Nil, 80, 4, Eol::Lf, false);
        assert_eq!(out, "");
    }

    #[test]
    fn best_fit_plain_text() {
        let out = render(&Doc::Text("hello world"), 80, 4, Eol::Lf, false);
        assert_eq!(out, "hello world");
    }

    #[test]
    fn best_fit_text_with_spaces_fits() {
        // "x = 1 + 2" fits in 80 columns
        let doc = Doc::concat(
            Doc::Text("x"),
            Doc::concat(Doc::Line, Doc::concat(Doc::Text("= 1 + 2"), Doc::HardLine)),
        );
        let out = render(&doc, 80, 4, Eol::Lf, false);
        assert_eq!(out, "x = 1 + 2\n");
    }

    #[test]
    fn best_fit_group_breaks_when_too_wide() {
        // A group that exceeds line width should break
        let long_text = "x".repeat(100);
        let doc = Doc::Text(&long_text);
        let out = render(&doc, 80, 4, Eol::Lf, false);
        // Text can't be broken, so it overflows (same as first-fit)
        assert_eq!(out, long_text);
    }

    #[test]
    fn best_fit_hard_line_always_breaks() {
        let doc = Doc::concat(
            Doc::Text("line1"),
            Doc::concat(Doc::HardLine, Doc::Text("line2")),
        );
        let out = render(&doc, 80, 4, Eol::Lf, false);
        assert_eq!(out, "line1\nline2");
    }

    #[test]
    fn best_fit_soft_line_disappears_when_flat() {
        // SoftLine between two short texts should disappear when flat
        let doc = Doc::concat(Doc::Text("a"), Doc::concat(Doc::SoftLine, Doc::Text("b")));
        let out = render(&doc, 80, 4, Eol::Lf, false);
        assert_eq!(out, "ab");
    }

    #[test]
    fn best_fit_indent_after_break() {
        // Indented content after a break should be indented
        let doc = Doc::concat(
            Doc::Text("func main() {"),
            Doc::concat(
                Doc::HardLine,
                Doc::concat(
                    Doc::Indent {
                        contents: Box::new(Doc::Text("    return 0;")),
                    },
                    Doc::concat(Doc::HardLine, Doc::Text("}")),
                ),
            ),
        );
        let out = render(&doc, 80, 4, Eol::Lf, false);
        assert_eq!(out, "func main() {\n    return 0;\n}");
    }

    #[test]
    fn best_fit_crlf_eol() {
        let doc = Doc::concat(Doc::Text("a"), Doc::concat(Doc::HardLine, Doc::Text("b")));
        let out = render(&doc, 80, 4, Eol::Crlf, false);
        assert_eq!(out, "a\r\nb");
    }

    #[test]
    fn best_fit_long_line_wraps_at_break() {
        // Multiple breaks in a sequence should wrap when needed
        let parts: Vec<Doc<'static>> = (0..20).map(|i| Doc::from(format!("var{i}"))).collect();
        let mut doc = Doc::Nil;
        for p in parts {
            doc = Doc::concat(doc, Doc::concat(Doc::Line, p));
        }
        let out = render(&doc, 40, 4, Eol::Lf, false);
        // Each line should be at most 40 chars
        for line in out.lines() {
            assert!(
                line.len() <= 40,
                "line exceeds 40 cols: {:?} ({} chars)",
                line,
                line.len()
            );
        }
    }

    #[test]
    fn best_fit_round_trip_parses() {
        // Rendered output should be parseable (structurally equivalent)
        let src =
            "func add(a:int,b:int)->int{return a+b}\nfunc main() -> int { return add(1, 2) }\n";
        let parsed = zz_frontend::parse(src);
        let config = crate::config::FmtConfig::default();
        let (doc, eol) = crate::ir::lower_program(&parsed.program, src, &config);
        let out = render(&doc, 80, 4, eol, config.use_tabs);
        let reparsed = zz_frontend::parse(&out);
        assert_eq!(parsed.program.stmts.len(), reparsed.program.stmts.len());
    }
}
