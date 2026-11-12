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
    /// Host-native codegen: `-march=native` (AVX2/AVX-512/BMI2, etc.) for
    /// builds that run on the same machine they compile on. Only honored
    /// when there is no `--target` (cross builds keep portable codegen).
    pub native_cpu: bool,
    /// Path to an LLVM profdata file for PGO use (`-fprofile-use=<path>`).
    /// When set with `pgo == PgoMode::Use`, the build links against this
    /// profile instead of the default `./default.profdata`. Opt-in via
    /// `ZZ_PGO_PROFILE`; default behavior unchanged.
    pub profile_path: Option<PathBuf>,
    /// Relaxed-float codegen: `-ffast-math` (+`-fno-trapping-math`) for host
    /// builds. Folds NaN guards and reassociates floats, so it breaks
    /// specified float semantics — opt-in via `ZZ_RELAXED_FLOAT=1` only,
    /// never on cross builds (`--target`) where it could change results on
    /// other CPUs / with other toolchains.
    pub relaxed_float: bool,
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
            native_cpu: false,
            profile_path: None,
            relaxed_float: false,
        }
    }

    pub fn release() -> Self {
        BuildOptions {
            optimize: true,
            strip: true,
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
            native_cpu: false,
            profile_path: None,
            relaxed_float: false,
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
            native_cpu: false,
            profile_path: None,
            relaxed_float: false,
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
            native_cpu: false,
            profile_path: None,
            relaxed_float: false,
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
            native_cpu: true,
            profile_path: None,
            relaxed_float: false,
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
            native_cpu: true,
            profile_path: None,
            relaxed_float: false,
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
            native_cpu: false,
            profile_path: None,
            relaxed_float: false,
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
    /// AddressSanitizer/UBSan are incompatible with this build mode:
    /// static linking, full LTO, or `zig cc` each produce unresolved ASan
    /// runtime symbols at link time. Use `--dynamic` (thin LTO is fine) or
    /// `--cc clang`.
    SanitizerUnsupported { reason: String },
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
            BuildError::SanitizerUnsupported { reason } => {
                write!(
                    f,
                    "sanitizers (ZZ_SANITIZE) are incompatible with this build: {reason}\n\
                     hint: use `--dynamic` (thin LTO + sanitizers is supported) or \
                     `--cc clang`; see `zz build --help`"
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

/// C reserved keywords that must not be emitted as bare symbol name
/// suffixes (they would collide with the language itself).
const C_KEYWORDS: &[&str] = &[
    "auto",
    "break",
    "case",
    "char",
    "const",
    "continue",
    "default",
    "do",
    "double",
    "else",
    "enum",
    "extern",
    "float",
    "for",
    "goto",
    "if",
    "inline",
    "int",
    "long",
    "register",
    "restrict",
    "return",
    "short",
    "signed",
    "sizeof",
    "static",
    "struct",
    "switch",
    "typedef",
    "union",
    "unsigned",
    "void",
    "volatile",
    "while",
    "_Alignas",
    "_Alignof",
    "_Atomic",
    "_Bool",
    "_Complex",
    "_Generic",
    "_Imaginary",
    "_Noreturn",
    "_Static_assert",
    "_Thread_local",
    "main",
];

/// Runtime symbol prefixes reserved for the embedded C runtime. User
/// symbols already carry `zz_g_`, `zz_chunk_`, or `zz_e_` prefixes, so this
/// guards the mangled suffix against colliding with runtime helpers like
/// `zz_int`, `zz_float`, `zz_arena`, etc.
const RUNTIME_SYMBOLS: &[&str] = &[
    "zz_int",
    "zz_float",
    "zz_bool",
    "zz_str",
    "zz_array",
    "zz_dict",
    "zz_arena",
    "zz_value",
    "zz_str_new",
    "zz_str_static",
    "zz_str_get",
    "zz_str_set",
    "zz_str_len",
    "zz_str_char_len",
    "zz_str_ptr",
    "zz_array_new",
    "zz_array_push",
    "zz_array_get",
    "zz_array_set",
    "zz_dict_new",
    "zz_dict_get",
    "zz_dict_set",
    "zz_index_get",
    "zz_index_trap",
    "zz_to_str",
    "zz_to_str_fmt",
    "zz_binop_cat",
    "zz_clone",
    "zz_release",
    "zz_unimplemented_native",
    "zz_float_format_raw",
    "zz_str_lower",
    "zz_str_upper",
    "zz_str_replace",
];

/// Check whether `symbol` collides with a C keyword or reserved runtime
/// symbol. Returns `true` if a suffix `_` should be appended to avoid the
/// collision.
fn is_reserved_symbol(symbol: &str) -> bool {
    if C_KEYWORDS.contains(&symbol) {
        return true;
    }
    if RUNTIME_SYMBOLS.contains(&symbol) {
        return true;
    }
    // C reserves identifiers beginning with `_` followed by an uppercase
    // letter or another underscore, and identifiers beginning with `__`.
    let bytes = symbol.as_bytes();
    if bytes.len() >= 2 && bytes[0] == b'_' && (bytes[1] == b'_' || bytes[1].is_ascii_uppercase()) {
        return true;
    }
    false
}

/// Append `_` to `symbol` until it no longer collides with a reserved
/// symbol. This guarantees user-defined names never shadow C keywords or
/// runtime helpers.
fn reserve_symbol(mut symbol: String) -> String {
    while is_reserved_symbol(&symbol) {
        if symbol.starts_with('_') {
            // Insert a character at the front to break the reserved `__` or
            // `_<uppercase>` prefix that C reserves in all scopes.
            symbol.insert(0, 'z');
        } else {
            symbol.push('_');
        }
    }
    symbol
}

/// Mangle a dotted ZZ name into a C identifier.
///
/// The mapping is invertible:
/// - `_` → `__` (escape underscore)
/// - `.` → `_` (dot becomes single underscore)
/// - ASCII alphanumeric → itself
/// - any other char → `_u{HEX}_` (reversible hex escape)
///
/// This guarantees `mangle("a.b") != mangle("a__b")`, preventing C symbol
/// collisions between dotted paths and bare names containing underscores.
/// The result is also checked against C keywords and runtime symbols so
/// that user-defined names never shadow reserved identifiers.
pub fn mangle(name: &str) -> String {
    let mut out = String::with_capacity(name.len());
    for c in name.chars() {
        match c {
            '_' => out.push_str("__"),
            '.' => out.push('_'),
            c if c.is_ascii_alphanumeric() => out.push(c),
            c => {
                out.push('_');
                out.push('u');
                out.push_str(&format!("{:04x}", c as u32));
                out.push('_');
            }
        }
    }
    reserve_symbol(out)
}

/// Recover the dotted ZZ name from a C identifier produced by [`mangle`].
///
/// This is the inverse of [`mangle`] for module-qualified identifiers (the
/// shape used for struct names, e.g. `mod.Type` → `mod_Type`). It undoes the
/// reserved-symbol suffix appended by `reserve_symbol` on a best-effort
/// basis; names that `reserve_symbol` had to prefix with `z` (only possible
/// for identifiers beginning with `_`, which ZZ reserves for the wildcard)
/// are not recovered.
///
/// Round-trips every `mangle` output for names made of alphanumeric
/// characters, `.` and `_`.
pub fn demangle(mangled: &str) -> String {
    let bytes = mangled.as_bytes();
    // A single trailing `_` is the reserved-symbol suffix appended by
    // `reserve_symbol` (`int` → `int_`); an original trailing `_` mangles to
    // `__`, so a lone `_` is unambiguous.
    let end =
        if bytes.len() >= 2 && bytes[bytes.len() - 1] == b'_' && bytes[bytes.len() - 2] != b'_' {
            bytes.len() - 1
        } else {
            bytes.len()
        };

    let mut out = String::with_capacity(end);
    let mut i = 0;
    while i < end {
        if bytes[i] != b'_' {
            out.push(bytes[i] as char);
            i += 1;
            continue;
        }
        // `_uXXXX_` reversible hex escape (7 chars).
        if i + 6 < end
            && bytes[i + 1] == b'u'
            && bytes[i + 6] == b'_'
            && bytes[i + 2..i + 6].iter().all(u8::is_ascii_hexdigit)
        {
            let hex = std::str::from_utf8(&bytes[i + 2..i + 6]).unwrap_or("0");
            if let Some(c) = u32::from_str_radix(hex, 16).ok().and_then(char::from_u32) {
                out.push(c);
                i += 7;
                continue;
            }
        }
        // `__` is an escaped underscore; a lone `_` is a path separator.
        if i + 1 < end && bytes[i + 1] == b'_' {
            out.push('_');
            i += 2;
        } else {
            out.push('.');
            i += 1;
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mangle_is_invertible_for_dotted_vs_underscore_names() {
        // The core bug: `a.b` and `a__b` must not collide.
        assert_ne!(mangle("a.b"), mangle("a__b"));
        assert_ne!(mangle("a.b.c"), mangle("a__b__c"));
        assert_ne!(mangle("foo.bar.baz"), mangle("foo__bar__baz"));
    }

    #[test]
    fn mangle_preserves_alphanumeric_names() {
        assert_eq!(mangle("hello"), "hello");
        assert_eq!(mangle("hello123"), "hello123");
        // A lone `_` mangles to `__` which C reserves, so it gets prefixed.
        assert_eq!(mangle("_"), "z__");
        assert_eq!(mangle("__"), "z____");
    }

    #[test]
    fn mangle_dots_become_single_underscore() {
        assert_eq!(mangle("a.b"), "a_b");
        assert_eq!(mangle("a.b.c"), "a_b_c");
        assert_eq!(mangle("mod.Type.method"), "mod_Type_method");
    }

    #[test]
    fn demangle_inverts_mangle_for_qualified_identifiers() {
        for name in [
            "Pt",
            "mod.Pt",
            "bench_struct_access.Pt",
            "a_b",
            "a__b",
            "a.b",
            "a.b.c",
            "mod.Type.method",
            "a.b_c",
            "a_b.c",
            "hello123",
        ] {
            assert_eq!(
                demangle(&mangle(name)),
                name,
                "round-trip failed for {name}"
            );
        }
    }

    #[test]
    fn demangle_undoes_reserved_suffix() {
        // `reserve_symbol` appends `_` to C keywords; demangle strips it.
        assert_eq!(demangle(&mangle("int")), "int");
        assert_eq!(demangle(&mangle("main")), "main");
        // A genuine trailing underscore mangles to `__` and survives.
        assert_eq!(demangle(&mangle("mod.int_")), "mod.int_");
    }

    #[test]
    fn mangle_underscores_become_double() {
        assert_eq!(mangle("a_b"), "a__b");
        assert_eq!(mangle("a__b"), "a____b");
        assert_eq!(mangle("a_b_c"), "a__b__c");
    }

    #[test]
    fn mangle_reserved_symbols_get_suffix() {
        // C keywords get a trailing `_` after mangling.
        assert_eq!(mangle("main"), "main_");
        assert_eq!(mangle("int"), "int_");
        assert_eq!(mangle("struct"), "struct_");
        // Runtime symbols with underscores get the underscore escaped to `__`,
        // which already avoids collision (e.g. `zz__int` != `zz_int`).
        assert_eq!(mangle("zz_int"), "zz__int");
        assert_eq!(mangle("zz_float"), "zz__float");
    }

    #[test]
    fn mangle_non_ascii_gets_hex_escape() {
        // Non-ASCII characters get a reversible `_u{HEX}_` escape.
        let mangled = mangle("café");
        assert!(mangled.contains("_u00e9_"));
        assert_ne!(mangled, "café");
    }

    #[test]
    fn mangle_no_double_underscore_prefix() {
        // C reserves identifiers starting with `__` or `_` + uppercase.
        // Mangling `__foo` → `____foo`, which starts with `__`; reserve_symbol
        // prefixes with `z` to break the reserved pattern.
        let mangled = mangle("__foo");
        assert!(!mangled.starts_with("__"));
        let mangled = mangle("_Foo");
        assert!(!mangled.starts_with("_F"));
    }
}
