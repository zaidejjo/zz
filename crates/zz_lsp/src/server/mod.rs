mod code_action;
mod display;
mod document_sync;
mod intellisense;
mod lifecycle;
mod navigation;
mod rename;
mod symbols;
#[cfg(test)]
mod tests;

use std::sync::Arc;

use tower_lsp::jsonrpc::Result;
use tower_lsp::lsp_types::*;
use tower_lsp::{Client, LanguageServer};

use crate::state::GlobalState;

pub struct Backend {
    pub(crate) client: Client,
    pub(crate) state: Arc<GlobalState>,
}

impl Backend {
    pub fn new(client: Client) -> Self {
        Self {
            client,
            state: Arc::new(GlobalState::new()),
        }
    }
}

pub(crate) fn find_struct_field_location(
    check_result: &zz_checker::CheckResult,
    struct_name: &str,
    field_name: &str,
    program: &zz_frontend::ast::Program,
    uri: &Url,
    source: &str,
) -> Option<GotoDefinitionResponse> {
    if let Some(ssig) = check_result.structs.get(struct_name) {
        if let Some((fname, _)) = ssig.fields.iter().find(|(n, _)| n == field_name) {
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
                                return Some(GotoDefinitionResponse::Scalar(Location {
                                    uri: uri.clone(),
                                    range: crate::convert::span_to_range(source, fi_name.span),
                                }));
                            }
                        }
                    }
                }
            }
        }
    }
    None
}

#[tower_lsp::async_trait]
impl LanguageServer for Backend {
    async fn initialize(&self, params: InitializeParams) -> Result<InitializeResult> {
        lifecycle::handle_initialize(self, params).await
    }

    async fn initialized(&self, params: InitializedParams) {
        lifecycle::handle_initialized(self, params).await
    }

    async fn did_change_workspace_folders(&self, params: DidChangeWorkspaceFoldersParams) {
        lifecycle::handle_did_change_workspace_folders(self, params).await
    }

    async fn did_open(&self, params: DidOpenTextDocumentParams) {
        document_sync::handle_did_open(self, params).await
    }

    async fn did_change(&self, params: DidChangeTextDocumentParams) {
        document_sync::handle_did_change(self, params).await
    }

    async fn did_save(&self, params: DidSaveTextDocumentParams) {
        document_sync::handle_did_save(self, params).await
    }

    async fn did_close(&self, params: DidCloseTextDocumentParams) {
        document_sync::handle_did_close(self, params).await
    }

    async fn hover(&self, params: HoverParams) -> Result<Option<Hover>> {
        intellisense::handle_hover(self, params).await
    }

    async fn goto_definition(
        &self,
        params: GotoDefinitionParams,
    ) -> Result<Option<GotoDefinitionResponse>> {
        navigation::handle_goto_definition(self, params).await
    }

    async fn code_action(&self, params: CodeActionParams) -> Result<Option<CodeActionResponse>> {
        code_action::handle_code_action(self, params).await
    }

    async fn document_symbol(
        &self,
        params: DocumentSymbolParams,
    ) -> Result<Option<DocumentSymbolResponse>> {
        symbols::handle_document_symbol(self, params).await
    }

    async fn symbol(
        &self,
        params: WorkspaceSymbolParams,
    ) -> Result<Option<Vec<SymbolInformation>>> {
        symbols::handle_symbol(self, params).await
    }

    async fn references(&self, params: ReferenceParams) -> Result<Option<Vec<Location>>> {
        navigation::handle_references(self, params).await
    }

    async fn prepare_rename(
        &self,
        params: TextDocumentPositionParams,
    ) -> Result<Option<PrepareRenameResponse>> {
        rename::handle_prepare_rename(self, params).await
    }

    async fn rename(&self, params: RenameParams) -> Result<Option<WorkspaceEdit>> {
        rename::handle_rename(self, params).await
    }

    async fn document_highlight(
        &self,
        params: DocumentHighlightParams,
    ) -> Result<Option<Vec<DocumentHighlight>>> {
        navigation::handle_document_highlight(self, params).await
    }

    async fn completion(&self, params: CompletionParams) -> Result<Option<CompletionResponse>> {
        intellisense::handle_completion(self, params).await
    }

    async fn completion_resolve(&self, item: CompletionItem) -> Result<CompletionItem> {
        intellisense::handle_completion_resolve(self, item).await
    }

    async fn signature_help(&self, params: SignatureHelpParams) -> Result<Option<SignatureHelp>> {
        intellisense::handle_signature_help(self, params).await
    }

    async fn formatting(&self, params: DocumentFormattingParams) -> Result<Option<Vec<TextEdit>>> {
        display::handle_formatting(self, params).await
    }

    async fn inlay_hint(&self, params: InlayHintParams) -> Result<Option<Vec<InlayHint>>> {
        display::handle_inlay_hint(self, params).await
    }

    async fn semantic_tokens_full(
        &self,
        params: SemanticTokensParams,
    ) -> Result<Option<SemanticTokensResult>> {
        display::handle_semantic_tokens_full(self, params).await
    }

    async fn folding_range(&self, params: FoldingRangeParams) -> Result<Option<Vec<FoldingRange>>> {
        display::handle_folding_range(self, params).await
    }

    async fn shutdown(&self) -> Result<()> {
        log::info!("zz-lsp shutting down");
        Ok(())
    }
}
