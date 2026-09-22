use std::collections::HashMap;

use zz_checker::check_program;
use zz_frontend::parse;
use zz_runtime::{Interp, Value};

use super::{register_module_namespace, stdlib_funcs, stdlib_natives};

pub(super) fn run(src: &str) -> Result<Value, String> {
    let parsed = parse(src);
    if !parsed.errors.is_empty() {
        return Err(format!("parse errors: {:?}", parsed.errors));
    }

    let mut funcs = stdlib_funcs();
    let mut natives = stdlib_natives();
    for module in ["io", "str", "vec", "json", "fs", "env", "math", "time"] {
        register_module_namespace(module, module, &mut funcs, &mut natives).expect("known module");
    }

    let checked = check_program(&parsed.program, HashMap::new(), funcs, HashMap::new());
    let has_errors = checked
        .errors
        .iter()
        .any(|e| e.severity == zz_frontend::diag::Severity::Error);
    if has_errors {
        return Err(format!("check errors: {:?}", checked.errors));
    }

    let mut interp = Interp::with_natives(natives);
    interp
        .run(&parsed.program)
        .map_err(|e| e.message.to_string())
}

mod control_flow;
mod conversions;
mod env;
mod fs;
mod functions;
mod math;
mod methods;
mod operators;
mod pipeline;
mod time;
mod typeof_tests;
mod types;
