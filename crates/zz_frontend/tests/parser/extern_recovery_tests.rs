//! `extern "C"` block grammar and error-recovery regression tests.
//!
//! History: an `extern "C" { fn ... }` block used to hang the parser.
//! `skip_to_stmt_end` stops on *any* closing bracket, so after the first
//! "expected `func` signature" error the cursor parked on `)` and the
//! recovery loop re-reported it forever — one diagnostic per iteration
//! until the process OOM'd. These tests pin termination, bounded error
//! counts, and the `func`-only grammar decision.

use std::time::{Duration, Instant};

use zz_frontend::ast::{Stmt, TyKind};
use zz_frontend::parse;

fn parse_timed(src: &str) -> (zz_frontend::Parsed, Duration) {
    let started = Instant::now();
    let parsed = parse(src);
    (parsed, started.elapsed())
}

#[test]
fn extern_fn_keyword_is_rejected_with_a_bounded_error() {
    // `fn` is Rust spelling. ZZ uses `func`; there is deliberately no alias,
    // because accepting both would leave two ways to spell one signature.
    let src = "extern \"C\" {\n    fn ping() -> int\n}\n";
    let (parsed, elapsed) = parse_timed(src);

    assert!(
        elapsed < Duration::from_millis(100),
        "parse took {elapsed:?} — recovery is looping again"
    );
    assert!(
        parsed.errors.len() == 1,
        "expected exactly one diagnostic, got {} errors: {:?}",
        parsed.errors.len(),
        parsed
            .errors
            .iter()
            .map(|e| e.message.as_str())
            .collect::<Vec<_>>()
    );
    let joined = parsed
        .errors
        .iter()
        .map(|e| e.message.as_str())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(
        joined.contains("`func`") && joined.contains("`fn`"),
        "diagnostic should name the supported spelling, got: {joined}"
    );
    // No alias: the `fn` item must not become an extern signature.
    match parsed.program.stmts.first() {
        Some(Stmt::ExternBlock { items, .. }) => assert!(items.is_empty(), "got {items:?}"),
        other => panic!("expected an ExternBlock, got {other:?}"),
    }
}

#[test]
fn extern_func_signature_parses_clean() {
    let src = "extern \"C\" {\n    func ping() -> int\n}\n";
    let (parsed, elapsed) = parse_timed(src);

    assert!(
        elapsed < Duration::from_millis(100),
        "parse took {elapsed:?}"
    );
    assert!(
        parsed.errors.is_empty(),
        "expected clean parse, got: {:?}",
        parsed
            .errors
            .iter()
            .map(|e| e.message.as_str())
            .collect::<Vec<_>>()
    );
    match parsed.program.stmts.first() {
        Some(Stmt::ExternBlock { abi, items, .. }) => {
            assert_eq!(abi, "C");
            assert_eq!(items.len(), 1);
            assert_eq!(items[0].name.name, "ping");
            assert!(items[0].params.is_empty());
            assert!(matches!(
                items[0].ret.as_ref().map(|t| &t.kind),
                Some(TyKind::Int)
            ));
        }
        other => panic!("expected an ExternBlock, got {other:?}"),
    }
}

#[test]
fn malformed_extern_blocks_terminate_with_bounded_errors() {
    let cases: &[(&str, &str)] = &[
        (
            "missing closing brace",
            "extern \"C\" {\n    func ping() -> int\n",
        ),
        ("junk tokens", "extern \"C\" {\n    !!! @@@ ###\n}\n"),
        (
            "rust fn keyword",
            "extern \"C\" {\n    fn ping() -> int\n}\n",
        ),
        (
            "nested braces",
            "extern \"C\" {\n    func ping() -> int {\n        func noop()\n    }\n}\n",
        ),
        ("stray close paren", "extern \"C\" {\n    )\n}\n"),
        ("many stray close parens", "extern \"C\" {\n    ))))))\n}\n"),
        ("stray close bracket", "extern \"C\" {\n    ] ] ]\n}\n"),
        ("unterminated signature", "extern \"C\" {\n    func \n"),
        ("no body at all", "extern \"C\""),
        ("junk before brace", "extern foo\n    func ping() -> int\n"),
    ];

    for (name, src) in cases {
        let (parsed, elapsed) = parse_timed(src);
        assert!(
            elapsed < Duration::from_secs(1),
            "case `{name}` took {elapsed:?} — recovery is looping"
        );
        assert!(
            parsed.errors.len() < 1000,
            "case `{name}` produced {} errors (cap should bound this)",
            parsed.errors.len()
        );
    }
}

#[test]
fn diagnostic_cap_aborts_instead_of_allocating_forever() {
    // More stray closers than the cap: recovery reports each one, so the
    // parse must stop at the cap and park on Eof — never grow the list to
    // tens of thousands.
    let src = format!("extern \"C\" {{\n{}\n}}\n", ")".repeat(5000));
    let (parsed, elapsed) = parse_timed(&src);

    assert!(elapsed < Duration::from_secs(1), "parse took {elapsed:?}");
    assert!(
        parsed.errors.len() <= 1002,
        "diagnostic cap did not bound the list: {} errors",
        parsed.errors.len()
    );
    assert!(
        parsed
            .errors
            .iter()
            .any(|e| e.message.contains("too many errors")),
        "expected a cap diagnostic, got: {:?}",
        parsed.errors.last().map(|e| e.message.as_str())
    );
}
