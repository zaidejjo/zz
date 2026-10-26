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
///
/// Disk files cache by (content hash, seed version): consecutive runs
/// only pay for changed files or a moved seed. Open buffers always
/// recheck (unsaved edits are the truth) and never populate the cache.
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
    let seed_version = state.seed_version();
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
        if let Some((cached_hash, cached_seed, diags)) = cache.get(path) {
            if *cached_hash == hash && *cached_seed == seed_version {
                out.push((uri, diags.clone()));
                continue;
            }
        }
        let parsed = zz_frontend::parse(&source);
        let program = Some(parsed.program);
        let parse_errors = parsed.errors;
        if let Some(diags) = check_one(state, path, &source, program, parse_errors) {
            cache.insert(path.clone(), (hash, seed_version, diags.clone()));
            out.push((uri, diags));
        }
    }
    // Drop entries for files that left the tree (renames/deletes).
    let live: std::collections::HashSet<_> = paths.iter().collect();
    cache.retain(|cached_path, _| live.contains(cached_path));
    if truncated {
        log::warn!(
            "project check truncated at {MAX_PROJECT_FILES} files under {}",
            root.display()
        );
    }
    out
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

    #[test]
    fn cache_hit_reuses_diags_and_prunes_deleted() {
        let state = GlobalState::new();
        let dir = project_tmp("cachehit");
        let dep_path = dir.join("dep.zz");
        std::fs::write(&dep_path, "func broken() -> int {\n    return nope\n}\n").unwrap();

        let first = check_project_files(&state, &dir);
        let dep_first = first
            .iter()
            .find(|(u, _)| u.path().ends_with("dep.zz"))
            .expect("dep reported");
        assert!(!dep_first.1.is_empty(), "broken file has diags");
        assert_eq!(state.project_cache.read().unwrap().len(), 1);

        // Unchanged second run: same diagnostics, served from cache.
        let second = check_project_files(&state, &dir);
        let dep_second = second
            .iter()
            .find(|(u, _)| u.path().ends_with("dep.zz"))
            .expect("dep reported");
        assert_eq!(dep_first.1, dep_second.1);

        // Fix the file: cache invalidates, diagnostics clear.
        std::fs::write(&dep_path, "func broken() -> int {\n    return 1\n}\n").unwrap();
        let third = check_project_files(&state, &dir);
        let dep_third = third
            .iter()
            .find(|(u, _)| u.path().ends_with("dep.zz"))
            .expect("dep reported");
        assert!(dep_third.1.is_empty(), "fixed file is clean");

        // Delete the file: entry pruned, file unreported.
        std::fs::remove_file(&dep_path).unwrap();
        let fourth = check_project_files(&state, &dir);
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
    fn cache_invalidates_when_seed_moves() {
        let state = GlobalState::new();
        let dir = project_tmp("cacheseed");
        let dep_path = dir.join("dep.zz");
        std::fs::write(&dep_path, "func main() {\n    println(1)\n}\n").unwrap();

        let first = check_project_files(&state, &dir);
        assert!(first.iter().all(|(_, d)| d.is_empty()));
        // Simulate an unrelated absorb between runs: same disk content,
        // moved seed → entry recomputed, not blindly reused.
        state
            .seed_version
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let second = check_project_files(&state, &dir);
        assert!(second.iter().all(|(_, d)| d.is_empty()));
        let (_, cached_seed, _) = state
            .project_cache
            .read()
            .unwrap()
            .get(&dep_path)
            .cloned()
            .expect("entry refreshed");
        assert_eq!(cached_seed, 1);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
