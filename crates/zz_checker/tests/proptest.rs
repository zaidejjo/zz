//! Property-based tests for the type checker using proptest.
//!
//! These tests verify invariant properties that should hold for all valid
//! inputs, complementing the example-based tests in type_check_tests.rs.

use proptest::prelude::*;
use std::collections::HashMap;
use zz_checker::{check_program, CheckResult};
use zz_frontend::parse;

/// A small subset of expressions that always parse and type-check without
/// errors. Non-recursive to avoid strategy construction issues.
fn simple_expr() -> BoxedStrategy<String> {
    prop_oneof![
        // Integer literals
        any::<i32>().prop_map(|n| n.to_string()),
        // Boolean literals
        prop_oneof![Just("true".to_string()), Just("false".to_string())],
        // Variable references (simple identifiers)
        "[a-z]+".prop_map(|s: String| s),
        // Parenthesized atomic expressions
        prop_oneof![
            any::<i32>().prop_map(|n| n.to_string()),
            prop_oneof![Just("true".to_string()), Just("false".to_string())],
            "[a-z]+".prop_map(|s: String| s),
        ]
        .prop_map(|e| format!("({})", e)),
        // Binary addition of atomics
        (
            prop_oneof![
                any::<i32>().prop_map(|n| n.to_string()),
                prop_oneof![Just("true".to_string()), Just("false".to_string())],
                "[a-z]+".prop_map(|s: String| s),
            ],
            prop_oneof![
                any::<i32>().prop_map(|n| n.to_string()),
                prop_oneof![Just("true".to_string()), Just("false".to_string())],
                "[a-z]+".prop_map(|s: String| s),
            ],
        )
            .prop_map(|(a, b)| format!("{} + {}", a, b)),
    ]
    .boxed()
}

/// Test that parsing a simple expression never produces parse errors.
proptest! {
    #![proptest_config(ProptestConfig::with_cases(200))]

    #[test]
    fn parser_never_errors_simple_expr(expr in simple_expr()) {
        let parsed = parse(&expr);
        assert!(
            parsed.errors.is_empty(),
            "parse errors for `{expr}`: {:?}",
            parsed.errors
        );
    }
}

/// Test that type-checking simple expressions never panics.
proptest! {
    #![proptest_config(ProptestConfig::with_cases(200))]

    #[test]
    fn type_checker_no_panic_simple_expr(expr in simple_expr()) {
        let parsed = parse(&expr);
        if parsed.errors.is_empty() {
            let result = std::panic::catch_unwind(|| {
                check_program(
                    &parsed.program,
                    HashMap::new(),
                    HashMap::new(),
                    HashMap::new(),
                    HashMap::new(),
                    HashMap::new(),
                )
            });
            assert!(
                result.is_ok(),
                "type-checker panicked on expression: {expr}"
            );
            let _ = result.unwrap();
        }
    }
}

/// Test that unifying a type with itself always succeeds (reflexivity).
proptest! {
    #![proptest_config(ProptestConfig::with_cases(200))]

    #[test]
    fn unifier_reflexivity(ty in simple_type()) {
        use zz_unify::Unifier;

        let mut unifier = Unifier::new();
        let t1 = ty.clone();
        let t2 = ty;
        assert!(
            unifier.unify(&t1, &t2).is_ok(),
            "unifier reflexivity failed for type: {:?}",
            t1
        );
    }
}

/// Simple type generator for unifier tests.
fn simple_type() -> BoxedStrategy<zz_types::Type> {
    prop_oneof![
        Just(zz_types::Type::Int),
        Just(zz_types::Type::Float),
        Just(zz_types::Type::Bool),
        Just(zz_types::Type::Str),
        Just(zz_types::Type::Unit),
        Just(zz_types::Type::Error),
    ]
    .boxed()
}
