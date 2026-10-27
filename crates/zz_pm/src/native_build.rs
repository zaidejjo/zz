//! Declarative native plugin builds — no shell, no scripts, no network.
//!
//! The only child processes ever spawned here are `cc` (compile + shared
//! link), `ar` (static archive), `pkg-config` (flag resolution), and `nm`
//! (read-only symbol verification). All run with structured argv under a
//! hermetic environment ([`hermetic_env`]): no shell, no script, and no
//! network access by construction (prebuilt downloads happen in the fetch
//! path before this module is entered).
//!
//! Slice 0 is Linux-first: tag selection, flag validation, and artifact
//! verification are tag-driven (works for macOS/Windows tags by
//! construction), but cross-compilation and non-Linux hosts are exercised
//! in later slices.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::manifest::{validate_cflag, validate_pkg_config_token, BuildCcSpec, BuildRustSpec};
use crate::rust_lock;

/// Fixed build timestamp for reproducibility (recorded in the lock;
/// bit-for-bit across toolchains is explicitly out of v1 scope).
pub const SOURCE_DATE_EPOCH: &str = "0";

/// C-ABI plugin version stamp (pure-C plugins, e.g. zimg).
const C_ABI_STAMP: &str = "ZZ_C_PLUGIN_ABI_VERSION";
/// Rust-cdylib plugin version stamp (accepted, not produced, in v1).
const RUST_ABI_STAMP: &str = "ZZ_PLUGIN_ABI_VERSION";

/// A probed C toolchain.
#[derive(Debug, Clone)]
pub struct CcToolchain {
    /// Absolute (or PATH-resolved) compiler path.
    pub path: PathBuf,
    /// First line of `cc --version` (recorded in `zz.lock`).
    pub version: String,
    /// True when the driver accepts clang-style `--target=`.
    pub is_clang: bool,
}

/// Output of a successful declarative build (all paths absolute).
#[derive(Debug, Clone)]
pub struct CcBuildOutput {
    /// Per-source objects (`build/<stem>.o`) — AOT link inputs.
    pub objects: Vec<PathBuf>,
    /// Static archive (`build/lib<name>.a`) — AOT link input.
    pub archive: PathBuf,
    /// Shared library (`build/lib<name>.so|.dylib|.dll`) — VM dlopen input.
    pub shared: PathBuf,
    /// `build/cflags.txt` path (cache-key input).
    pub cflags_path: PathBuf,
    /// `build/ldflags.txt` path (link args for AOT).
    pub ldflags_path: PathBuf,
    /// Resolved platform tag (e.g. `linux-x86_64-gnu-glibc2.44`).
    pub tag: String,
    /// Compiler version string (recorded in `zz.lock`).
    pub compiler: String,
}

/// Detect the host platform tag (`os-arch-abi-floor`).
///
/// - Linux glibc: `linux-<arch>-gnu-glibc<major>.<minor>` (floor read from
///   `ldd --version`; musl hosts → `linux-<arch>-musl`).
/// - macOS: `macos-<arm64|x86_64>-min<major>.<minor>` from `sw_vers`.
/// - Windows: `windows-x86_64-msvc` (AOT-only; VM load deferred).
pub fn host_tag() -> Result<String, String> {
    let arch = std::env::consts::ARCH;
    #[cfg(target_os = "linux")]
    {
        let normalized = match arch {
            "x86_64" => "x86_64",
            "aarch64" => "aarch64",
            _ => return Err(format!("unsupported linux arch `{arch}` for native builds")),
        };
        if is_musl_host() {
            return Ok(format!("linux-{normalized}-musl"));
        }
        let (major, minor) = glibc_version()
            .ok_or_else(|| "cannot determine glibc version (ldd failed)".to_string())?;
        Ok(format!("linux-{normalized}-gnu-glibc{major}.{minor}"))
    }
    #[cfg(target_os = "macos")]
    {
        let normalized = match arch {
            "x86_64" => "x86_64",
            "aarch64" => "arm64",
            _ => return Err(format!("unsupported macos arch `{arch}` for native builds")),
        };
        let (major, minor) = macos_version()
            .ok_or_else(|| "cannot determine macOS version (sw_vers failed)".to_string())?;
        return Ok(format!("macos-{normalized}-min{major}.{minor}"));
    }
    #[cfg(target_os = "windows")]
    {
        let normalized = match arch {
            "x86_64" => "x86_64",
            "aarch64" => "aarch64",
            _ => {
                return Err(format!(
                    "unsupported windows arch `{arch}` for native builds"
                ))
            }
        };
        return Ok(format!("windows-{normalized}-msvc"));
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
    {
        let _ = arch;
        return Err("unsupported host OS for native builds".to_string());
    }
}

/// True when `artifact_tag` satisfies `host_tag`: same os+arch+abi and
/// `artifact_floor <= host_floor`. glibc/macOS floors compare numerically;
/// musl/msvc match exactly.
pub fn tags_compatible(artifact: &str, host: &str) -> bool {
    let a: Vec<&str> = artifact.split('-').collect();
    let h: Vec<&str> = host.split('-').collect();
    match (a.as_slice(), h.as_slice()) {
        (["linux", a_arch, "gnu", a_floor], ["linux", h_arch, "gnu", h_floor]) => {
            a_arch == h_arch && floor_le(a_floor, h_floor, "glibc")
        }
        (["linux", a_arch, "musl"], ["linux", h_arch, "musl"]) => a_arch == h_arch,
        (["macos", a_arch, a_floor], ["macos", h_arch, h_floor]) => {
            a_arch == h_arch && floor_le(a_floor, h_floor, "min")
        }
        (["windows", a_arch, "msvc"], ["windows", h_arch, "msvc"]) => a_arch == h_arch,
        _ => false,
    }
}

/// `artifact_floor <= host_floor` for a `prefix<major>.<minor>` floor.
fn floor_le(artifact: &str, host: &str, prefix: &str) -> bool {
    match (
        artifact.strip_prefix(prefix).and_then(parse_dotted),
        host.strip_prefix(prefix).and_then(parse_dotted),
    ) {
        (Some((am, ai)), Some((hm, hi))) => (am, ai) <= (hm, hi),
        _ => false,
    }
}

fn parse_dotted(s: &str) -> Option<(u32, u32)> {
    let (a, b) = s.split_once('.')?;
    Some((a.parse().ok()?, b.parse().ok()?))
}

/// LLVM triple for a platform tag (used as clang `--target=`).
pub fn llvm_triple_for_tag(tag: &str) -> Result<&'static str, String> {
    let parts: Vec<&str> = tag.split('-').collect();
    match parts.as_slice() {
        ["linux", "x86_64", ..] => Ok("x86_64-unknown-linux-gnu"),
        ["linux", "aarch64", ..] => Ok("aarch64-unknown-linux-gnu"),
        ["macos", "arm64", ..] => Ok("aarch64-apple-darwin"),
        ["macos", "x86_64", ..] => Ok("x86_64-apple-darwin"),
        ["windows", "x86_64", ..] => Ok("x86_64-pc-windows-msvc"),
        ["windows", "aarch64", ..] => Ok("aarch64-pc-windows-msvc"),
        _ => Err(format!("no LLVM triple for platform tag `{tag}`")),
    }
}

/// Rust target triple for a platform tag (`cargo --target`).
///
/// Centralized mapping: every supported tag maps to the triple whose
/// std/core artifacts cargo needs. Linux keeps its libc split (gnu vs
/// musl); macOS uses Apple's ABI; Windows maps to MSVC (the only
/// first-class Rust Windows target, AOT-only in v1). Unsupported tags
/// fail with the tag and the accepted set — never a silent host build.
pub fn rust_triple_for_tag(tag: &str) -> Result<&'static str, String> {
    let parts: Vec<&str> = tag.split('-').collect();
    match parts.as_slice() {
        ["linux", "x86_64", "gnu", _] => Ok("x86_64-unknown-linux-gnu"),
        ["linux", "aarch64", "gnu", _] => Ok("aarch64-unknown-linux-gnu"),
        ["linux", "x86_64", "musl"] => Ok("x86_64-unknown-linux-musl"),
        ["linux", "aarch64", "musl"] => Ok("aarch64-unknown-linux-musl"),
        ["macos", "arm64", _] => Ok("aarch64-apple-darwin"),
        ["macos", "x86_64", _] => Ok("x86_64-apple-darwin"),
        ["windows", "x86_64", "msvc"] => Ok("x86_64-pc-windows-msvc"),
        ["windows", "aarch64", "msvc"] => Ok("aarch64-pc-windows-msvc"),
        _ => Err(format!(
            "no Rust target triple for platform tag `{tag}`\n\
             hint: supported tags are linux-{{x86_64,aarch64}}-{{gnu-glibc*,musl}}, \
             macos-{{arm64,x86_64}}-min*, windows-x86_64-msvc"
        )),
    }
}

/// Shared-library suffix for a platform tag.
pub fn shared_suffix_for_tag(tag: &str) -> &'static str {
    if tag.starts_with("macos-") {
        "dylib"
    } else if tag.starts_with("windows-") {
        "dll"
    } else {
        "so"
    }
}

/// Shared-link driver flag for a platform tag (`-dynamiclib` on macOS,
/// `-shared` elsewhere).
pub fn shared_link_flag_for_tag(tag: &str) -> &'static str {
    if tag.starts_with("macos-") {
        "-dynamiclib"
    } else {
        "-shared"
    }
}

/// Probe the C toolchain: `$CC` → `cc` → `clang`. Records the version
/// string for the lockfile. Never a shell: direct argv exec only.
pub fn probe_cc() -> Result<CcToolchain, String> {
    let candidates: Vec<String> = std::env::var("CC")
        .map(|cc| vec![cc])
        .unwrap_or_else(|_| vec!["cc".to_string(), "clang".to_string()]);
    for cand in &candidates {
        let path = PathBuf::from(cand);
        let out = match Command::new(&path).arg("--version").output() {
            Ok(o) if o.status.success() => o,
            _ => continue,
        };
        let text = String::from_utf8_lossy(&out.stdout);
        let version = text.lines().next().unwrap_or("unknown").trim().to_string();
        let is_clang = version.to_lowercase().contains("clang") || cand.contains("clang");
        return Ok(CcToolchain {
            path,
            version,
            is_clang,
        });
    }
    Err("no C compiler found ($CC, cc, clang all missing)\n\
         hint: install clang or gcc to build native plugins"
        .to_string())
}

/// Resolve `pkg-config --cflags/--libs` for `modules`, validating every
/// emitted token with [`validate_pkg_config_token`] before use.
/// Fails closed: missing binary, missing module, or a forbidden token.
pub fn pkg_config_flags(modules: &[String]) -> Result<(Vec<String>, Vec<String>), String> {
    if modules.is_empty() {
        return Ok((Vec::new(), Vec::new()));
    }
    let cflags_out = Command::new("pkg-config")
        .arg("--cflags")
        .args(modules)
        .output()
        .map_err(|e| {
            format!(
                "pkg-config not found: {e}\n\
                 hint: install pkg-config to resolve system dependencies"
            )
        })?;
    if !cflags_out.status.success() {
        let stderr = String::from_utf8_lossy(&cflags_out.stderr);
        return Err(format!(
            "pkg-config --cflags {} failed: {}\n\
             hint: install the matching -dev package",
            modules.join(" "),
            stderr.trim()
        ));
    }
    let libs_out = Command::new("pkg-config")
        .arg("--libs")
        .args(modules)
        .output()
        .map_err(|e| format!("pkg-config failed: {e}"))?;
    if !libs_out.status.success() {
        let stderr = String::from_utf8_lossy(&libs_out.stderr);
        return Err(format!(
            "pkg-config --libs {} failed: {}",
            modules.join(" "),
            stderr.trim()
        ));
    }
    let mut cflags = Vec::new();
    for tok in String::from_utf8_lossy(&cflags_out.stdout)
        .split_whitespace()
        .map(str::to_string)
    {
        validate_pkg_config_token(&tok)?;
        cflags.push(tok);
    }
    let mut libs = Vec::new();
    for tok in String::from_utf8_lossy(&libs_out.stdout)
        .split_whitespace()
        .map(str::to_string)
    {
        validate_pkg_config_token(&tok)?;
        libs.push(tok);
    }
    Ok((cflags, libs))
}

/// Audit verdict for one locked native build record (#265): `errors`
/// fail `zz audit` (drift = re-run `zz install`), `warnings` do not.
/// Probes the live machine (host tag, compiler, pkg-config) and diffs
/// against the lock. `modules` are the manifest's `pkg_config` modules.
pub struct NativeDrift {
    pub errors: Vec<String>,
    pub warnings: Vec<String>,
}

pub fn check_native_drift(
    name: &str,
    rec: &crate::lock::LockedNative,
    modules: &[String],
) -> NativeDrift {
    let mut out = NativeDrift {
        errors: Vec::new(),
        warnings: Vec::new(),
    };
    match host_tag() {
        Ok(host) if host != rec.tag => {
            out.errors.push(format!(
                "{name}: recorded for `{}` but host is `{host}` (re-run `zz install`)",
                rec.tag
            ));
        }
        Err(e) => out
            .warnings
            .push(format!("{name}: cannot probe host tag ({e})")),
        _ => {}
    }
    if rec.backend == "rust" {
        match probe_rust_toolchain() {
            Ok(tc) if tc.rustc_version != rec.compiler => {
                out.errors.push(format!(
                    "{name}: rustc drift (lock `{}`, now `{}`) (re-run `zz install`)",
                    rec.compiler, tc.rustc_version
                ));
            }
            Err(e) => out
                .warnings
                .push(format!("{name}: cannot probe Rust toolchain ({e})")),
            _ => {}
        }
    }
    if rec.backend == "cc" {
        match probe_cc() {
            Ok(cc) if cc.version != rec.compiler => {
                out.errors.push(format!(
                    "{name}: compiler drift (lock `{}`, now `{}`) (re-run `zz install`)",
                    rec.compiler, cc.version
                ));
            }
            Err(e) => out
                .warnings
                .push(format!("{name}: cannot probe compiler ({e})")),
            _ => {}
        }
        let now = pkg_config_versions(modules);
        if now != rec.pkg_config_resolved {
            out.errors.push(format!(
                "{name}: pkg-config drift (lock `{}`, now `{now}`) (re-run `zz install`)",
                rec.pkg_config_resolved
            ));
        }
    }
    out
}

/// Resolved `pkg-config` module versions for the lock audit record:
/// `name=version,...` sorted by name. Unresolvable modules record `?`
/// with a warning (audit-only; the build already validated the flags).
pub fn pkg_config_versions(modules: &[String]) -> String {
    let mut pairs: Vec<String> = Vec::new();
    for m in modules {
        let ver = Command::new("pkg-config")
            .arg("--modversion")
            .arg(m)
            .output()
            .ok()
            .filter(|o| o.status.success())
            .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
            .filter(|v| !v.is_empty());
        match ver {
            Some(v) => pairs.push(format!("{m}={v}")),
            None => {
                eprintln!("warning: cannot resolve pkg-config version for `{m}`");
                pairs.push(format!("{m}=?"));
            }
        }
    }
    pairs.sort();
    pairs.join(",")
}

/// Hermetic environment for `cc`/`ar`/`nm`: cleared, then only `PATH`
/// (inherited so the driver finds `as`/`ld`), `CC`, an isolated `TMPDIR`
/// under the build dir (`HOME` deliberately unset), and the fixed
/// `SOURCE_DATE_EPOCH`.
fn hermetic_env(tmpdir: &Path) -> Vec<(String, String)> {
    let mut env = Vec::new();
    if let Ok(path) = std::env::var("PATH") {
        env.push(("PATH".to_string(), path));
    }
    if let Ok(cc) = std::env::var("CC") {
        env.push(("CC".to_string(), cc));
    }
    env.push(("TMPDIR".to_string(), tmpdir.to_string_lossy().into_owned()));
    env.push((
        "SOURCE_DATE_EPOCH".to_string(),
        SOURCE_DATE_EPOCH.to_string(),
    ));
    // Windows children need the system root to load DLLs at all, plus the
    // MSVC/SDK search paths when clang drives `link.exe`. Only forwarded
    // when present, so Unix stays as hermetic as before.
    #[cfg(windows)]
    for key in [
        "SystemRoot",
        "SYSTEMROOT",
        "TEMP",
        "TMP",
        "INCLUDE",
        "LIB",
        "VCToolsInstallDir",
        "WindowsSdkDir",
        "UCRTVersion",
        "WindowsSDKVersion",
        "VSCMD_ARG_TGT_ARCH",
    ] {
        if let Ok(v) = std::env::var(key) {
            env.push((key.to_string(), v));
        }
    }
    env
}

/// First tool from `candidates` that can be spawned and reports a version.
///
/// Windows MSVC tooling (`lib.exe`, `nm`-equivalents) does not speak GNU
/// `--version`, so a failed version probe still counts as "spawnable" —
/// the real invocation surfaces the error if the tool is unusable.
fn find_tool(candidates: &[&str]) -> Option<String> {
    for cand in candidates {
        if let Ok(out) = Command::new(cand).arg("--version").output() {
            if out.status.success() {
                return Some((*cand).to_string());
            }
            // Spawnable, just not a GNU tool (e.g. MSVC `lib`).
            return Some((*cand).to_string());
        }
    }
    None
}

/// Create a static archive from `objects` with whatever archiver the
/// platform ships: `ar`/`llvm-ar` (`rcs`) or MSVC `lib` (`/OUT:`).
fn archive_objects(archive: &Path, objects: &[PathBuf], tmpdir: &Path) -> Result<(), String> {
    if let Some(ar) = find_tool(&["llvm-ar", "ar"]) {
        let mut cmd = Command::new(&ar);
        cmd.arg("rcs").arg(archive);
        for o in objects {
            cmd.arg(o);
        }
        return run_hermetic(cmd, tmpdir, "ar rcs").map(|_| ());
    }
    if let Some(lib) = find_tool(&["lib"]) {
        let mut cmd = Command::new(&lib);
        cmd.arg("/nologo")
            .arg(format!("/OUT:{}", archive.display()));
        for o in objects {
            cmd.arg(o);
        }
        return run_hermetic(cmd, tmpdir, "lib /OUT").map(|_| ());
    }
    Err(
        "no static archiver found (need `ar`, `llvm-ar`, or MSVC `lib`)\n\
         hint: install the LLVM binutils (`llvm-ar`) or Visual Studio Build Tools"
            .to_string(),
    )
}

/// Run `cmd` with the hermetic environment, returning stdout on success
/// or a loud stderr-carrying error.
fn run_hermetic(mut cmd: Command, tmpdir: &Path, what: &str) -> Result<Vec<u8>, String> {
    let prog = cmd.get_program().to_string_lossy().into_owned();
    let out = cmd
        .env_clear()
        .envs(hermetic_env(tmpdir))
        .output()
        .map_err(|e| format!("{what} failed to spawn `{prog}`: {e}"))?;
    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr);
        return Err(format!("{what} failed: {}", stderr.trim()));
    }
    Ok(out.stdout)
}

/// Extract expected C symbols from a `plugin.zzi` file.
///
/// Minimal line parser (not the full loader): collects `func <zz-name>`
/// entries inside the `extern "C"` block plus explicit `= "c_sym"`
/// overrides. The full manifest validation still runs through
/// `zz_plugin::load_manifest` in `zz_cli`; this only feeds the `nm`
/// cross-check, so unknown lines are skipped, not errors — an empty
/// result is the error (a plugin with no funcs is never valid).
pub fn expected_c_symbols(zzi_path: &Path) -> Result<Vec<String>, String> {
    let text = std::fs::read_to_string(zzi_path)
        .map_err(|e| format!("cannot read {}: {e}", zzi_path.display()))?;
    let mut syms = Vec::new();
    let mut in_block = false;
    for raw in text.lines() {
        let line = raw.trim();
        if line.starts_with("extern") && line.contains("\"C\"") {
            in_block = true;
            continue;
        }
        if !in_block {
            continue;
        }
        if line.starts_with('}') {
            break;
        }
        let Some(rest) = line.strip_prefix("func ") else {
            continue;
        };
        // func <zz.name>(...) ... [= "c_sym"];
        let name_end = rest.find(['(', ' ', '\t']).unwrap_or(rest.len());
        let zz_name = rest[..name_end].trim();
        if zz_name.is_empty() {
            continue;
        }
        let c_sym = rest
            .find('=')
            .and_then(|i| {
                let tail = rest[i + 1..].trim().trim_end_matches(';').trim();
                tail.strip_prefix('"')?
                    .strip_suffix('"')
                    .map(str::to_string)
            })
            .unwrap_or_else(|| zz_name.replace('.', "_"));
        if !syms.contains(&c_sym) {
            syms.push(c_sym);
        }
    }
    if syms.is_empty() {
        return Err(format!("no extern funcs found in {}", zzi_path.display()));
    }
    Ok(syms)
}

/// Defined global symbols of an object/archive/shared lib
/// (`nm -g --defined-only`), preserving multiplicity so the ABI stamp can
/// be required exactly once.
fn defined_symbols(path: &Path, tmpdir: &Path) -> Result<Vec<String>, String> {
    let nm = find_tool(&["llvm-nm", "nm"]).ok_or_else(|| {
        "no symbol dumper found (need `nm` or `llvm-nm`)\n\
         hint: install binutils (`nm`) or LLVM (`llvm-nm`)"
            .to_string()
    })?;
    let mut cmd = Command::new(&nm);
    cmd.arg("-g").arg("--defined-only").arg(path);
    let out = run_hermetic(cmd, tmpdir, &format!("nm {}", path.display()))?;
    let mut symbols = Vec::new();
    for line in String::from_utf8_lossy(&out).lines() {
        let line = line.trim();
        if line.is_empty() || line.ends_with(':') {
            continue; // archive member headers
        }
        // `addr TYPE name` or `TYPE name` or bare `name`.
        let cols: Vec<&str> = line.split_whitespace().collect();
        if let Some(name) = cols.last() {
            symbols.push(name.to_string());
        }
    }
    Ok(symbols)
}

/// Count the ABI version stamp across both accepted spellings.
fn abi_stamp_count(symbols: &[String]) -> usize {
    symbols
        .iter()
        .filter(|s| s.as_str() == C_ABI_STAMP || s.as_str() == RUST_ABI_STAMP)
        .count()
}

/// Declarative source build for one package.
///
/// Validates `spec`, resolves the host (or `target_tag`) platform tag,
/// compiles each source (`cc -c`), archives (`ar rcs`), links the shared
/// lib, writes `cflags.txt`/`ldflags.txt`, then verifies BOTH engines:
/// every `plugin.zzi` symbol must resolve in the archive (AOT) and in the
/// shared lib (VM), and exactly one ABI stamp must be present in the
/// shared lib. Missing either artifact class is a hard error (except
/// Windows tags, where the VM-shared check is waived with a loud note —
/// `LoadLibrary` is still deferred).
///
/// `lib_stem` names the outputs (`lib<stem>.a/.so`); pass the
/// underscored package name.
pub fn ensure_cc(
    pkg_dir: &Path,
    pkg_name: &str,
    spec: &BuildCcSpec,
    target_tag: Option<&str>,
) -> Result<CcBuildOutput, String> {
    let pkg_dir = std::fs::canonicalize(pkg_dir)
        .map_err(|e| format!("cannot resolve {}: {e}", pkg_dir.display()))?;
    // Re-validate defensively: callers pass manifest structs that may not
    // have gone through NativeSpec::validate (path deps, CAS checkouts).
    for s in &spec.sources {
        if Path::new(s).is_absolute() || s.split('/').any(|c| c == "..") || !s.ends_with(".c") {
            return Err(format!(
                "invalid source `{s}`: must be a contained `.c` path"
            ));
        }
    }
    for d in &spec.include_dirs {
        if Path::new(d).is_absolute() || d.split('/').any(|c| c == "..") {
            return Err(format!(
                "invalid include_dir `{d}`: must stay inside the package"
            ));
        }
    }
    for f in &spec.cflags {
        validate_cflag(f)?;
    }

    let host = host_tag()?;
    let tag = target_tag.unwrap_or(&host).to_string();
    if !spec.targets.is_empty()
        && !spec
            .targets
            .iter()
            .any(|t| tags_compatible(&tag, t) || tags_compatible(t, &tag) || *t == tag)
    {
        return Err(format!(
            "package `{pkg_name}` declares no build for tag `{tag}` (declares: {})\n\
             hint: ask the maintainer for a `{tag}` prebuilt or build",
            spec.targets.join(", ")
        ));
    }
    let llvm_triple = llvm_triple_for_tag(&tag)?;

    let cc = probe_cc()?;
    if tag != host && !cc.is_clang {
        return Err(format!(
            "cross build for `{tag}` requires clang (found: {})\n\
             hint: set CC=clang",
            cc.version
        ));
    }

    let stem: String = pkg_name
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect();
    let build_dir = pkg_dir.join("build");
    std::fs::create_dir_all(&build_dir)
        .map_err(|e| format!("cannot create {}: {e}", build_dir.display()))?;
    let tmpdir = build_dir.join(".zz-tmp");
    std::fs::create_dir_all(&tmpdir)
        .map_err(|e| format!("cannot create {}: {e}", tmpdir.display()))?;

    // Resolve + validate system flags before compiling anything.
    let (pc_cflags, pc_libs) = pkg_config_flags(&spec.pkg_config)?;

    // Containment with symlink resolution: the joined path must stay
    // inside the canonical package dir.
    let join_contained = |rel: &str| -> Result<PathBuf, String> {
        let joined = pkg_dir.join(rel);
        let canon =
            std::fs::canonicalize(&joined).map_err(|e| format!("cannot resolve `{rel}`: {e}"))?;
        if !canon.starts_with(&pkg_dir) {
            return Err(format!("`{rel}` escapes the package root"));
        }
        Ok(canon)
    };

    let mut objects = Vec::new();
    let mut seen_stems = HashSet::new();
    for src_rel in &spec.sources {
        let src = join_contained(src_rel)?;
        let stem_name = src
            .file_stem()
            .and_then(|s| s.to_str())
            .ok_or_else(|| format!("bad source name `{src_rel}`"))?;
        if !seen_stems.insert(stem_name.to_string()) {
            return Err(format!(
                "duplicate object stem `{stem_name}`: sources must have distinct file names"
            ));
        }
        let obj = build_dir.join(format!("{stem_name}.o"));
        let mut cmd = Command::new(&cc.path);
        cmd.arg("-c").arg(&src).arg("-o").arg(&obj);
        for inc in &spec.include_dirs {
            cmd.arg(format!("-I{}", join_contained(inc)?.display()));
        }
        for def in &spec.defines {
            cmd.arg(format!("-D{def}"));
        }
        cmd.args(&spec.cflags).args(&pc_cflags);
        if cc.is_clang {
            cmd.arg(format!("--target={llvm_triple}"));
        }
        run_hermetic(cmd, &tmpdir, &format!("cc -c {src_rel}"))?;
        objects.push(obj);
    }

    // Static archive (AOT input). MSVC toolchains use `.lib`; everything
    // else uses `.a` (llvm-ar writes a COFF archive either way).
    let archive_ext = if tag.starts_with("windows-") {
        "lib"
    } else {
        "a"
    };
    let archive = build_dir.join(format!("lib{stem}.{archive_ext}"));
    archive_objects(&archive, &objects, &tmpdir)?;

    // Shared library (VM dlopen input).
    let suffix = shared_suffix_for_tag(&tag);
    let shared = build_dir.join(format!("lib{stem}.{suffix}"));
    let mut link_libs: Vec<String> = spec.libs.iter().map(|l| format!("-l{l}")).collect();
    link_libs.extend(pc_libs.clone());
    {
        let mut cmd = Command::new(&cc.path);
        cmd.arg(shared_link_flag_for_tag(&tag))
            .arg("-o")
            .arg(&shared);
        for o in &objects {
            cmd.arg(o);
        }
        cmd.args(&link_libs);
        if cc.is_clang {
            cmd.arg(format!("--target={llvm_triple}"));
        }
        run_hermetic(cmd, &tmpdir, "cc -shared")?;
    }

    // Flag files (cache-key inputs + AOT link args).
    let mut cflag_record: Vec<String> = spec.cflags.clone();
    cflag_record.extend(pc_cflags);
    for inc in &spec.include_dirs {
        cflag_record.push(format!("-I{inc}"));
    }
    for def in &spec.defines {
        cflag_record.push(format!("-D{def}"));
    }
    let cflags_path = build_dir.join("cflags.txt");
    let ldflags_path = build_dir.join("ldflags.txt");
    std::fs::write(&cflags_path, cflag_record.join(" "))
        .map_err(|e| format!("cannot write {}: {e}", cflags_path.display()))?;
    std::fs::write(&ldflags_path, link_libs.join(" "))
        .map_err(|e| format!("cannot write {}: {e}", ldflags_path.display()))?;
    let _ = std::fs::remove_dir_all(&tmpdir);

    // Engine-parity verification (hard requirement): every .zzi symbol
    // resolves in BOTH the archive and the shared lib.
    let zzi_path = pkg_dir.join("plugin.zzi");
    verify_engine_parity(
        pkg_name,
        std::slice::from_ref(&archive),
        &shared,
        &zzi_path,
        &tag,
        &build_dir,
    )?;

    Ok(CcBuildOutput {
        objects,
        archive,
        shared,
        cflags_path,
        ldflags_path,
        tag,
        compiler: cc.version,
    })
}

/// Output of a hermetic Rust crate build (all paths absolute).
#[derive(Debug, Clone)]
pub struct RustBuildOutput {
    /// Static archive (`build/lib<name>.a|.lib`) — AOT link input.
    pub archive: PathBuf,
    /// Shared library (`build/lib<name>.so|.dylib|.dll`) — VM dlopen input.
    pub shared: PathBuf,
    /// `build/cflags.txt` path (cache-key input).
    pub cflags_path: PathBuf,
    /// `build/ldflags.txt` path (link args for AOT).
    pub ldflags_path: PathBuf,
    /// Resolved platform tag.
    pub tag: String,
    /// `rustc --version` first line (audit record).
    pub compiler: String,
    /// `cargo --version` first line (audit record).
    pub cargo: String,
    /// Content key of the vendored closure (empty when no deps).
    pub vendor_key: String,
    /// Rust target triple passed to cargo.
    pub triple: String,
    /// Isolated cargo target directory under `<package>/build/`.
    pub target_dir: PathBuf,
    /// Cargo profile directory (`release` or `debug`).
    pub profile: String,
}

/// Environment variables a cargo child may inherit under the hermetic
/// allowlist. `HOME`/`RUSTUP_HOME` are needed only so a rustup shim can
/// locate its toolchain; `CARGO_HOME` is always replaced with a scratch
/// directory so user cargo config/registry state cannot leak in.
pub const RUST_ENV_ALLOWLIST: &[&str] = &["PATH", "HOME", "RUSTUP_HOME"];

/// A fully specified child process: program + argv + cleared environment.
///
/// Planning is pure so tests can assert the exact command (offline,
/// locked, explicit target, isolated target dir) without spawning cargo.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChildPlan {
    pub program: String,
    pub args: Vec<String>,
    pub cwd: PathBuf,
    pub env: Vec<(String, String)>,
}

impl ChildPlan {
    /// Materialize the plan as a `Command` (always `env_clear` first).
    pub fn to_command(&self) -> Command {
        let mut cmd = Command::new(&self.program);
        cmd.args(&self.args)
            .current_dir(&self.cwd)
            .env_clear()
            .envs(self.env.iter().cloned());
        cmd
    }

    /// Human/audit rendering (one line, never executed through a shell).
    pub fn display_line(&self) -> String {
        let mut parts = vec![self.program.clone()];
        parts.extend(self.args.iter().cloned());
        parts.join(" ")
    }
}

/// Base hermetic environment for cargo: allowlisted inheritance, scratch
/// `CARGO_HOME`, isolated `TMPDIR`, fixed timestamp, and an explicit
/// offline switch for build (vendor fetch keeps it false).
fn rust_hermetic_env(cargo_home: &Path, tmpdir: &Path, offline: bool) -> Vec<(String, String)> {
    let mut env = Vec::new();
    for key in RUST_ENV_ALLOWLIST {
        if let Ok(value) = std::env::var(key) {
            env.push(((*key).to_string(), value));
        }
    }
    env.push((
        "CARGO_HOME".to_string(),
        cargo_home.to_string_lossy().into_owned(),
    ));
    env.push(("TMPDIR".to_string(), tmpdir.to_string_lossy().into_owned()));
    env.push((
        "SOURCE_DATE_EPOCH".to_string(),
        SOURCE_DATE_EPOCH.to_string(),
    ));
    env.push(("CARGO_TERM_COLOR".to_string(), "never".to_string()));
    env.push(("CARGO_INCREMENTAL".to_string(), "0".to_string()));
    env.push((
        "CARGO_NET_OFFLINE".to_string(),
        if offline { "true" } else { "false" }.to_string(),
    ));
    env
}

/// Plan the hermetic build command:
/// `cargo build --offline --locked --target <triple> --manifest-path … --target-dir …`.
#[allow(clippy::too_many_arguments)]
pub fn plan_cargo_build(
    cargo: &str,
    manifest_path: &Path,
    target_dir: &Path,
    cwd: &Path,
    cargo_home: &Path,
    tmpdir: &Path,
    triple: &str,
    profile: crate::manifest::RustProfile,
    features: &[String],
) -> ChildPlan {
    let mut args: Vec<String> = vec!["build".to_string()];
    args.extend(profile.cargo_args().iter().map(|s| (*s).to_string()));
    args.extend([
        "--offline".to_string(),
        "--locked".to_string(),
        "--target".to_string(),
        triple.to_string(),
        "--manifest-path".to_string(),
        manifest_path.to_string_lossy().into_owned(),
        "--target-dir".to_string(),
        target_dir.to_string_lossy().into_owned(),
    ]);
    if !features.is_empty() {
        args.push("--features".to_string());
        args.push(features.join(","));
    }
    ChildPlan {
        program: cargo.to_string(),
        args,
        cwd: cwd.to_path_buf(),
        env: rust_hermetic_env(cargo_home, tmpdir, true),
    }
}

/// Plan the closure vendor fetch (`cargo vendor --locked`). This is the
/// only Rust step allowed to touch the network; the resulting sources are
/// stored content-addressed and every later build is `--offline`.
pub fn plan_cargo_vendor(
    cargo: &str,
    manifest_path: &Path,
    vendor_out: &Path,
    cwd: &Path,
    cargo_home: &Path,
    tmpdir: &Path,
) -> ChildPlan {
    ChildPlan {
        program: cargo.to_string(),
        args: vec![
            "vendor".to_string(),
            "--locked".to_string(),
            "--manifest-path".to_string(),
            manifest_path.to_string_lossy().into_owned(),
            vendor_out.to_string_lossy().into_owned(),
        ],
        cwd: cwd.to_path_buf(),
        env: rust_hermetic_env(cargo_home, tmpdir, false),
    }
}

/// Run a planned child and return stdout, with a loud stderr-carrying
/// error. Never goes through a shell.
fn run_plan(plan: &ChildPlan, what: &str) -> Result<Vec<u8>, String> {
    let out = plan
        .to_command()
        .output()
        .map_err(|e| format!("{what} failed to spawn `{}`: {e}", plan.program))?;
    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr);
        return Err(format!(
            "{what} failed (`{}`)\n{}",
            plan.display_line(),
            stderr.trim()
        ));
    }
    Ok(out.stdout)
}

/// Probed Rust toolchain (paths are command names resolved from `PATH`).
#[derive(Debug, Clone)]
pub struct RustToolchain {
    pub cargo: String,
    pub cargo_version: String,
    pub rustc_version: String,
    pub rustc_semver: semver::Version,
}

/// Probe `cargo` + `rustc`. Absence is a hard, actionable error — the
/// Rust backend never falls back to a shell hook.
pub fn probe_rust_toolchain() -> Result<RustToolchain, String> {
    let cargo = std::env::var("CARGO").unwrap_or_else(|_| "cargo".to_string());
    let cargo_out = Command::new(&cargo)
        .arg("--version")
        .output()
        .map_err(|e| {
            format!(
                "cargo not found (`{cargo}`): {e}\n\
                 hint: install a Rust toolchain (https://rustup.rs); \
                 [native.build-rust] requires cargo and rustc, and never falls back to build hooks"
            )
        })?;
    if !cargo_out.status.success() {
        return Err(format!(
            "`{cargo} --version` failed: {}",
            String::from_utf8_lossy(&cargo_out.stderr).trim()
        ));
    }
    let cargo_version = String::from_utf8_lossy(&cargo_out.stdout)
        .lines()
        .next()
        .unwrap_or("cargo (unknown)")
        .trim()
        .to_string();

    let rustc_out = Command::new("rustc")
        .arg("--version")
        .output()
        .map_err(|e| {
            format!(
                "rustc not found: {e}\n\
             hint: install a Rust toolchain (https://rustup.rs); \
             [native.build-rust] requires cargo and rustc, and never falls back to build hooks"
            )
        })?;
    if !rustc_out.status.success() {
        return Err(format!(
            "`rustc --version` failed: {}",
            String::from_utf8_lossy(&rustc_out.stderr).trim()
        ));
    }
    let rustc_version = String::from_utf8_lossy(&rustc_out.stdout)
        .lines()
        .next()
        .unwrap_or("rustc (unknown)")
        .trim()
        .to_string();
    // `rustc 1.85.0 (4d91de4e4 2025-02-17)` → 1.85.0
    let raw = rustc_version
        .split_whitespace()
        .nth(1)
        .ok_or_else(|| format!("cannot parse rustc version from `{rustc_version}`"))?;
    let rustc_semver = semver::Version::parse(raw)
        .map_err(|e| format!("cannot parse rustc version `{raw}`: {e}"))?;
    Ok(RustToolchain {
        cargo,
        cargo_version,
        rustc_version,
        rustc_semver,
    })
}

/// Enforce a rustc compatibility range against the running toolchain.
pub fn check_rustc_range(req: &str, version: &semver::Version) -> Result<(), String> {
    let parsed =
        semver::VersionReq::parse(req).map_err(|e| format!("invalid rustc_range `{req}`: {e}"))?;
    if parsed.matches(version) {
        Ok(())
    } else {
        Err(format!(
            "rustc {version} does not satisfy [native.build-rust] rustc_range `{req}`\n\
             hint: upgrade/downgrade the toolchain, or widen rustc_range"
        ))
    }
}

/// Content key of the vendored closure for a spec (lock + manifest +
/// crate sources + spec fingerprint).
pub fn rust_vendor_key(crate_dir: &Path, spec: &BuildRustSpec) -> Result<String, String> {
    let fingerprint = toml::to_string(spec)
        .map_err(|e| format!("cannot fingerprint [native.build-rust]: {e}"))?;
    rust_lock::vendor_key(crate_dir, &fingerprint)
}

/// Audit a Rust plugin's vendored closure: pins, checksums, licenses.
pub fn audit_rust_plugin(
    crate_dir: &Path,
    spec: &BuildRustSpec,
) -> Result<rust_lock::RustClosureAudit, String> {
    let (_manifest, closure) = rust_lock::load_validated(crate_dir)?;
    let key = rust_vendor_key(crate_dir, spec)?;
    let vendor_dir = crate::paths::cas_entry(&key);
    Ok(rust_lock::audit_closure(
        &closure,
        closure.needs_vendor().then_some(vendor_dir.as_path()),
    ))
}

/// Hermetic Rust crate build: validate, stage, vendor (CAS), then drive
/// `cargo build --offline --locked` and copy both engine artifacts into
/// the package `build/` directory.
///
/// No shell, no package scripts (`build.sh`/`build.rs` banned), no
/// network during the build (the closure is vendored beforehand).
pub fn ensure_rust(
    pkg_dir: &Path,
    pkg_name: &str,
    spec: &BuildRustSpec,
    target_tag: Option<&str>,
) -> Result<RustBuildOutput, String> {
    let pkg_dir = std::fs::canonicalize(pkg_dir)
        .map_err(|e| format!("cannot resolve {}: {e}", pkg_dir.display()))?;
    let crate_dir = rust_lock::crate_dir_for(&pkg_dir, &spec.crate_path)?;
    let (_crate_manifest, closure) = rust_lock::load_validated(&crate_dir)?;

    // Defensive re-validation (callers may hand us an unvalidated spec).
    let profile = spec.rust_profile()?;
    semver::VersionReq::parse(spec.rustc_req())
        .map_err(|e| format!("invalid [native.build-rust] rustc_range: {e}"))?;
    if let Some(target) = &spec.target {
        if target.is_empty()
            || !target
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
        {
            return Err(format!(
                "invalid [native.build-rust] target `{target}` (must be a Rust target triple)"
            ));
        }
    }

    let host = host_tag()?;
    let tag = target_tag.unwrap_or(&host).to_string();
    let triple = match &spec.target {
        Some(t) => t.clone(),
        None => rust_triple_for_tag(&tag)?.to_string(),
    };

    let toolchain = probe_rust_toolchain()?;
    check_rustc_range(spec.rustc_req(), &toolchain.rustc_semver)?;

    let stem: String = pkg_name
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect();
    let build_dir = pkg_dir.join("build");
    std::fs::create_dir_all(&build_dir)
        .map_err(|e| format!("cannot create {}: {e}", build_dir.display()))?;
    let tmpdir = build_dir.join(".zz-tmp-rust");
    let cargo_home = build_dir.join("rust-cargo-home");
    let cwd = build_dir.join("rust-cwd");
    let target_dir = build_dir.join("rust-target");
    for dir in [&tmpdir, &cargo_home, &cwd, &target_dir] {
        std::fs::create_dir_all(dir)
            .map_err(|e| format!("cannot create {}: {e}", dir.display()))?;
    }

    // Content key: lock + manifest + sources + spec. Same key reuses the
    // staged tree (stable mtimes for cargo fingerprints) and CAS closure.
    let content_key = rust_vendor_key(&crate_dir, spec)?;
    let key_stamp = build_dir.join(".rust-src-key");
    let staged = build_dir.join("rust-src");
    let staged_manifest = staged.join("Cargo.toml");
    let stage_current = std::fs::read_to_string(&key_stamp).ok().as_deref()
        == Some(content_key.as_str())
        && staged_manifest.is_file();
    if !stage_current {
        let _ = std::fs::remove_dir_all(&staged);
        rust_lock::copy_crate_tree(&crate_dir, &staged)?;
        rust_lock::detach_workspace(&staged_manifest)?;
        std::fs::write(&key_stamp, &content_key)
            .map_err(|e| format!("cannot write {}: {e}", key_stamp.display()))?;
    }

    // Closure: reuse CAS when warm (offline after first fetch); otherwise
    // `cargo vendor` once, then store content-addressed.
    let mut vendor_key = String::new();
    if closure.needs_vendor() {
        vendor_key = content_key;
        let cas = crate::paths::cas_entry(&vendor_key);
        if !cas.exists() {
            eprintln!(
                "zz: vendoring Rust closure for `{pkg_name}` (one-time network fetch, then offline)"
            );
            let vox_out = build_dir.join("rust-vendor-fetch");
            let _ = std::fs::remove_dir_all(&vox_out);
            let plan = plan_cargo_vendor(
                &toolchain.cargo,
                &staged_manifest,
                &vox_out,
                &cwd,
                &cargo_home,
                &tmpdir,
            );
            run_plan(&plan, "cargo vendor")?;
            validate_vendor_dir(&vox_out, pkg_name)?;
            let opts = crate::cas::CasStoreOptions {
                source_url: format!("rust-vendor:{vendor_key}"),
                version: pkg_name.to_string(),
                ..Default::default()
            };
            crate::cas::store_dir_at_key(&vox_out, &vendor_key, &opts)?;
            let _ = std::fs::remove_dir_all(&vox_out);
        }
        let cas = crate::paths::cas_entry(&vendor_key);
        if !cas.exists() {
            return Err(format!(
                "vendored Rust closure for `{pkg_name}` is missing after fetch ({}); re-run `zz install`",
                cas.display()
            ));
        }
        // Cargo needs a writable directory source; hardlink/copy out of
        // read-only CAS into build/.
        let local = build_dir.join("rust-vendor");
        let vendor_stamp = build_dir.join(".rust-vendor-key");
        let local_current = std::fs::read_to_string(&vendor_stamp).ok().as_deref()
            == Some(vendor_key.as_str())
            && local.is_dir();
        if !local_current {
            let _ = std::fs::remove_dir_all(&local);
            copy_dir_writable(&cas, &local)?;
            std::fs::write(&vendor_stamp, &vendor_key)
                .map_err(|e| format!("cannot write {}: {e}", vendor_stamp.display()))?;
        }
        write_cargo_source_config(&cargo_home, &local)?;
    } else {
        // Defensive: a stale config from a previous dependency set must
        // not redirect this dependency-free build.
        let config = cargo_home.join("config.toml");
        let _ = std::fs::remove_file(config);
    }

    let plan = plan_cargo_build(
        &toolchain.cargo,
        &staged_manifest,
        &target_dir,
        &cwd,
        &cargo_home,
        &tmpdir,
        &triple,
        profile,
        &spec.cargo_features,
    );
    eprintln!(
        "zz: building plugin `{pkg_name}` (hermetic cargo: {} {} {})",
        triple,
        profile.dir_name(),
        plan.display_line()
    );
    run_plan(&plan, "cargo build")?;

    // Cargo output layout: <target-dir>/<triple>/<profile>/lib<lib>.{a,so}
    // (MSVC emits `<lib>.lib`/`<lib>.dll` without the `lib` prefix).
    let lib_dir = target_dir.join(&triple).join(profile.dir_name());
    let lib_name = _crate_manifest.lib_name;
    let suffix = shared_suffix_for_tag(&tag);
    let windows = tag.starts_with("windows-");
    let built_archive = if windows {
        lib_dir.join(format!("{lib_name}.lib"))
    } else {
        lib_dir.join(format!("lib{lib_name}.a"))
    };
    let built_shared = if windows {
        lib_dir.join(format!("{lib_name}.dll"))
    } else {
        lib_dir.join(format!("lib{lib_name}.{suffix}"))
    };
    if !built_archive.is_file() {
        return Err(format!(
            "cargo build for `{pkg_name}` produced no staticlib at {}\n\
             hint: [lib] crate-type must include \"staticlib\"",
            built_archive.display()
        ));
    }
    if !built_shared.is_file() {
        return Err(format!(
            "cargo build for `{pkg_name}` produced no cdylib at {}\n\
             hint: [lib] crate-type must include \"cdylib\"",
            built_shared.display()
        ));
    }

    // Same naming rule as `ensure_cc`: MSVC linkers want `.lib`, and the
    // DLL is staged so VM loaders and tooling see one stable layout per
    // platform.
    let archive_ext = if windows { "lib" } else { "a" };
    let archive = build_dir.join(format!("lib{stem}.{archive_ext}"));
    let shared = build_dir.join(format!("lib{stem}.{suffix}"));
    copy_replace(&built_archive, &archive)?;
    copy_replace(&built_shared, &shared)?;

    let mut cflag_record = vec![format!("--target={triple}")];
    if !spec.cargo_features.is_empty() {
        cflag_record.push(format!("--features={}", spec.cargo_features.join(",")));
    }
    cflag_record.push(format!("--profile={}", profile.dir_name()));
    let cflags_path = build_dir.join("cflags.txt");
    let ldflags_path = build_dir.join("ldflags.txt");
    std::fs::write(&cflags_path, cflag_record.join(" "))
        .map_err(|e| format!("cannot write {}: {e}", cflags_path.display()))?;
    // Rust std's staticlib needs the platform's threading/dl/math libs.
    // `zz` dedupes flags at link time, so overlap with the runtime is safe.
    let link_libs = if cfg!(windows) {
        // Windows MSVC links these by default; no explicit flags needed.
        ""
    } else if cfg!(target_os = "linux") || cfg!(target_os = "macos") {
        "-lpthread -ldl -lm"
    } else {
        // BSDs: no libdl.
        "-lpthread -lm"
    };
    std::fs::write(&ldflags_path, link_libs)
        .map_err(|e| format!("cannot write {}: {e}", ldflags_path.display()))?;

    // Engine-parity verification: same contract as the C backend.
    let zzi_path = pkg_dir.join("plugin.zzi");
    verify_engine_parity(
        pkg_name,
        std::slice::from_ref(&archive),
        &shared,
        &zzi_path,
        &tag,
        &build_dir,
    )?;
    let _ = std::fs::remove_dir_all(&tmpdir);
    let _ = std::fs::remove_dir_all(&cwd);

    Ok(RustBuildOutput {
        archive,
        shared,
        cflags_path,
        ldflags_path,
        tag,
        compiler: toolchain.rustc_version,
        cargo: toolchain.cargo_version,
        vendor_key,
        triple,
        target_dir,
        profile: profile.dir_name().to_string(),
    })
}

/// A `cargo vendor` output must be a directory of crate dirs, each with a
/// `Cargo.toml` (a failed/empty fetch must never enter CAS).
fn validate_vendor_dir(dir: &Path, pkg_name: &str) -> Result<(), String> {
    let entries = std::fs::read_dir(dir)
        .map_err(|e| format!("cargo vendor for `{pkg_name}` produced no directory: {e}"))?;
    let mut crates = 0usize;
    for entry in entries.flatten() {
        if entry.path().is_dir() && entry.path().join("Cargo.toml").is_file() {
            crates += 1;
        }
    }
    if crates == 0 {
        return Err(format!(
            "cargo vendor for `{pkg_name}` produced no crate sources in {}",
            dir.display()
        ));
    }
    Ok(())
}

/// Copy a tree with writable permissions (CAS files are 0444).
fn copy_dir_writable(src: &Path, dst: &Path) -> Result<(), String> {
    std::fs::create_dir_all(dst).map_err(|e| format!("cannot create {}: {e}", dst.display()))?;
    let entries =
        std::fs::read_dir(src).map_err(|e| format!("cannot read {}: {e}", src.display()))?;
    for entry in entries.flatten() {
        let path = entry.path();
        let dest = dst.join(entry.file_name());
        if path.is_dir() {
            copy_dir_writable(&path, &dest)?;
        } else if path.is_file() {
            std::fs::copy(&path, &dest)
                .map_err(|e| format!("cannot copy {}: {e}", path.display()))?;
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                let _ = std::fs::set_permissions(&dest, std::fs::Permissions::from_mode(0o644));
            }
        }
    }
    Ok(())
}

fn copy_replace(src: &Path, dst: &Path) -> Result<(), String> {
    let _ = std::fs::remove_file(dst);
    std::fs::copy(src, dst).map_err(|e| {
        format!(
            "cannot install Rust artifact {} -> {}: {e}",
            src.display(),
            dst.display()
        )
    })?;
    Ok(())
}

/// Point cargo at the local vendored directory source and force offline.
fn write_cargo_source_config(cargo_home: &Path, vendor_dir: &Path) -> Result<(), String> {
    let dir = toml::Value::String(vendor_dir.to_string_lossy().into_owned()).to_string();
    let config = format!(
        "[source.crates-io]\nreplace-with = \"zz-vendored\"\n\n\
         [source.zz-vendored]\ndirectory = {dir}\n\n\
         [net]\noffline = true\n"
    );
    let path = cargo_home.join("config.toml");
    std::fs::write(&path, config)
        .map_err(|e| format!("cannot write cargo config {}: {e}", path.display()))
}

/// `plugin.zzi` symbol resolves in the archive set (AOT) and in the
/// shared lib (VM), with exactly one ABI stamp in the shared lib.
/// Windows tags waive the shared check with a loud note (AOT-only).
fn verify_engine_parity(
    pkg_name: &str,
    archives: &[PathBuf],
    shared: &Path,
    zzi_path: &Path,
    tag: &str,
    probe_tmp: &Path,
) -> Result<(), String> {
    let expected = expected_c_symbols(zzi_path)?;
    let mut arch_syms: HashSet<String> = HashSet::new();
    let mut arch_stamps = 0usize;
    for archive in archives {
        let set = defined_symbols(archive, probe_tmp)
            .map_err(|e| format!("static verification failed for `{pkg_name}`: {e}"))?;
        arch_stamps += abi_stamp_count(&set);
        arch_syms.extend(set);
    }
    for s in &expected {
        if !arch_syms.contains(s) {
            return Err(format!(
                "static verification failed for `{pkg_name}`: symbol `{s}` missing from archives\n\
                 hint: the sources do not implement every plugin.zzi function"
            ));
        }
    }
    if arch_stamps != 1 {
        return Err(format!(
            "static verification failed for `{pkg_name}`: expected exactly one ABI stamp ({C_ABI_STAMP} or {RUST_ABI_STAMP}) in the archives, found {arch_stamps}\n\
             hint: export the stamp exactly once (see docs/plugin-author-guide.md §11)"
        ));
    }
    if tag.starts_with("windows-") {
        eprintln!("note: `{pkg_name}` on windows is AOT-only (VM dlopen deferred)");
        return Ok(());
    }
    let shared_syms = defined_symbols(shared, probe_tmp)
        .map_err(|e| format!("shared verification failed for `{pkg_name}`: {e}"))?;
    for s in &expected {
        if !shared_syms.contains(s) {
            return Err(format!(
                "shared verification failed for `{pkg_name}`: symbol `{s}` missing from {}\n\
                 hint: the shared lib must export every plugin.zzi function for `zz run`",
                shared.display()
            ));
        }
    }
    let shared_stamps = abi_stamp_count(&shared_syms);
    if shared_stamps != 1 {
        return Err(format!(
            "shared verification failed for `{pkg_name}`: expected exactly one ABI stamp ({C_ABI_STAMP} or {RUST_ABI_STAMP}) in {}, found {shared_stamps}\n\
             hint: export the stamp exactly once (see docs/plugin-author-guide.md §11)",
            shared.display()
        ));
    }
    Ok(())
}

/// Download a prebuilt artifact tarball (`https:` only), verify its sha256
/// (fail closed), and install it via [`install_prebuilt_bytes`].
///
/// CAS fast path: `cas_entry(sha256)` reuse keeps installs offline-safe
/// when warm. Network failure returns a retriable error (the caller
/// falls back to source); sha mismatch is fatal.
pub fn fetch_prebuilt(
    pkg_dir: &Path,
    pkg_name: &str,
    tag: &str,
    artifact: &crate::manifest::PrebuiltArtifact,
) -> Result<CcBuildOutput, String> {
    if !(artifact.url.starts_with("https://") || artifact.url.starts_with("http://")) {
        return Err(format!(
            "prebuilt url for `{tag}` must be https: (registry: is deferred to v2)"
        ));
    }
    if artifact.sha256.len() != 64 {
        return Err(format!("prebuilt sha256 for `{tag}` must be 64 hex chars"));
    }
    // CAS fast path (verified bytes are content-addressed).
    let cas_dir = crate::paths::cas_entry(&artifact.sha256);
    if cas_dir.exists() {
        return install_prebuilt_dir(&cas_dir, pkg_dir, pkg_name, tag, &artifact.sha256);
    }
    let agent = ureq::Agent::new_with_config(
        ureq::config::Config::builder()
            .timeout_global(Some(std::time::Duration::from_secs(60)))
            .user_agent(format!("zzpm/{}", env!("CARGO_PKG_VERSION")))
            .build(),
    );
    let mut resp = agent.get(&artifact.url).call().map_err(|e| {
        format!(
            "cannot download prebuilt for `{tag}`: {e}\n\
             hint: check network, or re-run with --allow-source-builds"
        )
    })?;
    let bytes = resp.body_mut().read_to_vec().map_err(|e| {
        format!(
            "cannot download prebuilt for `{tag}`: {e}\n\
             hint: check network, or re-run with --allow-source-builds"
        )
    })?;
    let actual = crate::hash::hash_bytes(&bytes);
    if actual != artifact.sha256 {
        return Err(format!(
            "prebuilt sha256 mismatch for `{tag}`: manifest says {}, download is {actual}\n\
             hint: the mirror may be compromised or stale; refusing",
            artifact.sha256
        ));
    }
    // Stage into CAS via scratch sibling (same pattern as package fetch:
    // a crashed unpack never leaves a half-populated entry behind).
    if let Some(parent) = cas_dir.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("cannot create CAS parent: {e}"))?;
    }
    let tmp = crate::cas::scratch_sibling(&cas_dir, &format!("{pkg_name}-{tag}"));
    if let Some(parent) = tmp.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("cannot create CAS parent: {e}"))?;
    }
    extract_validated_prebuilt(&bytes, &tmp, pkg_name, tag)?;
    crate::cas::stage_dir(&tmp, &cas_dir).map_err(|e| format!("cannot stage CAS entry: {e}"))?;
    install_prebuilt_dir(&cas_dir, pkg_dir, pkg_name, tag, &artifact.sha256)
}

/// Install prebuilt tarball bytes after verifying sha + layout.
/// Test seam: unit tests craft tarballs in-memory without network.
pub fn install_prebuilt_bytes(
    bytes: &[u8],
    pkg_dir: &Path,
    pkg_name: &str,
    tag: &str,
    expected_sha256: &str,
) -> Result<CcBuildOutput, String> {
    let actual = crate::hash::hash_bytes(bytes);
    if actual != expected_sha256 {
        return Err(format!(
            "prebuilt sha256 mismatch for `{tag}`: expected {expected_sha256}, got {actual}"
        ));
    }
    let staging = pkg_dir.join(".zz-prebuilt-tmp");
    let _ = std::fs::remove_dir_all(&staging);
    std::fs::create_dir_all(&staging).map_err(|e| format!("cannot create staging dir: {e}"))?;
    let result = (|| {
        extract_validated_prebuilt(bytes, &staging, pkg_name, tag)?;
        install_prebuilt_dir(&staging, pkg_dir, pkg_name, tag, expected_sha256)
    })();
    let _ = std::fs::remove_dir_all(&staging);
    result
}

/// Unpack + validate a prebuilt tarball into `dest` (full-set rule:
/// at least one `.o`/`.a`, the platform shared lib, and `ldflags.txt`).
/// Leading `build/` prefixes are stripped; traversal attacks are refused
/// by the shared extractor.
fn extract_validated_prebuilt(
    bytes: &[u8],
    dest: &Path,
    pkg_name: &str,
    tag: &str,
) -> Result<(), String> {
    crate::remote::extract_tarball(bytes, dest)
        .map_err(|e| format!("cannot unpack prebuilt for `{pkg_name}` ({tag}): {e}"))?;
    validate_prebuilt_layout(dest, pkg_name, tag)
}

/// Full-set rule: each per-tag tarball carries the complete `build/`
/// layout. A static-only upload would fix AOT while silently breaking VM.
fn validate_prebuilt_layout(dir: &Path, pkg_name: &str, tag: &str) -> Result<(), String> {
    let mut files: Vec<PathBuf> = Vec::new();
    collect_files(dir, &mut files);
    let has_object = files
        .iter()
        .any(|p| matches!(p.extension().and_then(|e| e.to_str()), Some("o" | "a")));
    let suffix = shared_suffix_for_tag(tag);
    let has_shared = files.iter().any(|p| {
        p.extension().and_then(|e| e.to_str()) == Some(suffix)
            && p.file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n.starts_with("lib"))
    });
    let has_ldflags = files
        .iter()
        .any(|p| p.file_name().and_then(|n| n.to_str()) == Some("ldflags.txt"));
    if has_object && has_shared && has_ldflags {
        return Ok(());
    }
    let mut parts = Vec::new();
    if !has_object {
        parts.push("*.o/*.a (AOT static input)".to_string());
    }
    if !has_shared {
        parts.push(format!("lib*.{suffix} (VM shared input)"));
    }
    if !has_ldflags {
        parts.push("ldflags.txt".to_string());
    }
    Err(format!(
        "prebuilt for `{pkg_name}` ({tag}) is partial: missing {}\n\
         hint: ship the full build/ layout — static-only tarballs silently break `zz run`",
        parts.join(", ")
    ))
}

/// Recursively collect files under `dir`.
fn collect_files(dir: &Path, out: &mut Vec<PathBuf>) {
    let entries = match std::fs::read_dir(dir) {
        Ok(rd) => rd,
        Err(_) => return,
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect_files(&path, out);
        } else if path.is_file() {
            out.push(path);
        }
    }
}

/// Copy a validated prebuilt tree into `<pkg>/build/` (hardlink-or-copy
/// from CAS) and run engine-parity verification. Returns the same shape
/// as [`ensure_cc`] with an empty compiler string (no local compile).
fn install_prebuilt_dir(
    src_dir: &Path,
    pkg_dir: &Path,
    pkg_name: &str,
    tag: &str,
    _sha256: &str,
) -> Result<CcBuildOutput, String> {
    let build_dir = pkg_dir.join("build");
    std::fs::create_dir_all(&build_dir)
        .map_err(|e| format!("cannot create {}: {e}", build_dir.display()))?;
    let mut files: Vec<PathBuf> = Vec::new();
    collect_files(src_dir, &mut files);
    files.sort();
    let mut objects = Vec::new();
    let mut archives = Vec::new();
    let mut shared: Option<PathBuf> = None;
    let mut cflags_path = build_dir.join("cflags.txt");
    let mut ldflags_path = build_dir.join("ldflags.txt");
    let suffix = shared_suffix_for_tag(tag);
    for src in &files {
        // Flatten: strip any leading build/ prefix, keep the file name.
        let name = src
            .file_name()
            .and_then(|n| n.to_str())
            .ok_or_else(|| format!("bad prebuilt entry: {}", src.display()))?;
        if name.starts_with('.') || name.ends_with(".tmp") {
            continue;
        }
        let dest = build_dir.join(name);
        let _ = std::fs::remove_file(&dest);
        if std::fs::hard_link(src, &dest).is_err() {
            std::fs::copy(src, &dest)
                .map_err(|e| format!("cannot install {}: {e}", dest.display()))?;
        }
        match dest.extension().and_then(|e| e.to_str()) {
            Some("o") => objects.push(dest.clone()),
            Some("a") => archives.push(dest.clone()),
            Some(ext) if ext == suffix => {
                shared.get_or_insert(dest.clone());
            }
            _ => {}
        }
        if name == "cflags.txt" {
            cflags_path = dest.clone();
        }
        if name == "ldflags.txt" {
            ldflags_path = dest;
        }
    }
    let Some(shared) = shared else {
        return Err(format!(
            "prebuilt for `{pkg_name}` ({tag}) has no lib*.{suffix}"
        ));
    };
    if archives.is_empty() && objects.is_empty() {
        return Err(format!("prebuilt for `{pkg_name}` ({tag}) has no *.o/*.a"));
    }
    let first_archive = archives
        .first()
        .or(objects.first())
        .cloned()
        .unwrap_or_else(|| shared.clone());
    let zzi_path = pkg_dir.join("plugin.zzi");
    verify_engine_parity(
        pkg_name,
        if archives.is_empty() {
            &objects
        } else {
            &archives
        },
        &shared,
        &zzi_path,
        tag,
        &build_dir,
    )?;
    Ok(CcBuildOutput {
        objects,
        archive: first_archive,
        shared,
        cflags_path,
        ldflags_path,
        tag: tag.to_string(),
        compiler: String::new(),
    })
}

/// Verify one `<name>-<version>-<tag>.tgz` from `--artifact-dir`: tag
/// grammar, full build/ layout, and every `plugin.zzi` symbol resolvable
/// in both engines. Returns `(tag, sha256)` for the publish stanza.
pub fn verify_artifact_file(
    tgz_path: &Path,
    pkg_dir: &Path,
    pkg_name: &str,
) -> Result<(String, String), String> {
    let file_name = tgz_path
        .file_name()
        .and_then(|n| n.to_str())
        .ok_or_else(|| format!("bad artifact name: {}", tgz_path.display()))?;
    let stem = file_name.strip_suffix(".tgz").ok_or_else(|| {
        format!(
            "artifact `{file_name}` must end in .tgz\n\
             hint: tar the build/ dir per platform tag"
        )
    })?;
    // `<name>-<version>-<tag>`: the tag itself contains dashes, so find
    // it by trying every split point from the left.
    let parts = stem.split('-').collect::<Vec<_>>();
    if parts.len() < 4 {
        return Err(format!(
            "artifact `{file_name}` must be named <name>-<version>-<tag>.tgz"
        ));
    }
    let mut tag: Option<String> = None;
    for i in 2..parts.len() {
        let candidate = parts[i..].join("-");
        if crate::manifest::validate_platform_tag(&candidate).is_ok() {
            tag = Some(candidate);
            break;
        }
    }
    let Some(tag) = tag else {
        return Err(format!(
            "artifact `{file_name}` embeds no valid platform tag\n\
             hint: e.g. {pkg_name}-0.3.0-linux-x86_64-gnu-glibc2.28.tgz"
        ));
    };
    let bytes =
        std::fs::read(tgz_path).map_err(|e| format!("cannot read {}: {e}", tgz_path.display()))?;
    let sha256 = crate::hash::hash_bytes(&bytes);
    let staging = pkg_dir.join(".zz-artifact-check");
    let _ = std::fs::remove_dir_all(&staging);
    std::fs::create_dir_all(&staging).map_err(|e| format!("cannot create staging dir: {e}"))?;
    let result = (|| {
        extract_validated_prebuilt(&bytes, &staging, pkg_name, &tag)?;
        // Symbol check against this package's own plugin.zzi.
        let mut files: Vec<PathBuf> = Vec::new();
        collect_files(&staging, &mut files);
        let archives: Vec<PathBuf> = files
            .iter()
            .filter(|p| p.extension().and_then(|e| e.to_str()) == Some("a"))
            .cloned()
            .collect();
        let suffix = shared_suffix_for_tag(&tag);
        let shared = files
            .iter()
            .find(|p| {
                p.extension().and_then(|e| e.to_str()) == Some(suffix)
                    && p.file_name()
                        .and_then(|n| n.to_str())
                        .is_some_and(|n| n.starts_with("lib"))
            })
            .cloned()
            .ok_or_else(|| format!("no lib*.{suffix} in `{file_name}`"))?;
        let zzi_path = pkg_dir.join("plugin.zzi");
        let objects: Vec<PathBuf> = files
            .iter()
            .filter(|p| p.extension().and_then(|e| e.to_str()) == Some("o"))
            .cloned()
            .collect();
        verify_engine_parity(
            pkg_name,
            if archives.is_empty() {
                &objects
            } else {
                &archives
            },
            &shared,
            &zzi_path,
            &tag,
            &staging,
        )?;
        Ok((tag, sha256))
    })();
    let _ = std::fs::remove_dir_all(&staging);
    result
}

/// True on musl hosts (`ldd --version` mentions musl).
#[cfg(target_os = "linux")]
fn is_musl_host() -> bool {
    Command::new("ldd")
        .arg("--version")
        .output()
        .map(|o| {
            let text = format!(
                "{}{}",
                String::from_utf8_lossy(&o.stdout),
                String::from_utf8_lossy(&o.stderr)
            );
            text.to_lowercase().contains("musl")
        })
        .unwrap_or(false)
}

/// glibc `(major, minor)` from `ldd --version` (last `X.Y` on line 1).
#[cfg(target_os = "linux")]
fn glibc_version() -> Option<(u32, u32)> {
    let out = Command::new("ldd").arg("--version").output().ok()?;
    let line = String::from_utf8_lossy(&out.stdout)
        .lines()
        .next()?
        .to_string();
    // Scan tokens right-to-left for the first dotted pair.
    for tok in line.split_whitespace().rev() {
        let clean: String = tok
            .chars()
            .take_while(|c| c.is_ascii_digit() || *c == '.')
            .collect();
        if let Some(v) = parse_dotted(&clean) {
            return Some(v);
        }
    }
    None
}

/// macOS `(major, minor)` from `sw_vers -productVersion`.
#[cfg(target_os = "macos")]
fn macos_version() -> Option<(u32, u32)> {
    let out = Command::new("sw_vers")
        .arg("-productVersion")
        .output()
        .ok()?;
    parse_dotted(String::from_utf8_lossy(&out.stdout).trim())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn host_tag_linux_shape() {
        // This suite runs on Linux CI/dev: tag must parse back clean.
        #[cfg(target_os = "linux")]
        {
            let tag = host_tag().expect("host tag detects");
            assert!(
                tag.starts_with("linux-x86_64-") || tag.starts_with("linux-aarch64-"),
                "{tag}"
            );
            crate::manifest::validate_platform_tag(&tag).expect("host tag valid");
        }
    }

    /// Build a `cc` record matching the live machine (no drift expected).
    fn live_cc_record() -> crate::lock::LockedNative {
        crate::lock::LockedNative {
            backend: "cc".to_string(),
            tag: host_tag().unwrap_or_default(),
            compiler: probe_cc().map(|c| c.version).unwrap_or_default(),
            artifact_sha256: String::new(),
            pkg_config_resolved: pkg_config_versions(&[]),
            vendor_key: String::new(),
        }
    }

    #[test]
    fn drift_clean_record_passes() {
        let rec = live_cc_record();
        let verdict = check_native_drift("toy", &rec, &[]);
        assert!(
            verdict.errors.is_empty(),
            "unexpected drift errors: {:?}",
            verdict.errors
        );
    }

    #[test]
    fn drift_compiler_tag_and_pkg_config_fail() {
        let mut rec = live_cc_record();
        rec.compiler = "cc-tampered (audit test)".to_string();
        rec.tag = "plan9-arm64-none".to_string();
        rec.pkg_config_resolved = "vips=9.9.9".to_string();
        let verdict = check_native_drift("toy", &rec, &[]);
        let joined = verdict.errors.join("\n");
        // Compiler assertion needs a live compiler (CI and the C-plugin
        // e2e both require one; without it the checker warns, not errors).
        if probe_cc().is_ok() {
            assert!(
                joined.contains("compiler drift"),
                "missing compiler drift: {joined}"
            );
        }
        assert!(
            joined.contains("recorded for `plan9-arm64-none`"),
            "missing tag drift: {joined}"
        );
        assert!(
            joined.contains("pkg-config drift"),
            "missing pkg-config drift: {joined}"
        );
    }

    #[test]
    fn drift_prebuilt_skips_compiler_and_pkg_config() {
        let mut rec = live_cc_record();
        rec.backend = "prebuilt".to_string();
        rec.compiler = String::new();
        let verdict = check_native_drift("toy", &rec, &[]);
        assert!(
            verdict.errors.is_empty(),
            "prebuilt must not check compiler/pkg-config: {:?}",
            verdict.errors
        );
    }

    #[test]
    fn compat_same_tag() {
        assert!(tags_compatible(
            "linux-x86_64-gnu-glibc2.28",
            "linux-x86_64-gnu-glibc2.44"
        ));
    }

    #[test]
    fn compat_floor_above_host_rejected() {
        assert!(!tags_compatible(
            "linux-x86_64-gnu-glibc2.44",
            "linux-x86_64-gnu-glibc2.28"
        ));
    }

    #[test]
    fn compat_libc_never_crosses() {
        assert!(!tags_compatible(
            "linux-x86_64-gnu-glibc2.28",
            "linux-x86_64-musl"
        ));
        assert!(!tags_compatible(
            "linux-x86_64-musl",
            "linux-x86_64-gnu-glibc2.44"
        ));
    }

    #[test]
    fn compat_arch_never_crosses() {
        assert!(!tags_compatible(
            "linux-aarch64-gnu-glibc2.28",
            "linux-x86_64-gnu-glibc2.44"
        ));
    }

    #[test]
    fn compat_macos_floor() {
        assert!(tags_compatible(
            "macos-arm64-min11.0",
            "macos-arm64-min14.0"
        ));
        assert!(!tags_compatible(
            "macos-arm64-min14.0",
            "macos-arm64-min11.0"
        ));
        assert!(!tags_compatible(
            "macos-arm64-min11.0",
            "macos-x86_64-min14.0"
        ));
    }

    #[test]
    fn llvm_triples() {
        assert_eq!(
            llvm_triple_for_tag("linux-x86_64-gnu-glibc2.28").unwrap(),
            "x86_64-unknown-linux-gnu"
        );
        assert_eq!(
            llvm_triple_for_tag("macos-arm64-min11.0").unwrap(),
            "aarch64-apple-darwin"
        );
        assert_eq!(
            llvm_triple_for_tag("windows-x86_64-msvc").unwrap(),
            "x86_64-pc-windows-msvc"
        );
        assert_eq!(
            llvm_triple_for_tag("windows-aarch64-msvc").unwrap(),
            "aarch64-pc-windows-msvc"
        );
    }

    #[test]
    fn shared_suffix_map() {
        assert_eq!(shared_suffix_for_tag("linux-x86_64-musl"), "so");
        assert_eq!(shared_suffix_for_tag("macos-arm64-min11.0"), "dylib");
        assert_eq!(shared_suffix_for_tag("windows-x86_64-msvc"), "dll");
        assert_eq!(
            shared_link_flag_for_tag("macos-arm64-min11.0"),
            "-dynamiclib"
        );
        assert_eq!(
            shared_link_flag_for_tag("linux-x86_64-gnu-glibc2.28"),
            "-shared"
        );
    }

    #[test]
    fn zzi_symbols_with_override() {
        let d = std::env::temp_dir().join(format!("zz_nb_zzi_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        let zzi = d.join("plugin.zzi");
        std::fs::write(
            &zzi,
            "// Version: 1\n// C-ABI: 1\n// Plugin-version: 0.1.0\n\nextern \"C\" {\n    func my.init() -> int\n    func my.add(a: int, b: int) -> int = \"my_add_impl\";\n}\n",
        )
        .unwrap();
        let syms = expected_c_symbols(&zzi).unwrap();
        assert_eq!(syms, vec!["my_init".to_string(), "my_add_impl".to_string()]);
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn zzi_empty_is_error() {
        let d = std::env::temp_dir().join(format!("zz_nb_empty_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        let zzi = d.join("plugin.zzi");
        std::fs::write(&zzi, "// nothing here\n").unwrap();
        assert!(expected_c_symbols(&zzi).is_err());
        let _ = std::fs::remove_dir_all(&d);
    }

    /// Scratch C plugin: `csrc/add.c` + C-ABI `plugin.zzi`.
    fn scratch_plugin(dir: &Path, c_src: &str) {
        std::fs::create_dir_all(dir.join("csrc")).unwrap();
        std::fs::write(dir.join("csrc/add.c"), c_src).unwrap();
        std::fs::write(
            dir.join("plugin.zzi"),
            "// Version: 1\n// C-ABI: 1\n// Plugin-version: 0.1.0\n\nextern \"C\" {\n    func my.add(a: int, b: int) -> int\n    func my.version() -> int\n}\n",
        )
        .unwrap();
    }

    fn scratch_spec() -> BuildCcSpec {
        BuildCcSpec {
            sources: vec!["csrc/add.c".to_string()],
            include_dirs: vec!["csrc".to_string()],
            defines: vec!["NDEBUG".to_string()],
            cflags: vec!["-O2".to_string(), "-Wall".to_string(), "-fPIC".to_string()],
            libs: vec![],
            pkg_config: vec![],
            targets: vec![],
        }
    }

    const GOOD_C: &str = "const unsigned int ZZ_C_PLUGIN_ABI_VERSION = 1;\n\
         long long my_add(long long a, long long b) { return a + b; }\n\
         long long my_version(void) { return 1; }\n";

    #[test]
    fn ensure_cc_builds_and_verifies_both_engines() {
        if probe_cc().is_err() {
            eprintln!("skipping: no C compiler");
            return;
        }
        let d = std::env::temp_dir().join(format!(
            "zz_nb_e2e_{}_{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        scratch_plugin(&d, GOOD_C);
        let out = ensure_cc(&d, "my", &scratch_spec(), None).expect("declarative build works");
        assert!(out.archive.exists(), "static archive for AOT");
        assert!(out.shared.exists(), "shared lib for VM");
        assert_eq!(out.objects.len(), 1);
        assert!(out.cflags_path.exists() && out.ldflags_path.exists());
        assert!(!out.compiler.is_empty() && !out.tag.is_empty());
        // Rebuild is idempotent (same inputs → same layout, fresh bytes).
        let out2 = ensure_cc(&d, "my", &scratch_spec(), None).expect("rebuild works");
        assert_eq!(out.archive, out2.archive);
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn ensure_cc_rejects_missing_symbol() {
        if probe_cc().is_err() {
            eprintln!("skipping: no C compiler");
            return;
        }
        let d = std::env::temp_dir().join(format!(
            "zz_nb_neg_{}_{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        // Implements my_add but NOT my_version → static check must fail.
        scratch_plugin(
            &d,
            "const unsigned int ZZ_C_PLUGIN_ABI_VERSION = 1;\n\
             long long my_add(long long a, long long b) { return a + b; }\n",
        );
        let err = ensure_cc(&d, "my", &scratch_spec(), None).unwrap_err();
        assert!(err.contains("my_version"), "{err}");
        let _ = std::fs::remove_dir_all(&d);
    }

    /// Pack `(name, bytes)` pairs into `.tar.gz` bytes (test helper).
    fn tar_gz(files: &[(&str, &[u8])]) -> Vec<u8> {
        let enc = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
        let mut tar = tar::Builder::new(enc);
        for (name, data) in files {
            let mut header = tar::Header::new_gnu();
            header.set_size(data.len() as u64);
            header.set_mode(0o644);
            header.set_cksum();
            tar.append_data(&mut header, name, *data).unwrap();
        }
        let enc = tar.into_inner().unwrap();
        enc.finish().unwrap()
    }

    fn prebuilt_scratch(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!(
            "zz_nb_pb_{tag}_{}_{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        scratch_plugin(&d, GOOD_C);
        d
    }

    #[test]
    fn prebuilt_round_trip_installs_and_verifies() {
        if probe_cc().is_err() {
            eprintln!("skipping: no C compiler");
            return;
        }
        // Build real artifacts once, tar the build/ tree, install the
        // bytes into a fresh package dir (no compiler needed there —
        // install path never spawns cc).
        let src = prebuilt_scratch("src");
        let tag = host_tag().expect("host tag");
        let built = ensure_cc(&src, "my", &scratch_spec(), None).expect("source build");
        let mut entries: Vec<(String, Vec<u8>)> = Vec::new();
        for f in [
            &built.archive,
            &built.shared,
            &built.cflags_path,
            &built.ldflags_path,
        ]
        .into_iter()
        .chain(built.objects.iter())
        {
            let name = f.file_name().unwrap().to_string_lossy().into_owned();
            entries.push((name, std::fs::read(f).unwrap()));
        }
        let refs: Vec<(&str, &[u8])> = entries
            .iter()
            .map(|(n, b)| (n.as_str(), b.as_slice()))
            .collect();
        let bytes = tar_gz(&refs);
        let sha = crate::hash::hash_bytes(&bytes);

        let dest = prebuilt_scratch("dest");
        let out =
            install_prebuilt_bytes(&bytes, &dest, "my", &tag, &sha).expect("prebuilt install");
        assert_eq!(out.tag, tag);
        assert!(out.compiler.is_empty(), "no local compile");
        assert!(out.archive.exists() && out.shared.exists());
        let _ = std::fs::remove_dir_all(&src);
        let _ = std::fs::remove_dir_all(&dest);
    }

    #[test]
    fn prebuilt_partial_rejected() {
        // Static-only tarball: no shared lib → must fail closed.
        let bytes = tar_gz(&[
            ("libmy.a", b"fake-archive" as &[u8]),
            ("ldflags.txt", b"-lvips" as &[u8]),
        ]);
        let sha = crate::hash::hash_bytes(&bytes);
        let dest = prebuilt_scratch("partial");
        let err = install_prebuilt_bytes(&bytes, &dest, "my", "linux-x86_64-gnu-glibc2.28", &sha)
            .unwrap_err();
        assert!(err.contains("partial"), "{err}");
        let _ = std::fs::remove_dir_all(&dest);
    }

    #[test]
    fn prebuilt_sha_mismatch_fails_closed() {
        let bytes = tar_gz(&[("libmy.a", b"x" as &[u8])]);
        let dest = prebuilt_scratch("shamismatch");
        let err = install_prebuilt_bytes(
            &bytes,
            &dest,
            "my",
            "linux-x86_64-gnu-glibc2.28",
            &"0".repeat(64),
        )
        .unwrap_err();
        assert!(err.contains("mismatch"), "{err}");
        let _ = std::fs::remove_dir_all(&dest);
    }

    #[test]
    fn artifact_filename_parsing() {
        let d = prebuilt_scratch("names");
        // Wrong suffix.
        let bad = d.join("my-0.1.0-linux-x86_64-gnu-glibc2.28.zip");
        std::fs::write(&bad, b"x").unwrap();
        assert!(verify_artifact_file(&bad, &d, "my").is_err());
        // No valid tag inside.
        let bad2 = d.join("my-0.1.0-notaplatform.tgz");
        std::fs::write(&bad2, b"x").unwrap();
        assert!(verify_artifact_file(&bad2, &d, "my").is_err());
        let _ = std::fs::remove_dir_all(&d);
    }

    // ---- hermetic Rust backend -------------------------------------------

    fn rust_scratch(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!(
            "zz_rust_build_{name}_{}_{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn rust_spec() -> crate::manifest::BuildRustSpec {
        crate::manifest::BuildRustSpec {
            crate_path: "native".to_string(),
            cargo_features: Vec::new(),
            target: None,
            rustc_range: None,
            profile: None,
        }
    }

    fn write_no_dep_crate(pkg: &Path) {
        let native = pkg.join("native");
        std::fs::create_dir_all(native.join("src")).unwrap();
        std::fs::write(
            native.join("Cargo.toml"),
            r#"[package]
name = "rplug"
version = "0.1.0"
license = "MIT"
edition = "2021"

[lib]
name = "rplug"
crate-type = ["cdylib", "staticlib"]

[workspace]
"#,
        )
        .unwrap();
        std::fs::write(
            native.join("Cargo.lock"),
            "version = 3\n\n[[package]]\nname = \"rplug\"\nversion = \"0.1.0\"\n",
        )
        .unwrap();
        std::fs::write(
            native.join("src/lib.rs"),
            r#"#[no_mangle]
pub static ZZ_PLUGIN_ABI_VERSION: u32 = 1;

#[no_mangle]
pub extern "C" fn rplug_add(a: i64, b: i64) -> i64 {
    a + b
}
"#,
        )
        .unwrap();
        std::fs::write(
            pkg.join("plugin.zzi"),
            "// Version: 1\n// C-ABI: 1\n// Plugin-version: 0.1.0\n\nextern \"C\" {\n    func rplug.add(a: int, b: int) -> int\n}\n",
        )
        .unwrap();
    }

    #[test]
    fn rust_target_mapping_covers_supported_tags() {
        assert_eq!(
            rust_triple_for_tag("linux-x86_64-gnu-glibc2.28").unwrap(),
            "x86_64-unknown-linux-gnu"
        );
        assert_eq!(
            rust_triple_for_tag("linux-aarch64-musl").unwrap(),
            "aarch64-unknown-linux-musl"
        );
        assert_eq!(
            rust_triple_for_tag("macos-arm64-min12.0").unwrap(),
            "aarch64-apple-darwin"
        );
        assert_eq!(
            rust_triple_for_tag("windows-x86_64-msvc").unwrap(),
            "x86_64-pc-windows-msvc"
        );
        assert_eq!(
            rust_triple_for_tag("windows-aarch64-msvc").unwrap(),
            "aarch64-pc-windows-msvc"
        );
        let err = rust_triple_for_tag("plan9-arm64-none").unwrap_err();
        assert!(err.contains("no Rust target triple"), "{err}");
    }

    #[test]
    fn rust_build_plan_is_offline_locked_and_targeted() {
        let plan = plan_cargo_build(
            "cargo",
            Path::new("/pkg/build/rust-src/Cargo.toml"),
            Path::new("/pkg/build/rust-target"),
            Path::new("/pkg/build/rust-cwd"),
            Path::new("/pkg/build/rust-cargo-home"),
            Path::new("/pkg/build/.zz-tmp-rust"),
            "x86_64-unknown-linux-gnu",
            crate::manifest::RustProfile::Release,
            &["feat-a".to_string()],
        );
        assert_eq!(plan.program, "cargo");
        assert_eq!(plan.args[0], "build");
        assert!(plan.args.iter().any(|a| a == "--offline"));
        assert!(plan.args.iter().any(|a| a == "--locked"));
        assert!(plan.args.iter().any(|a| a == "--release"));
        assert!(plan
            .args
            .windows(2)
            .any(|w| w == ["--target", "x86_64-unknown-linux-gnu"]));
        assert!(plan
            .args
            .windows(2)
            .any(|w| w == ["--manifest-path", "/pkg/build/rust-src/Cargo.toml"]));
        assert!(plan
            .args
            .windows(2)
            .any(|w| w == ["--target-dir", "/pkg/build/rust-target"]));
        assert!(plan.args.windows(2).any(|w| w == ["--features", "feat-a"]));
        // No shell, ever; only the allowlisted environment is inherited.
        assert!(!plan.display_line().contains("sh -c"));
        assert_eq!(
            plan.env
                .iter()
                .find(|(k, _)| k == "CARGO_NET_OFFLINE")
                .map(|(_, v)| v.as_str()),
            Some("true")
        );
        for (k, _) in &plan.env {
            assert!(
                RUST_ENV_ALLOWLIST.contains(&k.as_str())
                    || matches!(
                        k.as_str(),
                        "CARGO_HOME"
                            | "TMPDIR"
                            | "SOURCE_DATE_EPOCH"
                            | "CARGO_TERM_COLOR"
                            | "CARGO_INCREMENTAL"
                            | "CARGO_NET_OFFLINE"
                    ),
                "unexpected inherited env `{k}`"
            );
        }
    }

    #[test]
    fn rust_vendor_plan_is_locked_and_fetch_only() {
        let plan = plan_cargo_vendor(
            "cargo",
            Path::new("/pkg/build/rust-src/Cargo.toml"),
            Path::new("/pkg/build/vendor"),
            Path::new("/pkg/build/rust-cwd"),
            Path::new("/pkg/build/rust-cargo-home"),
            Path::new("/pkg/build/.zz-tmp-rust"),
        );
        assert_eq!(plan.args[0], "vendor");
        assert!(plan.args.iter().any(|a| a == "--locked"));
        assert!(!plan.args.iter().any(|a| a == "--offline"));
        assert_eq!(
            plan.env
                .iter()
                .find(|(k, _)| k == "CARGO_NET_OFFLINE")
                .map(|(_, v)| v.as_str()),
            Some("false")
        );
    }

    #[test]
    fn rustc_range_is_enforced() {
        let v = semver::Version::parse("1.85.0").unwrap();
        assert!(check_rustc_range(crate::manifest::DEFAULT_RUSTC_RANGE, &v).is_ok());
        assert!(check_rustc_range(">=1.90", &v).is_err());
        assert!(check_rustc_range("not a range", &v).is_err());
    }

    #[test]
    fn rust_ensure_builds_both_engines_runtime_gated() {
        if probe_rust_toolchain().is_err() {
            eprintln!("skipping: cargo/rustc unavailable");
            return;
        }
        let pkg = rust_scratch("no_deps");
        write_no_dep_crate(&pkg);
        let spec = rust_spec();
        let tag = host_tag().expect("host tag");
        let out = ensure_rust(&pkg, "rplug", &spec, Some(&tag)).expect("hermetic rust build");
        assert!(out.archive.is_file(), "staticlib missing");
        assert!(out.shared.is_file(), "cdylib missing");
        assert!(out.cflags_path.is_file());
        assert_eq!(out.triple, rust_triple_for_tag(&tag).unwrap());
        assert_eq!(out.profile, "release");
        assert!(out.vendor_key.is_empty(), "no deps means no vendor key");
        // Artifacts live in the package build dir, never the crate tree.
        assert!(out.archive.starts_with(pkg.join("build")));
        let _ = std::fs::remove_dir_all(&pkg);
    }

    #[test]
    fn rust_ensure_rejects_build_rs_runtime_gated() {
        if probe_rust_toolchain().is_err() {
            eprintln!("skipping: cargo/rustc unavailable");
            return;
        }
        let pkg = rust_scratch("build_rs");
        write_no_dep_crate(&pkg);
        std::fs::write(pkg.join("native/build.rs"), "fn main() {}\n").unwrap();
        let err = ensure_rust(&pkg, "rplug", &rust_spec(), None).unwrap_err();
        assert!(err.contains("build.rs"), "{err}");
        let _ = std::fs::remove_dir_all(&pkg);
    }

    #[test]
    fn abi_stamp_count_requires_exactly_one() {
        let one = vec![C_ABI_STAMP.to_string(), "other".to_string()];
        assert_eq!(abi_stamp_count(&one), 1);
        let none = vec!["other".to_string()];
        assert_eq!(abi_stamp_count(&none), 0);
        let mixed = vec![
            C_ABI_STAMP.to_string(),
            RUST_ABI_STAMP.to_string(),
            C_ABI_STAMP.to_string(),
        ];
        assert_eq!(abi_stamp_count(&mixed), 3);
    }
}
