//! Document symbol extraction from the ZZ AST.
//!
//! Produces a hierarchical `DocumentSymbol` tree for the outline panel
//! and a flat `SymbolInformation` list for workspace-wide symbol search.

#![allow(deprecated)]

use tower_lsp::lsp_types::{DocumentSymbol, Position, Range, SymbolInformation, SymbolKind, Url};
use zz_frontend::ast::{Block, Expr, Param, Program, Stmt, TyKind};
use zz_frontend::span::Span;

use crate::convert::span_to_range;

// ── Document symbols (hierarchical) ──────────────────────────────────────

/// Extract a hierarchical symbol tree from the program.
pub fn document_symbols(program: &Program, source: &str) -> Vec<DocumentSymbol> {
    let mut symbols = Vec::new();
    for stmt in &program.stmts {
        if let Some(sym) = stmt_to_document_symbol(stmt, source) {
            symbols.push(sym);
        }
    }
    symbols
}

fn stmt_to_document_symbol(stmt: &Stmt, source: &str) -> Option<DocumentSymbol> {
    match stmt {
        Stmt::Func {
            name,
            params,
            ret,
            body,
            span,
            ..
        } => {
            let full_name = name.join(".");
            let detail = func_detail(params, ret);
            let children = block_children(body, source);
            Some(DocumentSymbol {
                name: full_name,
                detail: Some(detail),
                kind: SymbolKind::FUNCTION,
                tags: None,
                deprecated: None,
                range: span_to_range(source, *span),
                selection_range: func_name_range(name, source),
                children: if children.is_empty() {
                    None
                } else {
                    Some(children)
                },
            })
        }
        Stmt::Struct {
            name, fields, span, ..
        } => {
            let full_name = name.join(".");
            let children: Vec<DocumentSymbol> = fields
                .iter()
                .map(|(fname, fty)| {
                    let detail = fmt_ty(fty);
                    DocumentSymbol {
                        name: fname.name.clone(),
                        detail: Some(detail),
                        kind: SymbolKind::FIELD,
                        tags: None,
                        deprecated: None,
                        range: span_to_range(source, fname.span),
                        selection_range: span_to_range(source, fname.span),
                        children: None,
                    }
                })
                .collect();
            Some(DocumentSymbol {
                name: full_name,
                detail: None,
                kind: SymbolKind::STRUCT,
                tags: None,
                deprecated: None,
                range: span_to_range(source, *span),
                selection_range: struct_name_range(name, source),
                children: if children.is_empty() {
                    None
                } else {
                    Some(children)
                },
            })
        }
        Stmt::Decl {
            name, value, span, ..
        } => {
            let kind = if is_const_expr(value) {
                SymbolKind::CONSTANT
            } else {
                SymbolKind::VARIABLE
            };
            Some(DocumentSymbol {
                name: name.name.clone(),
                detail: None,
                kind,
                tags: None,
                deprecated: None,
                range: span_to_range(source, *span),
                selection_range: span_to_range(source, name.span),
                children: None,
            })
        }
        Stmt::Import {
            path, alias, span, ..
        } => {
            let display = match alias {
                Some(a) => format!("{} as {}", path.join("."), a),
                None => path.join("."),
            };
            Some(DocumentSymbol {
                name: display,
                detail: None,
                kind: SymbolKind::MODULE,
                tags: None,
                deprecated: None,
                range: span_to_range(source, *span),
                selection_range: span_to_range(source, *span),
                children: None,
            })
        }
        Stmt::For {
            vars,
            iter,
            body,
            span,
        } => {
            let iter_detail = {
                let p = zz_frontend::printer::Printer::new(source);
                p.print_expr(iter)
            };
            let children = block_children(body, source);
            let var_name = if vars.len() == 1 {
                vars[0].name.clone()
            } else {
                let names: Vec<&str> = vars.iter().map(|v| v.name.as_str()).collect();
                names.join(", ")
            };
            Some(DocumentSymbol {
                name: var_name,
                detail: Some(format!("in {iter_detail}")),
                kind: SymbolKind::VARIABLE,
                tags: None,
                deprecated: None,
                range: span_to_range(source, *span),
                selection_range: span_to_range(source, vars[0].span),
                children: if children.is_empty() {
                    None
                } else {
                    Some(children)
                },
            })
        }
        Stmt::Expr(Expr::Call { .. }) => None,
        Stmt::Return { .. } | Stmt::Break { .. } | Stmt::Continue { .. } | Stmt::Defer { .. } => {
            None
        }
        Stmt::Assign { .. } => None,
        Stmt::Destructure { .. } => None,
        Stmt::Expr(_) => None,
    }
}

/// Recursively extract child symbols from a block.
fn block_children(block: &Block, source: &str) -> Vec<DocumentSymbol> {
    let mut children = Vec::new();
    for stmt in &block.stmts {
        if let Some(sym) = stmt_to_document_symbol(stmt, source) {
            children.push(sym);
        }
    }
    children
}

/// Build a function signature detail: `(x: int, y: int) -> int`.
fn func_detail(params: &[Param], ret: &Option<zz_frontend::ast::Ty>) -> String {
    let mut s = String::from("(");
    for (i, p) in params.iter().enumerate() {
        if i > 0 {
            s.push_str(", ");
        }
        s.push_str(&p.name.name);
        if let Some(ty) = &p.ty {
            s.push_str(&format!(": {}", fmt_ty(ty)));
        }
    }
    s.push(')');
    if let Some(ret_ty) = ret {
        s.push_str(&format!(" -> {}", fmt_ty(ret_ty)));
    }
    s
}

/// Check if an expression is a constant literal.
fn is_const_expr(expr: &Expr) -> bool {
    matches!(
        expr,
        Expr::Int { .. } | Expr::Float { .. } | Expr::Str { .. } | Expr::Bool { .. }
    )
}

/// Format a type annotation to a string.
fn fmt_ty(ty: &zz_frontend::ast::Ty) -> String {
    match &ty.kind {
        TyKind::Int => "int".into(),
        TyKind::Float => "float".into(),
        TyKind::Bool => "bool".into(),
        TyKind::Str => "str".into(),
        TyKind::Unit => "unit".into(),
        TyKind::Named(name, args) => {
            if args.is_empty() {
                name.clone()
            } else {
                let as_: Vec<String> = args.iter().map(fmt_ty).collect();
                format!("{}<{}>", name, as_.join(", "))
            }
        }
        TyKind::Option(inner) => format!("Option<{}>", fmt_ty(inner)),
        TyKind::Result(ok, err) => format!("Result<{}, {}>", fmt_ty(ok), fmt_ty(err)),
        TyKind::Array(inner) => format!("[{}]", fmt_ty(inner)),
        TyKind::Dict(k, v) => format!("{{{}: {}}}", fmt_ty(k), fmt_ty(v)),
        TyKind::Func(params, ret) => {
            let ps: Vec<String> = params.iter().map(fmt_ty).collect();
            format!("func({}) -> {}", ps.join(", "), fmt_ty(ret))
        }
        TyKind::Tuple(ts) => {
            let ps: Vec<String> = ts.iter().map(fmt_ty).collect();
            format!("({})", ps.join(", "))
        }
        TyKind::Union(ts) => {
            let ps: Vec<String> = ts.iter().map(fmt_ty).collect();
            ps.join(" | ")
        }
    }
}

/// Get the selection range for a function/struct name.
fn func_name_range(name: &[String], source: &str) -> Range {
    if let Some(first) = name.first() {
        if let Some(span) = find_name_in_source(source, first) {
            return span_to_range(source, span);
        }
    }
    Range {
        start: Position::new(0, 0),
        end: Position::new(0, 1),
    }
}

fn struct_name_range(name: &[String], source: &str) -> Range {
    func_name_range(name, source)
}

/// Find a standalone name token in source.
fn find_name_in_source(source: &str, name: &str) -> Option<Span> {
    let bytes = source.as_bytes();
    let name_bytes = name.as_bytes();
    let name_len = name_bytes.len() as u32;

    for i in 0..bytes.len() {
        if bytes[i..].starts_with(name_bytes) {
            let start = i as u32;
            let end = start + name_len;
            let prev_ok = i == 0 || !bytes[i - 1].is_ascii_alphanumeric();
            let next_ok =
                end as usize >= bytes.len() || !bytes[end as usize].is_ascii_alphanumeric();
            if prev_ok && next_ok {
                return Some(Span::new(start, end));
            }
        }
    }
    None
}

// ── Workspace symbols (flat) ─────────────────────────────────────────────

/// Collect all document symbols as a flat list for workspace symbol search.
pub fn workspace_symbols(
    program: &Program,
    source: &str,
    uri: &Url,
    _container_name: Option<String>,
) -> Vec<SymbolInformation> {
    let mut symbols = Vec::new();
    collect_workspace_symbols(program, source, uri, &mut symbols);
    symbols
}

fn collect_workspace_symbols(
    program: &Program,
    source: &str,
    uri: &Url,
    out: &mut Vec<SymbolInformation>,
) {
    for stmt in &program.stmts {
        match stmt {
            Stmt::Func {
                name,
                params,
                ret,
                span,
                ..
            } => {
                let full_name = name.join(".");
                let _detail = func_detail(params, ret);
                out.push(SymbolInformation {
                    name: full_name,
                    kind: SymbolKind::FUNCTION,
                    tags: None,
                    deprecated: None,
                    location: tower_lsp::lsp_types::Location {
                        uri: uri.clone(),
                        range: span_to_range(source, *span),
                    },
                    container_name: None,
                });
            }
            Stmt::Struct { name, span, .. } => {
                let full_name = name.join(".");
                out.push(SymbolInformation {
                    name: full_name,
                    kind: SymbolKind::STRUCT,
                    tags: None,
                    deprecated: None,
                    location: tower_lsp::lsp_types::Location {
                        uri: uri.clone(),
                        range: span_to_range(source, *span),
                    },
                    container_name: None,
                });
            }
            Stmt::Decl { name, span, .. } => {
                out.push(SymbolInformation {
                    name: name.name.clone(),
                    kind: SymbolKind::VARIABLE,
                    tags: None,
                    deprecated: None,
                    location: tower_lsp::lsp_types::Location {
                        uri: uri.clone(),
                        range: span_to_range(source, *span),
                    },
                    container_name: None,
                });
            }
            Stmt::Import {
                path, alias, span, ..
            } => {
                let display = match alias {
                    Some(a) => format!("{} as {}", path.join("."), a),
                    None => path.join("."),
                };
                out.push(SymbolInformation {
                    name: display,
                    kind: SymbolKind::MODULE,
                    tags: None,
                    deprecated: None,
                    location: tower_lsp::lsp_types::Location {
                        uri: uri.clone(),
                        range: span_to_range(source, *span),
                    },
                    container_name: None,
                });
            }
            _ => {}
        }
    }
}

/// Filter workspace symbols by a query string (case-insensitive substring).
pub fn filter_symbols(symbols: &[SymbolInformation], query: &str) -> Vec<SymbolInformation> {
    let lower = query.to_lowercase();
    symbols
        .iter()
        .filter(|s| s.name.to_lowercase().contains(&lower))
        .cloned()
        .collect()
}

// ── Tests ────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use zz_frontend::parse;

    fn make_source() -> &'static str {
        "func add(x: int, y: int) -> int {\n  return x + y\n}\n\nstruct Point {\n  x: int\n  y: int\n}\n\nPI := 3\n"
    }

    #[test]
    fn document_symbol_count() {
        let source = make_source();
        let parsed = parse(source);
        let syms = document_symbols(&parsed.program, source);
        assert_eq!(syms.len(), 3, "expected 3 top-level symbols");
    }

    #[test]
    fn func_symbol_kind() {
        let source = make_source();
        let parsed = parse(source);
        let syms = document_symbols(&parsed.program, source);
        let add = syms.iter().find(|s| s.name == "add").unwrap();
        assert_eq!(add.kind, SymbolKind::FUNCTION);
        assert!(add.detail.is_some());
        assert!(add.detail.as_ref().unwrap().contains("int"));
    }

    #[test]
    fn struct_symbol_has_field_children() {
        let source = make_source();
        let parsed = parse(source);
        let syms = document_symbols(&parsed.program, source);
        let point = syms.iter().find(|s| s.name == "Point").unwrap();
        assert_eq!(point.kind, SymbolKind::STRUCT);
        let children = point.children.as_ref().unwrap();
        assert_eq!(children.len(), 2);
        assert_eq!(children[0].name, "x");
        assert_eq!(children[0].kind, SymbolKind::FIELD);
    }

    #[test]
    fn global_let_is_variable() {
        let source = make_source();
        let parsed = parse(source);
        let syms = document_symbols(&parsed.program, source);
        let pi = syms.iter().find(|s| s.name == "PI").unwrap();
        assert_eq!(pi.kind, SymbolKind::CONSTANT);
    }

    #[test]
    fn workspace_symbol_search() {
        let source = make_source();
        let parsed = parse(source);
        let uri: Url = "file:///test.zz".parse().unwrap();
        let syms = workspace_symbols(&parsed.program, source, &uri, None);
        assert!(!syms.is_empty());
        let filtered = filter_symbols(&syms, "add");
        assert_eq!(filtered.len(), 1);
        assert_eq!(filtered[0].name, "add");
    }

    #[test]
    fn workspace_symbol_case_insensitive() {
        let source = make_source();
        let parsed = parse(source);
        let uri: Url = "file:///test.zz".parse().unwrap();
        let syms = workspace_symbols(&parsed.program, source, &uri, None);
        let filtered = filter_symbols(&syms, "POINT");
        assert_eq!(filtered.len(), 1);
        assert_eq!(filtered[0].name, "Point");
    }
}
