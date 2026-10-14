//! Workspace-wide diagnostics on demand (`zz.checkProject`).
//!
//! Open files report diagnostics as you type, and saves refresh
//! dependents — but a broken file you never opened stays invisible.
//! This module closes that hole: one command checks every `.zz` file
//! under the project root and publishes the full truth to the Problems
//! panel.
//!
//! Diagnostics-only by design: unlike `recheck_and_publish` it never
//! mutates the global seed (checked-in results for unopened files would
//! go stale without an owner to prune them). Open buffers are checked
//! from memory, everything else from disk.

use std::sync::Arc;

use tower_lsp::lsp_types::*;
use zz_checker::check_program;

use crate::diagnostics::convert_diagnostic;
use crate::state::GlobalState;

/// The command editors invoke for a project-wide check.
pub const CHECK_PROJECT_COMMAND: &str = "zz.checkProject";

/// Safety cap: beyond this many files the command reports truncation
/// instead of stalling the editor on a generated tree.
const MAX_PROJECT_FILES: usize = 1000;

/// Check every project file: `(uri, diagnostics)` in path order.
/// Open documents are checked from their buffers, the rest from disk.
pub fn check_project_files(
    state: &GlobalState,
    root: &std::path::Path,
) -> Vec<(Url, Vec<Diagnostic>)> {
    let mut paths = crate::cross_file::scan_for_zz_files(root);
    paths.sort();
    let truncated = paths.len() > MAX_PROJECT_FILES;
    if truncated {
        paths.truncate(MAX_PROJECT_FILES);
    }
    let mut out = Vec::new();
    for path in &paths {
        let uri = match Url::from_file_path(path) {
            Ok(u) => u,
            Err(_) => continue,
        };
        // Open buffers win over disk (unsaved edits are the truth).
        let (source, program, parse_errors) = match state.documents.get(&uri) {
            Some(doc) => (
                doc.source.clone(),
                doc.program.clone(),
                doc.parse_errors.clone(),
            ),
            None => match std::fs::read_to_string(path) {
                Ok(source) => {
                    let parsed = zz_frontend::parse(&source);
                    (source, Some(parsed.program), parsed.errors)
                }
                Err(_) => continue,
            },
        };
        let Some(program) = program else {
            continue;
        };
        let (ib, ifunc, is, ia, ie) = state.checker_seed_for_path(&program, Some(path));
        let checked = check_program(&program, ib, ifunc, is, ia, ie);
        let mut raw = parse_errors;
        raw.extend(crate::import_seed::unknown_import_diags(&program));
        raw.extend(checked.errors);
        let diags: Vec<Diagnostic> = raw
            .iter()
            .filter(|e| e.span.is_some())
            .map(|d| convert_diagnostic(&source, d))
            .collect();
        out.push((uri, diags));
    }
    if truncated {
        log::warn!(
            "project check truncated at {MAX_PROJECT_FILES} files under {}",
            root.display()
        );
    }
    out
}

/// Run the check off-thread, publish every file (empty vecs clear
/// stale), and report a summary. Returns a JSON summary for the
/// `execute_command` response.
pub async fn run_project_check(
    state: Arc<GlobalState>,
    client: &tower_lsp::Client,
    root: std::path::PathBuf,
) -> serde_json::Value {
    client
        .show_message(MessageType::INFO, "checking project…")
        .await;
    let reports = tokio::task::spawn_blocking(move || check_project_files(&state, &root))
        .await
        .unwrap_or_default();
    let mut files = 0u32;
    let mut errors = 0u32;
    let mut warnings = 0u32;
    for (uri, diags) in &reports {
        files += 1;
        for d in diags {
            match d.severity {
                Some(DiagnosticSeverity::ERROR) => errors += 1,
                Some(DiagnosticSeverity::WARNING) => warnings += 1,
                _ => {}
            }
        }
        client
            .publish_diagnostics(uri.clone(), diags.clone(), None)
            .await;
    }
    let summary =
        format!("project check: {files} file(s), {errors} error(s), {warnings} warning(s)");
    client
        .show_message(
            if errors == 0 {
                MessageType::INFO
            } else {
                MessageType::ERROR
            },
            summary.clone(),
        )
        .await;
    serde_json::Value::String(summary)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn project_tmp(tag: &str) -> std::path::PathBuf {
        let dir =
            std::env::temp_dir().join(format!("zz-lsp-projcheck-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn closed_broken_file_reported() {
        let state = GlobalState::new();
        let dir = project_tmp("broken");
        std::fs::write(dir.join("main.zz"), "func main() {\n    println(1)\n}\n").unwrap();
        std::fs::write(
            dir.join("dep.zz"),
            "func broken() -> int {\n    return nope\n}\n",
        )
        .unwrap();

        let reports = check_project_files(&state, &dir);
        assert_eq!(reports.len(), 2);
        let dep = reports
            .iter()
            .find(|(u, _)| u.path().ends_with("dep.zz"))
            .expect("dep reported");
        assert!(
            dep.1.iter().any(|d| d.message.contains("nope")),
            "dep error surfaced: {:?}",
            dep.1.iter().map(|d| &d.message).collect::<Vec<_>>()
        );
        let main = reports
            .iter()
            .find(|(u, _)| u.path().ends_with("main.zz"))
            .expect("main reported");
        assert!(main.1.is_empty(), "clean file stays clean");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn open_buffer_wins_over_disk() {
        let state = GlobalState::new();
        let dir = project_tmp("buffer");
        let dep_path = dir.join("dep.zz");
        std::fs::write(&dep_path, "func broken() -> int {\n    return nope\n}\n").unwrap();
        // Same file open with fixed content: disk staleness must not leak.
        let uri = Url::from_file_path(&dep_path).unwrap();
        state.update_document(
            uri,
            1,
            "func broken() -> int {\n    return 1\n}\n".to_string(),
        );

        let reports = check_project_files(&state, &dir);
        let dep = reports
            .iter()
            .find(|(u, _)| u.path().ends_with("dep.zz"))
            .expect("dep reported");
        assert!(dep.1.is_empty(), "buffer truth, not disk: {:?}", dep.1);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
