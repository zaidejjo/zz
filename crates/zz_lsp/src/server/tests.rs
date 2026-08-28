use crate::state::GlobalState;

use std::sync::Arc;
use tower_lsp::lsp_types::*;
use tower_lsp::LanguageServer;
use tower_lsp::LspService;

use super::Backend;

    fn setup() -> (tower_lsp::LspService<Backend>, Arc<GlobalState>) {
        let (service, _) = LspService::new(|client| Backend::new(client));
        let state = Arc::clone(&service.inner().state);
        (service, state)
    }

    fn open_and_check(state: &GlobalState, uri: &Url, source: &str) {
        state.update_document(uri.clone(), 1, source.to_string());
        let (ib, ifunc, is) = state.checker_seed();
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

    #[tokio::test(flavor = "current_thread")]
    async fn shutdown_succeeds() {
        let (service, _) = setup();
        let result = service.inner().shutdown().await;
        assert!(result.is_ok());
    }

    #[tokio::test(flavor = "current_thread")]
    async fn hover_function() {
        let (service, state) = setup();
        let uri: Url = "file:///test.zz".parse().unwrap();
        let src = "func add(a: int, b: int) -> int { return a + b }\n";
        open_and_check(&state, &uri, src);

        let pos = Position {
            line: 0,
            character: 5,
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
            character: 7,
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
        let src = "x := 42\n";
        open_and_check(&state, &uri, src);

        let pos = Position {
            line: 0,
            character: 0,
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

    #[tokio::test(flavor = "current_thread")]
    async fn goto_definition_function() {
        let (service, state) = setup();
        let uri: Url = "file:///test.zz".parse().unwrap();
        let src = "func add(a: int, b: int) -> int { return a + b }\nx := add(1, 2)\n";
        open_and_check(&state, &uri, src);

        let pos = Position {
            line: 1,
            character: 5,
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
        let src = "struct Point { x: int, y: int }\np := Point{ x: 1, y: 2 }\nv := p.x\n";
        open_and_check(&state, &uri, src);

        let pos = Position {
            line: 2,
            character: 7,
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

    #[tokio::test(flavor = "current_thread")]
    async fn document_symbol_lists_functions_and_structs() {
        let (service, state) = setup();
        let uri: Url = "file:///test.zz".parse().unwrap();
        let src = "func add(a: int, b: int) -> int { return a + b }\nstruct Point { x: int, y: int }\nc := 1\n";
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

    #[tokio::test(flavor = "current_thread")]
    async fn references_finds_usages() {
        let (service, state) = setup();
        let uri: Url = "file:///test.zz".parse().unwrap();
        let src = "func id(x: int) -> int { return x }\na := id(1)\nb := id(2)\n";
        open_and_check(&state, &uri, src);

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

    #[tokio::test(flavor = "current_thread")]
    async fn prepare_rename_on_symbol() {
        let (service, state) = setup();
        let uri: Url = "file:///test.zz".parse().unwrap();
        let src = "myvar := 1\n";
        open_and_check(&state, &uri, src);

        let resp = service
            .inner()
            .prepare_rename(TextDocumentPositionParams {
                text_document: TextDocumentIdentifier { uri },
                position: Position {
                    line: 0,
                    character: 0,
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
        let src = "x := 1\n";
        open_and_check(&state, &uri, src);

        let resp = service
            .inner()
            .prepare_rename(TextDocumentPositionParams {
                text_document: TextDocumentIdentifier { uri },
                position: Position {
                    line: 0,
                    character: 1,
                },
            })
            .await
            .unwrap();
        assert!(resp.is_none());
    }

    #[tokio::test(flavor = "current_thread")]
    async fn rename_replaces_all_refs() {
        let (service, state) = setup();
        let uri: Url = "file:///test.zz".parse().unwrap();
        let src = "x := 1\ny := x + x\n";
        open_and_check(&state, &uri, src);

        let edit = service
            .inner()
            .rename(RenameParams {
                text_document_position: TextDocumentPositionParams {
                    text_document: TextDocumentIdentifier { uri: uri.clone() },
                    position: Position {
                        line: 0,
                        character: 0,
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
        assert!(edits.len() >= 3, "should have at least 3 replacements");
    }

    #[tokio::test(flavor = "current_thread")]
    async fn document_highlight_read_write() {
        let (service, state) = setup();
        let uri: Url = "file:///test.zz".parse().unwrap();
        let src = "x := 1\ny := x\n";
        open_and_check(&state, &uri, src);

        let highlights = service
            .inner()
            .document_highlight(DocumentHighlightParams {
                text_document_position_params: TextDocumentPositionParams {
                    text_document: TextDocumentIdentifier { uri },
                    position: Position {
                        line: 0,
                        character: 0,
                    },
                },
                work_done_progress_params: WorkDoneProgressParams::default(),
                partial_result_params: PartialResultParams::default(),
            })
            .await
            .unwrap();
        let highlights = highlights.expect("should have highlights");
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

    #[tokio::test(flavor = "current_thread")]
    async fn completion_scope_mode() {
        let (service, state) = setup();
        let uri: Url = "file:///test.zz".parse().unwrap();
        let src = "func greet(n: str) -> str { return n }\nx := gr\n";
        open_and_check(&state, &uri, src);

        let resp = service
            .inner()
            .completion(CompletionParams {
                text_document_position: TextDocumentPositionParams {
                    text_document: TextDocumentIdentifier { uri },
                    position: Position {
                        line: 1,
                        character: 9,
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
        let src = "struct Point { x: int, y: int }\np := Point{ x: 1, y: 2 }\nv := p.\n";
        open_and_check(&state, &uri, src);

        let resp = service
            .inner()
            .completion(CompletionParams {
                text_document_position: TextDocumentPositionParams {
                    text_document: TextDocumentIdentifier { uri },
                    position: Position {
                        line: 2,
                        character: 7,
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

    #[tokio::test(flavor = "current_thread")]
    async fn signature_help_inside_call() {
        let (service, state) = setup();
        let uri: Url = "file:///test.zz".parse().unwrap();
        let src = "func add(a: int, b: int) -> int { return a + b }\nx := add(";
        open_and_check(&state, &uri, src);

        let help = service
            .inner()
            .signature_help(SignatureHelpParams {
                text_document_position_params: TextDocumentPositionParams {
                    text_document: TextDocumentIdentifier { uri },
                    position: Position {
                        line: 1,
                        character: 12,
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
        let src = "x := 1\n";
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

    #[tokio::test(flavor = "current_thread")]
    async fn did_close_removes_document() {
        let (service, state) = setup();
        let uri: Url = "file:///test.zz".parse().unwrap();
        state.update_document(uri.clone(), 1, "x := 1\n".into());
        assert!(state.documents.contains_key(&uri));

        service
            .inner()
            .did_close(DidCloseTextDocumentParams {
                text_document: TextDocumentIdentifier { uri: uri.clone() },
            })
            .await;

        assert!(!state.documents.contains_key(&uri));
    }

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
            index
                .module_to_uri
                .insert(module_path.to_string(), uri.clone());
            index.uri_to_module.insert(uri.clone(), module_path.to_string());
            index.entries.insert(uri, entry);
        }
    }

    #[tokio::test(flavor = "current_thread")]
    async fn goto_definition_cross_file_import() {
        let (service, state) = setup();

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

        open_and_check(&state, &main_uri, main_src);
        state.set_root(std::path::PathBuf::from("/workspace"));

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
                assert!(loc.range.start.line == 0);
            }
            _ => panic!("expected scalar location"),
        }
    }

    #[tokio::test(flavor = "current_thread")]
    async fn references_cross_file() {
        let (service, state) = setup();

        let uri_a: Url = "file:///workspace/a.zz".parse().unwrap();
        let uri_b: Url = "file:///workspace/b.zz".parse().unwrap();

        let src_a = "shared_val := 42\n";
        let src_b = "x := shared_val\n";

        populate_module_index(
            &state,
            vec![
                (uri_a.clone(), "a", src_a),
                (uri_b.clone(), "b", src_b),
            ],
        );
        open_and_check(&state, &uri_a, src_a);
        open_and_check(&state, &uri_b, src_b);

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
        assert!(
            refs.len() >= 2,
            "expected at least 2 cross-file references, got {}",
            refs.len()
        );

        let has_b = refs.iter().any(|r| r.uri == uri_b);
        assert!(has_b, "should have a reference in file b.zz");
    }

    #[tokio::test(flavor = "current_thread")]
    async fn rename_cross_file() {
        let (service, state) = setup();

        let uri_a: Url = "file:///workspace/a.zz".parse().unwrap();
        let uri_b: Url = "file:///workspace/b.zz".parse().unwrap();

        let src_a = "myvar := 1\n";
        let src_b = "y := myvar\n";

        populate_module_index(
            &state,
            vec![
                (uri_a.clone(), "a", src_a),
                (uri_b.clone(), "b", src_b),
            ],
        );
        open_and_check(&state, &uri_a, src_a);
        open_and_check(&state, &uri_b, src_b);

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

        assert!(
            changes.contains_key(&uri_a),
            "should have edits in file a"
        );
        assert!(
            changes.contains_key(&uri_b),
            "should have edits in file b"
        );

        let edits_a = changes.get(&uri_a).unwrap();
        assert_eq!(edits_a.len(), 1);
        assert_eq!(edits_a[0].new_text, "renamed");

        let edits_b = changes.get(&uri_b).unwrap();
        assert_eq!(edits_b.len(), 1);
        assert_eq!(edits_b[0].new_text, "renamed");
    }

    #[tokio::test(flavor = "current_thread")]
    async fn workspace_symbol_includes_indexed_files() {
        let (service, state) = setup();

        let uri_open: Url = "file:///workspace/open.zz".parse().unwrap();
        let uri_indexed: Url = "file:///workspace/indexed.zz".parse().unwrap();

        let src_open = "func open_func() -> int { return 1 }\n";
        let src_indexed =
            "func indexed_func() -> int { return 2 }\nstruct IndexedStruct { val: int }\n";

        open_and_check(&state, &uri_open, src_open);

        populate_module_index(
            &state,
            vec![(uri_indexed.clone(), "indexed", src_indexed)],
        );

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

    #[tokio::test(flavor = "current_thread")]
    async fn initialize_scans_workspace() {
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

        {
            let index = state.module_index.read().unwrap();
            assert!(
                index.module_to_uri.contains_key("helper"),
                "should have indexed helper module"
            );
        }

        std::fs::remove_dir_all(&root).unwrap();
    }

    #[tokio::test(flavor = "current_thread")]
    async fn workspace_folder_change_triggers_rescan() {
        let root = std::path::PathBuf::from("/tmp/test_ws_folders");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("a.zz"), "x := 1\n").unwrap();

        let (service, state) = setup();
        let params = InitializeParams {
            workspace_folders: Some(vec![WorkspaceFolder {
                uri: Url::from_file_path(&root).unwrap(),
                name: "test_ws_folders".to_string(),
            }]),
            ..Default::default()
        };
        service.inner().initialize(params).await.unwrap();

        {
            let index = state.module_index.read().unwrap();
            assert!(index.module_to_uri.contains_key("a"));
        }

        std::fs::write(root.join("b.zz"), "y := 2\n").unwrap();
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

        {
            let index = state.module_index.read().unwrap();
            assert!(
                index.module_to_uri.contains_key("b"),
                "should have indexed b.zz after folder change"
            );
        }

        std::fs::remove_dir_all(&root).unwrap();
    }

    #[tokio::test(flavor = "current_thread")]
    async fn module_index_find_definition() {
        let (_service, state) = setup();

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

    #[tokio::test(flavor = "current_thread")]
    async fn module_index_find_references() {
        let (_service, state) = setup();

        let uri_a: Url = "file:///workspace/a.zz".parse().unwrap();
        let uri_b: Url = "file:///workspace/b.zz".parse().unwrap();

        populate_module_index(
            &state,
            vec![
                (uri_a.clone(), "a", "x := 1\ny := 2\n"),
                (uri_b.clone(), "b", "x := 3\n"),
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

    #[tokio::test(flavor = "current_thread")]
    async fn module_path_for_file_unit() {
        use crate::cross_file::module_path_for_file;
        let root = std::path::PathBuf::from("/workspace");
        assert_eq!(
            module_path_for_file(
                &std::path::PathBuf::from("/workspace/main.zz"),
                &root
            ),
            Some("main".to_string())
        );
        assert_eq!(
            module_path_for_file(
                &std::path::PathBuf::from("/workspace/utils/math.zz"),
                &root
            ),
            Some("utils.math".to_string())
        );
        assert_eq!(
            module_path_for_file(
                &std::path::PathBuf::from("/workspace/utils/mod.zz"),
                &root
            ),
            Some("utils".to_string())
        );
        assert_eq!(
            module_path_for_file(
                &std::path::PathBuf::from("/workspace/readme.md"),
                &root
            ),
            None,
        );
    }
