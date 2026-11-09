#![no_main]

use libfuzzer_sys::fuzz_target;
use zz_checker::{check_program, CheckResult};
use zz_frontend::parse;
use std::collections::HashMap;

fuzz_target!(|data: &[u8]| {
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
});
