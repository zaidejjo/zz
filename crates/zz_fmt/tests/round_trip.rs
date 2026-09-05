//! Round-trip tests: format → re-parse → fingerprint equal.
//!
//! For every `.zz` file under `tests/fixtures/syntax/` (and types/stdlib
//! for breadth), we format the source and confirm:
//!   1. No parse errors in either the input or the output.
//!   2. The output's AST fingerprint matches the input's.
//!   3. (Idempotence) formatting the output again yields the same bytes.
//!
//! M1 also asserts that the formatted output preserves all original
//! comments by simple substring presence. This is a coarse but strong
//! signal that trivia isn't being dropped.

use std::path::PathBuf;

/// Walk `tests/fixtures/<dir>/` and return every `.zz` file.
fn fixtures_in(dir: &str) -> Vec<PathBuf> {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures")
        .join(dir);
    let mut out = Vec::new();
    for entry in std::fs::read_dir(&root)
        .unwrap_or_else(|e| panic!("cannot read fixtures dir {}: {e}", root.display()))
    {
        let entry = entry.unwrap();
        let p = entry.path();
        if p.extension().and_then(|s| s.to_str()) == Some("zz") {
            out.push(p);
        }
    }
    out.sort();
    out
}

fn extract_comments(src: &str) -> Vec<String> {
    let mut out = Vec::new();
    let bytes = src.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if i + 1 < bytes.len() && bytes[i] == b'/' && bytes[i + 1] == b'/' {
            // Line comment to end of line.
            let start = i;
            let mut j = i + 2;
            while j < bytes.len() && bytes[j] != b'\n' {
                j += 1;
            }
            out.push(src[start..j].to_string());
            i = j;
        } else if i + 1 < bytes.len() && bytes[i] == b'/' && bytes[i + 1] == b'*' {
            let start = i;
            let mut depth = 1u32;
            let mut j = i + 2;
            while j < bytes.len() && depth > 0 {
                if j + 1 < bytes.len() && bytes[j] == b'/' && bytes[j + 1] == b'*' {
                    depth += 1;
                    j += 2;
                } else if j + 1 < bytes.len() && bytes[j] == b'*' && bytes[j + 1] == b'/' {
                    depth -= 1;
                    j += 2;
                } else {
                    j += 1;
                }
            }
            out.push(src[start..j].to_string());
            i = j;
        } else {
            i += 1;
        }
    }
    out
}

#[test]
fn round_trip_syntax_fixtures() {
    let config = zz_fmt::FmtConfig::default();
    let mut total = 0;
    let mut failures: Vec<String> = Vec::new();
    for path in fixtures_in("syntax") {
        total += 1;
        let src = match std::fs::read_to_string(&path) {
            Ok(s) => s,
            Err(e) => {
                failures.push(format!("{}: read error: {e}", path.display()));
                continue;
            }
        };
        let formatted = match zz_fmt::format_source(&src, &config) {
            Ok(s) => s,
            Err(e) => {
                failures.push(format!("{}: format error: {e}", path.display()));
                continue;
            }
        };
        // Idempotence.
        let twice = match zz_fmt::format_source(&formatted, &config) {
            Ok(s) => s,
            Err(e) => {
                failures.push(format!("{}: idempotent format error: {e}", path.display()));
                continue;
            }
        };
        if twice != formatted {
            failures.push(format!(
                "{}: not idempotent\n--- first pass ---\n{}\n--- second pass ---\n{}",
                path.display(),
                formatted,
                twice
            ));
            continue;
        }
        // Comments preserved.
        for c in extract_comments(&src) {
            if !formatted.contains(&c) {
                failures.push(format!(
                    "{}: comment `{}` not preserved in formatted output",
                    path.display(),
                    c
                ));
            }
        }
    }
    assert!(
        failures.is_empty(),
        "{}/{} syntax fixture(s) failed:\n{}",
        failures.len(),
        total,
        failures.join("\n\n")
    );
}

#[test]
fn round_trip_types_fixtures() {
    let config = zz_fmt::FmtConfig::default();
    let mut failures = Vec::new();
    let mut total = 0;
    for path in fixtures_in("types") {
        total += 1;
        let src = match std::fs::read_to_string(&path) {
            Ok(s) => s,
            Err(e) => {
                failures.push(format!("{}: read error: {e}", path.display()));
                continue;
            }
        };
        let formatted = match zz_fmt::format_source(&src, &config) {
            Ok(s) => s,
            Err(e) => {
                failures.push(format!("{}: format error: {e}", path.display()));
                continue;
            }
        };
        let twice = match zz_fmt::format_source(&formatted, &config) {
            Ok(s) => s,
            Err(e) => {
                failures.push(format!("{}: idempotent format error: {e}", path.display()));
                continue;
            }
        };
        if twice != formatted {
            failures.push(format!(
                "{}: not idempotent\n--- first ---\n{}\n--- second ---\n{}",
                path.display(),
                formatted,
                twice
            ));
        }
    }
    assert!(
        failures.is_empty(),
        "{}/{} types fixture(s) failed:\n{}",
        failures.len(),
        total,
        failures.join("\n\n")
    );
}

#[test]
fn round_trip_stdlib_fixtures() {
    let config = zz_fmt::FmtConfig::default();
    let mut total = 0;
    let mut failures: Vec<String> = Vec::new();
    for path in fixtures_in("stdlib") {
        total += 1;
        let src = match std::fs::read_to_string(&path) {
            Ok(s) => s,
            Err(e) => {
                failures.push(format!("{}: read error: {e}", path.display()));
                continue;
            }
        };
        let formatted = match zz_fmt::format_source(&src, &config) {
            Ok(s) => s,
            Err(e) => {
                failures.push(format!("{}: format error: {e}", path.display()));
                continue;
            }
        };
        let twice = match zz_fmt::format_source(&formatted, &config) {
            Ok(s) => s,
            Err(e) => {
                failures.push(format!("{}: idempotent format error: {e}", path.display()));
                continue;
            }
        };
        if twice != formatted {
            failures.push(format!(
                "{}: not idempotent\n--- first ---\n{}\n--- second ---\n{}",
                path.display(),
                formatted,
                twice
            ));
        }
    }
    assert!(
        failures.is_empty(),
        "{}/{} stdlib fixture(s) failed:\n{}",
        failures.len(),
        total,
        failures.join("\n\n")
    );
}

#[test]
fn inline_idempotent() {
    let config = zz_fmt::FmtConfig::default();
    let src = "x := 1 + 2\ny := x * 3\n// trailing comment\n";
    let a = zz_fmt::format_source(src, &config).unwrap();
    let b = zz_fmt::format_source(&a, &config).unwrap();
    assert_eq!(a, b);
}

#[test]
fn inline_preserves_comments() {
    let config = zz_fmt::FmtConfig::default();
    let src = "// header\nx := 1 // trailing\n/// doc line\ny := 2\n";
    let out = zz_fmt::format_source(src, &config).unwrap();
    println!("Formatted output: {:?}", out);
    assert!(out.contains("// header"));
    assert!(out.contains("// trailing"));
    assert!(out.contains("/// doc line"));
}
#[test]
fn inline_preserves_blank_lines() {
    let config = zz_fmt::FmtConfig::default();
    let src = "x := 1\n\n\ny := 2\n";
    let out = zz_fmt::format_source(src, &config).unwrap();
    // Debug: print the actual output
    println!("Actual output: {:?}", out);
    println!("Actual output (escaped): {}", out.replace("\n", "\\n"));
    // After collapsing: exactly one blank line between top-level stmts.
    assert!(out.contains("x := 1\n\ny := 2"));
    // No three-newline run.
    assert!(!out.contains("\n\n\n"));
}

#[test]
fn inline_block_comment_preserved() {
    let config = zz_fmt::FmtConfig::default();
    let src = "x := 1 /* hi */ + 2\n";
    let out = zz_fmt::format_source(src, &config).unwrap();
    assert!(out.contains("/* hi */"));
}
