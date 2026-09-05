//! AST structural-fingerprint verification.
//!
//! M2 verifies three independent properties of the formatted output:
//!
//! 1. **AST fingerprint**: re-parse the formatted source and compare a
//!    structural fingerprint of both ASTs (variant tags, child counts,
//!    identifiers, literal values, operator kinds). Spans are ignored.
//!
//! 2. **Significant-token equivalence**: re-lex the formatted source
//!    and compare the sequence of significant tokens (text content,
//!    skipping trivia). This catches any formatter bug that altered
//!    punctuation, identifier spelling, or token kinds even if the
//!    AST shape still matched by coincidence.
//!
//! 3. **Trivia integrity**: every comment that appeared in the input
//!    must appear, byte-identical, in the formatted output. This
//!    catches accidental loss of comments.
//!
//! All three checks are required; if any fails, the formatter refuses
//! to write the output. Together they implement **Zero Structural
//! Drift**: the formatter can never silently change meaning.

use crate::error::FmtError;
use std::path::Path;
use zz_frontend::ast::*;
use zz_frontend::lexer::lex;
use zz_frontend::parse;
use zz_frontend::token::TokenKind;

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
                out.push(':');
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

/// Lex `source` and return the sequence of significant-token texts
/// (whitespace, comments, and StmtEnd newlines stripped).
fn significant_token_sequence(source: &str) -> Vec<String> {
    let lexed = lex(source);
    let mut seq = Vec::new();
    for t in &lexed.tokens {
        match t.kind {
            TokenKind::Eof => break,
            // StmtEnd newlines are pure trivia for verification purposes.
            TokenKind::StmtEnd => continue,
            _ => {
                let s = t.span.start as usize;
                let e = (t.span.end as usize).min(source.len());
                if e > s {
                    seq.push(source[s..e].to_string());
                }
            }
        }
    }
    seq
}

/// Extract every comment substring (`// ...`, `/// ...`, `/* ... */`)
/// from `source`. Comments are returned in source order.
fn extract_comments(source: &str) -> Vec<String> {
    let bytes = source.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        if i + 1 < bytes.len() && bytes[i] == b'/' && bytes[i + 1] == b'/' {
            // Line comment to end of line.
            let start = i;
            let mut j = i + 2;
            while j < bytes.len() && bytes[j] != b'\n' {
                j += 1;
            }
            out.push(source[start..j].to_string());
            i = j;
        } else if i + 1 < bytes.len() && bytes[i] == b'/' && bytes[i + 1] == b'*' {
            let start = i;
            let mut depth = 1u32;
            let mut j = i + 2;
            while j < bytes.len() && depth > 0 {
                if j + 1 < bytes.len() && bytes[j] == b'/' && bytes[j + 1] == b'*' {
                    depth += 1;
                    j += 2;
                } else if j + 1 < bytes.len() && bytes[j] == b'*' && bytes[j + 1] == b'/' {
                    depth -= 1;
                    j += 2;
                } else {
                    j += 1;
                }
            }
            out.push(source[start..j].to_string());
            i = j;
        } else {
            i += 1;
        }
    }
    out
}

/// Strong verification: the formatted output must match the input on
/// three independent axes — AST shape, significant-token sequence,
/// and comment coverage. Returns `Ok(())` only when all three pass.
pub fn verify(
    path: &Path,
    original: &Program,
    original_src: &str,
    formatted_src: &str,
) -> Result<(), FmtError> {
    // 1. Re-parse must succeed.
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

    // 2. AST fingerprints must match.
    let a = Fingerprint::of_program(original);
    let b = Fingerprint::of_program(&parsed.program);
    if a != b {
        return Err(FmtError::Verify {
            path: path.to_path_buf(),
            mismatch: format!(
                "AST fingerprint differs\n  input:    {}\n  formatted:{}",
                a.tree, b.tree
            ),
        });
    }

    // 3. Significant-token sequences must match.
    let orig_tokens = significant_token_sequence(original_src);
    let new_tokens = significant_token_sequence(formatted_src);
    if orig_tokens != new_tokens {
        let summary = diff_token_sequences(&orig_tokens, &new_tokens);
        return Err(FmtError::Verify {
            path: path.to_path_buf(),
            mismatch: format!("token sequence differs\n{summary}"),
        });
    }

    // 4. Every comment in the original must appear in the formatted
    //    output.
    for c in extract_comments(original_src) {
        if !formatted_src.contains(c.as_str()) {
            return Err(FmtError::Verify {
                path: path.to_path_buf(),
                mismatch: format!("comment dropped: `{c}`"),
            });
        }
    }

    Ok(())
}

/// Produce a short summary of where two token sequences diverge.
fn diff_token_sequences(a: &[String], b: &[String]) -> String {
    let mut out = String::new();
    let n = a.len().min(b.len());
    let mut first_diff: Option<usize> = None;
    for i in 0..n {
        if a[i] != b[i] {
            first_diff = Some(i);
            break;
        }
    }
    match first_diff {
        Some(i) => {
            let lo = i.saturating_sub(2);
            let hi = (i + 3).min(a.len()).min(b.len());
            out.push_str(&format!(
                "first divergence at token #{i}\n  input context:    {:?}\n  formatted context: {:?}",
                &a[lo..hi],
                &b[lo..hi],
            ));
            if a.len() != b.len() {
                out.push_str(&format!(
                    "\n  (input has {} tokens, formatted has {})",
                    a.len(),
                    b.len()
                ));
            }
        }
        None => {
            out.push_str(&format!(
                "length mismatch: input={}, formatted={}",
                a.len(),
                b.len()
            ));
        }
    }
    out
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

    #[test]
    fn significant_token_sequence_strips_whitespace_and_newlines() {
        let a = significant_token_sequence("x := 1 + 2\n");
        let b = significant_token_sequence("  x   :=  1+2 \n");
        assert_eq!(a, b);
    }

    #[test]
    fn significant_token_sequence_differs_on_operator_change() {
        let a = significant_token_sequence("x := 1 + 2\n");
        let b = significant_token_sequence("x := 1 - 2\n");
        assert_ne!(a, b);
    }

    #[test]
    fn extract_comments_finds_line_block_and_doc() {
        let src = "// line\nx := 1 /* block */ + 2\n/// doc\n";
        let cs = extract_comments(src);
        assert_eq!(cs, vec!["// line", "/* block */", "/// doc"]);
    }
}
