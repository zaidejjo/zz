//! LSP server backend: handles all `textDocument/*` requests.

use std::sync::Arc;

use tower_lsp::jsonrpc::Result;
use tower_lsp::lsp_types::*;
use tower_lsp::{Client, LanguageServer};

use crate::diagnostics::recheck_and_publish;
use crate::state::GlobalState;
use zz_frontend::ast::Expr;

/// The ZZ language server backend.
pub struct Backend {
    client: Client,
    state: Arc<GlobalState>,
}

impl Backend {
    pub fn new(client: Client) -> Self {
        Self {
            client,
            state: Arc::new(GlobalState::new()),
        }
    }
}

#[tower_lsp::async_trait]
impl LanguageServer for Backend {
    async fn initialize(&self, params: InitializeParams) -> Result<InitializeResult> {
        // Capture workspace root.
        if let Some(folders) = &params.workspace_folders {
            if let Some(folder) = folders.first() {
                if let Ok(path) = folder.uri.to_file_path() {
                    self.state.set_root(path);
                }
            }
        } else if let Some(uri) = &params.root_uri {
            if let Ok(path) = uri.to_file_path() {
                self.state.set_root(path);
            }
        }

        self.client
            .log_message(MessageType::INFO, "zz-lsp initialized")
            .await;

        Ok(InitializeResult {
            capabilities: ServerCapabilities {
                text_document_sync: Some(TextDocumentSyncCapability::Options(
                    TextDocumentSyncOptions {
                        open_close: Some(true),
                        change: Some(TextDocumentSyncKind::FULL),
                        save: Some(TextDocumentSyncSaveOptions::Supported(true)),
                        ..Default::default()
                    },
                )),
                hover_provider: Some(HoverProviderCapability::Simple(true)),
                definition_provider: Some(OneOf::Left(true)),
                code_action_provider: Some(CodeActionProviderCapability::Simple(true)),
                document_symbol_provider: Some(OneOf::Left(true)),
                workspace_symbol_provider: Some(OneOf::Left(true)),
                rename_provider: Some(OneOf::Left(true)),
                document_highlight_provider: Some(OneOf::Left(true)),
                completion_provider: Some(CompletionOptions {
                    resolve_provider: Some(true),
                    trigger_characters: Some(vec![".".to_string()]),
                    ..Default::default()
                }),
                signature_help_provider: Some(SignatureHelpOptions {
                    trigger_characters: Some(vec!["(".to_string(), ",".to_string()]),
                    retrigger_characters: None,
                    work_done_progress_options: WorkDoneProgressOptions::default(),
                }),
                ..Default::default()
            },
            server_info: Some(ServerInfo {
                name: "zz-lsp".to_string(),
                version: Some(env!("CARGO_PKG_VERSION").to_string()),
            }),
        })
    }

    async fn initialized(&self, _: InitializedParams) {
        self.client
            .log_message(MessageType::INFO, "zz-lsp ready")
            .await;
    }

    async fn did_open(&self, params: DidOpenTextDocumentParams) {
        let uri = params.text_document.uri;
        let version = params.text_document.version;
        let text = params.text_document.text;

        let (_, old_defs) = self.state.update_document(uri.clone(), version, text);
        // Prune any symbols the previous version of this file contributed.
        if let Some(defs) = &old_defs {
            self.state.prune_defs(defs);
        }

        recheck_and_publish(self.state.clone(), &self.client, uri).await;
    }

    async fn did_change(&self, params: DidChangeTextDocumentParams) {
        let uri = params.text_document.uri;
        let version = params.text_document.version;

        // FULL sync: single change with the entire new content.
        let text = params
            .content_changes
            .into_iter()
            .next()
            .map(|c| c.text)
            .unwrap_or_default();

        let (_, old_defs) = self.state.update_document(uri.clone(), version, text);
        // Prune any symbols the previous version of this file contributed.
        if let Some(defs) = &old_defs {
            self.state.prune_defs(defs);
        }

        recheck_and_publish(self.state.clone(), &self.client, uri).await;
    }

    async fn did_save(&self, params: DidSaveTextDocumentParams) {
        recheck_and_publish(self.state.clone(), &self.client, params.text_document.uri).await;
    }

    async fn did_close(&self, params: DidCloseTextDocumentParams) {
        // Remove document and prune its definitions from the global seed.
        if let Some(defs) = self.state.remove_document(&params.text_document.uri) {
            self.state.prune_defs(&defs);
        }
        // Clear diagnostics for the closed file.
        self.client
            .publish_diagnostics(params.text_document.uri, vec![], None)
            .await;
    }

    async fn hover(&self, params: HoverParams) -> Result<Option<Hover>> {
        let uri = &params.text_document_position_params.text_document.uri;
        let pos = params.text_document_position_params.position;

        let doc = match self.state.documents.get(uri) {
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

        // Build hover content from type info.
        let mut contents = String::new();

        // Check function signatures first.
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
            // Struct definition.
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
            // Resolve struct field type.
            if let Some(zz_checker::Type::Struct(struct_name)) =
                crate::lookup::resolve_type_of_expr(program, check_result, obj)
            {
                if let Some(ssig) = check_result.structs.get(&struct_name) {
                    if let Some((_, fty)) =
                        ssig.fields.iter().find(|(n, _)| n == field)
                    {
                        contents.push_str(&format!(
                            "**{struct_name}.{field}: {fty}**\n"
                        ));
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

    async fn goto_definition(
        &self,
        params: GotoDefinitionParams,
    ) -> Result<Option<GotoDefinitionResponse>> {
        let uri = &params.text_document_position_params.text_document.uri;
        let pos = params.text_document_position_params.position;

        let doc = match self.state.documents.get(uri) {
            Some(doc) => doc.clone(),
            None => return Ok(None),
        };
        let program = match &doc.program {
            Some(p) => p,
            None => return Ok(None),
        };

        let offset = doc.line_index.position_to_offset(&doc.source, pos);
        let node = crate::lookup::find_node_at(program, &doc.source, offset);
        let name = match &node.name {
            Some(n) => n.clone(),
            None => return Ok(None),
        };

        // Collect all definitions and find the one matching this name.
        let defs = crate::lookup::collect_definitions(program, &doc.source);
        let def = defs.values().find(|d| d.name == name);

        // For struct fields (`s.field`), resolve through the struct.
        let def = if def.is_none() {
            if let Some(Expr::Field { name: field, obj, .. }) = node.expr {
                // Resolve the object type to find which struct it is.
                if let Some(check_result) = &doc.check_result {
                    if let Some(zz_checker::Type::Struct(struct_name)) =
                        crate::lookup::resolve_type_of_expr(program, check_result, obj)
                    {
                        // Find the struct definition and its field span.
                        if let Some(ssig) = check_result.structs.get(&struct_name) {
                            if let Some((fname, _)) =
                                ssig.fields.iter().find(|(n, _)| n == field)
                            {
                                // Search for the field in the struct definition.
                                for stmt in &program.stmts {
                                    if let zz_frontend::ast::Stmt::Struct {
                                        name: sname,
                                        fields,
                                        ..
                                    } = stmt
                                    {
                                        if sname.join(".") == struct_name {
                                            for (fi_name, _) in fields {
                                                if fi_name.name == *fname {
                                                    return Ok(Some(
                                                        GotoDefinitionResponse::Scalar(
                                                            Location {
                                                                uri: uri.clone(),
                                                                range: crate::convert::span_to_range(
                                                                    &doc.source,
                                                                    fi_name.span,
                                                                ),
                                                            },
                                                        ),
                                                    ));
                                                }
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
                None
            } else {
                None
            }
        } else {
            def
        };

        let def = match def {
            Some(d) => d,
            None => return Ok(None),
        };

        Ok(Some(GotoDefinitionResponse::Scalar(Location {
            uri: uri.clone(),
            range: doc.line_index.span_to_range(&doc.source, def.span),
        })))
    }

    async fn code_action(&self, params: CodeActionParams) -> Result<Option<CodeActionResponse>> {
        let uri = &params.text_document.uri;
        let source = match self.state.documents.get(uri) {
            Some(doc) => doc.source.clone(),
            None => return Ok(None),
        };

        let actions = crate::code_action::code_actions_for_range(&params, &source, uri);
        if actions.is_empty() {
            Ok(None)
        } else {
            Ok(Some(actions))
        }
    }

    async fn document_symbol(
        &self,
        params: DocumentSymbolParams,
    ) -> Result<Option<DocumentSymbolResponse>> {
        let uri = &params.text_document.uri;
        let doc = match self.state.documents.get(uri) {
            Some(doc) => doc.clone(),
            None => return Ok(None),
        };
        let program = match &doc.program {
            Some(p) => p,
            None => return Ok(None),
        };

        let symbols = crate::symbols::document_symbols(program, &doc.source);
        Ok(Some(DocumentSymbolResponse::Nested(symbols)))
    }

    async fn symbol(&self, params: WorkspaceSymbolParams) -> Result<Option<Vec<SymbolInformation>>> {
        let query = &params.query;
        let mut all_symbols = Vec::new();

        // Collect symbols from all open documents.
        for entry in self.state.documents.iter() {
            let doc = entry.value();
            let program = match &doc.program {
                Some(p) => p,
                None => continue,
            };
            let syms = crate::symbols::workspace_symbols(
                program,
                &doc.source,
                &doc.uri,
                None,
            );
            all_symbols.extend(syms);
        }

        let filtered = crate::symbols::filter_symbols(&all_symbols, query);
        Ok(Some(filtered))
    }

    async fn references(&self, params: ReferenceParams) -> Result<Option<Vec<Location>>> {
        let uri = &params.text_document_position.text_document.uri;
        let pos = params.text_document_position.position;

        let doc = match self.state.documents.get(uri) {
            Some(doc) => doc.clone(),
            None => return Ok(None),
        };
        let program = match &doc.program {
            Some(p) => p,
            None => return Ok(None),
        };

        let offset = doc.line_index.position_to_offset(&doc.source, pos);
        let refs = crate::lookup::find_references_in_program(program, &doc.source, offset);

        // LSP spec: return empty array, not null.
        let locations: Vec<Location> = refs
            .iter()
            .map(|r| Location {
                uri: uri.clone(),
                range: doc.line_index.span_to_range(&doc.source, r.span),
            })
            .collect();

        Ok(Some(locations))
    }

    async fn prepare_rename(
        &self,
        params: TextDocumentPositionParams,
    ) -> Result<Option<PrepareRenameResponse>> {
        let uri = &params.text_document.uri;
        let pos = params.position;

        let doc = match self.state.documents.get(uri) {
            Some(doc) => doc.clone(),
            None => return Ok(None),
        };
        let program = match &doc.program {
            Some(p) => p,
            None => return Ok(None),
        };

        let offset = doc.line_index.position_to_offset(&doc.source, pos);
        let node = crate::lookup::find_node_at(program, &doc.source, offset);

        // Only allow rename if cursor is on a named symbol.
        match &node.name {
            Some(name) => {
                let name_span = node.name_span.unwrap_or_else(|| {
                    zz_frontend::span::Span::new(offset, offset + name.len() as u32)
                });
                Ok(Some(PrepareRenameResponse::RangeWithPlaceholder {
                    range: doc.line_index.span_to_range(&doc.source, name_span),
                    placeholder: name.clone(),
                }))
            }
            None => Ok(None),
        }
    }

    async fn rename(&self, params: RenameParams) -> Result<Option<WorkspaceEdit>> {
        let uri = &params.text_document_position.text_document.uri;
        let pos = params.text_document_position.position;
        let new_name = &params.new_name;

        let doc = match self.state.documents.get(uri) {
            Some(doc) => doc.clone(),
            None => return Ok(None),
        };
        let program = match &doc.program {
            Some(p) => p,
            None => return Ok(None),
        };

        let offset = doc.line_index.position_to_offset(&doc.source, pos);
        let refs = crate::lookup::find_references_in_program(program, &doc.source, offset);

        if refs.is_empty() {
            return Ok(None);
        }

        let edits: Vec<TextEdit> = refs
            .iter()
            .map(|r| TextEdit {
                range: doc.line_index.span_to_range(&doc.source, r.span),
                new_text: new_name.clone(),
            })
            .collect();

        let mut changes = std::collections::HashMap::new();
        changes.insert(uri.clone(), edits);

        Ok(Some(WorkspaceEdit {
            changes: Some(changes),
            ..Default::default()
        }))
    }

    async fn document_highlight(
        &self,
        params: DocumentHighlightParams,
    ) -> Result<Option<Vec<DocumentHighlight>>> {
        let uri = &params.text_document_position_params.text_document.uri;
        let pos = params.text_document_position_params.position;

        let doc = match self.state.documents.get(uri) {
            Some(doc) => doc.clone(),
            None => return Ok(None),
        };
        let program = match &doc.program {
            Some(p) => p,
            None => return Ok(None),
        };

        let offset = doc.line_index.position_to_offset(&doc.source, pos);
        let highlights = crate::lookup::find_highlights_in_program(program, &doc.source, offset);

        let result: Vec<DocumentHighlight> = highlights
            .iter()
            .map(|h| DocumentHighlight {
                range: doc.line_index.span_to_range(&doc.source, h.span),
                kind: Some(match h.kind {
                    crate::lookup::HighlightKind::Read => DocumentHighlightKind::READ,
                    crate::lookup::HighlightKind::Write => DocumentHighlightKind::WRITE,
                }),
            })
            .collect();

        Ok(Some(result))
    }

    async fn completion(&self, params: CompletionParams) -> Result<Option<CompletionResponse>> {
        let uri = &params.text_document_position.text_document.uri;
        let pos = params.text_document_position.position;

        let doc = match self.state.documents.get(uri) {
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

    async fn completion_resolve(&self, mut item: CompletionItem) -> Result<CompletionItem> {
        // Try to resolve rich detail from any open document's check result.
        for entry in self.state.documents.iter() {
            let doc = entry.value();
            if doc.check_result.is_some() {
                crate::completion::resolve_completion_detail(&mut item, doc.check_result.as_ref());
                break;
            }
        }
        Ok(item)
    }

    async fn signature_help(&self, params: SignatureHelpParams) -> Result<Option<SignatureHelp>> {
        let uri = &params.text_document_position_params.text_document.uri;
        let pos = params.text_document_position_params.position;

        let doc = match self.state.documents.get(uri) {
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

    async fn shutdown(&self) -> Result<()> {
        Ok(())
    }
}
