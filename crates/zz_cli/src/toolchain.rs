//! `zz toolchain`: manage the Zig C backend.
//!
//! `zz build` needs a C backend: system `clang`, or `zig cc`. This command
//! installs a pinned Zig release under `~/.zz/toolchain` so builds work on
//! machines without (or with an ancient) system toolchain, and stay
//! reproducible across machines:
//!
//! - `zz toolchain install [--version X.Y.Z]` — download the release
//!   (latest stable by default), verify its sha256 against the official
//!   index, extract, smoke-test, and pin it.
//! - `zz toolchain use <version>` — switch the pin among installed versions.
//! - `zz toolchain uninstall <version>` — remove an installed version.
//! - `zz toolchain status` — installed versions, pin, active backend.
//!
//! Layout (override root with `ZZ_TOOLCHAIN_ROOT`):
//! `versions/<semver>/` holds one extracted release; the `pin` file names
//! the active version. A pin is explicit opt-in: once present, provider
//! probing prefers the managed `zig` over everything on PATH (see
//! `zz_codegen::detect_clang`), and the runtime-archive cache key includes
//! the pin so switching toolchains rebuilds instead of reusing.
//!
//! Sources are official `ziglang.org` releases via `download/index.json`
//! (never a constructed URL — asset naming changed between eras). All
//! writes go to a staging dir first and publish atomically.

use crate::ui;

// Keep in sync with `zz_codegen::compile::toolchain_root` (single source
// of truth lives there; this re-export avoids drift between the installer
// and the probing/cache code that consumes the layout).
fn root() -> std::path::PathBuf {
    zz_codegen::compile::toolchain_root()
}

fn versions_dir() -> std::path::PathBuf {
    root().join("versions")
}

fn version_dir(version: &str) -> std::path::PathBuf {
    versions_dir().join(version)
}

fn pin_file() -> std::path::PathBuf {
    root().join("pin")
}

const INDEX_URL: &str = "https://ziglang.org/download/index.json";

/// `(arch, os)` index key fragment, or an error for unsupported platforms.
/// The Zig index keys platforms `{arch}-{os}` with `macos` (not `darwin`).
fn platform_key() -> Result<&'static str, String> {
    match (std::env::consts::ARCH, std::env::consts::OS) {
        ("x86_64", "linux") => Ok("x86_64-linux"),
        ("aarch64", "linux") => Ok("aarch64-linux"),
        ("x86_64", "macos") => Ok("x86_64-macos"),
        ("aarch64", "macos") => Ok("aarch64-macos"),
        ("x86_64", "windows") => Ok("x86_64-windows"),
        (arch, os) => Err(format!(
            "no prebuilt zig for {arch}-{os}\n\
              hint: install clang 18+ or zig manually and ensure it is on PATH"
        )),
    }
}

/// Parse `X.Y.Z` (leading `v` tolerated) into `(major, minor, patch)`.
fn parse_version(tag: &str) -> Result<(u64, u64, u64), String> {
    let nums: Vec<&str> = tag.trim_start_matches('v').split('.').collect();
    if nums.len() != 3 {
        return Err(format!("invalid version `{tag}` (expected X.Y.Z)"));
    }
    let mut out = [0u64; 3];
    for (i, n) in nums.iter().enumerate() {
        // Reject build metadata (`0.15.1+…`) — pins track releases only.
        out[i] = n
            .parse()
            .map_err(|_| format!("invalid version `{tag}` (expected X.Y.Z)"))?;
    }
    Ok((out[0], out[1], out[2]))
}

/// HTTP agent with an explicit global timeout (bare `ureq::get` has none).
fn agent(timeout_secs: u64) -> ureq::Agent {
    ureq::Agent::new_with_config(
        ureq::config::Config::builder()
            .timeout_global(Some(std::time::Duration::from_secs(timeout_secs)))
            .user_agent(format!("zz-toolchain/{}", env!("CARGO_PKG_VERSION")))
            .build(),
    )
}

/// One resolved release: normalized version + verified download location.
struct Release {
    version: String,
    tarball: String,
    shasum: String,
}

/// Resolve `wanted` (or the latest stable) against the official index.
/// `master`/nightly builds are never selected implicitly — pins track
/// releases so builds stay reproducible.
fn resolve_release(
    index: &serde_json::Value,
    plat: &str,
    wanted: Option<&str>,
) -> Result<Release, String> {
    let table = index
        .as_object()
        .ok_or_else(|| "toolchain index is not a JSON object".to_string())?;
    let version: String = match wanted {
        Some(w) => {
            let w = w.trim_start_matches('v').to_string();
            parse_version(&w)?;
            if !table.contains_key(&w) {
                return Err(format!(
                    "zig {w} not in the release index\n\
                      hint: run `zz toolchain install` for the latest stable"
                ));
            }
            w
        }
        None => table
            .keys()
            .filter(|k| k.as_str() != "master")
            .filter_map(|k| parse_version(k).ok().map(|v| (v, k.clone())))
            .max()
            .map(|(_, k)| k)
            .ok_or_else(|| "toolchain index has no stable releases".to_string())?,
    };
    let entry = table
        .get(&version)
        .ok_or_else(|| format!("zig {version} missing from the release index"))?;
    let asset = entry.get(plat).ok_or_else(|| {
        format!(
            "zig {version} has no build for this platform ({plat})\n\
              hint: install clang 18+ manually instead"
        )
    })?;
    let tarball = asset
        .get("tarball")
        .and_then(|t| t.as_str())
        .filter(|t| !t.is_empty())
        .ok_or_else(|| format!("zig {version} index entry has no tarball URL"))?
        .to_string();
    let shasum = asset
        .get("shasum")
        .and_then(|t| t.as_str())
        .filter(|t| !t.is_empty())
        .ok_or_else(|| format!("zig {version} index entry has no shasum"))?
        .to_string();
    Ok(Release {
        version,
        tarball,
        shasum,
    })
}

/// Fetch the official release index.
fn fetch_index() -> Result<serde_json::Value, String> {
    let mut resp = agent(30)
        .get(INDEX_URL)
        .call()
        .map_err(|e| format!("cannot reach ziglang.org: {e}"))?;
    let text = resp
        .body_mut()
        .read_to_string()
        .map_err(|e| format!("cannot read toolchain index: {e}"))?;
    serde_json::from_str(&text).map_err(|e| format!("cannot parse toolchain index: {e}"))
}

/// sha256 hex of `bytes`.
fn sha256_hex(bytes: &[u8]) -> String {
    use sha2::Digest;
    hex::encode(sha2::Sha256::digest(bytes))
}

/// Path to the `zig` binary inside an extracted release dir.
fn zig_in(dir: &std::path::Path) -> std::path::PathBuf {
    dir.join(if cfg!(windows) { "zig.exe" } else { "zig" })
}

/// Smoke-test an installed release: `zig version` must run and report the
/// expected version. Old-but-working releases pass with a warning.
fn smoke_test(zig: &std::path::Path, version: &str) -> Result<(), String> {
    let out = std::process::Command::new(zig)
        .arg("version")
        .output()
        .map_err(|e| format!("cannot run {}: {e}", zig.display()))?;
    if !out.status.success() {
        return Err(format!(
            "{} version failed\n\
              hint: the download may be corrupt — remove {} and retry",
            zig.display(),
            version_dir(version).display()
        ));
    }
    let reported = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if reported != version {
        return Err(format!(
            "downloaded zig reports `{reported}` instead of `{version}`"
        ));
    }
    if parse_version(version)
        .map(|v| v < (0, 13, 0))
        .unwrap_or(false)
    {
        ui::warn(&format!(
            "zig {version} predates 0.13 — `zz build -p` flags may not be supported"
        ));
    }
    Ok(())
}

/// Is `version` installed and passing smoke test?
fn installed_ok(version: &str) -> bool {
    let dir = version_dir(version);
    let zig = zig_in(&dir);
    zig.is_file() && smoke_test(&zig, version).is_ok()
}

/// Record the pin (atomic: write temp + rename).
fn write_pin(version: &str) -> Result<(), String> {
    std::fs::create_dir_all(root())
        .map_err(|e| format!("cannot create {}: {e}", root().display()))?;
    let tmp = root().join(format!(".pin.{}.tmp", std::process::id()));
    std::fs::write(&tmp, format!("{version}\n")).map_err(|e| format!("cannot write pin: {e}"))?;
    std::fs::rename(&tmp, pin_file()).map_err(|e| format!("cannot publish pin: {e}"))?;
    Ok(())
}

/// Extract a `.tar.xz` release into `dest` (created fresh by the caller).
/// Entries are validated to stay inside `dest` (tar-slip refusal); mode
/// bits are preserved so `zig` stays executable.
fn extract_tar_xz(archive: &std::path::Path, dest: &std::path::Path) -> Result<(), String> {
    let xz = std::fs::File::open(archive).map_err(|e| format!("cannot read download: {e}"))?;
    let tar_path = dest.join("release.tar");
    let tar_out =
        std::fs::File::create(&tar_path).map_err(|e| format!("cannot stage archive: {e}"))?;
    let mut xz_reader = std::io::BufReader::new(xz);
    let mut tar_writer = std::io::BufWriter::new(tar_out);
    lzma_rs::xz_decompress(&mut xz_reader, &mut tar_writer)
        .map_err(|e| format!("cannot decompress release (not a valid .tar.xz?): {e}"))?;
    drop(tar_writer);
    let tar_in =
        std::fs::File::open(&tar_path).map_err(|e| format!("cannot read staged archive: {e}"))?;
    let mut ar = tar::Archive::new(tar_in);
    ar.set_preserve_permissions(true);
    for raw in ar
        .entries()
        .map_err(|e| format!("cannot list archive: {e}"))?
    {
        let mut entry = raw.map_err(|e| format!("cannot read archive entry: {e}"))?;
        // Fail closed on tar-slip: `unpack_in` skips entries escaping
        // `dest` (`Ok(false)`) instead of erroring, so a skipped entry
        // aborts the install rather than silently shorting the release.
        if !entry
            .unpack_in(dest)
            .map_err(|e| format!("cannot extract release: {e}"))?
        {
            return Err("release archive contains paths outside its root (refusing)".to_string());
        }
    }
    let _ = std::fs::remove_file(&tar_path);
    Ok(())
}

/// Extract a Windows `.zip` release with system `tar` (bsdtar handles zip
/// on modern Windows; same tool as the `zz upgrade` path uses `unzip` for).
fn extract_zip(archive: &std::path::Path, dest: &std::path::Path) -> Result<(), String> {
    let out = std::process::Command::new("tar")
        .arg("-xf")
        .arg(archive)
        .arg("-C")
        .arg(dest)
        .output()
        .map_err(|e| format!("cannot extract release zip: {e} (need `tar` on PATH)"))?;
    if !out.status.success() {
        return Err(format!(
            "cannot extract release zip:\n{}",
            String::from_utf8_lossy(&out.stderr)
        ));
    }
    Ok(())
}

/// After extraction, the release lives one level down
/// (`zig-<arch>-<os>-<ver>/`); hoist its contents into `dest` so the
/// layout is a stable `versions/<ver>/{zig,lib,…}`.
fn hoist_top_dir(dest: &std::path::Path) -> Result<(), String> {
    let mut kids: Vec<std::path::PathBuf> = std::fs::read_dir(dest)
        .map_err(|e| format!("cannot list staged release: {e}"))?
        .flatten()
        .map(|e| e.path())
        .collect();
    kids.sort();
    // Skip staging byproducts (the consumed release.tar is already gone,
    // but be explicit so future staging files never get hoisted).
    kids.retain(|p| p.file_name().and_then(|n| n.to_str()) != Some("release.tar"));
    let [top] = kids.as_slice() else {
        return Err("release archive has no top-level directory".to_string());
    };
    if !top.is_dir() {
        return Err("release archive top level is not a directory".to_string());
    }
    for entry in std::fs::read_dir(top)
        .map_err(|e| format!("cannot list staged release: {e}"))?
        .flatten()
    {
        let to = dest.join(entry.file_name());
        // `rename` may cross filesystems (TMPDIR vs HOME); fall back to copy.
        if std::fs::rename(entry.path(), &to).is_err() {
            if entry.path().is_dir() {
                copy_dir_all(&entry.path(), &to)?;
                let _ = std::fs::remove_dir_all(entry.path());
            } else {
                std::fs::copy(entry.path(), &to)
                    .map_err(|e| format!("cannot stage release file: {e}"))?;
                let _ = std::fs::remove_file(entry.path());
            }
        }
    }
    let _ = std::fs::remove_dir_all(top);
    Ok(())
}

fn copy_dir_all(from: &std::path::Path, to: &std::path::Path) -> Result<(), String> {
    std::fs::create_dir_all(to).map_err(|e| format!("cannot stage release dir: {e}"))?;
    for entry in std::fs::read_dir(from)
        .map_err(|e| format!("cannot list release dir: {e}"))?
        .flatten()
    {
        let to = to.join(entry.file_name());
        if entry.path().is_dir() {
            copy_dir_all(&entry.path(), &to)?;
        } else {
            std::fs::copy(entry.path(), &to)
                .map_err(|e| format!("cannot stage release file: {e}"))?;
        }
    }
    Ok(())
}

/// Installed versions (sorted), from `versions/`.
fn installed_versions() -> Vec<String> {
    let mut out = Vec::new();
    if let Ok(rd) = std::fs::read_dir(versions_dir()) {
        for e in rd.flatten() {
            if e.path().is_dir() {
                if let Some(n) = e.file_name().to_str() {
                    if parse_version(n).is_ok() && zig_in(&e.path()).is_file() {
                        out.push(n.to_string());
                    }
                }
            }
        }
    }
    out.sort_by(|a, b| {
        parse_version(a)
            .unwrap_or((0, 0, 0))
            .cmp(&parse_version(b).unwrap_or((0, 0, 0)))
    });
    out
}

/// One-line `zig version` for a path, or `None` when it cannot run.
fn zig_version_line(path: &std::path::Path) -> Option<String> {
    let out = std::process::Command::new(path)
        .arg("version")
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let v = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if v.is_empty() {
        None
    } else {
        Some(v)
    }
}

/// `zz toolchain <subcommand>`.
pub fn run(args: &[String]) -> Result<(), String> {
    let sub = args.first().map(|s| s.as_str()).unwrap_or("status");
    match sub {
        "install" => cmd_install(&args[1..]),
        "use" => cmd_use(&args[1..]),
        "uninstall" => cmd_uninstall(&args[1..]),
        "status" | "list" => cmd_status(),
        "--help" | "-h" | "help" => {
            print!("{TOOLCHAIN_USAGE}");
            Ok(())
        }
        other => Err(format!(
            "unknown toolchain subcommand `{other}`\n\
              hint: usage: zz toolchain <install|use|uninstall|status>"
        )),
    }
}

const TOOLCHAIN_USAGE: &str = "\
zz toolchain — manage the Zig C backend (into ~/.zz/toolchain)

  zz toolchain install [--version X.Y.Z]   download + verify + pin (default: latest stable)
  zz toolchain use <version>               switch the pin among installed versions
  zz toolchain uninstall <version>         remove an installed version
  zz toolchain status                      installed versions, pin, active backend
";

fn version_flag(args: &[String]) -> Result<Option<String>, String> {
    if let Some(i) = args.iter().position(|a| a == "--version") {
        return match args.get(i + 1) {
            Some(v) => Ok(Some(v.clone())),
            None => Err("missing value for --version (expected X.Y.Z)".to_string()),
        };
    }
    if let Some(v) = args.iter().find_map(|a| a.strip_prefix("--version=")) {
        return Ok(Some(v.to_string()));
    }
    Ok(None)
}

fn cmd_install(args: &[String]) -> Result<(), String> {
    let wanted = version_flag(args)?;
    if args.iter().any(|a| a == "--help" || a == "-h") {
        print!("{TOOLCHAIN_USAGE}");
        return Ok(());
    }
    let plat = platform_key()?;

    ui::header("zz toolchain install");
    ui::step(1, 4, "Resolving release");
    let index = fetch_index().map_err(|e| format!("{e}\nhint: check your network"))?;
    let rel = resolve_release(&index, plat, wanted.as_deref())?;
    ui::ok(&format!("zig {} for {plat}", rel.version));

    // Idempotent: a healthy install of the wanted release just re-pins.
    if installed_ok(&rel.version)
        && zz_codegen::compile::toolchain_pin().as_deref() == Some(rel.version.as_str())
    {
        println!(
            "zig {} already installed and pinned — nothing to do.",
            rel.version
        );
        return Ok(());
    }

    ui::step(2, 4, &format!("Downloading zig {}", rel.version));
    let archive_name = rel.tarball.rsplit('/').next().unwrap_or("release");
    let spinner = ui::Spinner::start(&format!("Fetching {archive_name}"));
    // Stage the download under versions/ directly: streaming to disk
    // avoids holding the ~50MB release in RAM (and ureq's default
    // in-memory body cap).
    let staging = versions_dir().join(format!(".tmp-{}-{}", rel.version, std::process::id()));
    if staging.exists() {
        let _ = std::fs::remove_dir_all(&staging);
    }
    std::fs::create_dir_all(&staging).map_err(|e| format!("cannot stage install: {e}"))?;
    let cleanup = |r: Result<(), String>| {
        let _ = std::fs::remove_dir_all(&staging);
        r
    };
    let archive_path = staging.join(archive_name);
    let download = || -> Result<u64, String> {
        let mut resp = agent(600)
            .get(&rel.tarball)
            .call()
            .map_err(|e| format!("download failed: {e}"))?;
        let mut body = resp.body_mut().as_reader();
        let mut out = std::fs::File::create(&archive_path)
            .map_err(|e| format!("cannot stage download: {e}"))?;
        std::io::copy(&mut body, &mut out).map_err(|e| format!("download failed: {e}"))?;
        out.metadata()
            .map(|m| m.len())
            .map_err(|e| format!("cannot stat download: {e}"))
    };
    match download() {
        Ok(n) => spinner.finish(&format!("downloaded {}", ui::human_bytes(n))),
        Err(e) => {
            drop(spinner);
            return cleanup(Err(e));
        }
    };

    ui::step(3, 4, "Verifying + extracting");
    let bytes = std::fs::read(&archive_path).map_err(|e| format!("cannot read download: {e}"))?;
    let digest = sha256_hex(&bytes);
    drop(bytes);
    if digest != rel.shasum {
        return Err(format!(
            "sha256 mismatch for zig {} (expected {}, got {digest})\n\
              hint: the mirror may be compromised or truncated — retry",
            rel.version, rel.shasum
        ));
    }
    ui::ok("sha256 matches the official index");

    // Publish by rename: a failed install never leaves a half-written
    // version dir behind (staging already holds the verified archive).
    if archive_name.ends_with(".zip") {
        if let Err(e) = extract_zip(&archive_path, &staging) {
            return cleanup(Err(e));
        }
    } else if archive_name.ends_with(".tar.xz") {
        if let Err(e) = extract_tar_xz(&archive_path, &staging) {
            return cleanup(Err(e));
        }
    } else {
        return cleanup(Err(format!("unsupported release archive `{archive_name}`")));
    }
    let _ = std::fs::remove_file(&archive_path);
    if let Err(e) = hoist_top_dir(&staging) {
        return cleanup(Err(e));
    }
    let staged_zig = zig_in(&staging);
    if let Err(e) = smoke_test(&staged_zig, &rel.version) {
        return cleanup(Err(e));
    }
    let dest = version_dir(&rel.version);
    if dest.exists() {
        let _ = std::fs::remove_dir_all(&dest);
    }
    if let Err(e) = std::fs::rename(&staging, &dest) {
        let _ = std::fs::remove_dir_all(&staging);
        return Err(format!("cannot publish install: {e}"));
    }
    // Re-verify from the published location (rename cannot corrupt, but a
    // pre-existing same-version dir could have raced us).
    if let Err(e) = smoke_test(&zig_in(&dest), &rel.version) {
        let _ = std::fs::remove_dir_all(&dest);
        return Err(e);
    }
    write_pin(&rel.version)?;

    ui::step(4, 4, "Done");
    ui::ok(&format!(
        "zig {} installed to {} and pinned",
        rel.version,
        dest.display()
    ));
    println!("pinned zig {}", rel.version);
    Ok(())
}

fn cmd_use(args: &[String]) -> Result<(), String> {
    let version = args
        .iter()
        .find(|a| !a.starts_with('-'))
        .cloned()
        .ok_or_else(|| {
            "missing version\n\
              hint: usage: zz toolchain use <version>"
                .to_string()
        })?;
    let version = version.trim_start_matches('v').to_string();
    parse_version(&version)?;
    if !installed_ok(&version) {
        return Err(format!(
            "zig {version} is not installed (or fails to run)\n\
              hint: install it with `zz toolchain install --version {version}`"
        ));
    }
    write_pin(&version)?;
    println!("pinned zig {version}");
    Ok(())
}

fn cmd_uninstall(args: &[String]) -> Result<(), String> {
    let version = args
        .iter()
        .find(|a| !a.starts_with('-'))
        .cloned()
        .ok_or_else(|| {
            "missing version\n\
              hint: usage: zz toolchain uninstall <version>"
                .to_string()
        })?;
    let version = version.trim_start_matches('v').to_string();
    parse_version(&version)?;
    let dir = version_dir(&version);
    if !dir.is_dir() {
        return Err(format!("zig {version} is not installed"));
    }
    std::fs::remove_dir_all(&dir).map_err(|e| format!("cannot remove {}: {e}", dir.display()))?;
    if zz_codegen::compile::toolchain_pin().as_deref() == Some(version.as_str()) {
        let _ = std::fs::remove_file(pin_file());
        println!("uninstalled zig {version} (pin cleared — probing PATH again)");
    } else {
        println!("uninstalled zig {version}");
    }
    Ok(())
}

fn cmd_status() -> Result<(), String> {
    ui::header("zz toolchain");
    println!("root: {}", root().display());
    let installed = installed_versions();
    if installed.is_empty() {
        println!("installed: none");
    } else {
        println!("installed: {}", installed.join(", "));
    }
    match zz_codegen::compile::toolchain_pin() {
        Some(pin) => {
            if installed.contains(&pin) {
                println!("pinned: {pin}");
            } else {
                println!("pinned: {pin} (missing — reinstall or `use` another)");
            }
        }
        None => println!("pinned: none"),
    }
    // Active backend follows the same probing builds use.
    match zz_codegen::detect_clang() {
        Some(clang) => {
            let detail = zig_version_line(&clang.path)
                .map(|v| format!("{v} at {}", clang.path.display()))
                .unwrap_or_else(|| format!("{} at {}", clang.label, clang.path.display()));
            let managed = zz_codegen::compile::managed_zig_path().is_some_and(|p| p == clang.path);
            println!(
                "active: {detail}{}",
                if managed { " (managed)" } else { "" }
            );
        }
        None => println!("active: none (install clang or run `zz toolchain install`)"),
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn version_parses_and_orders() {
        assert_eq!(parse_version("0.17.0").unwrap(), (0, 17, 0));
        assert_eq!(parse_version("v0.15.1").unwrap(), (0, 15, 1));
        assert!(parse_version("0.17").is_err());
        assert!(parse_version("latest").is_err());
        assert!(parse_version("0.15.1+build").is_err());
        assert!(parse_version("0.9.1").unwrap() < parse_version("0.17.0").unwrap());
        assert!(parse_version("0.15.2").unwrap() < parse_version("0.16.0").unwrap());
    }

    #[test]
    fn index_resolution_picks_stable_and_honors_pin() {
        let index = serde_json::json!({
            "master": {"version": "0.18.0-dev", "x86_64-linux": {"tarball": "m", "shasum": "s"}},
            "0.9.1": {"x86_64-linux": {"tarball": "https://old/zig.tar.xz", "shasum": "aaa"}},
            "0.17.0": {"x86_64-linux": {"tarball": "https://new/zig.tar.xz", "shasum": "bbb"}},
        });
        // Default: newest stable, never master.
        let rel = resolve_release(&index, "x86_64-linux", None).unwrap();
        assert_eq!(rel.version, "0.17.0");
        assert_eq!(rel.tarball, "https://new/zig.tar.xz");
        // Explicit pin (v-prefix tolerated).
        let rel = resolve_release(&index, "x86_64-linux", Some("v0.9.1")).unwrap();
        assert_eq!(rel.version, "0.9.1");
        // Unknown version / platform surface hints, not panics.
        assert!(resolve_release(&index, "x86_64-linux", Some("9.9.9")).is_err());
        assert!(resolve_release(&index, "riscv64-linux", None).is_err());
    }

    #[test]
    fn sha256_matches_known_vector() {
        assert_eq!(
            sha256_hex(b"abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }

    #[test]
    fn tar_slip_entries_are_refused() {
        // Build a malicious tar in memory: take a valid entry and patch
        // its name field to a `..` escape (the builder refuses to create
        // these, so patch the raw header + fix the checksum by hand).
        let mut buf = Vec::new();
        {
            let mut ar = tar::Builder::new(&mut buf);
            let mut hdr = tar::Header::new_gnu();
            hdr.set_size(3);
            hdr.set_mode(0o644);
            hdr.set_cksum();
            ar.append_data(&mut hdr, "x", b"xx\n".as_slice()).unwrap();
            ar.finish().unwrap();
        }
        assert!(buf.len() >= 512);
        buf[..11].copy_from_slice(b"../../evil\0");
        // Recompute the header checksum (field at 148..156, spaces during sum).
        let sum: u32 = buf[..148].iter().map(|b| *b as u32).sum::<u32>()
            + 8 * 32
            + buf[156..512].iter().map(|b| *b as u32).sum::<u32>();
        let cksum = format!("{sum:06o}\0 ");
        buf[148..156].copy_from_slice(cksum.as_bytes());
        let dir = std::env::temp_dir().join(format!("zz_toolchain_test_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let tar_path = dir.join("evil.tar");
        std::fs::write(&tar_path, &buf).unwrap();
        let mut ar = tar::Archive::new(std::fs::File::open(&tar_path).unwrap());
        // `unpack_in` skips (`Ok(false)`) entries escaping the dest
        // instead of erroring — the installer treats a skip as refusal.
        let mut skipped = false;
        for entry in ar.entries().unwrap() {
            let mut entry = entry.unwrap();
            if !entry.unpack_in(&dir).unwrap() {
                skipped = true;
            }
        }
        assert!(skipped, "escape entry must be skipped, not written");
        assert!(!dir.join("evil").exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn pin_round_trips_through_env_root() {
        // Hermetic: point the toolchain root at a temp dir.
        let root = std::env::temp_dir().join(format!("zz_toolchain_root_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::env::set_var("ZZ_TOOLCHAIN_ROOT", &root);
        assert!(zz_codegen::compile::toolchain_pin().is_none());
        write_pin("0.17.0").unwrap();
        assert_eq!(
            zz_codegen::compile::toolchain_pin().as_deref(),
            Some("0.17.0")
        );
        // Garbage pins are treated as unpinned, never fatal.
        std::fs::write(pin_file(), "../evil\n").unwrap();
        assert!(zz_codegen::compile::toolchain_pin().is_none());
        std::env::remove_var("ZZ_TOOLCHAIN_ROOT");
        let _ = std::fs::remove_dir_all(&root);
    }
}
