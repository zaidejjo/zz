//! HIR → C lowering for the native AOT backend.
//!
//! Consumes the DCE-pruned [`TypedProgram`] and [`ReachableSet`] from
//! zz_hir and produces a single self-contained C translation unit (plus the
//! embedded runtime). A `cc` invocation compiles it to a native binary.

use std::collections::HashMap;

use zz_frontend::ast::{Block, Expr, FmtPart, Param, Stmt};

/// Result of lowering.
#[derive(Debug)]
pub struct LoweredC {
    /// The full generated C source (runtime + user code + main glue).
    pub source: String,
}

/// Scope-aware C identifier allocator (handles shadowing).
#[derive(Default)]
pub struct NameCtx {
    /// zz var name → stack of active C identifiers (innermost last).
    stack: HashMap<String, Vec<String>>,
    counter: usize,
}

impl NameCtx {
    fn new() -> Self {
        NameCtx {
            stack: HashMap::new(),
            counter: 0,
        }
    }

    fn enter(&mut self, name: &str) -> String {
        let cid = format!("v{}", self.counter);
        self.counter += 1;
        self.stack
            .entry(name.to_string())
            .or_default()
            .push(cid.clone());
        cid
    }

    fn lookup(&self, name: &str) -> Option<&str> {
        self.stack
            .get(name)
            .and_then(|s| s.last())
            .map(String::as_str)
    }

    fn leave(&mut self, name: &str) {
        if let Some(s) = self.stack.get_mut(name) {
            s.pop();
        }
    }
}

/// Mangle a zz qualified name to a C identifier.
pub fn mangle(name: &str) -> String {
    name.replace('.', "__")
}

/// Expressions that are pure leaves (no side effects, no captured value).
fn is_leaf_expr(e: &Expr) -> bool {
    matches!(
        e,
        Expr::Int { .. }
            | Expr::Float { .. }
            | Expr::Str { .. }
            | Expr::Bool { .. }
            | Expr::Ident { .. }
            | Expr::Path { .. }
            | Expr::Binary { .. }
            | Expr::Paren { .. }
    )
}

/// Expressions that must be captured into a temp before return.
fn needs_temp(e: &Expr) -> bool {
    matches!(
        e,
        Expr::Unary { .. }
            | Expr::If { .. }
            | Expr::Block(_)
            | Expr::Array { .. }
            | Expr::Range { .. }
            | Expr::Fmt { .. }
            | Expr::Tuple { .. }
            | Expr::Dict { .. }
            | Expr::Variant { .. }
            | Expr::Field { .. }
            | Expr::Index { .. }
            | Expr::Slice { .. }
    )
}

/// The lowering context.
pub struct Lowerer {
    reachable_funcs: std::collections::HashSet<String>,
    reachable_natives: std::collections::HashSet<String>,
    entry_main: String,
}

impl Lowerer {
    pub fn new(
        reachable_funcs: std::collections::HashSet<String>,
        reachable_natives: std::collections::HashSet<String>,
        entry_main: String,
    ) -> Self {
        Lowerer {
            reachable_funcs,
            reachable_natives,
            entry_main,
        }
    }

    pub fn lower(&self, tp: &zz_hir::TypedProgram) -> LoweredC {
        let mut funcs = String::new();
        let mut body = String::new();
        // One NameCtx shared across ALL top-level statements: top-level vars
        // remain visible across statements (like zz_main's single frame).
        let mut names = NameCtx::new();

        for stmt in tp.stmts() {
            match stmt {
                Stmt::Func {
                    name,
                    params,
                    body: b,
                    ..
                } => {
                    let fname = name.join(".");
                    if !self.reachable_funcs.contains(&fname) {
                        continue;
                    }
                    funcs.push_str(&self.emit_function(&fname, params, b));
                }
                Stmt::Struct { .. } | Stmt::Impl { .. } | Stmt::Import { .. } => {}
                other => {
                    let mut out = String::new();
                    self.emit_stmt(other, &mut names, &mut out, false);
                    body.push_str(&out);
                }
            }
        }

        let main_decl = if self.reachable_funcs.contains(&self.entry_main) {
            // main exists: call its stub from zz_call_main.
            "zz_call_into_main();".to_string()
        } else {
            String::new()
        };

        let source = format!(
            "{runtime_h}\n{runtime_c}\n\n// ---- generated code ----\n{funcs}\nvoid zz_main(void) {{\n{body}}}\n\nint zz_call_main(void) {{\n    {main_decl}\n    return 0;\n}}\n",
            runtime_h = crate::RUNTIME_H,
            runtime_c = crate::RUNTIME_C,
            funcs = funcs,
            body = body,
            main_decl = main_decl,
        );
        // runtime.c includes "runtime.h", but it is inlined above; strip the
        // include directive so the single translation unit compiles.
        let source = source.replace("#include \"runtime.h\"\n", "");

        // If main is reachable, append a stub that calls it (runtime does
        // not know the symbol; we emit a forward decl + call here).
        let with_main = if self.reachable_funcs.contains(&self.entry_main) {
            let m = format!("zz_fn_{}", mangle(&self.entry_main));
            format!(
                "\nstatic void zz_call_into_main(void);\nstatic void zz_call_into_main(void) {{ zz_value _r = {m}(NULL, 0); (void)_r; }}\n"
            )
        } else {
            String::new()
        };
        // Insert before the `int zz_call_main` body.
        let source = source.replacen(
            "int zz_call_main(void) {",
            &format!("{with_main}\nint zz_call_main(void) {{"),
            1,
        );

        LoweredC { source }
    }

    fn emit_function(&self, fname: &str, params: &[Param], block: &Block) -> String {
        let cname = format!("zz_fn_{}", mangle(fname));
        let mut o = String::new();
        o.push_str(&format!(
            "static zz_value {cname}(zz_value *args, size_t argc) {{\n"
        ));
        o.push_str("    (void)argc;\n");
        let mut names = NameCtx::new();
        for (i, p) in params.iter().enumerate() {
            let cid = names.enter(&p.name.name);
            o.push_str(&format!("    zz_value {cid} = args[{i}];\n"));
        }
        let mut body_out = String::new();
        self.emit_block(block, &mut names, &mut body_out);
        o.push_str(&body_out);
        // Implicit return: last statement expression is the function value.
        if self.last_stmt_value(block, &mut names, &mut o).is_none() {
            o.push_str("    return zz_unit();\n");
        }
        o.push_str("}\n\n");
        o
    }

    fn emit_block(&self, block: &Block, names: &mut NameCtx, out: &mut String) {
        let n = block.stmts.len();
        for (i, stmt) in block.stmts.iter().enumerate() {
            let is_tail = i == n - 1;
            self.emit_stmt(stmt, names, out, is_tail);
        }
    }

    /// The last statement of a function body — if it's an expression, that
    /// becomes the implicit return value. Handles tail calls, plain
    /// expression tails, and tail if/else (returning from each branch).
    fn last_stmt_value(&self, block: &Block, names: &mut NameCtx, out: &mut String) -> Option<()> {
        if let Some(last) = block.stmts.last() {
            if let Stmt::Expr(e) = last {
                if matches!(e, Expr::If { .. }) {
                    self.emit_tail_expr(e, names, out);
                    return Some(());
                }
                // Tail call/compound was captured into __tail by emit_block.
                if let Some(tmp) = names.stack.get("__tail").and_then(|s| s.last()).cloned() {
                    out.push_str(&format!("    return {tmp};\n"));
                    return Some(());
                }
                // Pure leaf tail: emit directly (rarely reached).
                let val = self.emit_expr(e, names, out);
                out.push_str(&format!("    return {val};\n"));
                return Some(());
            }
        }
        None
    }

    /// Emit an expression in return position, emitting `return <val>;`.
    /// If/else tails return from each branch.
    fn emit_tail_expr(&self, e: &Expr, names: &mut NameCtx, out: &mut String) {
        match e {
            Expr::If {
                cond, then, els, ..
            } => {
                let c = self.emit_expr(cond, names, out);
                out.push_str(&format!("    if (zz_truthy({c})) {{\n"));
                if self.last_stmt_value(then, names, out).is_none() {
                    out.push_str("        return zz_unit();\n");
                }
                out.push_str("    } else {\n");
                if let Some(el) = els {
                    match el.as_ref() {
                        Expr::Block(b) => {
                            if self.last_stmt_value(b, names, out).is_none() {
                                out.push_str("        return zz_unit();\n");
                            }
                        }
                        other => self.emit_tail_expr(other, names, out),
                    }
                } else {
                    out.push_str("        return zz_unit();\n");
                }
                out.push_str("    }\n");
            }
            _ => {
                let val = self.emit_expr(e, names, out);
                out.push_str(&format!("    return {val};\n"));
            }
        }
    }

    fn emit_stmt(&self, stmt: &Stmt, names: &mut NameCtx, out: &mut String, is_tail: bool) {
        match stmt {
            Stmt::Decl { name, value, .. } => {
                let cid = names.enter(&name.name);
                let val = self.emit_expr(value, names, out);
                out.push_str(&format!("    zz_value {cid} = {val};\n"));
            }
            Stmt::Assign { target, value, .. } => {
                let val = self.emit_expr(value, names, out);
                match target {
                    Expr::Ident { name, .. } => {
                        if let Some(cid) = names.lookup(name.as_str()) {
                            out.push_str(&format!("    zz_assign(&{cid}, {val});\n"));
                        }
                    }
                    Expr::Path { parts, .. } => {
                        let joined = parts.join(".");
                        if let Some(cid) = names.lookup(&joined) {
                            out.push_str(&format!("    zz_assign(&{cid}, {val});\n"));
                        }
                    }
                    _ => {}
                }
            }
            Stmt::Expr(e) => {
                // Tail expressions in branch/function bodies are captured
                // into a temp so enclosing `return` can reuse the value;
                // the temp assignment still executes side effects.
                if is_tail {
                    if matches!(e, Expr::If { .. }) {
                        // If/else tails are emitted as branches by
                        // last_stmt_value (each arm returns).
                    } else if matches!(e, Expr::Call { .. }) || needs_temp(e) {
                        let tmp = format!("__tail{}", names.counter);
                        names.counter += 1;
                        let val = self.emit_expr(e, names, out);
                        out.push_str(&format!("    zz_value {tmp} = {val};\n"));
                        names
                            .stack
                            .entry("__tail".to_string())
                            .or_default()
                            .push(tmp);
                    }
                    // leaf tails skipped (no side effect)
                } else if matches!(e, Expr::Call { .. }) {
                    let val = self.emit_expr(e, names, out);
                    out.push_str(&format!("    (void)({val});\n"));
                } else if !is_leaf_expr(e) {
                    let _ = self.emit_expr(e, names, out);
                }
            }
            Stmt::Return { value, .. } => match value {
                Some(v) => {
                    let val = self.emit_expr(v, names, out);
                    out.push_str(&format!("    return {val};\n"));
                }
                None => out.push_str("    return zz_unit();\n"),
            },
            Stmt::For {
                vars, iter, body, ..
            } => {
                self.emit_for(vars, iter, body, names, out);
            }
            Stmt::Break { .. } => out.push_str("    break;\n"),
            Stmt::Continue { .. } => out.push_str("    continue;\n"),
            Stmt::Defer { .. } | Stmt::Destructure { .. } => {
                out.push_str("    // unsupported statement skipped\n");
            }
            Stmt::Func { .. } | Stmt::Struct { .. } | Stmt::Impl { .. } | Stmt::Import { .. } => {}
        }
    }

    fn emit_for(
        &self,
        vars: &[zz_frontend::ast::Ident],
        iter: &Expr,
        body: &Block,
        names: &mut NameCtx,
        out: &mut String,
    ) {
        if let Expr::Range { start, end, .. } = iter {
            let sv = self.emit_expr(start, names, out);
            let ev = self.emit_expr(end, names, out);
            let v = &vars[0].name;
            let cid = names.enter(v);
            let s = format!(
                "{{ zz_value _s = {sv}; zz_value _e = {ev};\n    \
                 if (_s.tag == ZZ_INT && _e.tag == ZZ_INT) {{\n        \
                 for (int64_t {cid}_i = _s.i; {cid}_i < _e.i; {cid}_i++) {{\n            \
                 zz_value {cid} = zz_int({cid}_i);\n"
            );
            out.push_str(&s);
            // Loop body is never a function tail.
            for bstmt in &body.stmts {
                self.emit_stmt(bstmt, names, out, false);
            }
            out.push_str("        }\n    }\n}\n");
            names.leave(v);
        } else {
            out.push_str("    // for over non-range unsupported\n");
        }
    }

    fn emit_expr(&self, e: &Expr, names: &mut NameCtx, out: &mut String) -> String {
        match e {
            Expr::Int { value, .. } => format!("zz_int({value})"),
            Expr::Float { value, .. } => {
                let s = if *value == (*value).floor() && (*value).abs() < 1e15 {
                    format!("{value:.1}")
                } else {
                    format!("{value}")
                };
                format!("zz_float({s})")
            }
            Expr::Bool { value, .. } => {
                format!("zz_bool({})", if *value { "true" } else { "false" })
            }
            Expr::Str { value, .. } => self.emit_str_literal(value),
            Expr::Fmt { parts, .. } => {
                // Full fstring: build via runtime interp. MVP: only literal
                // text handled; embedded exprs appended as str(values).
                let mut acc = String::from("zz_str_static(\"\")");
                for part in parts {
                    match part {
                        FmtPart::Text(t) => {
                            let lit = self.emit_str_literal(t);
                            acc = format!("zz_binop_cat({acc}, {lit})");
                        }
                        FmtPart::Expr(inner, _) => {
                            let v = self.emit_expr(inner, names, out);
                            acc = format!("zz_binop_cat_str({acc}, {v})");
                        }
                    }
                }
                acc
            }
            Expr::Ident { name, .. } => match names.lookup(name) {
                Some(cid) => format!("zz_clone({cid})"),
                None => "zz_unit()".to_string(),
            },
            Expr::Path { parts, .. } => {
                let joined = parts.join(".");
                if let Some(cid) = names.lookup(&joined) {
                    format!("zz_clone({cid})")
                } else {
                    "zz_unit()".to_string()
                }
            }
            Expr::Paren { expr, .. } => self.emit_expr(expr, names, out),
            Expr::Unary { op, expr, .. } => {
                let v = self.emit_expr(expr, names, out);
                match op {
                    zz_frontend::ast::UnOp::Neg => format!("zz_neg({v})"),
                    zz_frontend::ast::UnOp::Pos => v,
                    zz_frontend::ast::UnOp::Not => format!("zz_not({v})"),
                }
            }
            Expr::Binary {
                op, left, right, ..
            } => {
                let l = self.emit_expr(left, names, out);
                let r = self.emit_expr(right, names, out);
                match op {
                    zz_frontend::ast::BinOp::And => {
                        format!("zz_bool(zz_truthy({l}) && zz_truthy({r}))")
                    }
                    zz_frontend::ast::BinOp::Or => {
                        format!("zz_bool(zz_truthy({l}) || zz_truthy({r}))")
                    }
                    zz_frontend::ast::BinOp::Elvis => {
                        format!("(zz_truthy({l}) ? ({l}) : ({r}))")
                    }
                    _ => {
                        let cop = match op {
                            zz_frontend::ast::BinOp::Add => "ZZOP_ADD",
                            zz_frontend::ast::BinOp::Sub => "ZZOP_SUB",
                            zz_frontend::ast::BinOp::Mul => "ZZOP_MUL",
                            zz_frontend::ast::BinOp::Div => "ZZOP_DIV",
                            zz_frontend::ast::BinOp::Rem => "ZZOP_REM",
                            zz_frontend::ast::BinOp::Pow => "ZZOP_POW",
                            zz_frontend::ast::BinOp::Eq => "ZZOP_EQ",
                            zz_frontend::ast::BinOp::Ne => "ZZOP_NE",
                            zz_frontend::ast::BinOp::Lt => "ZZOP_LT",
                            zz_frontend::ast::BinOp::Gt => "ZZOP_GT",
                            zz_frontend::ast::BinOp::Le => "ZZOP_LE",
                            zz_frontend::ast::BinOp::Ge => "ZZOP_GE",
                            _ => "ZZOP_ADD",
                        };
                        format!("zz_binop({cop}, {l}, {r})")
                    }
                }
            }
            Expr::Call { callee, args, .. } => self.emit_call(callee, args, names, out),
            Expr::If {
                cond, then, els, ..
            } => {
                let c = self.emit_expr(cond, names, out);
                out.push_str(&format!("    if (zz_truthy({c})) {{\n"));
                self.emit_block(then, names, out);
                if let Some(el) = els {
                    out.push_str("    } else {\n");
                    // else branch as expression: emit statements then unit
                    self.emit_block(get_block(el), names, out);
                    out.push_str("    }\n");
                } else {
                    out.push_str("    }\n");
                }
                "zz_unit()".to_string()
            }
            Expr::Block(b) => {
                self.emit_block(b, names, out);
                "zz_unit()".to_string()
            }
            Expr::Range { start, end, .. } => {
                let s = self.emit_expr(start, names, out);
                let en = self.emit_expr(end, names, out);
                format!("zz_range_build({s}, {en})")
            }
            _ => "zz_unit()".to_string(),
        }
    }

    fn emit_call(
        &self,
        callee: &Expr,
        args: &[Expr],
        names: &mut NameCtx,
        out: &mut String,
    ) -> String {
        let cname = match callee {
            Expr::Ident { name, .. } => Some(name.clone()),
            Expr::Path { parts, .. } => Some(parts.join(".")),
            _ => None,
        };
        let cname = match cname {
            Some(n) => n,
            None => return "zz_unit()".to_string(),
        };

        let mut arg_items: Vec<String> = Vec::new();
        for a in args {
            arg_items.push(self.emit_expr(a, names, out));
        }

        // Only lower natives that survived DCE reachability AND have a C
        // runtime implementation.
        let cname_for_native = cname.clone();
        // A native may be bound under `std.io.println` (stdlib_funcs) while
        // the source calls `io.println` (namespace-registered). Match either.
        let std_name = format!("std.{cname_for_native}");
        let native_rt = if self.reachable_natives.contains(&cname_for_native)
            || self.reachable_natives.contains(&std_name)
        {
            native_impl(&cname_for_native)
        } else {
            None
        };
        if let Some(impl_name) = native_rt {
            return match arg_items.len() {
                1 => {
                    let a = &arg_items[0];
                    format!("zz_call_native1({impl_name}, {a})")
                }
                0 => format!("zz_call_native0({impl_name})"),
                _ => "zz_unit()".to_string(),
            };
        }

        if self.reachable_funcs.contains(&cname) {
            let cf = format!("zz_fn_{}", mangle(&cname));
            if arg_items.is_empty() {
                return format!("{cf}(NULL, 0)");
            }
            return format!(
                "({cf}((zz_value[]){{ {joined} }}, {n}))",
                joined = arg_items.join(", "),
                n = args.len()
            );
        }

        "zz_unit()".to_string()
    }

    fn emit_str_literal(&self, s: &str) -> String {
        let mut o = String::from("\"");
        for c in s.chars() {
            match c {
                '"' => o.push_str("\\\""),
                '\\' => o.push_str("\\\\"),
                '\n' => o.push_str("\\n"),
                '\r' => o.push_str("\\r"),
                '\t' => o.push_str("\\t"),
                c if (c as u32) < 32 => o.push_str(&format!("\\x{:02x}", c as u32)),
                c => o.push(c),
            }
        }
        o.push('"');
        format!("zz_str_static({o})")
    }
}

/// Get a Block from an else-expression (or empty block).
fn get_block(e: &Expr) -> &Block {
    match e {
        Expr::Block(b) => b,
        _ => {
            // Wrap expression in an implicit block reference is not possible;
            // return a static empty block via leak — MVPs accept unit.
            static EMPTY: Block = Block {
                stmts: Vec::new(),
                span: zz_frontend::span::Span { start: 0, end: 0 },
            };
            &EMPTY
        }
    }
}

/// Map a zz native qualified name to its C runtime implementation name.
fn native_impl(name: &str) -> Option<&'static str> {
    match name {
        "io.println" | "std.io.println" => Some("zz_io_println"),
        "io.print" | "std.io.print" => Some("zz_io_print"),
        "math.pow" | "std.math.pow" => Some("zz_math_pow"),
        _ => None,
    }
}

/// Whether a reachable native has a C runtime implementation.
pub fn native_supported(name: &str) -> bool {
    native_impl(name).is_some()
}
