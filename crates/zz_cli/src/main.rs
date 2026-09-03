//! ZZ CLI entry point.
//!
//! Phase 1:
//! - `zz`            → interactive REPL
//! - `zz eval <src>` → evaluate source once and print the result
//! - `zz run <file>` → type-check and run a `.zz` file
//! - `zz check <file>` → parse and type-check a `.zz` file without running it
//! - `zz --help`     → usage

use std::process::ExitCode;

mod build;
mod loader;
mod repl;
mod session;

use zz_frontend::diag::{error_at, render_to_string, Files};
use zz_frontend::span::Span;
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
    zz fmt [FLAGS] [PATH]         format ZZ source files in-place

FLAGS:
    --check, -c       check formatting without writing (exit 1 if changed)
    --fix, -f          apply safe auto-fixes (typo replacements, field corrections)
    --hard             with --fix, apply ALL fixes including ambiguous ones (no prompts)
    --interactive, -i  with --fix, prompt for ambiguous fixes interactively
    --native           with run, use the native AOT compiler instead of the VM
    -p, --release      with build, full optimization (-O3 -flto, DCE, stripped)
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
    zz fmt .                         format all .zz files in current directory
    zz fmt -c src/                   check formatting without writing
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
        Some("run") => {
            let native = rest.iter().any(|a| a == "--native");
            let args: Vec<String> = rest.iter().filter(|a| *a != "--native").cloned().collect();
            let script_args = args.get(1..).unwrap_or(&[]).to_vec();
            let file = args.iter().find(|a| !a.starts_with('-'));
            if native {
                match run_native(file, &script_args) {
                    Ok(()) => ExitCode::SUCCESS,
                    Err(msg) => {
                        eprintln!("zz: {msg}");
                        ExitCode::FAILURE
                    }
                }
            } else {
                match run_file(rest.first(), &script_args) {
                    Ok(()) => ExitCode::SUCCESS,
                    Err(msg) => {
                        eprintln!("zz: {msg}");
                        ExitCode::FAILURE
                    }
                }
            }
        }
        Some("build") => match build_cmd(rest) {
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
        Some("fmt") => {
            let (path, flags) = parse_path_and_flags(rest);
            let check_only =
                flags.contains(&"--check".to_string()) || flags.contains(&"-c".to_string());
            match fmt_path(&path, check_only) {
                Ok(changed) => {
                    if check_only && changed {
                        ExitCode::FAILURE
                    } else {
                        ExitCode::SUCCESS
                    }
                }
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
                let mut diag = error_at(e.message.clone(), e.span);
                for (name, _span) in &e.backtrace {
                    if !name.is_empty() {
                        diag = diag.with_note(format!("  at {name}"));
                    }
                }
                let diags = vec![diag];
                eprint!("{}", render_to_string(&files, id, &diags));
                return Err("program failed".to_string());
            }
        }
    }
    if last != Value::Unit {
        println!("{last}");
    }

    // Auto-call `main()` if defined in the entry file.
    // The entry file's namespace is its file stem (e.g. `myapp.zz` → `myapp`).
    let entry_ns = std::path::Path::new(path)
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    let main_key = format!("{entry_ns}.main");
    if let Some(fv) = interp.funcs.get(&main_key).cloned() {
        let span = Span::new(0, 0);
        // Pass script args only if main() accepts parameters.
        let call_args = if fv.params.is_empty() {
            vec![]
        } else {
            vec![Value::Array(Box::new(
                script_args
                    .iter()
                    .map(|a| Value::Str(a.clone().into()))
                    .collect(),
            ))]
        };
        match interp.call(Value::Func(Box::new(fv)), call_args, span) {
            Ok(_) => {}
            Err(e) => {
                let mut files = Files::new();
                let id = files.add(path.clone(), String::new());
                let mut diag = error_at(e.message.clone(), e.span);
                for (name, _) in &e.backtrace {
                    if !name.is_empty() {
                        diag = diag.with_note(format!("  at {name}"));
                    }
                }
                let diags = vec![diag];
                eprint!("{}", render_to_string(&files, id, &diags));
                return Err("program failed".to_string());
            }
        }
    }

    Ok(())
}

/// `zz run --native <file>`: compile to a temp location, execute, cleanup.
fn run_native(path: Option<&String>, script_args: &[String]) -> Result<(), String> {
    let path = path
        .ok_or_else(|| "missing file argument\n\nusage: zz run --native <file.zz>".to_string())?;
    let p = std::path::Path::new(path);
    // Use cache when possible (fast re-run).
    let cached = build::build_native(p, build::BuildMode::Dev)?;
    let code = build::exec_binary(&cached, script_args)?;
    if code != 0 {
        return Err(format!("native program exited with code {code}"));
    }
    Ok(())
}

/// `zz build [-p] <file>`: compile a native binary (cached).
fn build_cmd(args: &[String]) -> Result<(), String> {
    let release = args.iter().any(|a| a == "-p" || a == "--release");
    let path = args
        .iter()
        .find(|a| !a.starts_with('-'))
        .ok_or_else(|| "missing file argument\n\nusage: zz build [-p] <file.zz>".to_string())?;
    let p = std::path::Path::new(path);
    let mode = if release {
        build::BuildMode::Release
    } else {
        build::BuildMode::Dev
    };
    let out = build::build_native(p, mode)?;
    // Copy the cached binary next to the source (zz build intent).
    let stem = p
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    let dest = p.with_file_name(format!("{stem}"));
    std::fs::copy(&out, &dest).map_err(|e| format!("cannot write binary: {e}"))?;
    let meta = std::fs::metadata(&dest).map(|m| m.len()).unwrap_or(0);
    println!(
        "built {} ({}, {:.1} KB)",
        dest.display(),
        if release { "release" } else { "dev" },
        meta as f64 / 1024.0
    );
    Ok(())
}

/// Format all `.zz` files under a path.  Returns `Ok(true)` when at
/// least one file was changed (useful for `--check` mode).
fn fmt_path(path_arg: &Option<String>, check_only: bool) -> Result<bool, String> {
    let raw = path_arg.as_deref().unwrap_or(".");
    let base = std::path::Path::new(raw);
    let files = collect_zz_files(base)?;

    if files.is_empty() {
        return Err(format!("no .zz files found in `{raw}`"));
    }

    let config = zz_frontend::FormatConfig::default();
    let mut changed_any = false;

    for path in &files {
        let path_str = path.display().to_string();
        let source =
            std::fs::read_to_string(path).map_err(|e| format!("cannot read `{path_str}`: {e}"))?;

        let parsed = zz_frontend::parse(&source);
        let formatted = zz_frontend::format_program(&parsed.program, &source, &config);

        if formatted != source {
            changed_any = true;
            if check_only {
                eprintln!("would reformat: {path_str}");
            } else {
                std::fs::write(path, &formatted)
                    .map_err(|e| format!("cannot write `{path_str}`: {e}"))?;
                eprintln!("reformatted: {path_str}");
            }
        }
    }

    if check_only && changed_any {
        eprintln!(
            "zz: {} file(s) need formatting (use `zz fmt` without --check to fix)",
            files.len()
        );
    } else if !changed_any {
        eprintln!("zz: all files already formatted");
    }

    Ok(changed_any)
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
        // Collect all approved fixits (safe + accepted ambiguous) into one list.
        let mut approved: Vec<zz_frontend::diag::FixIt> = safe_fixits;

        // Handle ambiguous fixes based on mode.
        if !ambiguous_fixits.is_empty() {
            if force {
                // --hard: auto-apply all ambiguous fixes.
                approved.extend(ambiguous_fixits);
            } else if interactive {
                // -i / --interactive: prompt user for each ambiguous fix.
                for fixit in &ambiguous_fixits {
                    let start = fixit.span.start as usize;
                    let end = fixit.span.end as usize;
                    if end > source.len() || start >= end {
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
                                let mut chosen_fixit = fixit.clone();
                                chosen_fixit.replacement = fixit.alternatives[n - 1].clone();
                                approved.push(chosen_fixit);
                                eprintln!(
                                    "    applied: `{original}` → `{}`",
                                    fixit.alternatives[n - 1]
                                );
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
                            approved.push(fixit.clone());
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

        // Apply all approved fixits in one pass, right-to-left to avoid offset shifts.
        let mut applied = 0u32;
        if !approved.is_empty() {
            let mut new_source = source.clone();
            approved.sort_by(|a, b| b.span.start.cmp(&a.span.start));
            for fixit in &approved {
                let start = fixit.span.start as usize;
                let end = fixit.span.end as usize;
                if end <= new_source.len() && start < end {
                    let original = source[start..end].to_string();
                    new_source.replace_range(start..end, &fixit.replacement);
                    applied += 1;
                    let label = if fixit.safety == zz_frontend::diag::FixSafety::Safe {
                        "fixed"
                    } else {
                        "fixed (force)"
                    };
                    eprintln!(
                        "  {label}: `{original}` → `{}` at {path_str}:{}",
                        fixit.replacement, fixit.span.start,
                    );
                }
            }

            if applied > 0 {
                std::fs::write(path, &new_source)
                    .map_err(|e| format!("cannot write `{path_str}`: {e}"))?;
                eprintln!("zz: applied {applied} fix(es) to `{path_str}`");
                total_fixes += applied;
            }
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
        let examples_dir =
            std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../examples");
        assert!(examples_dir.is_dir(), "examples dir should exist");
        let result = check_or_fix_path(
            &Some(examples_dir.display().to_string()),
            false,
            false,
            false,
        );
        // The function may fail type-check on examples; the point is it
        // should find files and not panic/IO-error.
        match &result {
            Err(msg) if msg.contains("does not exist") || msg.contains("no .zz files") => {
                panic!("scan should find files: {msg}");
            }
            _ => {} // either Ok or type-check errors — both prove scanning worked.
        }
    }
}
