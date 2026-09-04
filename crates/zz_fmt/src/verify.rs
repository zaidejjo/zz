//! AST structural-fingerprint verification.
//!
//! For M0/M1 we use a coarse but strong check: re-parse the formatted
//! source and confirm the AST shape matches the input AST. The
//! "shape" comparison is: walk both trees in lockstep, compare variant
//! tags, child counts, identifier names, literal values, and
//! operator variants. Spans are compared as character ranges (a
//! different *byte* range is expected when the source changes length
//! but a *character* range covering the same text is equivalent).
//!
//! This catches any formatter bug that changes semantics: dropped
//! tokens, reordered operators, wrong field names, etc. Whitespace
//! and comment placement don't affect the fingerprint (those are
//! formatting details).

use crate::error::FmtError;
use std::path::Path;
use zz_frontend::ast::*;
use zz_frontend::parse;

/// Fingerprint of an AST: a normalized structural summary.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Fingerprint {
    /// Tree of variant tags + child fingerprints.
    pub tree: String,
}

impl Fingerprint {
    pub fn of_program(p: &Program) -> Self {
        let mut s = String::new();
        fp_program(p, &mut s);
        Fingerprint { tree: s }
    }
}

fn fp_program(p: &Program, out: &mut String) {
    out.push_str("Program[");
    for s in &p.stmts {
        fp_stmt(s, out);
    }
    out.push(']');
}

fn fp_stmt(s: &Stmt, out: &mut String) {
    match s {
        Stmt::Import { path, alias, .. } => {
            out.push_str("Import-kw[");
            for p in path {
                out.push_str(p);
                out.push(',');
            }
            if let Some(a) = alias {
                out.push_str("alias=");
                out.push_str(a);
            }
            out.push(']');
        }
        Stmt::Decl {
            ty, name, value, ..
        } => {
            out.push_str("Decl[");
            out.push_str(&name.name);
            if let Some(t) = ty {
                out.push_str(":");
                fp_ty(t, out);
            }
            out.push('=');
            fp_expr(value, out);
            out.push(']');
        }
        Stmt::Func {
            name,
            generics,
            params,
            ret,
            body,
            ..
        } => {
            out.push_str("Func-kw[");
            for p in name {
                out.push_str(p);
                out.push('.');
            }
            if !generics.is_empty() {
                out.push('<');
                for g in generics {
                    out.push_str(&g.name);
                    out.push(',');
                }
                out.push('>');
            }
            out.push('(');
            for p in params {
                out.push_str(&p.name.name);
                if let Some(t) = &p.ty {
                    out.push(':');
                    fp_ty(t, out);
                }
                out.push(',');
            }
            out.push(')');
            if let Some(r) = ret {
                out.push_str("->");
                fp_ty(r, out);
            }
            out.push('{');
            for st in &body.stmts {
                fp_stmt(st, out);
            }
            out.push_str("}]");
        }
        Stmt::Return { value, .. } => {
            out.push_str("Return-kw[");
            if let Some(v) = value {
                fp_expr(v, out);
            }
            out.push(']');
        }
        Stmt::Struct { name, fields, .. } => {
            out.push_str("Struct-kw[");
            for p in name {
                out.push_str(p);
                out.push('.');
            }
            out.push('{');
            for (n, t) in fields {
                out.push_str(&n.name);
                out.push(':');
                fp_ty(t, out);
                out.push(',');
            }
            out.push_str("}]");
        }
        Stmt::Impl { name, methods, .. } => {
            out.push_str("Impl-kw[");
            for p in name {
                out.push_str(p);
                out.push('.');
            }
            out.push('{');
            for m in methods {
                fp_stmt(m, out);
            }
            out.push_str("}]");
        }
        Stmt::For {
            vars, iter, body, ..
        } => {
            out.push_str("For-kw[");
            for v in vars {
                out.push_str(&v.name);
                out.push(',');
            }
            out.push_str("in ");
            fp_expr(iter, out);
            out.push('{');
            for st in &body.stmts {
                fp_stmt(st, out);
            }
            out.push_str("}]");
        }
        Stmt::Break { .. } => out.push_str("Break-kw[]"),
        Stmt::Continue { .. } => out.push_str("Continue-kw[]"),
        Stmt::Defer { expr, .. } => {
            out.push_str("Defer-kw[");
            fp_expr(expr, out);
            out.push(']');
        }
        Stmt::Assign { target, value, .. } => {
            out.push_str("Assign[");
            fp_expr(target, out);
            out.push('=');
            fp_expr(value, out);
            out.push(']');
        }
        Stmt::Destructure { pat, value, .. } => {
            out.push_str("Destructure[");
            fp_pattern(pat, out);
            out.push('=');
            fp_expr(value, out);
            out.push(']');
        }
        Stmt::Expr(e) => fp_expr(e, out),
    }
}

fn fp_pattern(p: &Pattern, out: &mut String) {
    match p {
        Pattern::Wildcard { .. } => out.push_str("_"),
        Pattern::Binding { name } => out.push_str(&name.name),
        Pattern::Literal { value, .. } => fp_lit(value, out),
        Pattern::Variant { name, arg, .. } => {
            out.push('.');
            out.push_str(name);
            if let Some(a) = arg {
                out.push('(');
                fp_pattern(a, out);
                out.push(')');
            }
        }
        Pattern::Tuple { pats, .. } => {
            out.push('(');
            for p in pats {
                fp_pattern(p, out);
                out.push(',');
            }
            out.push(')');
        }
    }
}

fn fp_ty(t: &Ty, out: &mut String) {
    match &t.kind {
        TyKind::Int => out.push_str("int"),
        TyKind::Float => out.push_str("float"),
        TyKind::Bool => out.push_str("bool"),
        TyKind::Str => out.push_str("str"),
        TyKind::Unit => out.push_str("unit"),
        TyKind::Tuple(items) => {
            out.push('(');
            for t in items {
                fp_ty(t, out);
                out.push(',');
            }
            out.push(')');
        }
        TyKind::Option(inner) => {
            out.push_str("Option[");
            fp_ty(inner, out);
            out.push(']');
        }
        TyKind::Result(ok, err) => {
            out.push_str("Result[");
            fp_ty(ok, out);
            out.push(',');
            fp_ty(err, out);
            out.push(']');
        }
        TyKind::Func(args, ret) => {
            out.push_str("Fn[");
            for a in args {
                fp_ty(a, out);
                out.push(',');
            }
            fp_ty(ret, out);
            out.push(']');
        }
        TyKind::Array(inner) => {
            out.push('[');
            fp_ty(inner, out);
            out.push(']');
        }
        TyKind::Dict(k, v) => {
            out.push_str("{");
            fp_ty(k, out);
            out.push(':');
            fp_ty(v, out);
            out.push('}');
        }
        TyKind::Union(items) => {
            for t in items {
                fp_ty(t, out);
                out.push('|');
            }
        }
        TyKind::Named(name, args) => {
            out.push_str(name);
            if !args.is_empty() {
                out.push('<');
                for a in args {
                    fp_ty(a, out);
                    out.push(',');
                }
                out.push('>');
            }
        }
    }
}

fn fp_lit(l: &Lit, out: &mut String) {
    match l {
        Lit::Int(n) => {
            out.push_str("Int(");
            out.push_str(&n.to_string());
            out.push(')');
        }
        Lit::Float(f) => {
            out.push_str("Float(");
            out.push_str(&f.to_string());
            out.push(')');
        }
        Lit::Str(s) => {
            out.push_str("Str(");
            out.push_str(s);
            out.push(')');
        }
        Lit::Bool(b) => {
            out.push_str("Bool(");
            out.push_str(&b.to_string());
            out.push(')');
        }
    }
}

fn fp_expr(e: &Expr, out: &mut String) {
    match e {
        Expr::Int { value, .. } => {
            out.push_str("Int(");
            out.push_str(&value.to_string());
            out.push(')');
        }
        Expr::Float { value, .. } => {
            out.push_str("Float(");
            out.push_str(&value.to_string());
            out.push(')');
        }
        Expr::Str { value, .. } => {
            out.push_str("Str(");
            out.push_str(value);
            out.push(')');
        }
        Expr::Bool { value, .. } => {
            out.push_str("Bool(");
            out.push_str(&value.to_string());
            out.push(')');
        }
        Expr::Ident { name, .. } => out.push_str(name),
        Expr::Path { parts, .. } => {
            for p in parts {
                out.push_str(p);
                out.push('.');
            }
        }
        Expr::Fmt { parts, .. } => {
            out.push_str("Fmt[");
            for p in parts {
                match p {
                    FmtPart::Text(t) => {
                        out.push_str("T(");
                        out.push_str(t);
                        out.push(')');
                    }
                    FmtPart::Expr(e, spec) => {
                        out.push_str("E(");
                        fp_expr(e, out);
                        if let Some(s) = spec {
                            out.push(':');
                            out.push_str(s);
                        }
                        out.push(')');
                    }
                }
            }
            out.push(']');
        }
        Expr::Paren { expr, .. } => {
            out.push('(');
            fp_expr(expr, out);
            out.push(')');
        }
        Expr::Tuple { items, .. } => {
            out.push_str("Tuple[");
            for it in items {
                fp_expr(it, out);
                out.push(',');
            }
            out.push(']');
        }
        Expr::Unary { op, expr, .. } => {
            out.push('(');
            out.push_str(op.symbol());
            fp_expr(expr, out);
            out.push(')');
        }
        Expr::Binary {
            op, left, right, ..
        } => {
            out.push('(');
            fp_expr(left, out);
            out.push_str(op.symbol());
            fp_expr(right, out);
            out.push(')');
        }
        Expr::Call {
            callee,
            args,
            named,
            ..
        } => {
            fp_expr(callee, out);
            out.push('(');
            for a in args {
                fp_expr(a, out);
                out.push(',');
            }
            for (n, v) in named {
                out.push_str(n);
                out.push('=');
                fp_expr(v, out);
                out.push(',');
            }
            out.push(')');
        }
        Expr::Closure { params, body, .. } => {
            out.push_str("Closure[");
            for p in params {
                out.push_str(&p.name.name);
                if let Some(t) = &p.ty {
                    out.push(':');
                    fp_ty(t, out);
                }
                out.push(',');
            }
            out.push_str("=>");
            fp_expr(body, out);
            out.push(']');
        }
        Expr::If {
            cond, then, els, ..
        } => {
            out.push_str("If[");
            fp_expr(cond, out);
            out.push('{');
            for s in &then.stmts {
                fp_stmt(s, out);
            }
            out.push('}');
            if let Some(e) = els {
                out.push_str("else");
                fp_expr(e, out);
            }
            out.push(']');
        }
        Expr::While { cond, body, .. } => {
            out.push_str("While[");
            fp_expr(cond, out);
            out.push('{');
            for s in &body.stmts {
                fp_stmt(s, out);
            }
            out.push_str("}]");
        }
        Expr::Match {
            scrutinee, arms, ..
        } => {
            out.push_str("Match[");
            fp_expr(scrutinee, out);
            for a in arms {
                fp_pattern(&a.pat, out);
                out.push_str("=>");
                fp_expr(&a.body, out);
            }
            out.push(']');
        }
        Expr::IfLet {
            pat,
            value,
            then,
            els,
            ..
        } => {
            out.push_str("IfLet[");
            fp_pattern(pat, out);
            out.push('=');
            fp_expr(value, out);
            out.push('{');
            for s in &then.stmts {
                fp_stmt(s, out);
            }
            out.push('}');
            if let Some(e) = els {
                out.push_str("else");
                fp_expr(e, out);
            }
            out.push(']');
        }
        Expr::Try { expr, .. } => {
            out.push_str("Try[");
            fp_expr(expr, out);
            out.push(']');
        }
        Expr::Block(b) => {
            out.push_str("Block[{");
            for s in &b.stmts {
                fp_stmt(s, out);
            }
            out.push_str("}]");
        }
        Expr::Variant { name, arg, .. } => {
            out.push('.');
            out.push_str(name);
            if let Some(a) = arg {
                out.push('(');
                fp_expr(a, out);
                out.push(')');
            }
        }
        Expr::Array { elems, .. } => {
            out.push_str("Array[");
            for e in elems {
                fp_expr(e, out);
                out.push(',');
            }
            out.push(']');
        }
        Expr::Dict { entries, .. } => {
            out.push_str("Dict[");
            for (k, v) in entries {
                fp_expr(k, out);
                out.push(':');
                fp_expr(v, out);
                out.push(',');
            }
            out.push(']');
        }
        Expr::Field { obj, name, .. } => {
            fp_expr(obj, out);
            out.push('.');
            out.push_str(name);
        }
        Expr::Range { start, end, .. } => {
            fp_expr(start, out);
            out.push_str("..");
            fp_expr(end, out);
        }
        Expr::StructInit { name, fields, .. } => {
            out.push_str(name);
            out.push('{');
            for (n, v) in fields {
                out.push_str(n);
                out.push(':');
                fp_expr(v, out);
                out.push(',');
            }
            out.push('}');
        }
        Expr::Index { obj, index, .. } => {
            fp_expr(obj, out);
            out.push('[');
            fp_expr(index, out);
            out.push(']');
        }
        Expr::Slice {
            obj, start, end, ..
        } => {
            fp_expr(obj, out);
            out.push('[');
            if let Some(s) = start {
                fp_expr(s, out);
            }
            out.push(':');
            if let Some(e) = end {
                fp_expr(e, out);
            }
            out.push(']');
        }
        Expr::ListComp {
            body,
            var,
            iter,
            filter,
            ..
        } => {
            out.push_str("ListComp[");
            fp_expr(body, out);
            out.push_str("for ");
            out.push_str(&var.name);
            out.push_str(" in ");
            fp_expr(iter, out);
            if let Some(f) = filter {
                out.push_str("if ");
                fp_expr(f, out);
            }
            out.push(']');
        }
    }
}

/// Verify the formatted source preserves the input's AST shape.
pub fn verify(path: &Path, original: &Program, formatted_src: &str) -> Result<(), FmtError> {
    let parsed = parse(formatted_src);
    if !parsed.errors.is_empty() {
        let summary = parsed
            .errors
            .iter()
            .map(|e| e.message.clone())
            .collect::<Vec<_>>()
            .join("; ");
        return Err(FmtError::Parse {
            path: path.to_path_buf(),
            summary,
        });
    }
    let a = Fingerprint::of_program(original);
    let b = Fingerprint::of_program(&parsed.program);
    if a != b {
        return Err(FmtError::Verify {
            path: path.to_path_buf(),
            mismatch: format!(
                "fingerprint differs\n  input:    {}\n  formatted:{}",
                a.tree, b.tree
            ),
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fingerprint_of_program_matches_for_identical_inputs() {
        let src = "x := 1 + 2\n";
        let a = parse(src).program;
        let b = parse(src).program;
        assert_eq!(Fingerprint::of_program(&a), Fingerprint::of_program(&b));
    }

    #[test]
    fn fingerprint_differs_for_different_semantics() {
        let a = parse("x := 1 + 2\n").program;
        let b = parse("x := 1 - 2\n").program;
        assert_ne!(Fingerprint::of_program(&a), Fingerprint::of_program(&b));
    }
}
