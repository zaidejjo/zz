use tower_lsp::jsonrpc::Result;
use tower_lsp::lsp_types::*;

use super::Backend;

pub(crate) async fn handle_initialize(
    backend: &Backend,
    params: InitializeParams,
) -> Result<InitializeResult> {
    log::info!("zz-lsp initializing");
    if let Some(folders) = &params.workspace_folders {
        if let Some(folder) = folders.first() {
            if let Ok(path) = folder.uri.to_file_path() {
                log::info!("workspace root: {}", path.display());
                backend.state.set_root(path);
            }
        }
    } else if let Some(uri) = &params.root_uri {
        if let Ok(path) = uri.to_file_path() {
            log::info!("workspace root: {}", path.display());
            backend.state.set_root(path);
        }
    }

    backend
        .client
        .log_message(MessageType::INFO, "zz-lsp initialized")
        .await;

    backend.state.scan_workspace_async();

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
            document_formatting_provider: Some(OneOf::Left(true)),
            folding_range_provider: Some(FoldingRangeProviderCapability::Simple(true)),
            semantic_tokens_provider: Some(
                SemanticTokensServerCapabilities::SemanticTokensOptions(SemanticTokensOptions {
                    legend: SemanticTokensLegend {
                        token_types: crate::semantic_tokens::token_type_legend(),
                        token_modifiers: vec![],
                    },
                    range: Some(true),
                    full: Some(SemanticTokensFullOptions::Bool(true)),
                    ..Default::default()
                }),
            ),
            ..Default::default()
        },
        server_info: Some(ServerInfo {
            name: "zz-lsp".to_string(),
            version: Some(env!("CARGO_PKG_VERSION").to_string()),
        }),
    })
}

pub(crate) async fn handle_initialized(backend: &Backend, _: InitializedParams) {
    backend
        .client
        .log_message(MessageType::INFO, "zz-lsp ready")
        .await;
}

pub(crate) async fn handle_did_change_workspace_folders(
    backend: &Backend,
    params: DidChangeWorkspaceFoldersParams,
) {
    let _ = params;
    backend
        .state
        .workspace_scanned
        .store(false, std::sync::atomic::Ordering::SeqCst);
    backend.state.scan_workspace_async();
    backend
        .client
        .log_message(
            MessageType::INFO,
            "workspace folders changed, rescan complete",
        )
        .await;
}
