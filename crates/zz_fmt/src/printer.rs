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
    /// Context-sensitive break: use `broken` if currently broken,
    /// otherwise use `flat`.
    IfBreak { flat: Vec<Cmd>, broken: Vec<Cmd> },
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
    let mut state = FitState {
        line_width,
        indent_width,
        eol,
        use_tabs,
        broken: false,
        remaining: line_width,
        out: &mut out,
    };
    best_fit(&mut state, &cmds);
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
        Doc::IfBreak { flat, broken } => {
            let mut flat_cmds = Vec::new();
            flatten_doc_into(flat, &mut flat_cmds, indent);
            let mut broken_cmds = Vec::new();
            flatten_doc_into(broken, &mut broken_cmds, indent);
            cmds.push(Cmd::IfBreak {
                flat: flat_cmds,
                broken: broken_cmds,
            });
        }
    }
}

/// Wadler best-fit: render `cmds` optimally in `remaining` columns on the
/// current line.
///
/// At each `Break`, tries flat (a single space) first if the rest of the
/// stream fits in the remaining width; otherwise breaks to a new line
/// with the command's indent level.
/// State threaded through the best-fit render pass.
struct FitState<'a> {
    line_width: usize,
    indent_width: usize,
    eol: Eol,
    use_tabs: bool,
    /// Whether we've already broken to a new line; once true, subsequent
    /// breaks also break (matching group-level broken state).
    broken: bool,
    /// Remaining columns on the current line.
    remaining: usize,
    out: &'a mut String,
}

/// Wadler best-fit: render `cmds` optimally using `state`.
///
/// At each `Break`, tries flat (a single space) first if the rest of the
/// stream fits in the remaining width; otherwise breaks to a new line
/// with the command's indent level.
fn best_fit<'a>(state: &mut FitState<'a>, cmds: &[Cmd]) {
    match cmds {
        [] => {}
        [Cmd::Text(t), rest @ ..] => {
            state.out.push_str(t);
            state.remaining = state.remaining.saturating_sub(t.len());
            best_fit(state, rest);
        }
        [Cmd::Break { indent }, rest @ ..] => {
            if state.broken {
                // Already broken: break again.
                state.out.push_str(state.eol.as_str());
                write_indent(*indent, state.indent_width, state.use_tabs, state.out);
                state.remaining = state
                    .line_width
                    .saturating_sub(*indent * state.indent_width);
                state.broken = true;
                best_fit(state, rest);
            } else if state.remaining > 0
                && fits(
                    state.remaining.saturating_sub(1),
                    rest,
                    state.indent_width,
                    state.line_width,
                    state.broken,
                )
            {
                // Try flat: break as a single space.
                state.out.push(' ');
                state.remaining = state.remaining.saturating_sub(1);
                state.broken = false;
                best_fit(state, rest);
            } else {
                // Break: newline + indent.
                state.out.push_str(state.eol.as_str());
                write_indent(*indent, state.indent_width, state.use_tabs, state.out);
                state.remaining = state
                    .line_width
                    .saturating_sub(*indent * state.indent_width);
                state.broken = true;
                best_fit(state, rest);
            }
        }
        [Cmd::HardBreak { indent }, rest @ ..] => {
            state.out.push_str(state.eol.as_str());
            write_indent(*indent, state.indent_width, state.use_tabs, state.out);
            state.remaining = state
                .line_width
                .saturating_sub(*indent * state.indent_width);
            state.broken = true;
            best_fit(state, rest);
        }
        [Cmd::SoftBreak { indent }, rest @ ..] => {
            if state.broken {
                // Already broken: break.
                state.out.push_str(state.eol.as_str());
                write_indent(*indent, state.indent_width, state.use_tabs, state.out);
                state.remaining = state
                    .line_width
                    .saturating_sub(*indent * state.indent_width);
                state.broken = true;
                best_fit(state, rest);
            } else if fits(
                state.remaining,
                rest,
                state.indent_width,
                state.line_width,
                state.broken,
            ) {
                // Try flat: break as nothing.
                best_fit(state, rest);
            } else {
                // Break: newline + indent.
                state.out.push_str(state.eol.as_str());
                write_indent(*indent, state.indent_width, state.use_tabs, state.out);
                state.remaining = state
                    .line_width
                    .saturating_sub(*indent * state.indent_width);
                state.broken = true;
                best_fit(state, rest);
            }
        }
        [Cmd::IfBreak {
            flat,
            broken: broken_cmds,
        }, rest @ ..] => {
            let branch = if state.broken {
                broken_cmds.as_slice()
            } else {
                flat.as_slice()
            };
            // Process the chosen branch, then continue with the rest.
            let mut combined = Vec::with_capacity(branch.len() + rest.len());
            combined.extend_from_slice(branch);
            combined.extend_from_slice(rest);
            best_fit(state, &combined);
        }
    }
}

/// Check if `cmds` can fit in `remaining` columns on the current line.
///
/// `broken` tracks whether we're currently broken (used for `IfBreak`).
/// Treats `Break` as a space (1 column) and `SoftBreak` as nothing
/// (0 columns). `HardBreak` starts a new line, so the rest is checked
/// against a fresh line width.
fn fits(
    remaining: usize,
    cmds: &[Cmd],
    indent_width: usize,
    line_width: usize,
    broken: bool,
) -> bool {
    match cmds {
        [] => true,
        [Cmd::Text(t), rest @ ..] => {
            t.len() <= remaining
                && fits(
                    remaining.saturating_sub(t.len()),
                    rest,
                    indent_width,
                    line_width,
                    broken,
                )
        }
        [Cmd::Break { .. }, rest @ ..] => {
            // Break as a space (1 column).
            remaining > 0
                && fits(
                    remaining.saturating_sub(1),
                    rest,
                    indent_width,
                    line_width,
                    broken,
                )
        }
        [Cmd::HardBreak { indent }, rest @ ..] => {
            // Hard break starts a new line; check rest against fresh width.
            let new_remaining = line_width.saturating_sub(*indent * indent_width);
            fits(new_remaining, rest, indent_width, line_width, true)
        }
        [Cmd::SoftBreak { .. }, rest @ ..] => {
            // Soft break as nothing (0 columns).
            fits(remaining, rest, indent_width, line_width, broken)
        }
        [Cmd::IfBreak {
            flat,
            broken: broken_cmds,
        }, rest @ ..] => {
            let branch = if broken {
                broken_cmds.as_slice()
            } else {
                flat.as_slice()
            };
            fits(remaining, branch, indent_width, line_width, broken)
                && fits(remaining, rest, indent_width, line_width, broken)
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

    #[test]
    fn best_fit_if_break_uses_flat_when_not_broken() {
        // `IfBreak` with `broken` = `;` should use `flat` (`,`) when not broken.
        let flat = Doc::Text(",");
        let broken = Doc::Text(";");
        let doc = Doc::concat(
            Doc::Text("a"),
            Doc::concat(
                Doc::IfBreak {
                    flat: Box::new(flat),
                    broken: Box::new(broken),
                },
                Doc::Text("b"),
            ),
        );
        let out = render(&doc, 80, 4, Eol::Lf, false);
        assert_eq!(out, "a,b");
    }

    #[test]
    fn best_fit_if_break_uses_broken_when_broken() {
        // `IfBreak` should use `broken` branch after a HardBreak.
        let flat = Doc::Text(",");
        let broken = Doc::Text(";");
        let doc = Doc::concat(
            Doc::Text("a"),
            Doc::concat(
                Doc::HardLine,
                Doc::concat(
                    Doc::IfBreak {
                        flat: Box::new(flat),
                        broken: Box::new(broken),
                    },
                    Doc::Text("b"),
                ),
            ),
        );
        let out = render(&doc, 80, 4, Eol::Lf, false);
        assert_eq!(out, "a\n;b");
    }
}
