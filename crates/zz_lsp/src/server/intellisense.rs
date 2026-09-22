use tower_lsp::jsonrpc::Result;
use tower_lsp::lsp_types::*;

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
        Some(p) => p,
        None => return Ok(None),
    };

    let offset = doc.line_index.position_to_offset(&doc.source, pos);
    let resp = crate::completion::completions_for_position(
        program,
        &doc.source,
        offset,
        doc.check_result.as_ref(),
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
            crate::completion::resolve_completion_detail(
                &mut item,
                doc.check_result.as_ref(),
            );
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

pub(crate) async fn handle_hover(
    backend: &Backend,
    params: HoverParams,
) -> Result<Option<Hover>> {
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
    let name = match &node.name {
        Some(n) => n.clone(),
        None => return Ok(None),
    };

    let mut contents = String::new();

    if let Some(sig) = check_result.funcs.get(&name) {
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
    } else if let Some(Expr::Field { name: field, obj, .. }) = node.expr {
        if let Some(zz_checker::Type::Struct(struct_name)) =
            crate::lookup::resolve_type_of_expr(program, check_result, obj)
        {
            if let Some(ssig) = check_result.structs.get(&struct_name) {
                if let Some((_, fty)) = ssig.fields.iter().find(|(n, _)| n == field) {
                    contents.push_str(&format!("**{struct_name}.{field}: {fty}**\n"));
                }
            }
        }
    }

    if contents.is_empty() {
        return Ok(None);
    }

    Ok(Some(Hover {
        contents: HoverContents::Markup(MarkupContent {
            kind: MarkupKind::Markdown,
            value: contents,
        }),
        range: None,
    }))
}
