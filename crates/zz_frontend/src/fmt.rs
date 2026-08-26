//! ZZ source code formatter.
//!
//! Produces reformatted source text from the AST according to ZZ style
//! conventions.  This module has **no** dependency on `tower-lsp` or
//! any other LSP crate, so it can be used by the CLI (`zz fmt`) as
//! well as the LSP server.

use crate::ast::*;

/// Configuration for the formatter.
#[derive(Debug, Clone)]
pub struct FormatConfig {
    /// Number of spaces per indentation level.
    pub indent_width: usize,
}

impl Default for FormatConfig {
    fn default() -> Self {
        Self { indent_width: 4 }
    }
}

/// Format a ZZ program and return the reformatted source as a single string.
pub fn format_program(program: &Program, source: &str, config: &FormatConfig) -> String {
    let mut ctx = FmtCtx {
        config,
        indent: 0,
        out: String::new(),
    };
    ctx.fmt_program(program, source);
    ctx.out
}

/// Returns true when `source` is already well-formatted.
pub fn is_formatted(program: &Program, source: &str, config: &FormatConfig) -> bool {
    format_program(program, source, config) == source
}

struct FmtCtx<'a> {
    config: &'a FormatConfig,
    indent: usize,
    out: String,
}

impl<'a> FmtCtx<'a> {
    fn write_indent(&mut self) {
        let width = self.indent * self.config.indent_width;
        self.out.push_str(&" ".repeat(width));
    }

    fn write_line(&mut self) {
        self.out.push('\n');
    }

    fn write_str(&mut self, s: &str) {
        self.out.push_str(s);
    }

    fn fmt_program(&mut self, program: &Program, source: &str) {
        for (i, stmt) in program.stmts.iter().enumerate() {
            if i > 0 {
                self.write_line();
                self.write_line(); // blank line between top-level items
            }
            self.fmt_stmt(stmt, source);
        }
        // Ensure trailing newline.
        if !self.out.ends_with('\n') {
            self.write_line();
        }
    }

    fn fmt_stmt(&mut self, stmt: &Stmt, source: &str) {
        match stmt {
            Stmt::Func {
                name,
                generics,
                params,
                ret,
                body,
                ..
            } => {
                self.write_indent();
                self.write_str("func ");
                self.write_str(&name.join("."));
                if !generics.is_empty() {
                    self.write_str("[");
                    for (i, g) in generics.iter().enumerate() {
                        if i > 0 {
                            self.write_str(", ");
                        }
                        self.write_str(&g.name);
                    }
                    self.write_str("]");
                }
                self.write_str("(");
                self.fmt_params(params, source);
                self.write_str(")");
                if let Some(ret_ty) = ret {
                    self.write_str(" -> ");
                    self.fmt_ty(ret_ty, source);
                }
                self.write_str(" ");
                self.fmt_block(body, source);
            }
            Stmt::Struct { name, fields, .. } => {
                self.write_indent();
                self.write_str("struct ");
                self.write_str(&name.join("."));
                self.write_str(" {");
                if fields.is_empty() {
                    self.write_str("}");
                } else {
                    self.write_line();
                    for (fname, fty) in fields {
                        self.write_indent();
                        self.write_str(&fname.name);
                        self.write_str(": ");
                        self.fmt_ty(fty, source);
                        self.write_str(",");
                        self.write_line();
                    }
                    self.write_indent();
                    self.write_str("}");
                }
            }
            Stmt::Decl {
                ty, name, value, ..
            } => {
                self.write_indent();
                if let Some(ty) = ty {
                    self.write_str(&name.name);
                    self.write_str(": ");
                    self.fmt_ty(ty, source);
                    self.write_str(" = ");
                } else {
                    self.write_str(&name.name);
                    self.write_str(" := ");
                }
                self.fmt_expr(value, source);
            }
            Stmt::Import { path, alias, .. } => {
                self.write_indent();
                self.write_str("import ");
                self.write_str(&path.join("."));
                if let Some(a) = alias {
                    self.write_str(" as ");
                    self.write_str(a);
                }
            }
            Stmt::Return { value, .. } => {
                self.write_indent();
                self.write_str("return");
                if let Some(v) = value {
                    self.write_str(" ");
                    self.fmt_expr(v, source);
                }
            }
            Stmt::For {
                var, iter, body, ..
            } => {
                self.write_indent();
                self.write_str("for ");
                self.write_str(&var.name);
                self.write_str(" in ");
                self.fmt_expr(iter, source);
                self.write_str(" ");
                self.fmt_block(body, source);
            }
            Stmt::Break { .. } => {
                self.write_indent();
                self.write_str("break");
            }
            Stmt::Continue { .. } => {
                self.write_indent();
                self.write_str("continue");
            }
            Stmt::Defer { expr, .. } => {
                self.write_indent();
                self.write_str("defer ");
                self.fmt_expr(expr, source);
            }
            Stmt::Assign { target, value, .. } => {
                self.write_indent();
                self.fmt_expr(target, source);
                self.write_str(" = ");
                self.fmt_expr(value, source);
            }
            Stmt::Expr(e) => {
                self.write_indent();
                self.fmt_expr(e, source);
            }
        }
    }

    fn fmt_block(&mut self, block: &Block, _source: &str) {
        if block.stmts.is_empty() {
            self.write_str("{}");
            return;
        }
        self.write_str("{");
        self.indent += 1;
        self.write_line();
        for (i, stmt) in block.stmts.iter().enumerate() {
            if i > 0 {
                self.write_line();
            }
            self.fmt_stmt(stmt, _source);
        }
        self.indent -= 1;
        self.write_line();
        self.write_indent();
        self.write_str("}");
    }

    fn fmt_params(&mut self, params: &[Param], source: &str) {
        for (i, param) in params.iter().enumerate() {
            if i > 0 {
                self.write_str(", ");
            }
            self.write_str(&param.name.name);
            if let Some(ty) = &param.ty {
                self.write_str(": ");
                self.fmt_ty(ty, source);
            }
        }
    }

    fn fmt_ty(&mut self, ty: &Ty, _source: &str) {
        match &ty.kind {
            TyKind::Int => self.write_str("int"),
            TyKind::Float => self.write_str("float"),
            TyKind::Bool => self.write_str("bool"),
            TyKind::Str => self.write_str("str"),
            TyKind::Unit => self.write_str("()"),
            TyKind::Named(name, generics) => {
                self.write_str(name);
                if !generics.is_empty() {
                    self.write_str("[");
                    for (i, g) in generics.iter().enumerate() {
                        if i > 0 {
                            self.write_str(", ");
                        }
                        self.fmt_ty(g, _source);
                    }
                    self.write_str("]");
                }
            }
            TyKind::Array(inner) => {
                self.fmt_ty(inner, _source);
                self.write_str("[]");
            }
            TyKind::Dict(key, val) => {
                self.write_str("{");
                self.fmt_ty(key, _source);
                self.write_str(": ");
                self.fmt_ty(val, _source);
                self.write_str("}");
            }
            TyKind::Option(inner) => {
                self.fmt_ty(inner, _source);
                self.write_str("?");
            }
            TyKind::Result(ok, err) => {
                self.fmt_ty(ok, _source);
                self.write_str("!");
                self.fmt_ty(err, _source);
            }
            TyKind::Tuple(elems) => {
                self.write_str("(");
                for (i, e) in elems.iter().enumerate() {
                    if i > 0 {
                        self.write_str(", ");
                    }
                    self.fmt_ty(e, _source);
                }
                self.write_str(")");
            }
            TyKind::Func(params, ret) => {
                self.write_str("\\(");
                for (i, p) in params.iter().enumerate() {
                    if i > 0 {
                        self.write_str(", ");
                    }
                    self.fmt_ty(p, _source);
                }
                self.write_str(") -> ");
                self.fmt_ty(ret, _source);
            }
            TyKind::Union(variants) => {
                for (i, v) in variants.iter().enumerate() {
                    if i > 0 {
                        self.write_str(" | ");
                    }
                    self.fmt_ty(v, _source);
                }
            }
        }
    }

    fn fmt_expr(&mut self, expr: &Expr, source: &str) {
        match expr {
            Expr::Int { value, .. } => self.write_str(&value.to_string()),
            Expr::Float { value, .. } => self.write_str(&value.to_string()),
            Expr::Str { value, .. } => {
                self.write_str("\"");
                self.write_str(value);
                self.write_str("\"");
            }
            Expr::Bool { value, .. } => {
                self.write_str(if *value { "true" } else { "false" });
            }
            Expr::Ident { name, .. } => self.write_str(name),
            Expr::Path { parts, .. } => self.write_str(&parts.join(".")),
            Expr::Unary { op, expr, .. } => {
                self.write_str(unop_str(*op));
                self.fmt_expr(expr, source);
            }
            Expr::Binary {
                op, left, right, ..
            } => {
                self.fmt_expr(left, source);
                self.write_str(" ");
                self.write_str(binop_str(*op));
                self.write_str(" ");
                self.fmt_expr(right, source);
            }
            Expr::Call {
                callee,
                args,
                named,
                ..
            } => {
                self.fmt_expr(callee, source);
                self.write_str("(");
                for (i, arg) in args.iter().enumerate() {
                    if i > 0 {
                        self.write_str(", ");
                    }
                    self.fmt_expr(arg, source);
                }
                for (i, (name, arg)) in named.iter().enumerate() {
                    if i > 0 || !args.is_empty() {
                        self.write_str(", ");
                    }
                    self.write_str(name);
                    self.write_str(": ");
                    self.fmt_expr(arg, source);
                }
                self.write_str(")");
            }
            Expr::If {
                cond, then, els, ..
            } => {
                self.write_str("if ");
                self.fmt_expr(cond, source);
                self.write_str(" ");
                self.fmt_block(then, source);
                if let Some(e) = els {
                    self.write_str(" else ");
                    self.fmt_expr(e, source);
                }
            }
            Expr::While { cond, body, .. } => {
                self.write_str("while ");
                self.fmt_expr(cond, source);
                self.write_str(" ");
                self.fmt_block(body, source);
            }
            Expr::Match {
                scrutinee, arms, ..
            } => {
                self.write_str("match ");
                self.fmt_expr(scrutinee, source);
                self.write_str(" {");
                self.indent += 1;
                self.write_line();
                for arm in arms {
                    self.write_indent();
                    self.fmt_pattern(&arm.pat, source);
                    self.write_str(" => ");
                    self.fmt_expr(&arm.body, source);
                    self.write_line();
                }
                self.indent -= 1;
                self.write_indent();
                self.write_str("}");
            }
            Expr::Block(b) => self.fmt_block(b, source),
            Expr::Array { elems, .. } => {
                self.write_str("[");
                for (i, e) in elems.iter().enumerate() {
                    if i > 0 {
                        self.write_str(", ");
                    }
                    self.fmt_expr(e, source);
                }
                self.write_str("]");
            }
            Expr::Dict { entries, .. } => {
                self.write_str("{");
                for (i, (k, v)) in entries.iter().enumerate() {
                    if i > 0 {
                        self.write_str(", ");
                    }
                    self.fmt_expr(k, source);
                    self.write_str(": ");
                    self.fmt_expr(v, source);
                }
                self.write_str("}");
            }
            Expr::Field { obj, name, .. } => {
                self.fmt_expr(obj, source);
                self.write_str(".");
                self.write_str(name);
            }
            Expr::Index { obj, index, .. } => {
                self.fmt_expr(obj, source);
                self.write_str("[");
                self.fmt_expr(index, source);
                self.write_str("]");
            }
            Expr::Slice {
                obj, start, end, ..
            } => {
                self.fmt_expr(obj, source);
                self.write_str("[");
                if let Some(s) = start {
                    self.fmt_expr(s, source);
                }
                self.write_str(":");
                if let Some(e) = end {
                    self.fmt_expr(e, source);
                }
                self.write_str("]");
            }
            Expr::Range { start, end, .. } => {
                self.fmt_expr(start, source);
                self.write_str("..");
                self.fmt_expr(end, source);
            }
            Expr::Try { expr, .. } => {
                self.fmt_expr(expr, source);
                self.write_str("?");
            }
            Expr::Fmt { parts, .. } => {
                self.write_str("\"");
                for part in parts {
                    match part {
                        FmtPart::Text(t) => self.write_str(t),
                        FmtPart::Expr(e, _) => {
                            self.write_str("{");
                            self.fmt_expr(e, source);
                            self.write_str("}");
                        }
                    }
                }
                self.write_str("\"");
            }
            Expr::Paren { expr, .. } => {
                self.write_str("(");
                self.fmt_expr(expr, source);
                self.write_str(")");
            }
            Expr::StructInit { name, fields, .. } => {
                self.write_str(name);
                self.write_str("{ ");
                for (i, (fname, fval)) in fields.iter().enumerate() {
                    if i > 0 {
                        self.write_str(", ");
                    }
                    self.write_str(fname);
                    self.write_str(": ");
                    self.fmt_expr(fval, source);
                }
                self.write_str(" }");
            }
            Expr::ListComp {
                body,
                var,
                iter,
                filter,
                ..
            } => {
                self.write_str("[");
                self.fmt_expr(body, source);
                self.write_str(" for ");
                self.write_str(&var.name);
                self.write_str(" in ");
                self.fmt_expr(iter, source);
                if let Some(f) = filter {
                    self.write_str(" if ");
                    self.fmt_expr(f, source);
                }
                self.write_str("]");
            }
            Expr::Closure { params, body, .. } => {
                self.write_str("|");
                self.fmt_params(params, source);
                self.write_str("| ");
                self.fmt_expr(body, source);
            }
            Expr::Variant { name, arg, .. } => {
                self.write_str(".");
                self.write_str(name);
                if let Some(a) = arg {
                    self.write_str("(");
                    self.fmt_expr(a, source);
                    self.write_str(")");
                }
            }
            Expr::IfLet {
                pat,
                value,
                then,
                els,
                ..
            } => {
                self.write_str("if let ");
                self.fmt_pattern(pat, source);
                self.write_str(" = ");
                self.fmt_expr(value, source);
                self.write_str(" ");
                self.fmt_block(then, source);
                if let Some(e) = els {
                    self.write_str(" else ");
                    self.fmt_expr(e, source);
                }
            }
        }
    }

    #[allow(clippy::only_used_in_recursion)]
    fn fmt_pattern(&mut self, pat: &Pattern, source: &str) {
        match pat {
            Pattern::Wildcard { .. } => self.write_str("_"),
            Pattern::Binding { name } => self.write_str(&name.name),
            Pattern::Literal { value, .. } => match value {
                Lit::Int(v) => self.write_str(&v.to_string()),
                Lit::Float(v) => self.write_str(&v.to_string()),
                Lit::Str(v) => {
                    self.write_str("\"");
                    self.write_str(v);
                    self.write_str("\"");
                }
                Lit::Bool(v) => {
                    self.write_str(if *v { "true" } else { "false" });
                }
            },
            Pattern::Variant { name, arg, .. } => {
                self.write_str(".");
                self.write_str(name);
                if let Some(a) = arg {
                    self.write_str("(");
                    self.fmt_pattern(a, source);
                    self.write_str(")");
                }
            }
        }
    }
}

fn unop_str(op: UnOp) -> &'static str {
    match op {
        UnOp::Neg => "-",
        UnOp::Pos => "+",
        UnOp::Not => "!",
    }
}

fn binop_str(op: BinOp) -> &'static str {
    match op {
        BinOp::Add => "+",
        BinOp::Sub => "-",
        BinOp::Mul => "*",
        BinOp::Div => "/",
        BinOp::Rem => "%",
        BinOp::Pow => "**",
        BinOp::Eq => "==",
        BinOp::Ne => "!=",
        BinOp::Lt => "<",
        BinOp::Gt => ">",
        BinOp::Le => "<=",
        BinOp::Ge => ">=",
        BinOp::And => "&&",
        BinOp::Or => "||",
        BinOp::Elvis => "?:",
    }
}

// ── Tests ────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parse;

    fn fmt(source: &str) -> String {
        let parsed = parse(source);
        format_program(&parsed.program, source, &FormatConfig::default())
    }

    #[test]
    fn format_func_decl() {
        let src = "func add(a:int,b:int)->int{return a+b}";
        let out = fmt(src);
        assert!(out.contains("func add("));
        assert!(out.contains("-> int"));
        assert!(out.contains("return a + b"));
    }

    #[test]
    fn format_struct() {
        let src = "struct Point{x:int,y:int}";
        let out = fmt(src);
        assert!(out.contains("struct Point {"));
        assert!(out.contains("x: int,"));
        assert!(out.contains("y: int,"));
    }

    #[test]
    fn format_let_binding() {
        let src = "x:=1+2";
        let out = fmt(src);
        assert!(out.contains("x := 1 + 2"));
    }

    #[test]
    fn format_import() {
        let src = "import std.io as console";
        let out = fmt(src);
        assert!(out.contains("import std.io as console"));
    }

    #[test]
    fn format_nested_indentation() {
        let src = "func f() -> int { if true { return 1 } return 0 }";
        let out = fmt(src);
        assert!(out.contains("    if true {"));
        assert!(out.contains("        return 1"));
    }

    #[test]
    fn format_for_loop() {
        let src = "for x in items { print(x) }";
        let out = fmt(src);
        assert!(out.contains("for x in items {"));
        assert!(out.contains("print(x)"));
    }

    #[test]
    fn format_return_with_value() {
        let src = "return 42";
        let out = fmt(src);
        assert!(out.contains("return 42"));
    }

    #[test]
    fn format_empty_block() {
        let src = "func f() {}";
        let out = fmt(src);
        assert!(out.contains("func f() {}"));
    }

    #[test]
    fn format_binary_precedence() {
        let src = "x:=1+2*3";
        let out = fmt(src);
        assert!(out.contains("1 + 2 * 3"));
    }

    #[test]
    fn format_if_else() {
        let src = "if x > 0{x} else {-x}";
        let out = fmt(src);
        assert!(out.contains("if x > 0 {"));
        assert!(out.contains("} else {"));
    }

    #[test]
    fn is_formatted_true_for_well_formatted() {
        let src = "x := 1\n";
        let parsed = parse(src);
        assert!(is_formatted(&parsed.program, src, &FormatConfig::default()));
    }

    #[test]
    fn is_formatted_false_for_unformatted() {
        let src = "x:=1";
        let parsed = parse(src);
        assert!(!is_formatted(
            &parsed.program,
            src,
            &FormatConfig::default()
        ));
    }
}
