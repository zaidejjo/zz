//! Eval session: shared by the REPL, `zz eval`, and integration tests.
//!
//! Keeps the interpreter environment across calls so the REPL accumulates
//! bindings, and renders diagnostics through the same path everywhere.

use zz_frontend::diag::{error_at, render_to_string, Files, RawDiag};
use zz_frontend::parse;
use zz_runtime::{EvalError, Interp, Value};

/// Result of evaluating one source snippet.
pub struct EvalOutput {
    /// Value output (empty string for unit).
    pub output: String,
    /// Rendered diagnostics, if any errors occurred.
    pub errors: Option<String>,
}

pub struct Session {
    pub interp: Interp,
    files: Files,
    file_id: usize,
    name: String,
    last_had_errors: bool,
}

impl Session {
    pub fn new(name: impl Into<String>) -> Self {
        let name = name.into();
        let mut files = Files::new();
        let file_id = files.add(name.clone(), String::new());
        Session {
            interp: Interp::new(),
            files,
            file_id,
            name,
            last_had_errors: false,
        }
    }

    /// Whether the most recent `eval` produced errors.
    pub fn last_eval_had_errors(&self) -> bool {
        self.last_had_errors
    }

    /// Evaluate source, updating the environment. Returns printed value and
    /// any rendered errors. On parse/runtime errors the environment is not
    /// modified (except partially-run statements are possible — future
    /// phases will validate first, then run).
    pub fn eval(&mut self, src: &str) -> EvalOutput {
        // Update the file backing so diagnostics show the current source.
        self.file_id = self.files.add(self.name.clone(), src.to_string());

        let parsed = parse(src);
        if !parsed.errors.is_empty() {
            self.last_had_errors = true;
            return EvalOutput {
                output: String::new(),
                errors: Some(render_to_string(&self.files, self.file_id, &parsed.errors)),
            };
        }

        match self.interp.run(&parsed.program) {
            Ok(v) => {
                self.last_had_errors = false;
                EvalOutput {
                    output: display_value(&v),
                    errors: None,
                }
            }
            Err(e) => {
                self.last_had_errors = true;
                let diags = eval_error_to_diag(&e);
                EvalOutput {
                    output: String::new(),
                    errors: Some(render_to_string(&self.files, self.file_id, &diags)),
                }
            }
        }
    }

    /// Like `eval`, but writes rendered errors to stderr.
    pub fn eval_to_console(&mut self, src: &str) -> String {
        let out = self.eval(src);
        if let Some(errs) = &out.errors {
            eprintln!("{}", errs.trim_end_matches('\n'));
        }
        out.output
    }
}

fn display_value(v: &Value) -> String {
    v.to_string()
}

fn eval_error_to_diag(e: &EvalError) -> Vec<RawDiag> {
    vec![error_at(e.message.clone(), e.span)]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn acceptance_gate_let_binding() {
        // Phase 0 gate: `let x = 1 + 2` evaluates to 3.
        let mut s = Session::new("<test>");
        let out = s.eval("let x = 1 + 2");
        assert!(out.errors.is_none(), "unexpected errors: {:?}", out.errors);
        assert_eq!(out.output, "3");
    }

    #[test]
    fn session_accumulates_bindings() {
        let mut s = Session::new("<test>");
        s.eval("let a = 10");
        let out = s.eval("a + 5");
        assert_eq!(out.output, "15");
    }

    #[test]
    fn parse_error_renders() {
        let mut s = Session::new("<test>");
        let out = s.eval("let = 3");
        let errs = out.errors.expect("expected parse errors");
        assert!(errs.contains("expected identifier"), "errors: {errs}");
    }

    #[test]
    fn runtime_error_renders() {
        let mut s = Session::new("<test>");
        let out = s.eval("1 / 0");
        let errs = out.errors.expect("expected runtime errors");
        assert!(errs.contains("division by zero"), "errors: {errs}");
    }

    #[test]
    fn arithmetic_output() {
        let mut s = Session::new("<test>");
        let out = s.eval("2 + 2 * 0");
        assert_eq!(out.output, "2");
    }
}
