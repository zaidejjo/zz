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
    zz                            start the interactive REPL
    zz eval <source>              evaluate source and print the result
    zz run <file.zz>              type-check and run a file
    zz check [FLAGS] [PATH]       scan for errors/warnings (file or directory)
    zz fix [FLAGS] [PATH]         apply auto-fixes (shortcut for check --fix)

FLAGS:
    --fix, -f          apply safe auto-fixes (typo replacements, field corrections)
    --hard             with --fix, apply ALL fixes including ambiguous ones (no prompts)
    --interactive, -i  with --fix, prompt for ambiguous fixes interactively
    --help, -h         show this help
    --version, -V      show version

FIX SAFETY:
    Safe fixes (single unambiguous match) are applied automatically with --fix.
    Ambiguous fixes (multiple candidates) require --hard or --interactive (-i).

PATH can be a single .zz file or a directory (recursively scans all .zz files).
Defaults to `.` (current directory) if omitted.

EXAMPLES:
    zz check .                       scan current directory
    zz check src/ --fix              fix all safe issues in src/
    zz fix hello.zz                  fix a single file
    zz check --fix --hard src/       force-apply all fixes, no prompts
    zz check --fix -i src/           interactive mode for ambiguous fixes
";

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();

    // Separate the subcommand from flags and path.
    let cmd = args.first().map(String::as_str);
    let rest = args.get(1..).unwrap_or(&[]);

    match cmd {
        None => match repl::run() {
            Ok(()) => ExitCode::SUCCESS,
            Err(e) => {
                eprintln!("zz: repl error: {e}");
                ExitCode::FAILURE
            }
        },
        Some("eval") => {
            let src = rest.join(" ");
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
        Some("run") => match run_file(rest.first(), &rest[1..]) {
            Ok(()) => ExitCode::SUCCESS,
            Err(msg) => {
                eprintln!("zz: {msg}");
                ExitCode::FAILURE
            }
        },
        Some("check") => {
            let (path, flags) = parse_path_and_flags(rest);
            let has_fix = flags.contains(&"--fix".to_string());
            let has_hard = flags.contains(&"--hard".to_string());
            let has_interactive = flags.contains(&"--interactive".to_string());

            let interactive = has_fix && has_interactive && !has_hard;
            match check_or_fix_path(&path, has_fix, interactive, has_hard) {
                Ok(()) => ExitCode::SUCCESS,
                Err(msg) => {
                    eprintln!("zz: {msg}");
                    ExitCode::FAILURE
                }
            }
        }
        Some("fix") => {
            let (path, flags) = parse_path_and_flags(rest);
            let has_hard = flags.contains(&"--hard".to_string());
            let has_interactive = flags.contains(&"--interactive".to_string());
            let interactive = has_interactive && !has_hard;
            match check_or_fix_path(&path, true, interactive, has_hard) {
                Ok(()) => ExitCode::SUCCESS,
                Err(msg) => {
                    eprintln!("zz: {msg}");
                    ExitCode::FAILURE
                }
            }
        }
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

/// Split args into flags (--flag items) and path (last non-flag arg).
/// Flags must precede the path: `zz check --fix src/`.
/// Short aliases are normalized: `-i` → `--interactive`, `-f` → `--fix`.
fn parse_path_and_flags(args: &[String]) -> (Option<String>, Vec<String>) {
    let mut path = None;
    let mut flags = Vec::new();
    for a in args {
        if a == "-i" {
            flags.push("--interactive".to_string());
        } else if a == "-f" {
            flags.push("--fix".to_string());
        } else if a.starts_with("--") {
            flags.push(a.clone());
        } else {
            // Last non-flag wins as path.
            path = Some(a.clone());
        }
    }
    (path, flags)
}

/// Collect all `.zz` files from a path (file or directory, recursive).
fn collect_zz_files(path: &std::path::Path) -> Result<Vec<std::path::PathBuf>, String> {
    if path.is_file() {
        Ok(vec![path.to_path_buf()])
    } else if path.is_dir() {
        let mut files = Vec::new();
        collect_zz_recursive(path, &mut files)?;
        files.sort();
        Ok(files)
    } else {
        Err(format!("path `{}` does not exist", path.display()))
    }
}

fn collect_zz_recursive(
    dir: &std::path::Path,
    out: &mut Vec<std::path::PathBuf>,
) -> Result<(), String> {
    let entries = std::fs::read_dir(dir)
        .map_err(|e| format!("cannot read directory `{}`: {e}", dir.display()))?;
    for entry in entries {
        let entry = entry.map_err(|e| format!("read_dir error: {e}"))?;
        let p = entry.path();
        if p.is_dir() {
            collect_zz_recursive(&p, out)?;
        } else if p.extension().and_then(|s| s.to_str()) == Some("zz") {
            out.push(p);
        }
    }
    Ok(())
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

/// Parse, type-check, and optionally auto-fix files.
fn check_or_fix_path(
    path_arg: &Option<String>,
    do_fix: bool,
    interactive: bool,
    force: bool,
) -> Result<(), String> {
    use zz_frontend::diag::{FixSafety, Severity};

    let raw = path_arg.as_deref().unwrap_or(".");
    let base = std::path::Path::new(raw);
    let files = collect_zz_files(base)?;

    if files.is_empty() {
        return Err(format!("no .zz files found in `{raw}`"));
    }

    let mut total_errors = 0u32;
    let mut total_fixes = 0u32;
    let mut any_safe_fixits = false;
    let mut any_ambiguous = false;

    for path in &files {
        let path_str = path.display().to_string();
        let source =
            std::fs::read_to_string(path).map_err(|e| format!("cannot read `{path_str}`: {e}"))?;

        let loaded = loader::load_program(path)?;

        // Classify fixits by safety.
        let mut safe_fixits: Vec<zz_frontend::diag::FixIt> = Vec::new();
        let mut ambiguous_fixits: Vec<zz_frontend::diag::FixIt> = Vec::new();
        let mut has_hard_errors = false;

        for e in &loaded.errors {
            for d in &e.diags {
                match d.severity {
                    Severity::Error | Severity::Warning => {
                        for fixit in &d.fixits {
                            match fixit.safety {
                                FixSafety::Safe => safe_fixits.push(fixit.clone()),
                                FixSafety::Ambiguous => ambiguous_fixits.push(fixit.clone()),
                            }
                        }
                        if d.severity == Severity::Error && d.fixits.is_empty() {
                            has_hard_errors = true;
                        }
                    }
                    _ => {}
                }
            }
        }

        if !do_fix {
            // Check-only mode: print diagnostics, track what's fixable.
            for e in &loaded.errors {
                let mut files_ctx = Files::new();
                let id = files_ctx.add(e.name.clone(), e.source.clone());
                eprint!("{}", render_to_string(&files_ctx, id, &e.diags));
            }
            if !safe_fixits.is_empty() {
                any_safe_fixits = true;
            }
            if !ambiguous_fixits.is_empty() {
                any_ambiguous = true;
            }
            if has_hard_errors {
                total_errors += 1;
            }
            continue;
        }

        // Fix mode.
        let mut new_source = source.clone();
        let mut applied = 0u32;

        // Always apply safe fixes.
        safe_fixits.sort_by(|a, b| b.span.start.cmp(&a.span.start));
        for fixit in &safe_fixits {
            let start = fixit.span.start as usize;
            let end = fixit.span.end as usize;
            if end <= new_source.len() && start < end {
                new_source.replace_range(start..end, &fixit.replacement);
                applied += 1;
                eprintln!(
                    "  fixed: `{}` → `{}` at {path_str}:{}",
                    &source[start..end],
                    fixit.replacement,
                    fixit.span.start,
                );
            }
        }

        // Handle ambiguous fixes based on mode.
        if !ambiguous_fixits.is_empty() {
            if force {
                // --hard: auto-apply first candidate for ambiguous fixes.
                ambiguous_fixits.sort_by(|a, b| b.span.start.cmp(&a.span.start));
                for fixit in &ambiguous_fixits {
                    let start = fixit.span.start as usize;
                    let end = fixit.span.end as usize;
                    if end <= new_source.len() && start < end {
                        new_source.replace_range(start..end, &fixit.replacement);
                        applied += 1;
                        eprintln!(
                            "  fixed (force): `{}` → `{}` at {path_str}:{}",
                            &source[start..end],
                            fixit.replacement,
                            fixit.span.start,
                        );
                    }
                }
            } else if interactive {
                // -i / --interactive: prompt user for each ambiguous fix.
                for fixit in &ambiguous_fixits {
                    let start = fixit.span.start as usize;
                    let end = fixit.span.end as usize;
                    if end > new_source.len() || start >= end {
                        continue;
                    }
                    let original = &source[start..end];
                    let line_num = source[..start].matches('\n').count() + 1;

                    if fixit.alternatives.len() > 1 {
                        // Multiple candidates: show numbered menu.
                        eprintln!("  Ambiguous field `{original}` at {path_str}:{line_num}:");
                        for (i, alt) in fixit.alternatives.iter().enumerate() {
                            eprintln!("    [{}] {}", i + 1, alt);
                        }
                        eprintln!("    [s] Skip");
                        eprint!("  Choice [1-{}]: ", fixit.alternatives.len());

                        let mut input = String::new();
                        std::io::stdin()
                            .read_line(&mut input)
                            .map_err(|e| format!("stdin error: {e}"))?;
                        let trimmed = input.trim();

                        if trimmed.is_empty() || trimmed.eq_ignore_ascii_case("s") {
                            eprintln!("    skipped");
                            continue;
                        }

                        // Parse number.
                        match trimmed.parse::<usize>() {
                            Ok(n) if n >= 1 && n <= fixit.alternatives.len() => {
                                let chosen = &fixit.alternatives[n - 1];
                                new_source.replace_range(start..end, chosen);
                                applied += 1;
                                eprintln!("    applied: `{original}` → `{chosen}`");
                            }
                            _ => {
                                eprintln!("    invalid choice, skipped");
                            }
                        }
                    } else {
                        // Single candidate: simple Y/n prompt.
                        eprint!(
                            "  ambiguous fix: `{original}` at {path_str}:{} — apply `{}`? [Y/n]: ",
                            fixit.span.start, fixit.replacement,
                        );
                        let mut input = String::new();
                        std::io::stdin()
                            .read_line(&mut input)
                            .map_err(|e| format!("stdin error: {e}"))?;
                        let trimmed = input.trim();
                        if trimmed.is_empty() || trimmed.eq_ignore_ascii_case("y") {
                            new_source.replace_range(start..end, &fixit.replacement);
                            applied += 1;
                            eprintln!("    applied: `{original}` → `{}`", fixit.replacement);
                        } else {
                            eprintln!("    skipped");
                        }
                    }
                }
            } else {
                // Default --fix: skip ambiguous, hint user.
                any_ambiguous = true;
            }
        }

        if applied > 0 {
            std::fs::write(path, &new_source)
                .map_err(|e| format!("cannot write `{path_str}`: {e}"))?;
            eprintln!("zz: applied {applied} fix(es) to `{path_str}`");
            total_fixes += applied;
        }
    }

    if !do_fix && total_errors > 0 {
        return Err(format!("{total_errors} file(s) failed type-check"));
    }
    if do_fix && total_fixes == 0 {
        eprintln!("zz: no fixable diagnostics in `{raw}`");
    }

    // Footer hints in check-only mode.
    if !do_fix && (any_safe_fixits || any_ambiguous) {
        if any_safe_fixits {
            eprintln!("help: run `zz check --fix {raw}` to automatically apply safe fixes");
        }
        if any_ambiguous {
            eprintln!(
                "help: run `zz check --fix -i {raw}` to review ambiguous fixes interactively"
            );
            eprintln!("help: run `zz check --fix --hard {raw}` to force-apply all fixes");
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::PathBuf;

    use super::check_or_fix_path;

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
        let result = check_or_fix_path(
            &Some(path.to_string_lossy().to_string()),
            false,
            false,
            false,
        );
        assert!(result.is_ok(), "expected ok, got {result:?}");
        let _ = fs::remove_file(&path);
    }

    #[test]
    fn check_ok_on_phase4_features() {
        let path = write_temp(
            "scores := [10, 20, 30]\ny := scores[1]\nz := scores[1:3]\nfunc dbl(a: int, b: int) -> int { a * b }\nw := 5 |> dbl(3)\nt := typeof(w)\n",
        );
        let result = check_or_fix_path(
            &Some(path.to_string_lossy().to_string()),
            false,
            false,
            false,
        );
        assert!(result.is_ok(), "expected ok, got {result:?}");
        let _ = fs::remove_file(&path);
    }

    #[test]
    fn check_rejects_type_error() {
        let path = write_temp("x := 1 + \"a\"\n");
        let result = check_or_fix_path(
            &Some(path.to_string_lossy().to_string()),
            false,
            false,
            false,
        );
        assert!(result.is_err(), "expected type error");
        let _ = fs::remove_file(&path);
    }

    #[test]
    fn check_rejects_index_error() {
        let path = write_temp("x := 5\nx[0]\n");
        let result = check_or_fix_path(
            &Some(path.to_string_lossy().to_string()),
            false,
            false,
            false,
        );
        assert!(result.is_err(), "expected index type error");
        let _ = fs::remove_file(&path);
    }

    #[test]
    fn check_missing_file_errors() {
        let result = check_or_fix_path(
            &Some("/tmp/zz_no_such_file_zz.zz".to_string()),
            false,
            false,
            false,
        );
        assert!(result.is_err(), "expected error for missing file");
    }

    #[test]
    fn check_no_arg_errors() {
        // Default path "." should work (current dir).
        let result = check_or_fix_path(&None, false, false, false);
        assert!(result.is_ok(), "default path should scan current dir");
    }
}
