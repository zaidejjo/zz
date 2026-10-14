//! Filesystem watching: keep the workspace index and dependent
//! diagnostics fresh when files change *outside* open buffers.
//!
//! Open buffers are owned by `did_change`; this module covers everything
//! else: a dependency `.zz` edited in another editor pane, a `zz.toml` /
//! `zz.lock` rewritten by `zz add`, a file deleted behind our back.
//! Without it the module index, dep caches, and dependents' diagnostics
//! go stale until restart.
//!
//! Flow: after `initialized`, the server registers `**/*.zz`,
//! `**/zz.toml`, and `**/zz.lock` watchers (dynamic registration; clients
//! without support get a logged no-op). Change events re-index the file,
//! drop the affected caches, and recheck open dependents.

use std::sync::Arc;

use tower_lsp::lsp_types::*;
use tower_lsp::Client;

use crate::state::GlobalState;

/// Register workspace file watchers with the client.
///
/// Best-effort: clients without dynamic registration return an error,
/// which is logged and ignored (polling is deliberately not added —
/// untracked clients simply keep the old restart-to-refresh behavior).
pub async fn register_watches(client: &Client) {
    let watchers = ["**/*.zz", "**/zz.toml", "**/zz.lock"]
        .into_iter()
        .map(|glob| FileSystemWatcher {
            glob_pattern: GlobPattern::String(glob.to_string()),
            kind: None,
        })
        .collect();
    let options = DidChangeWatchedFilesRegistrationOptions { watchers };
    let registration = Registration {
        id: "zz-workspace-watch".to_string(),
        method: "workspace/didChangeWatchedFiles".to_string(),
        register_options: Some(serde_json::to_value(options).unwrap_or_default()),
    };
    if let Err(e) = client.register_capability(vec![registration]).await {
        log::debug!("file watching not supported by client: {e}");
    } else {
        log::info!("watching **/*.zz, **/zz.toml, **/zz.lock");
    }
}

/// Handle one batch of watched-file events.
pub async fn handle_watched_changes(
    state: Arc<GlobalState>,
    client: &Client,
    params: DidChangeWatchedFilesParams,
) {
    for event in &params.changes {
        handle_watched_event(state.clone(), client, event).await;
    }
}

async fn handle_watched_event(state: Arc<GlobalState>, client: &Client, event: &FileEvent) {
    let path = match event.uri.to_file_path() {
        Ok(p) => p,
        Err(_) => return,
    };
    let file_name = path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or_default();
    let is_manifest = file_name == "zz.toml" || file_name == "zz.lock";

    if event.typ == FileChangeType::DELETED {
        forget_file(&state, &event.uri, &path);
        if is_manifest {
            manifest_changed(&state, client, &path).await;
        } else {
            recheck_open_dependents(&state, client, &event.uri).await;
        }
        return;
    }

    if is_manifest {
        manifest_changed(&state, client, &path).await;
        return;
    }

    if path.extension().and_then(|e| e.to_str()) != Some("zz") {
        return;
    }
    // Open buffers are owned by did_change — disk truth is stale there.
    if state.documents.contains_key(&event.uri) {
        return;
    }
    let source = match std::fs::read_to_string(&path) {
        Ok(s) => s,
        Err(_) => {
            forget_file(&state, &event.uri, &path);
            return;
        }
    };
    reindex_file(&state, &event.uri, &path, source);
    recheck_open_dependents(&state, client, &event.uri).await;
}

/// Re-parse and re-index one workspace file.
fn reindex_file(state: &GlobalState, uri: &Url, path: &std::path::Path, source: String) {
    let root = state.root.read().unwrap().clone();
    let module_path = root
        .as_deref()
        .and_then(|r| crate::cross_file::module_path_for_file(path, r));
    let entry = crate::cross_file::parse_file_entry(
        uri.clone(),
        source,
        module_path.clone().unwrap_or_default(),
    );
    {
        let mut index = state.module_index.write().unwrap();
        if let Some(module) = module_path {
            let old = index.uri_to_module.get(uri).cloned();
            if let Some(old) = old {
                index.module_to_uri.remove(&old);
            }
            index.module_to_uri.insert(module.clone(), uri.clone());
            index.uri_to_module.insert(uri.clone(), module);
        }
        index.entries.insert(uri.clone(), entry);
    }
    // Drop harvested workspace seeds for this file (namespace unknown
    // here — evict every entry keyed by this path).
    state
        .ws_cache
        .write()
        .unwrap()
        .retain(|(file, _), _| file.as_path() != path);
}

/// Forget a deleted file everywhere.
fn forget_file(state: &GlobalState, uri: &Url, path: &std::path::Path) {
    {
        let mut index = state.module_index.write().unwrap();
        if let Some(old) = index.uri_to_module.remove(uri) {
            index.module_to_uri.remove(&old);
        }
        index.entries.remove(uri);
    }
    state
        .ws_cache
        .write()
        .unwrap()
        .retain(|(file, _), _| file.as_path() != path);
    state.dependents.remove(uri);
}

/// A manifest changed: drop locked-dep seeds for its project root and
/// recheck every open file under that root.
async fn manifest_changed(state: &Arc<GlobalState>, client: &Client, path: &std::path::Path) {
    let Some(root) = path.parent().map(|p| p.to_path_buf()) else {
        return;
    };
    state.dep_cache.write().unwrap().remove(&root);
    log::info!("manifest changed, dep cache dropped for {}", root.display());
    publish_manifest_diagnostic(client, &root).await;
    for entry in state.documents.iter() {
        let uri = entry.key();
        let under_root = uri
            .to_file_path()
            .ok()
            .is_some_and(|p| p.starts_with(&root));
        if under_root {
            crate::diagnostics::recheck_and_publish(state.clone(), client, uri.clone()).await;
        }
    }
}

/// Publish (or clear) the `zz.toml` diagnostic for `root`.
///
/// A malformed manifest is otherwise completely silent in the editor
/// while every import quietly fails to resolve — one file-level error
/// with the parse message fixes that.
pub async fn publish_manifest_diagnostic(client: &Client, root: &std::path::Path) {
    let manifest_uri = match Url::from_file_path(root.join("zz.toml")) {
        Ok(u) => u,
        Err(_) => return,
    };
    client
        .publish_diagnostics(manifest_uri, manifest_diagnostic_for(root), None)
        .await;
}

/// The `zz.toml` diagnostic for `root`: one parse error, or nothing when
/// the manifest loads.
pub fn manifest_diagnostic_for(root: &std::path::Path) -> Vec<Diagnostic> {
    match zz_pm::manifest::Manifest::load(&root.join("zz.toml")) {
        Ok(_) => Vec::new(),
        Err(e) => vec![Diagnostic {
            range: Range {
                start: Position {
                    line: 0,
                    character: 0,
                },
                end: Position {
                    line: 0,
                    character: 0,
                },
            },
            severity: Some(DiagnosticSeverity::ERROR),
            code: None,
            code_description: None,
            source: Some("zz".to_string()),
            message: e,
            related_information: None,
            tags: None,
            data: None,
        }],
    }
}

/// Recheck every *open* file that depends on `uri`.
///
/// One level only: transitive dependents refresh when their own
/// dependencies change or on save. Bounded by the open set — never a
/// full-project sweep per keystroke.
async fn recheck_open_dependents(state: &Arc<GlobalState>, client: &Client, uri: &Url) {
    for dependent in state.get_dependents(uri) {
        if state.documents.contains_key(&dependent) {
            crate::diagnostics::recheck_and_publish(state.clone(), client, dependent.clone()).await;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn manifest_tmp(tag: &str, content: &str) -> std::path::PathBuf {
        let dir =
            std::env::temp_dir().join(format!("zz-lsp-manifest-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("zz.toml"), content).unwrap();
        dir
    }

    #[test]
    fn broken_manifest_yields_error() {
        let dir = manifest_tmp("broken", "[package\nname = ");
        let diags = manifest_diagnostic_for(&dir);
        assert_eq!(diags.len(), 1);
        assert_eq!(
            diags[0].severity,
            Some(tower_lsp::lsp_types::DiagnosticSeverity::ERROR)
        );
        assert!(
            diags[0].message.contains("zz.toml"),
            "message names the file: {}",
            diags[0].message
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn valid_manifest_yields_nothing() {
        let dir = manifest_tmp("valid", "[package]\nname = \"app\"\nversion = \"0.1.0\"\n");
        assert!(manifest_diagnostic_for(&dir).is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
