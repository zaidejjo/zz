//! `zz fmt` parity gate — lossless, idempotent, production-ready.
//!
//! Implements the `tests/fmt_parity_test.rs` specification:
//!   1. Golden `ztasks.zz` Todo fixture preserves multiline SQL, `\x0a`
//!      escapes, `{expr}` interpolation, and all comments byte-for-byte.
//!   2. Struct definitions expand to the canonical 4-space multiline form.
//!   3. Binary / pipe / assignment / match-arrow operators keep single-space
//!      padding; imports group at the top.
//!   4. `fmt(fmt(x)) == fmt(x)` holds for the golden file, all syntax /
//!      types / stdlib fixtures, and inline property cases.

use std::path::PathBuf;

fn config() -> zz_fmt::FmtConfig {
    zz_fmt::FmtConfig::default()
}

fn fixtures_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures")
}

fn fmt_parity_fixture() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/ztasks.zz")
}

fn format_twice(src: &str) -> (String, String) {
    let cfg = config();
    let once =
        zz_fmt::format_source(src, &cfg).unwrap_or_else(|e| panic!("first format failed: {e}"));
    let twice = zz_fmt::format_source(&once, &cfg)
        .unwrap_or_else(|e| panic!("second format (idempotence) failed: {e}"));
    (once, twice)
}

fn assert_idempotent(path: &std::path::Path, src: &str, once: &str, twice: &str) {
    assert_eq!(
        once, twice,
        "{}: not idempotent (fmt(fmt(x)) != fmt(x))\n--- first ---\n{once}\n--- second ---\n{twice}",
        path.display(),
    );
    let _ = src;
}

// ── Golden ztasks fixture ────────────────────────────────────────────────

#[test]
fn ztasks_preserves_multiline_sql_verbatim() {
    let src = std::fs::read_to_string(fmt_parity_fixture()).unwrap();
    let (once, twice) = format_twice(&src);
    // Exact multiline SQL layout (indentation + newlines) survives.
    assert!(
        once.contains("CREATE TABLE IF NOT EXISTS tasks ("),
        "multiline CREATE TABLE SQL lost:\n{once}"
    );
    assert!(
        once.contains("            id INTEGER PRIMARY KEY,"),
        "SQL inner indentation changed:\n{once}"
    );
    assert!(
        once.contains("INSERT INTO tasks (name, is_done) VALUES ({name}, 0)"),
        "parameterized INSERT SQL lost:\n{once}"
    );
    assert_idempotent(&fmt_parity_fixture(), &src, &once, &twice);
}

#[test]
fn ztasks_preserves_interpolation_and_escapes() {
    let src = std::fs::read_to_string(fmt_parity_fixture()).unwrap();
    let (once, twice) = format_twice(&src);
    // `\x0a` hex escape must not be rewritten to `\n`.
    assert!(
        once.contains(r#""added\x0aline:{name}\n""#),
        "escape sequence mutated:\n{once}"
    );
    // Interpolated expressions survive with exact spelling.
    for needle in [
        "{name}",
        "{t} done? {n}",
        "SELECT id, name FROM tasks WHERE is_done = {n}",
    ] {
        assert!(
            once.contains(needle),
            "interpolation `{needle}` lost:\n{once}"
        );
    }
    assert_idempotent(&fmt_parity_fixture(), &src, &once, &twice);
}

#[test]
fn ztasks_preserves_all_comments() {
    let src = std::fs::read_to_string(fmt_parity_fixture()).unwrap();
    let (once, twice) = format_twice(&src);
    for needle in [
        "// ztasks — Todo application golden fixture",
        "// Task status filter.",
        "/* Block comment: status values are",
        "// Ensure the tasks table exists.",
        "// trailing comment: insertion uses bound params",
        "// Entry: wire everything with pipe-friendly calls.",
        "// done: {total} tasks listed",
    ] {
        assert!(
            once.contains(needle),
            "comment displaced/dropped: `{needle}`\n{once}"
        );
    }
    assert_idempotent(&fmt_parity_fixture(), &src, &once, &twice);
}

#[test]
fn ztasks_struct_expands_to_canonical_form() {
    let src = std::fs::read_to_string(fmt_parity_fixture()).unwrap();
    let (once, _) = format_twice(&src);
    assert!(
        once.contains("struct Task {\n    id: int,\n    name: str,\n    is_done: bool,\n}"),
        "struct not in canonical expanded form:\n{once}"
    );
}

#[test]
fn ztasks_imports_grouped_at_top() {
    let src = std::fs::read_to_string(fmt_parity_fixture()).unwrap();
    let (once, _) = format_twice(&src);
    let lines: Vec<&str> = once.lines().collect();
    let io_pos = lines
        .iter()
        .position(|l| l.starts_with("import std.io"))
        .unwrap();
    let sqlz_pos = lines
        .iter()
        .position(|l| l.starts_with("import std.sqlz"))
        .unwrap();
    // Both imports precede any non-import, non-comment, non-blank line.
    let first_code = lines
        .iter()
        .position(|l| {
            let t = l.trim();
            !t.is_empty()
                && !t.starts_with("//")
                && !t.starts_with("/*")
                && !t.starts_with("import ")
        })
        .unwrap();
    assert!(
        io_pos < first_code && sqlz_pos < first_code,
        "imports not hoisted:\n{once}"
    );
}

#[test]
fn imports_sorted_std_first_then_external() {
    // Source order deliberately scrambled: external first, std unsorted.
    let src = "import zimg\nimport std.vec\nimport std.io\nimport helpers.math\nx := 1\n";
    let (once, twice) = format_twice(src);
    let expected = "import std.io\nimport std.vec\n\nimport helpers.math\nimport zimg\n\nx := 1\n";
    assert_eq!(once, expected, "import sorting wrong:\n{once}");
    assert_eq!(once, twice);
}

#[test]
fn imports_sorted_single_group_has_no_blank_separator() {
    let (std_only, _) = format_twice("import std.vec\nimport std.io\nx := 1\n");
    assert_eq!(std_only, "import std.io\nimport std.vec\n\nx := 1\n");
    let (ext_only, _) = format_twice("import zimg\nimport aaa\nx := 1\n");
    assert_eq!(ext_only, "import aaa\nimport zimg\n\nx := 1\n");
}

#[test]
fn imports_sorted_keeps_attached_comments_with_import() {
    let src = "import zimg\nx := 1\n// docs for db\nimport std.db\ny := 2\n";
    let (once, twice) = format_twice(src);
    let db_pos = once.find("import std.db").unwrap();
    let comment_pos = once.find("// docs for db").unwrap();
    assert!(
        comment_pos < db_pos,
        "attached comment did not move with import:\n{once}"
    );
    assert!(
        once.contains("import zimg"),
        "external import lost:\n{once}"
    );
    assert_eq!(once, twice);
}

// ── Operator / spacing standards ─────────────────────────────────────────

#[test]
fn operators_have_single_space_padding() {
    let cases = [
        ("x:=1+2*3\n", "x := 1 + 2 * 3"),
        ("y:=a&&b||c\n", "y := a && b || c"),
        ("match x { 1=>a,_=>b }\n", "1 => a"),
        (
            "func add(a:int,b:int)->int{return a+b}\n",
            "func add(a: int, b: int) -> int",
        ),
    ];
    for (input, needle) in cases {
        let (once, twice) = format_twice(input);
        assert!(
            once.contains(needle),
            "input {input:?} lost padding:\n{once}"
        );
        assert_eq!(once, twice, "not idempotent for {input:?}");
    }
}

// ── Comment fidelity edge cases ──────────────────────────────────────────

#[test]
fn comment_spacing_preserved_byte_identical() {
    for src in [
        "//hello\nx := 1\n",
        "x := 1 // trailing\n",
        "x := 1 /*  spaced  */ + 2\n",
        "/// doc line\nx := 1\n",
    ] {
        let (once, twice) = format_twice(src);
        // The comment text (up to end of line for `//`) must appear verbatim.
        let comment = src
            .lines()
            .find(|l| l.contains("//") || l.contains("/*"))
            .unwrap();
        let needle = if let Some(idx) = comment.find("//") {
            comment[idx..].trim_end().to_string()
        } else {
            let s = comment.find("/*").unwrap();
            let e = comment.find("*/").unwrap() + 2;
            comment[s..e].to_string()
        };
        assert!(
            once.contains(&needle),
            "comment `{needle}` mutated in {src:?}:\n{once}"
        );
        assert_eq!(once, twice);
    }
}

// ── Idempotence across the whole fixture corpus ──────────────────────────

fn check_dir_idempotent(dir: &str) {
    let root = fixtures_root().join(dir);
    let entries = std::fs::read_dir(&root)
        .unwrap_or_else(|e| panic!("cannot read fixtures dir {}: {e}", root.display()));
    let mut total = 0;
    let mut failures = Vec::new();
    for entry in entries {
        let path = entry.unwrap().path();
        if path.extension().and_then(|s| s.to_str()) != Some("zz") {
            continue;
        }
        total += 1;
        let src = match std::fs::read_to_string(&path) {
            Ok(s) => s,
            Err(e) => {
                failures.push(format!("{}: read error: {e}", path.display()));
                continue;
            }
        };
        // Skip fixtures with intentional parse errors (error corpus is
        // covered by the CLI e2e suite, not the formatter).
        if !zz_frontend::parse(&src).errors.is_empty() {
            continue;
        }
        let cfg = config();
        let once = match zz_fmt::format_source(&src, &cfg) {
            Ok(s) => s,
            Err(e) => {
                failures.push(format!("{}: format error: {e}", path.display()));
                continue;
            }
        };
        match zz_fmt::format_source(&once, &cfg) {
            Ok(twice) if twice == once => {}
            Ok(twice) => failures.push(format!(
                "{}: not idempotent\n--- first ---\n{once}\n--- second ---\n{twice}",
                path.display()
            )),
            Err(e) => failures.push(format!("{}: second-pass error: {e}", path.display())),
        }
    }
    assert!(
        failures.is_empty(),
        "{}/{} {dir} fixture(s) failed:\n{}",
        failures.len(),
        total,
        failures.join("\n\n")
    );
}

#[test]
fn idempotent_syntax_corpus() {
    check_dir_idempotent("syntax");
}

#[test]
fn idempotent_types_corpus() {
    check_dir_idempotent("types");
}

#[test]
fn idempotent_stdlib_corpus() {
    check_dir_idempotent("stdlib");
}
