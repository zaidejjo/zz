//! `textDocument/signatureHelp` — shows function parameter info at call sites.
//!
//! Walks backward from the cursor to find the enclosing `fn_name(...)` call,
//! counts commas to determine the active parameter index, and resolves the
//! function's signature from the `CheckResult`.

use tower_lsp::lsp_types::{
    ParameterInformation, ParameterLabel, SignatureHelp, SignatureInformation,
};
use zz_checker::{CheckResult, FuncSig};
use zz_frontend::ast::Program;

// ── Public API ───────────────────────────────────────────────────────────

/// Build signature help for the cursor position inside a function call.
pub fn signature_help_for_position(
    _program: &Program,
    source: &str,
    offset: u32,
    check_result: Option<&CheckResult>,
) -> Option<SignatureHelp> {
    let ctx = detect_call_context(source, offset)?;
    let cr = check_result?;

    let sig = cr.funcs.get(&ctx.func_name)?;

    let label = format_func_label(&ctx.func_name, sig);
    let parameters: Vec<ParameterInformation> = sig
        .params
        .iter()
        .map(|(name, ty)| ParameterInformation {
            label: ParameterLabel::Simple(format!("{name}: {ty}")),
            documentation: None,
        })
        .collect();

    let documentation = format!("```zz\n{label}\n```");

    Some(SignatureHelp {
        signatures: vec![SignatureInformation {
            label,
            documentation: Some(tower_lsp::lsp_types::Documentation::String(documentation)),
            parameters: Some(parameters),
            active_parameter: None,
        }],
        active_signature: Some(0),
        active_parameter: Some(ctx.active_param),
    })
}

// ── Call context detection ───────────────────────────────────────────────

/// Result of parsing the call site context.
#[derive(Debug, Clone, PartialEq)]
struct CallContext {
    func_name: String,
    active_param: u32,
}

/// Walk backward from `offset` to find the enclosing function call and
/// count commas to determine the active parameter index.
///
/// # Algorithm
///
/// Walking backward through the source:
/// - `)` increments depth (opens a nested paren group going backward).
/// - `(` with `depth > 0` decrements depth (closes an inner nested group).
/// - `(` with `depth == 0` is the **target function opening** — extract the
///   identifier before it and return.
/// - Commas at `depth == 0` increment the active parameter counter.
/// - Characters inside string literals (`"..."`) are skipped so that commas
///   and parens inside strings don't affect the result.
fn detect_call_context(source: &str, offset: u32) -> Option<CallContext> {
    let bytes = source.as_bytes();
    let pos = offset as usize;
    if pos == 0 {
        return None;
    }

    let mut depth: i32 = 0;
    let mut commas_at_depth_zero: u32 = 0;
    let mut in_string = false;
    let mut i = pos - 1;

    loop {
        let ch = bytes[i];

        // ── String literal detection ────────────────────────────────
        // Toggle in_string on unescaped double quotes.
        if ch == b'"' {
            let escaped = i > 0 && bytes[i - 1] == b'\\';
            if !escaped {
                in_string = !in_string;
            }
            if i == 0 {
                break;
            }
            i -= 1;
            continue;
        }

        // Skip everything inside string literals.
        if in_string {
            if i == 0 {
                break;
            }
            i -= 1;
            continue;
        }

        // ── Paren depth tracking ────────────────────────────────────
        match ch {
            b')' => {
                depth += 1;
            }
            b'(' => {
                if depth > 0 {
                    // Closes an inner nested paren group.
                    depth -= 1;
                } else {
                    // depth == 0 — this is the target function call opening.
                    let name = extract_func_name_before(bytes, i);
                    if name.is_empty() {
                        return None;
                    }
                    return Some(CallContext {
                        func_name: name,
                        active_param: commas_at_depth_zero,
                    });
                }
            }
            b',' if depth == 0 => {
                commas_at_depth_zero += 1;
            }
            _ => {}
        }

        if i == 0 {
            break;
        }
        i -= 1;
    }

    None
}

/// Extract the identifier (function name) immediately before byte index `pos`.
/// The byte at `pos` should be `(`.
fn extract_func_name_before(bytes: &[u8], pos: usize) -> String {
    let mut end = pos;
    // Skip whitespace before `(`.
    while end > 0 && bytes[end - 1] == b' ' {
        end -= 1;
    }
    if end == 0 {
        return String::new();
    }
    // Now walk backwards through identifier chars.
    let mut i = end - 1;
    loop {
        let ch = bytes[i];
        if ch.is_ascii_alphanumeric() || ch == b'_' {
            if i == 0 {
                return String::from_utf8_lossy(&bytes[0..end]).to_string();
            }
            i -= 1;
        } else {
            return String::from_utf8_lossy(&bytes[i + 1..end]).to_string();
        }
    }
}

// ── Formatting helpers ───────────────────────────────────────────────────

fn format_func_label(name: &str, sig: &FuncSig) -> String {
    let params: Vec<String> = sig
        .params
        .iter()
        .map(|(n, t)| format!("{n}: {t}"))
        .collect();
    format!("func {name}({}) -> {}", params.join(", "), sig.ret)
}

// ── Tests ────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use zz_checker::check_program;
    use zz_frontend::parse;

    fn check(source: &str) -> (Program, Option<CheckResult>) {
        let parsed = parse(source);
        let cr = check_program(
            &parsed.program,
            HashMap::new(),
            HashMap::new(),
            HashMap::new(),
        );
        (parsed.program, Some(cr))
    }

    // ── Context detection ─────────────────────────────────────────────

    #[test]
    fn call_context_after_open_paren() {
        // foo(|) — cursor right after `(`
        let src = "func foo(x: int, y: int) -> int { return x + y }\nfoo(";
        let ctx = detect_call_context(src, src.len() as u32);
        let ctx = ctx.unwrap();
        assert_eq!(ctx.func_name, "foo");
        assert_eq!(ctx.active_param, 0);
    }

    #[test]
    fn call_context_after_first_comma() {
        // foo(1, |) — cursor after the comma
        let src = "func foo(x: int, y: int) -> int { return x + y }\nfoo(1,";
        let ctx = detect_call_context(src, src.len() as u32);
        let ctx = ctx.unwrap();
        assert_eq!(ctx.func_name, "foo");
        assert_eq!(ctx.active_param, 1);
    }

    #[test]
    fn call_context_after_second_comma() {
        // foo(1, 2, |)
        let src = "func foo(x: int, y: int, z: int) -> int { return x + y + z }\nfoo(1, 2,";
        let ctx = detect_call_context(src, src.len() as u32);
        let ctx = ctx.unwrap();
        assert_eq!(ctx.func_name, "foo");
        assert_eq!(ctx.active_param, 2);
    }

    #[test]
    fn call_context_nested_calls() {
        // foo(bar(x), |) — comma is at depth 0 (outer call), bar's parens cancel out.
        let src = "func bar(a: int) -> int { return a }\nfunc foo(x: int, y: int) -> int { return x + y }\nfoo(bar(x),";
        let ctx = detect_call_context(src, src.len() as u32);
        let ctx = ctx.unwrap();
        assert_eq!(ctx.func_name, "foo");
        assert_eq!(ctx.active_param, 1);
    }

    #[test]
    fn call_context_nested_deep() {
        // foo(bar(baz(x)), |) — triple nesting, comma still counts for outer call.
        let src = "func baz(a: int) -> int { return a }\nfunc bar(a: int) -> int { return a }\nfunc foo(x: int, y: int) -> int { return x + y }\nfoo(bar(baz(x)),";
        let ctx = detect_call_context(src, src.len() as u32);
        let ctx = ctx.unwrap();
        assert_eq!(ctx.func_name, "foo");
        assert_eq!(ctx.active_param, 1);
    }

    #[test]
    fn call_context_string_comma_ignored() {
        // foo("a,b") — comma inside string is skipped, active_param stays 0.
        // Source ends before closing paren so the outer `(` is at depth 0.
        let src = "foo(\"a,b\"";
        let ctx = detect_call_context(src, src.len() as u32);
        let ctx = ctx.unwrap();
        assert_eq!(ctx.func_name, "foo");
        assert_eq!(ctx.active_param, 0);
    }

    #[test]
    fn call_context_nested_with_extra_arg() {
        // foo(bar(x), y) — with an additional second arg, cursor after `y`.
        // Walking back: `y` skip, `,` depth=0 commas=1, `)` depth=1,
        // bar's `(` depth>0 → 0 continue, foo's `(` depth==0 → return.
        let src = "foo(bar(x), y";
        let ctx = detect_call_context(src, src.len() as u32);
        let ctx = ctx.unwrap();
        assert_eq!(ctx.func_name, "foo");
        assert_eq!(ctx.active_param, 1);
    }

    // ── Full signature help pipeline ──────────────────────────────────

    #[test]
    fn signature_help_first_param() {
        let src = "func add(x: int, y: int) -> int { return x + y }\nadd(";
        let (program, cr) = check(src);
        let help = signature_help_for_position(&program, src, src.len() as u32, cr.as_ref());
        let help = help.unwrap();
        assert_eq!(help.signatures.len(), 1);
        assert_eq!(help.active_signature, Some(0));
        assert_eq!(help.active_parameter, Some(0));
        assert!(help.signatures[0].label.contains("add"));
        let params = help.signatures[0].parameters.as_ref().unwrap();
        assert_eq!(params.len(), 2);
    }

    #[test]
    fn signature_help_second_param() {
        let src = "func add(x: int, y: int) -> int { return x + y }\nadd(1,";
        let (program, cr) = check(src);
        let help = signature_help_for_position(&program, src, src.len() as u32, cr.as_ref());
        let help = help.unwrap();
        assert_eq!(help.active_parameter, Some(1));
    }

    #[test]
    fn signature_help_no_call_returns_none() {
        let src = "func add(x: int, y: int) -> int { return x + y }\na := 1";
        let (program, cr) = check(src);
        let help = signature_help_for_position(&program, src, src.len() as u32, cr.as_ref());
        assert!(help.is_none());
    }

    #[test]
    fn signature_help_unknown_func_returns_none() {
        let src = "unknown_func(";
        let (program, cr) = check(src);
        let help = signature_help_for_position(&program, src, src.len() as u32, cr.as_ref());
        assert!(help.is_none());
    }

    #[test]
    fn signature_help_label_format() {
        let src = "func greet(name: str) -> str { return name }\ngreet(";
        let (program, cr) = check(src);
        let help = signature_help_for_position(&program, src, src.len() as u32, cr.as_ref());
        let label = &help.unwrap().signatures[0].label;
        assert!(label.contains("func greet(name: str) -> str"));
    }
}
