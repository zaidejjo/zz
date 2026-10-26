//! `zz dis --vm`: print the **VM bytecode** (loader-path `Chunk`), not the
//! portable IR that plain `zz dis` prints.
//!
//! Why it exists: fusion/specialization claims ("the hot loop runs
//! `SlotAddIntImm`, not `LoadSlot` + `BinOp` + `StoreSlot`") are only
//! checkable by looking at what the VM is actually handed. Portable IR is
//! a different representation and cannot answer them — which is exactly
//! how a wrong conclusion got into the Phase 2 plan once already.
//!
//! Two modes:
//! - ops (default): every op with its opcode index and resolved `line:col`
//! - summary (`--stats`): per-class mix plus the top opcodes of each chunk

use std::collections::BTreeMap;
use std::fmt::Write as _;

use zz_runtime::vm::{Chunk, Op};

/// Byte-offset → 1-based line/column resolver for one source file.
pub struct LineIndex<'a> {
    source: &'a str,
    starts: Vec<usize>,
}

impl<'a> LineIndex<'a> {
    pub fn new(source: &'a str) -> Self {
        let mut starts = vec![0];
        starts.extend(
            source
                .bytes()
                .enumerate()
                .filter(|(_, b)| *b == b'\n')
                .map(|(i, _)| i + 1),
        );
        Self { source, starts }
    }

    /// `(line, column)`, both 1-based. Columns count characters, so a
    /// multi-byte identifier does not shift the reported position. Offsets
    /// inside a multi-byte character (possible for spans raised from IR)
    /// snap down to the character start instead of panicking.
    pub fn line_col(&self, offset: usize) -> (usize, usize) {
        let mut offset = offset.min(self.source.len());
        while !self.source.is_char_boundary(offset) {
            offset -= 1;
        }
        let line = match self.starts.binary_search(&offset) {
            Ok(i) => i,
            Err(0) => 0,
            Err(i) => i - 1,
        };
        let column = self.source[self.starts[line]..offset].chars().count() + 1;
        (line + 1, column)
    }
}

/// Render one compiled program: the module chunk followed by every nested
/// function chunk, depth-first.
///
/// `source` is the file the chunk was compiled from; an empty source means
/// the map is unavailable (`.zzc` raised from portable IR), so op sites are
/// omitted rather than clamped to `1:1`.
pub fn render_module(label: &str, source: &str, chunk: &Chunk, show_ops: bool) -> String {
    let index = LineIndex::new(source);
    let sites = !source.is_empty();
    let mut out = String::new();
    let _ = writeln!(out, "; module {label}");
    render_chunk(&mut out, &index, "<module>", chunk, show_ops, sites, 0);
    out
}

#[allow(clippy::too_many_arguments)]
fn render_chunk(
    out: &mut String,
    index: &LineIndex<'_>,
    label: &str,
    chunk: &Chunk,
    show_ops: bool,
    sites: bool,
    depth: usize,
) {
    let pad = "  ".repeat(depth);
    let total = chunk.code.len();
    let fast = chunk.code.iter().filter(|op| op.class().is_fast()).count();
    let pct = if total == 0 {
        0.0
    } else {
        fast as f64 * 100.0 / total as f64
    };
    let _ = writeln!(
        out,
        "{pad}; fn {label} — {total} ops, fast {fast}/{total} ({pct:.1}%)"
    );

    // Slot types explain why fusion fired (or did not): `try_slot_binop`
    // needs the checker to prove both operands are `Int`.
    let typed: Vec<String> = chunk
        .slot_types
        .iter()
        .enumerate()
        .filter_map(|(slot, ty)| ty.as_ref().map(|ty| format!("{slot}:{ty:?}")))
        .collect();
    if !typed.is_empty() {
        let _ = writeln!(out, "{pad};   slots: {}", typed.join(" "));
    }

    if show_ops {
        for (i, op) in chunk.code.iter().enumerate() {
            let mut site = String::new();
            if let Some(span) = chunk.spans.get(i) {
                if sites && span.start < span.end {
                    let (line, col) = index.line_col(span.start as usize);
                    let _ = write!(site, "{line}:{col}");
                }
            }
            // `MakeFunc` embeds a whole `Chunk`; its Debug would dump the
            // nested body inline. The body is printed on its own below, so
            // show a summary here instead.
            let text = match op {
                Op::MakeFunc {
                    name,
                    params,
                    chunk: inner,
                    ..
                } => format!(
                    "MakeFunc {{ name: {name:?}, params: {}, chunk: {} ops }}",
                    params.len(),
                    inner.code.len()
                ),
                _ => format!("{op:?}"),
            };
            let _ = writeln!(out, "{pad}{i:5}  {text:<48}{site}");
        }
    } else {
        let mut classes: BTreeMap<&'static str, usize> = BTreeMap::new();
        let mut ops: BTreeMap<&'static str, usize> = BTreeMap::new();
        for op in &chunk.code {
            *classes.entry(op.class().name()).or_default() += 1;
            *ops.entry(op.mnemonic()).or_default() += 1;
        }
        for (name, n) in &classes {
            let share = if total == 0 {
                0.0
            } else {
                *n as f64 * 100.0 / total as f64
            };
            let _ = writeln!(out, "{pad};   {name:<8} {n:>6}  {share:>5.1}%");
        }
        let mut ranked: Vec<(&'static str, usize)> = ops.into_iter().collect();
        ranked.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(b.0)));
        let top: Vec<String> = ranked
            .iter()
            .take(8)
            .map(|(name, n)| format!("{name}({n})"))
            .collect();
        let _ = writeln!(out, "{pad};   top: {}", top.join(" "));
    }

    // Nested function bodies are compiled inline as `MakeFunc` payloads;
    // walking them here keeps the output self-contained.
    for op in &chunk.code {
        if let Op::MakeFunc {
            name, chunk: inner, ..
        } = op
        {
            render_chunk(out, index, name, inner, show_ops, sites, depth + 1);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn line_index_counts_lines_and_chars() {
        let src = "abc\ndef\n\nxé";
        let idx = LineIndex::new(src);
        assert_eq!(idx.line_col(0), (1, 1));
        assert_eq!(idx.line_col(2), (1, 3));
        assert_eq!(idx.line_col(4), (2, 1));
        assert_eq!(idx.line_col(8), (3, 1));
        assert_eq!(idx.line_col(9), (4, 1));
        // 'é' is two bytes: columns count characters, not bytes, and an
        // offset inside it snaps down to the character start.
        assert_eq!(idx.line_col(10), (4, 2));
        assert_eq!(idx.line_col(11), (4, 2));
        assert_eq!(idx.line_col(12), (4, 3));
        // Out-of-range offsets clamp instead of panicking.
        assert_eq!(idx.line_col(999), (4, 3));
    }

    #[test]
    fn render_module_prints_ops_and_spans() {
        let mut chunk = Chunk::new();
        chunk.code.push(Op::PushConst(0));
        chunk.code.push(Op::Return);
        chunk.spans.push(zz_frontend::span::Span::new(0, 1));
        chunk.spans.push(zz_frontend::span::Span::default());
        let out = render_module("m.zz", "x", &chunk, true);
        assert!(out.contains("; module m.zz"), "{out}");
        assert!(out.contains("fn <module> — 2 ops"), "{out}");
        assert!(out.contains("PushConst(0)"), "{out}");
        assert!(out.contains("1:1"), "{out}");
    }

    #[test]
    fn empty_source_omits_sites() {
        let mut chunk = Chunk::new();
        chunk.code.push(Op::PushConst(0));
        chunk.spans.push(zz_frontend::span::Span::new(3, 9));
        let out = render_module("m.zzc", "", &chunk, true);
        assert!(out.contains("PushConst(0)"), "{out}");
        assert!(!out.contains("1:1"), "clamped site leaked:\n{out}");
    }

    #[test]
    fn render_shows_slot_types() {
        let mut chunk = Chunk::new();
        chunk.code.push(Op::Pop);
        chunk.slot_types.push(Some(zz_checker::Type::Int));
        chunk.slot_types.push(None);
        let out = render_module("m.zz", "x", &chunk, false);
        assert!(out.contains("slots: 0:Int"), "{out}");
        assert!(!out.contains("1:"), "untyped slot leaked:\n{out}");
    }

    #[test]
    fn render_summary_lists_classes() {
        let mut chunk = Chunk::new();
        chunk.code.push(Op::Pop);
        chunk.code.push(Op::SlotInc { slot: 0 });
        let out = render_module("m.zz", "", &chunk, false);
        assert!(out.contains("stack"), "{out}");
        assert!(out.contains("fused"), "{out}");
        assert!(out.contains("fast 1/2 (50.0%)"), "{out}");
        assert!(!out.contains("SlotInc {"), "summary mode must not dump ops");
    }
}
