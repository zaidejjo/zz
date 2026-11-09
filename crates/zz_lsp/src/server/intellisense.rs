use std::collections::HashMap;
use std::sync::Arc;

use tower_lsp::jsonrpc::Result;
use tower_lsp::lsp_types::*;
use zz_checker::{SpanKey, Type, TOP_SCOPE};
use zz_hir::TypedProgram;

use super::Backend;

pub(crate) async fn handle_completion(
    backend: &Backend,
    params: CompletionParams,
) -> Result<Option<CompletionResponse>> {
    let uri = &params.text_document_position.text_document.uri;
    let pos = params.text_document_position.position;

    let doc = match backend.state.documents.get(uri) {
        Some(doc) => doc.clone(),
        None => return Ok(None),
    };
    let program = match &doc.program {
        Some(p) => p.clone(),
        None => return Ok(None),
    };

    // Self-heal when the async recheck hasn't populated `check_result` yet
    // (slow check, debounce skip, or a check that predates this edit):
    // run a synchronous single-file check so member completion (`math.`)
    // never returns zero items on a healthy document (#257). Scope
    // completion already answers without a result, but dot-access needs one.
    let owned_result;
    let owned_bindings: Option<HashMap<String, zz_checker::Type>>;
    let check_ref = match doc.check_result.as_ref() {
        Some(cr) => {
            owned_bindings = None;
            Some(cr)
        }
        None => {
            let doc_path = uri.to_file_path().ok();
            let (ib, ifunc, is, ia, ie) = backend
                .state
                .checker_seed_for_path(&program, doc_path.as_deref());
            owned_bindings = Some(ib.clone());
            owned_result = zz_checker::check_program(&program, ib, ifunc, is, ia, ie);
            Some(&owned_result)
        }
    };

    let offset = doc.line_index.position_to_offset(&doc.source, pos);
    let seed_bindings =
        owned_bindings.unwrap_or_else(|| doc.seed_bindings.clone().unwrap_or_default());
    let cenv = crate::completion::CompletionEnv {
        doc_dir: uri
            .to_file_path()
            .ok()
            .and_then(|p| p.parent().map(|d| d.to_path_buf())),
        seed_bindings,
    };
    let resp = crate::completion::completions_for_position(
        &program,
        &doc.source,
        offset,
        check_ref,
        Some(&cenv),
    );
    Ok(resp)
}

pub(crate) async fn handle_completion_resolve(
    backend: &Backend,
    mut item: CompletionItem,
) -> Result<CompletionItem> {
    for entry in backend.state.documents.iter() {
        let doc = entry.value();
        if doc.check_result.is_some() {
            crate::completion::resolve_completion_detail(&mut item, doc.check_result.as_ref());
            break;
        }
    }
    Ok(item)
}

pub(crate) async fn handle_signature_help(
    backend: &Backend,
    params: SignatureHelpParams,
) -> Result<Option<SignatureHelp>> {
    let uri = &params.text_document_position_params.text_document.uri;
    let pos = params.text_document_position_params.position;

    let doc = match backend.state.documents.get(uri) {
        Some(doc) => doc.clone(),
        None => return Ok(None),
    };
    let program = match &doc.program {
        Some(p) => p,
        None => return Ok(None),
    };

    let offset = doc.line_index.position_to_offset(&doc.source, pos);
    let help = crate::signature_help::signature_help_for_position(
        program,
        &doc.source,
        offset,
        doc.check_result.as_ref(),
    );
    Ok(help)
}

pub(crate) async fn handle_hover(backend: &Backend, params: HoverParams) -> Result<Option<Hover>> {
    use zz_frontend::ast::Expr;

    let uri = &params.text_document_position_params.text_document.uri;
    let pos = params.text_document_position_params.position;

    let doc = match backend.state.documents.get(uri) {
        Some(doc) => doc.clone(),
        None => return Ok(None),
    };
    let program = match &doc.program {
        Some(p) => p,
        None => return Ok(None),
    };
    let check_result = match &doc.check_result {
        Some(cr) => cr,
        None => return Ok(None),
    };

    let offset = doc.line_index.position_to_offset(&doc.source, pos);
    let node = crate::lookup::find_node_at(program, &doc.source, offset);

    // Hover on `?` / `try`: show the resolved error conversion (identity vs
    // `convert_to_` impl). `try_resolutions` is keyed by the `Try` span.
    if let Some(Expr::Try { span, .. }) = node.expr {
        if let Some(res) = check_result.try_resolutions.get(span) {
            let contents = match res {
                None => "**`try`**: identity conversion (same error type, zero-cost)".to_string(),
                Some(impl_span) => format!(
                    "**`try`**: converts via `convert_to_` impl defined at offset {}",
                    impl_span.start
                ),
            };
            return Ok(Some(Hover {
                contents: HoverContents::Markup(MarkupContent {
                    kind: MarkupKind::Markdown,
                    value: contents,
                }),
                range: None,
            }));
        }
    }

    let name = match &node.name {
        Some(n) => n.clone(),
        None => String::new(),
    };

    let mut contents = String::new();

    if !name.is_empty() {
        if let Some(sig) = zz_checker::lookup_std(&check_result.funcs, &name) {
            contents.push_str(&format!("**func** `{name}`\n\n"));
            contents.push_str("```zz\n");
            contents.push_str(&format!("func {name}("));
            for (i, (pname, pty)) in sig.params.iter().enumerate() {
                if i > 0 {
                    contents.push_str(", ");
                }
                contents.push_str(&format!("{pname}: {pty}"));
            }
            contents.push_str(&format!(") -> {}\n", sig.ret));
            contents.push_str("```\n");
        } else if let Some(sig) = check_result.structs.get(&name) {
            contents.push_str(&format!("**struct** `{name}`\n\n"));
            contents.push_str("```zz\n");
            contents.push_str(&format!("struct {name} {{\n"));
            for (fname, fty) in &sig.fields {
                contents.push_str(&format!("  {fname}: {fty}\n"));
            }
            contents.push_str("}\n");
            contents.push_str("```\n");
        } else if let Some(ty) = check_result.bindings.get(&name) {
            contents.push_str(&format!("**let** `{name}: {ty}`\n"));
        } else if let Some(Expr::Field {
            name: field, obj, ..
        }) = node.expr
        {
            if let Some(zz_checker::Type::Struct(struct_name, _)) =
                crate::lookup::resolve_type_of_expr(program, check_result, obj)
            {
                if let Some(ssig) = check_result.structs.get(&struct_name) {
                    if let Some((_, fty)) = ssig.fields.iter().find(|(n, _)| n == field) {
                        contents.push_str(&format!("**{struct_name}.{field}: {fty}**\n"));
                    }
                }
            }
        }
    }

    if !contents.is_empty() {
        return Ok(Some(Hover {
            contents: HoverContents::Markup(MarkupContent {
                kind: MarkupKind::Markdown,
                value: contents,
            }),
            range: None,
        }));
    }

    // 2. Arbitrary expressions: resolve the type via the scope-qualified
    //    `span_types` map (`TypedProgram::type_at`). Covers literals,
    //    calls, binaries, arrays, closures, and every other expression
    //    kind the checker assigned a type to.
    if let Some(expr) = node.expr {
        if let Some(ty_str) = expression_type_at(doc.span_types.as_ref(), program, expr, offset) {
            return Ok(Some(Hover {
                contents: HoverContents::Markup(MarkupContent {
                    kind: MarkupKind::Markdown,
                    value: format!("**expr**: `{ty_str}`\n"),
                }),
                range: None,
            }));
        }
    }

    Ok(None)
}

/// The qualified name of the function (or [`TOP_SCOPE`]) enclosing the
/// expression at `offset`. Used to build the `SpanKey` for type lookup.
fn enclosing_function(program: &zz_frontend::ast::Program, offset: u32) -> Arc<str> {
    for stmt in &program.stmts {
        if let Some(name) = func_containing_offset(stmt, offset) {
            return name;
        }
    }
    Arc::from(TOP_SCOPE)
}

fn func_containing_offset(stmt: &zz_frontend::ast::Stmt, offset: u32) -> Option<Arc<str>> {
    let span = stmt.span();
    if offset < span.start || offset >= span.end {
        return None;
    }
    match stmt {
        zz_frontend::ast::Stmt::Func { name, body, .. } => {
            for s in &body.stmts {
                if let Some(n) = func_containing_offset(s, offset) {
                    return Some(n);
                }
            }
            Some(Arc::from(name.join(".")))
        }
        _ => None,
    }
}

/// Resolve the type of an arbitrary expression via the
/// scope-qualified `span_types` map (`TypedProgram::type_at`).
fn expression_type_at(
    span_types: Option<&std::collections::HashMap<SpanKey, Type>>,
    program: &zz_frontend::ast::Program,
    expr: &zz_frontend::ast::Expr,
    offset: u32,
) -> Option<String> {
    let span_types = span_types?;
    let tp = TypedProgram {
        program: program.clone(),
        types: span_types.clone(),
        bindings: HashMap::new(),
        funcs: HashMap::new(),
        structs: HashMap::new(),
        enums: HashMap::new(),
        try_converts: HashMap::new(),
        module_origins: HashMap::new(),
    };
    let func = enclosing_function(program, offset);
    let ty = tp.type_at(&func, expr.span())?;
    Some(format!("{ty}"))
}
