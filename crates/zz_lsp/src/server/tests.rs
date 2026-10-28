use crate::state::GlobalState;

use std::sync::Arc;
use tower_lsp::lsp_types::*;
use tower_lsp::LanguageServer;
use tower_lsp::LspService;

use super::Backend;

fn setup() -> (tower_lsp::LspService<Backend>, Arc<GlobalState>) {
    let (service, _) = LspService::new(Backend::new);
    let state = Arc::clone(&service.inner().state);
    (service, state)
}

fn open_and_check(state: &GlobalState, uri: &Url, source: &str) {
    state.update_document(uri.clone(), 1, source.to_string());
    // Clone the program first: holding the read guard across `get_mut`
    // below deadlocks the backing DashMap shard.
    let program = state.documents.get(uri).unwrap().program.clone().unwrap();
    let doc_path = uri.to_file_path().ok();
    let (ib, ifunc, is, ia, ie) = state.checker_seed_for_path(&program, doc_path.as_deref());
    if let Some(mut doc) = state.documents.get_mut(uri) {
        doc.seed_bindings = Some(ib.clone());
    }
    let cr = zz_checker::check_program(&program, ib, ifunc, is, ia, ie);
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
    assert!(caps.references_provider.is_some());
    assert!(caps.document_highlight_provider.is_some());
    assert!(caps.inlay_hint_provider.is_some());
    assert!(caps.completion_provider.is_some());
    assert!(caps.signature_help_provider.is_some());
}

#[tokio::test(flavor = "current_thread")]
async fn initialize_sets_root_from_workspace_folders() {
    let (service, state) = setup();
    let ws_root = std::env::temp_dir().join(format!("zz_lsp_root_ws_{}", std::process::id()));
    let params = InitializeParams {
        workspace_folders: Some(vec![WorkspaceFolder {
            uri: Url::from_file_path(&ws_root).unwrap(),
            name: "myproject".to_string(),
        }]),
        ..Default::default()
    };
    service.inner().initialize(params).await.unwrap();
    let root = state.root.read().unwrap();
    assert_eq!(*root, Some(ws_root));
}

#[tokio::test(flavor = "current_thread")]
async fn initialize_sets_root_from_root_uri() {
    let (service, state) = setup();
    let alt_root = std::env::temp_dir().join(format!("zz_lsp_root_alt_{}", std::process::id()));
    let params = InitializeParams {
        root_uri: Some(Url::from_file_path(&alt_root).unwrap()),
        ..Default::default()
    };
    service.inner().initialize(params).await.unwrap();
    let root = state.root.read().unwrap();
    assert_eq!(*root, Some(alt_root));
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
    assert!(
        def.is_some(),
        "should resolve struct field go-to-definition"
    );
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

fn watched_tmp(tag: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("zz-lsp-watch-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

#[tokio::test(flavor = "current_thread")]
async fn watched_change_reindexes_and_rechecks_dependents() {
    let (service, state) = setup();
    let dir = watched_tmp("reindex");
    let dep_path = dir.join("dep.zz");
    std::fs::write(&dep_path, "pub func answer() -> int {\n    return 1\n}\n").unwrap();
    let dep_uri = Url::from_file_path(&dep_path).unwrap();
    state.set_root(dir.clone());

    let main_src = "import dep(answer)\nfunc main() {\n    println(answer())\n}\n";
    let main_uri: Url = "file:///main.zz".parse().unwrap();
    open_and_check(&state, &main_uri, main_src);
    state.record_dependency(dep_uri.clone(), main_uri.clone());
    // Wipe main's result: the watched change must restore it.
    state.documents.get_mut(&main_uri).unwrap().check_result = None;

    std::fs::write(&dep_path, "pub func answer() -> int {\n    return 2\n}\n").unwrap();
    service
        .inner()
        .did_change_watched_files(DidChangeWatchedFilesParams {
            changes: vec![FileEvent {
                uri: dep_uri.clone(),
                typ: FileChangeType::CHANGED,
            }],
        })
        .await;

    assert!(
        state
            .module_index
            .read()
            .unwrap()
            .entries
            .contains_key(&dep_uri),
        "changed file re-indexed"
    );
    assert!(
        state
            .documents
            .get(&main_uri)
            .unwrap()
            .check_result
            .is_some(),
        "open dependent rechecked"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test(flavor = "current_thread")]
async fn watched_delete_forgets_file() {
    let (service, state) = setup();
    let dir = watched_tmp("delete");
    let dep_path = dir.join("dep.zz");
    std::fs::write(&dep_path, "pub func answer() -> int {\n    return 1\n}\n").unwrap();
    let dep_uri = Url::from_file_path(&dep_path).unwrap();
    state.set_root(dir.clone());
    index_test_file(
        &state,
        &dep_uri,
        "dep",
        "pub func answer() -> int {\n    return 1\n}\n",
    );
    std::fs::remove_file(&dep_path).unwrap();

    service
        .inner()
        .did_change_watched_files(DidChangeWatchedFilesParams {
            changes: vec![FileEvent {
                uri: dep_uri.clone(),
                typ: FileChangeType::DELETED,
            }],
        })
        .await;

    assert!(
        !state
            .module_index
            .read()
            .unwrap()
            .entries
            .contains_key(&dep_uri),
        "deleted file forgotten"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test(flavor = "current_thread")]
async fn save_rechecks_open_dependents() {
    let (service, state) = setup();
    let dep_uri: Url = "file:///dep.zz".parse().unwrap();
    open_and_check(
        &state,
        &dep_uri,
        "pub func answer() -> int {\n    return 1\n}\n",
    );
    let main_uri: Url = "file:///main.zz".parse().unwrap();
    open_and_check(&state, &main_uri, "func main() {\n    println(1)\n}\n");
    state.record_dependency(dep_uri.clone(), main_uri.clone());
    state.documents.get_mut(&main_uri).unwrap().check_result = None;

    service
        .inner()
        .did_save(DidSaveTextDocumentParams {
            text_document: TextDocumentIdentifier {
                uri: dep_uri.clone(),
            },
            text: None,
        })
        .await;

    assert!(
        state
            .documents
            .get(&dep_uri)
            .unwrap()
            .check_result
            .is_some(),
        "saved file rechecked"
    );
    assert!(
        state
            .documents
            .get(&main_uri)
            .unwrap()
            .check_result
            .is_some(),
        "open dependent rechecked on save"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn code_lens_lists_tests() {
    let (service, state) = setup();
    let uri: Url = "file:///tests.zz".parse().unwrap();
    let src = "@test\nfunc adds() {\n    assert_eq(1 + 1, 2)\n}\nfunc helper() -> int {\n    return 1\n}\n";
    open_and_check(&state, &uri, src);

    let lenses = service
        .inner()
        .code_lens(CodeLensParams {
            text_document: TextDocumentIdentifier { uri },
            work_done_progress_params: WorkDoneProgressParams::default(),
            partial_result_params: PartialResultParams::default(),
        })
        .await
        .unwrap()
        .expect("lenses");
    assert_eq!(lenses.len(), 1, "one lens per @test");
    let command = lenses[0].command.clone().expect("command");
    assert_eq!(command.command, "zz.runTest");
    assert_eq!(lenses[0].range.start.line, 0);
}

#[tokio::test(flavor = "current_thread")]
async fn execute_unknown_command_rejected() {
    let (service, _) = setup();
    let err = service
        .inner()
        .execute_command(ExecuteCommandParams {
            command: "zz.nope".to_string(),
            arguments: vec![],
            work_done_progress_params: WorkDoneProgressParams::default(),
        })
        .await
        .expect_err("unknown command rejected");
    assert_eq!(err.code, tower_lsp::jsonrpc::ErrorCode::MethodNotFound);
}

#[tokio::test(flavor = "current_thread")]
async fn goto_selective_member_jumps_to_def() {
    let (service, state) = setup();
    let lib_src = "pub func circle(r: float) -> float {\n    return r\n}\n";
    let tmp = std::env::temp_dir().join(format!("zz_lsp_goto_{}", std::process::id()));
    let lib_uri = Url::from_file_path(tmp.join("lib.zz")).unwrap();
    open_and_check(&state, &lib_uri, lib_src);
    index_test_file(&state, &lib_uri, "lib", lib_src);

    let main_src = "import lib(circle)\nfunc main() {\n    y := circle(1.0)\n    println(y)\n}\n";
    let main_uri = Url::from_file_path(tmp.join("main.zz")).unwrap();
    open_and_check(&state, &main_uri, main_src);

    let def = service
        .inner()
        .goto_definition(GotoDefinitionParams {
            text_document_position_params: TextDocumentPositionParams {
                text_document: TextDocumentIdentifier { uri: main_uri },
                position: Position {
                    line: 0,
                    character: 13,
                },
            },
            work_done_progress_params: WorkDoneProgressParams::default(),
            partial_result_params: PartialResultParams::default(),
        })
        .await
        .unwrap()
        .expect("selective member resolves");
    match def {
        GotoDefinitionResponse::Scalar(loc) => {
            assert_eq!(loc.uri, lib_uri);
            assert_eq!(loc.range.start.line, 0, "lands on the def");
        }
        _ => panic!("expected scalar location"),
    }
}

#[tokio::test(flavor = "current_thread")]
async fn goto_dep_path_jumps_to_package_entry() {
    let (service, state) = setup();
    let dir = watched_tmp("gotodep");
    std::fs::create_dir_all(dir.join("vendor/tiny/src")).unwrap();
    std::fs::write(
        dir.join("zz.toml"),
        "[package]\nname = \"proj\"\nversion = \"0.1.0\"\n\n[dependencies]\ntiny = \"^0.1.0\"\n",
    )
    .unwrap();
    std::fs::write(
        dir.join("vendor/tiny/src/tiny.zz"),
        "pub func shout(name: str) -> str {\n    return \"Hey {name}!\"\n}\n",
    )
    .unwrap();
    state.set_root(dir.clone());

    let main_uri = Url::from_file_path(dir.join("main.zz")).unwrap();
    let main_src = "import tiny\nfunc main() {\n    println(tiny.shout(\"a\"))\n}\n";
    open_and_check(&state, &main_uri, main_src);

    let def = service
        .inner()
        .goto_definition(GotoDefinitionParams {
            text_document_position_params: TextDocumentPositionParams {
                text_document: TextDocumentIdentifier { uri: main_uri },
                position: Position {
                    line: 0,
                    character: 9,
                },
            },
            work_done_progress_params: WorkDoneProgressParams::default(),
            partial_result_params: PartialResultParams::default(),
        })
        .await
        .unwrap()
        .expect("dep path resolves");
    let tiny_uri = Url::from_file_path(dir.join("vendor/tiny/src/tiny.zz")).unwrap();
    match def {
        GotoDefinitionResponse::Scalar(loc) => {
            assert_eq!(loc.uri, tiny_uri);
        }
        _ => panic!("expected scalar location"),
    }
    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test(flavor = "current_thread")]
async fn rename_onto_existing_name_blocked() {
    let (service, state) = setup();
    let uri: Url = "file:///test.zz".parse().unwrap();
    open_and_check(&state, &uri, "a := 1\nb := 2\nc := a + b\n");

    let err = service
        .inner()
        .rename(RenameParams {
            text_document_position: TextDocumentPositionParams {
                text_document: TextDocumentIdentifier { uri },
                position: Position {
                    line: 0,
                    character: 0,
                },
            },
            new_name: "b".to_string(),
            work_done_progress_params: WorkDoneProgressParams::default(),
        })
        .await
        .expect_err("collision must fail");
    assert_eq!(err.code, tower_lsp::jsonrpc::ErrorCode::InvalidParams);
}

#[tokio::test(flavor = "current_thread")]
async fn rename_to_same_name_is_noop() {
    let (service, state) = setup();
    let uri: Url = "file:///test.zz".parse().unwrap();
    open_and_check(&state, &uri, "a := 1\nb := a + 1\n");

    let edit = service
        .inner()
        .rename(RenameParams {
            text_document_position: TextDocumentPositionParams {
                text_document: TextDocumentIdentifier { uri },
                position: Position {
                    line: 0,
                    character: 0,
                },
            },
            new_name: "a".to_string(),
            work_done_progress_params: WorkDoneProgressParams::default(),
        })
        .await
        .unwrap();
    assert!(edit.is_none(), "same-name rename is a no-op");
}

fn undefined_diag_for(message: &str) -> Diagnostic {
    let raw = zz_frontend::diag::RawDiag {
        severity: zz_frontend::diag::Severity::Error,
        message: message.to_string(),
        span: Some(zz_frontend::span::Span::new(0, 1)),
        notes: vec![],
        fixits: vec![],
        secondary: None,
    };
    Diagnostic {
        range: Range {
            start: Position {
                line: 0,
                character: 0,
            },
            end: Position {
                line: 0,
                character: 5,
            },
        },
        severity: Some(DiagnosticSeverity::ERROR),
        code: None,
        code_description: None,
        source: Some("zz".to_string()),
        message: message.to_string(),
        related_information: None,
        tags: None,
        data: Some(serde_json::to_value(raw).unwrap()),
    }
}

fn import_action_titles(actions: &[CodeActionOrCommand]) -> Vec<String> {
    actions
        .iter()
        .filter_map(|a| match a {
            CodeActionOrCommand::CodeAction(a) => Some(a.title.clone()),
            _ => None,
        })
        .filter(|t| t.starts_with("add import"))
        .collect()
}

#[tokio::test(flavor = "current_thread")]
async fn auto_import_silent_for_member_typo() {
    // `lib.squre` with `lib` imported is a member error, not a missing
    // import — no matter what the workspace contains.
    let (service, state) = setup();
    let lib_src = "pub func circle(r: float) -> float {\n    return r\n}\n";
    let lib_uri: Url = "file:///lib.zz".parse().unwrap();
    open_and_check(&state, &lib_uri, lib_src);
    index_test_file(&state, &lib_uri, "lib", lib_src);
    let main_uri: Url = "file:///main.zz".parse().unwrap();
    let main_src = "import lib\nfunc main() {\n    y := lib.squre(1.0)\n    println(y)\n}\n";
    open_and_check(&state, &main_uri, main_src);

    let actions = service
        .inner()
        .code_action(CodeActionParams {
            text_document: TextDocumentIdentifier { uri: main_uri },
            range: Range {
                start: Position {
                    line: 2,
                    character: 0,
                },
                end: Position {
                    line: 2,
                    character: 20,
                },
            },
            context: CodeActionContext {
                trigger_kind: None,
                diagnostics: vec![undefined_diag_for("undefined variable `lib.squre`")],
                only: None,
            },
            work_done_progress_params: WorkDoneProgressParams::default(),
            partial_result_params: PartialResultParams::default(),
        })
        .await
        .unwrap()
        .unwrap_or_default();
    assert!(
        import_action_titles(&actions).is_empty(),
        "member typos must not offer imports"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn auto_import_silent_for_local_typo() {
    // `prntln` matches no provider anywhere: silence, not guesses.
    let (service, state) = setup();
    let uri: Url = "file:///main.zz".parse().unwrap();
    open_and_check(&state, &uri, "func main() {\n    prntln(\"hi\")\n}\n");

    let actions = service
        .inner()
        .code_action(CodeActionParams {
            text_document: TextDocumentIdentifier { uri },
            range: Range {
                start: Position {
                    line: 1,
                    character: 0,
                },
                end: Position {
                    line: 1,
                    character: 14,
                },
            },
            context: CodeActionContext {
                trigger_kind: None,
                diagnostics: vec![undefined_diag_for("undefined variable `prntln`")],
                only: None,
            },
            work_done_progress_params: WorkDoneProgressParams::default(),
            partial_result_params: PartialResultParams::default(),
        })
        .await
        .unwrap()
        .unwrap_or_default();
    assert!(
        import_action_titles(&actions).is_empty(),
        "unresolvable names must not offer imports"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn rename_field_renames_def_label_and_typed_usages() {
    let (service, state) = setup();
    let uri: Url = "file:///lib.zz".parse().unwrap();
    let src = "struct Point {\n    x: int,\n    y: int,\n}\nstruct Other {\n    x: int,\n}\np := Point{x: 1, y: 2}\nq := Other{x: 3}\na := p.x\nb := q.x\n";
    open_and_check(&state, &uri, src);

    let edit = service
        .inner()
        .rename(RenameParams {
            text_document_position: TextDocumentPositionParams {
                text_document: TextDocumentIdentifier { uri: uri.clone() },
                position: Position {
                    line: 1,
                    character: 4,
                },
            },
            new_name: "z".to_string(),
            work_done_progress_params: WorkDoneProgressParams::default(),
        })
        .await
        .unwrap()
        .expect("field rename produces edits");
    let changes = edit.changes.expect("changes");
    assert_eq!(changes.len(), 1, "single file touched");
    let edits = &changes[&uri];
    // Def site + literal label + typed usage; sibling struct untouched.
    assert_eq!(edits.len(), 3, "def + label + usage, got: {edits:?}");
    assert!(edits.iter().all(|e| e.new_text == "z"));
    let lines: Vec<u32> = edits.iter().map(|e| e.range.start.line).collect();
    assert!(lines.contains(&1), "def site renamed");
    assert!(lines.contains(&7), "literal label renamed");
    assert!(lines.contains(&9), "typed usage renamed");
}

#[tokio::test(flavor = "current_thread")]
async fn rename_field_collision_blocked() {
    // `Other.y` blocks renaming `Point.x` to `y`, same file.
    let (service, state) = setup();
    let uri: Url = "file:///lib.zz".parse().unwrap();
    open_and_check(
        &state,
        &uri,
        "struct Point {\n    x: int,\n}\nstruct Other {\n    y: int,\n}\n",
    );

    let err = service
        .inner()
        .rename(RenameParams {
            text_document_position: TextDocumentPositionParams {
                text_document: TextDocumentIdentifier { uri },
                position: Position {
                    line: 1,
                    character: 4,
                },
            },
            new_name: "y".to_string(),
            work_done_progress_params: WorkDoneProgressParams::default(),
        })
        .await
        .expect_err("field collision must fail");
    assert_eq!(err.code, tower_lsp::jsonrpc::ErrorCode::InvalidParams);
}

fn index_test_file(state: &GlobalState, uri: &Url, module: &str, src: &str) {
    let entry =
        crate::cross_file::parse_file_entry(uri.clone(), src.to_string(), module.to_string());
    let mut index = state.module_index.write().unwrap();
    index.module_to_uri.insert(module.to_string(), uri.clone());
    index.uri_to_module.insert(uri.clone(), module.to_string());
    index.entries.insert(uri.clone(), entry);
}

#[tokio::test(flavor = "current_thread")]
async fn rename_reaches_importing_file_not_unrelated() {
    let (service, state) = setup();
    let lib_src = "pub func circle(r: float) -> float {\n    return r\n}\n";
    let tmp = std::env::temp_dir().join(format!("zz_lsp_rename_{}", std::process::id()));
    let lib_uri = Url::from_file_path(tmp.join("lib.zz")).unwrap();
    open_and_check(&state, &lib_uri, lib_src);
    index_test_file(&state, &lib_uri, "lib", lib_src);

    let main_src = "import lib(circle)\nfunc main() {\n    y := circle(1.0)\n    println(y)\n}\n";
    let main_uri = Url::from_file_path(tmp.join("main.zz")).unwrap();
    open_and_check(&state, &main_uri, main_src);

    // Same name, no import: must not be touched.
    let other_src = "func circle() -> int {\n    return 1\n}\nc := circle()\n";
    let other_uri = Url::from_file_path(tmp.join("other.zz")).unwrap();
    open_and_check(&state, &other_uri, other_src);

    let edit = service
        .inner()
        .rename(RenameParams {
            text_document_position: TextDocumentPositionParams {
                text_document: TextDocumentIdentifier {
                    uri: main_uri.clone(),
                },
                position: Position {
                    line: 2,
                    character: 10,
                },
            },
            new_name: "round".to_string(),
            work_done_progress_params: WorkDoneProgressParams::default(),
        })
        .await
        .unwrap()
        .expect("rename should produce edits");
    let changes = edit.changes.expect("changes");
    assert!(changes.contains_key(&lib_uri), "defining file edited");
    assert!(changes.contains_key(&main_uri), "importing file edited");
    assert!(
        !changes.contains_key(&other_uri),
        "unrelated same-name file untouched"
    );
    // Member edit in main covers exactly `circle` in `circle(1.0)`.
    let main_edits = &changes[&main_uri];
    assert!(
        main_edits
            .iter()
            .any(|e| e.range.start == Position::new(2, 9) && e.range.end == Position::new(2, 15)),
        "usage range exact: {main_edits:?}"
    );
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
async fn code_action_blank_position_returns_none() {
    let (service, state) = setup();
    let uri: Url = "file:///test.zz".parse().unwrap();
    let src = "x := 1\n";
    open_and_check(&state, &uri, src);

    // Position (0, 2) sits on `:=`: no diagnostic, no symbol, no
    // statement range — nothing to offer (a cursor *on* `x` would offer
    // inline/remove refactors).
    let pos = Position {
        line: 0,
        character: 2,
    };
    let range = Range {
        start: pos,
        end: pos,
    };
    let actions = service
        .inner()
        .code_action(CodeActionParams {
            text_document: TextDocumentIdentifier { uri },
            range,
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

fn populate_module_index(state: &GlobalState, files: Vec<(Url, &str, &str)>) {
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
        index
            .uri_to_module
            .insert(uri.clone(), module_path.to_string());
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
async fn goto_definition_collection_member_string() {
    let (service, state) = setup();
    let root = std::env::temp_dir().join(format!("zz_lsp_goto_collection_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(root.join("src")).unwrap();
    std::fs::create_dir_all(root.join("commands")).unwrap();
    std::fs::write(
        root.join("zz.toml"),
        "[package]\nname = \"demo\"\nversion = \"0.1.0\"\n\
         [collect.commands]\ndir = \"commands\"\nsig = \"func([str]) -> int\"\n",
    )
    .unwrap();
    let member_path = root.join("commands/greet.zz");
    std::fs::write(
        &member_path,
        "pub func run(args: [str]) -> int { return 0 }\n",
    )
    .unwrap();
    let main_path = root.join("src/main.zz");
    let main_src = "import collect.commands\ncommands.get(\"greet\")\n";
    std::fs::write(&main_path, main_src).unwrap();

    let main_uri = Url::from_file_path(&main_path).unwrap();
    let member_uri = Url::from_file_path(&member_path).unwrap();
    open_and_check(&state, &main_uri, main_src);

    let def = service
        .inner()
        .goto_definition(GotoDefinitionParams {
            text_document_position_params: TextDocumentPositionParams {
                text_document: TextDocumentIdentifier {
                    uri: main_uri.clone(),
                },
                position: Position {
                    line: 1,
                    character: 16,
                },
            },
            work_done_progress_params: WorkDoneProgressParams::default(),
            partial_result_params: PartialResultParams::default(),
        })
        .await
        .unwrap();

    match def.expect("member string resolves to its file") {
        GotoDefinitionResponse::Scalar(loc) => {
            assert_eq!(loc.uri, member_uri);
            assert_eq!(loc.range.start.line, 0);
        }
        _ => panic!("expected scalar location"),
    }
    let _ = std::fs::remove_dir_all(&root);
}

#[tokio::test(flavor = "current_thread")]
async fn goto_definition_collection_import() {
    let (service, state) = setup();
    let root = std::env::temp_dir().join(format!(
        "zz_lsp_goto_collection_import_{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(root.join("src")).unwrap();
    std::fs::create_dir_all(root.join("commands")).unwrap();
    std::fs::write(
        root.join("zz.toml"),
        "[package]\nname = \"demo\"\nversion = \"0.1.0\"\n\
         [collect.commands]\ndir = \"commands\"\nsig = \"func([str]) -> int\"\n",
    )
    .unwrap();
    let member_path = root.join("commands/greet.zz");
    std::fs::write(
        &member_path,
        "pub func run(args: [str]) -> int { return 0 }\n",
    )
    .unwrap();
    let main_path = root.join("src/main.zz");
    let main_src = "import collect.commands\ncommands.all()\n";
    std::fs::write(&main_path, main_src).unwrap();

    let main_uri = Url::from_file_path(&main_path).unwrap();
    let member_uri = Url::from_file_path(&member_path).unwrap();
    open_and_check(&state, &main_uri, main_src);

    let def = service
        .inner()
        .goto_definition(GotoDefinitionParams {
            text_document_position_params: TextDocumentPositionParams {
                text_document: TextDocumentIdentifier { uri: main_uri },
                position: Position {
                    line: 0,
                    character: 18,
                },
            },
            work_done_progress_params: WorkDoneProgressParams::default(),
            partial_result_params: PartialResultParams::default(),
        })
        .await
        .unwrap();

    match def.expect("collection import resolves to first member") {
        GotoDefinitionResponse::Scalar(loc) => {
            assert_eq!(loc.uri, member_uri);
        }
        _ => panic!("expected scalar location"),
    }
    let _ = std::fs::remove_dir_all(&root);
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
        vec![(uri_a.clone(), "a", src_a), (uri_b.clone(), "b", src_b)],
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
        vec![(uri_a.clone(), "a", src_a), (uri_b.clone(), "b", src_b)],
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

    assert!(changes.contains_key(&uri_a), "should have edits in file a");
    assert!(changes.contains_key(&uri_b), "should have edits in file b");

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

    populate_module_index(&state, vec![(uri_indexed.clone(), "indexed", src_indexed)]);

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
    let root = std::env::temp_dir().join(format!("zz_lsp_init_ws_{}", std::process::id()));
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
    let root = std::env::temp_dir().join(format!("zz_lsp_folders_ws_{}", std::process::id()));
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

// ── Issue #256: selective-import diagnostics match `zz check` ─────────────

fn check_errors_for(state: &GlobalState, uri: &Url) -> Vec<String> {
    let doc = state.documents.get(uri).expect("doc should exist");
    let program = doc.program.as_ref().expect("program should exist").clone();
    let (ib, ifunc, is, ia, ie) = state.checker_seed_for(&program);
    let cr = zz_checker::check_program(&program, ib, ifunc, is, ia, ie);
    cr.errors.into_iter().map(|e| e.message).collect()
}

#[tokio::test(flavor = "current_thread")]
async fn selective_const_import_is_clean() {
    let (_service, state) = setup();
    let uri: Url = "file:///sel.zz".parse().unwrap();
    let src = "import std.math(PI)\nfunc main() {\n    println(PI)\n}\n";
    state.update_document(uri.clone(), 1, src.to_string());
    let messages = check_errors_for(&state, &uri);
    assert!(
        messages.is_empty(),
        "selective const should be clean, got: {messages:?}"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn selective_fn_import_is_clean() {
    let (_service, state) = setup();
    let uri: Url = "file:///selfn.zz".parse().unwrap();
    let src =
        "import std.fs(read_dir)\nfunc main() {\n    d := read_dir(\".\")\n    println(d)\n}\n";
    state.update_document(uri.clone(), 1, src.to_string());
    let messages = check_errors_for(&state, &uri);
    assert!(
        messages.is_empty(),
        "selective fn should be clean, got: {messages:?}"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn selective_alias_import_is_clean() {
    let (_service, state) = setup();
    let uri: Url = "file:///selalias.zz".parse().unwrap();
    let src = "import std.math(PI as pi)\nfunc main() {\n    println(pi)\n}\n";
    state.update_document(uri.clone(), 1, src.to_string());
    let messages = check_errors_for(&state, &uri);
    assert!(
        messages.is_empty(),
        "aliased selective const should be clean, got: {messages:?}"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn unused_selective_import_warns_once() {
    let (_service, state) = setup();
    let uri: Url = "file:///selunused.zz".parse().unwrap();
    let src = "import std.math(PI)\nfunc main() {\n    println(1)\n}\n";
    state.update_document(uri.clone(), 1, src.to_string());
    let messages = check_errors_for(&state, &uri);
    assert_eq!(
        messages.len(),
        1,
        "genuinely-unused selective should warn exactly once, got: {messages:?}"
    );
    assert!(
        messages[0].contains("PI"),
        "warning should name the import, got: {:?}",
        messages[0]
    );
}

// ── Issue #257: completion serves bare and member positions ───────────────

async fn completion_labels(
    service: &tower_lsp::LspService<super::Backend>,
    uri: Url,
    line: u32,
    character: u32,
) -> Vec<(String, Option<CompletionItemKind>)> {
    use tower_lsp::LanguageServer;
    let resp = service
        .inner()
        .completion(CompletionParams {
            text_document_position: TextDocumentPositionParams {
                text_document: TextDocumentIdentifier { uri },
                position: Position { line, character },
            },
            work_done_progress_params: WorkDoneProgressParams::default(),
            partial_result_params: PartialResultParams::default(),
            context: None,
        })
        .await
        .unwrap()
        .expect("should have completions");
    match resp {
        CompletionResponse::Array(items) => items.into_iter().map(|i| (i.label, i.kind)).collect(),
        CompletionResponse::List(_) => panic!("expected array"),
    }
}

#[tokio::test(flavor = "current_thread")]
async fn completion_bare_position_serves_names_and_keywords() {
    let (service, state) = setup();
    let uri: Url = "file:///bare.zz".parse().unwrap();
    let src = "import std.math\nfunc main() {\n\n}\n";
    open_and_check(&state, &uri, src);

    // Empty line inside main (line 2, char 4): at minimum the imported
    // namespace plus keywords must appear, each with a kind.
    let items = completion_labels(&service, uri, 2, 4).await;
    assert!(!items.is_empty(), "bare position must not be empty");
    assert!(
        items.iter().any(|(l, _)| l == "math"),
        "should offer imported namespace `math`"
    );
    assert!(
        items.iter().any(|(l, _)| l == "return"),
        "should offer keywords"
    );
    assert!(
        items.iter().all(|(_, k)| k.is_some()),
        "every item must carry a kind"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn completion_math_member_lists_functions_with_detail() {
    let (service, state) = setup();
    let uri: Url = "file:///mathmem.zz".parse().unwrap();
    let src = "import std.math\nfunc main() {\n    m := math.\n}\n";
    open_and_check(&state, &uri, src);

    // End of `    m := math.` on line 2: `    m := math.` is 14 chars.
    let resp = service
        .inner()
        .completion(CompletionParams {
            text_document_position: TextDocumentPositionParams {
                text_document: TextDocumentIdentifier { uri },
                position: Position {
                    line: 2,
                    character: 14,
                },
            },
            work_done_progress_params: WorkDoneProgressParams::default(),
            partial_result_params: PartialResultParams::default(),
            context: None,
        })
        .await
        .unwrap()
        .expect("member position must not be empty");
    let items = match resp {
        CompletionResponse::Array(v) => v,
        CompletionResponse::List(_) => panic!("expected array"),
    };
    assert!(!items.is_empty(), "`math.` must list members");
    let labels: Vec<_> = items.iter().map(|i| i.label.as_str()).collect();
    assert!(
        labels.contains(&"abs") && labels.contains(&"sin"),
        "expected math members, got: {labels:?}"
    );
    assert!(
        items
            .iter()
            .all(|i| i.kind == Some(CompletionItemKind::FUNCTION)),
        "member items must be FUNCTION kind"
    );
    assert!(
        items
            .iter()
            .all(|i| i.detail.as_deref().is_some_and(|d| d.starts_with("math."))),
        "member items must carry `math.name` details"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn completion_serves_selective_bare_name() {
    let (service, state) = setup();
    let uri: Url = "file:///selcomp.zz".parse().unwrap();
    let src = "import std.math(PI)\nfunc main() {\n    println(P)\n}\n";
    open_and_check(&state, &uri, src);

    // After `P` on line 2 (`    println(P` is 13 chars): seeded `PI` appears.
    let items = completion_labels(&service, uri, 2, 13).await;
    assert!(
        items.iter().any(|(l, _)| l == "PI"),
        "selective `PI` should complete, got: {:?}",
        items.iter().map(|(l, _)| l).collect::<Vec<_>>()
    );
}

#[tokio::test(flavor = "current_thread")]
async fn completion_without_check_result_still_serves() {
    // No `open_and_check`: only the didOpen buffer exists, mimicking a
    // completion that races ahead of the async recheck (#257 self-heal).
    let (service, state) = setup();
    let uri: Url = "file:///race.zz".parse().unwrap();
    let src = "import std.math\nfunc main() {\n    m := math.\n}\n";
    state.update_document(uri.clone(), 1, src.to_string());

    let resp = service
        .inner()
        .completion(CompletionParams {
            text_document_position: TextDocumentPositionParams {
                text_document: TextDocumentIdentifier { uri },
                position: Position {
                    line: 2,
                    character: 14,
                },
            },
            work_done_progress_params: WorkDoneProgressParams::default(),
            partial_result_params: PartialResultParams::default(),
            context: None,
        })
        .await
        .unwrap()
        .expect("racing completion must not be empty");
    let n = match resp {
        CompletionResponse::Array(v) => v.len(),
        CompletionResponse::List(l) => l.items.len(),
    };
    assert!(n > 0, "on-demand check must serve member items");
}

#[tokio::test(flavor = "current_thread")]
async fn full_import_const_use_is_clean() {
    // The reported screenshot: `import std.math` + `println(math.PI)`
    // flagged BOTH `unused import 'math'` and `undefined variable`.
    // Seeded std namespaces resolve the const, marking the import used.
    let (_service, state) = setup();
    let uri: Url = "file:///mathpi.zz".parse().unwrap();
    let src = "import std.math\nfunc main() {\n    println(math.PI)\n}\n";
    state.update_document(uri.clone(), 1, src.to_string());
    let messages = check_errors_for(&state, &uri);
    assert!(
        messages.is_empty(),
        "full-import const use clean: {messages:?}"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn completion_dep_member_through_service() {
    // Live-shape regression: buffer-only probe inside a real project with
    // a vendored dep. Skipped when the fixture project is absent (it is
    // built by the packaging audit, not by cargo).
    let probe_path = std::path::PathBuf::from("/tmp/zzpkgtest/probe_svc.zz");
    if !probe_path.parent().unwrap().join("zz.toml").is_file() {
        return;
    }
    let (service, state) = setup();
    let uri = Url::from_file_path(&probe_path).unwrap();
    let src = "import table\nfunc main() {\n    t := table.\n}\n";
    open_and_check(&state, &uri, src);

    let doc = state.documents.get(&uri).unwrap().clone();
    assert!(doc.check_result.is_some(), "recheck must store a result");
    let cr = doc.check_result.as_ref().unwrap();
    assert!(
        cr.funcs.keys().any(|k| k == "table.render"),
        "seed must carry table.*"
    );

    let resp = service
        .inner()
        .completion(CompletionParams {
            text_document_position: TextDocumentPositionParams {
                text_document: TextDocumentIdentifier { uri },
                // `    t := table.` is 15 chars: the cursor sits AFTER the dot.
                position: Position {
                    line: 2,
                    character: 15,
                },
            },
            work_done_progress_params: WorkDoneProgressParams::default(),
            partial_result_params: PartialResultParams::default(),
            context: None,
        })
        .await
        .unwrap()
        .expect("member position must answer");
    let items = match resp {
        CompletionResponse::Array(v) => v,
        CompletionResponse::List(_) => panic!("expected array"),
    };
    assert!(
        items.iter().any(|i| i.label == "render"),
        "table. serves render, got: {:?}",
        items.iter().map(|i| &i.label).collect::<Vec<_>>()
    );
}
