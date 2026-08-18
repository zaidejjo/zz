//! Eval session: shared by the REPL, `zz eval`, and integration tests.
//!
//! Keeps the interpreter environment across calls so the REPL accumulates
//! bindings, and renders diagnostics through the same path everywhere.
//!
//! Phase 1: every snippet is type-checked before it runs. The checker is
//! seeded with the types of prior top-level bindings and functions, so the
//! REPL can reference earlier definitions.

use std::collections::HashMap;

use zz_checker::{check_program, FuncSig, Type};
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
    /// Types of top-level bindings from previous snippets (checker seed).
    bindings: HashMap<String, Type>,
    /// Signatures of functions from previous snippets (checker seed).
    funcs: HashMap<String, FuncSig>,
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
            bindings: HashMap::new(),
            funcs: HashMap::new(),
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

    /// Evaluate source: parse, type-check, then run. The environment is only
    /// updated when the snippet is both syntactically and type-correct.
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

        let checked = check_program(&parsed.program, self.bindings.clone(), self.funcs.clone());
        if !checked.errors.is_empty() {
            self.last_had_errors = true;
            return EvalOutput {
                output: String::new(),
                errors: Some(render_to_string(&self.files, self.file_id, &checked.errors)),
            };
        }

        match self.interp.run(&parsed.program) {
            Ok(v) => {
                self.last_had_errors = false;
                // Seed the checker with this snippet's new top-level types.
                self.bindings.extend(checked.bindings);
                self.funcs.extend(checked.funcs);
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
        // Phase 0 gate: `x := 1 + 2` evaluates to 3.
        let mut s = Session::new("<test>");
        let out = s.eval("x := 1 + 2");
        assert!(out.errors.is_none(), "unexpected errors: {:?}", out.errors);
        assert_eq!(out.output, "3");
    }

    #[test]
    fn session_accumulates_bindings() {
        let mut s = Session::new("<test>");
        s.eval("a := 10");
        let out = s.eval("a + 5");
        assert_eq!(out.output, "15");
    }

    #[test]
    fn parse_error_renders() {
        let mut s = Session::new("<test>");
        let out = s.eval("= 3");
        let errs = out.errors.expect("expected parse errors");
        assert!(errs.contains("expected expression"), "errors: {errs}");
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

    #[test]
    fn type_error_blocks_run() {
        let mut s = Session::new("<test>");
        let out = s.eval("x := 1 + \"a\"");
        let errs = out.errors.expect("expected type errors");
        assert!(
            errs.contains("cannot apply `+` to `int` and `str`"),
            "errors: {errs}"
        );
        // The bad binding must not leak into the session.
        let out2 = s.eval("x");
        assert!(out2.errors.is_some(), "x should still be undefined");
    }

    #[test]
    fn func_defined_in_one_snippet_called_in_next() {
        let mut s = Session::new("<test>");
        let out = s.eval("func add(a: int, b: int) -> int { a + b }");
        assert!(out.errors.is_none(), "errors: {:?}", out.errors);
        let out2 = s.eval("add(2, 3)");
        assert!(out2.errors.is_none(), "errors: {:?}", out2.errors);
        assert_eq!(out2.output, "5");
    }

    #[test]
    fn generic_func_called_across_snippets() {
        let mut s = Session::new("<test>");
        let out = s.eval("func id<T>(x: T) -> T { x }");
        assert!(out.errors.is_none(), "errors: {:?}", out.errors);
        let out2 = s.eval("id(7)");
        assert!(out2.errors.is_none(), "errors: {:?}", out2.errors);
        assert_eq!(out2.output, "7");
    }

    #[test]
    fn match_and_variants_run() {
        let mut s = Session::new("<test>");
        let out = s.eval("v := .some(1)\nmatch v { .some(n) => n, .none => 0 }");
        assert!(out.errors.is_none(), "errors: {:?}", out.errors);
        assert_eq!(out.output, "1");
    }

    #[test]
    fn explicit_decl_runs() {
        let mut s = Session::new("<test>");
        let out = s.eval("int x = 10\nx");
        assert!(out.errors.is_none(), "errors: {:?}", out.errors);
        assert_eq!(out.output, "10");
    }

    #[test]
    fn array_and_dict_run() {
        let mut s = Session::new("<test>");
        let out = s.eval("scores := [10, 20, 30]\nscores");
        assert!(out.errors.is_none(), "errors: {:?}", out.errors);
        assert_eq!(out.output, "[10, 20, 30]");
        let out2 = s.eval("{str: int} ages = {\"a\": 1}\nages");
        assert!(out2.errors.is_none(), "errors: {:?}", out2.errors);
        assert_eq!(out2.output, "{a: 1}");
    }

    #[test]
    fn import_accepted() {
        let mut s = Session::new("<test>");
        let out = s.eval("import std.io\n1 + 1");
        assert!(out.errors.is_none(), "errors: {:?}", out.errors);
        assert_eq!(out.output, "2");
    }
}
