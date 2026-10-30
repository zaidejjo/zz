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
        }
    }
}

impl From<std::io::Error> for BuildError {
    fn from(e: std::io::Error) -> Self {
        BuildError::Io(e)
    }
}

/// Content hash of the embedded C runtime sources, stamped by build.rs
/// (`ZZ_RUNTIME_HASH`). Referenced so the hash is load-bearing: any runtime
/// edit changes this constant, which changes the crate fingerprint and
/// forces a rebuild + fresh embed. Without it, `include_str!` edits were
/// silently ignored until an unrelated Rust change rebuilt the crate.
pub const RUNTIME_SRC_HASH: &str = match option_env!("ZZ_RUNTIME_HASH") {
    Some(h) => h,
    None => "0000000000000000",
};

/// Version marker for generated binaries.
pub const C_BUILD_VERSION: &str = "0.1.0";

/// Embedded C runtime header source (concatenated from `src/runtime/*.h`).
pub const RUNTIME_H: &str = concat!(
    include_str!("runtime/runtime.h"),
    include_str!("runtime/platform.h"),
    include_str!("runtime/core.h"),
    include_str!("runtime/memory.h"),
    include_str!("runtime/strings.h"),
    include_str!("runtime/collections.h"),
    include_str!("runtime/json.h"),
);

/// Embedded C runtime implementation source (concatenated from
/// `src/runtime/*.c`).
pub const RUNTIME_C: &str = concat!(
    include_str!("runtime/memory.c"),
    include_str!("runtime/strings.c"),
    include_str!("runtime/collections.c"),
    include_str!("runtime/json.c"),
    include_str!("runtime/core.c"),
);

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

/// Map a zz native qualified name to its C runtime implementation name.
pub fn native_impl(name: &str) -> Option<&'static str> {
    match name {
        // Builtin console I/O (no import required).
        "println" => Some("zz_io_println"),
        "print" => Some("zz_io_print"),
        "input" => Some("zz_io_input"),
        // Test assertions — success returns unit, failure aborts with
        // `error:` on stderr (mirrors the VM's EvalError).
        // Three spellings: bare (no import), `test.*` (via `import std.test`),
        // `std.test.*` (canonical).
        "assert" | "test.assert" | "std.test.assert" => Some("zz_assert"),
        "assert_eq" | "test.assert_eq" | "std.test.assert_eq" => Some("zz_assert_eq"),
        "assert_ne" | "test.assert_ne" | "std.test.assert_ne" => Some("zz_assert_ne"),
        "assert_approx_eq" | "test.assert_approx_eq" | "std.test.assert_approx_eq" => {
            Some("zz_assert_approx_eq")
        }
        "fail" | "panic" | "test.fail" | "test.panic" | "std.test.fail" | "std.test.panic" => {
            Some("zz_fail")
        }
        "assert_contains" | "test.assert_contains" | "std.test.assert_contains" => {
            Some("zz_assert_contains")
        }
        "assert_starts_with" | "test.assert_starts_with" | "std.test.assert_starts_with" => {
            Some("zz_assert_starts_with")
        }
        "assert_ends_with" | "test.assert_ends_with" | "std.test.assert_ends_with" => {
            Some("zz_assert_ends_with")
        }
        "assert_len" | "test.assert_len" | "std.test.assert_len" => Some("zz_assert_len"),
        "assert_gt" | "test.assert_gt" | "std.test.assert_gt" => Some("zz_assert_gt"),
        "assert_lt" | "test.assert_lt" | "std.test.assert_lt" => Some("zz_assert_lt"),
        "assert_is_some" | "test.assert_is_some" | "std.test.assert_is_some" => {
            Some("zz_assert_is_some")
        }
        "assert_is_none" | "test.assert_is_none" | "std.test.assert_is_none" => {
            Some("zz_assert_is_none")
        }
        "assert_is_ok" | "test.assert_is_ok" | "std.test.assert_is_ok" => Some("zz_assert_is_ok"),
        "assert_is_err" | "test.assert_is_err" | "std.test.assert_is_err" => {
            Some("zz_assert_is_err")
        }
        "temp_dir" | "test.temp_dir" | "std.test.temp_dir" => Some("zz_test_temp_dir"),
        "free_port" | "test.free_port" | "std.test.free_port" => Some("zz_test_free_port"),
        "assert_snapshot" | "test.assert_snapshot" | "std.test.assert_snapshot" => {
            Some("zz_assert_snapshot")
        }
        "black_box" | "test.black_box" | "std.test.black_box" => Some("zz_black_box"),
        // Debug print preserving Option wrappers; returns its argument.
        "dbg" => Some("zz_dbg"),
        "len" => Some("zz_len"),
        "ord" | "str.ord" | "std.str.ord" => Some("zz_ord"),
        "chr" | "str.chr" | "std.str.chr" => Some("zz_chr"),
        "map" | "vec.map" | "std.vec.map" => Some("zz_iter_map"),
        "filter" | "vec.filter" | "std.vec.filter" => Some("zz_iter_filter"),
        "enumerate" | "vec.enumerate" | "std.vec.enumerate" => Some("zz_iter_enumerate"),
        "zip" | "vec.zip" | "std.vec.zip" => Some("zz_iter_zip"),
        "clone" => Some("zz_clone_deep"),
        "range" => Some("zz_range3"),
        "typeof" => Some("zz_typeof"),
        "int" => Some("zz_int_cast"),
        "float" => Some("zz_float_cast"),
        "bool" => Some("zz_bool_cast"),
        "str" => Some("zz_str_cast"),
        // vec methods — bare names for method dispatch
        "vec.len" | "std.vec.len" | "vec_len" => Some("zz_vec_len"),
        "bytes.len" | "std.bytes.len" => Some("zz_len"),
        // `vec.append` was removed — canonical spelling is `vec.push`.
        // (Mapping deleted; the checker errors with a rename hint.)
        "vec.push" | "std.vec.push" => Some("zz_vec_push"),
        "vec.pop" | "std.vec.pop" => Some("zz_vec_pop"),
        "vec.remove" | "std.vec.remove" => Some("zz_vec_remove"),
        "vec.insert" | "std.vec.insert" => Some("zz_vec_insert"),
        "vec.join" | "std.vec.join" => Some("zz_str_join"), // join is a str function called on arrays
        "vec.contains" | "std.vec.contains" => Some("zz_vec_contains"),
        "vec.sort" | "std.vec.sort" => Some("zz_vec_sort"),
        "vec.sort_by" | "std.vec.sort_by" => Some("zz_vec_sort_by"),
        "vec.sort_by_desc" | "std.vec.sort_by_desc" => Some("zz_vec_sort_by_desc"),
        "vec.sort_key" | "std.vec.sort_key" => Some("zz_vec_sort_key"),
        "vec.reverse" | "std.vec.reverse" => Some("zz_vec_reverse"),
        // str
        "str.length" | "std.str.length" => Some("zz_str_length"),
        "str.to_lower" | "std.str.to_lower" | "str.lower" | "std.str.lower" => Some("zz_str_lower"),
        "str.to_upper" | "std.str.to_upper" | "str.upper" | "std.str.upper" => Some("zz_str_upper"),
        "str.replace" | "std.str.replace" => Some("zz_str_replace"),
        "str.count" | "std.str.count" => Some("zz_str_count"),
        "str.contains" | "std.str.contains" => Some("zz_str_contains"),
        "str.starts_with" | "std.str.starts_with" | "str.startswith" | "std.str.startswith" => {
            Some("zz_str_startswith")
        }
        "str.ends_with" | "std.str.ends_with" | "str.endswith" | "std.str.endswith" => {
            Some("zz_str_endswith")
        }
        "str.trim" | "std.str.trim" => Some("zz_str_trim"),
        "str.trim_start" | "std.str.trim_start" => Some("zz_str_trim_start"),
        "str.trim_end" | "std.str.trim_end" => Some("zz_str_trim_end"),
        "str.join" | "std.str.join" => Some("zz_str_join"),
        "str.split" | "std.str.split" => Some("zz_str_split"),
        "str.find" | "std.str.find" => Some("zz_str_find"),
        "str.rfind" | "std.str.rfind" => Some("zz_str_rfind"),
        "str.starts_with_at" | "std.str.starts_with_at" => Some("zz_str_starts_with_at"),
        "str.ends_with_at" | "std.str.ends_with_at" => Some("zz_str_ends_with_at"),
        "str.trim_span" | "std.str.trim_span" => Some("zz_str_trim_span"),
        "str.find_in" | "std.str.find_in" => Some("zz_str_find_in"),
        "str.rfind_in" | "std.str.rfind_in" => Some("zz_str_rfind_in"),
        "str.count_in" | "std.str.count_in" => Some("zz_str_count_in"),
        "str.classify" | "std.str.classify" => Some("zz_str_classify"),
        "str.bytes" | "std.str.bytes" => Some("zz_str_bytes"),
        "str.byte_len" | "std.str.byte_len" => Some("zz_str_byte_len"),
        "bytes.to_str" | "std.bytes.to_str" => Some("zz_bytes_to_str"),
        "bytes.to_ints" | "std.bytes.to_ints" => Some("zz_bytes_to_ints"),
        "cell.new" | "std.cell.new" => Some("zz_cell_new"),
        "cell.get" | "std.cell.get" => Some("zz_cell_get"),
        "cell.set" | "std.cell.set" => Some("zz_cell_set"),
        "cell.drop" | "std.cell.drop" => Some("zz_cell_drop"),
        "bytes.finish" | "std.bytes.finish" => Some("zz_bytes_finish"),
        // math
        "math.abs" | "std.math.abs" => Some("zz_math_abs"),
        "math.sqrt" | "std.math.sqrt" => Some("zz_math_sqrt"),
        "math.pow" | "std.math.pow" => Some("zz_math_pow"),
        "math.floor" | "std.math.floor" => Some("zz_math_floor"),
        "math.ceil" | "std.math.ceil" => Some("zz_math_ceil"),
        "math.round" | "std.math.round" => Some("zz_math_round"),
        "math.trunc" | "std.math.trunc" => Some("zz_math_trunc"),
        "math.signum" | "std.math.signum" => Some("zz_math_signum"),
        "math.hypot" | "std.math.hypot" => Some("zz_math_hypot"),
        "math.clamp" | "std.math.clamp" => Some("zz_math_clamp"),
        "math.root" | "std.math.root" => Some("zz_math_root"),
        "math.factorial" | "std.math.factorial" => Some("zz_math_factorial"),
        "math.gcd" | "std.math.gcd" => Some("zz_math_gcd"),
        "math.lcm" | "std.math.lcm" => Some("zz_math_lcm"),
        "math.sin" | "std.math.sin" => Some("zz_math_sin"),
        "math.cos" | "std.math.cos" => Some("zz_math_cos"),
        "math.tan" | "std.math.tan" => Some("zz_math_tan"),
        "math.asin" | "std.math.asin" => Some("zz_math_asin"),
        "math.acos" | "std.math.acos" => Some("zz_math_acos"),
        "math.atan" | "std.math.atan" => Some("zz_math_atan"),
        "math.sin_deg" | "std.math.sin_deg" => Some("zz_math_sin_deg"),
        "math.cos_deg" | "std.math.cos_deg" => Some("zz_math_cos_deg"),
        "math.tan_deg" | "std.math.tan_deg" => Some("zz_math_tan_deg"),
        "math.to_radians" | "std.math.to_radians" => Some("zz_math_to_radians"),
        "math.to_degrees" | "std.math.to_degrees" => Some("zz_math_to_degrees"),
        "math.log" | "std.math.log" => Some("zz_math_log"),
        "math.log10" | "std.math.log10" => Some("zz_math_log10"),
        "math.exp" | "std.math.exp" => Some("zz_math_exp"),
        "math.random" | "std.math.random" => Some("zz_math_random"),
        "math.is_nan" | "std.math.is_nan" => Some("zz_math_is_nan"),
        "math.is_inf" | "std.math.is_inf" => Some("zz_math_is_inf"),
        "math.PI" | "std.math.PI" => Some("zz_math_pi"),
        "math.E" | "std.math.E" => Some("zz_math_e"),
        "math.TAU" | "std.math.TAU" => Some("zz_math_tau"),
        "math.INF" | "std.math.INF" => Some("zz_math_inf"),
        "math.NAN" | "std.math.NAN" => Some("zz_math_nan"),
        "math.isqrt" | "std.math.isqrt" => Some("zz_math_isqrt"),
        "math.mean" | "std.math.mean" => Some("zz_math_mean"),
        "math.median" | "std.math.median" => Some("zz_math_median"),
        "math.rand_range" | "std.math.rand_range" => Some("zz_math_rand_range"),
        "math.dot_product" | "std.math.dot_product" => Some("zz_math_dot_product"),
        "math.magnitude" | "std.math.magnitude" => Some("zz_math_magnitude"),
        // json
        "json.parse" | "std.json.parse" => Some("zz_json_parse"),
        "json.stringify" | "std.json.stringify" => Some("zz_json_stringify"),
        "json.null" | "std.json.null" => Some("zz_json_null"),
        "json.get" | "std.json.get" => Some("zz_json_get"),
        "json.as_str" | "std.json.as_str" => Some("zz_json_as_str"),
        "json.as_int" | "std.json.as_int" => Some("zz_json_as_int"),
        "json.as_float" | "std.json.as_float" => Some("zz_json_as_float"),
        "json.as_bool" | "std.json.as_bool" => Some("zz_json_as_bool"),
        "json.type" | "std.json.type" => Some("zz_json_type"),
        "json.len" | "std.json.len" => Some("zz_json_len"),
        "json.keys" | "std.json.keys" => Some("zz_json_keys"),
        "json.has" | "std.json.has" => Some("zz_json_has"),
        "json.pretty" | "std.json.pretty" => Some("zz_json_pretty"),
        "json.merge" | "std.json.merge" => Some("zz_json_merge"),
        "json.deep_get" | "std.json.deep_get" => Some("zz_json_deep_get"),
        "json.array_push" | "std.json.array_push" => Some("zz_json_array_push"),
        // env
        "env.get" | "std.env.get" | "envmod.get" | "std.envmod.get" => Some("zz_env_get"),
        "env.var" | "std.env.var" | "envmod.var" | "std.envmod.var" => Some("zz_env_var"),
        "env.set" | "std.env.set" => Some("zz_env_set"),
        "env.remove" | "std.env.remove" | "env.unset" | "std.env.unset" => Some("zz_env_remove"),
        "env.vars" | "std.env.vars" => Some("zz_env_vars"),
        "env.cwd" | "std.env.cwd" => Some("zz_env_cwd"),
        "env.set_cwd" | "std.env.set_cwd" => Some("zz_env_set_cwd"),
        "env.exe_path" | "std.env.exe_path" => Some("zz_env_exe_path"),
        "env.home_dir" | "std.env.home_dir" => Some("zz_env_home_dir"),
        "env.temp_dir" | "std.env.temp_dir" => Some("zz_env_temp_dir"),
        "env.user" | "std.env.user" => Some("zz_env_user"),
        "env.os" | "std.env.os" => Some("zz_env_os"),
        "env.arch" | "std.env.arch" => Some("zz_env_arch"),
        "env.args" | "std.env.args" | "envmod.args" | "std.envmod.args" => Some("zz_env_args"),
        // dict
        "dict.len" => Some("zz_dict_len_val"),
        "dict.keys" => Some("zz_dict_keys"),
        "dict.has" => Some("zz_dict_has"),
        // option / result
        "option.expect" | "std.option.expect" => Some("zz_option_expect"),
        "result.expect" | "std.result.expect" => Some("zz_result_expect"),
        "option.unwrap" | "std.option.unwrap" => Some("zz_option_unwrap"),
        "option.unwrap_or" | "std.option.unwrap_or" => Some("zz_option_unwrap_or"),
        "result.unwrap" | "std.result.unwrap" => Some("zz_result_unwrap"),
        "result.unwrap_or" | "std.result.unwrap_or" => Some("zz_result_unwrap_or"),
        // fs — comprehensive non-blocking filesystem (C runtime in
        // core.c; unified `fs:<op>:<code>` diagnostics match the VM).
        "fs.read" | "std.fs.read" => Some("zz_fs_read"),
        "fs.read_file" | "std.fs.read_file" => Some("zz_fs_read"),
        "fs.read_to_string" | "std.fs.read_to_string" => Some("zz_fs_read"),
        "fs.read_bytes" | "std.fs.read_bytes" => Some("zz_fs_read_bytes"),
        "fs.write" | "std.fs.write" => Some("zz_fs_write"),
        "fs.write_file" | "std.fs.write_file" => Some("zz_fs_write"),
        "fs.append" | "std.fs.append" => Some("zz_fs_append"),
        "fs.copy" | "std.fs.copy" => Some("zz_fs_copy"),
        "fs.move" | "std.fs.move" => Some("zz_fs_move"),
        "fs.rename" | "std.fs.rename" => Some("zz_fs_move"),
        "fs.exists" | "std.fs.exists" => Some("zz_fs_exists"),
        "fs.is_file" | "std.fs.is_file" => Some("zz_fs_is_file"),
        "fs.is_dir" | "std.fs.is_dir" => Some("zz_fs_is_dir"),
        "fs.remove" | "std.fs.remove" | "fs.remove_file" | "std.fs.remove_file" => {
            Some("zz_fs_remove")
        }
        "fs.mkdir" | "std.fs.mkdir" => Some("zz_fs_mkdir"),
        "fs.mkdir_all" | "std.fs.mkdir_all" => Some("zz_fs_mkdir_all"),
        "fs.readdir" | "std.fs.readdir" => Some("zz_fs_readdir"),
        "fs.read_dir" | "std.fs.read_dir" => Some("zz_fs_read_dir"),
        "fs.remove_dir_all" | "std.fs.remove_dir_all" => Some("zz_fs_remove_dir_all"),
        "fs.walk_dir" | "std.fs.walk_dir" => Some("zz_fs_walk_dir"),
        "fs.stat" | "std.fs.stat" => Some("zz_fs_stat"),
        "fs.scan_counts" | "std.fs.scan_counts" => Some("zz_fs_scan_counts"),
        "fs.is_generated" | "std.fs.is_generated" => Some("zz_fs_is_generated"),
        "fs.open" | "std.fs.open" | "File.open" => Some("zz_fs_open"),
        "fs.read_chunk" | "std.fs.read_chunk" | "file.read_chunk" => Some("zz_fs_read_chunk"),
        "fs.read_chunk_bytes" | "std.fs.read_chunk_bytes" | "file.read_chunk_bytes" => {
            Some("zz_fs_read_chunk_bytes")
        }
        "fs.write_chunk" | "std.fs.write_chunk" | "file.write_chunk" => Some("zz_fs_write_chunk"),
        "fs.seek" | "std.fs.seek" | "file.seek" => Some("zz_fs_seek"),
        "fs.flush" | "std.fs.flush" | "file.flush" => Some("zz_fs_flush"),
        "fs.close" | "std.fs.close" | "file.close" => Some("zz_fs_close"),
        "fs.normalize" | "std.fs.normalize" => Some("zz_fs_normalize"),
        "fs.join" | "std.fs.join" => Some("zz_fs_join"),
        "fs.basename" | "std.fs.basename" => Some("zz_fs_basename"),
        "fs.dirname" | "std.fs.dirname" => Some("zz_fs_dirname"),
        "fs.is_absolute" | "std.fs.is_absolute" => Some("zz_fs_is_absolute"),
        "fs.extension" | "std.fs.extension" => Some("zz_fs_extension"),
        "fs.osfs" | "std.fs.osfs" => Some("zz_fs_osfs"),
        "fs.memfs" | "std.fs.memfs" => Some("zz_fs_memfs"),
        "fs.tarfs" | "std.fs.tarfs" => Some("zz_fs_tarfs"),
        "fs.embedfs" | "std.fs.embedfs" => Some("zz_fs_embedfs"),
        "fs.read_to_string_at" | "std.fs.read_to_string_at" => Some("zz_fs_read_to_string_at"),
        "fs.read_bytes_at" | "std.fs.read_bytes_at" => Some("zz_fs_read_bytes_at"),
        "fs.write_at" | "std.fs.write_at" => Some("zz_fs_write_at"),
        "fs.append_at" | "std.fs.append_at" => Some("zz_fs_append_at"),
        "fs.exists_at" | "std.fs.exists_at" => Some("zz_fs_exists_at"),
        "fs.is_file_at" | "std.fs.is_file_at" => Some("zz_fs_is_file_at"),
        "fs.is_dir_at" | "std.fs.is_dir_at" => Some("zz_fs_is_dir_at"),
        "fs.read_dir_at" | "std.fs.read_dir_at" => Some("zz_fs_read_dir_at"),
        "fs.mkdir_all_at" | "std.fs.mkdir_all_at" => Some("zz_fs_mkdir_all_at"),
        "fs.remove_file_at" | "std.fs.remove_file_at" => Some("zz_fs_remove_file_at"),
        // encoding
        "encoding.url_encode" | "std.encoding.url_encode" => Some("zz_encoding_url_encode"),
        "encoding.url_decode" | "std.encoding.url_decode" => Some("zz_encoding_url_decode"),
        "encoding.base64_encode" | "std.encoding.base64_encode" => {
            Some("zz_encoding_base64_encode")
        }
        "encoding.base64_decode" | "std.encoding.base64_decode" => {
            Some("zz_encoding_base64_decode")
        }
        "encoding.hex_encode" | "std.encoding.hex_encode" => Some("zz_encoding_hex_encode"),
        "encoding.hex_decode" | "std.encoding.hex_decode" => Some("zz_encoding_hex_decode"),
        // time
        "time.now_ms" | "std.time.now_ms" => Some("zz_time_now_ms"),
        "time.sleep_ms" | "std.time.sleep_ms" => Some("zz_time_sleep_ms"),
        // net tcp
        "net.tcp_connect" | "std.net.tcp_connect" => Some("zz_tcp_connect"),
        "net.tcp_listen" | "std.net.tcp_listen" => Some("zz_tcp_listen"),
        "net.tcp_accept" | "std.net.tcp_accept" => Some("zz_tcp_accept"),
        "net.tcp_write" | "std.net.tcp_write" => Some("zz_tcp_write"),
        "net.tcp_read" | "std.net.tcp_read" => Some("zz_tcp_read"),
        "net.tcp_readline" | "std.net.tcp_readline" => Some("zz_tcp_readline"),
        "net.tcp_read_bytes" | "std.net.tcp_read_bytes" => Some("zz_tcp_read_bytes"),
        "net.tcp_read_exact" | "std.net.tcp_read_exact" => Some("zz_tcp_read_exact"),
        "net.tcp_write_bytes" | "std.net.tcp_write_bytes" => Some("zz_tcp_write_bytes"),
        "net.tcp_shutdown" | "std.net.tcp_shutdown" => Some("zz_tcp_shutdown"),
        // Ergonomic `net.*` method aliases (same C impls as the canonicals).
        "net.accept" | "std.net.accept" => Some("zz_tcp_accept"),
        "net.read" | "std.net.read" => Some("zz_tcp_read"),
        "net.read_line" | "std.net.read_line" => Some("zz_tcp_readline"),
        "net.read_bytes" | "std.net.read_bytes" => Some("zz_tcp_read_bytes"),
        "net.read_exact" | "std.net.read_exact" => Some("zz_tcp_read_exact"),
        "net.write" | "std.net.write" => Some("zz_tcp_write"),
        "net.write_bytes" | "std.net.write_bytes" => Some("zz_tcp_write_bytes"),
        "net.shutdown" | "std.net.shutdown" => Some("zz_tcp_shutdown"),
        "net.peer_addr" | "std.net.peer_addr" => Some("zz_tcp_peer_addr"),
        "net.local_addr" | "std.net.local_addr" => Some("zz_tcp_local_addr"),
        "net.set_read_timeout" | "std.net.set_read_timeout" => Some("zz_tcp_set_read_timeout"),
        "net.set_write_timeout" | "std.net.set_write_timeout" => Some("zz_tcp_set_write_timeout"),
        // sqlz (AOT: sqlite3 prepared-statement C impl + postgres via
        // the staticlib; the C handle records the backend).
        // Canonical `sqlz.*` / `std.sqlz.*` (`db` was removed);
        // `pg.*` / `std.sqlz.postgres.*` are the explicit PG spellings
        // (`pg` only exists as an import alias of the nested module).
        "sqlz.open" | "std.sqlz.open" => Some("zz_db_open"),
        "pg.connect" | "std.sqlz.postgres.connect" => Some("zz_pg_connect"),
        "sqlz.exec" | "std.sqlz.exec" => Some("zz_db_exec"),
        "pg.exec" | "std.sqlz.postgres.exec" => Some("zz_db_exec"),
        "sqlz.query" | "std.sqlz.query" => Some("zz_db_query"),
        "pg.query" | "std.sqlz.postgres.query" => Some("zz_db_query"),
        "sqlz.close" | "std.sqlz.close" => Some("zz_db_close"),
        "pg.close" | "std.sqlz.postgres.close" => Some("zz_db_close"),
        // channels
        "chan" | "std.chan" => Some("zz_chan_new"),
        "chan.send" | "std.chan.send" => Some("zz_chan_send"),
        "chan.recv" | "std.chan.recv" => Some("zz_chan_recv"),
        "chan.try_recv" | "std.chan.try_recv" => Some("zz_chan_try_recv"),
        // spawn / task join
        "spawn" | "std.spawn" | "task.spawn" | "std.task.spawn" => Some("zz_spawn"),
        "task.recv" | "std.task.recv" | "task.join" | "std.task.join" => Some("zz_task_join_recv"),
        "task.try_join" | "std.task.try_join" => Some("zz_task_try_join"),
        // http (AOT: route table with closure handlers; http.test
        // dispatches in-process; epoll workers serve static OK until Step 2)
        "http.server" | "std.http.server" => Some("zz_http_server"),
        "http.route_get" | "std.http.route_get" => Some("zz_http_route_get"),
        "http.route_post" | "std.http.route_post" => Some("zz_http_route_post"),
        "http.route_put" | "std.http.route_put" => Some("zz_http_route_put"),
        "http.route_delete" | "std.http.route_delete" => Some("zz_http_route_delete"),
        "http.route" | "std.http.route" => Some("zz_http_route"),
        "http.log" | "std.http.log" => Some("zz_http_log"),
        "http.pipe" | "std.http.pipe" => Some("zz_http_pipe"),
        "http.pipe_post" | "std.http.pipe_post" => Some("zz_http_pipe_post"),
        "http.with_headers" | "std.http.with_headers" => Some("zz_http_with_headers"),
        "http.rate_limit" | "std.http.rate_limit" => Some("zz_http_rate_limit"),
        "http.listen" | "std.http.listen" => Some("zz_http_listen"),
        "http.listen_cfg" | "std.http.listen_cfg" => Some("zz_http_listen_cfg"),
        "http.serve_dir" | "std.http.serve_dir" => Some("zz_http_serve_dir"),
        "http.serve_dir_at" | "std.http.serve_dir_at" => Some("zz_http_serve_dir_at"),
        "http.test" | "std.http.test" => Some("zz_http_test"),
        "http.test_req" | "std.http.test_req" => Some("zz_http_test_req"),
        "http.handle" | "std.http.handle" => Some("zz_http_handle"),
        "http.respond" | "std.http.respond" => Some("zz_http_respond"),
        "http.param" | "std.http.param" => Some("zz_http_param"),
        "http.query" | "std.http.query" => Some("zz_http_query"),
        "http.header" | "std.http.header" => Some("zz_http_header"),
        "http.body_json" | "std.http.body_json" => Some("zz_http_body_json"),
        "http.body_form" | "std.http.body_form" => Some("zz_http_body_form"),
        "http.body_bytes" | "std.http.body_bytes" => Some("zz_http_body_bytes"),
        // http Response methods
        "http.status" | "std.http.status" => Some("zz_http_response_status"),
        "http.text" | "std.http.text" => Some("zz_http_response_text"),
        "http.json" | "std.http.json" => Some("zz_http_response_json"),
        "http.headers" | "std.http.headers" => Some("zz_http_response_headers"),
        // http request functions
        "http.get" | "std.http.get" => Some("zz_http_get"),
        "http.post" | "std.http.post" => Some("zz_http_post"),
        "http.fetch" | "std.http.fetch" => Some("zz_http_fetch"),
        "http.post_json" | "std.http.post_json" => Some("zz_http_post_json"),
        _ => None,
    }
}

/// Map a zz native qualified name to its Rust-staticlib C symbol.
///
/// Populated per module as it lands (Phase 1: `std.regexp`). Both the
/// `std.<mod>.*` and bare `<mod>.*` spellings map to the same symbol,
/// mirroring the `native_impl` convention for embedded-C natives.
pub fn ffi_impl(name: &str) -> Option<&'static str> {
    match name {
        "regexp.compile" | "std.regexp.compile" => Some("zz_regexp_compile"),
        "encoding.base64_decode_bytes" | "std.encoding.base64_decode_bytes" => {
            Some("zz_encoding_base64_decode_bytes")
        }
        "regexp.is_match" | "std.regexp.is_match" => Some("zz_regexp_is_match"),
        "regexp.find" | "std.regexp.find" => Some("zz_regexp_find"),
        "regexp.replace_all" | "std.regexp.replace_all" => Some("zz_regexp_replace_all"),
        "regexp.captures" | "std.regexp.captures" => Some("zz_regexp_captures"),
        "crypto.sha256" | "std.crypto.sha256" => Some("zz_crypto_sha256"),
        "crypto.sha256_bytes" | "std.crypto.sha256_bytes" => Some("zz_crypto_sha256_bytes"),
        "crypto.sha512" | "std.crypto.sha512" => Some("zz_crypto_sha512"),
        "crypto.hmac_sha256" | "std.crypto.hmac_sha256" => Some("zz_crypto_hmac_sha256"),
        "crypto.random_bytes" | "std.crypto.random_bytes" => Some("zz_crypto_random_bytes"),
        "crypto.ct_eq" | "std.crypto.ct_eq" => Some("zz_crypto_ct_eq"),
        "crypto.argon2_hash" | "std.crypto.argon2_hash" => Some("zz_crypto_argon2_hash"),
        "crypto.argon2_verify" | "std.crypto.argon2_verify" => Some("zz_crypto_argon2_verify"),
        "crypto.bcrypt_hash" | "std.crypto.bcrypt_hash" => Some("zz_crypto_bcrypt_hash"),
        "crypto.bcrypt_verify" | "std.crypto.bcrypt_verify" => Some("zz_crypto_bcrypt_verify"),
        "crypto.ed25519_keypair" | "std.crypto.ed25519_keypair" => {
            Some("zz_crypto_ed25519_keypair")
        }
        "crypto.ed25519_sign" | "std.crypto.ed25519_sign" => Some("zz_crypto_ed25519_sign"),
        "crypto.ed25519_verify" | "std.crypto.ed25519_verify" => Some("zz_crypto_ed25519_verify"),
        "crypto.rsa_keypair" | "std.crypto.rsa_keypair" => Some("zz_crypto_rsa_keypair"),
        "crypto.rsa_sign" | "std.crypto.rsa_sign" => Some("zz_crypto_rsa_sign"),
        "crypto.rsa_verify" | "std.crypto.rsa_verify" => Some("zz_crypto_rsa_verify"),
        "crypto.jwt_encode" | "std.crypto.jwt_encode" => Some("zz_crypto_jwt_encode"),
        "crypto.jwt_decode" | "std.crypto.jwt_decode" => Some("zz_crypto_jwt_decode"),
        "crypto.jwt_encode_ed" | "std.crypto.jwt_encode_ed" => Some("zz_crypto_jwt_encode_ed"),
        "crypto.jwt_decode_ed" | "std.crypto.jwt_decode_ed" => Some("zz_crypto_jwt_decode_ed"),
        "time.now_nanos" | "std.time.now_nanos" => Some("zz_time_now_nanos"),
        "time.now_micros" | "std.time.now_micros" => Some("zz_time_now_micros"),
        "time.monotonic_nanos" | "std.time.monotonic_nanos" => Some("zz_time_monotonic_nanos"),
        "time.sleep_micros" | "std.time.sleep_micros" => Some("zz_time_sleep_micros"),
        "log.set_level" | "std.log.set_level" => Some("zz_log_set_level"),
        "log.get_level" | "std.log.get_level" => Some("zz_log_get_level"),
        "log.set_format" | "std.log.set_format" => Some("zz_log_set_format"),
        "log.to_file" | "std.log.to_file" => Some("zz_log_to_file"),
        "log.to_stderr" | "std.log.to_stderr" => Some("zz_log_to_stderr"),
        "log.trace" | "std.log.trace" => Some("zz_log_trace"),
        "log.debug" | "std.log.debug" => Some("zz_log_debug"),
        "log.info" | "std.log.info" => Some("zz_log_info"),
        "log.warn" | "std.log.warn" => Some("zz_log_warn"),
        "log.error" | "std.log.error" => Some("zz_log_error"),
        "log.span_begin" | "std.log.span_begin" => Some("zz_log_span_begin"),
        "span.end" | "std.span.end" => Some("zz_span_end"),
        "sys.os" | "std.sys.os" => Some("zz_sys_os"),
        "sys.arch" | "std.sys.arch" => Some("zz_sys_arch"),
        "sys.cpu_count" | "std.sys.cpu_count" => Some("zz_sys_cpu_count"),
        "sys.hostname" | "std.sys.hostname" => Some("zz_sys_hostname"),
        "sys.total_mem" | "std.sys.total_mem" => Some("zz_sys_total_mem"),
        "sys.avail_mem" | "std.sys.avail_mem" => Some("zz_sys_avail_mem"),
        "term.enable_raw" | "std.term.enable_raw" => Some("zz_term_enable_raw"),
        "term.disable_raw" | "std.term.disable_raw" => Some("zz_term_disable_raw"),
        "term.read_key" | "std.term.read_key" => Some("zz_term_read_key"),
        "term.poll" | "std.term.poll" => Some("zz_term_poll"),
        "term.get_size" | "std.term.get_size" => Some("zz_term_get_size"),
        "term.is_tty" | "std.term.is_tty" => Some("zz_term_is_tty"),
        "term.flush" | "std.term.flush" => Some("zz_term_flush"),
        // Raw argv shares the fixed `zz_env_args` symbol (one source of
        // truth for both spellings and both engines).
        "args.get_raw" | "std.args.get_raw" => Some("zz_env_args"),
        "args.parser" | "std.args.parser" => Some("zz_args_parser"),
        "args.str_flag" | "std.args.str_flag" => Some("zz_args_str_flag"),
        "args.int_flag" | "std.args.int_flag" => Some("zz_args_int_flag"),
        "args.bool_flag" | "std.args.bool_flag" => Some("zz_args_bool_flag"),
        "args.parse" | "std.args.parse" => Some("zz_args_parse"),
        "args.get_str" | "std.args.get_str" => Some("zz_args_get_str"),
        "args.get_int" | "std.args.get_int" => Some("zz_args_get_int"),
        "args.get_bool" | "std.args.get_bool" => Some("zz_args_get_bool"),
        "args.positional" | "std.args.positional" => Some("zz_args_positional"),
        "args.subcommand" | "std.args.subcommand" => Some("zz_args_subcommand"),
        "args.help" | "std.args.help" => Some("zz_args_help"),
        "args.error" | "std.args.error" => Some("zz_args_error"),
        "args.was_help" | "std.args.was_help" => Some("zz_args_was_help"),
        "process.run" | "std.process.run" => Some("zz_process_run"),
        "process.run_with_env" | "std.process.run_with_env" => Some("zz_process_run_with_env"),
        "process.spawn" | "std.process.spawn" => Some("zz_process_spawn"),
        "process.wait" | "std.process.wait" => Some("zz_process_wait"),
        "process.exit" | "std.process.exit" => Some("zz_process_exit"),
        "process.pid" | "std.process.pid" => Some("zz_process_pid"),
        "uuid.v4" | "std.uuid.v4" => Some("zz_uuid_v4"),
        "uuid.v7" | "std.uuid.v7" => Some("zz_uuid_v7"),
        "uuid.parse" | "std.uuid.parse" => Some("zz_uuid_parse"),
        "uuid.is_valid" | "std.uuid.is_valid" => Some("zz_uuid_is_valid"),
        "net.tls_connect" | "std.net.tls_connect" | "tls.connect" | "std.tls.connect" => {
            Some("zz_tls_connect")
        }
        "net.tls_connect_with_ca"
        | "std.net.tls_connect_with_ca"
        | "tls.connect_with_ca"
        | "std.tls.connect_with_ca" => Some("zz_tls_connect_with_ca"),
        "net.tls_connect_insecure"
        | "std.net.tls_connect_insecure"
        | "tls.connect_insecure"
        | "std.tls.connect_insecure" => Some("zz_tls_connect_insecure"),
        "net.tls_write" | "std.net.tls_write" | "tls.write" | "std.tls.write" => {
            Some("zz_tls_write")
        }
        "net.tls_write_bytes"
        | "std.net.tls_write_bytes"
        | "tls.write_bytes"
        | "std.tls.write_bytes" => Some("zz_tls_write_bytes"),
        "net.tls_read" | "std.net.tls_read" | "tls.read" | "std.tls.read" => Some("zz_tls_read"),
        "net.tls_read_bytes"
        | "std.net.tls_read_bytes"
        | "tls.read_bytes"
        | "std.tls.read_bytes" => Some("zz_tls_read_bytes"),
        "net.tls_read_exact"
        | "std.net.tls_read_exact"
        | "tls.read_exact"
        | "std.tls.read_exact" => Some("zz_tls_read_exact"),
        "net.tls_readline" | "std.net.tls_readline" | "tls.read_line" | "std.tls.read_line" => {
            Some("zz_tls_read_line")
        }
        "net.tls_set_read_timeout"
        | "std.net.tls_set_read_timeout"
        | "tls.set_read_timeout"
        | "std.tls.set_read_timeout" => Some("zz_tls_set_read_timeout"),
        "net.tls_set_write_timeout"
        | "std.net.tls_set_write_timeout"
        | "tls.set_write_timeout"
        | "std.tls.set_write_timeout" => Some("zz_tls_set_write_timeout"),
        "net.tls_close"
        | "std.net.tls_close"
        | "tls.close"
        | "std.tls.close"
        | "tls.shutdown"
        | "std.tls.shutdown"
        | "net.tls_shutdown"
        | "std.net.tls_shutdown" => Some("zz_tls_close"),
        "net.unix_connect" | "std.net.unix_connect" => Some("zz_unix_connect"),
        "net.unix_listen" | "std.net.unix_listen" => Some("zz_unix_listen"),
        "net.unix_accept"
        | "std.net.unix_accept"
        | "unix_listener.accept"
        | "std.unix_listener.accept" => Some("zz_unix_accept"),
        "net.unix_listener_close"
        | "std.net.unix_listener_close"
        | "unix_listener.close"
        | "std.unix_listener.close" => Some("zz_unix_listener_close"),
        "net.unix_write" | "std.net.unix_write" | "unix.write" | "std.unix.write" => {
            Some("zz_unix_write")
        }
        "net.unix_write_bytes"
        | "std.net.unix_write_bytes"
        | "unix.write_bytes"
        | "std.unix.write_bytes" => Some("zz_unix_write_bytes"),
        "net.unix_read" | "std.net.unix_read" | "unix.read" | "std.unix.read" => {
            Some("zz_unix_read")
        }
        "net.unix_read_bytes"
        | "std.net.unix_read_bytes"
        | "unix.read_bytes"
        | "std.unix.read_bytes" => Some("zz_unix_read_bytes"),
        "net.unix_read_exact"
        | "std.net.unix_read_exact"
        | "unix.read_exact"
        | "std.unix.read_exact" => Some("zz_unix_read_exact"),
        "net.unix_readline" | "std.net.unix_readline" | "unix.read_line" | "std.unix.read_line" => {
            Some("zz_unix_read_line")
        }
        "net.unix_set_read_timeout"
        | "std.net.unix_set_read_timeout"
        | "unix.set_read_timeout"
        | "std.unix.set_read_timeout" => Some("zz_unix_set_read_timeout"),
        "net.unix_set_write_timeout"
        | "std.net.unix_set_write_timeout"
        | "unix.set_write_timeout"
        | "std.unix.set_write_timeout" => Some("zz_unix_set_write_timeout"),
        "net.unix_close"
        | "std.net.unix_close"
        | "unix.close"
        | "std.unix.close"
        | "unix.shutdown"
        | "std.unix.shutdown"
        | "net.unix_shutdown"
        | "std.net.unix_shutdown" => Some("zz_unix_close"),
        "net.tls_listen" | "std.net.tls_listen" => Some("zz_tls_listen"),
        "net.tls_accept"
        | "std.net.tls_accept"
        | "tls_listener.accept"
        | "std.tls_listener.accept" => Some("zz_tls_accept"),
        "net.tls_listener_close"
        | "std.net.tls_listener_close"
        | "tls_listener.close"
        | "std.tls_listener.close" => Some("zz_tls_listener_close"),
        "net.tls_peer_addr" | "std.net.tls_peer_addr" | "tls.peer_addr" | "std.tls.peer_addr" => {
            Some("zz_tls_peer_addr")
        }
        _ => None,
    }
}

/// `sqlz.*` / `pg.*` spellings (both bare and `std.`-qualified):
/// all lower to the C handle natives whose PG backend lives in the
/// staticlib. MySQL spellings are excluded (still VM-only, lowered to
/// unit — no staticlib symbols involved).
fn is_sql_native(name: &str) -> bool {
    const PREFIXES: &[&str] = &["sqlz.", "std.sqlz.", "pg.", "std.sqlz.postgres."];
    PREFIXES.iter().any(|p| name.starts_with(p))
}

/// ZZ spellings that lower to curl-backed C client calls, bare and
/// `std.`-qualified. `http.post` is a prefix of `http.post_json`, so one
/// entry covers both; listed explicitly for readability.
fn is_http_client_native(name: &str) -> bool {
    const PREFIXES: &[&str] = &[
        "http.get",
        "http.post",
        "http.fetch",
        "std.http.get",
        "std.http.post",
        "std.http.fetch",
    ];
    PREFIXES.iter().any(|p| name.starts_with(p))
}

/// True when the program can reach the Postgres backend: force-extract
/// its objects from the archive at link time (see `PG_LINK_SYMBOLS`).
/// Split from [`needs_native_rt`] (which only decides *whether* to link)
/// because weak C refs never pull archive members on their own.
pub fn needs_pg_link(natives: &std::collections::HashSet<String>) -> bool {
    natives.iter().any(|n| is_sql_native(n))
}

/// True when reachable natives lower to C calls backed by libcurl
/// (`zz_http_get/post/fetch` → `http_client_perform`).
/// Only the outbound client needs curl; the server/route/test/dispatch
/// surface is plain epoll + libc. `fetch_insecure` is included
/// conservatively (currently VM-only, but a future C impl would also
/// ride `http_client_perform`).
pub fn needs_curl_link(natives: &std::collections::HashSet<String>) -> bool {
    natives.iter().any(|n| is_http_client_native(n))
}

/// True when reachable natives lower to C calls backed by libsqlite3
/// (`zz_db_*` → `sqlite3_*`). Any sqlz/db/pg reachability counts: the C
/// handle dispatch references sqlite unconditionally (backend is chosen
/// at runtime), including the inlined `db.transaction` path which emits
/// `zz_db_exec_raw` directly.
pub fn needs_sqlite_link(natives: &std::collections::HashSet<String>) -> bool {
    natives.iter().any(|n| is_sql_native(n))
}

/// True when reachable natives need the C runtime (any native that has
/// a C implementation other than `zz_env_args`).
pub fn needs_native_rt(natives: &std::collections::HashSet<String>) -> bool {
    natives.iter().any(|n| {
        if is_sql_native(n) {
            return true;
        }
        matches!(ffi_impl(n), Some(s) if s != "zz_env_args")
    })
}
/// C declaration for a staticlib symbol. Extended alongside [`ffi_impl`].
pub fn ffi_decl(symbol: &str) -> Option<&'static str> {
    match symbol {
        "zz_regexp_compile" => Some("zz_value zz_regexp_compile(zz_value pat, int *err);"),
        "zz_regexp_is_match" => {
            Some("zz_value zz_regexp_is_match(zz_value re, zz_value s, int *err);")
        }
        "zz_regexp_find" => Some("zz_value zz_regexp_find(zz_value re, zz_value s, int *err);"),
        "zz_regexp_replace_all" => {
            Some("zz_value zz_regexp_replace_all(zz_value re, zz_value s, zz_value rep, int *err);")
        }
        "zz_regexp_captures" => {
            Some("zz_value zz_regexp_captures(zz_value re, zz_value s, int *err);")
        }
        "zz_crypto_sha256" => Some("zz_value zz_crypto_sha256(zz_value s, int *err);"),
        "zz_crypto_sha256_bytes" => {
            Some("zz_value zz_crypto_sha256_bytes(zz_value b, int *err);")
        }
        "zz_encoding_base64_decode_bytes" => {
            Some("zz_value zz_encoding_base64_decode_bytes(zz_value s, int *err);")
        }
        "zz_crypto_sha512" => Some("zz_value zz_crypto_sha512(zz_value s, int *err);"),
        "zz_crypto_hmac_sha256" => {
            Some("zz_value zz_crypto_hmac_sha256(zz_value key, zz_value msg, int *err);")
        }
        "zz_crypto_random_bytes" => Some("zz_value zz_crypto_random_bytes(zz_value n, int *err);"),
        "zz_crypto_ct_eq" => Some("zz_value zz_crypto_ct_eq(zz_value a, zz_value b, int *err);"),
        "zz_crypto_argon2_hash" => Some("zz_value zz_crypto_argon2_hash(zz_value pw, int *err);"),
        "zz_crypto_argon2_verify" => {
            Some("zz_value zz_crypto_argon2_verify(zz_value hash, zz_value pw, int *err);")
        }
        "zz_crypto_bcrypt_hash" => Some("zz_value zz_crypto_bcrypt_hash(zz_value pw, int *err);"),
        "zz_crypto_bcrypt_verify" => {
            Some("zz_value zz_crypto_bcrypt_verify(zz_value hash, zz_value pw, int *err);")
        }
        "zz_crypto_ed25519_keypair" => {
            Some("zz_value zz_crypto_ed25519_keypair(zz_value unit, int *err);")
        }
        "zz_crypto_ed25519_sign" => {
            Some("zz_value zz_crypto_ed25519_sign(zz_value sk, zz_value msg, int *err);")
        }
        "zz_crypto_ed25519_verify" => Some(
            "zz_value zz_crypto_ed25519_verify(zz_value pk, zz_value msg, zz_value sig, int *err);",
        ),
        "zz_crypto_rsa_keypair" => Some("zz_value zz_crypto_rsa_keypair(zz_value unit, int *err);"),
        "zz_crypto_rsa_sign" => {
            Some("zz_value zz_crypto_rsa_sign(zz_value sk, zz_value msg, int *err);")
        }
        "zz_crypto_rsa_verify" => Some(
            "zz_value zz_crypto_rsa_verify(zz_value pk, zz_value msg, zz_value sig, int *err);",
        ),
        "zz_crypto_jwt_encode" => {
            Some("zz_value zz_crypto_jwt_encode(zz_value payload, zz_value secret, int *err);")
        }
        "zz_crypto_jwt_decode" => {
            Some("zz_value zz_crypto_jwt_decode(zz_value token, zz_value secret, int *err);")
        }
        "zz_crypto_jwt_encode_ed" => {
            Some("zz_value zz_crypto_jwt_encode_ed(zz_value payload, zz_value sk, int *err);")
        }
        "zz_crypto_jwt_decode_ed" => {
            Some("zz_value zz_crypto_jwt_decode_ed(zz_value token, zz_value pk, int *err);")
        }
        "zz_time_now_nanos" => Some("zz_value zz_time_now_nanos(zz_value unit, int *err);"),
        "zz_time_now_micros" => Some("zz_value zz_time_now_micros(zz_value unit, int *err);"),
        "zz_time_monotonic_nanos" => {
            Some("zz_value zz_time_monotonic_nanos(zz_value unit, int *err);")
        }
        "zz_time_sleep_micros" => Some("zz_value zz_time_sleep_micros(zz_value n, int *err);"),
        "zz_log_set_level" => Some("zz_value zz_log_set_level(zz_value name, int *err);"),
        "zz_log_get_level" => Some("zz_value zz_log_get_level(zz_value unit, int *err);"),
        "zz_log_set_format" => Some("zz_value zz_log_set_format(zz_value name, int *err);"),
        "zz_log_to_file" => Some("zz_value zz_log_to_file(zz_value path, int *err);"),
        "zz_log_to_stderr" => Some("zz_value zz_log_to_stderr(zz_value unit, int *err);"),
        "zz_log_trace" => Some("zz_value zz_log_trace(zz_value msg, int *err);"),
        "zz_log_debug" => Some("zz_value zz_log_debug(zz_value msg, int *err);"),
        "zz_log_info" => Some("zz_value zz_log_info(zz_value msg, int *err);"),
        "zz_log_warn" => Some("zz_value zz_log_warn(zz_value msg, int *err);"),
        "zz_log_error" => Some("zz_value zz_log_error(zz_value msg, int *err);"),
        "zz_log_span_begin" => Some("zz_value zz_log_span_begin(zz_value name, int *err);"),
        "zz_span_end" => Some("zz_value zz_span_end(zz_value id, int *err);"),
        "zz_sys_os" => Some("zz_value zz_sys_os(zz_value unit, int *err);"),
        "zz_sys_arch" => Some("zz_value zz_sys_arch(zz_value unit, int *err);"),
        "zz_sys_cpu_count" => Some("zz_value zz_sys_cpu_count(zz_value unit, int *err);"),
        "zz_sys_hostname" => Some("zz_value zz_sys_hostname(zz_value unit, int *err);"),
        "zz_sys_total_mem" => Some("zz_value zz_sys_total_mem(zz_value unit, int *err);"),
        "zz_sys_avail_mem" => Some("zz_value zz_sys_avail_mem(zz_value unit, int *err);"),
        "zz_term_enable_raw" => Some("zz_value zz_term_enable_raw(zz_value unit, int *err);"),
        "zz_term_disable_raw" => {
            Some("zz_value zz_term_disable_raw(zz_value unit, int *err);")
        }
        "zz_term_read_key" => Some("zz_value zz_term_read_key(zz_value unit, int *err);"),
        "zz_term_poll" => Some("zz_value zz_term_poll(zz_value ms, int *err);"),
        "zz_term_get_size" => Some("zz_value zz_term_get_size(zz_value unit, int *err);"),
        "zz_term_is_tty" => Some("zz_value zz_term_is_tty(zz_value unit, int *err);"),
        "zz_term_flush" => Some("zz_value zz_term_flush(zz_value unit, int *err);"),
        "zz_args_parser" => Some("zz_value zz_args_parser(zz_value unit, int *err);"),
        "zz_args_str_flag" => {
            Some("zz_value zz_args_str_flag(zz_value h, zz_value name, zz_value def, int *err);")
        }
        "zz_args_int_flag" => {
            Some("zz_value zz_args_int_flag(zz_value h, zz_value name, zz_value def, int *err);")
        }
        "zz_args_bool_flag" => {
            Some("zz_value zz_args_bool_flag(zz_value h, zz_value name, int *err);")
        }
        "zz_args_parse" => Some("zz_value zz_args_parse(zz_value h, zz_value argv, int *err);"),
        "zz_args_get_str" => Some("zz_value zz_args_get_str(zz_value h, zz_value name, int *err);"),
        "zz_args_get_int" => Some("zz_value zz_args_get_int(zz_value h, zz_value name, int *err);"),
        "zz_args_get_bool" => {
            Some("zz_value zz_args_get_bool(zz_value h, zz_value name, int *err);")
        }
        "zz_args_positional" => {
            Some("zz_value zz_args_positional(zz_value h, zz_value i, int *err);")
        }
        "zz_args_subcommand" => Some("zz_value zz_args_subcommand(zz_value h, int *err);"),
        "zz_args_help" => Some("zz_value zz_args_help(zz_value h, zz_value prog, int *err);"),
        "zz_args_error" => Some("zz_value zz_args_error(zz_value h, int *err);"),
        "zz_args_was_help" => Some("zz_value zz_args_was_help(zz_value h, int *err);"),
        "zz_process_run" => Some("zz_value zz_process_run(zz_value cmd, zz_value argv, int *err);"),
        "zz_process_run_with_env" => {
            Some("zz_value zz_process_run_with_env(zz_value cmd, zz_value argv, zz_value env, int *err);")
        }
        "zz_process_spawn" => Some("zz_value zz_process_spawn(zz_value cmd, zz_value argv, int *err);"),
        "zz_process_wait" => Some("zz_value zz_process_wait(zz_value id, int *err);"),
        "zz_process_exit" => Some("zz_value zz_process_exit(zz_value code, int *err);"),
        "zz_process_pid" => Some("zz_value zz_process_pid(zz_value unit, int *err);"),
        "zz_uuid_v4" => Some("zz_value zz_uuid_v4(zz_value unit, int *err);"),
        "zz_uuid_v7" => Some("zz_value zz_uuid_v7(zz_value unit, int *err);"),
        "zz_uuid_parse" => Some("zz_value zz_uuid_parse(zz_value s, int *err);"),
        "zz_tls_connect" => {
            Some("zz_value zz_tls_connect(zz_value addr, zz_value name, zz_value timeout, int *err);")
        }
        "zz_tls_connect_with_ca" => Some(
            "zz_value zz_tls_connect_with_ca(zz_value addr, zz_value name, zz_value ca, zz_value timeout, int *err);",
        ),
        "zz_tls_connect_insecure" => Some(
            "zz_value zz_tls_connect_insecure(zz_value addr, zz_value name, zz_value timeout, int *err);",
        ),
        "zz_tls_write" => Some("zz_value zz_tls_write(zz_value id, zz_value data, int *err);"),
        "zz_tls_write_bytes" => {
            Some("zz_value zz_tls_write_bytes(zz_value id, zz_value data, int *err);")
        }
        "zz_tls_read" => Some("zz_value zz_tls_read(zz_value id, zz_value max, int *err);"),
        "zz_tls_read_bytes" => {
            Some("zz_value zz_tls_read_bytes(zz_value id, zz_value max, int *err);")
        }
        "zz_tls_read_exact" => {
            Some("zz_value zz_tls_read_exact(zz_value id, zz_value n, int *err);")
        }
        "zz_tls_read_line" => Some("zz_value zz_tls_read_line(zz_value id, int *err);"),
        "zz_tls_set_read_timeout" => {
            Some("zz_value zz_tls_set_read_timeout(zz_value id, zz_value ms, int *err);")
        }
        "zz_tls_set_write_timeout" => {
            Some("zz_value zz_tls_set_write_timeout(zz_value id, zz_value ms, int *err);")
        }
        "zz_tls_close" => Some("zz_value zz_tls_close(zz_value id, int *err);"),
        "zz_tls_peer_addr" => Some("zz_value zz_tls_peer_addr(zz_value id, int *err);"),
        "zz_unix_connect" => {
            Some("zz_value zz_unix_connect(zz_value path, zz_value timeout, int *err);")
        }
        "zz_unix_listen" => Some("zz_value zz_unix_listen(zz_value path, int *err);"),
        "zz_unix_accept" => Some("zz_value zz_unix_accept(zz_value id, int *err);"),
        "zz_unix_listener_close" => {
            Some("zz_value zz_unix_listener_close(zz_value id, int *err);")
        }
        "zz_unix_write" => Some("zz_value zz_unix_write(zz_value id, zz_value data, int *err);"),
        "zz_unix_write_bytes" => {
            Some("zz_value zz_unix_write_bytes(zz_value id, zz_value data, int *err);")
        }
        "zz_unix_read" => Some("zz_value zz_unix_read(zz_value id, zz_value max, int *err);"),
        "zz_unix_read_bytes" => {
            Some("zz_value zz_unix_read_bytes(zz_value id, zz_value max, int *err);")
        }
        "zz_unix_read_exact" => {
            Some("zz_value zz_unix_read_exact(zz_value id, zz_value n, int *err);")
        }
        "zz_unix_read_line" => Some("zz_value zz_unix_read_line(zz_value id, int *err);"),
        "zz_unix_set_read_timeout" => {
            Some("zz_value zz_unix_set_read_timeout(zz_value id, zz_value ms, int *err);")
        }
        "zz_unix_set_write_timeout" => {
            Some("zz_value zz_unix_set_write_timeout(zz_value id, zz_value ms, int *err);")
        }
        "zz_unix_close" => Some("zz_value zz_unix_close(zz_value id, int *err);"),
        "zz_tls_listen" => Some(
            "zz_value zz_tls_listen(zz_value addr, zz_value cert, zz_value key, int *err);",
        ),
        "zz_tls_accept" => Some("zz_value zz_tls_accept(zz_value id, int *err);"),
        "zz_tls_listener_close" => Some("zz_value zz_tls_listener_close(zz_value id, int *err);"),
        "zz_uuid_is_valid" => Some("zz_value zz_uuid_is_valid(zz_value s, int *err);"),
        _ => None,
    }
}

/// Build the FFI prelude: C declarations for all FFI symbols used by the
/// given set of native names.
pub fn ffi_prelude(natives: &std::collections::HashSet<String>) -> String {
    let mut symbols: Vec<&str> = natives.iter().filter_map(|n| ffi_impl(n)).collect();
    symbols.sort_unstable();
    symbols.dedup();
    if symbols.is_empty() {
        return String::new();
    }
    let mut out = String::from(FFI_H);
    out.push('\n');
    for sym in symbols {
        if let Some(decl) = ffi_decl(sym) {
            out.push_str(decl);
            out.push('\n');
        }
    }
    out
}

/// FFI header declarations included in every generated C module.
pub const FFI_H: &str = r#"
#include <stdint.h>
#include <stddef.h>
#include <stdbool.h>
uint64_t zz_rt_version(void);
uint64_t zz_rt_handle_alloc(const uint8_t *tag, size_t tag_len);
bool zz_rt_handle_drop(uint64_t id);
uint64_t zz_rt_handle_live(void);
bool zz_rt_handle_tag_eq(uint64_t id, const uint8_t *tag, size_t tag_len);
"#;
