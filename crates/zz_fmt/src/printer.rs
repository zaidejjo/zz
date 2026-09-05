//! Wadler-style best-fit pretty-printer.
//!
//! Renders a `Doc` into a String, wrapping groups onto multiple lines
//! when they don't fit the configured line width. Best-fit (a.k.a.
//! "max-break") keeps a stack of `Cmd` (break-or-flat) decisions and
//! picks the layout with the lowest cost, matching the algorithm used
//! by `prettyplease` and `dprint`.
//!
//! For M0/M1 we use a simpler "first-fit" variant: try flat; if it
//! overflows the line width, break. This is fast, deterministic, and
//! correct. A best-fit upgrade can land in a later milestone without
//! changing the Doc IR or the IR lowering.

use crate::doc::Doc;

/// What to emit at end-of-line.
#[derive(Debug, Clone, Copy)]
pub enum Eol {
    Lf,
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

/// Render `doc` into a `String` using the given `line_width`, `indent_width`
/// (spaces per `Doc::Indent` level), and EOL style.
pub fn render<'src>(doc: &Doc<'src>, line_width: usize, indent_width: usize, eol: Eol) -> String {
    let mut out = String::new();
    let mut state = State {
        out: &mut out,
        indent: 0,
        line_width,
        indent_width,
        eol,
        // Stack of "are we currently broken?" for IfBreak decisions.
        // Empty = flat; non-empty = broken at the deepest level.
        break_stack: Vec::new(),
    };
    // Measure the flat width to decide whether to break the outermost group.
    let flat_width = measure_flat(doc, 0);
    let broken = flat_width > line_width;
    state.break_stack.push(broken);
    render_doc(&mut state, doc, broken);
    state.out.clone()
}

/// Internal renderer state.
struct State<'o> {
    out: &'o mut String,
    indent: usize,
    line_width: usize,
    indent_width: usize,
    eol: Eol,
    break_stack: Vec<bool>,
}

impl<'o> State<'o> {
    fn current_broken(&self) -> bool {
        self.break_stack.last().copied().unwrap_or(false)
    }

    fn write_indent(&mut self) {
        // Each `Doc::Indent` level contributes `indent_width` spaces.
        for _ in 0..self.indent * self.indent_width {
            self.out.push(' ');
        }
    }

    fn newline(&mut self) {
        self.out.push_str(self.eol.as_str());
    }
}

fn render_doc<'src>(s: &mut State<'_>, doc: &Doc<'src>, broken: bool) {
    match doc {
        Doc::Nil => {}
        Doc::Text(t) => s.out.push_str(t),
        Doc::TextOwned(t) => s.out.push_str(t.as_ref()),
        Doc::Line => {
            if broken {
                s.newline();
                s.write_indent();
            } else {
                s.out.push(' ');
            }
        }
        Doc::HardLine => {
            s.newline();
            s.write_indent();
        }
        Doc::SoftLine => {
            if broken {
                s.newline();
                s.write_indent();
            }
            // else: emit nothing.
        }
        Doc::Concat(parts) => {
            for p in parts {
                render_doc(s, p, broken);
            }
        }
        Doc::Group { contents } => {
            let flat_width = measure_flat(contents, s.indent);
            let remaining = s.line_width.saturating_sub(current_col(s));
            let group_broken = flat_width > remaining || broken;
            s.break_stack.push(group_broken);
            render_doc(s, contents, group_broken);
            s.break_stack.pop();
        }
        Doc::Indent { contents } => {
            s.indent += 1;
            render_doc(s, contents, broken);
            s.indent -= 1;
        }
        Doc::IfBreak { flat, broken: b } => {
            if s.current_broken() {
                render_doc(s, b, broken);
            } else {
                render_doc(s, flat, broken);
            }
        }
    }
}

/// Measure the width of `doc` rendered flat (all `Line`/`SoftLine` collapsed
/// to a single space, `HardLine` collapsed to a space, `IfBreak` picks flat).
fn measure_flat<'src>(doc: &Doc<'src>, indent: usize) -> usize {
    fn go<'src>(d: &Doc<'src>, indent: &mut usize) -> usize {
        match d {
            Doc::Nil => 0,
            Doc::Text(t) => t.chars().count(),
            Doc::TextOwned(t) => t.chars().count(),
            Doc::Line | Doc::SoftLine | Doc::HardLine => 1,
            Doc::Concat(parts) => parts.iter().map(|p| go(p, indent)).sum(),
            Doc::Group { contents } => go(contents, indent),
            Doc::Indent { contents } => {
                *indent += 1;
                let w = go(contents, indent);
                *indent -= 1;
                w
            }
            Doc::IfBreak { flat, .. } => go(flat, indent),
        }
    }
    let mut indent = indent;
    go(doc, &mut indent)
}

/// Best-effort current column (0-based byte count since last newline).
/// This is an approximation: we count bytes, not display columns, which
/// is fine for ASCII-heavy ZZ code. Non-ASCII strings still get a
/// reasonable (slightly conservative) width estimate.
fn current_col(s: &State<'_>) -> usize {
    // Walk back to the last newline in the output buffer.
    let bytes = s.out.as_bytes();
    let mut col = 0usize;
    for i in (0..bytes.len()).rev() {
        if bytes[i] == b'\n' {
            return col;
        }
        col += 1;
    }
    col
}
