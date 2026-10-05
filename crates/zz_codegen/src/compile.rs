//! C compilation: turn generated C source into a native binary.
//!
//! Single-backend model: Clang everywhere (system `clang`, or `zig cc` as
//! the provider). One flag set for dev inspection (`-O0 -g`) and one for
//! release (`-O3 -flto=thin`, ThinLTO always). Cross-compilation goes
//! through `--target=<triple>`; the host toolchain is never required to
//! match the target.
//!
//! Cross edge-case rules (enforced by [`validate`]):
//! - `-march=native` is host-only: dropped whenever `--target` is present.
//! - `--pgo` with a foreign `--target` is rejected.
//! - `--static` on macOS (`apple-darwin`) targets is rejected.
//! - cross builds link with `-fuse-ld=lld`; Windows triples link `ws2_32`.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

/// Error surfaced from a build.
#[derive(Debug)]
pub enum BuildError {
    /// No Clang provider found on PATH.
    NoClang,
    /// `--pgo` was combined with a foreign `--target`.
    /// Display text is the exact CLI contract.
    PgoCross,
    /// `--static` was requested for a macOS target.
    /// Display text is the exact CLI contract.
    StaticMacos,
    /// The C compiler failed with `stderr`.
    CompileFailed { stderr: String },
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
                "no clang found (tried clang, clang-22, zig); \
                 install clang 18+ or zig for -p builds"
            ),
            BuildError::PgoCross => {
                write!(f, "Error: --pgo requires a native build target")
            }
            BuildError::StaticMacos => {
                write!(
                    f,
                    "Error: Static binaries are not supported on macOS targets"
                )
            }
            BuildError::CompileFailed { stderr } => write!(f, "C compile failed:\n{stderr}"),
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

/// Provider preference for Clang detection (`--cc` flag).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum ClangProvider {
    /// Probe `clang`, then `clang-22`, then `zig`.
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

/// Probe whether fully-static linking works with `clang` (static
/// `libcurl`/`libsqlite3` present). Only the libraries the program
/// actually needs are probed: with conditional linking, programs that
/// neither fetch nor query need no static syslibs at all. Used to
/// downgrade a *default* static build to dynamic with a note instead of
/// failing it on machines without the static system libraries. Results
/// are cached per provider + lib set; a probe that cannot run fails open
/// (proceed static — the real link surfaces any problem).
pub fn static_syslibs_available(clang: &Clang) -> bool {
    static_syslibs_available_for(clang, true, true)
}

/// [`static_syslibs_available`] scoped to the program's actual needs.
/// `need_curl`/`need_sqlite` come from lowering (`curl_link`/
/// `sqlite_link`); when neither is needed the probe trivially passes.
pub fn static_syslibs_available_for(clang: &Clang, need_curl: bool, need_sqlite: bool) -> bool {
    use std::collections::HashMap;
    use std::sync::{Mutex, OnceLock};
    static CACHE: OnceLock<Mutex<HashMap<String, bool>>> = OnceLock::new();
    let key = format!(
        "{}:{}:{need_curl}:{need_sqlite}",
        clang.path.display(),
        clang.zig
    );
    if let Some(hit) = CACHE
        .get_or_init(Mutex::default)
        .lock()
        .ok()
        .and_then(|m| m.get(&key).copied())
    {
        return hit;
    }
    let ok = static_syslibs_probe(clang, need_curl, need_sqlite);
    if let Some(mut m) = CACHE.get().and_then(|c| c.lock().ok()) {
        m.insert(key, ok);
    }
    ok
}

fn static_syslibs_probe(clang: &Clang, need_curl: bool, need_sqlite: bool) -> bool {
    if !need_curl && !need_sqlite {
        return true;
    }
    let dir = std::env::temp_dir().join(format!("zz-static-probe-{}", std::process::id()));
    if std::fs::create_dir_all(&dir).is_err() {
        return true;
    }
    let out = dir.join("probe");
    let mut cmd = std::process::Command::new(&clang.path);
    if clang.zig {
        cmd.arg("cc");
    }
    // Empty TU + `-static` + needed system libs: succeeds only when the
    // static archives exist in the linker search path.
    cmd.arg("-static");
    if need_curl {
        cmd.arg("-lcurl");
    }
    if need_sqlite {
        cmd.arg("-lsqlite3");
    }
    let r = cmd
        .arg("-o")
        .arg(&out)
        .arg("-x")
        .arg("c")
        .arg("/dev/null")
        .output();
    let _ = std::fs::remove_dir_all(&dir);
    match r {
        Ok(o) => o.status.success(),
        Err(_) => true,
    }
}

/// Managed Zig toolchain root: `~/.zz/toolchain` (override with
/// `ZZ_TOOLCHAIN_ROOT`, used by hermetic tests). Layout:
/// `versions/<semver>/` holds one extracted Zig release (the `zig`
/// binary plus its adjacent `lib/`); a `pin` file names the active
/// version. Written by `zz toolchain install`; read here so builds and
/// the runtime-archive cache follow the pin.
pub fn toolchain_root() -> PathBuf {
    if let Some(root) = std::env::var_os("ZZ_TOOLCHAIN_ROOT") {
        if !root.is_empty() {
            return PathBuf::from(root);
        }
    }
    toolchain_home_dir()
        .unwrap_or_else(|| std::env::temp_dir().join("zz"))
        .join(".zz")
        .join("toolchain")
}

fn toolchain_home_dir() -> Option<PathBuf> {
    #[cfg(unix)]
    {
        std::env::var_os("HOME").map(PathBuf::from)
    }
    #[cfg(windows)]
    {
        std::env::var("USERPROFILE").ok().map(PathBuf::from)
    }
}

/// Pinned Zig version (`versions/<pin>/`), if `zz toolchain install` (or
/// `zz toolchain use`) recorded one. The file holds `X.Y.Z` plus a
/// trailing newline; anything unparseable is treated as unpinned.
pub fn toolchain_pin() -> Option<String> {
    let pin = std::fs::read_to_string(toolchain_root().join("pin")).ok()?;
    let pin = pin.trim().to_string();
    if pin.is_empty()
        || !pin.bytes().any(|b| b.is_ascii_alphanumeric())
        || !pin.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'.')
    {
        return None;
    }
    Some(pin)
}

/// Path to the managed `zig` binary for the pinned version, when the
/// install is present and executable. Returns `None` when unpinned,
/// incompletely installed, or not executable (falls back to PATH probing).
/// Honors `ZZ_TEST_HIDE_CLANG` like [`detect_clang_with`].
pub fn managed_zig_path() -> Option<PathBuf> {
    if std::env::var_os("ZZ_TEST_HIDE_CLANG").is_some() {
        return None;
    }
    let pin = toolchain_pin()?;
    let exe = if cfg!(windows) { "zig.exe" } else { "zig" };
    let path = toolchain_root().join("versions").join(&pin).join(exe);
    if !path.is_file() {
        return None;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if std::fs::metadata(&path)
            .map(|m| m.permissions().mode() & 0o111 == 0)
            .unwrap_or(true)
        {
            return None;
        }
    }
    Some(path)
}

/// Probe PATH for a Clang provider.
///
/// Order: managed `zig` (an explicit `zz toolchain install` pin always
/// wins), then `clang`, `clang-22`, then PATH `zig`. Test hook: when the
/// environment variable `ZZ_TEST_HIDE_CLANG` is set, detection pretends
/// nothing is installed (used by the missing-toolchain fallback tests).
pub fn detect_clang() -> Option<Clang> {
    detect_clang_with(ClangProvider::Any)
}

/// Probe PATH for a Clang provider, honoring a `--cc` preference.
pub fn detect_clang_with(provider: ClangProvider) -> Option<Clang> {
    if std::env::var_os("ZZ_TEST_HIDE_CLANG").is_some() {
        return None;
    }
    let allow_clang = matches!(provider, ClangProvider::Any | ClangProvider::Clang);
    let allow_zig = matches!(provider, ClangProvider::Any | ClangProvider::Zig);
    // Explicit opt-in wins: a pinned managed toolchain outranks everything
    // on PATH (`--cc=clang` / `--cc=zig` still force their provider).
    if provider == ClangProvider::Any {
        if let Some(path) = managed_zig_path() {
            return Some(Clang {
                path,
                zig: true,
                label: "zig cc",
            });
        }
    }
    if allow_clang {
        for name in ["clang", "clang-22"] {
            if let Some(path) = which(name) {
                return Some(Clang {
                    path,
                    zig: false,
                    label: "clang",
                });
            }
        }
    }
    if allow_zig {
        if let Some(path) = managed_zig_path().or_else(|| which("zig")) {
            return Some(Clang {
                path,
                zig: true,
                label: "zig cc",
            });
        }
    }
    None
}

fn which(name: &str) -> Option<PathBuf> {
    let path_var = std::env::var_os("PATH")?;
    for dir in std::env::split_paths(&path_var) {
        let candidate = dir.join(name);
        if candidate.is_file() {
            return Some(candidate);
        }
        #[cfg(windows)]
        {
            let exe = dir.join(format!("{name}.exe"));
            if exe.is_file() {
                return Some(exe);
            }
        }
    }
    None
}

/// Normalized `(arch, os)` pair for triple comparison.
///
/// Aliases are folded so `x86_64-pc-linux-gnu` and
/// `x86_64-unknown-linux-gnu` (or an explicit `--target` repeating the
/// host) compare equal instead of tripping the PGO-cross guard.
fn norm_triple(triple: &str) -> (String, String) {
    let t = triple.to_lowercase();
    let arch = t
        .split('-')
        .next()
        .unwrap_or("")
        .replace("amd64", "x86_64")
        .replace("arm64", "aarch64");
    let os = if t.contains("windows") || t.contains("win32") || t.contains("msvc") {
        "windows".to_string()
    } else if t.contains("darwin") || t.contains("macos") || t.contains("apple") {
        "macos".to_string()
    } else if t.contains("linux") {
        "linux".to_string()
    } else {
        t.clone()
    };
    (arch, os)
}

/// Host triple, e.g. `x86_64-unknown-linux-gnu`.
///
/// Prefers `rustc -vV` (`host: <triple>`); falls back to
/// `std::env::consts` mapping when rustc is unavailable.
pub fn host_triple() -> String {
    if let Ok(out) = Command::new("rustc").arg("-vV").output() {
        if out.status.success() {
            let text = String::from_utf8_lossy(&out.stdout);
            for line in text.lines() {
                if let Some(host) = line.strip_prefix("host: ") {
                    let host = host.trim().to_string();
                    if !host.is_empty() {
                        return host;
                    }
                }
            }
        }
    }
    let arch = match std::env::consts::ARCH {
        "x86_64" => "x86_64",
        "aarch64" => "aarch64",
        "arm" => "arm",
        "x86" => "i686",
        "riscv64" => "riscv64",
        other => other,
    };
    let os = match std::env::consts::OS {
        "linux" => "unknown-linux-gnu",
        "macos" => "apple-darwin",
        "windows" => "pc-windows-msvc",
        other => other,
    };
    format!("{arch}-{os}")
}

/// True when `target` names a macOS/Apple triple.
pub fn is_macos_target(target: &str) -> bool {
    norm_triple(target).1 == "macos"
}

/// True when `target` names a Windows triple.
pub fn is_windows_target(target: &str) -> bool {
    norm_triple(target).1 == "windows"
}

/// True when `target` differs from the host (arch or OS).
fn is_foreign_target(target: &str) -> bool {
    norm_triple(target) != norm_triple(&host_triple())
}

/// Enforce the cross-compilation edge-case rules.
///
/// - `--pgo` + foreign `--target` → [`BuildError::PgoCross`].
/// - `--static` on a macOS target (or macOS host without `--target`) →
///   [`BuildError::StaticMacos`].
pub fn validate(opts: &BuildOptions, target: Option<&str>) -> Result<(), BuildError> {
    if opts.pgo != PgoMode::None {
        if let Some(t) = target {
            if is_foreign_target(t) {
                return Err(BuildError::PgoCross);
            }
        }
    }
    if opts.static_link {
        let macos = match target {
            Some(t) => is_macos_target(t),
            // Native macOS host without --target: static link of system
            // libraries is equally unsupported.
            None => cfg!(target_os = "macos"),
        };
        if macos {
            return Err(BuildError::StaticMacos);
        }
    }
    Ok(())
}

/// PGO (Profile-Guided Optimization) mode.
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

/// One embedded asset: virtual path (slash-separated, relative) + bytes.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct EmbedAsset {
    pub name: String,
    pub bytes: Vec<u8>,
}

/// Build options.
#[derive(Debug, Clone, Default)]
pub struct BuildOptions {
    /// Optimization level: false = `-O0 -g` (dev inspection), true = release.
    pub optimize: bool,
    /// Strip the binary (`-s`).
    pub strip: bool,
    /// Static link (rejected on macOS targets by [`validate`]).
    pub static_link: bool,
    /// Enable function-section + gc-sections (DCE at link level).
    pub gc_sections: bool,
    /// Kept for fingerprint compatibility; release always uses ThinLTO.
    /// New code should not branch on this.
    pub thin_lto: bool,
    /// PGO mode for profile-guided optimization (native only).
    pub pgo: PgoMode,
    /// Link the Rust native runtime (`libzz_native_rt.a`) for FFI natives.
    /// Set automatically from the lowered program; tests can opt in directly.
    pub native_rt: bool,
    /// Force-extract the Postgres objects from the static archive (`-u`).
    /// Set automatically alongside `native_rt` when sqlz/pg natives are
    /// reachable (the C dispatcher's weak refs never pull members alone).
    pub pg_link: bool,
    /// Force-extract the float-format object (`-u zz_float_format_raw`).
    /// Set automatically when any typed node can hold a float (float
    /// Display routes through the Rust core per spec §4). Implies the
    /// staticlib link like `native_rt`.
    pub float_link: bool,
    /// Link libcurl (outbound `http.get/post/fetch` client). Set
    /// automatically from the lowered program; programs that never fetch
    /// omit `-lcurl` entirely so no `DT_NEEDED` entry is emitted.
    pub curl_link: bool,
    /// Link libsqlite3 (`sqlz`/`db` queries). Set automatically from the
    /// lowered program; programs that never query omit `-lsqlite3`.
    pub sqlite_link: bool,
    /// Allow silently downgrading `static_link` to dynamic when static is
    /// impossible (macOS target, or the program needs the Rust native
    /// runtime). Set by the CLI only when static came from the *default*,
    /// never from an explicit `--static` (which keeps today's hard error).
    /// Part of the cache fingerprint: downgraded output differs.
    pub allow_static_downgrade: bool,
    /// Full LTO (`-flto=full` instead of thin) for `--full` max-opt
    /// builds: deeper cross-TU optimization and elimination at the cost
    /// of slower links. Only meaningful with `optimize` (release base).
    pub full_lto: bool,
    /// Extra object files / static libraries from plugin packages to link
    /// into the final binary. Each entry is a path to a `.o` or `.a` file
    /// produced by a plugin's build hook.
    pub plugin_artifacts: Vec<std::path::PathBuf>,
    /// Extra raw linker flags from plugin packages (the `ldflags.txt`
    /// files their build hooks emit, e.g. `-lvips -lgio-2.0 ...`).
    pub plugin_link_args: Vec<String>,
    /// Static assets baked into the binary (`--embed`): emitted as byte
    /// tables + a static initializer calling `zz_embed_register`, served
    /// at runtime through `fs.embedfs()`. Empty = no embedding.
    pub embed_assets: Vec<EmbedAsset>,
}

impl BuildOptions {
    /// Debug build: fast native compile (`-O0 -g`, no LTO).
    /// Used for `zz build --dynamic` and parity sweeps (`ZZ_NATIVE_DEV=1`).
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
        }
    }

    /// Release build: `-O3 -flto=thin`, stripped, dynamic.
    /// ThinLTO is unconditional — Clang is the only backend now.
    pub fn release() -> Self {
        BuildOptions {
            optimize: true,
            strip: true,
            static_link: false, // dynamic - faster for dev/benchmark
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
        }
    }

    /// Static self-contained build: ThinLTO + DCE + strip.
    /// Use for production Docker/Serverless deployments (non-macOS).
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
        }
    }

    /// PGO build: generate profile data (native host only).
    /// Phase 1: build with this, run the binary, then rebuild with `pgo_use()`.
    pub fn pgo_generate() -> Self {
        BuildOptions {
            optimize: true,
            strip: false, // keep symbols for PGO
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
        }
    }

    /// Max optimization (`--full`): release base with full LTO —
    /// whole-program optimization and elimination, dynamic link,
    /// stripped. Slower links than ThinLTO release.
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
        }
    }

    /// Max optimization with PGO (`--full -- <train args>`): profile-use
    /// plus full LTO. The instrumented leg uses plain `pgo_generate()`.
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
        }
    }

    /// PGO build: use collected profile data (native host only).
    /// Phase 2: run after `pgo_generate()` collected profile data.
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
        }
    }
}

impl BuildOptions {
    /// Compute a fingerprint hash of this BuildOptions for cache key generation.
    /// Includes all flags that affect compilation output, plus the target
    /// triple (a cross build must never collide with a native one, and
    /// `-march=native` presence differs between them).
    pub fn fingerprint(&self) -> u64 {
        self.fingerprint_with(None)
    }

    /// Fingerprint including the `--target` triple.
    pub fn fingerprint_with(&self, target: Option<&str>) -> u64 {
        use std::hash::{Hash, Hasher};
        let mut h = std::collections::hash_map::DefaultHasher::new();
        self.optimize.hash(&mut h);
        self.strip.hash(&mut h);
        self.static_link.hash(&mut h);
        self.gc_sections.hash(&mut h);
        self.thin_lto.hash(&mut h);
        self.pgo.hash(&mut h);
        self.native_rt.hash(&mut h);
        self.pg_link.hash(&mut h);
        self.float_link.hash(&mut h);
        self.curl_link.hash(&mut h);
        self.sqlite_link.hash(&mut h);
        self.allow_static_downgrade.hash(&mut h);
        self.full_lto.hash(&mut h);
        // Hash plugin artifact paths so cache invalidates when plugins change.
        for p in &self.plugin_artifacts {
            p.hash(&mut h);
        }
        // Hash plugin link flags too (dependency lib sets change output).
        for a in &self.plugin_link_args {
            a.hash(&mut h);
        }
        // Hash embedded assets (name + bytes): changed assets must bust
        // the cache or binaries would serve stale files.
        for a in &self.embed_assets {
            a.name.hash(&mut h);
            a.bytes.hash(&mut h);
        }
        target.unwrap_or("host").hash(&mut h);
        h.finish()
    }
}

/// Render embedded assets as C byte tables plus a static initializer that
/// registers each file with the runtime embed table before `main` runs.
/// Output is appended to the generated program C (per-binary data — never
/// part of the precompiled runtime archive).
pub fn embed_c(assets: &[EmbedAsset]) -> String {
    if assets.is_empty() {
        return String::new();
    }
    let mut out = String::from(
        "\n// ---- zz --embed assets (generated) ------------------------------------\n",
    );
    for (i, a) in assets.iter().enumerate() {
        out.push_str(&format!("static const unsigned char zz_embed_{i}[] = {{"));
        for (k, b) in a.bytes.iter().enumerate() {
            if k % 12 == 0 {
                out.push('\n');
            }
            out.push_str(&format!("{b},"));
        }
        out.push_str("\n};\n");
        // C-escape the virtual path for the string literal.
        let mut esc = String::with_capacity(a.name.len() + 2);
        for c in a.name.chars() {
            match c {
                '\\' => esc.push_str("\\\\"),
                '"' => esc.push_str("\\\""),
                '\n' => esc.push_str("\\n"),
                c => esc.push(c),
            }
        }
        out.push_str(&format!(
            "static void zz_embed_init_{i}(void) __attribute__((constructor));\n\
             static void zz_embed_init_{i}(void) {{\n\
             \x20   zz_embed_register(\"{esc}\", zz_embed_{i}, sizeof(zz_embed_{i}));\n\
             }}\n"
        ));
    }
    out
}

/// The single release flag set (ThinLTO always).
///
/// Integer arithmetic is defined-wrapping (`-fwrapv`) and aliasing is
/// conservative (`-fno-strict-aliasing`) in every mode: both engines
/// specify wrap semantics and the C runtime type-puns `zz_value`, so
/// aggressive assumptions would be miscompiles, not optimizations.
/// There is deliberately NO `-ffast-math` (it folds NaN guards and
/// reassociates floats — incompatible with the specified float
/// semantics) and NO `-march=native` (host-CPU-specific codegen breaks
/// reproducible parity and benchmarks).
///
/// - cross builds add `-fuse-ld=lld`; Windows triples add `-lws2_32`.
pub fn clang_flags(opts: &BuildOptions, target: Option<&str>) -> Vec<String> {
    let mut flags: Vec<String> = Vec::new();
    // Wrapping + aliasing contract first: applies to dev and release.
    flags.push("-fwrapv".to_string());
    flags.push("-fno-strict-aliasing".to_string());
    if opts.optimize {
        flags.push("-O3".to_string());
        if opts.full_lto {
            // Max-opt (`--full`): whole-program LTO. Slower links, better
            // cross-TU inlining and elimination than ThinLTO.
            flags.push("-flto=full".to_string());
        } else {
            // ThinLTO unconditionally otherwise: Clang is the only backend.
            flags.push("-flto=thin".to_string());
        }
        // Loop/codegen tuning (FP-safe only: no -ffast-math, see above).
        flags.push("-funroll-loops".to_string());
        flags.push("-fomit-frame-pointer".to_string());
    } else {
        flags.push("-O0".to_string());
        flags.push("-g".to_string());
    }
    if opts.strip {
        flags.push("-s".to_string());
    }
    if opts.static_link {
        flags.push("-static".to_string());
    }
    if opts.gc_sections {
        flags.push("-ffunction-sections".to_string());
        flags.push("-fdata-sections".to_string());
        flags.push("-Wl,--gc-sections".to_string());
    }
    match opts.pgo {
        PgoMode::Generate => {
            flags.push("-fprofile-generate".to_string());
        }
        PgoMode::Use => {
            flags.push("-fprofile-use".to_string());
            flags.push("-fno-peel-loops".to_string());
        }
        PgoMode::None => {}
    }
    if let Some(t) = target {
        flags.push(format!("--target={t}"));
        // The system linker may not understand foreign triples.
        flags.push("-fuse-ld=lld".to_string());
        if is_windows_target(t) {
            flags.push("-lws2_32".to_string());
        }
    }
    flags
}

/// Compile C source (already including the runtime) to a binary at
/// `output_path` using the detected Clang provider.
///
/// `target` is the `--target=<triple>` cross triple, or `None` for a
/// native host build.
pub fn build(
    source: &str,
    output_path: &Path,
    opts: BuildOptions,
    target: Option<&str>,
) -> Result<Clang, BuildError> {
    let clang = detect_clang().ok_or(BuildError::NoClang)?;
    build_with(source, output_path, opts, target, &clang)
}

/// [`build`] with an explicit provider (honors `--cc` selection).
pub fn build_with(
    source: &str,
    output_path: &Path,
    opts: BuildOptions,
    target: Option<&str>,
    clang: &Clang,
) -> Result<Clang, BuildError> {
    validate(&opts, target)?;
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let uniq = COUNTER.fetch_add(1, Ordering::SeqCst);

    // Write the source to a unique temp .c file (tests run in parallel).
    let tmpdir = std::env::temp_dir().join(format!("zz-build-{}-{uniq}", std::process::id()));
    std::fs::create_dir_all(&tmpdir)?;
    let src_path = tmpdir.join("prog.c");
    // Per-binary `--embed` tables ride along in the program TU (never in
    // the precompiled archive, which is shared across programs).
    let full_source = if opts.embed_assets.is_empty() {
        source.to_string()
    } else {
        format!("{source}\n{}", embed_c(&opts.embed_assets))
    };
    std::fs::write(&src_path, &full_source)?;

    // Debug aid: ZZ_DUMP_C=/some/path.c writes the generated C to that path
    // before compilation. Used by perf investigations; no production code
    // depends on it.
    if let Ok(path) = std::env::var("ZZ_DUMP_C") {
        let _ = std::fs::write(&path, &full_source);
    }

    let mut cmd = Command::new(&clang.path);
    if clang.zig {
        cmd.arg("cc");
        if let Some(t) = target {
            // zig cc takes the target as a separate `-target` flag.
            cmd.arg("-target").arg(t);
        }
    }
    for flag in clang_flags(&opts, if clang.zig { None } else { target }) {
        // For zig the target travels via `-target`, not `--target=`.
        if clang.zig && flag.starts_with("--target=") {
            continue;
        }
        cmd.arg(flag);
    }
    cmd.arg("-o")
        .arg(output_path)
        .arg(&src_path)
        // libm stays unconditional (archive members like float binops
        // need it regardless of position).
        .arg("-lm")
        // sqlz: prepared-statement FFI needs sqlite3 headers.
        .arg("-DZZ_HAS_SQLITE3");
    // Precompiled C runtime archive: the runtime sources (core.c, strings.c,
    // collections.c, json.c, memory.c) are compiled once and cached. The
    // generated C only contains headers (declarations) + user code.
    match crate::cache::ensure_rt_a(&opts, clang, target) {
        Ok(rt_a) => {
            if let Some(parent) = rt_a.parent() {
                cmd.arg(format!("-L{}", parent.display()));
            }
            cmd.arg("-lzz_rt");
        }
        Err(e) => {
            eprintln!("zz: warning: precompiled runtime .a failed: {e}; falling back to embedded");
        }
    }
    // Unified Rust native runtime: link the static library providing FFI
    // natives. Fully-static binaries cannot use it (shared libstd), so fail
    // early with a clear message instead of a cryptic `ld` error.
    if opts.native_rt || opts.float_link {
        if opts.static_link {
            return Err(BuildError::NativeRt {
                reason: "fully-static builds cannot link the Rust native runtime \
                         (it needs the shared libstd); use `zz build` or `zz build -p`"
                    .to_string(),
            });
        }
        let extra = crate::ffi::link_args(opts.optimize)
            .map_err(|e| BuildError::NativeRt { reason: e.0 })?;
        // Postgres backend: the C dispatcher references the symbols
        // weakly (so sqlite-only programs link cleanly without the
        // staticlib); force-extract the objects whenever the gate fired.
        // `-u` precedes the archive that satisfies it (linker order).
        if opts.pg_link {
            for sym in crate::ffi::PG_LINK_SYMBOLS {
                cmd.arg("-u");
                cmd.arg(sym);
            }
        }
        // Float Display (spec §4): same weak-ref situation — pull the
        // Rust-core formatter explicitly when the gate fired.
        if opts.float_link {
            cmd.arg("-u");
            cmd.arg(crate::ffi::FLOAT_FMT_SYMBOL);
        }
        for a in &extra {
            cmd.arg(a);
        }
    }
    // Link plugin package artifacts (compiled .o / .a from build hooks).
    for artifact in &opts.plugin_artifacts {
        cmd.arg(artifact);
    }
    // Link plugin dependency libraries (e.g. `-lvips` from ldflags.txt).
    for arg in &opts.plugin_link_args {
        cmd.arg(arg);
    }
    // Scoped --as-needed LAST: with per-function sections + --gc-sections,
    // programs that never fetch/sql drop those runtime functions, so the
    // heavy libs (curl/TLS tree, sqlite) are skipped instead of faulting
    // ~1 MB of constructors at load. Placed after the runtime archive
    // (and every staticlib/plugin) so genuinely-needed refs keep them.
    // libm stays unconditional above (position-independent for shared).
    // Conditional link (fix/link-hygiene): `-lcurl`/`-lsqlite3` are added
    // only when reachable natives need them (`curl_link`/`sqlite_link`
    // from lowering). Previously both were unconditional, and the
    // single-TU runtime archive defeated `--as-needed` for sqlite: the
    // whole TU's undefined refs (including `sqlite3_*`) were visible at
    // the `--as-needed` decision point, so `DT_NEEDED libsqlite3` stuck
    // even when `--gc-sections` later removed every `zz_db_*` section.
    // Omitting the flag entirely leaves no NEEDED entry and links fine.
    if opts.curl_link || opts.sqlite_link {
        cmd.arg("-Wl,--as-needed");
        if opts.curl_link {
            cmd.arg("-lcurl");
        }
        if opts.sqlite_link {
            cmd.arg("-lsqlite3");
        }
        cmd.arg("-Wl,--no-as-needed");
    }

    let out = cmd.output().map_err(BuildError::Io)?;
    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
        return Err(BuildError::CompileFailed { stderr });
    }
    Ok(clang.clone())
}

/// Emit `bin/app.c` plus reproducible native build scripts for `dir`.
///
/// Used for dev C dumps and for the release-missing-Clang artifact (so a
/// user can build manually on a machine with Clang installed). Single
/// clang command line — no per-backend branching.
pub fn emit_c_plus_script(
    source: &str,
    dir: &Path,
    target: Option<&str>,
    opts: &BuildOptions,
) -> Result<(PathBuf, PathBuf, PathBuf), std::io::Error> {
    std::fs::create_dir_all(dir)?;
    let app_c = dir.join("app.c");
    // Manual builds must see the same bytes the real build compiles,
    // including `--embed` tables.
    let full_source = if opts.embed_assets.is_empty() {
        source.to_string()
    } else {
        format!("{source}\n{}", embed_c(&opts.embed_assets))
    };
    std::fs::write(&app_c, &full_source)?;

    let mut flags = clang_flags(opts, target);
    // Scripts always show the portable command: drop build-machine-specific
    // `-march=native` so the script is safe to run on other hosts.
    flags.retain(|f| f != "-march=native");
    let flag_str = flags.join(" ");
    // Reproduce plugin inputs (artifacts + link flags) in manual builds.
    let mut extra_inputs = String::new();
    for a in &opts.plugin_artifacts {
        extra_inputs.push(' ');
        extra_inputs.push_str(&a.display().to_string());
    }
    for a in &opts.plugin_link_args {
        extra_inputs.push(' ');
        extra_inputs.push_str(a);
    }
    let target_out = match target {
        Some(t) if is_windows_target(t) => "app.exe",
        Some(_) => "app",
        None => {
            if cfg!(windows) {
                "app.exe"
            } else {
                "app"
            }
        }
    };

    // Conditional system libs: mirror `build_with` so manual builds link
    // exactly what the real build links (no phantom sqlite/curl NEEDED).
    let mut syslibs = String::new();
    if opts.curl_link || opts.sqlite_link {
        syslibs.push_str(" -Wl,--as-needed");
        if opts.curl_link {
            syslibs.push_str(" -lcurl");
        }
        if opts.sqlite_link {
            syslibs.push_str(" -lsqlite3");
        }
        syslibs.push_str(" -Wl,--no-as-needed");
    }

    let sh = dir.join("build.sh");
    std::fs::write(
        &sh,
        format!(
            "#!/bin/sh\n# Generated by `zz build`. Requires clang 18+ (or: replace `clang` with `zig cc -target <triple>`).\nset -e\ncd \"$(dirname \"$0\")\"\nclang {flag_str} -o {target_out} app.c{extra_inputs} -lm{syslibs} -DZZ_HAS_SQLITE3\n"
        ),
    )?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if let Ok(meta) = std::fs::metadata(&sh) {
            let mut perms = meta.permissions();
            perms.set_mode(perms.mode() | 0o111);
            let _ = std::fs::set_permissions(&sh, perms);
        }
    }

    let bat = dir.join("build.bat");
    std::fs::write(
        &bat,
        format!(
            "@echo off\r\nREM Generated by `zz build`. Requires clang (LLVM) on PATH.\r\ncd /d %~dp0\r\nclang {flag_str} -o {target_out} app.c{extra_inputs} -lm{syslibs} -DZZ_HAS_SQLITE3\r\n"
        ),
    )?;
    Ok((app_c, sh, bat))
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
/// Native host build (no cross target).
pub fn compile_and_run(
    source: &str,
    opts: BuildOptions,
    args: &[&str],
) -> Result<(i32, String), BuildError> {
    compile_and_run_for_target(source, opts, None, args)
}

/// [`compile_and_run`] with an explicit cross target.
pub fn compile_and_run_for_target(
    source: &str,
    opts: BuildOptions,
    target: Option<&str>,
    args: &[&str],
) -> Result<(i32, String), BuildError> {
    let tmpdir = transient_dir("zz-run");
    std::fs::create_dir_all(&tmpdir)?;
    let bin = tmpdir.join("zz_tmp_bin");
    build(source, &bin, opts, target)?;
    let r = run_binary(&bin, args);
    let _ = std::fs::remove_file(&bin);
    let _ = std::fs::remove_dir_all(&tmpdir);
    r
}

/// Unique temp dir for one transient build/run execution.
/// The counter (not just the pid) keeps parallel tests in the same
/// process from sharing a dir — they would otherwise overwrite and
/// delete each other's binaries mid-run.
fn transient_dir(prefix: &str) -> std::path::PathBuf {
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let uniq = COUNTER.fetch_add(1, Ordering::SeqCst);
    std::env::temp_dir().join(format!("{prefix}-{}-{uniq}", std::process::id()))
}

/// Returns the temp binary path without deleting it (for `zz build`).
/// Native host build (no cross target).
pub fn build_to_temp(source: &str, opts: BuildOptions) -> Result<(PathBuf, Clang), BuildError> {
    let tmpdir = transient_dir("zz-build-out");
    std::fs::create_dir_all(&tmpdir)?;
    let bin = tmpdir.join(if cfg!(windows) {
        "zz_out.exe"
    } else {
        "zz_out"
    });
    let cc = build(source, &bin, opts, None)?;
    Ok((bin, cc))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn release_opts() -> BuildOptions {
        BuildOptions::release()
    }

    #[test]
    fn no_host_specific_or_unsafe_fp_flags() {
        // Parity contract: no -march=native anywhere (host-specific
        // codegen) and no -ffast-math anywhere (folds NaN guards,
        // reassociates floats). Both native and cross, dev and release.
        for t in [
            None,
            Some("aarch64-unknown-linux-gnu"),
            Some("x86_64-pc-windows-gnu"),
            Some("x86_64-apple-darwin"),
        ] {
            for opts in [release_opts(), BuildOptions::dev()] {
                let flags = clang_flags(&opts, t);
                assert!(
                    !flags.iter().any(|f| f == "-march=native"),
                    "forbidden -march=native: {flags:?}"
                );
                assert!(
                    !flags.iter().any(|f| f == "-ffast-math"),
                    "forbidden -ffast-math: {flags:?}"
                );
            }
        }
    }

    #[test]
    fn release_is_thin_lto_with_wrap_contract() {
        let flags = clang_flags(&release_opts(), None);
        assert!(flags.contains(&"-O3".to_string()));
        assert!(flags.contains(&"-flto=thin".to_string()));
        // Wrapping + aliasing contract (both modes — checked below).
        for opts in [release_opts(), BuildOptions::dev()] {
            let flags = clang_flags(&opts, None);
            assert!(
                flags.contains(&"-fwrapv".to_string()),
                "missing -fwrapv: {flags:?}"
            );
            assert!(
                flags.contains(&"-fno-strict-aliasing".to_string()),
                "missing -fno-strict-aliasing: {flags:?}"
            );
        }
        let dev = clang_flags(&BuildOptions::dev(), None);
        assert!(dev.contains(&"-O0".to_string()));
    }

    #[test]
    fn pgo_cross_rejected() {
        let host = host_triple();
        // Foreign triple → exact CLI error.
        let foreign = if host.contains("aarch64") {
            "x86_64-unknown-linux-gnu"
        } else {
            "aarch64-unknown-linux-gnu"
        };
        let err = validate(&BuildOptions::pgo_generate(), Some(foreign)).unwrap_err();
        assert_eq!(
            err.to_string(),
            "Error: --pgo requires a native build target"
        );
        // Same-triple-as-host explicit --target is native → allowed.
        assert!(validate(&BuildOptions::pgo_generate(), Some(&host)).is_ok());
        // No target at all → allowed.
        assert!(validate(&BuildOptions::pgo_generate(), None).is_ok());
        // Non-PGO builds never trip the guard.
        assert!(validate(&release_opts(), Some(foreign)).is_ok());
    }

    #[test]
    fn static_macos_rejected() {
        for t in ["x86_64-apple-darwin", "aarch64-apple-darwin"] {
            let err = validate(&BuildOptions::static_lto(), Some(t)).unwrap_err();
            assert_eq!(
                err.to_string(),
                "Error: Static binaries are not supported on macOS targets"
            );
        }
        // Linux static is fine.
        assert!(validate(
            &BuildOptions::static_lto(),
            Some("x86_64-unknown-linux-gnu")
        )
        .is_ok());
    }

    #[test]
    fn lld_only_cross() {
        let native = clang_flags(&release_opts(), None);
        assert!(!native.iter().any(|f| f == "-fuse-ld=lld"));
        let cross = clang_flags(&release_opts(), Some("aarch64-unknown-linux-gnu"));
        assert!(cross.iter().any(|f| f == "-fuse-ld=lld"));
    }

    #[test]
    fn ws2_32_only_windows() {
        let linux = clang_flags(&release_opts(), Some("x86_64-unknown-linux-gnu"));
        assert!(!linux.iter().any(|f| f == "-lws2_32"));
        let win = clang_flags(&release_opts(), Some("x86_64-pc-windows-gnu"));
        assert!(win.iter().any(|f| f == "-lws2_32"));
        let native = clang_flags(&release_opts(), None);
        assert!(!native.iter().any(|f| f == "-lws2_32"));
    }

    #[test]
    fn detect_clang_probes_only_clang_zig() {
        // Cannot control the real PATH portably here; assert the contract:
        // when a provider is found its label is one of the two known ones,
        // and the hidden-clang hook forces None.
        std::env::set_var("ZZ_TEST_HIDE_CLANG", "1");
        assert!(detect_clang().is_none());
        assert!(detect_clang_with(ClangProvider::Zig).is_none());
        std::env::remove_var("ZZ_TEST_HIDE_CLANG");
        if let Some(c) = detect_clang() {
            assert!(c.label == "clang" || c.label == "zig cc");
        }
    }

    #[test]
    fn target_in_fingerprint() {
        let opts = release_opts();
        assert_ne!(
            opts.fingerprint_with(None),
            opts.fingerprint_with(Some("aarch64-unknown-linux-gnu")),
            "native and cross fingerprints must not collide"
        );
    }

    #[test]
    fn emit_script_drops_march_native() {
        let dir = transient_dir("zz-emit-test");
        let opts = release_opts();
        let (_c, sh, _bat) =
            emit_c_plus_script("int main(){return 0;}", &dir, None, &opts).expect("emit");
        let text = std::fs::read_to_string(&sh).expect("read sh");
        assert!(
            !text.contains("-march=native"),
            "build scripts must stay portable: {text}"
        );
        assert!(text.contains("clang"), "script must use clang: {text}");
        let _ = std::fs::remove_dir_all(&dir);
    }
}

#[cfg(test)]
mod toolchain_tests {
    use super::*;
    use std::sync::Mutex;

    /// Env vars are process-global: serialize every hermetic test so
    /// parallel threads never observe (or remove) each other's root.
    static ENV_LOCK: Mutex<()> = Mutex::new(());

    /// Hermetic toolchain root: unique temp dir + env override, restored
    /// on drop so parallel tests never observe it afterwards. Mirrors the
    /// existing `ZZ_TEST_HIDE_CLANG` pattern (set, assert, restore fast).
    struct HermeticRoot {
        dir: PathBuf,
    }

    impl HermeticRoot {
        fn new() -> Self {
            use std::sync::atomic::{AtomicU64, Ordering};
            static COUNTER: AtomicU64 = AtomicU64::new(0);
            let uniq = COUNTER.fetch_add(1, Ordering::SeqCst);
            let dir =
                std::env::temp_dir().join(format!("zz-tc-test-{}-{uniq}", std::process::id()));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).unwrap();
            std::env::set_var("ZZ_TOOLCHAIN_ROOT", &dir);
            HermeticRoot { dir }
        }

        fn pin(&self, version: &str) {
            std::fs::write(self.dir.join("pin"), format!("{version}\n")).unwrap();
        }

        fn fake_zig(&self, version: &str) -> PathBuf {
            let exe = if cfg!(windows) { "zig.exe" } else { "zig" };
            let dir = self.dir.join("versions").join(version);
            std::fs::create_dir_all(&dir).unwrap();
            let path = dir.join(exe);
            #[cfg(unix)]
            {
                std::fs::write(&path, "#!/bin/sh\necho fake-zig\n").unwrap();
                use std::os::unix::fs::PermissionsExt;
                std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
            }
            #[cfg(windows)]
            {
                std::fs::write(&path, "fake").unwrap();
            }
            path
        }
    }

    impl Drop for HermeticRoot {
        fn drop(&mut self) {
            std::env::remove_var("ZZ_TOOLCHAIN_ROOT");
            let _ = std::fs::remove_dir_all(&self.dir);
        }
    }

    #[test]
    fn unpinned_has_no_managed_zig() {
        let _lock = ENV_LOCK.lock().unwrap();
        let _root = HermeticRoot::new();
        assert!(toolchain_pin().is_none());
        assert!(managed_zig_path().is_none());
    }

    #[test]
    fn garbage_pin_is_unpinned_not_fatal() {
        let _lock = ENV_LOCK.lock().unwrap();
        let root = HermeticRoot::new();
        for bad in ["", ".", "..", "../evil", "0.17.0\nrm -rf", "v0.17.0-rc1!"] {
            std::fs::write(root.dir.join("pin"), bad).unwrap();
            assert!(toolchain_pin().is_none(), "bad pin must not parse: {bad:?}");
            assert!(managed_zig_path().is_none());
        }
    }

    #[test]
    fn pin_without_install_is_not_managed() {
        let _lock = ENV_LOCK.lock().unwrap();
        let root = HermeticRoot::new();
        root.pin("0.17.0");
        assert_eq!(toolchain_pin().as_deref(), Some("0.17.0"));
        assert!(managed_zig_path().is_none());
    }

    #[test]
    fn managed_zig_wins_probe_and_hide_covers_it() {
        let _lock = ENV_LOCK.lock().unwrap();
        let root = HermeticRoot::new();
        let fake = root.fake_zig("0.17.0");
        root.pin("0.17.0");
        assert_eq!(managed_zig_path(), Some(fake.clone()));
        // Explicit opt-in outranks PATH.
        let found = detect_clang_with(ClangProvider::Any).expect("managed zig must probe");
        assert!(found.zig);
        assert_eq!(found.path, fake);
        // `--cc=clang` still forces the system provider.
        if which("clang").or_else(|| which("clang-22")).is_some() {
            let c = detect_clang_with(ClangProvider::Clang).expect("system clang present");
            assert!(!c.zig);
        }
        // The test hook hides the managed toolchain too.
        std::env::set_var("ZZ_TEST_HIDE_CLANG", "1");
        assert!(detect_clang().is_none());
        assert!(managed_zig_path().is_none());
        std::env::remove_var("ZZ_TEST_HIDE_CLANG");
    }

    #[test]
    fn cache_key_follows_the_pin() {
        let _lock = ENV_LOCK.lock().unwrap();
        let root = HermeticRoot::new();
        let clang = Clang {
            path: PathBuf::from("/usr/bin/clang"),
            zig: false,
            label: "clang",
        };
        let unpinned = crate::cache::cache_key(None, true, &clang);
        assert!(unpinned.contains("-nopin-"), "unpinned key: {unpinned}");
        root.pin("0.17.0");
        let pinned = crate::cache::cache_key(None, true, &clang);
        assert!(pinned.contains("-0.17.0-"), "pinned key: {pinned}");
        assert_ne!(unpinned, pinned);
    }
}
