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
//!
//! Closed-file results are cached by content hash only: if a file's
//! disk bytes haven't changed since the last check, its cached
//! diagnostics are reused without re-running the checker. This makes
//! repeated project checks incremental — only new or modified files
//! pay the checking cost.
//!
//! Pagination: `skip` and `take` let callers page through large projects
//! without checking every file on every call.

use std::sync::Arc;

use tower_lsp::lsp_types::*;
use zz_checker::check_program;

use crate::diagnostics::convert_diagnostic;
use crate::state::GlobalState;

/// The command editors invoke for a project-wide check.
pub const CHECK_PROJECT_COMMAND: &str = "zz.checkProject";

/// Check project files with pagination.
///
/// `skip` skips the first `skip` files (for pagination).
/// `take` limits how many files are returned (`None` = no limit).
/// Returns `((uri, diagnostics) in path order, total_file_count)`.
pub fn check_project_files(
    state: &GlobalState,
    root: &std::path::Path,
    skip: usize,
    take: Option<usize>,
) -> (Vec<(Url, Vec<Diagnostic>)>, usize) {
    let mut paths = crate::cross_file::scan_for_zz_files(root);
    paths.sort();
    let total = paths.len();
    // Apply pagination before checking: only pay for the files in this page.
    let paths: Vec<_> = paths.into_iter().skip(skip).take(take.unwrap_or(usize::MAX)).collect();
    let mut cache = state.project_cache.write().unwrap();
    let mut out = Vec::new();
    for path in &paths {
        let uri = match Url::from_file_path(path) {
            Ok(u) => u,
            Err(_) => continue,
        };
        // Open buffers win over disk (unsaved edits are the truth).
        if let Some(doc) = state.documents.get(&uri) {
            let (source, program, parse_errors) = (
                doc.source.clone(),
                doc.program.clone(),
                doc.parse_errors.clone(),
            );
            drop(doc);
            if let Some(diags) = check_one(state, path, &source, program, parse_errors) {
                out.push((uri, diags));
            }
            continue;
        }
        let source = match std::fs::read_to_string(path) {
            Ok(source) => source,
            Err(_) => continue,
        };
        let hash = crate::state::content_hash(&source);
        // Closed-file cache keyed by content hash only: unchanged disk
        // bytes means unchanged diagnostics (no re-check needed).
        if let Some((cached_hash, diags)) = cache.get(path) {
            if *cached_hash == hash {
                out.push((uri, diags.clone()));
                continue;
            }
        }
        let parsed = zz_frontend::parse(&source);
        let program = Some(parsed.program);
        let parse_errors = parsed.errors;
        if let Some(diags) = check_one(state, path, &source, program, parse_errors) {
            cache.insert(path.clone(), (hash, diags.clone()));
            out.push((uri, diags));
        }
    }
    // Drop entries for files that left the tree (renames/deletes).
    let live: std::collections::HashSet<_> = paths.iter().collect();
    cache.retain(|cached_path, _| live.contains(cached_path));
    (out, total)
}

/// Check one file: seed → check → convert. Returns `None` for files
/// with no program (unparseable shell).
fn check_one(
    state: &GlobalState,
    path: &std::path::Path,
    source: &str,
    program: Option<zz_frontend::ast::Program>,
    parse_errors: Vec<zz_frontend::diag::RawDiag>,
) -> Option<Vec<Diagnostic>> {
    let program = program?;
    let (ib, ifunc, is, ia, ie) = state.checker_seed_for_path(&program, Some(path));
    let checked = check_program(&program, ib, ifunc, is, ia, ie);
    let mut raw = parse_errors;
    raw.extend(crate::import_seed::unknown_import_diags(&program));
    raw.extend(checked.errors);
    let diags: Vec<Diagnostic> = raw
        .iter()
        .filter(|e| e.span.is_some())
        .map(|d| convert_diagnostic(source, d))
        .collect();
    Some(diags)
}

/// Run the check off-thread, publish every file (empty vecs clear
/// stale), and report a summary. Returns a JSON summary for the
/// `execute_command` response.
pub async fn run_project_check(
    state: Arc<GlobalState>,
    client: &tower_lsp::Client,
    root: std::path::PathBuf,
) -> serde_json::Value {
    run_project_check_paginated(state, client, root, 0, None).await
}

/// Run the check off-thread with pagination, publish every file in the
/// page, and report a summary.
pub async fn run_project_check_paginated(
    state: Arc<GlobalState>,
    client: &tower_lsp::Client,
    root: std::path::PathBuf,
    skip: usize,
    take: Option<usize>,
) -> serde_json::Value {
    client
        .show_message(MessageType::INFO, "checking project…")
        .await;
    let (reports, total) = tokio::task::spawn_blocking(move || {
        check_project_files(&state, &root, skip, take)
    })
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
    let summary = if total > reports.len() {
        format!(
            "project check: {} of {} file(s), {} error(s), {} warning(s)",
            reports.len(), total, errors, warnings
        )
    } else {
        format!("project check: {files} file(s), {errors} error(s), {warnings} warning(s)")
    };
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

        let (reports, _total) = check_project_files(&state, &dir, 0, None);
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

        let (reports, _total) = check_project_files(&state, &dir, 0, None);
        let dep = reports
            .iter()
            .find(|(u, _)| u.path().ends_with("dep.zz"))
            .expect("dep reported");
        assert!(dep.1.is_empty(), "buffer truth, not disk: {:?}", dep.1);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn cache_hit_reuses_diags_and_prunes_deleted() {
        let state = GlobalState::new();
        let dir = project_tmp("cachehit");
        let dep_path = dir.join("dep.zz");
        std::fs::write(&dep_path, "func broken() -> int {\n    return nope\n}\n").unwrap();

        let (first, _) = check_project_files(&state, &dir, 0, None);
        let dep_first = first
            .iter()
            .find(|(u, _)| u.path().ends_with("dep.zz"))
            .expect("dep reported");
        assert!(!dep_first.1.is_empty(), "broken file has diags");
        assert_eq!(state.project_cache.read().unwrap().len(), 1);

        // Unchanged second run: same diagnostics, served from cache.
        let (second, _) = check_project_files(&state, &dir, 0, None);
        let dep_second = second
            .iter()
            .find(|(u, _)| u.path().ends_with("dep.zz"))
            .expect("dep reported");
        assert_eq!(dep_first.1, dep_second.1);

        // Fix the file: cache invalidates, diagnostics clear.
        std::fs::write(&dep_path, "func broken() -> int {\n    return 1\n}\n").unwrap();
        let (third, _) = check_project_files(&state, &dir, 0, None);
        let dep_third = third
            .iter()
            .find(|(u, _)| u.path().ends_with("dep.zz"))
            .expect("dep reported");
        assert!(dep_third.1.is_empty(), "fixed file is clean");

        // Delete the file: entry pruned, file unreported.
        std::fs::remove_file(&dep_path).unwrap();
        let (fourth, _) = check_project_files(&state, &dir, 0, None);
        assert!(
            fourth.iter().all(|(u, _)| !u.path().ends_with("dep.zz")),
            "deleted file unreported"
        );
        assert!(
            state.project_cache.read().unwrap().is_empty(),
            "deleted entry pruned"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn pagination_returns_subset_and_total() {
        let state = GlobalState::new();
        let dir = project_tmp("pagination");
        for i in 0..10 {
            std::fs::write(
                dir.join(format!("f{i}.zz")),
                format!("func f{i}() -> int {{ return {i} }}\n"),
            )
            .unwrap();
        }

        // Page 1: first 3 files
        let (page1, total) = check_project_files(&state, &dir, 0, Some(3));
        assert_eq!(total, 10);
        assert_eq!(page1.len(), 3);

        // Page 2: next 3 files
        let (page2, _) = check_project_files(&state, &dir, 3, Some(3));
        assert_eq!(page2.len(), 3);

        // No overlap between pages
        let page1_uris: std::collections::HashSet<_> = page1.iter().map(|(u, _)| u.clone()).collect();
        let page2_uris: std::collections::HashSet<_> = page2.iter().map(|(u, _)| u.clone()).collect();
        assert!(page1_uris.is_disjoint(&page2_uris));

        // Last page: remaining 4 files
        let (page3, _) = check_project_files(&state, &dir, 6, Some(10));
        assert_eq!(page3.len(), 4);

        // Beyond end: empty
        let (page4, _) = check_project_files(&state, &dir, 20, Some(10));
        assert!(page4.is_empty());

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn cache_invalidates_when_content_changes() {
        let state = GlobalState::new();
        let dir = project_tmp("cachechange");
        let dep_path = dir.join("dep.zz");
        std::fs::write(&dep_path, "func main() {\n    println(1)\n}\n").unwrap();

        let (first, _) = check_project_files(&state, &dir, 0, None);
        assert!(first.iter().all(|(_, d)| d.is_empty()));
        assert_eq!(state.project_cache.read().unwrap().len(), 1);

        // Same content: cache hit, no re-check.
        let (second, _) = check_project_files(&state, &dir, 0, None);
        assert!(second.iter().all(|(_, d)| d.is_empty()));

        // Change content: cache invalidates.
        std::fs::write(&dep_path, "func main() {\n    println(2)\n}\n").unwrap();
        let (third, _) = check_project_files(&state, &dir, 0, None);
        assert!(third.iter().all(|(_, d)| d.is_empty()));

        let _ = std::fs::remove_dir_all(&dir);
    }
}
