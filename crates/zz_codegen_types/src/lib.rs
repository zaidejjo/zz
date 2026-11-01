//! Shared types for the ZZ native codegen pipeline.
//!
//! This is a leaf crate with no `zz_` dependencies, so every codegen
//! sub-crate can depend on it without creating cycles.

use std::path::PathBuf;

/// Optimization level: false = `-O0 -g` (dev inspection), true = release.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct BuildOptions {
    pub optimize: bool,
    pub strip: bool,
    pub static_link: bool,
    pub gc_sections: bool,
    pub thin_lto: bool,
    pub pgo: PgoMode,
    pub native_rt: bool,
    pub pg_link: bool,
    pub float_link: bool,
    pub curl_link: bool,
    pub sqlite_link: bool,
    pub allow_static_downgrade: bool,
    pub full_lto: bool,
    pub plugin_artifacts: Vec<PathBuf>,
    pub plugin_link_args: Vec<String>,
    pub embed_assets: Vec<EmbedAsset>,
    pub verbose: bool,
}

impl BuildOptions {
    pub fn dev() -> Self {
        BuildOptions {
            optimize: false,
            strip: false,
            static_link: false,
            gc_sections: true,
            thin_lto: false,
            pgo: PgoMode::None,
            native_rt: false,
            pg_link: false,
            float_link: false,
            curl_link: false,
            sqlite_link: false,
            allow_static_downgrade: false,
            full_lto: false,
            plugin_artifacts: Vec::new(),
            plugin_link_args: Vec::new(),
            embed_assets: Vec::new(),
            verbose: false,
        }
    }

    pub fn release() -> Self {
        BuildOptions {
            optimize: true,
            strip: true,
            static_link: false,
            gc_sections: true,
            thin_lto: true,
            pgo: PgoMode::None,
            native_rt: false,
            pg_link: false,
            float_link: false,
            curl_link: false,
            sqlite_link: false,
            allow_static_downgrade: false,
            full_lto: false,
            plugin_artifacts: Vec::new(),
            plugin_link_args: Vec::new(),
            embed_assets: Vec::new(),
            verbose: false,
        }
    }

    pub fn static_lto() -> Self {
        BuildOptions {
            optimize: true,
            strip: true,
            static_link: true,
            gc_sections: true,
            thin_lto: true,
            pgo: PgoMode::None,
            native_rt: false,
            pg_link: false,
            float_link: false,
            curl_link: false,
            sqlite_link: false,
            allow_static_downgrade: false,
            full_lto: false,
            plugin_artifacts: Vec::new(),
            plugin_link_args: Vec::new(),
            embed_assets: Vec::new(),
            verbose: false,
        }
    }

    pub fn pgo_generate() -> Self {
        BuildOptions {
            optimize: true,
            strip: false,
            static_link: false,
            gc_sections: true,
            thin_lto: true,
            pgo: PgoMode::Generate,
            native_rt: false,
            pg_link: false,
            float_link: false,
            curl_link: false,
            sqlite_link: false,
            allow_static_downgrade: false,
            full_lto: false,
            plugin_artifacts: Vec::new(),
            plugin_link_args: Vec::new(),
            embed_assets: Vec::new(),
            verbose: false,
        }
    }

    pub fn full() -> Self {
        BuildOptions {
            optimize: true,
            strip: true,
            static_link: false,
            gc_sections: true,
            thin_lto: true,
            pgo: PgoMode::None,
            native_rt: false,
            pg_link: false,
            float_link: false,
            curl_link: false,
            sqlite_link: false,
            allow_static_downgrade: false,
            full_lto: true,
            plugin_artifacts: Vec::new(),
            plugin_link_args: Vec::new(),
            embed_assets: Vec::new(),
            verbose: false,
        }
    }

    pub fn full_pgo_use() -> Self {
        BuildOptions {
            optimize: true,
            strip: true,
            static_link: false,
            gc_sections: true,
            thin_lto: true,
            pgo: PgoMode::Use,
            native_rt: false,
            pg_link: false,
            float_link: false,
            curl_link: false,
            sqlite_link: false,
            allow_static_downgrade: false,
            full_lto: true,
            plugin_artifacts: Vec::new(),
            plugin_link_args: Vec::new(),
            embed_assets: Vec::new(),
            verbose: false,
        }
    }

    pub fn pgo_use() -> Self {
        BuildOptions {
            optimize: true,
            strip: true,
            static_link: false,
            gc_sections: true,
            thin_lto: true,
            pgo: PgoMode::Use,
            native_rt: false,
            pg_link: false,
            float_link: false,
            curl_link: false,
            sqlite_link: false,
            allow_static_downgrade: false,
            full_lto: false,
            plugin_artifacts: Vec::new(),
            plugin_link_args: Vec::new(),
            embed_assets: Vec::new(),
            verbose: false,
        }
    }
}

/// One embedded asset: virtual path (slash-separated, relative) + bytes.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct EmbedAsset {
    pub name: String,
    pub bytes: Vec<u8>,
}

/// PGO mode for profile-guided optimization (native only).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub enum PgoMode {
    /// No PGO - normal compilation.
    #[default]
    None,
    /// Generate profile data (`-fprofile-generate`).
    Generate,
    /// Use collected profile data (`-fprofile-use`).
    Use,
}

/// Provider preference for Clang detection (`--cc` flag).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum ClangProvider {
    /// Probe `clang`, then `clang-23`, `clang-22`, then `zig`.
    #[default]
    Any,
    /// Only accept a `clang*` binary.
    Clang,
    /// Only accept `zig` (invoked as `zig cc`).
    Zig,
}

impl ClangProvider {
    /// Parse the `--cc=<name>` flag value.
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "clang" => Some(ClangProvider::Clang),
            "zig" => Some(ClangProvider::Zig),
            "any" | "auto" => Some(ClangProvider::Any),
            _ => None,
        }
    }
}

/// A detected Clang provider.
#[derive(Debug, Clone)]
pub struct Clang {
    /// Full path to the binary (`clang` or `zig`).
    pub path: PathBuf,
    /// True when the provider is `zig` (invoked as `zig cc ...`).
    pub zig: bool,
    /// Human-readable label for build output (`clang` / `zig cc`).
    pub label: &'static str,
}

impl Clang {
    /// One-line compiler version for build logs (`clang version 19.1.2
    /// ...` / `0.14.0` for zig; `unknown` when the probe fails). Used by
    /// the sanitizer CI leg to pin + log the exact toolchain.
    pub fn version(&self) -> String {
        let mut cmd = std::process::Command::new(&self.path);
        if self.zig {
            cmd.arg("version");
        } else {
            cmd.arg("--version");
        }
        cmd.output()
            .ok()
            .and_then(|o| {
                String::from_utf8(o.stdout)
                    .ok()
                    .and_then(|s| s.lines().next().map(|l| l.trim().to_string()))
                    .filter(|l| !l.is_empty())
            })
            .unwrap_or_else(|| "unknown".to_string())
    }
}

/// Errors produced by the codegen build pipeline.
#[derive(Debug)]
pub enum BuildError {
    /// No Clang provider found on PATH.
    NoClang,
    /// `--pgo` was combined with a foreign `--target`.
    PgoCross,
    /// `--static` was requested for a macOS target.
    StaticMacos,
    /// The C compiler failed with `stderr`.
    CompileFailed { stderr: String },
    /// The precompiled C runtime archive could not be produced.
    RuntimeArchive { reason: String },
    /// The Rust native runtime could not be built or linked.
    NativeRt { reason: String },
    /// Rust-side IO error.
    Io(std::io::Error),
    /// A reachable stdlib native has no C runtime implementation and is not
    /// an intentional VM-only gap. The build fails before clang with the
    /// offending names listed (#715).
    UnimplementedNative { names: Vec<String> },
}

impl std::fmt::Display for BuildError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            BuildError::NoClang => write!(
                f,
                "no clang found (tried clang, clang-23, clang-22, zig); \
                 install clang 18+ or zig for -p builds"
            ),
            BuildError::PgoCross => write!(f, "Error: --pgo requires a native build target"),
            BuildError::StaticMacos => write!(
                f,
                "Error: Static binaries are not supported on macOS targets"
            ),
            BuildError::CompileFailed { stderr } => {
                write!(f, "C compile failed:\n{stderr}")?;
                if stderr.contains("pthread.h") {
                    write!(
                        f,
                        "\nhint: the AOT C runtime is POSIX-thread based, so native \
                         builds are not supported on Windows yet; `zz run`, `zz check` \
                         and `zz test` run on the VM there"
                    )?;
                }
                Ok(())
            }
            BuildError::RuntimeArchive { reason } => write!(
                f,
                "cannot build the C runtime archive: {reason}\n\
                 hint: `zz cache clean` rebuilds it"
            ),
            BuildError::NativeRt { reason } => write!(f, "native runtime link failed: {reason}"),
            BuildError::Io(e) => write!(f, "io error: {e}"),
            BuildError::UnimplementedNative { names } => {
                write!(
                    f,
                    "AOT build blocked: {} reachable stdlib native(s) have no C \
                     runtime implementation and are not intentional VM-only gaps: {}",
                    names.len(),
                    names.join(", ")
                )?;
                write!(
                    f,
                    "\n\nUse the VM (`zz run` / `zz test`) for these, or port them to the C \
                     runtime (native_impl + ffi_impl) before `-p` / `zz build`.\n\
                     Tracked VM-only gaps: std.http.listen_tls, std.http.fetch_insecure, \
                     std.http.hijack, std.sqlz.mysql.*, and pure-ZZ stdlib functions."
                )
            }
        }
    }
}

impl From<std::io::Error> for BuildError {
    fn from(e: std::io::Error) -> Self {
        BuildError::Io(e)
    }
}

/// Progress notifications from the codegen pipeline (#629).
///
/// The native backend reports *what it is doing* instead of leaving the
/// caller to guess: the CLI renders these as build steps, everything else
/// passes [`NoopSink`]. Every method has a no-op default so an
/// implementation only overrides the boundaries it cares about.
///
/// Contract: methods are called from the thread driving the build, except
/// [`CodegenSink::file_done`], which is called from the compiler worker
/// pool — implementations must be `Sync` and must not block for long.
pub trait CodegenSink: Sync {
    /// Generated code is being produced from the typed program.
    fn generating(&self) {}
    /// Code generation finished, yielding `files` translation units.
    fn generated(&self, _files: usize) {}
    /// Compilation starts: `total` units, of which `cached` already have a
    /// reusable object on disk.
    fn compiling(&self, _total: usize, _cached: usize) {}
    /// One translation unit finished (compiled now or served from cache).
    /// `done` is 1-based and monotonically increasing up to `total`.
    fn file_done(&self, _done: usize) {}
    /// Objects are being linked into the output binary.
    fn linking(&self) {}
    /// The binary exists and is executable.
    fn linked(&self) {}
}

/// Sink that ignores every notification; used by every caller that does not
/// render build progress (`zz run --native`, tests, benchmarks, plugins).
pub struct NoopSink;

impl CodegenSink for NoopSink {}

/// Mangle a dotted ZZ name into a C identifier: replace `.` with `__` and
/// any non-alphanumeric/underscore character with `_`.
pub fn mangle(name: &str) -> String {
    name.replace('.', "__")
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect()
}
