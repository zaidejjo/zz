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
#[derive(Debug, Clone, Default)]
pub struct DebugSession {
    files: HashMap<String, SourceFile>,
    breakpoints: HashSet<(String, u32)>,
    /// Last attributable position executed. Breakpoints fire only on
    /// line *changes*: without this, resuming past a breakpoint would
    /// re-suspend on the next op of the same line forever, while loops
    /// still re-break on genuine revisits.
    last: Option<(String, u32)>,
}

impl DebugSession {
    pub fn new() -> Self {
        Self::default()
    }

    /// Register a source under `label` (matches the chunk source label).
    pub fn add_source(&mut self, label: impl Into<String>, source: &str) {
        self.files.insert(label.into(), SourceFile::new(source));
    }

    /// Break before the first attributable op on `line` (1-based) of `label`.
    pub fn add_breakpoint(&mut self, label: impl Into<String>, line: u32) {
        self.breakpoints.insert((label.into(), line));
    }

    /// Line breakpoint check for one op span: the 1-based line when this
    /// op should suspend, `None` otherwise.
    /// Line breakpoint check for one op span: the 1-based line when this
    /// op should suspend, `None` otherwise. Every attributable op
    /// refreshes the last-position tracker (breakpoints and plain lines
    /// alike) so loop revisits still fire.
    pub fn check_span(
        &mut self,
        label: Option<&str>,
        span: zz_frontend::span::Span,
    ) -> Option<u32> {
        if span.start == 0 && span.end == 0 {
            return None;
        }
        let label = label?;
        let file = self.files.get(label)?;
        let line = file.line_of(span.start);
        let current = (label.to_string(), line);
        let changed = self.last.replace(current.clone()) != Some(current.clone());
        (changed && self.breakpoints.contains(&current)).then_some(line)
    }
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
        chunk.source = Some(Arc::from(label));
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
        chunk.source = Some(Arc::from("spike"));
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
