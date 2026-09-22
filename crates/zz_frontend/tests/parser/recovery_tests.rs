//! Parser error recovery tests.

use zz_frontend::ast::Expr as E;
use zz_frontend::tests::common::parse_ok;

#[test]
fn struct_init_with_space() {
    let p = parse_ok("p := Point { x: 1, y: 2 }");
    match &p.stmts[0] {
        zz_frontend::ast::Stmt::Decl {
            value: E::StructInit { name, fields, .. },
            ..
        } => {
            assert_eq!(name, "Point");
            assert_eq!(fields.len(), 2);
        }
        other => panic!("expected StructInit, got {other:?}"),
    }
}

#[test]
fn struct_init_no_space_still_works() {
    let p = parse_ok("p := Point{ x: 1 }");
    match &p.stmts[0] {
        zz_frontend::ast::Stmt::Decl {
            value: E::StructInit { name, .. },
            ..
        } => assert_eq!(name, "Point"),
        other => panic!("expected StructInit, got {other:?}"),
    }
}

#[test]
fn if_block_not_struct_init() {
    // `if x { ... }` must stay a block, not become `x{...}`.
    let p = parse_ok("if x { y := 1 }");
    match &p.stmts[0] {
        zz_frontend::ast::Stmt::Expr(E::If { cond, then, .. }) => {
            assert!(matches!(cond.as_ref(), E::Ident { name, .. } if name == "x"));
            assert_eq!(then.stmts.len(), 1);
        }
        other => panic!("expected If, got {other:?}"),
    }
}

#[test]
fn while_block_not_struct_init() {
    let p = parse_ok("while x { y := 1 }");
    match &p.stmts[0] {
        zz_frontend::ast::Stmt::Expr(E::While { cond, .. }) => {
            assert!(matches!(cond.as_ref(), E::Ident { name, .. } if name == "x"));
        }
        other => panic!("expected While, got {other:?}"),
    }
}

#[test]
fn if_let_value_block_not_struct_init() {
    // `if let .some(v) = x { ... }` — the `{` after the value is the
    // then-block, not a struct literal.
    let p = parse_ok("if let .some(v) = x { v }");
    match &p.stmts[0] {
        zz_frontend::ast::Stmt::Expr(E::IfLet { value, then, .. }) => {
            assert!(matches!(value.as_ref(), E::Ident { name, .. } if name == "x"));
            assert_eq!(then.stmts.len(), 1);
        }
        other => panic!("expected IfLet, got {other:?}"),
    }
}

#[test]
fn struct_init_in_if_cond() {
    // Struct literal directly in a condition still parses.
    let p = parse_ok("if Point { x: 1 } == p { y := 1 }");
    match &p.stmts[0] {
        zz_frontend::ast::Stmt::Expr(E::If { cond, .. }) => {
            assert!(matches!(cond.as_ref(), E::Binary { .. }));
        }
        other => panic!("expected If, got {other:?}"),
    }
}

#[test]
fn malformed_struct_recovers_without_hanging() {
    // A dotted struct name is now VALID (cross-module struct).
    // The parser should accept it and not spin.
    let parsed = zz_frontend::parse("struct shapes.Point { x: int }\nz := 1");
    assert!(
        parsed.errors.is_empty(),
        "dotted struct name should be valid: {:?}",
        parsed.errors
    );
    assert_eq!(parsed.program.stmts.len(), 2);
}

#[test]
fn malformed_struct_init_recovers_without_hanging() {
    // Garbage inside a struct literal must not spin the parser.
    let parsed = zz_frontend::parse("p := Point{ 123 }\nz := 1");
    assert!(!parsed.errors.is_empty(), "expected parse errors");
    assert_eq!(parsed.program.stmts.len(), 2);
}

#[test]
fn method_call_parses_as_path_callee() {
    let p = parse_ok("z := p.dist()");
    match &p.stmts[0] {
        zz_frontend::ast::Stmt::Decl {
            value: E::Call { callee, args, .. },
            ..
        } => {
            assert!(matches!(
                callee.as_ref(),
                E::Path { parts, .. } if parts == &["p", "dist"]
            ));
            assert!(args.is_empty());
        }
        other => panic!("expected Call, got {other:?}"),
    }
}

#[test]
fn method_call_with_args() {
    let p = parse_ok("z := p.move(1, 2)");
    match &p.stmts[0] {
        zz_frontend::ast::Stmt::Decl {
            value: E::Call { args, .. },
            ..
        } => assert_eq!(args.len(), 2),
        other => panic!("expected Call, got {other:?}"),
    }
}

// --- indexing & slicing -------------------------------------------------

#[test]
fn parses_index() {
    let p = parse_ok("x := arr[0]");
    match &p.stmts[0] {
        zz_frontend::ast::Stmt::Decl {
            value: E::Index { obj, index, .. },
            ..
        } => {
            assert!(matches!(obj.as_ref(), E::Ident { name, .. } if name == "arr"));
            assert!(matches!(index.as_ref(), E::Int { value: 0, .. }));
        }
        other => panic!("expected Index, got {other:?}"),
    }
}

#[test]
fn parses_dict_index() {
    let p = parse_ok("x := ages[\"key\"]");
    match &p.stmts[0] {
        zz_frontend::ast::Stmt::Decl {
            value: E::Index { index, .. },
            ..
        } => assert!(matches!(index.as_ref(), E::Str { value, .. } if value == "key")),
        other => panic!("expected Index, got {other:?}"),
    }
}

#[test]
fn parses_slice_variants() {
    for (src, start, end) in [
        ("x := s[1:3]", Some(1), Some(3)),
        ("x := s[:3]", None, Some(3)),
        ("x := s[1:]", Some(1), None),
        ("x := s[:]", None, None),
    ] {
        let p = parse_ok(src);
        match &p.stmts[0] {
            zz_frontend::ast::Stmt::Decl {
                value: E::Slice {
                    start: st, end: en, ..
                },
                ..
            } => {
                let st_v = st.as_ref().map(|e| match e.as_ref() {
                    E::Int { value, .. } => *value,
                    other => panic!("expected int start, got {other:?}"),
                });
                let en_v = en.as_ref().map(|e| match e.as_ref() {
                    E::Int { value, .. } => *value,
                    other => panic!("expected int end, got {other:?}"),
                });
                assert_eq!(st_v, start, "start of `{src}`");
                assert_eq!(en_v, end, "end of `{src}`");
            }
            other => panic!("expected Slice for `{src}`, got {other:?}"),
        }
    }
}

#[test]
fn parses_index_on_path_and_call() {
    let p = parse_ok("x := ns.arr[0]");
    match &p.stmts[0] {
        zz_frontend::ast::Stmt::Decl {
            value: E::Index { obj, .. },
            ..
        } => assert!(matches!(obj.as_ref(), E::Path { parts, .. } if parts == &["ns", "arr"])),
        other => panic!("expected Index, got {other:?}"),
    }
    let p = parse_ok("x := make()[0]");
    match &p.stmts[0] {
        zz_frontend::ast::Stmt::Decl {
            value: E::Index { obj, .. },
            ..
        } => assert!(matches!(obj.as_ref(), E::Call { .. })),
        other => panic!("expected Index, got {other:?}"),
    }
}

#[test]
fn parses_index_assign() {
    let p = parse_ok("arr[0] = 5");
    match &p.stmts[0] {
        zz_frontend::ast::Stmt::Assign { target, value, .. } => {
            assert!(matches!(target, E::Index { .. }));
            assert!(matches!(value, E::Int { value: 5, .. }));
        }
        other => panic!("expected Assign, got {other:?}"),
    }
}

#[test]
fn missing_close_bracket_reports_error() {
    let parsed = zz_frontend::parse("x := arr[0");
    assert_eq!(parsed.errors.len(), 1);
}

// --- pipeline -----------------------------------------------------------

#[test]
fn pipe_desugars_to_call_with_lhs_first() {
    let p = parse_ok("x := a |> f(b)");
    match &p.stmts[0] {
        zz_frontend::ast::Stmt::Decl {
            value: E::Call { callee, args, .. },
            ..
        } => {
            assert!(matches!(callee.as_ref(), E::Ident { name, .. } if name == "f"));
            assert_eq!(args.len(), 2);
            assert!(matches!(&args[0], E::Ident { name, .. } if name == "a"));
            assert!(matches!(&args[1], E::Ident { name, .. } if name == "b"));
        }
        other => panic!("expected Call, got {other:?}"),
    }
}

#[test]
fn pipe_bare_name_becomes_call() {
    let p = parse_ok("x := a |> f");
    match &p.stmts[0] {
        zz_frontend::ast::Stmt::Decl {
            value: E::Call { callee, args, .. },
            ..
        } => {
            assert!(matches!(callee.as_ref(), E::Ident { name, .. } if name == "f"));
            assert_eq!(args.len(), 1);
        }
        other => panic!("expected Call, got {other:?}"),
    }
}

#[test]
fn pipe_empty_call_becomes_call() {
    let p = parse_ok("x := a |> f()");
    match &p.stmts[0] {
        zz_frontend::ast::Stmt::Decl {
            value: E::Call { callee, args, .. },
            ..
        } => {
            assert!(matches!(callee.as_ref(), E::Ident { name, .. } if name == "f"));
            assert_eq!(args.len(), 1);
        }
        other => panic!("expected Call, got {other:?}"),
    }
}

#[test]
fn pipe_chains_left_assoc() {
    let p = parse_ok("x := a |> f(b) |> g(c)");
    match &p.stmts[0] {
        zz_frontend::ast::Stmt::Decl {
            value: E::Call { callee, args, .. },
            ..
        } => {
            assert!(matches!(callee.as_ref(), E::Ident { name, .. } if name == "g"));
            assert_eq!(args.len(), 2);
            // First arg is the previous pipe result: f(a, b).
            match &args[0] {
                E::Call { callee, args, .. } => {
                    assert!(matches!(callee.as_ref(), E::Ident { name, .. } if name == "f"));
                    assert_eq!(args.len(), 2);
                }
                other => panic!("expected nested Call, got {other:?}"),
            }
            assert!(matches!(&args[1], E::Ident { name, .. } if name == "c"));
        }
        other => panic!("expected Call, got {other:?}"),
    }
}

#[test]
fn pipe_path_callee() {
    let p = parse_ok("x := a |> ns.f(b)");
    match &p.stmts[0] {
        zz_frontend::ast::Stmt::Decl {
            value: E::Call { callee, args, .. },
            ..
        } => {
            assert!(matches!(callee.as_ref(), E::Path { parts, .. } if parts == &["ns", "f"]));
            assert_eq!(args.len(), 2);
        }
        other => panic!("expected Call, got {other:?}"),
    }
}

#[test]
fn pipe_non_call_rhs_errors() {
    let parsed = zz_frontend::parse("x := a |> 5");
    assert_eq!(parsed.errors.len(), 1);
}

#[test]
fn pipe_lowest_precedence() {
    // `a + b |> f` pipes the whole sum.
    let p = parse_ok("x := a + b |> f");
    match &p.stmts[0] {
        zz_frontend::ast::Stmt::Decl {
            value: E::Call { args, .. },
            ..
        } => {
            assert!(matches!(&args[0], E::Binary { .. }));
        }
        other => panic!("expected Call, got {other:?}"),
    }
}
