//! `zz doctor`: environment audit with fix hints.
//!
//! Checks the toolchain end to end — binary, C backend, shell integration
//! (reuses `setup --check` state), git, and registry reachability — and
//! reports one grouped verdict. `zz doctor --fix` auto-runs `zz setup`
//! when shell integration is the problem.

use crate::{setup, ui};

/// One check result.
struct Check {
    step: &'static str,
    ok: bool,
    line: String,
    hint: Option<String>,
}

fn pass(step: &'static str, line: impl Into<String>) -> Check {
    Check {
        step,
        ok: true,
        line: line.into(),
        hint: None,
    }
}

fn fail(step: &'static str, line: impl Into<String>, hint: impl Into<String>) -> Check {
    Check {
        step,
        ok: false,
        line: line.into(),
        hint: Some(hint.into()),
    }
}

/// `zz doctor [--fix]`.
pub fn run(args: &[String]) -> Result<(), String> {
    let fix = args.iter().any(|a| a == "--fix");
    ui::header("zz doctor");
    let mut checks: Vec<Check> = Vec::new();

    // 1. Binary.
    let version = env!("CARGO_PKG_VERSION");
    match std::env::current_exe() {
        Ok(exe) => {
            let dir = exe
                .parent()
                .map(|p| p.display().to_string())
                .unwrap_or_default();
            // Writable install dir = future `zz upgrade` can work. Probe
            // with a real (empty, immediately removed) file: permission
            // bits alone lie on read-only mounts and under ACLs.
            let probe = exe
                .parent()
                .unwrap_or(std::path::Path::new("."))
                .join(".zz-write-test");
            let writable = std::fs::write(&probe, b"").is_ok();
            let _ = std::fs::remove_file(&probe);
            if writable {
                checks.push(pass("Binary", format!("zz v{version} at {dir} (writable)")));
            } else {
                checks.push(fail(
                    "Binary",
                    format!("zz v{version} at {dir} (read-only)"),
                    "upgrade will fail here — reinstall somewhere writable, e.g. ~/.zz/bin",
                ));
            }
        }
        Err(e) => checks.push(fail(
            "Binary",
            "cannot locate zz binary".to_string(),
            format!("current_exe failed: {e}"),
        )),
    }

    // 2. C backend.
    match zz_codegen::detect_clang_with(zz_codegen::ClangProvider::Any) {
        Some(clang) => {
            let ver = std::process::Command::new(&clang.path)
                .arg("--version")
                .output()
                .ok()
                .and_then(|o| String::from_utf8(o.stdout).ok())
                .and_then(|s| s.lines().next().map(str::to_string))
                .unwrap_or_default();
            let detail = if ver.is_empty() {
                clang.label.to_string()
            } else {
                format!("{} ({})", clang.label, ver.trim())
            };
            checks.push(pass("C backend", format!("clang backend: {detail}")));
        }
        None => checks.push(fail(
            "C backend",
            "no clang backend found".to_string(),
            "run `zz toolchain install` for a managed Zig backend, or install clang",
        )),
    }

    // 3. Shell integration (same state as `zz setup --check`).
    let st = setup::status();
    if !st.bin_exists {
        checks.push(fail(
            "Shell PATH",
            format!("{} missing", st.bin.display()),
            "run `zz setup` to create it",
        ));
    } else if !st.on_path {
        if st.shells.iter().all(|s| s.rc_wired) {
            checks.push(pass(
                "Shell PATH",
                "~/.zz/bin wired (restart shell to activate)".to_string(),
            ));
        } else {
            checks.push(fail(
                "Shell PATH",
                "~/.zz/bin not on PATH".to_string(),
                "run `zz setup`, then restart your shell",
            ));
        }
    } else {
        checks.push(pass("Shell PATH", "~/.zz/bin on PATH".to_string()));
    }
    let stale: Vec<&str> = st
        .shells
        .iter()
        .filter(|s| s.rc_wired && !s.completion_installed)
        .map(|s| s.shell)
        .collect();
    if stale.is_empty() {
        checks.push(pass(
            "Completions",
            "shell completions installed".to_string(),
        ));
    } else {
        checks.push(fail(
            "Completions",
            format!("stale completions: {}", stale.join(", ")),
            "run `zz setup` to refresh them",
        ));
    }

    // 4. git (git deps + `zz new` scaffolding).
    match std::process::Command::new("git").arg("--version").output() {
        Ok(out) if out.status.success() => {
            let ver = String::from_utf8_lossy(&out.stdout).trim().to_string();
            checks.push(pass("git", ver));
        }
        _ => checks.push(fail(
            "git",
            "git not found".to_string(),
            "install git — needed for git dependencies and `zz new`",
        )),
    }

    // 5. Registry reachability (short timeout, read-only probe).
    let base = zz_pm::remote::registry_base();
    let reachable = zz_pm::remote::RegistryClient::new(&base)
        .search("zz", 1)
        .is_ok();
    if reachable {
        checks.push(pass("Registry", format!("registry reachable ({base})")));
    } else {
        checks.push(fail(
            "Registry",
            format!("registry unreachable ({base})"),
            "check your network or ZZ_REGISTRY — `zz add`/`install` need it",
        ));
    }

    // 6. Cache size.
    let cache = zz_pm::paths::cache_objects_dir();
    let (entries, bytes) = zz_pm::paths::dir_usage(&cache);
    checks.push(pass(
        "Cache",
        format!(
            "build cache: {entries} entries ({})",
            ui::human_bytes(bytes)
        ),
    ));

    // Report.
    let mut failed = 0u32;
    for (i, c) in checks.iter().enumerate() {
        ui::step(i + 1, checks.len(), c.step);
        if c.ok {
            ui::ok(&c.line);
        } else {
            failed += 1;
            ui::warn(&c.line);
            if let Some(h) = &c.hint {
                eprintln!("      hint: {h}");
            }
        }
    }

    if failed == 0 {
        println!("doctor: all {} checks passed", checks.len());
        return Ok(());
    }
    if fix {
        let integration_broken = !st.bin_exists
            || (!st.on_path && !st.shells.iter().all(|s| s.rc_wired))
            || st
                .shells
                .iter()
                .any(|s| s.rc_wired && !s.completion_installed);
        if integration_broken {
            ui::header("doctor --fix: running setup");
            setup::run(&[])?;
            println!("doctor: re-run `zz doctor` after restarting your shell");
            return Ok(());
        }
    }
    Err(format!(
        "doctor: {failed} of {} checks failed",
        checks.len()
    ))
}

#[cfg(test)]
mod tests {
    use zz_pm::paths::dir_usage;

    #[test]
    fn usage_counts_files() {
        let dir = std::env::temp_dir().join(format!("zz_doctor_test_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("sub")).unwrap();
        std::fs::write(dir.join("a"), "1234").unwrap();
        std::fs::write(dir.join("sub").join("b"), "123456").unwrap();
        let (entries, bytes) = dir_usage(&dir);
        assert_eq!(entries, 2);
        assert_eq!(bytes, 10);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn usage_missing_dir_is_empty() {
        assert_eq!(
            dir_usage(std::path::Path::new("/nonexistent-zz-dir")),
            (0, 0)
        );
    }
}
