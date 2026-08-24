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
    zz fix <file.zz>      apply safe auto-fixes (typo replacements, etc.)
    zz ifix <file.zz>     interactive fix: prompts before each change
    zz --help             show this help
    zz --version          show version

EXAMPLES:
    zz eval 'let x = 1 + 2'
    zz eval '1 + 2 * 3'
    zz run hello.zz
    zz check hello.zz
    zz fix hello.zz
    zz ifix hello.zz
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
        Some("fix") => match fix_file(args.get(1), false) {
            Ok(()) => ExitCode::SUCCESS,
            Err(msg) => {
                eprintln!("zz: {msg}");
                ExitCode::FAILURE
            }
        },
        Some("ifix") => match fix_file(args.get(1), true) {
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
    let mut has_errors = false;
    for e in &loaded.errors {
        let mut files = Files::new();
        let id = files.add(e.name.clone(), e.source.clone());
        eprint!("{}", render_to_string(&files, id, &e.diags));
        if e.diags
            .iter()
            .any(|d| d.severity == zz_frontend::diag::Severity::Error)
        {
            has_errors = true;
        }
    }
    if has_errors {
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
    let mut has_errors = false;
    for e in &loaded.errors {
        let mut files = Files::new();
        let id = files.add(e.name.clone(), e.source.clone());
        eprint!("{}", render_to_string(&files, id, &e.diags));
        if e.diags
            .iter()
            .any(|d| d.severity == zz_frontend::diag::Severity::Error)
        {
            has_errors = true;
        }
    }
    if has_errors {
        return Err("type-check failed".to_string());
    }
    Ok(())
}

/// Apply auto-fixes to a file. If `interactive` is true, prompt before each
/// destructive fix (unused imports/variables). Safe fixes (typo replacements
/// via FixIt) are always applied silently.
fn fix_file(path: Option<&String>, interactive: bool) -> Result<(), String> {
    use zz_frontend::diag::Severity;

    let path =
        path.ok_or_else(|| "missing file argument\n\nusage: zz fix <file.zz>".to_string())?;

    let source = std::fs::read_to_string(path).map_err(|e| format!("cannot read `{path}`: {e}"))?;

    let loaded = loader::load_program(std::path::Path::new(path))?;

    // Collect all fixits from diagnostics.
    let mut fixits: Vec<zz_frontend::diag::FixIt> = Vec::new();
    let mut warnings_only: Vec<zz_frontend::diag::RawDiag> = Vec::new();

    for e in &loaded.errors {
        for d in &e.diags {
            match d.severity {
                Severity::Error => {
                    // Errors have no fixits — skip.
                }
                Severity::Warning if !d.fixits.is_empty() => {
                    // Safe fixes: collect for auto-apply.
                    fixits.extend(d.fixits.iter().cloned());
                }
                Severity::Warning => {
                    // Warnings without fixits (e.g., unused vars without
                    // explicit fixits) — show in interactive mode.
                    warnings_only.push(d.clone());
                }
                _ => {}
            }
        }
    }

    if fixits.is_empty() && warnings_only.is_empty() {
        eprintln!("zz: no fixable diagnostics in `{path}`");
        return Ok(());
    }

    let mut new_source = source.clone();
    let mut applied = 0u32;

    // Apply safe FixIt replacements (typos, etc.) in reverse span order
    // so earlier spans aren't invalidated.
    fixits.sort_by(|a, b| b.span.start.cmp(&a.span.start));
    for fixit in &fixits {
        let start = fixit.span.start as usize;
        let end = fixit.span.end as usize;
        if end <= new_source.len() && start < end {
            new_source.replace_range(start..end, &fixit.replacement);
            applied += 1;
            eprintln!(
                "  fixed: `{}` → `{}` at {path}:{}",
                &source[fixit.span.start as usize..fixit.span.end as usize],
                fixit.replacement,
                fixit.span.start,
            );
        }
    }

    // In interactive mode, prompt for warnings without fixits.
    if interactive {
        for d in &warnings_only {
            let msg = &d.message;
            let span_info = d
                .span
                .map(|s| format!(" at byte {}", s.start))
                .unwrap_or_default();
            eprint!("  apply fix for `{msg}`{span_info}? [y/N]: ");
            let mut input = String::new();
            std::io::stdin()
                .read_line(&mut input)
                .map_err(|e| format!("stdin error: {e}"))?;
            if input.trim().eq_ignore_ascii_case("y") {
                // For warnings without explicit fixits, we can't auto-fix.
                // Just inform the user.
                eprintln!("    (no automatic fix available — edit manually)");
            } else {
                eprintln!("    (skipped)");
            }
        }
    }

    if applied > 0 {
        std::fs::write(path, &new_source).map_err(|e| format!("cannot write `{path}`: {e}"))?;
        eprintln!("zz: applied {applied} fix(es) to `{path}`");
    } else if !interactive {
        eprintln!("zz: no fixable diagnostics in `{path}`");
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
