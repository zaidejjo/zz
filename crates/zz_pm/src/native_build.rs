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

use crate::manifest::{validate_cflag, validate_pkg_config_token, BuildCcSpec};

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
        return Ok("windows-x86_64-msvc".to_string());
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
        (["windows", "x86_64", "msvc"], ["windows", "x86_64", "msvc"]) => true,
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
        ["windows", "x86_64", ..] => Ok("x86_64-pc-windows-gnu"),
        _ => Err(format!("no LLVM triple for platform tag `{tag}`")),
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
    env
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
/// (`nm -g --defined-only`), as a set.
fn defined_symbols(path: &Path, tmpdir: &Path) -> Result<HashSet<String>, String> {
    let mut cmd = Command::new("nm");
    cmd.arg("-g").arg("--defined-only").arg(path);
    let out = run_hermetic(cmd, tmpdir, &format!("nm {}", path.display()))?;
    let mut set = HashSet::new();
    for line in String::from_utf8_lossy(&out).lines() {
        let line = line.trim();
        if line.is_empty() || line.ends_with(':') {
            continue; // archive member headers
        }
        // `addr TYPE name` or `TYPE name` or bare `name`.
        let cols: Vec<&str> = line.split_whitespace().collect();
        if let Some(name) = cols.last() {
            set.insert(name.to_string());
        }
    }
    Ok(set)
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

    // Static archive (AOT input).
    let archive = build_dir.join(format!("lib{stem}.a"));
    {
        let mut cmd = Command::new("ar");
        cmd.arg("rcs").arg(&archive);
        for o in &objects {
            cmd.arg(o);
        }
        run_hermetic(cmd, &tmpdir, "ar rcs")?;
    }

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
    let expected = expected_c_symbols(&zzi_path)?;
    let arch_syms = defined_symbols(&archive, &build_dir)
        .map_err(|e| format!("static verification failed for `{pkg_name}`: {e}"))?;
    for s in &expected {
        if !arch_syms.contains(s) {
            return Err(format!(
                "static verification failed for `{pkg_name}`: symbol `{s}` missing from {}\n\
                 hint: the C sources do not implement every plugin.zzi function",
                archive.display()
            ));
        }
    }
    let windows_aot_only = tag.starts_with("windows-");
    if !windows_aot_only {
        let shared_syms = defined_symbols(&shared, &build_dir)
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
        if !(shared_syms.contains(C_ABI_STAMP) || shared_syms.contains(RUST_ABI_STAMP)) {
            return Err(format!(
                "shared verification failed for `{pkg_name}`: neither {C_ABI_STAMP} nor {RUST_ABI_STAMP} in {}\n\
                 hint: export the ABI version stamp (see docs/plugin-author-guide.md §11)",
                shared.display()
            ));
        }
    } else {
        eprintln!("note: `{pkg_name}` on windows is AOT-only (VM dlopen deferred)");
    }

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
            "x86_64-pc-windows-gnu"
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
}
