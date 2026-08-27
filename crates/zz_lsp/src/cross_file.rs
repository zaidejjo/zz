//! Cross-file support: module resolution, multi-file symbol indexing,
//! and workspace scanning.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use tower_lsp::lsp_types::Url;
use zz_frontend::ast::Program;

use crate::lookup::Definition;

// ── Module index ─────────────────────────────────────────────────────────

/// Per-file metadata stored in the cross-file index.
#[derive(Debug, Clone)]
pub struct FileEntry {
    pub uri: Url,
    pub module_path: String, // e.g. "foo.bar"
    pub source: String,
    pub program: Option<Program>,
    pub definitions: Vec<Definition>,
}

/// Index mapping module paths ↔ file URIs, plus per-file definitions.
#[derive(Debug, Default)]
pub struct ModuleIndex {
    /// Module path → file URI (e.g. "foo.bar" → file:///.../foo/bar.zz).
    pub module_to_uri: HashMap<String, Url>,
    /// File URI → module path (reverse).
    pub uri_to_module: HashMap<Url, String>,
    /// File URI → per-file entry.
    pub entries: HashMap<Url, FileEntry>,
}

impl ModuleIndex {
    /// Resolve an import path (e.g. `["foo", "bar"]`) to a file URI.
    ///
    /// First checks the in-memory module index (populated by workspace scan
    /// or test setup), then falls back to filesystem conventions:
    /// `<root>/foo/bar.zz` or `<root>/foo/bar/mod.zz`.
    pub fn resolve_import(&self, path: &[String], workspace_root: &Path) -> Option<Url> {
        let module_path = path.join(".");

        // 1. Check in-memory index first (handles indexed workspace files).
        if let Some(uri) = self.module_to_uri.get(&module_path) {
            return Some(uri.clone());
        }

        // 2. Try filesystem: <root>/foo/bar.zz
        let relative: PathBuf = path.iter().collect();
        let candidate = workspace_root.join(&relative).with_extension("zz");
        if candidate.exists() {
            return Url::from_file_path(&candidate).ok();
        }
        // 3. Try <root>/foo/bar/mod.zz
        let candidate = workspace_root.join(&relative).join("mod.zz");
        if candidate.exists() {
            return Url::from_file_path(&candidate).ok();
        }
        None
    }

    /// Get the definition list for a file, if indexed.
    pub fn definitions_for(&self, uri: &Url) -> Vec<Definition> {
        self.entries
            .get(uri)
            .map(|e| e.definitions.clone())
            .unwrap_or_default()
    }

    /// Find a definition by name across all indexed files.
    /// Returns (uri, definition) for the first match.
    pub fn find_definition_across_files(&self, name: &str) -> Option<(Url, Definition)> {
        for entry in self.entries.values() {
            for def in &entry.definitions {
                if def.name == name {
                    return Some((entry.uri.clone(), def.clone()));
                }
            }
        }
        None
    }

    /// Find all references to a name across all indexed files.
    pub fn find_references_across_files(&self, name: &str) -> Vec<(Url, Definition)> {
        let mut results = Vec::new();
        for entry in self.entries.values() {
            for def in &entry.definitions {
                if def.name == name {
                    results.push((entry.uri.clone(), def.clone()));
                }
            }
        }
        results
    }
}

// ── Workspace scanning ───────────────────────────────────────────────────

/// Derive the module path for a file from its path relative to the workspace root.
///
/// E.g., `<root>/utils/math.zz` → `"utils.math"`.
pub fn module_path_for_file(file_path: &Path, workspace_root: &Path) -> Option<String> {
    let rel = file_path.strip_prefix(workspace_root).ok()?;
    let mut parts: Vec<String> = rel
        .components()
        .map(|c| c.as_os_str().to_string_lossy().into_owned())
        .collect();
    // Remove .zz extension from last component.
    #[allow(clippy::question_mark)]
    if let Some(last) = parts.last_mut() {
        if let Some(stem) = last.strip_suffix(".zz") {
            *last = stem.to_string();
        } else {
            return None; // Not a .zz file.
        }
    }
    // Remove "mod" if the file is mod.zz inside a directory.
    if parts.last().map(String::as_str) == Some("mod") {
        parts.pop();
    }
    if parts.is_empty() {
        return None;
    }
    Some(parts.join("."))
}

/// Scan a directory recursively for `.zz` files.
pub fn scan_for_zz_files(dir: &Path) -> Vec<PathBuf> {
    let mut results = Vec::new();
    if !dir.is_dir() {
        return results;
    }
    if let Ok(entries) = std::fs::read_dir(dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                // Skip hidden directories and target/.
                let name = path
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_default();
                if name.starts_with('.') || name == "target" {
                    continue;
                }
                results.extend(scan_for_zz_files(&path));
            } else if path.extension().map(|e| e == "zz").unwrap_or(false) {
                results.push(path);
            }
        }
    }
    results
}

/// Build a `FileEntry` by parsing a single file.
pub fn parse_file_entry(uri: Url, source: String, module_path: String) -> FileEntry {
    let parsed = zz_frontend::parse(&source);
    let definitions = crate::lookup::collect_definitions(&parsed.program, &source);
    let defs: Vec<Definition> = definitions.into_values().collect();
    FileEntry {
        uri,
        module_path,
        source,
        program: Some(parsed.program),
        definitions: defs,
    }
}

// ── Tests ────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use zz_frontend::span::Span;

    #[test]
    fn module_path_simple() {
        let root = PathBuf::from("/workspace");
        let file = PathBuf::from("/workspace/utils/math.zz");
        assert_eq!(
            module_path_for_file(&file, &root),
            Some("utils.math".to_string())
        );
    }

    #[test]
    fn module_path_mod_file() {
        let root = PathBuf::from("/workspace");
        let file = PathBuf::from("/workspace/utils/mod.zz");
        assert_eq!(
            module_path_for_file(&file, &root),
            Some("utils".to_string())
        );
    }

    #[test]
    fn module_path_root_file() {
        let root = PathBuf::from("/workspace");
        let file = PathBuf::from("/workspace/main.zz");
        assert_eq!(module_path_for_file(&file, &root), Some("main".to_string()));
    }

    #[test]
    fn module_path_not_zz() {
        let root = PathBuf::from("/workspace");
        let file = PathBuf::from("/workspace/main.rs");
        assert_eq!(module_path_for_file(&file, &root), None);
    }

    #[test]
    fn module_path_outside_root() {
        let root = PathBuf::from("/workspace");
        let file = PathBuf::from("/other/math.zz");
        assert_eq!(module_path_for_file(&file, &root), None);
    }

    #[test]
    fn resolve_import_basic() {
        let root = PathBuf::from("/tmp/test_ws");
        // Create a temporary workspace with a .zz file.
        std::fs::create_dir_all(root.join("math")).unwrap();
        std::fs::write(
            root.join("math/funcs.zz"),
            "func add(a: int, b: int) -> int { return a + b }\n",
        )
        .unwrap();

        let mut index = ModuleIndex::default();
        let uri = Url::from_file_path(root.join("math/funcs.zz")).unwrap();
        index
            .module_to_uri
            .insert("math.funcs".to_string(), uri.clone());

        let resolved = index.resolve_import(&["math".to_string(), "funcs".to_string()], &root);
        assert_eq!(resolved, Some(uri));

        // Cleanup.
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn resolve_import_mod_file() {
        let root = PathBuf::from("/tmp/test_ws_mod");
        std::fs::create_dir_all(root.join("math")).unwrap();
        std::fs::write(
            root.join("math/mod.zz"),
            "func add(a: int, b: int) -> int { return a + b }\n",
        )
        .unwrap();

        let resolved = ModuleIndex::default().resolve_import(&["math".to_string()], &root);
        assert!(resolved.is_some());

        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn resolve_import_not_found() {
        let root = PathBuf::from("/tmp/test_ws_missing");
        let resolved = ModuleIndex::default().resolve_import(&["nonexistent".to_string()], &root);
        assert!(resolved.is_none());
    }

    #[test]
    fn find_definition_across_files() {
        let mut index = ModuleIndex::default();
        let uri_a: Url = "file:///a.zz".parse().unwrap();
        let uri_b: Url = "file:///b.zz".parse().unwrap();

        index.entries.insert(
            uri_a.clone(),
            FileEntry {
                uri: uri_a.clone(),
                module_path: "a".to_string(),
                source: String::new(),
                program: None,
                definitions: vec![Definition {
                    name: "foo".to_string(),
                    span: Span::new(0, 3),
                    kind: crate::lookup::DefKind::Func,
                }],
            },
        );
        index.entries.insert(
            uri_b.clone(),
            FileEntry {
                uri: uri_b.clone(),
                module_path: "b".to_string(),
                source: String::new(),
                program: None,
                definitions: vec![Definition {
                    name: "bar".to_string(),
                    span: Span::new(0, 3),
                    kind: crate::lookup::DefKind::Func,
                }],
            },
        );

        let result = index.find_definition_across_files("foo");
        assert!(result.is_some());
        let (uri, def) = result.unwrap();
        assert_eq!(uri, uri_a);
        assert_eq!(def.name, "foo");

        let result = index.find_definition_across_files("nonexistent");
        assert!(result.is_none());
    }

    #[test]
    fn find_references_across_files() {
        let mut index = ModuleIndex::default();
        let uri_a: Url = "file:///a.zz".parse().unwrap();
        let uri_b: Url = "file:///b.zz".parse().unwrap();

        index.entries.insert(
            uri_a.clone(),
            FileEntry {
                uri: uri_a.clone(),
                module_path: "a".to_string(),
                source: String::new(),
                program: None,
                definitions: vec![
                    Definition {
                        name: "x".to_string(),
                        span: Span::new(0, 1),
                        kind: crate::lookup::DefKind::Var,
                    },
                    Definition {
                        name: "y".to_string(),
                        span: Span::new(5, 6),
                        kind: crate::lookup::DefKind::Var,
                    },
                ],
            },
        );
        index.entries.insert(
            uri_b.clone(),
            FileEntry {
                uri: uri_b.clone(),
                module_path: "b".to_string(),
                source: String::new(),
                program: None,
                definitions: vec![Definition {
                    name: "x".to_string(),
                    span: Span::new(0, 1),
                    kind: crate::lookup::DefKind::Var,
                }],
            },
        );

        let refs = index.find_references_across_files("x");
        assert_eq!(refs.len(), 2);

        let refs = index.find_references_across_files("y");
        assert_eq!(refs.len(), 1);
    }

    // ── Performance tests ──────────────────────────────────────────────

    #[test]
    fn parse_file_entry_performance() {
        // Verify that parsing a 1000-line file is fast.
        let mut source = String::new();
        for i in 0..1000 {
            source.push_str(&format!("x_{i} := {i}\n"));
        }
        let uri: Url = "file:///perf.zz".parse().unwrap();
        let start = std::time::Instant::now();
        let entry = parse_file_entry(uri, source, "perf".to_string());
        let elapsed = start.elapsed();
        // Should parse in under 50ms.
        assert!(
            elapsed.as_millis() < 50,
            "parsing 1000-line file took {:?}",
            elapsed
        );
        assert!(!entry.definitions.is_empty());
    }

    #[test]
    fn collect_definitions_many_symbols() {
        // Verify that collecting definitions from many symbols is fast.
        let mut source = String::new();
        for i in 0..500 {
            source.push_str(&format!("val_{i} := {i}\n"));
        }
        let parsed = zz_frontend::parse(&source);
        let start = std::time::Instant::now();
        let defs = crate::lookup::collect_definitions(&parsed.program, &source);
        let elapsed = start.elapsed();
        assert!(
            elapsed.as_millis() < 2000,
            "collecting 500 definitions took {:?}",
            elapsed
        );
        assert_eq!(defs.len(), 500);
    }

    #[test]
    fn module_index_many_files() {
        // Verify that indexing many files is fast.
        let mut index = ModuleIndex::default();
        let start = std::time::Instant::now();
        for i in 0..100 {
            let uri: Url = format!("file:///file_{i}.zz").parse().unwrap();
            let source = format!("func fn_{i}() -> int {{ return {i} }}\n");
            let entry = parse_file_entry(uri.clone(), source, format!("file_{i}"));
            index.module_to_uri.insert(format!("file_{i}"), uri.clone());
            index.uri_to_module.insert(uri.clone(), format!("file_{i}"));
            index.entries.insert(uri, entry);
        }
        let elapsed = start.elapsed();
        assert!(
            elapsed.as_millis() < 500,
            "indexing 100 files took {:?}",
            elapsed
        );
        assert_eq!(index.entries.len(), 100);
    }

    #[test]
    fn scan_for_zz_files_performance() {
        // Create a temp directory with many .zz files.
        let root = PathBuf::from("/tmp/perf_ws");
        std::fs::create_dir_all(&root).unwrap();
        for i in 0..50 {
            std::fs::write(root.join(format!("file_{i}.zz")), "x := 1\n").unwrap();
        }
        let start = std::time::Instant::now();
        let files = scan_for_zz_files(&root);
        let elapsed = start.elapsed();
        assert_eq!(files.len(), 50);
        assert!(
            elapsed.as_millis() < 50,
            "scanning 50 files took {:?}",
            elapsed
        );
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn cross_file_references_large_scale() {
        // Verify cross-file reference finding scales well.
        let mut index = ModuleIndex::default();
        let mut expected_refs = 0;
        for i in 0..50 {
            let uri: Url = format!("file:///f{i}.zz").parse().unwrap();
            let source = format!("shared := {i}\nlocal_{i} := shared\n");
            let entry = parse_file_entry(uri.clone(), source, format!("f{i}"));
            index.module_to_uri.insert(format!("f{i}"), uri.clone());
            index.uri_to_module.insert(uri.clone(), format!("f{i}"));
            index.entries.insert(uri, entry);
            expected_refs += 1; // definition of shared
        }
        let start = std::time::Instant::now();
        let refs = index.find_references_across_files("shared");
        let elapsed = start.elapsed();
        assert_eq!(refs.len(), expected_refs);
        assert!(
            elapsed.as_millis() < 10,
            "finding refs across 50 files took {:?}",
            elapsed
        );
    }
}
