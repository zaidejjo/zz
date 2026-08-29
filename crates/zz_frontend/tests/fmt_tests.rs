//! Formatter tests.

use zz_frontend::tests::common::parse_ok;
use zz_frontend::{format_program, is_formatted, FormatConfig};

#[test]
fn formats_short_decl() {
    let src = "x:=1+2";
    let p = parse_ok(src);
    let formatted = format_program(&p, src, &FormatConfig::default());
    assert_eq!(formatted, "x := 1 + 2\n");
}

#[test]
fn formats_explicit_decl() {
    let src = "x:int=10";
    let p = parse_ok(src);
    let formatted = format_program(&p, src, &FormatConfig::default());
    assert_eq!(formatted, "x: int = 10\n");
}

#[test]
fn formats_func() {
    let src = "func add(a:int,b:int)->int{return a+b}";
    let p = parse_ok(src);
    let formatted = format_program(&p, src, &FormatConfig::default());
    assert_eq!(
        formatted,
        "func add(a: int, b: int) -> int {\n    return a + b\n}\n"
    );
}

#[test]
fn formats_if_else() {
    let src = "if x>5{1}else{2}";
    let p = parse_ok(src);
    let formatted = format_program(&p, src, &FormatConfig::default());
    assert_eq!(formatted, "if x > 5 {\n    1\n} else {\n    2\n}\n");
}

#[test]
fn formats_match() {
    let src = "match x { .ok(v) => v, .err(e) => 0 }";
    let p = parse_ok(src);
    let formatted = format_program(&p, src, &FormatConfig::default());
    assert_eq!(
        formatted,
        "match x {\n    .ok(v) => v\n    .err(e) => 0\n}\n"
    );
}

#[test]
fn is_formatted_true() {
    let src = "x := 1 + 2\n";
    let p = parse_ok(src);
    assert!(is_formatted(&p, src, &FormatConfig::default()));
}

#[test]
fn is_formatted_false() {
    let src = "x:=1+2";
    let p = parse_ok(src);
    assert!(!is_formatted(&p, src, &FormatConfig::default()));
}

#[test]
fn custom_indent_width() {
    let src = "if x { y := 1 }";
    let p = parse_ok(src);
    let config = FormatConfig { indent_width: 2 };
    let formatted = format_program(&p, src, &config);
    assert_eq!(formatted, "if x {\n  y := 1\n}\n");
}
