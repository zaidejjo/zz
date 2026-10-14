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

/// Compute extract actions for the cursor/range in `source`.
pub fn refactor_actions(
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
fn used_in_expr(expr: &Expr, out: &mut Vec<String>) {
    match expr {
        Expr::Ident { name, .. } => out.push(name.clone()),
        Expr::Path { parts, .. } => {
            if let Some(first) = parts.first() {
                out.push(first.clone());
            }
        }
        Expr::Call { callee, args, .. } => {
            used_in_expr(callee, out);
            for arg in args {
                used_in_expr(arg, out);
            }
        }
        Expr::Binary { left, right, .. } => {
            used_in_expr(left, out);
            used_in_expr(right, out);
        }
        Expr::Unary { expr, .. } | Expr::Paren { expr, .. } => used_in_expr(expr, out),
        Expr::Field { obj, .. } => used_in_expr(obj, out),
        Expr::Index { obj, index, .. } => {
            used_in_expr(obj, out);
            used_in_expr(index, out);
        }
        _ => {}
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
            Stmt::Decl { name, value, .. } if tail_name.as_deref() == Some(name.name.as_str()) => {
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
        let actions = refactor_actions(
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
        let actions = refactor_actions(
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
        let actions = refactor_actions(
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
    fn refuses_outer_assignment() {
        let src = "func main() {\n    x := 0\n    x = 1\n    println(x)\n}\n";
        let program = zz_frontend::parse(src).program;
        let uri = test_uri();
        let actions = refactor_actions(
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
