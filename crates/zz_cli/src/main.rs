//! ZZ CLI entry point.
//!
//! Phase 1:
//! - `zz`            → interactive REPL
//! - `zz eval <src>` → evaluate source once and print the result
//! - `zz run <file>` → type-check and run a `.zz` file
//! - `zz --help`     → usage

use std::process::ExitCode;

mod loader;
mod repl;
mod session;

use zz_frontend::diag::{error_at, render_to_string, Files};
use zz_runtime::{Interp, Value};

use session::Session;

const VERSION: &str = env!("CARGO_PKG_VERSION");

const USAGE: &str = "\
zz — the ZZ programming language

USAGE:
    zz                    start the interactive REPL
    zz eval <source>      evaluate source and print the result
    zz run <file.zz>      type-check and run a file
    zz --help             show this help
    zz --version          show version

EXAMPLES:
    zz eval 'let x = 1 + 2'
    zz eval '1 + 2 * 3'
    zz run hello.zz
";

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        None => match repl::run() {
            Ok(()) => ExitCode::SUCCESS,
            Err(e) => {
                eprintln!("zz: repl error: {e}");
                ExitCode::FAILURE
            }
        },
        Some("eval") => {
            let src = args[1..].join(" ");
            let mut session = Session::new("<eval>");
            let output = session.eval_to_console(&src);
            if !output.is_empty() {
                println!("{output}");
            }
            if session.last_eval_had_errors() {
                ExitCode::FAILURE
            } else {
                ExitCode::SUCCESS
            }
        }
        Some("run") => match run_file(args.get(1)) {
            Ok(()) => ExitCode::SUCCESS,
            Err(msg) => {
                eprintln!("zz: {msg}");
                ExitCode::FAILURE
            }
        },
        Some("--help") | Some("-h") => {
            print!("{USAGE}");
            ExitCode::SUCCESS
        }
        Some("--version") | Some("-V") => {
            println!("zz {VERSION}");
            ExitCode::SUCCESS
        }
        Some(other) => {
            eprintln!("zz: unknown command `{other}`\n");
            eprint!("{USAGE}");
            ExitCode::from(2)
        }
    }
}

fn run_file(path: Option<&String>) -> Result<(), String> {
    let path =
        path.ok_or_else(|| "missing file argument\n\nusage: zz run <file.zz>".to_string())?;

    let loaded = loader::load_program(std::path::Path::new(path))?;
    if !loaded.errors.is_empty() {
        for e in &loaded.errors {
            let mut files = Files::new();
            let id = files.add(e.name.clone(), e.source.clone());
            eprint!("{}", render_to_string(&files, id, &e.diags));
        }
        return Err("program failed".to_string());
    }

    let mut interp = Interp::with_natives(zz_stdlib::stdlib_natives());
    let mut last = Value::Unit;
    for (i, program) in loaded.programs.iter().enumerate() {
        match interp.run(program) {
            Ok(v) => last = v,
            Err(e) => {
                let (name, source) = loaded
                    .files
                    .get(i)
                    .cloned()
                    .unwrap_or_else(|| (path.clone(), String::new()));
                let mut files = Files::new();
                let id = files.add(name, source);
                let diags = vec![error_at(e.message.clone(), e.span)];
                eprint!("{}", render_to_string(&files, id, &diags));
                return Err("program failed".to_string());
            }
        }
    }
    if last != Value::Unit {
        println!("{last}");
    }
    Ok(())
}
