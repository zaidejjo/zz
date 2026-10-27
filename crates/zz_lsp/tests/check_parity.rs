//! LSP-vs-check diagnostic parity gate (#264).
//!
//! Editor truth is a CI property: for every file in the corpus, the
//! diagnostic set from `zz check --format json` must equal the set the
//! language server would publish for the same file (didOpen path).
//! Selective-import false positives shipped server-only once (#256)
//! because no test pinned the two analysis paths together.
//!
//! How it works: the check side shells out to the freshly built `zz`
//! binary (same code path as the CLI); the LSP side drives
//! `GlobalState` in-process exactly like `didOpen` + `recheck`
//! (`update_document` → `checker_seed_for_path` → `check_program` →
//! `absorb_result`), collecting the same `parse_errors + errors` set
//! `recheck_and_publish` sends. Both sides normalize to
//! `(severity, message, span)` triples with span-less diagnostics
//! dropped (the publish path never sends those), then compare per
//! file.
//!
//! To add a case: drop a `.zz` file in `tests/fixtures/errors/` (or a
//! known-clean file — see CLEAN_FILES). It joins automatically; no
//! registration. Genuine divergences go in
//! `tests/fixtures/lsp_parity_allowlist.toml` with a reason and issue
//! link — never silently.

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::process::Command;

use tower_lsp::lsp_types::Url;
use zz_frontend::diag::RawDiag;
use zz_lsp::state::GlobalState;

/// Allowlist entry: a divergence under active investigation.
#[derive(Debug, serde::Deserialize)]
struct AllowEntry {
    file: String,
    reason: String,
}

#[derive(Debug, serde::Deserialize)]
struct Allowlist {
    #[serde(default)]
    allow: Vec<AllowEntry>,
}

/// Check-side JSON shape from `zz check --format json`.
#[derive(Debug, serde::Deserialize)]
struct CheckJson {
    files: Vec<CheckJsonFile>,
}

#[derive(Debug, serde::Deserialize)]
struct CheckJsonFile {
    file: String,
    diagnostics: Vec<RawDiag>,
}

/// Normalized diagnostic for comparison: severity + message + byte span.
/// Span-less diagnostics never reach `publishDiagnostics`, so both sides
/// drop them before comparing (same filter as the publish path).
fn normalize(diags: &[RawDiag]) -> Vec<String> {
    let mut out: Vec<String> = diags
        .iter()
        .filter(|d| d.span.is_some())
        .map(|d| {
            let span = d.span.unwrap();
            format!(
                "{:?}@{}..{}: {}",
                d.severity, span.start, span.end, d.message
            )
        })
        .collect();
    out.sort();
    out
}

fn workspace_root() -> PathBuf {
    // crates/zz_lsp/tests -> workspace root.
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
}

fn zz_bin() -> PathBuf {
    // target/debug/deps/check_parity-<hash> -> target/debug/zz(.exe).
    let exe = std::env::current_exe().expect("current exe");
    let name = if cfg!(windows) { "zz.exe" } else { "zz" };
    exe.parent()
        .expect("deps dir")
        .parent()
        .expect("debug dir")
        .join(name)
}

fn fixture_files() -> Vec<PathBuf> {
    let fixtures = workspace_root().join("tests").join("fixtures");
    // The error corpus plus the parity-only corpus: files that must
    // agree across `zz check` and the server but are not error fixtures
    // (the errors/ dir mandates runtime failure via e2e discovery, so
    // warning-only and clean agreement files live in lsp_parity/).
    let mut files: Vec<PathBuf> = Vec::new();
    for dir in [fixtures.join("errors"), fixtures.join("lsp_parity")] {
        let mut dir_files: Vec<PathBuf> = std::fs::read_dir(&dir)
            .unwrap_or_else(|e| panic!("cannot list {}: {e}", dir.display()))
            .filter_map(|e| e.ok().map(|e| e.path()))
            .filter(|p| p.extension().and_then(|e| e.to_str()) == Some("zz"))
            .collect();
        files.append(&mut dir_files);
    }
    for clean in CLEAN_FILES {
        files.push(workspace_root().join(clean));
    }
    files.sort();
    files
}

/// Valid files that must check clean on both sides. Keep this list tiny
/// and hand-verified: every entry asserts zero diagnostics everywhere.
const CLEAN_FILES: &[&str] = &["tests/fixtures/syntax/declarations.zz"];

/// Check side: `zz check --format json <file>`, diagnostics attributed
/// to the entry file only (multi-file closures attribute dep errors to
/// dep files on this side; the LSP side checks the open file — entry
/// scope keeps the comparison apples-to-apples).
///
/// The loader namespaces every module by file stem (`ErrA` checks as
/// `try_no_convert.ErrA`) while the server checks the bare file, so
/// entry-stem qualification is stripped from check-side messages before
/// comparing. Only the entry stem is stripped (other modules keep
/// theirs), and only before identifier characters.
fn check_side(zz: &Path, entry: &Path) -> Vec<String> {
    let out = Command::new(zz)
        .args(["check", "--format", "json"])
        .arg(entry)
        .output()
        .expect("zz check should run");
    let stdout = String::from_utf8_lossy(&out.stdout);
    let report: CheckJson = serde_json::from_str(&stdout).unwrap_or_else(|e| {
        panic!(
            "unparseable `zz check --format json` output for {}: {e}\nstdout:\n{stdout}\nstderr:\n{}",
            entry.display(),
            String::from_utf8_lossy(&out.stderr)
        )
    });
    let entry_name = entry.file_name().and_then(|n| n.to_str()).unwrap_or("");
    let entry_stem = entry.file_stem().and_then(|n| n.to_str()).unwrap_or("");
    let mut diags = Vec::new();
    for f in &report.files {
        let name = Path::new(&f.file)
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("");
        // Entry scope (see above): only the opened file counts.
        if name == entry_name {
            diags.extend(f.diagnostics.iter().cloned());
        }
    }
    strip_entry_stem(&mut diags, entry_stem);
    normalize(&diags)
}

/// Strip `<stem>.` qualification from diagnostic messages when it names
/// the entry file's own stem (see `check_side`). Only strips before
/// identifier characters, so `foo.5` version-ish text is untouched.
fn strip_entry_stem(diags: &mut [RawDiag], stem: &str) {
    if stem.is_empty() {
        return;
    }
    let prefix = format!("{stem}.");
    for d in diags {
        let mut rebuilt = String::with_capacity(d.message.len());
        let mut rest = d.message.as_str();
        while let Some(pos) = rest.find(&prefix) {
            let after = &rest[pos + prefix.len()..];
            let follows_ident = after
                .chars()
                .next()
                .is_some_and(|c| c.is_alphabetic() || c == '_');
            rebuilt.push_str(&rest[..pos]);
            if follows_ident {
                rest = after;
            } else {
                rebuilt.push_str(&prefix);
                rest = after;
            }
        }
        rebuilt.push_str(rest);
        d.message = rebuilt;
    }
}

/// LSP side: the didOpen path in-process (`update_document` →
/// `checker_seed_for_path` → `check_program` → `absorb_result`),
/// collecting exactly what `recheck_and_publish` would send.
fn lsp_side(entry: &Path) -> Vec<String> {
    let source = std::fs::read_to_string(entry)
        .unwrap_or_else(|e| panic!("cannot read {}: {e}", entry.display()));
    let abs = entry.canonicalize().unwrap_or_else(|_| entry.to_path_buf());
    let uri =
        Url::from_file_path(&abs).unwrap_or_else(|_| panic!("non-file path {}", abs.display()));
    let state = GlobalState::new();
    state.update_document(uri.clone(), 1, source);
    // Mirror `open_and_check` (server/tests.rs): seed from this
    // document's own imports, then check and absorb. Parse failures
    // leave no program — like `recheck_and_publish`, only parse errors
    // go out in that case.
    let doc = state.documents.get(&uri).unwrap();
    let (parse_errors, program) = (doc.parse_errors.clone(), doc.program.clone());
    drop(doc);
    let mut diags = parse_errors;
    if let Some(program) = program {
        // Unknown `std` imports (same set `recheck_and_publish` sends).
        diags.extend(zz_lsp::import_seed::unknown_import_diags(&program));
        let doc_path = uri.to_file_path().ok();
        let (ib, ifunc, is, ia, ie) = state.checker_seed_for_path(&program, doc_path.as_deref());
        let cr = zz_checker::check_program(&program, ib, ifunc, is, ia, ie);
        state.absorb_result(&uri, &cr);
        diags.extend(cr.errors.clone());
    }
    normalize(&diags)
}

fn load_allowlist() -> HashMap<String, String> {
    let path = workspace_root()
        .join("tests")
        .join("fixtures")
        .join("lsp_parity_allowlist.toml");
    let Ok(content) = std::fs::read_to_string(&path) else {
        return HashMap::new();
    };
    let parsed: Allowlist =
        toml::from_str(&content).unwrap_or_else(|e| panic!("invalid {}: {e}", path.display()));
    parsed
        .allow
        .into_iter()
        .map(|e| (e.file, e.reason))
        .collect()
}

#[test]
fn check_parity_corpus() {
    let zz = zz_bin();
    assert!(
        zz.exists(),
        "zz binary missing at {} — build it first (`cargo build -p zz_cli`)",
        zz.display()
    );
    let allowed = load_allowlist();
    let mut mismatches: BTreeMap<String, (Vec<String>, Vec<String>)> = BTreeMap::new();
    let mut allowed_skips = 0usize;
    let mut fixed: Vec<String> = Vec::new();
    let mut passed = 0usize;

    for entry in fixture_files() {
        let name = entry
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("?")
            .to_string();
        let check_diags = check_side(&zz, &entry);
        let lsp_diags = lsp_side(&entry);
        if check_diags == lsp_diags {
            if let Some(reason) = allowed.get(&name) {
                fixed.push(format!("{name} (was: {reason})"));
            } else {
                passed += 1;
            }
            continue;
        }
        if allowed.contains_key(&name) {
            allowed_skips += 1;
            continue;
        }
        mismatches.insert(name, (check_diags, lsp_diags));
    }

    if !fixed.is_empty() {
        panic!(
            "FIXED! remove from lsp_parity_allowlist.toml:\n  {}",
            fixed.join("\n  ")
        );
    }
    if !mismatches.is_empty() {
        let mut report = String::new();
        for (name, (check_diags, lsp_diags)) in &mismatches {
            report.push_str(&format!("\n--- {name} ---\n  check-only:\n"));
            for d in check_diags {
                report.push_str(&format!("    - {d}\n"));
            }
            report.push_str("  lsp-only:\n");
            for d in lsp_diags {
                report.push_str(&format!("    - {d}\n"));
            }
            let check_set: std::collections::HashSet<&str> =
                check_diags.iter().map(String::as_str).collect();
            let lsp_set: std::collections::HashSet<&str> =
                lsp_diags.iter().map(String::as_str).collect();
            for d in check_diags.iter().filter(|d| !lsp_set.contains(d.as_str())) {
                report.push_str(&format!("    check-only: {d}\n"));
            }
            for d in lsp_diags.iter().filter(|d| !check_set.contains(d.as_str())) {
                report.push_str(&format!("    lsp-only: {d}\n"));
            }
        }
        panic!(
            "{} file(s) diverge between `zz check` and LSP diagnostics.\
             Fix the analysis gap or record it in tests/fixtures/lsp_parity_allowlist.toml:{report}\n\
             (allowed skips: {allowed_skips}, passed: {passed})",
            mismatches.len()
        );
    }
    eprintln!("lsp-parity: {passed} file(s) match, {allowed_skips} allowlisted skip(s)");
}
