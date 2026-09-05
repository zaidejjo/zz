//! C compilation: turn generated C source into a native binary.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

/// Error surfaced from a build.
#[derive(Debug)]
pub enum BuildError {
    /// No C compiler found on PATH.
    NoCompiler,
    /// The C compiler failed with `stderr`.
    CompileFailed { stderr: String },
    /// Rust-side IO error.
    Io(std::io::Error),
}

impl std::fmt::Display for BuildError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            BuildError::NoCompiler => write!(f, "no C compiler found (tried cc, clang, gcc, tcc)"),
            BuildError::CompileFailed { stderr } => write!(f, "C compile failed:\n{stderr}"),
            BuildError::Io(e) => write!(f, "io error: {e}"),
        }
    }
}

impl From<std::io::Error> for BuildError {
    fn from(e: std::io::Error) -> Self {
        BuildError::Io(e)
    }
}

/// A detected C compiler.
#[derive(Debug, Clone)]
pub struct CCompiler {
    pub name: String,
    pub path: String,
}

/// Probe PATH for a C compiler in preference order.
pub fn detect_cc() -> Option<CCompiler> {
    for name in ["cc", "clang", "gcc", "tcc"] {
        if let Some(path) = which(name) {
            return Some(CCompiler {
                name: name.to_string(),
                path,
            });
        }
    }
    None
}

fn which(name: &str) -> Option<String> {
    let path_var = std::env::var_os("PATH")?;
    for dir in std::env::split_paths(&path_var) {
        let candidate = dir.join(name);
        if candidate.is_file() {
            // On Windows would append .exe; Linux/mac are fine.
            return Some(candidate.to_string_lossy().into_owned());
        }
    }
    None
}

/// Build options.
#[derive(Debug, Clone, Copy, Default)]
pub struct BuildOptions {
    /// Optimization level: 0 = -O1 (dev), 1 = -O3 (release).
    pub optimize: bool,
    /// Strip the binary (`-s`/`-Wl,--strip-all`).
    pub strip: bool,
    /// Static link (release target ≤2MB).
    pub static_link: bool,
    /// Enable function-section + gc-sections (DCE at link level).
    pub gc_sections: bool,
}

impl BuildOptions {
    /// Dev build: fast, debug-friendly.
    pub fn dev() -> Self {
        BuildOptions {
            optimize: false,
            strip: false,
            static_link: false,
            gc_sections: true,
        }
    }

    /// Release build: -O3 -flto, stripped, static, gc-sections.
    pub fn release() -> Self {
        BuildOptions {
            optimize: true,
            strip: true,
            static_link: true,
            gc_sections: true,
        }
    }
}

/// Compile C source (already including the runtime) to a binary at
/// `output_path`.
pub fn build(
    source: &str,
    output_path: &Path,
    opts: BuildOptions,
) -> Result<CCompiler, BuildError> {
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let cc = detect_cc().ok_or(BuildError::NoCompiler)?;
    let uniq = COUNTER.fetch_add(1, Ordering::SeqCst);

    // Write the source to a unique temp .c file (tests run in parallel).
    let tmpdir = std::env::temp_dir().join(format!("zz-build-{}-{uniq}", std::process::id()));
    std::fs::create_dir_all(&tmpdir)?;
    let src_path = tmpdir.join("prog.c");
    std::fs::write(&src_path, source)?;

    let mut cmd = Command::new(&cc.path);
    if opts.optimize {
        cmd.arg("-O3").arg("-flto");
    } else {
        cmd.arg("-O1");
    }
    if opts.strip {
        cmd.arg("-s");
    }
    if opts.static_link {
        cmd.arg("-static");
    }
    if opts.gc_sections {
        cmd.arg("-ffunction-sections").arg("-fdata-sections");
        cmd.arg("-Wl,--gc-sections");
    }
    cmd.arg("-o").arg(output_path).arg(&src_path).arg("-lm");

    let out = cmd.output().map_err(BuildError::Io)?;
    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
        return Err(BuildError::CompileFailed { stderr });
    }
    Ok(cc)
}

/// Execute a compiled binary, capturing stdout + exit code.
pub fn run_binary(path: &Path, args: &[&str]) -> Result<(i32, String), BuildError> {
    let out = Command::new(path)
        .args(args)
        .stdin(Stdio::inherit())
        .output()
        .map_err(BuildError::Io)?;
    Ok((
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stdout).into_owned(),
    ))
}

/// Compile and run in one shot, returning (exit, stdout). Cleans up the
/// binary and temp source afterwards (transient `--native` mode).
pub fn compile_and_run(
    source: &str,
    opts: BuildOptions,
    args: &[&str],
) -> Result<(i32, String), BuildError> {
    let tmpdir = std::env::temp_dir().join(format!("zz-run-{}", std::process::id()));
    std::fs::create_dir_all(&tmpdir)?;
    let bin = tmpdir.join("zz_tmp_bin");
    build(source, &bin, opts)?;
    let r = run_binary(&bin, args);
    let _ = std::fs::remove_file(&bin);
    let _ = std::fs::remove_dir_all(&tmpdir);
    r
}

/// Returns the temp binary path without deleting it (for `zz build`).
pub fn build_to_temp(source: &str, opts: BuildOptions) -> Result<(PathBuf, CCompiler), BuildError> {
    let tmpdir = std::env::temp_dir().join(format!("zz-build-out-{}", std::process::id()));
    std::fs::create_dir_all(&tmpdir)?;
    let bin = tmpdir.join(if cfg!(windows) {
        "zz_out.exe"
    } else {
        "zz_out"
    });
    let cc = build(source, &bin, opts)?;
    Ok((bin, cc))
}
