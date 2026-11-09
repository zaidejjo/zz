use std::collections::HashMap;
/// Fuzz target for the ZZ type checker.
///
/// This can be run with `cargo fuzz run checker_fuzz` after installing
/// cargo-fuzz, or directly as a unit test with `cargo test -p fuzz`.
///
/// The fuzz target feeds arbitrary byte slices as ZZ source code to the
/// parser and type checker, checking for panics or internal errors.
use zz_checker::{check_program, CheckResult};
use zz_frontend::parse;

/// Entry point for libFuzzer. Called with arbitrary byte slices.
pub fn fuzz_target(data: &[u8]) {
    let src = match std::str::from_utf8(data) {
        Ok(s) => s,
        Err(_) => return,
    };
    let parsed = parse(src);
    if !parsed.errors.is_empty() {
        return;
    }
    let _result: CheckResult = check_program(
        &parsed.program,
        HashMap::new(),
        HashMap::new(),
        HashMap::new(),
        HashMap::new(),
        HashMap::new(),
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fuzz_seed_inputs() {
        // Test that seed corpus inputs parse and type-check without panicking
        let seeds: &[&[u8]] = &[
            b"x := 1",
            b"x := \"hello\"",
            b"x := true",
            b"x := [1, 2, 3]",
            b"x := {a: 1, b: 2}",
            b"f := |x| x + 1",
            b"x := f(5)",
        ];
        for seed in seeds {
            fuzz_target(seed);
        }
    }
}
