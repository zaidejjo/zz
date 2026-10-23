//! Debugger support (spike): line breakpoints in the bytecode VM.
//!
//! A [`DebugSession`] maps source labels to line tables and holds the
//! breakpoint set. Attached to [`Vm`](crate::vm::Vm) via
//! [`Vm::attach_debug`](crate::vm::Vm::attach_debug), it pauses the run
//! loop *before* the first attributable op on a breakpoint line,
//! surfacing [`YieldReason::DebugBreak`](crate::value::YieldReason) with
//! the frame `ip` synced — so [`resume`](crate::vm::Vm::resume_chunk)
//! continues exactly where it stopped and the interpreter environment
//! can be inspected mid-flight.
//!
//! Cost when detached: one `Option` discriminant check per op (same
//! class as the coverage gate). Cost when attached: one span fetch +
//! line lookup per attributable op; unattributed ops skip on the span
//! default. Multi-chunk programs resolve per frame through the chunk's
//! source label; unattributed chunks never break.

use std::collections::{HashMap, HashSet};

/// One debuggable source file: line-start byte offsets for span→line.
#[derive(Debug, Clone)]
pub struct SourceFile {
    /// Byte offset where each 1-based line starts.
    pub line_starts: Vec<usize>,
}

impl SourceFile {
    pub fn new(source: &str) -> Self {
        let mut line_starts = vec![0];
        for (i, ch) in source.char_indices() {
            if ch == '\n' {
                line_starts.push(i + 1);
            }
        }
        Self { line_starts }
    }

    /// 1-based line containing byte `offset`.
    pub fn line_of(&self, offset: u32) -> u32 {
        let offset = offset as usize;
        match self.line_starts.binary_search(&offset) {
            Ok(i) => (i + 1) as u32,
            Err(i) => i as u32,
        }
    }
}

/// Attached debugger state: sources plus (label, line) breakpoints.
/// Why the last suspension happened (read by the adapter for the
/// `stopped` event reason).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum StopReason {
    #[default]
    Breakpoint,
    Step,
}

/// How a step suspends.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StepMode {
    /// First new line at our depth or shallower (calls run through).
    Over,
    /// First new line anywhere, descending into calls.
    Into,
    /// First opportunity shallower than our depth (i.e. after return).
    /// At top level this never fires: stepping out of `main` is just
    /// continuing.
    Out,
}

/// One-shot step rule, cleared when it fires.
#[derive(Debug, Clone)]
pub struct StepRule {
    pub mode: StepMode,
    pub depth: usize,
    pub label: String,
    pub line: u32,
}

#[derive(Debug, Clone, Default)]
pub struct DebugSession {
    files: HashMap<String, SourceFile>,
    breakpoints: HashSet<(String, u32)>,
    /// Last attributable position executed. Breakpoints fire only on
    /// line *changes*: without this, resuming past a breakpoint would
    /// re-suspend on the next op of the same line forever, while loops
    /// still re-break on genuine revisits.
    last: Option<(String, u32)>,
    /// Active step-over rule (one-shot: cleared when it fires).
    step: Option<StepRule>,
    /// Reason for the most recent suspension.
    pub stop_reason: StopReason,
}

impl DebugSession {
    pub fn new() -> Self {
        Self::default()
    }

    /// Register a source under `label` (matches the chunk source label).
    pub fn add_source(&mut self, label: impl Into<String>, source: &str) {
        self.files.insert(label.into(), SourceFile::new(source));
    }

    /// Drop every breakpoint registered for `label` (DAP replaces the
    /// whole file set on every `setBreakpoints` request).
    pub fn retain_file(&mut self, label: &str) {
        self.breakpoints.retain(|(l, _)| l != label);
    }

    /// Break before the first attributable op on `line` (1-based) of `label`.
    pub fn add_breakpoint(&mut self, label: impl Into<String>, line: u32) {
        self.breakpoints.insert((label.into(), line));
    }

    /// Line breakpoint check for one op span: the 1-based line when this
    /// op should suspend, `None` otherwise.
    /// 1-based line of `offset` in `label`, if registered.
    pub fn line_for(&self, label: &str, offset: u32) -> Option<u32> {
        self.files.get(label).map(|f| f.line_of(offset))
    }

    /// Start a step from `(label, line)` at `depth` frames.
    pub fn start_step(&mut self, mode: StepMode, label: String, line: u32, depth: usize) {
        self.step = Some(StepRule {
            mode,
            depth,
            label,
            line,
        });
    }

    /// Line breakpoint check for one op span at `depth` frames: the
    /// 1-based line when this op should suspend, `None` otherwise. Every
    /// attributable op refreshes the last-position tracker (breakpoints
    /// and plain lines alike) so loop revisits still fire.
    pub fn check_span(
        &mut self,
        label: Option<&str>,
        span: zz_frontend::span::Span,
        depth: usize,
    ) -> Option<u32> {
        if span.start == 0 && span.end == 0 {
            return None;
        }
        let label = label?;
        let file = self.files.get(label)?;
        let line = file.line_of(span.start);
        let current = (label.to_string(), line);
        let changed = self.last.replace(current.clone()) != Some(current.clone());
        // Steps win over breakpoints: one-shot, cleared on fire.
        if let Some(rule) = self.step.clone() {
            let moved = label != rule.label || line != rule.line;
            let fire = match rule.mode {
                StepMode::Over => depth <= rule.depth && moved,
                StepMode::Into => moved,
                StepMode::Out => depth < rule.depth,
            };
            if fire {
                self.step = None;
                self.stop_reason = StopReason::Step;
                return Some(line);
            }
        }
        if changed && self.breakpoints.contains(&current) {
            self.stop_reason = StopReason::Breakpoint;
            return Some(line);
        }
        None
    }
}

/// One step down a variable tree.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DebugSeg {
    /// Struct field / dict key / option payload (`value`) / result arm.
    Field(String),
    /// Absolute array/tuple index.
    Index(usize),
}

/// One child of a composite value for the variables view.
#[derive(Debug, Clone)]
pub enum DebugChild {
    /// A real child value.
    Value {
        name: String,
        value: crate::value::Value,
    },
    /// A paging window over a long sequence (`[0..99]`).
    Range {
        name: String,
        start: usize,
        len: usize,
    },
}

/// Maximum direct children before paging windows kick in (DAP convention).
pub const DEBUG_PAGE_LEN: usize = 100;

/// Whether a value expands in the variables view.
pub fn debug_has_children(value: &crate::value::Value) -> bool {
    use crate::value::Value;
    matches!(
        value,
        Value::Object(_)
            | Value::Array(_)
            | Value::Tuple(_)
            | Value::Dict(_)
            | Value::Option(_)
            | Value::Result(_)
            | Value::Bytes(_)
    )
}

/// Child count for counts (`namedVariables` / `indexedVariables`).
/// Sequences report the raw length (clients page via ranges).
pub fn debug_child_count(value: &crate::value::Value) -> (usize, usize) {
    use crate::value::Value;
    match value {
        Value::Object(o) => (o.fields.len(), 0),
        Value::Array(a) | Value::Tuple(a) => (0, a.len()),
        Value::Dict(d) => (d.len(), 0),
        Value::Option(o) => (usize::from(o.is_some()), 0),
        Value::Result(_) => (1, 0),
        Value::Bytes(b) => (0, b.len()),
        _ => (0, 0),
    }
}

/// Direct children of `value` (one level; sequences page past
/// [`DEBUG_PAGE_LEN`]).
pub fn debug_children(value: &crate::value::Value) -> Vec<DebugChild> {
    use crate::value::Value;
    match value {
        Value::Object(o) => o
            .fields
            .iter()
            .map(|(name, value)| DebugChild::Value {
                name: name.to_string(),
                value: value.clone(),
            })
            .collect(),
        Value::Array(a) | Value::Tuple(a) => page_sequence(a, "Array"),
        Value::Dict(d) => d
            .iter()
            .enumerate()
            .map(|(i, (k, v))| {
                let name = match k {
                    Value::Str(s) => s.to_string(),
                    Value::Int(n) => n.to_string(),
                    _ => format!("[{i}]"),
                };
                DebugChild::Value {
                    name,
                    value: v.clone(),
                }
            })
            .collect(),
        Value::Option(o) => match o.as_ref() {
            Some(v) => vec![DebugChild::Value {
                name: "value".to_string(),
                value: v.as_ref().clone(),
            }],
            None => Vec::new(),
        },
        Value::Result(r) => match r.as_ref() {
            Ok(v) => vec![DebugChild::Value {
                name: "ok".to_string(),
                value: v.clone(),
            }],
            Err(e) => vec![DebugChild::Value {
                name: "err".to_string(),
                value: e.clone(),
            }],
        },
        Value::Bytes(b) => {
            let bytes: Vec<crate::value::Value> = b
                .as_slice()
                .iter()
                .map(|byte| crate::value::Value::Int(*byte as i64))
                .collect();
            page_sequence(&bytes, "Bytes")
        }
        _ => Vec::new(),
    }
}

fn page_sequence(values: &[crate::value::Value], _kind: &str) -> Vec<DebugChild> {
    if values.len() <= DEBUG_PAGE_LEN {
        return values
            .iter()
            .enumerate()
            .map(|(i, value)| DebugChild::Value {
                name: format!("[{i}]"),
                value: value.clone(),
            })
            .collect();
    }
    values
        .chunks(DEBUG_PAGE_LEN)
        .enumerate()
        .map(|(page, chunk)| {
            let start = page * DEBUG_PAGE_LEN;
            DebugChild::Range {
                name: format!("[{start}..{}]", start + chunk.len() - 1),
                start,
                len: chunk.len(),
            }
        })
        .collect()
}

/// The named direct child value of a composite (ranges excluded —
/// absolute indexes address sequences directly).
pub fn debug_child_named(value: &crate::value::Value, name: &str) -> Option<crate::value::Value> {
    debug_children(value)
        .into_iter()
        .find_map(|child| match child {
            DebugChild::Value { name: n, value } if n == name => Some(value),
            _ => None,
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::eval::Interp;
    use crate::value::{Value, YieldReason};
    use crate::vm::{Compiler, Vm};
    use std::sync::Arc;

    fn run_debug(src: &str, label: &str, lines: &[u32]) -> (Vec<u32>, Value) {
        let parsed = zz_frontend::parse(src);
        assert!(parsed.errors.is_empty());
        let mut chunk = Compiler::compile_program(&parsed.program);
        // Label the whole tree (function bodies are sub-chunks in
        // MakeFunc ops): unlabeled chunks never resolve lines, so
        // breakpoints and steps inside functions would silently miss.
        crate::coverage::stamp_chunk_tree(&mut chunk, &Arc::from(label));
        let chunk = Arc::new(chunk);

        let mut session = DebugSession::new();
        session.add_source(label, src);
        for line in lines {
            session.add_breakpoint(label, *line);
        }

        let mut interp = Interp::new();
        let mut vm = Vm::new();
        vm.attach_debug(session);

        let mut hits = Vec::new();
        let mut first = true;
        let result = loop {
            // Exactly one frame push (first iteration); resumes continue
            // the preserved frames in place.
            let outcome = if first {
                first = false;
                vm.run_chunk(&chunk, &mut interp)
            } else {
                vm.resume_chunk(&mut interp)
            };
            match outcome {
                Ok(crate::runtime::Flow::Value(v)) => break v,
                Ok(crate::runtime::Flow::Yield(YieldReason::DebugBreak { line })) => {
                    hits.push(line);
                }
                other => panic!("unexpected outcome: {other:?}"),
            }
        };
        // Inspectability spot-check happens in dedicated tests; here the
        // value must equal the undebugged run.
        let plain = {
            let parsed = zz_frontend::parse(src);
            let mut interp = Interp::new();
            interp.run(&parsed.program).expect("plain run")
        };
        assert_eq!(result, plain);
        (hits, result)
    }

    #[test]
    fn breakpoint_hits_inspectable_line() {
        let src = "a := 10\nb := a + 5\nc := b * 2\nc\n";
        let parsed = zz_frontend::parse(src);
        let mut chunk = Compiler::compile_program(&parsed.program);
        crate::coverage::stamp_chunk_tree(&mut chunk, &Arc::from("spike"));
        let chunk = Arc::new(chunk);

        let mut session = DebugSession::new();
        session.add_source("spike", src);
        session.add_breakpoint("spike", 2);

        let mut interp = Interp::new();
        let mut vm = Vm::new();
        vm.attach_debug(session);

        match vm.run_chunk(&chunk, &mut interp) {
            Ok(crate::runtime::Flow::Yield(YieldReason::DebugBreak { line })) => {
                assert_eq!(line, 2);
                // Line 1 executed: `a` visible. (Unassigned slots
                // pre-read as `()` — spike-acceptable; real shells do
                // the same until declaration tracking lands.)
                assert_eq!(vm.debug_lookup(&interp, "a"), Some(Value::Int(10)));
            }
            other => panic!("expected a breakpoint yield, got: {other:?}"),
        }

        // Resume runs to completion with the right value.
        match vm.resume_chunk(&mut interp) {
            Ok(crate::runtime::Flow::Value(Value::Int(30))) => {}
            other => panic!("expected Int(30), got: {other:?}"),
        }
    }

    #[test]
    fn multiple_breakpoints_hit_in_order() {
        let src = "a := 10\nb := a + 5\nc := b * 2\nc\n";
        let (hits, value) = run_debug(src, "spike", &[2, 3]);
        assert_eq!(hits, vec![2, 3]);
        assert_eq!(value, Value::Int(30));
    }

    #[test]
    fn comment_line_breakpoint_never_fires() {
        let src = "a := 10\n// just a comment\nb := a + 1\nb\n";
        let (hits, value) = run_debug(src, "spike", &[2]);
        assert!(hits.is_empty(), "no attributable op on a comment line");
        assert_eq!(value, Value::Int(11));
    }

    #[test]
    fn line_table_maps_offsets() {
        let file = SourceFile::new("ab\ncde\n\nf");
        assert_eq!(file.line_of(0), 1);
        assert_eq!(file.line_of(2), 1);
        assert_eq!(file.line_of(3), 2);
        assert_eq!(file.line_of(8), 4);
    }
}

#[cfg(test)]
mod step_tests {
    use super::*;
    use crate::eval::Interp;
    use crate::value::{Value, YieldReason};
    use crate::vm::{Compiler, Vm};
    use std::sync::Arc;

    fn parked_program() -> (Vm, Interp, Arc<crate::vm::Chunk>) {
        // Attributable work follows the call so step-out has
        // somewhere to land (slot shuffles carry no spans).
        let src = "func add(a: int, b: int) -> int {\n    return a + b\n}\nx := add(1, 2)\ny := x + 1\ny\n";
        let parsed = zz_frontend::parse(src);
        assert!(parsed.errors.is_empty());
        let mut chunk = Compiler::compile_program(&parsed.program);
        crate::coverage::stamp_chunk_tree(&mut chunk, &Arc::from("spike"));
        let chunk = Arc::new(chunk);

        let mut session = DebugSession::new();
        session.add_source("spike", src);
        session.add_breakpoint("spike", 4);

        let mut interp = Interp::new();
        let mut vm = Vm::new();
        vm.attach_debug(session);
        match vm.run_chunk(&chunk, &mut interp) {
            Ok(crate::runtime::Flow::Yield(YieldReason::DebugBreak { line })) => {
                assert_eq!(line, 4);
            }
            other => panic!("expected break at line 4, got: {other:?}"),
        }
        (vm, interp, chunk)
    }

    #[test]
    fn step_into_descends_and_out_returns() {
        use crate::debug::StepMode;

        let (mut vm, mut interp, chunk) = parked_program();
        assert_eq!(vm.debug_frames().len(), 1);

        // Into the call: next suspension is inside `add`.
        {
            let session = vm.debug_session().expect("session");
            let mut session = session.lock().unwrap();
            session.start_step(StepMode::Into, "spike".to_string(), 4, 1);
        }
        match vm.resume_chunk(&mut interp) {
            Ok(crate::runtime::Flow::Yield(YieldReason::DebugBreak { line })) => {
                assert_ne!(line, 4, "stepped into the callee");
                assert_eq!(vm.debug_frames().len(), 2);
            }
            other => panic!("expected step-into suspend, got: {other:?}"),
        }

        // Out: back to the caller frame.
        {
            let session = vm.debug_session().expect("session");
            let mut session = session.lock().unwrap();
            session.start_step(StepMode::Out, "spike".to_string(), 1, 2);
        }
        match vm.resume_chunk(&mut interp) {
            Ok(crate::runtime::Flow::Yield(YieldReason::DebugBreak { .. })) => {
                assert_eq!(vm.debug_frames().len(), 1, "back in the caller");
            }
            other => panic!("expected step-out suspend, got: {other:?}"),
        }

        // To completion with the right value.
        match vm.resume_chunk(&mut interp) {
            Ok(crate::runtime::Flow::Value(Value::Int(4))) => {}
            other => panic!("expected Int(4), got: {other:?}"),
        }
        let _ = chunk;
    }
}
