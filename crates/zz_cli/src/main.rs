//! ZZ CLI entry point.
//!
//! Phase 1:
//! - `zz`            → interactive REPL
//! - `zz eval <src>` → evaluate source once and print the result
//! - `zz run <file>` → type-check and run a `.zz` file
//! - `zz check <file>` → parse and type-check a `.zz` file without running it
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
    zz check <file.zz>    parse and type-check a file without running it
    zz --help             show this help
    zz --version          show version

EXAMPLES:
    zz eval 'let x = 1 + 2'
    zz eval '1 + 2 * 3'
    zz run hello.zz
    zz check hello.zz
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
        Some("run") => match run_file(args.get(1), &args[2..]) {
            Ok(()) => ExitCode::SUCCESS,
            Err(msg) => {
                eprintln!("zz: {msg}");
                ExitCode::FAILURE
            }
        },
        Some("check") => match check_file(args.get(1)) {
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

fn run_file(path: Option<&String>, script_args: &[String]) -> Result<(), String> {
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

    let mut interp = Interp::with_natives(loaded.natives.clone());
    interp.args = script_args.to_vec();
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

/// Parse and type-check a file (and its imports) without executing it.
fn check_file(path: Option<&String>) -> Result<(), String> {
    let path =
        path.ok_or_else(|| "missing file argument\n\nusage: zz check <file.zz>".to_string())?;

    let loaded = loader::load_program(std::path::Path::new(path))?;
    if !loaded.errors.is_empty() {
        for e in &loaded.errors {
            let mut files = Files::new();
            let id = files.add(e.name.clone(), e.source.clone());
            eprint!("{}", render_to_string(&files, id, &e.diags));
        }
        return Err("type-check failed".to_string());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::PathBuf;

    use super::check_file;

    fn write_temp(src: &str) -> PathBuf {
        let path = std::env::temp_dir().join(format!(
            "zz_check_test_{}_{}.zz",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::write(&path, src).unwrap();
        path
    }

    #[test]
    fn check_ok_on_valid_file() {
        let path = write_temp("x := 1 + 2\nimport std.io\nio.println(x)\n");
        let result = check_file(Some(&path.to_string_lossy().to_string()));
        assert!(result.is_ok(), "expected ok, got {result:?}");
        let _ = fs::remove_file(&path);
    }

    #[test]
    fn check_ok_on_phase4_features() {
        let path = write_temp(
            "scores := [10, 20, 30]\ny := scores[1]\nz := scores[1:3]\nfunc dbl(a: int, b: int) -> int { a * b }\nw := 5 |> dbl(3)\nt := typeof(w)\n",
        );
        let result = check_file(Some(&path.to_string_lossy().to_string()));
        assert!(result.is_ok(), "expected ok, got {result:?}");
        let _ = fs::remove_file(&path);
    }

    #[test]
    fn check_rejects_type_error() {
        let path = write_temp("x := 1 + \"a\"\n");
        let result = check_file(Some(&path.to_string_lossy().to_string()));
        assert!(result.is_err(), "expected type error");
        let _ = fs::remove_file(&path);
    }

    #[test]
    fn check_rejects_index_error() {
        let path = write_temp("x := 5\nx[0]\n");
        let result = check_file(Some(&path.to_string_lossy().to_string()));
        assert!(result.is_err(), "expected index type error");
        let _ = fs::remove_file(&path);
    }

    #[test]
    fn check_missing_file_errors() {
        let result = check_file(Some(&"/tmp/zz_no_such_file_zz.zz".to_string()));
        assert!(result.is_err(), "expected error for missing file");
    }

    #[test]
    fn check_no_arg_errors() {
        assert!(check_file(None).is_err());
    }
}
