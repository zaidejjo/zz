//! Line coverage for the bytecode VM (`zz cover`).
//!
//! Design (see `docs/coverage.md`):
//! - Every compiled [`Chunk`](crate::vm::Chunk) carries an optional file
//!   label (`source`) plus a dense span id per opcode (`cov_ids`,
//!   `u32::MAX` = unmapped). Both are stamped post-compile by
//!   [`Interp::register_coverage_chunk`], so the compiler needs no
//!   file knowledge and cached (`.zzc`) chunks stay unattributed.
//! - The VM run loop records one counter bump per executed op into
//!   [`CoverageState::hits`] — two array reads, no hashing on the hot
//!   path. Disabled runs pay a single predictable branch.
//! - Coverable lines derive from the same emitted spans the hits use, so
//!   numerator and denominator are consistent by construction (no AST
//!   walking, no 100%-by-definition bug).

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use crate::vm::op::Op;
use crate::vm::Chunk;
use zz_frontend::span::Span;

/// Sentinel for opcodes with no coverage id (default spans, stdlib and
/// `.zzc` chunks compiled without coverage).
pub const COV_UNMAPPED: u32 = u32::MAX;

/// Byte offset → 0-based line number (counts `\n` before `offset`,
/// saturating at end of source).
pub fn line_of(source: &str, offset: u32) -> u32 {
    let offset = (offset as usize).min(source.len());
    source.as_bytes()[..offset]
        .iter()
        .filter(|&&b| b == b'\n')
        .count() as u32
}

/// Per-file coverage data.
#[derive(Debug, Clone, Default)]
pub struct CoverFile {
    /// Display label (usually the module path).
    pub label: String,
    /// Full source text (for line mapping and snippets).
    pub source: String,
    /// 0-based coverable lines (lines spanned by emitted code).
    pub coverable: HashSet<u32>,
}

/// Whole-run coverage state, owned by [`Interp`](crate::Interp).
#[derive(Debug, Clone, Default)]
pub struct CoverageState {
    pub files: Vec<CoverFile>,
    /// (file index, span start) → dense hit id.
    pub id_of: HashMap<(usize, u32), usize>,
    /// Hit id → (file index, 0-based line).
    pub id_loc: Vec<(usize, u32)>,
    /// Hit id → execution count.
    pub hits: Vec<u64>,
    /// Conditional sites in registration order.
    pub branch_sites: Vec<BranchSite>,
    /// Parallel to `branch_sites`: (jump-taken, fallthrough) counts.
    pub branch_hits: Vec<(u64, u64)>,
    /// (file index, span start) → branch id (dedupe for idempotent fill).
    pub branch_of: HashMap<(usize, u32), usize>,
}

/// One conditional jump: `JumpIfFalse/True/FalseBool` (if/match guards), the
/// fused slot-compare branches,
/// `WhileCond` and `ForNext` (loop exits).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BranchSite {
    pub file_idx: usize,
    /// 0-based line of the jump instruction.
    pub line: u32,
    /// 0-based line jumped to on the taken path (`None` = target unknown).
    pub taken_line: Option<u32>,
}

impl CoverageState {
    /// Find or create the file slot for `label` (registers `source` on
    /// first sight). Linear scan: file counts are tiny.
    pub fn file_idx(&mut self, label: &str, source: &str) -> usize {
        if let Some(i) = self.files.iter().position(|f| f.label == label) {
            return i;
        }
        self.files.push(CoverFile {
            label: label.to_string(),
            source: source.to_string(),
            coverable: HashSet::new(),
        });
        self.files.len() - 1
    }

    /// Read-only file lookup for the VM run loop (never creates).
    pub fn file_index(&self, label: &str) -> Option<usize> {
        self.files.iter().position(|f| f.label == label)
    }

    /// Register one emitted span; returns its dense hit id.
    pub fn span_id(&mut self, file_idx: usize, source: &str, span_start: u32) -> usize {
        if let Some(&id) = self.id_of.get(&(file_idx, span_start)) {
            return id;
        }
        let id = self.hits.len();
        let line = line_of(source, span_start);
        self.id_of.insert((file_idx, span_start), id);
        self.id_loc.push((file_idx, line));
        self.hits.push(0);
        self.files[file_idx].coverable.insert(line);
        id
    }

    /// Register a conditional site; returns its branch id. Idempotent per
    /// (file, span) so re-fills never duplicate sites.
    pub fn branch_id(
        &mut self,
        file_idx: usize,
        source: &str,
        span_start: u32,
        taken_start: Option<u32>,
    ) -> usize {
        if let Some(&id) = self.branch_of.get(&(file_idx, span_start)) {
            return id;
        }
        let id = self.branch_sites.len();
        let line = line_of(source, span_start);
        let taken_line = taken_start.map(|s| line_of(source, s));
        self.branch_of.insert((file_idx, span_start), id);
        self.branch_sites.push(BranchSite {
            file_idx,
            line,
            taken_line,
        });
        self.branch_hits.push((0, 0));
        id
    }

    /// (directions hit, directions total) over all sites (2 per site).
    pub fn branch_stats(&self) -> (usize, usize) {
        let mut hit = 0;
        for (taken, fallthrough) in &self.branch_hits {
            if *taken > 0 {
                hit += 1;
            }
            if *fallthrough > 0 {
                hit += 1;
            }
        }
        (hit, self.branch_sites.len() * 2)
    }

    /// Merge another run's hits (for multi-test `zz cover --tests`):
    /// matches by (file label, line), creating slots as needed, so merge
    /// order never matters.
    pub fn merge(&mut self, other: &CoverageState) {
        // (label, line) → hits in other.
        let mut by_loc: HashMap<(&str, u32), u64> = HashMap::new();
        for (id, &(fi, line)) in other.id_loc.iter().enumerate() {
            let label = other.files[fi].label.as_str();
            *by_loc.entry((label, line)).or_insert(0) += other.hits[id];
        }
        for ((label, line), count) in by_loc {
            let fi = self.file_idx(label, "");
            // Find our id for (file, line), creating one if missing.
            let id = (0..self.id_loc.len())
                .find(|&i| self.id_loc[i] == (fi, line))
                .unwrap_or_else(|| {
                    let id = self.hits.len();
                    // Rebuild the span key approximately: line-only merge
                    // cannot recover the original span start, so key the
                    // synthetic entry by line-derived pseudo-offset.
                    self.id_of.insert((fi, line_key(line)), id);
                    self.id_loc.push((fi, line));
                    self.hits.push(0);
                    self.files[fi].coverable.insert(line);
                    id
                });
            self.hits[id] += count;
        }
        // Branch sites merge by (label, line, taken line); counts add.
        for (si, site) in other.branch_sites.iter().enumerate() {
            let label = other.files[site.file_idx].label.as_str();
            let fi = self.file_idx(label, "");
            let found = (0..self.branch_sites.len()).find(|&i| {
                let s = &self.branch_sites[i];
                s.file_idx == fi && s.line == site.line && s.taken_line == site.taken_line
            });
            let id = match found {
                Some(i) => i,
                None => {
                    let id = self.branch_sites.len();
                    self.branch_sites.push(BranchSite {
                        file_idx: fi,
                        line: site.line,
                        taken_line: site.taken_line,
                    });
                    self.branch_hits.push((0, 0));
                    id
                }
            };
            self.branch_hits[id].0 += other.branch_hits[si].0;
            self.branch_hits[id].1 += other.branch_hits[si].1;
        }
    }

    /// (covered lines, coverable lines) for one file.
    pub fn file_stats(&self, file_idx: usize) -> (usize, usize) {
        let coverable = self.files[file_idx].coverable.len();
        let mut covered = HashSet::new();
        for (id, &(fi, line)) in self.id_loc.iter().enumerate() {
            if fi == file_idx && self.hits[id] > 0 {
                covered.insert(line);
            }
        }
        (covered.len(), coverable)
    }

    /// Lines with hits, sorted (0-based).
    pub fn covered_lines(&self, file_idx: usize) -> Vec<u32> {
        let mut lines: Vec<u32> = self
            .id_loc
            .iter()
            .enumerate()
            .filter(|(id, (fi, _))| *fi == file_idx && self.hits[*id] > 0)
            .map(|(_, (_, line))| *line)
            .collect();
        lines.sort_unstable();
        lines.dedup();
        lines
    }

    /// Coverable but never-hit lines, sorted (0-based).
    pub fn uncovered_lines(&self, file_idx: usize) -> Vec<u32> {
        let covered: HashSet<u32> = self
            .id_loc
            .iter()
            .enumerate()
            .filter(|(id, (fi, _))| *fi == file_idx && self.hits[*id] > 0)
            .map(|(_, (_, line))| *line)
            .collect();
        let mut lines: Vec<u32> = self.files[file_idx]
            .coverable
            .difference(&covered)
            .copied()
            .collect();
        lines.sort_unstable();
        lines
    }
}

/// Pseudo span-start key for merge-created entries (line-only data has no
/// original offset; the high bit marks it synthetic so it can never
/// collide with a real byte offset on lines < 2M).
fn line_key(line: u32) -> u32 {
    0x8000_0000 | line.min(0x007F_FFFF)
}

/// Stamp a compiled chunk tree with its file label: the top chunk plus
/// every nested function-body/default chunk reachable through
/// MakeFunc/MakeClosure/SpawnClosure ops. Shared subtrees (refcount > 1,
/// e.g. `.zzc`-raised chunks) are cloned, never mutated in place.
pub fn stamp_chunk_tree(chunk: &mut Chunk, label: &Arc<str>) {
    chunk.source = Some(label.clone());
    for op in &mut chunk.code {
        match op {
            Op::MakeFunc {
                chunk: c, defaults, ..
            }
            | Op::MakeClosure {
                chunk: c, defaults, ..
            }
            | Op::SpawnClosure {
                chunk: c, defaults, ..
            } => {
                edit_shared(c, label);
                for d in defaults.iter_mut().flatten() {
                    edit_shared(d, label);
                }
            }
            _ => {}
        }
    }
}

fn edit_shared(c: &mut Arc<Chunk>, label: &Arc<str>) {
    match Arc::get_mut(c) {
        Some(inner) => stamp_chunk_tree(inner, label),
        None => {
            let mut owned = (**c).clone();
            stamp_chunk_tree(&mut owned, label);
            *c = Arc::new(owned);
        }
    }
}

/// Fill `cov_ids` for a stamped chunk tree and register every emitted
/// (non-default) span as coverable. `source` is the label's full text.
/// Idempotent per chunk (re-registration finds existing ids).
pub fn fill_cov_ids(cov: &mut CoverageState, label: &str, source: &str, chunk: &mut Chunk) {
    let file_idx = cov.file_idx(label, source);
    fill_one(cov, file_idx, source, chunk);
}

/// Jump target of a conditional op (`None` for non-branch ops).
fn branch_target(op: &Op) -> Option<usize> {
    match op {
        Op::JumpIfFalse(t) | Op::JumpIfTrue(t) => Some(*t),
        Op::JumpIfFalseBool(t, _) => Some(*t),
        // Fused `if` guards: one op, still one branch site (the compare and
        // the branch it absorbed used to be registered as a pair).
        Op::JumpIfFalseSlotCmpIntImm { target, .. }
        | Op::JumpIfFalseSlotCmpIntSlot { target, .. } => Some(*target),
        Op::WhileCond { exit, .. } => Some(*exit),
        Op::ForNext { exit, .. } => Some(*exit),
        _ => None,
    }
}

fn fill_one(cov: &mut CoverageState, file_idx: usize, source: &str, chunk: &mut Chunk) {
    if chunk.cov_ids.len() != chunk.code.len() {
        chunk.cov_ids = vec![COV_UNMAPPED; chunk.code.len()];
    }
    // Branch array parallels `code` only when this fill runs; chunks
    // filled by older runs keep whatever they have (length-guarded at
    // the recording site).
    if chunk.cov_branch.len() != chunk.code.len() {
        chunk.cov_branch = vec![COV_UNMAPPED; chunk.code.len()];
    }
    for i in 0..chunk.code.len() {
        let span = chunk.spans.get(i).copied().unwrap_or_default();
        if span == Span::default() {
            continue;
        }
        if chunk.cov_ids[i] == COV_UNMAPPED {
            let id = cov.span_id(file_idx, source, span.start);
            chunk.cov_ids[i] = id as u32;
        }
        // Conditional jumps double as branch sites: one id, two
        // directions recorded at runtime (taken / fallthrough).
        if chunk.cov_branch[i] == COV_UNMAPPED {
            if let Some(target) = branch_target(&chunk.code[i]) {
                let taken_start = chunk
                    .spans
                    .get(target)
                    .map(|s| s.start)
                    .filter(|_| target < chunk.code.len());
                let id = cov.branch_id(file_idx, source, span.start, taken_start);
                chunk.cov_branch[i] = id as u32;
            }
        }
    }
    for op in &mut chunk.code {
        match op {
            Op::MakeFunc {
                chunk: c, defaults, ..
            }
            | Op::MakeClosure {
                chunk: c, defaults, ..
            }
            | Op::SpawnClosure {
                chunk: c, defaults, ..
            } => {
                fill_shared(cov, file_idx, source, c);
                for d in defaults.iter_mut().flatten() {
                    fill_shared(cov, file_idx, source, d);
                }
            }
            _ => {}
        }
    }
}

fn fill_shared(cov: &mut CoverageState, file_idx: usize, source: &str, c: &mut Arc<Chunk>) {
    match Arc::get_mut(c) {
        Some(inner) => fill_one(cov, file_idx, source, inner),
        None => {
            let mut owned = (**c).clone();
            fill_one(cov, file_idx, source, &mut owned);
            *c = Arc::new(owned);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn line_of_counts_newlines() {
        let src = "a\nbb\nccc";
        assert_eq!(line_of(src, 0), 0);
        assert_eq!(line_of(src, 1), 0);
        assert_eq!(line_of(src, 2), 1);
        assert_eq!(line_of(src, 5), 2);
        assert_eq!(line_of(src, 999), 2);
    }

    #[test]
    fn span_ids_dedupe_and_cover() {
        let mut cov = CoverageState::default();
        let fi = cov.file_idx("m.zz", "x := 1\ny := 2\n");
        let a = cov.span_id(fi, "x := 1\ny := 2\n", 0);
        let b = cov.span_id(fi, "x := 1\ny := 2\n", 0);
        assert_eq!(a, b);
        cov.hits[a] += 1;
        assert_eq!(cov.file_stats(fi), (1, 1));
        let c = cov.span_id(fi, "x := 1\ny := 2\n", 7);
        assert_eq!(cov.file_stats(fi), (1, 2));
        cov.hits[c] += 2;
        assert_eq!(cov.file_stats(fi), (2, 2));
        assert_eq!(cov.uncovered_lines(fi), Vec::<u32>::new());
    }

    #[test]
    fn merge_is_order_independent() {
        let mut a = CoverageState::default();
        let fa = a.file_idx("m.zz", "a\nb\n");
        let id = a.span_id(fa, "a\nb\n", 0);
        a.hits[id] = 3;
        let mut b = CoverageState::default();
        let fb = b.file_idx("m.zz", "a\nb\n");
        let id2 = b.span_id(fb, "a\nb\n", 2);
        b.hits[id2] = 5;
        a.merge(&b);
        assert_eq!(a.file_stats(fa), (2, 2));
        // Merging twice accumulates (documented: merge each run once).
        assert_eq!(a.hits.iter().sum::<u64>(), 8);
    }

    #[test]
    fn traversal_names_are_just_labels() {
        // Labels are never joined to paths inside CoverageState; the CLI
        // is responsible for safe display only. A hostile label stays inert.
        let mut cov = CoverageState::default();
        let fi = cov.file_idx("../../evil.zz", "x\n");
        assert_eq!(cov.files[fi].label, "../../evil.zz");
    }

    #[test]
    fn branch_ids_and_stats() {
        let mut cov = CoverageState::default();
        let fi = cov.file_idx("m.zz", "if a {\n b\n} else {\n c\n}\n");
        let b0 = cov.branch_id(fi, "if a {\n b\n} else {\n c\n}\n", 0, Some(12));
        // Idempotent re-registration returns the same id.
        assert_eq!(cov.branch_id(fi, "x", 0, Some(12)), b0);
        assert_eq!(cov.branch_stats(), (0, 2));
        cov.branch_hits[b0].1 += 1;
        assert_eq!(cov.branch_stats(), (1, 2));
        cov.branch_hits[b0].0 += 1;
        assert_eq!(cov.branch_stats(), (2, 2));
        let site = &cov.branch_sites[b0];
        assert_eq!((site.line, site.taken_line), (0, Some(2)));
    }

    #[test]
    fn branch_merge_adds_directions() {
        let mut a = CoverageState::default();
        let fa = a.file_idx("m.zz", "x\n");
        let id = a.branch_id(fa, "x\n", 0, None);
        a.branch_hits[id].1 = 4;
        let mut b = CoverageState::default();
        let fb = b.file_idx("m.zz", "x\n");
        let id2 = b.branch_id(fb, "x\n", 0, None);
        b.branch_hits[id2].0 = 7;
        a.merge(&b);
        assert_eq!(a.branch_stats(), (2, 2));
        assert_eq!(a.branch_hits[0], (7, 4));
    }
}
