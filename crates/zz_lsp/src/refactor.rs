//! Extract refactors: variable and function extraction as
//! `refactor.extract` code actions.
//!
//! Both are pure source-to-source transforms over spans, so they work on
//! partial programs mid-typing:
//!
//! - **Extract variable**: cursor on a meaningful expression
//!   (`a + b * c` with the cursor on `b`) → `extracted := b * c`
//!   inserted before the enclosing statement, expression replaced.
//!   The placeholder name is rename-ready (`F2` renames it everywhere).
//! - **Extract function**: a range covering whole statements → a new
//!   `func` after the enclosing function (or at top level), free
//!   variables as parameters, call site in place. Guardrailed: refuses
//!   ranges with `return`, `break`/`continue` escaping the range, or
//!   assignments to outer bindings.

use tower_lsp::lsp_types::*;
use zz_frontend::ast::{Block, Expr, Program, Stmt};
use zz_frontend::span::Span;

use crate::convert::LineIndex;

/// Compute refactor actions for the cursor/range in `source`:
/// extract variable/function, inline variable, and move-to-new-file.
pub fn refactor_actions(
    state: &crate::state::GlobalState,
    program: &Program,
    source: &str,
    file_uri: &Url,
    range: Range,
) -> CodeActionResponse {
    let mut actions = CodeActionResponse::new();
    let line_index = LineIndex::new(source);
    let start = line_index.position_to_offset(source, range.start);
    let end = line_index.position_to_offset(source, range.end);

    if let Some(action) = extract_variable(program, source, file_uri, start) {
        actions.push(action.into());
    }
    if let Some(action) = inline_variable(program, source, file_uri, start) {
        actions.push(action.into());
    }
    if let Some(action) = move_to_new_file(state, program, source, file_uri, start) {
        actions.push(action.into());
    }
    if start != end {
        if let Some(action) = extract_function(program, source, file_uri, start, end) {
            actions.push(action.into());
        }
    }
    actions
}

// ── Extract variable ─────────────────────────────────────────────────────

/// Expressions worth extracting (everything but bare names and literals).
fn extractable(expr: &Expr) -> bool {
    !matches!(
        expr,
        Expr::Ident { .. }
            | Expr::Int { .. }
            | Expr::Float { .. }
            | Expr::Str { .. }
            | Expr::Bool { .. }
    )
}

/// Innermost extractable expression containing `offset`, plus the span of
/// its enclosing statement (insertion anchor).
fn find_extract_target(program: &Program, offset: u32) -> Option<(Span, Span)> {
    let mut best: Option<(Span, Span, u32)> = None;
    for stmt in &program.stmts {
        walk_stmt_for_expr(stmt, offset, &mut best);
    }
    best.map(|(expr, stmt, _)| (expr, stmt))
}

fn consider(best: &mut Option<(Span, Span, u32)>, expr: Span, stmt: Span) {
    // Innermost wins: smaller expression span replaces.
    let width = expr.end - expr.start;
    if best.is_none_or(|(_, _, w)| width < w) {
        *best = Some((expr, stmt, width));
    }
}

fn walk_stmt_for_expr(stmt: &Stmt, offset: u32, best: &mut Option<(Span, Span, u32)>) {
    let span = stmt.span();
    if offset < span.start || offset >= span.end {
        return;
    }
    match stmt {
        Stmt::Decl { value, .. } => walk_expr_for_target(value, span, offset, best),
        Stmt::Expr(e) | Stmt::Assign { value: e, .. } => {
            walk_expr_for_target(e, span, offset, best)
        }
        Stmt::Return { value: Some(e), .. } => {
            walk_expr_for_target(e, span, offset, best);
        }
        Stmt::Return { value: None, .. } => {}
        Stmt::Func { body, .. } => walk_block_for_target(body, offset, best),
        Stmt::For { iter, body, .. } => {
            walk_expr_for_target(iter, span, offset, best);
            walk_block_for_target(body, offset, best);
        }
        _ => {}
    }
}

fn walk_block_for_target(block: &Block, offset: u32, best: &mut Option<(Span, Span, u32)>) {
    if offset < block.span.start || offset >= block.span.end {
        return;
    }
    for stmt in &block.stmts {
        walk_stmt_for_expr(stmt, offset, best);
    }
}

fn walk_expr_for_target(
    expr: &Expr,
    stmt: Span,
    offset: u32,
    best: &mut Option<(Span, Span, u32)>,
) {
    let span = expr.span();
    if offset < span.start || offset >= span.end {
        return;
    }
    if extractable(expr) {
        consider(best, span, stmt);
    }
    // Recurse into children (shapes mirror lookup.rs walkers).
    match expr {
        Expr::Call { callee, args, .. } => {
            walk_expr_for_target(callee, stmt, offset, best);
            for arg in args {
                walk_expr_for_target(arg, stmt, offset, best);
            }
        }
        Expr::Binary { left, right, .. } => {
            walk_expr_for_target(left, stmt, offset, best);
            walk_expr_for_target(right, stmt, offset, best);
        }
        Expr::Unary { expr, .. } => walk_expr_for_target(expr, stmt, offset, best),
        Expr::Field { obj, .. } => walk_expr_for_target(obj, stmt, offset, best),
        Expr::Index { obj, index, .. } => {
            walk_expr_for_target(obj, stmt, offset, best);
            walk_expr_for_target(index, stmt, offset, best);
        }
        Expr::Paren { expr, .. } => walk_expr_for_target(expr, stmt, offset, best),
        // Branch bodies: the anchor stays the innermost statement, so
        // hoisting never crosses a branch boundary.
        Expr::If {
            cond, then, els, ..
        } => {
            walk_expr_for_target(cond, stmt, offset, best);
            walk_block_for_target(then, offset, best);
            if let Some(els) = els {
                walk_expr_for_target(els, stmt, offset, best);
            }
        }
        Expr::While { cond, body, .. } => {
            walk_expr_for_target(cond, stmt, offset, best);
            walk_block_for_target(body, offset, best);
        }
        // Closures: never descend — hoisting out of a closure body
        // would unbind its parameters. The closure itself stays
        // extractable as a whole.
        _ => {}
    }
}

/// Fresh placeholder name not occurring in `source`.
fn fresh_name(source: &str, base: &str) -> String {
    if find_word(source, base).is_none() {
        return base.to_string();
    }
    for i in 2.. {
        let candidate = format!("{base}{i}");
        if find_word(source, &candidate).is_none() {
            return candidate;
        }
    }
    unreachable!("fresh name search exhausted")
}

fn find_word(haystack: &str, needle: &str) -> Option<usize> {
    if needle.is_empty() {
        return None;
    }
    let mut start = 0;
    while let Some(pos) = haystack[start..].find(needle) {
        let abs = start + pos;
        let before = haystack[..abs].chars().next_back();
        let after = haystack[abs + needle.len()..].chars().next();
        let boundary = |c: Option<char>| c.is_none_or(|c| !(c.is_alphanumeric() || c == '_'));
        if boundary(before) && boundary(after) {
            return Some(abs);
        }
        start = abs + 1;
    }
    None
}

fn extract_variable(
    program: &Program,
    source: &str,
    file_uri: &Url,
    offset: u32,
) -> Option<CodeAction> {
    let (expr_span, stmt_span) = find_extract_target(program, offset)?;
    let expr_text = source.get(expr_span.start as usize..expr_span.end as usize)?;
    let name = fresh_name(source, "extracted");

    let line_index = LineIndex::new(source);
    // Insert at the enclosing statement's line start, inheriting its indent.
    let stmt_range = line_index.span_to_range(source, stmt_span);
    let indent = line_indent(source, &line_index, stmt_range.start.line);

    let mut changes = std::collections::HashMap::new();
    changes.insert(
        file_uri.clone(),
        vec![
            TextEdit {
                range: Range {
                    start: Position {
                        line: stmt_range.start.line,
                        character: 0,
                    },
                    end: Position {
                        line: stmt_range.start.line,
                        character: 0,
                    },
                },
                new_text: format!("{indent}{name} := {expr_text}\n"),
            },
            TextEdit {
                range: line_index.span_to_range(source, expr_span),
                new_text: name.clone(),
            },
        ],
    );
    Some(CodeAction {
        title: format!("Extract to variable `{name}`"),
        kind: Some(CodeActionKind::REFACTOR_EXTRACT),
        edit: Some(WorkspaceEdit {
            changes: Some(changes),
            ..Default::default()
        }),
        ..Default::default()
    })
}

// ── Extract function ─────────────────────────────────────────────────────

/// Statements safe to extract as a group.
fn movable(stmt: &Stmt) -> bool {
    matches!(
        stmt,
        Stmt::Decl { .. } | Stmt::Expr(_) | Stmt::Assign { .. } | Stmt::CompoundAssign { .. }
    )
}

/// Collect identifier names used by an expression.
///
/// Anything missed here becomes a missing parameter (broken code) or a
/// missed escape (false accept), so every variant recurses. Scoped
/// binders (closure params, comprehension vars, match patterns) shadow
/// outer names and never leak out.
fn used_in_expr(expr: &Expr, out: &mut Vec<String>) {
    used_in_expr_scoped(expr, out, &mut Vec::new());
}

fn used_in_expr_scoped(expr: &Expr, out: &mut Vec<String>, bound: &mut Vec<String>) {
    match expr {
        Expr::Ident { name, .. } => {
            if !bound.contains(name) {
                out.push(name.clone());
            }
        }
        Expr::Path { parts, .. } => {
            if let Some(first) = parts.first() {
                if !bound.contains(first) {
                    out.push(first.clone());
                }
            }
        }
        Expr::Call {
            callee,
            args,
            named,
            ..
        } => {
            used_in_expr_scoped(callee, out, bound);
            for arg in args {
                used_in_expr_scoped(arg, out, bound);
            }
            for (_, value) in named {
                used_in_expr_scoped(value, out, bound);
            }
        }
        Expr::Binary { left, right, .. } => {
            used_in_expr_scoped(left, out, bound);
            used_in_expr_scoped(right, out, bound);
        }
        Expr::Unary { expr, .. } | Expr::Paren { expr, .. } | Expr::Try { expr, .. } => {
            used_in_expr_scoped(expr, out, bound)
        }
        Expr::Field { obj, .. } => used_in_expr_scoped(obj, out, bound),
        Expr::Index { obj, index, .. } => {
            used_in_expr_scoped(obj, out, bound);
            used_in_expr_scoped(index, out, bound);
        }
        Expr::Slice {
            obj, start, end, ..
        } => {
            used_in_expr_scoped(obj, out, bound);
            if let Some(s) = start {
                used_in_expr_scoped(s, out, bound);
            }
            if let Some(e) = end {
                used_in_expr_scoped(e, out, bound);
            }
        }
        Expr::Range { start, end, .. } => {
            used_in_expr_scoped(start, out, bound);
            used_in_expr_scoped(end, out, bound);
        }
        Expr::Array { elems, .. } => {
            for e in elems {
                used_in_expr_scoped(e, out, bound);
            }
        }
        Expr::Tuple { items, .. } => {
            for e in items {
                used_in_expr_scoped(e, out, bound);
            }
        }
        Expr::Dict { entries, .. } => {
            for (k, v) in entries {
                used_in_expr_scoped(k, out, bound);
                used_in_expr_scoped(v, out, bound);
            }
        }
        Expr::StructInit { fields, .. } => {
            for (_, v) in fields {
                used_in_expr_scoped(v, out, bound);
            }
        }
        Expr::Variant { arg: Some(arg), .. } => {
            used_in_expr_scoped(arg, out, bound);
        }
        Expr::Variant { arg: None, .. } => {}
        Expr::Fmt { parts, .. } => {
            for part in parts {
                if let zz_frontend::ast::FmtPart::Expr(e, _) = part {
                    used_in_expr_scoped(e, out, bound);
                }
            }
        }
        Expr::If {
            cond, then, els, ..
        } => {
            used_in_expr_scoped(cond, out, bound);
            used_in_block(then, out, bound);
            if let Some(e) = els {
                used_in_expr_scoped(e, out, bound);
            }
        }
        Expr::IfLet {
            value, then, els, ..
        } => {
            used_in_expr_scoped(value, out, bound);
            used_in_block(then, out, bound);
            if let Some(e) = els {
                used_in_expr_scoped(e, out, bound);
            }
        }
        Expr::While { cond, body, .. } => {
            used_in_expr_scoped(cond, out, bound);
            used_in_block(body, out, bound);
        }
        Expr::Match {
            scrutinee, arms, ..
        } => {
            used_in_expr_scoped(scrutinee, out, bound);
            for arm in arms {
                let depth = bound.len();
                pattern_bindings(&arm.pat, bound);
                if let Some(g) = &arm.guard {
                    used_in_expr_scoped(g, out, bound);
                }
                used_in_expr_scoped(&arm.body, out, bound);
                bound.truncate(depth);
            }
        }
        Expr::Closure { params, body, .. } => {
            let depth = bound.len();
            for p in params {
                bound.push(p.name.name.clone());
            }
            used_in_expr_scoped(body, out, bound);
            bound.truncate(depth);
        }
        Expr::ListComp {
            body,
            var,
            iter,
            filter,
            ..
        } => {
            used_in_expr_scoped(iter, out, bound);
            let depth = bound.len();
            bound.push(var.name.clone());
            used_in_expr_scoped(body, out, bound);
            if let Some(f) = filter {
                used_in_expr_scoped(f, out, bound);
            }
            bound.truncate(depth);
        }
        Expr::Block(b) => used_in_block(b, out, bound),
        // Literals, break/continue-as-expression: no names.
        _ => {}
    }
}

/// Names bound by a match pattern (`Some(v)`, `(a, b)`, `x`).
fn pattern_bindings(pat: &zz_frontend::ast::Pattern, bound: &mut Vec<String>) {
    match pat {
        zz_frontend::ast::Pattern::Binding { name } => bound.push(name.name.clone()),
        zz_frontend::ast::Pattern::Variant { arg: Some(arg), .. } => {
            pattern_bindings(arg, bound);
        }
        zz_frontend::ast::Pattern::Variant { arg: None, .. } => {}
        zz_frontend::ast::Pattern::Tuple { pats, .. } => {
            for p in pats {
                pattern_bindings(p, bound);
            }
        }
        zz_frontend::ast::Pattern::Or { pats, .. } => {
            for p in pats {
                pattern_bindings(p, bound);
            }
        }
        _ => {}
    }
}

/// Names read by every statement of a block.
fn used_in_block(block: &Block, out: &mut Vec<String>, bound: &mut Vec<String>) {
    for stmt in &block.stmts {
        let mut stmt_used = Vec::new();
        used_in_stmt(stmt, &mut stmt_used);
        // Re-filter through this block's binders (lets inside the block
        // shadow correctly only within it — approximation: block-level
        // shadowing is rare; callers pass outer scope).
        for name in stmt_used {
            if !bound.contains(&name) {
                out.push(name);
            }
        }
        for name in bound_by(stmt) {
            bound.push(name);
        }
    }
}

/// `break` / `continue` used as expressions: extracting them into a
/// function changes control flow, so any picked range containing one
/// is refused.
fn has_control_escape(expr: &Expr) -> bool {
    match expr {
        Expr::Break { .. } | Expr::Continue { .. } => true,
        Expr::Call { callee, args, .. } => {
            has_control_escape(callee) || args.iter().any(has_control_escape)
        }
        Expr::Binary { left, right, .. } => has_control_escape(left) || has_control_escape(right),
        Expr::Unary { expr, .. }
        | Expr::Paren { expr, .. }
        | Expr::Try { expr, .. }
        | Expr::Variant {
            arg: Some(expr), ..
        } => has_control_escape(expr),
        Expr::Field { obj, .. } => has_control_escape(obj),
        Expr::Index { obj, index, .. } => has_control_escape(obj) || has_control_escape(index),
        Expr::If {
            cond, then, els, ..
        } => {
            has_control_escape(cond)
                || then.stmts.iter().any(stmt_has_escape)
                || els.as_deref().is_some_and(has_control_escape)
        }
        Expr::Match {
            scrutinee, arms, ..
        } => {
            has_control_escape(scrutinee)
                || arms.iter().any(|a| {
                    has_control_escape(&a.body) || a.guard.as_ref().is_some_and(has_control_escape)
                })
        }
        _ => false,
    }
}

fn stmt_has_escape(stmt: &Stmt) -> bool {
    match stmt {
        Stmt::Break { .. } | Stmt::Continue { .. } | Stmt::Return { .. } => true,
        Stmt::Decl { value, .. } => has_control_escape(value),
        Stmt::Expr(e) => has_control_escape(e),
        Stmt::Assign { value, .. } | Stmt::CompoundAssign { value, .. } => {
            has_control_escape(value)
        }
        Stmt::For { iter, body, .. } => {
            has_control_escape(iter) || body.stmts.iter().any(stmt_has_escape)
        }
        _ => false,
    }
}

/// The plain identifier assigned by `x = ...`, if any.
fn ident_target(target: &Expr) -> Option<String> {
    match target {
        Expr::Ident { name, .. } => Some(name.clone()),
        _ => None,
    }
}

/// Identifier names read by a statement (targets excluded: `x = ...`
/// reads nothing under `x` itself for our purposes).
fn used_in_stmt(stmt: &Stmt, out: &mut Vec<String>) {
    match stmt {
        Stmt::Decl { value, .. } => used_in_expr(value, out),
        Stmt::Expr(e) => used_in_expr(e, out),
        Stmt::Assign { value, .. } | Stmt::CompoundAssign { value, .. } => used_in_expr(value, out),
        Stmt::For { iter, body, .. } => {
            used_in_expr(iter, out);
            let mut bound = Vec::new();
            used_in_block(body, out, &mut bound);
        }
        _ => {}
    }
}

/// Names bound by a statement: declarations only. Assignments do not
/// bind — `x = 1` with an outer `x` escapes the range (refused), and the
/// escape loop below relies on that.
fn bound_by(stmt: &Stmt) -> Vec<String> {
    match stmt {
        Stmt::Decl { name, .. } => vec![name.name.clone()],
        _ => vec![],
    }
}

/// Leading whitespace of a source line.
fn line_indent(source: &str, line_index: &LineIndex, line: u32) -> String {
    let byte = line_index.position_to_offset(source, Position { line, character: 0 });
    source
        .get(byte as usize..)
        .and_then(|rest| rest.lines().next())
        .map(|text| {
            text.chars()
                .take_while(|c| c.is_whitespace())
                .collect::<String>()
        })
        .unwrap_or_default()
}

fn extract_function(
    program: &Program,
    source: &str,
    file_uri: &Url,
    start: u32,
    end: u32,
) -> Option<CodeAction> {
    // The range must cover whole top-level statements or whole statements
    // of one function body — never a partial statement, never across
    // function boundaries.
    let run = enclosing_stmt_run(program, start, end)?;
    let container = run.container;
    let picked = run.picked;
    if picked.is_empty() || !picked.iter().all(|s| movable(s)) {
        return None;
    }
    // Break/continue/return change control flow: extracting them into a
    // function body rebinds their target. Refuse outright.
    if picked.iter().copied().any(stmt_has_escape) {
        return None;
    }
    // No escaping control flow and no writes to outer bindings.
    let bound_here: Vec<String> = picked.iter().copied().flat_map(bound_by).collect();
    for stmt in picked.iter().copied() {
        match stmt {
            // Writes to anything but a range-local plain name escape
            // (fields, indexes, outer names) and refuse the extract.
            Stmt::Assign { target, .. } | Stmt::CompoundAssign { target, .. } => {
                let bound = ident_target(target).is_some_and(|name| bound_here.contains(&name));
                if !bound {
                    return None;
                }
            }
            _ => {}
        }
    }

    // Escape analysis: a name bound in the range but read after it
    // would be lost — refuse unless it is the returned tail binding.
    let tail_name: Option<String> = match picked.last() {
        Some(Stmt::Decl { name, .. }) => Some(name.name.clone()),
        _ => None,
    };
    let mut following_used: Vec<String> = Vec::new();
    for stmt in run.following.iter().copied() {
        used_in_stmt(stmt, &mut following_used);
    }
    for name in &bound_here {
        if Some(name) != tail_name.as_ref() && following_used.contains(name) {
            return None;
        }
    }

    let mut params: Vec<String> = Vec::new();
    for stmt in picked.iter().copied() {
        let mut used = Vec::new();
        used_in_stmt(stmt, &mut used);
        for name in used {
            if !bound_here.contains(&name) && !params.contains(&name) {
                params.push(name);
            }
        }
    }

    // Result binding: a trailing `v := e` becomes `return e` in the body
    // and `v := name(args)` at the call site.
    let func_name = fresh_name(source, "extracted_func");
    let line_index = LineIndex::new(source);

    // New body: picked statements dedented to a common base, re-indented
    // four spaces under the new func; a trailing Decl becomes `return`.
    let first = picked.first().unwrap().span();
    let last = picked.last().unwrap().span();
    let mut body_lines: Vec<String> = Vec::new();
    for stmt in picked.iter().copied() {
        let span = stmt.span();
        let text = source
            .get(span.start as usize..span.end as usize)
            .unwrap_or("");
        match stmt {
            // Identity by span: a same-named earlier Decl must NOT be
            // rewritten (shadowing would return the wrong value).
            Stmt::Decl { value, .. } if tail_name.is_some() && stmt.span() == last => {
                let value_text = source
                    .get(value.span().start as usize..value.span().end as usize)
                    .unwrap_or("");
                body_lines.push(format!("    return {value_text}"));
            }
            _ => {
                for line in text.lines() {
                    body_lines.push(format!("    {}", line.trim_start()));
                }
            }
        }
    }
    let body = body_lines.join("\n");

    // Insertion: after the enclosing function, else after the picked run.
    let insert_line = match container {
        StmtContainer::Func { span } => line_index.span_to_range(source, span).end.line + 1,
        StmtContainer::TopLevel => line_index.span_to_range(source, last).end.line + 1,
    };

    let param_list = params.join(", ");
    let new_func = format!("func {func_name}({param_list}) {{\n{body}\n}}\n");
    let call_text = match tail_name {
        Some(v) => format!("{v} := {func_name}({param_list})"),
        None => format!("{func_name}({param_list})"),
    };
    let call_edit = TextEdit {
        range: line_index.span_to_range(source, Span::new(first.start, last.end)),
        // No indent prefix: the range starts mid-line, so the original
        // leading whitespace stays.
        new_text: call_text,
    };

    let mut changes = std::collections::HashMap::new();
    changes.insert(
        file_uri.clone(),
        vec![
            TextEdit {
                range: Range {
                    start: Position {
                        line: insert_line,
                        character: 0,
                    },
                    end: Position {
                        line: insert_line,
                        character: 0,
                    },
                },
                new_text: format!("\n{new_func}"),
            },
            call_edit,
        ],
    );
    Some(CodeAction {
        title: format!("Extract to function `{func_name}`"),
        kind: Some(CodeActionKind::REFACTOR_EXTRACT),
        edit: Some(WorkspaceEdit {
            changes: Some(changes),
            ..Default::default()
        }),
        ..Default::default()
    })
}

/// Where a statement run lives.
enum StmtContainer {
    Func { span: Span },
    TopLevel,
}

/// Statements inside one body (or top level) with the picked run marked.
struct StmtRun<'a> {
    container: StmtContainer,
    picked: Vec<&'a Stmt>,
    following: Vec<&'a Stmt>,
}

/// Find whole statements fully inside [start, end): top-level first,
/// then inside exactly one function body.
fn enclosing_stmt_run(program: &Program, start: u32, end: u32) -> Option<StmtRun<'_>> {
    // Whole top-level statements (but not a whole function definition —
    // extracting a def is a move, not an extract).
    let picked: Vec<&Stmt> = program
        .stmts
        .iter()
        .filter(|s| {
            s.span().start >= start && s.span().end <= end && !matches!(s, Stmt::Func { .. })
        })
        .collect();
    if !picked.is_empty() {
        let following: Vec<&Stmt> = program
            .stmts
            .iter()
            .filter(|s| s.span().start >= end)
            .collect();
        return Some(StmtRun {
            container: StmtContainer::TopLevel,
            picked,
            following,
        });
    }
    for stmt in &program.stmts {
        if let Stmt::Func { body, span, .. } = stmt {
            if span.start <= start && span.end >= end {
                let picked: Vec<&Stmt> = body
                    .stmts
                    .iter()
                    .filter(|s| s.span().start >= start && s.span().end <= end)
                    .collect();
                if !picked.is_empty() {
                    let following: Vec<&Stmt> = body
                        .stmts
                        .iter()
                        .filter(|s| s.span().start >= end)
                        .collect();
                    return Some(StmtRun {
                        container: StmtContainer::Func { span: *span },
                        picked,
                        following,
                    });
                }
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_uri() -> Url {
        "file:///ref.zz".parse().unwrap()
    }

    fn offset_at(source: &str, line: u32, character: u32) -> u32 {
        LineIndex::new(source).position_to_offset(source, Position { line, character })
    }

    /// Apply a code action's file edits to `source` (reverse order).
    fn apply_action(source: &str, action: &CodeAction, uri: &Url) -> String {
        let mut edits = action
            .edit
            .as_ref()
            .expect("edit")
            .changes
            .as_ref()
            .expect("changes")[uri]
            .clone();
        let line_index = LineIndex::new(source);
        edits.sort_by_key(|e| {
            std::cmp::Reverse(line_index.position_to_offset(source, e.range.start))
        });
        let mut text = source.to_string();
        for edit in edits {
            let start = line_index.position_to_offset(&text, edit.range.start) as usize;
            let end = line_index.position_to_offset(&text, edit.range.end) as usize;
            text.replace_range(start..end, &edit.new_text);
        }
        text
    }

    fn variable_action(source: &str, line: u32, character: u32) -> Option<CodeAction> {
        let program = zz_frontend::parse(source).program;
        let uri = test_uri();
        let state = crate::state::GlobalState::new();
        let actions = refactor_actions(
            &state,
            &program,
            source,
            &uri,
            Range {
                start: Position { line, character },
                end: Position { line, character },
            },
        );
        actions.into_iter().find_map(|a| match a {
            CodeActionOrCommand::CodeAction(a)
                if a.kind == Some(CodeActionKind::REFACTOR_EXTRACT)
                    && a.title.starts_with("Extract to variable") =>
            {
                Some(a)
            }
            _ => None,
        })
    }

    #[test]
    fn extracts_innermost_expression() {
        let src = "func main() {\n    y := a + b * c\n    println(y)\n}\n";
        let action = variable_action(src, 1, 13).expect("extract action");
        let out = apply_action(src, &action, &test_uri());
        assert_eq!(
            out,
            "func main() {\n    extracted := b * c\n    y := a + extracted\n    println(y)\n}\n",
            "got:\n{out}"
        );
        // Result still parses.
        assert!(zz_frontend::parse(&out).errors.is_empty());
    }

    #[test]
    fn no_variable_action_on_bare_name() {
        // Cursor on the declared name itself: the walk only descends
        // into values, so there is no target.
        let src = "func main() {\n    y := 1\n    println(y)\n}\n";
        assert!(
            variable_action(src, 1, 4).is_none(),
            "bare idents are not extracted"
        );
    }

    #[test]
    fn extracts_function_with_tail_return() {
        let src = "func main() {\n    a := 1\n    b := a + 2\n    println(b)\n}\n";
        let program = zz_frontend::parse(src).program;
        let uri = test_uri();
        let start = offset_at(src, 1, 0);
        let end = offset_at(src, 2, 14);
        let state = crate::state::GlobalState::new();
        let actions = refactor_actions(
            &state,
            &program,
            src,
            &uri,
            Range {
                start: Position {
                    line: 1,
                    character: 0,
                },
                end: Position {
                    line: 2,
                    character: 14,
                },
            },
        );
        let _ = (start, end);
        let action = actions
            .into_iter()
            .find_map(|a| match a {
                CodeActionOrCommand::CodeAction(a)
                    if a.kind == Some(CodeActionKind::REFACTOR_EXTRACT)
                        && a.title.starts_with("Extract to function") =>
                {
                    Some(a)
                }
                _ => None,
            })
            .expect("extract function action");
        let out = apply_action(src, &action, &uri);
        assert_eq!(
            out,
            "func main() {\n    b := extracted_func()\n    println(b)\n}\n\nfunc extracted_func() {\n    a := 1\n    return a + 2\n}\n",
            "got:\n{out}"
        );
        assert!(zz_frontend::parse(&out).errors.is_empty());
    }

    #[test]
    fn refuses_escaping_binding() {
        // `x` is bound in range, read after it, and is not the tail.
        let src = "func main() {\n    x := 1\n    y := 2\n    println(x)\n    println(y)\n}\n";
        let program = zz_frontend::parse(src).program;
        let uri = test_uri();
        let state = crate::state::GlobalState::new();
        let actions = refactor_actions(
            &state,
            &program,
            src,
            &uri,
            Range {
                start: Position {
                    line: 1,
                    character: 0,
                },
                end: Position {
                    line: 2,
                    character: 12,
                },
            },
        );
        assert!(
            actions.iter().all(|a| match a {
                CodeActionOrCommand::CodeAction(a) => !a.title.starts_with("Extract to function"),
                _ => true,
            }),
            "escaping binding refuses"
        );
    }

    #[test]
    fn tail_rewrite_matches_last_statement_only() {
        // Two Decls named `x`: only the trailing one becomes `return`.
        let src = "func main() {\n    x := 1\n    y := x + 1\n    x := 3\n    println(x)\n}\n";
        let program = zz_frontend::parse(src).program;
        let uri = test_uri();
        let state = crate::state::GlobalState::new();
        let actions = refactor_actions(
            &state,
            &program,
            src,
            &uri,
            Range {
                start: Position {
                    line: 1,
                    character: 0,
                },
                end: Position {
                    line: 3,
                    character: 10,
                },
            },
        );
        let action = actions
            .into_iter()
            .find_map(|a| match a {
                CodeActionOrCommand::CodeAction(a)
                    if a.kind == Some(CodeActionKind::REFACTOR_EXTRACT)
                        && a.title.starts_with("Extract to function") =>
                {
                    Some(a)
                }
                _ => None,
            })
            .expect("extract function action");
        let out = apply_action(src, &action, &uri);
        assert!(
            out.contains("    return 3\n"),
            "tail rewritten once: {out:?}"
        );
        assert!(
            !out.contains("return 1"),
            "earlier shadow untouched: {out:?}"
        );
        assert!(zz_frontend::parse(&out).errors.is_empty());
    }

    #[test]
    fn params_thread_through_containers() {
        // Array literal + if-expression: every free name becomes a param.
        let src = "func main() {\n    b := [a, c]\n    println(b)\n}\n";
        let program = zz_frontend::parse(src).program;
        let uri = test_uri();
        let state = crate::state::GlobalState::new();
        let actions = refactor_actions(
            &state,
            &program,
            src,
            &uri,
            Range {
                start: Position {
                    line: 1,
                    character: 0,
                },
                end: Position {
                    line: 1,
                    character: 15,
                },
            },
        );
        let action = actions
            .into_iter()
            .find_map(|a| match a {
                CodeActionOrCommand::CodeAction(a)
                    if a.kind == Some(CodeActionKind::REFACTOR_EXTRACT)
                        && a.title.starts_with("Extract to function") =>
                {
                    Some(a)
                }
                _ => None,
            })
            .expect("extract function action");
        let out = apply_action(src, &action, &uri);
        assert!(
            out.contains("b := extracted_func(a, c)"),
            "params threaded: {out:?}"
        );
        assert!(zz_frontend::parse(&out).errors.is_empty());
    }

    #[test]
    fn refuses_break_in_match_arm() {
        let src = "func main() {\n    m := match x {\n        1 => break,\n        _ => 2,\n    }\n    println(m)\n}\n";
        let program = zz_frontend::parse(src).program;
        assert!(
            zz_frontend::parse(src).errors.is_empty(),
            "fixture must parse"
        );
        let uri = test_uri();
        let state = crate::state::GlobalState::new();
        let actions = refactor_actions(
            &state,
            &program,
            src,
            &uri,
            Range {
                start: Position {
                    line: 1,
                    character: 0,
                },
                end: Position {
                    line: 4,
                    character: 5,
                },
            },
        );
        assert!(
            actions.iter().all(|a| match a {
                CodeActionOrCommand::CodeAction(a) => !a.title.starts_with("Extract to function"),
                _ => true,
            }),
            "break in range refuses"
        );
    }

    #[test]
    fn refuses_outer_assignment() {
        let src = "func main() {\n    x := 0\n    x = 1\n    println(x)\n}\n";
        let program = zz_frontend::parse(src).program;
        let uri = test_uri();
        let state = crate::state::GlobalState::new();
        let actions = refactor_actions(
            &state,
            &program,
            src,
            &uri,
            Range {
                start: Position {
                    line: 2,
                    character: 0,
                },
                end: Position {
                    line: 2,
                    character: 9,
                },
            },
        );
        assert!(
            actions.iter().all(|a| match a {
                CodeActionOrCommand::CodeAction(a) => !a.title.starts_with("Extract to function"),
                _ => true,
            }),
            "outer assignment refuses"
        );
    }
}

// ── Inline variable ────────────────────────────────────────────────────

/// Effect-free expressions: duplicating them at several use sites
/// changes nothing observable. Calls (and control flow) may run code.
fn is_effect_free(expr: &Expr) -> bool {
    match expr {
        Expr::Call { .. } => false,
        Expr::Ident { .. }
        | Expr::Int { .. }
        | Expr::Float { .. }
        | Expr::Str { .. }
        | Expr::Bool { .. } => true,
        Expr::Binary { left, right, .. } => is_effect_free(left) && is_effect_free(right),
        Expr::Unary { expr, .. } | Expr::Paren { expr, .. } => is_effect_free(expr),
        Expr::Field { obj, .. } => is_effect_free(obj),
        Expr::Index { obj, index, .. } => is_effect_free(obj) && is_effect_free(index),
        Expr::Array { elems, .. } => elems.iter().all(is_effect_free),
        Expr::Tuple { items, .. } => items.iter().all(is_effect_free),
        _ => false,
    }
}

/// Atomic expressions need no parentheses when inlined into an operand
/// position; everything else gets wrapped.
fn needs_parens(expr: &Expr) -> bool {
    !matches!(
        expr,
        Expr::Ident { .. }
            | Expr::Path { .. }
            | Expr::Call { .. }
            | Expr::Field { .. }
            | Expr::Index { .. }
            | Expr::Int { .. }
            | Expr::Float { .. }
            | Expr::Str { .. }
            | Expr::Bool { .. }
            | Expr::Array { .. }
            | Expr::Tuple { .. }
            | Expr::Paren { .. }
            | Expr::StructInit { .. }
            | Expr::Variant { .. }
            | Expr::Closure { .. }
            | Expr::Fmt { .. }
    )
}

/// True when `name` is bound anywhere nested inside `stmts`
/// (shadowing an outer binding of the same name).
fn nested_shadow(stmts: &[&Stmt], name: &str, decl_span: Span) -> bool {
    for stmt in stmts {
        if stmt.span() == decl_span {
            continue;
        }
        if stmt_binds(stmt, name) {
            return true;
        }
    }
    false
}

fn stmt_binds(stmt: &Stmt, name: &str) -> bool {
    match stmt {
        Stmt::Func { params, body, .. } => {
            params.iter().any(|p| p.name.name == name)
                || body.stmts.iter().any(|s| stmt_binds_deep(s, name))
        }
        _ => stmt_binds_deep(stmt, name),
    }
}

fn stmt_binds_deep(stmt: &Stmt, name: &str) -> bool {
    match stmt {
        Stmt::Decl { name: n, .. } => n.name == name,
        Stmt::Func { params, body, .. } => {
            params.iter().any(|p| p.name.name == name)
                || body.stmts.iter().any(|s| stmt_binds_deep(s, name))
        }
        Stmt::For { vars, body, .. } => {
            vars.iter().any(|v| v.name == name)
                || body.stmts.iter().any(|s| stmt_binds_deep(s, name))
        }
        _ => false,
    }
}

/// Bare-name usage spans of `name` at/after `from` inside `stmts`
/// (struct labels and qualified members are not variable refs).
fn usage_spans(stmts: &[&Stmt], name: &str, from: u32) -> Vec<Span> {
    let mut out = Vec::new();
    for stmt in stmts {
        collect_usages_in_stmt(stmt, name, from, &mut out);
    }
    out
}

fn collect_usages_in_stmt(stmt: &Stmt, name: &str, from: u32, out: &mut Vec<Span>) {
    match stmt {
        Stmt::Decl { value, .. } => collect_usages_in_expr(value, name, from, out),
        Stmt::Expr(e) => collect_usages_in_expr(e, name, from, out),
        Stmt::Return { value: Some(e), .. } => {
            collect_usages_in_expr(e, name, from, out);
        }
        Stmt::Return { value: None, .. } => {}
        Stmt::Assign { target, value, .. } | Stmt::CompoundAssign { target, value, .. } => {
            collect_usages_in_expr(target, name, from, out);
            collect_usages_in_expr(value, name, from, out);
        }
        Stmt::Func { body, .. } => {
            for s in &body.stmts {
                collect_usages_in_stmt(s, name, from, out);
            }
        }
        Stmt::For { iter, body, .. } => {
            collect_usages_in_expr(iter, name, from, out);
            for s in &body.stmts {
                collect_usages_in_stmt(s, name, from, out);
            }
        }
        _ => {}
    }
}

fn collect_usages_in_expr(expr: &Expr, name: &str, from: u32, out: &mut Vec<Span>) {
    match expr {
        Expr::Ident { name: n, span } => {
            if n == name && span.start >= from {
                out.push(*span);
            }
        }
        Expr::Call {
            callee,
            args,
            named,
            ..
        } => {
            collect_usages_in_expr(callee, name, from, out);
            for arg in args {
                collect_usages_in_expr(arg, name, from, out);
            }
            for (_, value) in named {
                collect_usages_in_expr(value, name, from, out);
            }
        }
        Expr::Binary { left, right, .. } => {
            collect_usages_in_expr(left, name, from, out);
            collect_usages_in_expr(right, name, from, out);
        }
        Expr::Unary { expr, .. } | Expr::Paren { expr, .. } => {
            collect_usages_in_expr(expr, name, from, out)
        }
        Expr::Field { obj, .. } => collect_usages_in_expr(obj, name, from, out),
        Expr::Index { obj, index, .. } => {
            collect_usages_in_expr(obj, name, from, out);
            collect_usages_in_expr(index, name, from, out);
        }
        Expr::Array { elems, .. } => {
            for e in elems {
                collect_usages_in_expr(e, name, from, out);
            }
        }
        _ => {}
    }
}

fn inline_variable(
    program: &Program,
    source: &str,
    file_uri: &Url,
    offset: u32,
) -> Option<CodeAction> {
    // The declaration under the cursor: a direct child of top level or
    // of one function body (deeper nesting is refused — scope analysis
    // would have to cross block boundaries).
    let mut found: Option<(Vec<&Stmt>, usize)> = None;
    for (i, stmt) in program.stmts.iter().enumerate() {
        if let Stmt::Decl { name, .. } = stmt {
            if offset >= name.span.start && offset < name.span.end {
                found = Some((program.stmts.iter().collect(), i));
                break;
            }
        }
        if let Stmt::Func { body, .. } = stmt {
            for (j, inner) in body.stmts.iter().enumerate() {
                if let Stmt::Decl { name, .. } = inner {
                    if offset >= name.span.start && offset < name.span.end {
                        found = Some((body.stmts.iter().collect(), j));
                        break;
                    }
                }
            }
            if found.is_some() {
                break;
            }
        }
    }
    let (container, idx) = found?;
    let (decl_name, decl_value, decl_span) = match container[idx] {
        Stmt::Decl {
            name, value, span, ..
        } => (name.name.clone(), value, *span),
        _ => return None,
    };

    // Rebinding anywhere in scope would make replacement ambiguous.
    let rebindings = container
        .iter()
        .filter(|s| match s {
            Stmt::Decl { name, .. } => name.name == decl_name,
            _ => false,
        })
        .count();
    if rebindings > 1 {
        return None;
    }
    let decl_end = decl_span.end;
    if nested_shadow(&container, &decl_name, decl_span) {
        return None;
    }

    let usages = usage_spans(&container, &decl_name, decl_end);
    // Effectful values inline at most once (same evaluation count);
    // effect-free values inline anywhere. Control flow never inlines.
    if has_control_escape(decl_value) {
        return None;
    }
    if !is_effect_free(decl_value) && usages.len() > 1 {
        return None;
    }

    let line_index = LineIndex::new(source);
    let value_text = source
        .get(decl_value.span().start as usize..decl_value.span().end as usize)
        .unwrap_or("");
    let replacement = if needs_parens(decl_value) {
        format!("({value_text})")
    } else {
        value_text.to_string()
    };

    // Delete the whole declaration line (consuming its newline when one
    // follows, the preceding one when it is the last line).
    let decl_range = line_index.span_to_range(source, decl_span);
    let first_line = decl_range.start.line;
    let last_line = decl_range.end.line;
    let line_count = source.lines().count() as u32;
    let (del_start, del_end) = if last_line + 1 < line_count {
        (
            line_index.position_to_offset(
                source,
                Position {
                    line: first_line,
                    character: 0,
                },
            ),
            line_index.position_to_offset(
                source,
                Position {
                    line: last_line + 1,
                    character: 0,
                },
            ),
        )
    } else if first_line > 0 {
        let prev_end = line_index.position_to_offset(
            source,
            Position {
                line: first_line - 1,
                character: u32::MAX,
            },
        );
        (prev_end.min(source.len() as u32), source.len() as u32)
    } else {
        (
            line_index.position_to_offset(
                source,
                Position {
                    line: first_line,
                    character: 0,
                },
            ),
            source.len() as u32,
        )
    };

    let mut edits = Vec::new();
    for span in &usages {
        edits.push(TextEdit {
            range: line_index.span_to_range(source, *span),
            new_text: replacement.clone(),
        });
    }
    edits.push(TextEdit {
        range: Range {
            start: line_index.offset_to_position(source, del_start),
            end: line_index.offset_to_position(source, del_end),
        },
        new_text: String::new(),
    });

    let mut changes = std::collections::HashMap::new();
    changes.insert(file_uri.clone(), edits);
    let title = if usages.is_empty() {
        format!("Remove unused variable `{decl_name}`")
    } else {
        format!("Inline variable `{decl_name}`")
    };
    Some(CodeAction {
        title,
        kind: Some(CodeActionKind::REFACTOR_INLINE),
        edit: Some(WorkspaceEdit {
            changes: Some(changes),
            ..Default::default()
        }),
        ..Default::default()
    })
}

// ── Move to new file ───────────────────────────────────────────────────

/// Move a top-level item (`func`, `struct`, `enum`, alias) to a new
/// sibling file named after it, leaving a selective import behind.
///
/// Preconditions (refusals are silent — no action offered):
/// - cursor on the definition's name;
/// - no decorators (they may not survive the move textually);
/// - no references outside the defining file;
/// - no qualified self-references (`lib.x` inside `lib` would dangle);
/// - destination file does not exist yet;
/// - the client applies `documentChanges` + `create` resource ops.
fn move_to_new_file(
    state: &crate::state::GlobalState,
    program: &Program,
    source: &str,
    file_uri: &Url,
    offset: u32,
) -> Option<CodeAction> {
    if !create_supported(state) {
        return None;
    }
    // The definition under the cursor: top-level items only.
    let defs = crate::lookup::collect_definitions(program, source);
    let def = defs.values().find(|d| {
        offset >= d.span.start
            && offset < d.span.end
            && matches!(
                d.kind,
                crate::lookup::DefKind::Func | crate::lookup::DefKind::Struct
            )
    })?;
    let (stmt, item_name) = program
        .stmts
        .iter()
        .filter_map(|s| match s {
            Stmt::Func { name, .. } => Some((s, name.join("."))),
            Stmt::Struct { name, .. } => Some((s, name.join("."))),
            Stmt::Enum { name, .. } => Some((s, name.join("."))),
            Stmt::TypeAlias { name, .. } => Some((s, name.join("."))),
            _ => None,
        })
        .find(|(_, name)| *name == def.name)?;
    // Decorated items stay put (decorator lines may sit outside the
    // statement span; moving would drop them).
    let decorated = match stmt {
        Stmt::Func { decorators, .. } => !decorators.is_empty(),
        _ => false,
    };
    if decorated {
        return None;
    }

    // No references outside the defining file (same-file bare usages
    // keep working through the import left behind).
    let scope = crate::symbol_refs::resolve_symbol_scope(state, file_uri, offset)?;
    if scope.files.len() > 1 {
        return None;
    }
    // No qualified self-references: `lib.x` inside `lib` would dangle.
    let refs = crate::lookup::find_references_to_name_in_program(program, source, &scope.name);
    for r in &refs {
        if r.is_definition {
            continue;
        }
        let text = source
            .get(r.span.start as usize..r.span.end as usize)
            .unwrap_or("");
        if text.contains('.') {
            return None;
        }
    }

    // Destination: `<lowercase-name>.zz` beside the source file.
    let dir = file_uri.to_file_path().ok()?.parent()?.to_path_buf();
    let stem = item_name.to_lowercase();
    let dest_path = dir.join(format!("{stem}.zz"));
    if dest_path.is_file() {
        return None;
    }
    let dest_uri = Url::from_file_path(&dest_path).ok()?;

    let line_index = LineIndex::new(source);
    let def_span = stmt.span();
    // Moved text: exact slice, normalized to one trailing newline.
    let mut moved = source
        .get(def_span.start as usize..def_span.end as usize)
        .unwrap_or("")
        .to_string();
    while moved.ends_with('\n') {
        moved.pop();
    }
    moved.push('\n');

    // Delete the definition's whole lines (same rule as inline).
    let def_range = line_index.span_to_range(source, def_span);
    let line_count = source.lines().count() as u32;
    let (del_start, del_end) = if def_range.end.line + 1 < line_count {
        (
            line_index.position_to_offset(
                source,
                Position {
                    line: def_range.start.line,
                    character: 0,
                },
            ),
            line_index.position_to_offset(
                source,
                Position {
                    line: def_range.end.line + 1,
                    character: 0,
                },
            ),
        )
    } else if def_range.start.line > 0 {
        let prev_end = line_index.position_to_offset(
            source,
            Position {
                line: def_range.start.line - 1,
                character: u32::MAX,
            },
        );
        (prev_end.min(source.len() as u32), source.len() as u32)
    } else {
        (
            line_index.position_to_offset(
                source,
                Position {
                    line: def_range.start.line,
                    character: 0,
                },
            ),
            source.len() as u32,
        )
    };

    // Import left behind (inserted after the import block like auto-import).
    let import_stmt = format!("import {stem}({item_name})");
    let import_range = crate::code_action::import_insert_range(source);

    let operations = vec![
        DocumentChangeOperation::Op(ResourceOp::Create(CreateFile {
            uri: dest_uri.clone(),
            options: Some(CreateFileOptions {
                overwrite: None,
                ignore_if_exists: Some(true),
            }),
            annotation_id: None,
        })),
        DocumentChangeOperation::Edit(TextDocumentEdit {
            text_document: OptionalVersionedTextDocumentIdentifier {
                uri: dest_uri,
                version: None,
            },
            edits: vec![OneOf::Left(TextEdit {
                range: Range {
                    start: Position {
                        line: 0,
                        character: 0,
                    },
                    end: Position {
                        line: 0,
                        character: 0,
                    },
                },
                new_text: moved,
            })],
        }),
        DocumentChangeOperation::Edit(TextDocumentEdit {
            text_document: OptionalVersionedTextDocumentIdentifier {
                uri: file_uri.clone(),
                version: None,
            },
            edits: vec![
                OneOf::Left(TextEdit {
                    range: Range {
                        start: line_index.offset_to_position(source, del_start),
                        end: line_index.offset_to_position(source, del_end),
                    },
                    new_text: String::new(),
                }),
                OneOf::Left(TextEdit {
                    range: import_range,
                    new_text: format!("{import_stmt}\n"),
                }),
            ],
        }),
    ];

    Some(CodeAction {
        title: format!("Move `{item_name}` to new file `{stem}.zz`"),
        kind: Some(CodeActionKind::REFACTOR),
        edit: Some(WorkspaceEdit {
            changes: None,
            document_changes: Some(DocumentChanges::Operations(operations)),
            change_annotations: None,
        }),
        ..Default::default()
    })
}

/// Whether the client applies file-creating workspace edits.
fn create_supported(state: &crate::state::GlobalState) -> bool {
    state
        .create_supported
        .load(std::sync::atomic::Ordering::SeqCst)
}

#[cfg(test)]
mod move_inline_tests {
    use super::*;

    fn test_uri() -> Url {
        "file:///ref.zz".parse().unwrap()
    }

    fn offset_at(source: &str, line: u32, character: u32) -> u32 {
        LineIndex::new(source).position_to_offset(source, Position { line, character })
    }

    fn apply_changes(source: &str, action: &CodeAction, uri: &Url) -> String {
        let mut edits = action
            .edit
            .as_ref()
            .expect("edit")
            .changes
            .as_ref()
            .expect("changes")[uri]
            .clone();
        let line_index = LineIndex::new(source);
        edits.sort_by_key(|e| {
            std::cmp::Reverse(line_index.position_to_offset(source, e.range.start))
        });
        let mut text = source.to_string();
        for edit in edits {
            let start = line_index.position_to_offset(&text, edit.range.start) as usize;
            let end = line_index.position_to_offset(&text, edit.range.end) as usize;
            text.replace_range(start..end, &edit.new_text);
        }
        text
    }

    fn inline_action(source: &str, line: u32, character: u32) -> Option<CodeAction> {
        let program = zz_frontend::parse(source).program;
        let uri = test_uri();
        let offset = offset_at(source, line, character);
        inline_variable(&program, source, &uri, offset)
    }

    #[test]
    fn inline_parenthesizes_binary() {
        let src = "func main() {\n    y := a + b\n    z := y * 2\n    println(z)\n}\n";
        let action = inline_action(src, 1, 4).expect("inline action");
        assert!(action.title.contains("Inline variable"));
        let out = apply_changes(src, &action, &test_uri());
        assert_eq!(
            out, "func main() {\n    z := (a + b) * 2\n    println(z)\n}\n",
            "got:\n{out}"
        );
        assert!(zz_frontend::parse(&out).errors.is_empty());
    }

    #[test]
    fn inline_single_use_call_no_parens() {
        let src = "func main() {\n    y := f(a)\n    z := y\n    println(z)\n}\n";
        let action = inline_action(src, 1, 4).expect("inline action");
        let out = apply_changes(src, &action, &test_uri());
        assert_eq!(
            out, "func main() {\n    z := f(a)\n    println(z)\n}\n",
            "got:\n{out}"
        );
    }

    #[test]
    fn inline_refuses_multi_use_call() {
        let src = "func main() {\n    y := f(a)\n    z := y + y\n    println(z)\n}\n";
        assert!(
            inline_action(src, 1, 4).is_none(),
            "effectful multi-use must refuse"
        );
    }

    #[test]
    fn inline_multi_use_pure_duplicates() {
        let src = "func main() {\n    y := a\n    z := y + y\n    println(z)\n}\n";
        let action = inline_action(src, 1, 4).expect("inline action");
        let out = apply_changes(src, &action, &test_uri());
        assert_eq!(
            out, "func main() {\n    z := a + a\n    println(z)\n}\n",
            "got:\n{out}"
        );
    }

    #[test]
    fn inline_removes_unused() {
        let src = "func main() {\n    y := 1\n    println(2)\n}\n";
        let action = inline_action(src, 1, 4).expect("remove action");
        assert!(action.title.contains("Remove unused"));
        let out = apply_changes(src, &action, &test_uri());
        assert_eq!(out, "func main() {\n    println(2)\n}\n", "got:\n{out}");
    }

    #[test]
    fn inline_refuses_shadowed_scope() {
        let src = "func main() {\n    y := 1\n    for y in [1, 2] {\n        println(y)\n    }\n    println(y)\n}\n";
        assert!(
            inline_action(src, 1, 4).is_none(),
            "shadowed scopes must refuse"
        );
    }

    // ── move ──

    fn move_tmp() -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("zz-lsp-move-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn move_action(
        state: &crate::state::GlobalState,
        uri: &Url,
        src: &str,
        line: u32,
        character: u32,
    ) -> Option<CodeAction> {
        let program = zz_frontend::parse(src).program;
        let offset = offset_at(src, line, character);
        move_to_new_file(state, &program, src, uri, offset)
    }

    #[test]
    fn move_creates_file_import_and_delete() {
        let state = crate::state::GlobalState::new();
        state
            .create_supported
            .store(true, std::sync::atomic::Ordering::SeqCst);
        let dir = move_tmp();
        let main_path = dir.join("main.zz");
        std::fs::write(&main_path, "").unwrap();
        let uri = Url::from_file_path(&main_path).unwrap();
        let src = "pub func circle(r: float) -> float {\n    return r * 2.0\n}\nfunc main() {\n    println(circle(1.0))\n}\n";
        state.update_document(uri.clone(), 1, src.to_string());

        let action = move_action(&state, &uri, src, 0, 9).expect("move action");
        assert!(
            action.title.contains("circle.zz"),
            "title: {}",
            action.title
        );
        let ops = match action.edit.expect("edit").document_changes.expect("ops") {
            DocumentChanges::Operations(ops) => ops,
            _ => panic!("expected operations"),
        };
        assert_eq!(ops.len(), 3, "create + fill + delete/import");
        // Create op targets circle.zz beside main.zz.
        match &ops[0] {
            DocumentChangeOperation::Op(ResourceOp::Create(create)) => {
                assert_eq!(
                    create.uri,
                    Url::from_file_path(dir.join("circle.zz")).unwrap()
                );
            }
            _ => panic!("first op creates the file"),
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn move_refuses_without_client_support() {
        let state = crate::state::GlobalState::new();
        let uri: Url = "file:///main.zz".parse().unwrap();
        let src = "pub func circle(r: float) -> float {\n    return r\n}\n";
        assert!(
            move_action(&state, &uri, src, 0, 9).is_none(),
            "no documentChanges support, no offer"
        );
    }

    #[test]
    fn move_refuses_existing_destination() {
        let state = crate::state::GlobalState::new();
        state
            .create_supported
            .store(true, std::sync::atomic::Ordering::SeqCst);
        let dir = move_tmp();
        std::fs::write(
            dir.join("circle.zz"),
            "pub func circle() -> int {\n    return 1\n}\n",
        )
        .unwrap();
        let main_path = dir.join("main.zz");
        std::fs::write(&main_path, "").unwrap();
        let uri = Url::from_file_path(&main_path).unwrap();
        let src = "pub func circle(r: float) -> float {\n    return r\n}\n";
        state.update_document(uri.clone(), 1, src.to_string());
        assert!(
            move_action(&state, &uri, src, 0, 9).is_none(),
            "existing destination refuses"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn move_refuses_external_references() {
        let state = crate::state::GlobalState::new();
        state
            .create_supported
            .store(true, std::sync::atomic::Ordering::SeqCst);
        let dir = move_tmp();
        let lib_src = "pub func circle(r: float) -> float {\n    return r\n}\n";
        let lib_uri: Url = "file:///lib.zz".parse().unwrap();
        state.update_document(lib_uri.clone(), 1, lib_src.to_string());
        {
            let entry = crate::cross_file::parse_file_entry(
                lib_uri.clone(),
                lib_src.to_string(),
                "lib".to_string(),
            );
            let mut index = state.module_index.write().unwrap();
            index
                .module_to_uri
                .insert("lib".to_string(), lib_uri.clone());
            index
                .uri_to_module
                .insert(lib_uri.clone(), "lib".to_string());
            index.entries.insert(lib_uri.clone(), entry);
        }
        let other_src = "import lib(circle)\nfunc other() {\n    println(circle(1.0))\n}\n";
        let other_uri: Url = "file:///other.zz".parse().unwrap();
        state.update_document(other_uri.clone(), 1, other_src.to_string());
        assert!(
            move_action(&state, &lib_uri, lib_src, 0, 9).is_none(),
            "external references refuse"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn move_refuses_decorated_items() {
        let state = crate::state::GlobalState::new();
        state
            .create_supported
            .store(true, std::sync::atomic::Ordering::SeqCst);
        let uri: Url = "file:///main.zz".parse().unwrap();
        let src = "@test\nfunc adds() {\n    assert_eq(1 + 1, 2)\n}\n";
        assert!(
            move_action(&state, &uri, src, 1, 5).is_none(),
            "decorated items stay put"
        );
    }
}
