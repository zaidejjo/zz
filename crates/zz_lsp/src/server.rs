//! LSP server backend: handles all `textDocument/*` requests.

use std::sync::Arc;

use tower_lsp::jsonrpc::Result;
use tower_lsp::lsp_types::*;
use tower_lsp::{Client, LanguageServer};

use crate::convert::LineIndex;
use crate::diagnostics::recheck_and_publish;
use crate::state::{DocumentState, GlobalState};
use zz_frontend::ast::{Expr, Stmt};

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

        // Scan workspace for .zz files to build cross-file index.
        self.state.scan_workspace();

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

    async fn did_change_workspace_folders(&self, params: DidChangeWorkspaceFoldersParams) {
        // Re-scan workspace on folder changes.
        let _ = params;
        self.state.scan_workspace();
        self.client
            .log_message(MessageType::INFO, "workspace folders changed, rescan complete")
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

        // Check if cursor is on an import statement — resolve cross-file.
        if let Some(Stmt::Import { path, .. }) = node.stmt {
            let root = self.state.root.read().unwrap().clone();
            if let Some(root) = root {
                let index = self.state.module_index.read().unwrap();
                if let Some(target_uri) = index.resolve_import(path, &root) {
                    // Navigate to first definition in the imported file,
                    // or the first line of the file.
                    let target_loc = if let Some(entry) = index.entries.get(&target_uri) {
                        if let Some(def) = entry.definitions.first() {
                            let target_doc = DocumentState {
                                uri: target_uri.clone(),
                                version: 0,
                                source: entry.source.clone(),
                                parse_errors: vec![],
                                program: entry.program.clone(),
                                check_result: None,
                                file_defs: None,
                                line_index: LineIndex::new(&entry.source),
                            };
                            Location {
                                uri: target_uri,
                                range: target_doc.line_index.span_to_range(&entry.source, def.span),
                            }
                        } else {
                            Location {
                                uri: target_uri,
                                range: Range::default(),
                            }
                        }
                    } else {
                        return Ok(None);
                    };
                    return Ok(Some(GotoDefinitionResponse::Scalar(target_loc)));
                }
            }
        }

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

        // Also collect symbols from indexed (non-open) workspace files.
        {
            let index = self.state.module_index.read().unwrap();
            for (uri, entry) in &index.entries {
                if self.state.documents.contains_key(uri) {
                    continue; // Already collected above.
                }
                if let Some(program) = &entry.program {
                    let syms = crate::symbols::workspace_symbols(
                        program,
                        &entry.source,
                        uri,
                        None,
                    );
                    all_symbols.extend(syms);
                }
            }
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

        let mut locations: Vec<Location> = refs
            .iter()
            .map(|r| Location {
                uri: uri.clone(),
                range: doc.line_index.span_to_range(&doc.source, r.span),
            })
            .collect();

        // Cross-file: find references in all other indexed files.
        let name = crate::lookup::find_node_at(program, &doc.source, offset)
            .name
            .unwrap_or_default();
        if !name.is_empty() {
            let index = self.state.module_index.read().unwrap();
            for (target_uri, entry) in &index.entries {
                if target_uri == uri {
                    continue; // Skip current file.
                }
                if let Some(target_program) = &entry.program {
                    let target_source = &entry.source;
                    let target_line_index = LineIndex::new(target_source);
                    let target_refs = crate::lookup::find_references_to_name_in_program(
                        target_program,
                        target_source,
                        &name,
                    );
                    for r in &target_refs {
                        locations.push(Location {
                            uri: target_uri.clone(),
                            range: target_line_index.span_to_range(target_source, r.span),
                        });
                    }
                }
            }
        }

        // LSP spec: return empty array, not null.
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

        let mut changes: std::collections::HashMap<Url, Vec<TextEdit>> =
            std::collections::HashMap::new();

        // Edits for the current file.
        let edits: Vec<TextEdit> = refs
            .iter()
            .map(|r| TextEdit {
                range: doc.line_index.span_to_range(&doc.source, r.span),
                new_text: new_name.clone(),
            })
            .collect();
        if !edits.is_empty() {
            changes.insert(uri.clone(), edits);
        }

        // Cross-file: find and rename references in all other indexed files.
        let name = crate::lookup::find_node_at(program, &doc.source, offset)
            .name
            .unwrap_or_default();
        if !name.is_empty() {
            let index = self.state.module_index.read().unwrap();
            for (target_uri, entry) in &index.entries {
                if target_uri == uri {
                    continue;
                }
                if let Some(target_program) = &entry.program {
                    let target_source = &entry.source;
                    let target_line_index = LineIndex::new(target_source);
                    let target_refs = crate::lookup::find_references_to_name_in_program(
                        target_program,
                        target_source,
                        &name,
                    );
                    let mut target_edits = Vec::new();
                    for r in &target_refs {
                        target_edits.push(TextEdit {
                            range: target_line_index.span_to_range(target_source, r.span),
                            new_text: new_name.clone(),
                        });
                    }
                    if !target_edits.is_empty() {
                        changes.insert(target_uri.clone(), target_edits);
                    }
                }
            }
        }

        if changes.is_empty() {
            return Ok(None);
        }

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

// ── Tests ────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::GlobalState;
    use tower_lsp::LspService;

    /// Create a service + backend pair for testing.
    fn setup() -> (tower_lsp::LspService<Backend>, std::sync::Arc<GlobalState>) {
        let (service, _) = LspService::new(|client| Backend::new(client));
        let state = Arc::clone(&service.inner().state);
        (service, state)
    }

    /// Open a document in the global state and run a check so hover/go-to work.
    fn open_and_check(state: &GlobalState, uri: &Url, source: &str) {
        state.update_document(uri.clone(), 1, source.to_string());
        let (ib, ifunc, is) = state.checker_seed();
        // Use the program already stored in the document by update_document.
        let cr = {
            let doc = state.documents.get(uri).unwrap();
            let program = doc.program.as_ref().unwrap().clone();
            zz_checker::check_program(&program, ib, ifunc, is)
        };
        if let Some(mut doc) = state.documents.get_mut(uri) {
            doc.check_result = Some(cr.clone());
        }
        state.absorb_result(uri, &cr);
    }

    // ── Initialize ───────────────────────────────────────────────────

    #[tokio::test(flavor = "current_thread")]
    async fn initialize_returns_capabilities() {
        let (service, _) = setup();
        let result = service
            .inner()
            .initialize(InitializeParams::default())
            .await
            .unwrap();
        let caps = result.capabilities;
        assert!(caps.hover_provider.is_some());
        assert!(caps.definition_provider.is_some());
        assert!(caps.code_action_provider.is_some());
        assert!(caps.document_symbol_provider.is_some());
        assert!(caps.workspace_symbol_provider.is_some());
        assert!(caps.rename_provider.is_some());
        assert!(caps.document_highlight_provider.is_some());
        assert!(caps.completion_provider.is_some());
        assert!(caps.signature_help_provider.is_some());
    }

    #[tokio::test(flavor = "current_thread")]
    async fn initialize_sets_root_from_workspace_folders() {
        let (service, state) = setup();
        let params = InitializeParams {
            workspace_folders: Some(vec![WorkspaceFolder {
                uri: Url::from_file_path("/tmp/myproject").unwrap(),
                name: "myproject".to_string(),
            }]),
            ..Default::default()
        };
        service.inner().initialize(params).await.unwrap();
        let root = state.root.read().unwrap();
        assert_eq!(*root, Some(std::path::PathBuf::from("/tmp/myproject")));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn initialize_sets_root_from_root_uri() {
        let (service, state) = setup();
        let params = InitializeParams {
            root_uri: Some(Url::from_file_path("/tmp/alt").unwrap()),
            ..Default::default()
        };
        service.inner().initialize(params).await.unwrap();
        let root = state.root.read().unwrap();
        assert_eq!(*root, Some(std::path::PathBuf::from("/tmp/alt")));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn server_info_has_version() {
        let (service, _) = setup();
        let result = service
            .inner()
            .initialize(InitializeParams::default())
            .await
            .unwrap();
        let info = result.server_info.unwrap();
        assert_eq!(info.name, "zz-lsp");
        assert!(info.version.is_some());
    }

    // ── Shutdown ─────────────────────────────────────────────────────

    #[tokio::test(flavor = "current_thread")]
    async fn shutdown_succeeds() {
        let (service, _) = setup();
        let result = service.inner().shutdown().await;
        assert!(result.is_ok());
    }

    // ── Hover ────────────────────────────────────────────────────────

    #[tokio::test(flavor = "current_thread")]
    async fn hover_function() {
        let (service, state) = setup();
        let uri: Url = "file:///test.zz".parse().unwrap();
        let src = "func add(a: int, b: int) -> int { return a + b }\n";
        open_and_check(&state, &uri, src);

        let pos = Position {
            line: 0,
            character: 5, // on "add"
        };
        let hover = service
            .inner()
            .hover(HoverParams {
                text_document_position_params: TextDocumentPositionParams {
                    text_document: TextDocumentIdentifier { uri },
                    position: pos,
                },
                work_done_progress_params: WorkDoneProgressParams::default(),
            })
            .await
            .unwrap();
        let hover = hover.expect("should have hover");
        match hover.contents {
            HoverContents::Markup(m) => {
                assert!(m.value.contains("func"));
                assert!(m.value.contains("add"));
            }
            _ => panic!("expected markup content"),
        }
    }

    #[tokio::test(flavor = "current_thread")]
    async fn hover_struct() {
        let (service, state) = setup();
        let uri: Url = "file:///test.zz".parse().unwrap();
        let src = "struct Point { x: int, y: int }\n";
        open_and_check(&state, &uri, src);

        let pos = Position {
            line: 0,
            character: 7, // on "Point"
        };
        let hover = service
            .inner()
            .hover(HoverParams {
                text_document_position_params: TextDocumentPositionParams {
                    text_document: TextDocumentIdentifier { uri },
                    position: pos,
                },
                work_done_progress_params: WorkDoneProgressParams::default(),
            })
            .await
            .unwrap();
        let hover = hover.expect("should have hover for struct");
        match hover.contents {
            HoverContents::Markup(m) => {
                assert!(m.value.contains("struct"));
                assert!(m.value.contains("Point"));
            }
            _ => panic!("expected markup content"),
        }
    }

    #[tokio::test(flavor = "current_thread")]
    async fn hover_let_binding() {
        let (service, state) = setup();
        let uri: Url = "file:///test.zz".parse().unwrap();
        let src = "let x = 42\n";
        open_and_check(&state, &uri, src);

        let pos = Position {
            line: 0,
            character: 4, // on "x"
        };
        let hover = service
            .inner()
            .hover(HoverParams {
                text_document_position_params: TextDocumentPositionParams {
                    text_document: TextDocumentIdentifier { uri },
                    position: pos,
                },
                work_done_progress_params: WorkDoneProgressParams::default(),
            })
            .await
            .unwrap();
        let hover = hover.expect("should have hover for let");
        match hover.contents {
            HoverContents::Markup(m) => {
                assert!(m.value.contains("x"));
            }
            _ => panic!("expected markup content"),
        }
    }

    #[tokio::test(flavor = "current_thread")]
    async fn hover_missing_document_returns_none() {
        let (service, _) = setup();
        let uri: Url = "file:///nonexistent.zz".parse().unwrap();
        let hover = service
            .inner()
            .hover(HoverParams {
                text_document_position_params: TextDocumentPositionParams {
                    text_document: TextDocumentIdentifier { uri },
                    position: Position::default(),
                },
                work_done_progress_params: WorkDoneProgressParams::default(),
            })
            .await
            .unwrap();
        assert!(hover.is_none());
    }

    // ── Go-to-definition ─────────────────────────────────────────────

    #[tokio::test(flavor = "current_thread")]
    async fn goto_definition_function() {
        let (service, state) = setup();
        let uri: Url = "file:///test.zz".parse().unwrap();
        let src = "func add(a: int, b: int) -> int { return a + b }\nlet x = add(1, 2)\n";
        open_and_check(&state, &uri, src);

        let pos = Position {
            line: 1,
            character: 8, // on "add" in call
        };
        let def = service
            .inner()
            .goto_definition(GotoDefinitionParams {
                text_document_position_params: TextDocumentPositionParams {
                    text_document: TextDocumentIdentifier { uri: uri.clone() },
                    position: pos,
                },
                work_done_progress_params: WorkDoneProgressParams::default(),
                partial_result_params: PartialResultParams::default(),
            })
            .await
            .unwrap();
        let def = def.expect("should have definition");
        match def {
            GotoDefinitionResponse::Scalar(loc) => {
                assert_eq!(loc.uri, uri);
                assert!(loc.range.start.line == 0);
            }
            _ => panic!("expected scalar location"),
        }
    }

    #[tokio::test(flavor = "current_thread")]
    async fn goto_definition_struct_field() {
        let (service, state) = setup();
        let uri: Url = "file:///test.zz".parse().unwrap();
        let src = "struct Point { x: int, y: int }\nlet p = Point{ x: 1, y: 2 }\nlet v = p.x\n";
        open_and_check(&state, &uri, src);

        let pos = Position {
            line: 2,
            character: 8, // on "x" in p.x
        };
        let def = service
            .inner()
            .goto_definition(GotoDefinitionParams {
                text_document_position_params: TextDocumentPositionParams {
                    text_document: TextDocumentIdentifier { uri: uri.clone() },
                    position: pos,
                },
                work_done_progress_params: WorkDoneProgressParams::default(),
                partial_result_params: PartialResultParams::default(),
            })
            .await
            .unwrap();
        // Should find the field definition in the struct.
        assert!(def.is_some(), "should resolve struct field go-to-definition");
    }

    #[tokio::test(flavor = "current_thread")]
    async fn goto_definition_missing_doc_returns_none() {
        let (service, _) = setup();
        let uri: Url = "file:///no.zz".parse().unwrap();
        let def = service
            .inner()
            .goto_definition(GotoDefinitionParams {
                text_document_position_params: TextDocumentPositionParams {
                    text_document: TextDocumentIdentifier { uri },
                    position: Position::default(),
                },
                work_done_progress_params: WorkDoneProgressParams::default(),
                partial_result_params: PartialResultParams::default(),
            })
            .await
            .unwrap();
        assert!(def.is_none());
    }

    // ── Document symbol ──────────────────────────────────────────────

    #[tokio::test(flavor = "current_thread")]
    async fn document_symbol_lists_functions_and_structs() {
        let (service, state) = setup();
        let uri: Url = "file:///test.zz".parse().unwrap();
        let src = "func add(a: int, b: int) -> int { return a + b }\nstruct Point { x: int, y: int }\nlet c = 1\n";
        open_and_check(&state, &uri, src);

        let syms = service
            .inner()
            .document_symbol(DocumentSymbolParams {
                text_document: TextDocumentIdentifier { uri },
                work_done_progress_params: WorkDoneProgressParams::default(),
                partial_result_params: PartialResultParams::default(),
            })
            .await
            .unwrap();
        let syms = syms.expect("should have symbols");
        match syms {
            DocumentSymbolResponse::Nested(syms) => {
                assert!(syms.len() >= 3, "should have add, Point, c");
                let names: Vec<_> = syms.iter().map(|s| s.name.clone()).collect();
                assert!(names.contains(&"add".to_string()));
                assert!(names.contains(&"Point".to_string()));
                assert!(names.contains(&"c".to_string()));
            }
            DocumentSymbolResponse::Flat(_) => panic!("expected nested symbols"),
        }
    }

    #[tokio::test(flavor = "current_thread")]
    async fn document_symbol_missing_doc_returns_none() {
        let (service, _) = setup();
        let uri: Url = "file:///no.zz".parse().unwrap();
        let syms = service
            .inner()
            .document_symbol(DocumentSymbolParams {
                text_document: TextDocumentIdentifier { uri },
                work_done_progress_params: WorkDoneProgressParams::default(),
                partial_result_params: PartialResultParams::default(),
            })
            .await
            .unwrap();
        assert!(syms.is_none());
    }

    // ── Workspace symbol ─────────────────────────────────────────────

    #[tokio::test(flavor = "current_thread")]
    async fn workspace_symbol_search() {
        let (service, state) = setup();
        let uri: Url = "file:///test.zz".parse().unwrap();
        let src = "func add(a: int, b: int) -> int { return a + b }\nstruct Adder { val: int }\n";
        open_and_check(&state, &uri, src);

        let syms = service
            .inner()
            .symbol(WorkspaceSymbolParams {
                query: "add".to_string(),
                work_done_progress_params: WorkDoneProgressParams::default(),
                partial_result_params: PartialResultParams::default(),
            })
            .await
            .unwrap();
        let syms = syms.expect("should have symbols");
        assert!(syms.len() >= 2, "should find add and Adder");
    }

    // ── References ───────────────────────────────────────────────────

    #[tokio::test(flavor = "current_thread")]
    async fn references_finds_usages() {
        let (service, state) = setup();
        let uri: Url = "file:///test.zz".parse().unwrap();
        let src = "func id(x: int) -> int { return x }\nlet a = id(1)\nlet b = id(2)\n";
        open_and_check(&state, &uri, src);

        // Cursor on the `id` function definition.
        let pos = Position {
            line: 0,
            character: 5,
        };
        let refs = service
            .inner()
            .references(ReferenceParams {
                text_document_position: TextDocumentPositionParams {
                    text_document: TextDocumentIdentifier { uri: uri.clone() },
                    position: pos,
                },
                context: ReferenceContext {
                    include_declaration: true,
                },
                work_done_progress_params: WorkDoneProgressParams::default(),
                partial_result_params: PartialResultParams::default(),
            })
            .await
            .unwrap();
        let refs = refs.expect("should have references");
        assert!(refs.len() >= 3, "should find def + 2 call sites");
    }

    #[tokio::test(flavor = "current_thread")]
    async fn references_missing_doc_returns_none() {
        let (service, _) = setup();
        let uri: Url = "file:///no.zz".parse().unwrap();
        let refs = service
            .inner()
            .references(ReferenceParams {
                text_document_position: TextDocumentPositionParams {
                    text_document: TextDocumentIdentifier { uri },
                    position: Position::default(),
                },
                context: ReferenceContext {
                    include_declaration: true,
                },
                work_done_progress_params: WorkDoneProgressParams::default(),
                partial_result_params: PartialResultParams::default(),
            })
            .await
            .unwrap();
        assert!(refs.is_none());
    }

    // ── Prepare rename ───────────────────────────────────────────────

    #[tokio::test(flavor = "current_thread")]
    async fn prepare_rename_on_symbol() {
        let (service, state) = setup();
        let uri: Url = "file:///test.zz".parse().unwrap();
        let src = "let myvar = 1\n";
        open_and_check(&state, &uri, src);

        let resp = service
            .inner()
            .prepare_rename(TextDocumentPositionParams {
                text_document: TextDocumentIdentifier { uri },
                position: Position {
                    line: 0,
                    character: 4,
                },
            })
            .await
            .unwrap();
        assert!(resp.is_some(), "should allow rename on variable");
    }

    #[tokio::test(flavor = "current_thread")]
    async fn prepare_rename_off_symbol_returns_none() {
        let (service, state) = setup();
        let uri: Url = "file:///test.zz".parse().unwrap();
        let src = "let x = 1\n";
        open_and_check(&state, &uri, src);

        let resp = service
            .inner()
            .prepare_rename(TextDocumentPositionParams {
                text_document: TextDocumentIdentifier { uri },
                position: Position {
                    line: 0,
                    character: 0, // on "let" keyword
                },
            })
            .await
            .unwrap();
        assert!(resp.is_none());
    }

    // ── Rename ───────────────────────────────────────────────────────

    #[tokio::test(flavor = "current_thread")]
    async fn rename_replaces_all_refs() {
        let (service, state) = setup();
        let uri: Url = "file:///test.zz".parse().unwrap();
        let src = "let x = 1\nlet y = x + x\n";
        open_and_check(&state, &uri, src);

        let edit = service
            .inner()
            .rename(RenameParams {
                text_document_position: TextDocumentPositionParams {
                    text_document: TextDocumentIdentifier { uri: uri.clone() },
                    position: Position {
                        line: 0,
                        character: 4,
                    },
                },
                new_name: "z".to_string(),
                work_done_progress_params: WorkDoneProgressParams::default(),
            })
            .await
            .unwrap();
        let edit = edit.expect("should produce rename edit");
        let changes = edit.changes.expect("should have changes");
        let edits = changes.get(&uri).expect("should have edits for uri");
        // Should replace `x` in let, and both `x` in `x + x`.
        assert!(edits.len() >= 3, "should have at least 3 replacements");
    }

    // ── Document highlight ───────────────────────────────────────────

    #[tokio::test(flavor = "current_thread")]
    async fn document_highlight_read_write() {
        let (service, state) = setup();
        let uri: Url = "file:///test.zz".parse().unwrap();
        let src = "let x = 1\nlet y = x\n";
        open_and_check(&state, &uri, src);

        let highlights = service
            .inner()
            .document_highlight(DocumentHighlightParams {
                text_document_position_params: TextDocumentPositionParams {
                    text_document: TextDocumentIdentifier { uri },
                    position: Position {
                        line: 0,
                        character: 4,
                    },
                },
                work_done_progress_params: WorkDoneProgressParams::default(),
                partial_result_params: PartialResultParams::default(),
            })
            .await
            .unwrap();
        let highlights = highlights.expect("should have highlights");
        // At least 2: the definition (Write) and usage (Read).
        assert!(highlights.len() >= 2);
        let kinds: Vec<_> = highlights.iter().filter_map(|h| h.kind).collect();
        assert!(kinds.contains(&DocumentHighlightKind::WRITE));
        assert!(kinds.contains(&DocumentHighlightKind::READ));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn document_highlight_missing_doc_returns_none() {
        let (service, _) = setup();
        let uri: Url = "file:///no.zz".parse().unwrap();
        let highlights = service
            .inner()
            .document_highlight(DocumentHighlightParams {
                text_document_position_params: TextDocumentPositionParams {
                    text_document: TextDocumentIdentifier { uri },
                    position: Position::default(),
                },
                work_done_progress_params: WorkDoneProgressParams::default(),
                partial_result_params: PartialResultParams::default(),
            })
            .await
            .unwrap();
        assert!(highlights.is_none());
    }

    // ── Completion ───────────────────────────────────────────────────

    #[tokio::test(flavor = "current_thread")]
    async fn completion_scope_mode() {
        let (service, state) = setup();
        let uri: Url = "file:///test.zz".parse().unwrap();
        let src = "func greet(n: str) -> str { return n }\nlet x = gr\n";
        open_and_check(&state, &uri, src);

        let resp = service
            .inner()
            .completion(CompletionParams {
                text_document_position: TextDocumentPositionParams {
                    text_document: TextDocumentIdentifier { uri },
                    position: Position {
                        line: 1,
                        character: 9, // after "gr"
                    },
                },
                work_done_progress_params: WorkDoneProgressParams::default(),
                partial_result_params: PartialResultParams::default(),
                context: None,
            })
            .await
            .unwrap();
        let resp = resp.expect("should have completions");
        match resp {
            CompletionResponse::Array(items) => {
                let labels: Vec<_> = items.iter().map(|i| i.label.clone()).collect();
                assert!(
                    labels.contains(&"greet".to_string()),
                    "should contain 'greet', got: {:?}",
                    labels
                );
            }
            CompletionResponse::List(_) => panic!("expected array"),
        }
    }

    #[tokio::test(flavor = "current_thread")]
    async fn completion_dot_access() {
        let (service, state) = setup();
        let uri: Url = "file:///test.zz".parse().unwrap();
        let src = "struct Point { x: int, y: int }\nlet p = Point{ x: 1, y: 2 }\nlet v = p.\n";
        open_and_check(&state, &uri, src);

        let resp = service
            .inner()
            .completion(CompletionParams {
                text_document_position: TextDocumentPositionParams {
                    text_document: TextDocumentIdentifier { uri },
                    position: Position {
                        line: 2,
                        character: 10, // after "p."
                    },
                },
                work_done_progress_params: WorkDoneProgressParams::default(),
                partial_result_params: PartialResultParams::default(),
                context: None,
            })
            .await
            .unwrap();
        let resp = resp.expect("should have completions");
        match resp {
            CompletionResponse::Array(items) => {
                let labels: Vec<_> = items.iter().map(|i| i.label.clone()).collect();
                assert!(
                    labels.contains(&"x".to_string()) && labels.contains(&"y".to_string()),
                    "should contain fields x, y, got: {:?}",
                    labels
                );
            }
            CompletionResponse::List(_) => panic!("expected array"),
        }
    }

    #[tokio::test(flavor = "current_thread")]
    async fn completion_missing_doc_returns_none() {
        let (service, _) = setup();
        let uri: Url = "file:///no.zz".parse().unwrap();
        let resp = service
            .inner()
            .completion(CompletionParams {
                text_document_position: TextDocumentPositionParams {
                    text_document: TextDocumentIdentifier { uri },
                    position: Position::default(),
                },
                work_done_progress_params: WorkDoneProgressParams::default(),
                partial_result_params: PartialResultParams::default(),
                context: None,
            })
            .await
            .unwrap();
        assert!(resp.is_none());
    }

    // ── Signature help ───────────────────────────────────────────────

    #[tokio::test(flavor = "current_thread")]
    async fn signature_help_inside_call() {
        let (service, state) = setup();
        let uri: Url = "file:///test.zz".parse().unwrap();
        let src = "func add(a: int, b: int) -> int { return a + b }\nlet x = add(";
        open_and_check(&state, &uri, src);

        let help = service
            .inner()
            .signature_help(SignatureHelpParams {
                text_document_position_params: TextDocumentPositionParams {
                    text_document: TextDocumentIdentifier { uri },
                    position: Position {
                        line: 1,
                        character: 12, // right after "add("
                    },
                },
                work_done_progress_params: WorkDoneProgressParams::default(),
                context: None,
            })
            .await
            .unwrap();
        let help = help.expect("should have signature help");
        assert_eq!(help.signatures.len(), 1);
        assert!(help.signatures[0].label.contains("add"));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn signature_help_missing_doc_returns_none() {
        let (service, _) = setup();
        let uri: Url = "file:///no.zz".parse().unwrap();
        let help = service
            .inner()
            .signature_help(SignatureHelpParams {
                text_document_position_params: TextDocumentPositionParams {
                    text_document: TextDocumentIdentifier { uri },
                    position: Position::default(),
                },
                work_done_progress_params: WorkDoneProgressParams::default(),
                context: None,
            })
            .await
            .unwrap();
        assert!(help.is_none());
    }

    // ── Code action ──────────────────────────────────────────────────

    #[tokio::test(flavor = "current_thread")]
    async fn code_action_missing_doc_returns_none() {
        let (service, _) = setup();
        let uri: Url = "file:///no.zz".parse().unwrap();
        let actions = service
            .inner()
            .code_action(CodeActionParams {
                text_document: TextDocumentIdentifier { uri },
                range: Range::default(),
                context: CodeActionContext::default(),
                work_done_progress_params: WorkDoneProgressParams::default(),
                partial_result_params: PartialResultParams::default(),
            })
            .await
            .unwrap();
        assert!(actions.is_none());
    }

    #[tokio::test(flavor = "current_thread")]
    async fn code_action_empty_range_returns_none() {
        let (service, state) = setup();
        let uri: Url = "file:///test.zz".parse().unwrap();
        let src = "let x = 1\n";
        open_and_check(&state, &uri, src);

        let actions = service
            .inner()
            .code_action(CodeActionParams {
                text_document: TextDocumentIdentifier { uri },
                range: Range::default(),
                context: CodeActionContext {
                    diagnostics: vec![],
                    only: None,
                    trigger_kind: None,
                },
                work_done_progress_params: WorkDoneProgressParams::default(),
                partial_result_params: PartialResultParams::default(),
            })
            .await
            .unwrap();
        assert!(actions.is_none());
    }

    // ── Did close clears diagnostics ─────────────────────────────────

    #[tokio::test(flavor = "current_thread")]
    async fn did_close_removes_document() {
        let (service, state) = setup();
        let uri: Url = "file:///test.zz".parse().unwrap();
        state.update_document(uri.clone(), 1, "let x = 1\n".into());
        assert!(state.documents.contains_key(&uri));

        service
            .inner()
            .did_close(DidCloseTextDocumentParams {
                text_document: TextDocumentIdentifier { uri: uri.clone() },
            })
            .await;

        assert!(!state.documents.contains_key(&uri));
    }

    // ── Completion resolve ───────────────────────────────────────────

    #[tokio::test(flavor = "current_thread")]
    async fn completion_resolve_returns_item() {
        let (service, state) = setup();
        let uri: Url = "file:///test.zz".parse().unwrap();
        let src = "func add(a: int, b: int) -> int { return a + b }\n";
        open_and_check(&state, &uri, src);

        let item = service
            .inner()
            .completion_resolve(CompletionItem {
                label: "add".to_string(),
                ..Default::default()
            })
            .await
            .unwrap();
        assert_eq!(item.label, "add");
    }

    // ═══════════════════════════════════════════════════════════════════
    // Cross-file tests
    // ═══════════════════════════════════════════════════════════════════

    /// Helper: populate the module index with a fake workspace so
    /// cross-file navigation works without a real filesystem.
    fn populate_module_index(
        state: &GlobalState,
        files: Vec<(Url, &str, &str)>,
    ) {
        let mut index = state.module_index.write().unwrap();
        for (uri, module_path, source) in files {
            let parsed = zz_frontend::parse(source);
            let definitions = crate::lookup::collect_definitions(&parsed.program, source);
            let defs: Vec<crate::lookup::Definition> = definitions.into_values().collect();
            let entry = crate::cross_file::FileEntry {
                uri: uri.clone(),
                module_path: module_path.to_string(),
                source: source.to_string(),
                program: Some(parsed.program),
                definitions: defs,
            };
            index.module_to_uri.insert(module_path.to_string(), uri.clone());
            index.uri_to_module.insert(uri.clone(), module_path.to_string());
            index.entries.insert(uri, entry);
        }
    }

    // ── Go-to-definition across files ─────────────────────────────────

    #[tokio::test(flavor = "current_thread")]
    async fn goto_definition_cross_file_import() {
        let (service, state) = setup();

        // Set up a workspace with two "files" in the module index.
        let lib_uri: Url = "file:///workspace/utils.zz".parse().unwrap();
        let main_uri: Url = "file:///workspace/main.zz".parse().unwrap();

        let lib_src = "func greet(name: str) -> str { return name }\n";
        let main_src = "import utils\ngreet(\"hi\")\n";

        populate_module_index(
            &state,
            vec![
                (lib_uri.clone(), "utils", lib_src),
                (main_uri.clone(), "main", main_src),
            ],
        );

        // Also open main so it has a program.
        open_and_check(&state, &main_uri, main_src);

        // Set workspace root (resolve_import checks module_to_uri first).
        state.set_root(std::path::PathBuf::from("/workspace"));

        // Cursor on the import path "utils" (line 0, char 7).
        let def = service
            .inner()
            .goto_definition(GotoDefinitionParams {
                text_document_position_params: TextDocumentPositionParams {
                    text_document: TextDocumentIdentifier { uri: main_uri },
                    position: Position {
                        line: 0,
                        character: 7,
                    },
                },
                work_done_progress_params: WorkDoneProgressParams::default(),
                partial_result_params: PartialResultParams::default(),
            })
            .await
            .unwrap();

        let def = def.expect("should resolve import to cross-file definition");
        match def {
            GotoDefinitionResponse::Scalar(loc) => {
                assert_eq!(loc.uri, lib_uri);
                // Should point to the `greet` function definition.
                assert!(loc.range.start.line == 0);
            }
            _ => panic!("expected scalar location"),
        }
    }

    // ── References across files ───────────────────────────────────────

    #[tokio::test(flavor = "current_thread")]
    async fn references_cross_file() {
        let (service, state) = setup();

        let uri_a: Url = "file:///workspace/a.zz".parse().unwrap();
        let uri_b: Url = "file:///workspace/b.zz".parse().unwrap();

        let src_a = "let shared_val = 42\n";
        let src_b = "let x = shared_val\n";

        populate_module_index(
            &state,
            vec![
                (uri_a.clone(), "a", src_a),
                (uri_b.clone(), "b", src_b),
            ],
        );
        open_and_check(&state, &uri_a, src_a);
        open_and_check(&state, &uri_b, src_b);

        // Cursor on `shared_val` definition in file a (line 0, char 4).
        let refs = service
            .inner()
            .references(ReferenceParams {
                text_document_position: TextDocumentPositionParams {
                    text_document: TextDocumentIdentifier { uri: uri_a.clone() },
                    position: Position {
                        line: 0,
                        character: 4,
                    },
                },
                context: ReferenceContext {
                    include_declaration: true,
                },
                work_done_progress_params: WorkDoneProgressParams::default(),
                partial_result_params: PartialResultParams::default(),
            })
            .await
            .unwrap();

        let refs = refs.expect("should find cross-file references");
        // Should find: definition in a.zz + usage in b.zz = at least 2.
        assert!(
            refs.len() >= 2,
            "expected at least 2 cross-file references, got {}",
            refs.len()
        );

        // At least one reference should be in file b.
        let has_b = refs.iter().any(|r| r.uri == uri_b);
        assert!(has_b, "should have a reference in file b.zz");
    }

    // ── Rename across files ───────────────────────────────────────────

    #[tokio::test(flavor = "current_thread")]
    async fn rename_cross_file() {
        let (service, state) = setup();

        let uri_a: Url = "file:///workspace/a.zz".parse().unwrap();
        let uri_b: Url = "file:///workspace/b.zz".parse().unwrap();

        let src_a = "let myvar = 1\n";
        let src_b = "let y = myvar\n";

        populate_module_index(
            &state,
            vec![
                (uri_a.clone(), "a", src_a),
                (uri_b.clone(), "b", src_b),
            ],
        );
        open_and_check(&state, &uri_a, src_a);
        open_and_check(&state, &uri_b, src_b);

        // Rename `myvar` → `renamed` from file a.
        let edit = service
            .inner()
            .rename(RenameParams {
                text_document_position: TextDocumentPositionParams {
                    text_document: TextDocumentIdentifier { uri: uri_a.clone() },
                    position: Position {
                        line: 0,
                        character: 4,
                    },
                },
                new_name: "renamed".to_string(),
                work_done_progress_params: WorkDoneProgressParams::default(),
            })
            .await
            .unwrap();

        let edit = edit.expect("should produce cross-file rename edit");
        let changes = edit.changes.expect("should have changes");

        // Should have edits in both files.
        assert!(
            changes.contains_key(&uri_a),
            "should have edits in file a"
        );
        assert!(
            changes.contains_key(&uri_b),
            "should have edits in file b"
        );

        // File a: rename declaration.
        let edits_a = changes.get(&uri_a).unwrap();
        assert_eq!(edits_a.len(), 1);
        assert_eq!(edits_a[0].new_text, "renamed");

        // File b: rename usage.
        let edits_b = changes.get(&uri_b).unwrap();
        assert_eq!(edits_b.len(), 1);
        assert_eq!(edits_b[0].new_text, "renamed");
    }

    // ── Workspace symbol includes indexed files ───────────────────────

    #[tokio::test(flavor = "current_thread")]
    async fn workspace_symbol_includes_indexed_files() {
        let (service, state) = setup();

        let uri_open: Url = "file:///workspace/open.zz".parse().unwrap();
        let uri_indexed: Url = "file:///workspace/indexed.zz".parse().unwrap();

        let src_open = "func open_func() -> int { return 1 }\n";
        let src_indexed = "func indexed_func() -> int { return 2 }\nstruct IndexedStruct { val: int }\n";

        // Open one file normally.
        open_and_check(&state, &uri_open, src_open);

        // Put the other in the module index only.
        populate_module_index(
            &state,
            vec![
                (uri_indexed.clone(), "indexed", src_indexed),
            ],
        );

        // Search for "indexed" — should find symbols from the indexed file.
        let syms = service
            .inner()
            .symbol(WorkspaceSymbolParams {
                query: "indexed".to_string(),
                work_done_progress_params: WorkDoneProgressParams::default(),
                partial_result_params: PartialResultParams::default(),
            })
            .await
            .unwrap();

        let syms = syms.expect("should have symbols");
        let names: Vec<_> = syms.iter().map(|s| s.name.clone()).collect();
        assert!(
            names.iter().any(|n| n.contains("indexed_func")),
            "should find indexed_func from non-open file, got: {:?}",
            names
        );
    }

    // ── Scan workspace on initialize ──────────────────────────────────

    #[tokio::test(flavor = "current_thread")]
    async fn initialize_scans_workspace() {
        // Create a temp workspace with a .zz file.
        let root = std::path::PathBuf::from("/tmp/test_ws_init");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(
            root.join("helper.zz"),
            "func helper() -> int { return 42 }\n",
        )
        .unwrap();

        let (service, state) = setup();
        let params = InitializeParams {
            workspace_folders: Some(vec![WorkspaceFolder {
                uri: Url::from_file_path(&root).unwrap(),
                name: "test_ws_init".to_string(),
            }]),
            ..Default::default()
        };
        service.inner().initialize(params).await.unwrap();

        // The module index should have scanned helper.zz.
        {
            let index = state.module_index.read().unwrap();
            assert!(
                index.module_to_uri.contains_key("helper"),
                "should have indexed helper module"
            );
        }

        // Cleanup.
        std::fs::remove_dir_all(&root).unwrap();
    }

    // ── didChangeWorkspaceFolders triggers rescan ─────────────────────

    #[tokio::test(flavor = "current_thread")]
    async fn workspace_folder_change_triggers_rescan() {
        let root = std::path::PathBuf::from("/tmp/test_ws_folders");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("a.zz"), "let x = 1\n").unwrap();

        let (service, state) = setup();
        let params = InitializeParams {
            workspace_folders: Some(vec![WorkspaceFolder {
                uri: Url::from_file_path(&root).unwrap(),
                name: "test_ws_folders".to_string(),
            }]),
            ..Default::default()
        };
        service.inner().initialize(params).await.unwrap();

        // Verify a.zz was indexed.
        {
            let index = state.module_index.read().unwrap();
            assert!(index.module_to_uri.contains_key("a"));
        }

        // Add another file and send workspace folder change.
        std::fs::write(root.join("b.zz"), "let y = 2\n").unwrap();
        service
            .inner()
            .did_change_workspace_folders(DidChangeWorkspaceFoldersParams {
                event: WorkspaceFoldersChangeEvent {
                    added: vec![WorkspaceFolder {
                        uri: Url::from_file_path(&root).unwrap(),
                        name: "test_ws_folders".to_string(),
                    }],
                    removed: vec![],
                },
            })
            .await;

        // After rescan, b.zz should be indexed too.
        {
            let index = state.module_index.read().unwrap();
            assert!(
                index.module_to_uri.contains_key("b"),
                "should have indexed b.zz after folder change"
            );
        }

        // Cleanup.
        std::fs::remove_dir_all(&root).unwrap();
    }

    // ── Module index: find_definition_across_files ────────────────────

    #[tokio::test(flavor = "current_thread")]
    async fn module_index_find_definition() {
        let (service, state) = setup();

        let uri: Url = "file:///workspace/mod.zz".parse().unwrap();
        let src = "func my_func() -> int { return 1 }\nstruct MyStruct { val: int }\n";

        populate_module_index(&state, vec![(uri, "mod", src)]);

        let index = state.module_index.read().unwrap();
        let result = index.find_definition_across_files("my_func");
        assert!(result.is_some(), "should find my_func across files");

        let result = index.find_definition_across_files("MyStruct");
        assert!(result.is_some(), "should find MyStruct across files");

        let result = index.find_definition_across_files("nonexistent");
        assert!(result.is_none());
    }

    // ── Module index: find_references_across_files ────────────────────

    #[tokio::test(flavor = "current_thread")]
    async fn module_index_find_references() {
        let (service, state) = setup();

        let uri_a: Url = "file:///workspace/a.zz".parse().unwrap();
        let uri_b: Url = "file:///workspace/b.zz".parse().unwrap();

        populate_module_index(
            &state,
            vec![
                (uri_a.clone(), "a", "let x = 1\nlet y = 2\n"),
                (uri_b.clone(), "b", "let x = 3\n"),
            ],
        );

        let index = state.module_index.read().unwrap();
        let refs = index.find_references_across_files("x");
        assert_eq!(refs.len(), 2, "x is defined in both files");

        let refs = index.find_references_across_files("y");
        assert_eq!(refs.len(), 1, "y is only in file a");

        let refs = index.find_references_across_files("z");
        assert!(refs.is_empty(), "z doesn't exist");
    }

    // ── Module path resolution ────────────────────────────────────────

    #[tokio::test(flavor = "current_thread")]
    async fn module_path_for_file_unit() {
        use crate::cross_file::module_path_for_file;
        let root = std::path::PathBuf::from("/workspace");
        assert_eq!(
            module_path_for_file(&std::path::PathBuf::from("/workspace/main.zz"), &root),
            Some("main".to_string())
        );
        assert_eq!(
            module_path_for_file(&std::path::PathBuf::from("/workspace/utils/math.zz"), &root),
            Some("utils.math".to_string())
        );
        assert_eq!(
            module_path_for_file(&std::path::PathBuf::from("/workspace/utils/mod.zz"), &root),
            Some("utils".to_string())
        );
        assert_eq!(
            module_path_for_file(&std::path::PathBuf::from("/workspace/readme.md"), &root),
            None,
        );
    }
}
