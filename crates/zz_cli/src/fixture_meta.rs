//! Fixture feature tags (M0).
//!
//! Every `.zz` fixture under `tests/fixtures/` declares its language
//! features in a leading `// features: ...` header (see
//! `tests/fixtures/FEATURES.md`). This module parses those headers,
//! owns the closed tag vocabulary, and defines the milestone feature
//! subsets (starting with [`M2_FEATURES`]) so later milestones gate on
//! computed fixture sets instead of prose descriptions.

use std::path::Path;

/// Closed tag vocabulary. Kept sorted; the unit test enforces it so
/// additions stay merge-clean. Every tag is kebab-case.
pub const VOCABULARY: &[&str] = &[
    "aliases",
    "arrays",
    "assert",
    "bitwise",
    "bool-logic",
    "break-continue",
    "bytes",
    "casts",
    "cli-args",
    "closures",
    "comparison",
    "const",
    "decorators",
    "defer",
    "destructuring",
    "dicts",
    "elvis",
    "embed",
    "enums",
    "error-case",
    "float-arith",
    "for-loop",
    "fstrings",
    "func-call",
    "generics",
    "hof",
    "if-else",
    "if-let",
    "import-alias",
    "imports",
    "indexing",
    "int-arith",
    "known-divergence",
    "locals",
    "match",
    "methods",
    "nondeterministic",
    "option-result",
    "pipe",
    "print",
    "pub-visibility",
    "ranges",
    "recursion",
    "return",
    "selective-import",
    "short-circuit",
    "slicing",
    "std-args",
    "std-bytes",
    "std-chan",
    "std-colors",
    "std-crypto",
    "std-csv",
    "std-db",
    "std-dec",
    "std-encoding",
    "std-env",
    "std-fs",
    "std-http",
    "std-json",
    "std-log",
    "std-map",
    "std-math",
    "std-net",
    "std-path",
    "std-process",
    "std-regexp",
    "std-sqlz",
    "std-str",
    "std-sys",
    "std-task",
    "std-term",
    "std-time",
    "std-uuid",
    "std-vec",
    "stdin",
    "string-blocks",
    "string-literal",
    "string-ops",
    "struct-methods",
    "structs",
    "try-question",
    "tuples",
    "type-inference",
    "vec-push",
    "vfs",
    "vm-only",
    "while-loop",
    "wildcard-import",
    "zz-test",
];

/// M2 lowering subset: ints, floats, bools, locals, if/while, calls,
/// print (comparisons ride along; `for-loop` lowers in M6 with ranges).
/// String *literals* are included: `print("marker")` needs only a const
/// pool entry, not string semantics (`string-ops`, `fstrings`,
/// `string-blocks` stay out). A fixture is an M2 candidate when every
/// tag is in this set.
pub const M2_FEATURES: &[&str] = &[
    "bool-logic",
    "comparison",
    "const",
    "float-arith",
    "func-call",
    "if-else",
    "int-arith",
    "locals",
    "print",
    "string-literal",
    "while-loop",
];

/// True when `tag` is in the closed vocabulary.
pub fn is_valid_tag(tag: &str) -> bool {
    VOCABULARY.contains(&tag)
}

/// Parse the `// features:` header(s) out of ZZ source.
///
/// Only the leading comment block is scanned (stops at the first
/// non-comment, non-blank line). Multiple `// features:` lines union.
/// Returns the sorted, de-duplicated tag list (may be empty).
pub fn parse_features(src: &str) -> Vec<String> {
    let mut tags: Vec<String> = Vec::new();
    for line in src.lines() {
        let t = line.trim();
        if t.is_empty() {
            continue;
        }
        let Some(rest) = t.strip_prefix("//") else {
            break;
        };
        let rest = rest.trim();
        let Some(list) = rest.strip_prefix("features:") else {
            continue;
        };
        for tag in list.split(',') {
            let tag = tag.trim().to_string();
            if !tag.is_empty() && !tags.contains(&tag) {
                tags.push(tag);
            }
        }
    }
    tags.sort();
    tags
}

/// Read `path` and parse its feature header. `Err` on IO failure or
/// when the file declares no tags.
pub fn fixture_features(path: &Path) -> Result<Vec<String>, String> {
    let src = std::fs::read_to_string(path)
        .map_err(|e| format!("cannot read {}: {e}", path.display()))?;
    let tags = parse_features(&src);
    if tags.is_empty() {
        return Err(format!(
            "{} declares no `// features:` tags",
            path.display()
        ));
    }
    Ok(tags)
}

/// True when every feature in `features` is a member of `subset`.
pub fn covers_subset(features: &[String], subset: &[&str]) -> bool {
    features.iter().all(|f| subset.contains(&f.as_str()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn vocabulary_is_sorted_unique_and_kebab() {
        let mut sorted = VOCABULARY.to_vec();
        sorted.sort_unstable();
        assert_eq!(sorted, VOCABULARY, "VOCABULARY must stay sorted");
        let mut seen = std::collections::HashSet::new();
        for tag in VOCABULARY {
            assert!(seen.insert(tag), "duplicate tag: {tag}");
            assert!(
                tag.bytes()
                    .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-'),
                "tag must be kebab-case: {tag}"
            );
        }
    }

    #[test]
    fn milestone_subsets_use_valid_tags() {
        for tag in M2_FEATURES {
            assert!(is_valid_tag(tag), "M2 tag not in vocabulary: {tag}");
        }
    }

    #[test]
    fn parse_single_and_multi_line_headers() {
        assert_eq!(parse_features("// features: print\n"), vec!["print"]);
        assert_eq!(
            parse_features("// E2E: demo\n// features: int-arith, print\nprintln(1)\n"),
            vec!["int-arith", "print"]
        );
        // Union across lines, sorted + de-duplicated.
        assert_eq!(
            parse_features("// features: print, locals\n// features: locals, int-arith\nx := 1\n"),
            vec!["int-arith", "locals", "print"]
        );
        // Stops at code: a later `// features:` is not a header.
        assert_eq!(
            parse_features("x := 1\n// features: print\n"),
            Vec::<String>::new()
        );
    }

    #[test]
    fn subset_gating() {
        let m2 = vec!["int-arith".to_string(), "print".to_string()];
        assert!(covers_subset(&m2, M2_FEATURES));
        let wider = vec!["int-arith".to_string(), "structs".to_string()];
        assert!(!covers_subset(&wider, M2_FEATURES));
    }

    /// Every `.zz` fixture in the workspace must declare at least one
    /// valid vocabulary tag. Walks the real `tests/fixtures/` tree so a
    /// new untagged (or mistyped-tag) fixture fails fast.
    #[test]
    fn all_fixtures_declare_valid_tags() {
        let root =
            std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures");
        let mut files: Vec<std::path::PathBuf> = Vec::new();
        let mut dirs = vec![root];
        while let Some(dir) = dirs.pop() {
            let entries =
                std::fs::read_dir(&dir).unwrap_or_else(|e| panic!("cannot read dir {dir:?}: {e}"));
            for entry in entries.flatten() {
                let path = entry.path();
                if path.is_dir() {
                    dirs.push(path);
                } else if path.extension().and_then(|s| s.to_str()) == Some("zz") {
                    files.push(path);
                }
            }
        }
        assert!(
            files.len() >= 260,
            "expected the full fixture tree, found {} files",
            files.len()
        );
        let mut problems = Vec::new();
        files.sort();
        for path in &files {
            match fixture_features(path) {
                Ok(tags) => {
                    for tag in &tags {
                        if !is_valid_tag(tag) {
                            problems.push(format!("{}: unknown tag `{tag}`", path.display()));
                        }
                    }
                }
                Err(e) => problems.push(e),
            }
        }
        assert!(
            problems.is_empty(),
            "{} fixture tag problem(s):\n{}",
            problems.len(),
            problems.join("\n")
        );
    }
}
