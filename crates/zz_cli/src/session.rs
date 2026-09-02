//! Eval session: shared by the REPL, `zz eval`, and integration tests.
//!
//! Keeps the interpreter environment across calls so the REPL accumulates
//! bindings, and renders diagnostics through the same path everywhere.
//!
//! Phase 1: every snippet is type-checked before it runs. The checker is
//! seeded with the types of prior top-level bindings and functions, so the
//! REPL can reference earlier definitions.

use std::collections::HashMap;

use zz_checker::{check_program, FuncSig, StructSig, Type};
use zz_frontend::diag::{error_at, render_to_string, Files, RawDiag};
use zz_frontend::parse;
use zz_runtime::{EvalError, Interp, Value};
use zz_stdlib::{register_module_namespace, stdlib_funcs, stdlib_natives, STDLIB_MODULES};

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
    /// Struct definitions from previous snippets (checker seed).
    structs: HashMap<String, StructSig>,
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
            interp: Interp::with_natives(stdlib_natives()),
            bindings: HashMap::new(),
            funcs: stdlib_funcs(),
            structs: HashMap::new(),
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

        // Process imports: register `std.*` modules under their namespace
        // (or alias) in both the checker seed and the interpreter. Relative
        // imports need a file context and are rejected in the REPL.
        for stmt in &parsed.program.stmts {
            if let zz_frontend::ast::Stmt::Import {
                path,
                alias,
                span,
                pub_: _,
            } = stmt
            {
                if path.first().map(String::as_str) != Some("std") {
                    self.last_had_errors = true;
                    return EvalOutput {
                        output: String::new(),
                        errors: Some(render_to_string(
                            &self.files,
                            self.file_id,
                            &[error_at(
                                "relative imports require a file context (`zz run`)",
                                *span,
                            )],
                        )),
                    };
                }
                let Some(module) = path.get(1) else { continue };
                if !STDLIB_MODULES.contains(&module.as_str()) {
                    self.last_had_errors = true;
                    return EvalOutput {
                        output: String::new(),
                        errors: Some(render_to_string(
                            &self.files,
                            self.file_id,
                            &[error_at(
                                format!("unknown standard library module `std.{module}`"),
                                *span,
                            )],
                        )),
                    };
                }
                let ns = alias.clone().unwrap_or_else(|| module.clone());
                if let Err(msg) = register_module_namespace(
                    module,
                    &ns,
                    &mut self.funcs,
                    &mut self.interp.natives,
                ) {
                    self.last_had_errors = true;
                    return EvalOutput {
                        output: String::new(),
                        errors: Some(render_to_string(
                            &self.files,
                            self.file_id,
                            &[error_at(msg, *span)],
                        )),
                    };
                }
            }
        }

        let checked = check_program(
            &parsed.program,
            self.bindings.clone(),
            self.funcs.clone(),
            self.structs.clone(),
        );
        let has_errors = checked
            .errors
            .iter()
            .any(|e| e.severity == zz_frontend::diag::Severity::Error);
        if has_errors {
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
                self.structs.extend(checked.structs);
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
    let mut diag = error_at(e.message.clone(), e.span);
    for (name, _span) in &e.backtrace {
        if name.is_empty() {
            diag = diag.with_note("  at <top-level>");
        } else {
            diag = diag.with_note(format!("  at {name}"));
        }
    }
    vec![diag]
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
        let out = s.eval("x: int = 10\nx");
        assert!(out.errors.is_none(), "errors: {:?}", out.errors);
        assert_eq!(out.output, "10");
    }

    #[test]
    fn array_and_dict_run() {
        let mut s = Session::new("<test>");
        let out = s.eval("scores := [10, 20, 30]\nscores");
        assert!(out.errors.is_none(), "errors: {:?}", out.errors);
        assert_eq!(out.output, "[10, 20, 30]");
        let out2 = s.eval("ages: {str: int} = {\"a\": 1}\nages");
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

    #[test]
    fn stdlib_callable_in_snippet() {
        let mut s = Session::new("<test>");
        let out = s.eval("import std.str\nstr.length(\"abc\")");
        assert!(out.errors.is_none(), "errors: {:?}", out.errors);
        assert_eq!(out.output, "3");
    }

    #[test]
    fn stdlib_generic_vec_call() {
        let mut s = Session::new("<test>");
        let out = s.eval("import std.vec\nv := [1, 2, 3]\np := vec.push(v, 4)\nvec.len(p)");
        assert!(out.errors.is_none(), "errors: {:?}", out.errors);
        assert_eq!(out.output, "4");
    }

    #[test]
    fn stdlib_print_any_value() {
        let mut s = Session::new("<test>");
        let out = s.eval("import std.io\nio.println(42)");
        assert!(out.errors.is_none(), "errors: {:?}", out.errors);
        assert_eq!(out.output, "");
    }

    #[test]
    fn stdlib_wrong_arg_type_blocks() {
        let mut s = Session::new("<test>");
        let out = s.eval("import std.str\nstr.length(5)");
        assert!(out.errors.is_some(), "expected type error");
        // Must not pollute the session.
        let out2 = s.eval("1");
        assert!(out2.errors.is_none());
    }

    #[test]
    fn stdlib_unknown_func_errors() {
        let mut s = Session::new("<test>");
        let out = s.eval("import std.io\nio.nope(1)");
        assert!(out.errors.is_some(), "expected error");
    }

    #[test]
    fn stdlib_import_alias() {
        let mut s = Session::new("<test>");
        let out = s.eval("import std.str as s\ns.length(\"abc\")");
        assert!(out.errors.is_none(), "errors: {:?}", out.errors);
        assert_eq!(out.output, "3");
    }

    #[test]
    fn relative_import_rejected_in_repl() {
        let mut s = Session::new("<test>");
        let out = s.eval("import config");
        let errs = out.errors.expect("expected error");
        assert!(errs.contains("require a file context"), "errors: {errs}");
    }

    #[test]
    fn unknown_stdlib_module_rejected_in_repl() {
        let mut s = Session::new("<test>");
        let out = s.eval("import std.nope");
        let errs = out.errors.expect("expected error");
        assert!(
            errs.contains("unknown standard library module"),
            "errors: {errs}"
        );
    }

    // --- string interpolation ---------------------------------------------

    #[test]
    fn fstring_binding() {
        let mut s = Session::new("<test>");
        let out = s.eval("name := \"World\"\n\"Hello {name}\"");
        assert!(out.errors.is_none(), "errors: {:?}", out.errors);
        assert_eq!(out.output, "Hello World");
    }

    #[test]
    fn fstring_expression() {
        let mut s = Session::new("<test>");
        let out = s.eval("x := 1 + 2\n\"sum: {x}\"");
        assert!(out.errors.is_none(), "errors: {:?}", out.errors);
        assert_eq!(out.output, "sum: 3");
    }

    #[test]
    fn fstring_function_call() {
        let mut s = Session::new("<test>");
        let out = s.eval("func dbl(n: int) -> int { n * 2 }\n\"double: {dbl(4)}\"");
        assert!(out.errors.is_none(), "errors: {:?}", out.errors);
        assert_eq!(out.output, "double: 8");
    }

    #[test]
    fn fstring_multi_part() {
        let mut s = Session::new("<test>");
        let out = s.eval("a := 1\nb := 2\n\"{a} + {b} = {a + b}\"");
        assert!(out.errors.is_none(), "errors: {:?}", out.errors);
        assert_eq!(out.output, "1 + 2 = 3");
    }

    #[test]
    fn fstring_literal_brace_not_interpolated() {
        // `%` is not an identifier start, so `{` stays literal.
        let mut s = Session::new("<test>");
        let out = s.eval("\"100% sure\"");
        assert!(out.errors.is_none(), "errors: {:?}", out.errors);
        assert_eq!(out.output, "100% sure");
    }

    // --- std.json ----------------------------------------------------------

    #[test]
    fn json_parse_and_get() {
        let mut s = Session::new("<test>");
        let out = s.eval(
            "import std.json\nj := json.parse(\"{\\\"a\\\": [1, 2]}\").unwrap()\njson.get(j, \"a\")",
        );
        assert!(out.errors.is_none(), "errors: {:?}", out.errors);
        assert_eq!(out.output, ".ok([1,2])");
    }

    #[test]
    fn json_as_int() {
        let mut s = Session::new("<test>");
        let out = s.eval(
            "import std.json\nj := json.parse(\"{\\\"n\\\": 42}\").unwrap()\njson.as_int(json.get(j, \"n\").unwrap())",
        );
        assert!(out.errors.is_none(), "errors: {:?}", out.errors);
        assert_eq!(out.output, "42");
    }

    #[test]
    fn json_stringify_array() {
        let mut s = Session::new("<test>");
        let out = s.eval("import std.json\njson.stringify([1, 2, 3])");
        assert!(out.errors.is_none(), "errors: {:?}", out.errors);
        assert_eq!(out.output, ".ok([1,2,3])");
    }

    #[test]
    fn json_stringify_dict() {
        let mut s = Session::new("<test>");
        let out = s.eval("import std.json\njson.stringify({\"name\": \"zaid\"})");
        assert!(out.errors.is_none(), "errors: {:?}", out.errors);
        assert_eq!(out.output, r#".ok({"name":"zaid"})"#);
    }

    #[test]
    fn json_wrong_access_errors() {
        let mut s = Session::new("<test>");
        let out = s.eval("import std.json\nj := json.parse(\"[1, 2]\").unwrap()\njson.as_int(j)");
        assert!(out.errors.is_some(), "expected error");
    }

    // --- std.http ----------------------------------------------------------

    #[test]
    fn http_get_route() {
        let mut s = Session::new("<test>");
        let out = s.eval(
            "import std.http\ns := http.server()\ns2 := http.get(s, \"/hi\", |path: str| \"hello\")\nhttp.handle(s2, \"GET\", \"/hi\", \"\")",
        );
        assert!(out.errors.is_none(), "errors: {:?}", out.errors);
        assert_eq!(out.output, "hello");
    }

    #[test]
    fn http_post_body_passthrough() {
        let mut s = Session::new("<test>");
        let out = s.eval(
            "import std.http\ns := http.server()\ns2 := http.post(s, \"/echo\", |body: str| body)\nhttp.handle(s2, \"POST\", \"/echo\", \"ping\")",
        );
        assert!(out.errors.is_none(), "errors: {:?}", out.errors);
        assert_eq!(out.output, "ping");
    }

    #[test]
    fn http_no_route_errors() {
        let mut s = Session::new("<test>");
        let out =
            s.eval("import std.http\ns := http.server()\nhttp.handle(s, \"GET\", \"/nope\", \"\")");
        assert!(out.errors.is_some(), "expected error");
    }

    // --- structs -----------------------------------------------------------

    #[test]
    fn struct_defined_in_one_snippet_used_in_next() {
        let mut s = Session::new("<test>");
        let out = s.eval("struct Point { x: int, y: int }");
        assert!(out.errors.is_none(), "errors: {:?}", out.errors);
        let out2 = s.eval("p := Point{ x: 1, y: 2 }\np.x");
        assert!(out2.errors.is_none(), "errors: {:?}", out2.errors);
        assert_eq!(out2.output, "1");
    }

    #[test]
    fn struct_field_mutation_in_session() {
        let mut s = Session::new("<test>");
        s.eval("struct Point { x: int, y: int }\np := Point{ x: 1, y: 2 }");
        let out = s.eval("p.x = 10\np");
        assert!(out.errors.is_none(), "errors: {:?}", out.errors);
        assert_eq!(out.output, "Point{x: 10, y: 2}");
    }

    #[test]
    fn struct_func_cross_snippet() {
        let mut s = Session::new("<test>");
        s.eval("struct Point { x: int, y: int }");
        let out = s.eval("func dist(p: Point) -> int { p.x + p.y }\ndist(Point{ x: 3, y: 4 })");
        assert!(out.errors.is_none(), "errors: {:?}", out.errors);
        assert_eq!(out.output, "7");
    }

    #[test]
    fn struct_type_error_blocks_run() {
        let mut s = Session::new("<test>");
        let out = s.eval("struct Point { x: int, y: int }\np := Point{ x: \"a\" }");
        assert!(out.errors.is_some(), "expected type error");
        let out2 = s.eval("p");
        assert!(out2.errors.is_some(), "p should not be bound");
    }

    // --- for loops ---------------------------------------------------------

    #[test]
    fn for_loop_in_session() {
        let mut s = Session::new("<test>");
        let out = s.eval("sum := 0\nfor i in 0..5 { sum = sum + i }\nsum");
        assert!(out.errors.is_none(), "errors: {:?}", out.errors);
        assert_eq!(out.output, "10");
    }

    #[test]
    fn for_loop_over_array_in_session() {
        let mut s = Session::new("<test>");
        let out = s.eval("total := 0\nfor n in [10, 20, 30] { total = total + n }\ntotal");
        assert!(out.errors.is_none(), "errors: {:?}", out.errors);
        assert_eq!(out.output, "60");
    }

    #[test]
    fn break_and_continue_in_session() {
        let mut s = Session::new("<test>");
        let out = s.eval("found := 0\nfor i in 0..10 { if i == 3 { found = i; break } }\nfound");
        assert!(out.errors.is_none(), "errors: {:?}", out.errors);
        assert_eq!(out.output, "3");
        let out2 = s
            .eval("count := 0\nfor i in 0..5 { if i == 2 { continue }; count = count + 1 }\ncount");
        assert!(out2.errors.is_none(), "errors: {:?}", out2.errors);
        assert_eq!(out2.output, "4");
    }

    #[test]
    fn break_outside_loop_rejected() {
        let mut s = Session::new("<test>");
        let out = s.eval("break");
        assert!(out.errors.is_some(), "expected error");
    }

    // --- indexing & slicing -------------------------------------------------

    #[test]
    fn index_in_session() {
        let mut s = Session::new("<test>");
        let out = s.eval("scores := [10, 20, 30]\nscores[1]");
        assert!(out.errors.is_none(), "errors: {:?}", out.errors);
        assert_eq!(out.output, "20");
    }

    #[test]
    fn dict_index_assign_in_session() {
        let mut s = Session::new("<test>");
        let out = s.eval("ages := {\"a\": 1}\nages[\"b\"] = 2\nages[\"b\"]");
        assert!(out.errors.is_none(), "errors: {:?}", out.errors);
        assert_eq!(out.output, "2");
    }

    #[test]
    fn slice_in_session() {
        let mut s = Session::new("<test>");
        let out = s.eval("s := \"hello\"\ns[1:3]");
        assert!(out.errors.is_none(), "errors: {:?}", out.errors);
        assert_eq!(out.output, "el");
    }

    // --- pipeline -----------------------------------------------------------

    #[test]
    fn pipe_in_session() {
        let mut s = Session::new("<test>");
        s.eval("func inc(n: int) -> int { n + 1 }");
        let out = s.eval("5 |> inc");
        assert!(out.errors.is_none(), "errors: {:?}", out.errors);
        assert_eq!(out.output, "6");
    }

    // --- typeof & stdlib ----------------------------------------------------

    #[test]
    fn typeof_in_session() {
        let mut s = Session::new("<test>");
        let out = s.eval("typeof(1)");
        assert!(out.errors.is_none(), "errors: {:?}", out.errors);
        assert_eq!(out.output, "int");
    }

    #[test]
    fn std_fs_in_session() {
        let mut s = Session::new("<test>");
        let out = s.eval("import std.fs\nfs.exists(\"/tmp/zz_no_such_file_zz\")");
        assert!(out.errors.is_none(), "errors: {:?}", out.errors);
        assert_eq!(out.output, "false");
    }

    #[test]
    fn std_env_in_session() {
        let mut s = Session::new("<test>");
        let out = s.eval("import std.env\nenv.get_var(\"ZZ_NO_SUCH_VAR_12345\")");
        assert!(out.errors.is_none(), "errors: {:?}", out.errors);
        assert_eq!(out.output, ".none");
    }

    // --- Phase 5: methods, space fix, conversions --------------------------

    #[test]
    fn method_call_in_session() {
        let mut s = Session::new("<test>");
        s.eval("struct Point { x: int, y: int }\nfunc dist(p: Point) -> int { p.x + p.y }");
        let out = s.eval("p := Point { x: 3, y: 4 }\np.dist()");
        assert!(out.errors.is_none(), "errors: {:?}", out.errors);
        assert_eq!(out.output, "7");
    }

    #[test]
    fn struct_init_with_space_in_session() {
        let mut s = Session::new("<test>");
        s.eval("struct Point { x: int }");
        let out = s.eval("p := Point { x: 5 }\np.x");
        assert!(out.errors.is_none(), "errors: {:?}", out.errors);
        assert_eq!(out.output, "5");
    }

    #[test]
    fn conversion_in_session() {
        let mut s = Session::new("<test>");
        let out = s.eval("int(\"42\")");
        assert!(out.errors.is_none(), "errors: {:?}", out.errors);
        assert_eq!(out.output, ".some(42)");
        let out2 = s.eval("str(3.5)");
        assert!(out2.errors.is_none(), "errors: {:?}", out2.errors);
        assert_eq!(out2.output, "3.5");
    }

    #[test]
    fn std_math_in_session() {
        let mut s = Session::new("<test>");
        let out = s.eval("import std.math\nmath.abs(-7)");
        assert!(out.errors.is_none(), "errors: {:?}", out.errors);
        assert_eq!(out.output, "7");
    }
}
