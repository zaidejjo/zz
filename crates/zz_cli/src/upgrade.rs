//! `zz upgrade`: self-update the compiler from GitHub releases.
//!
//! The installer scripts (`install.sh`/`install.ps1`) are for first-time
//! setup; `upgrade` keeps an existing install current without them:
//!
//! - `zz upgrade` — latest release over the current install dir
//! - `zz upgrade --dry-run` — report only (exit 1 when behind)
//! - `zz upgrade --version vX.Y.Z` — pin (downgrades warn)
//! - `zz upgrade --keep-backup` — retain the `.bak` binaries on success
//!
//! Safety: download → temp extract → verify → atomic swap with rollback.
//! The previous binaries are restored if anything fails after the swap.
//! `zz` and `zz-lsp` are always upgraded as one unit: a zip without the
//! LSP is rejected rather than leaving a version-skewed pair, and both
//! binaries are version-checked before the swap is considered successful.
//!
//! The zip's `lib/` (prebuilt native runtime, static libstd, bundled Zig
//! std) is swapped in the same transaction: those symbols are
//! version-locked to the compiler, so an old `lib/` next to a new binary
//! is a broken install. `lib/` keeps a `.bak` too and is restored with
//! the binaries when verification fails.

use crate::ui;

const REPO: &str = "zaidejjo/zz";
const API_LATEST: &str = "https://api.github.com/repos/zaidejjo/zz/releases/latest";

/// `(os, arch)` asset slug, or an error for unsupported platforms.
/// The compiler ships linux/macos/windows × x86_64/aarch64.
fn platform_slug() -> Result<(&'static str, &'static str), String> {
    let os = match std::env::consts::OS {
        "linux" => "linux",
        "macos" => "macos",
        "windows" => "windows",
        other => {
            return Err(format!(
                "no prebuilt zz for {other}\n\
                  hint: install from source with `cargo install --path crates/zz_cli`"
            ));
        }
    };
    let arch = match std::env::consts::ARCH {
        "x86_64" => "x86_64",
        "aarch64" => "aarch64",
        other => {
            return Err(format!(
                "no prebuilt zz for {os}-{other}\n\
                  hint: install from source with `cargo install --path crates/zz_cli`"
            ));
        }
    };
    Ok((os, arch))
}

/// Parse `v1.2.3` / `1.2.3` into `(major, minor, patch)`.
fn parse_version(tag: &str) -> Result<(u64, u64, u64), String> {
    let nums: Vec<&str> = tag.trim_start_matches('v').split('.').collect();
    if nums.len() != 3 {
        return Err(format!("invalid version `{tag}` (expected vX.Y.Z)"));
    }
    let mut out = [0u64; 3];
    for (i, n) in nums.iter().enumerate() {
        out[i] = n
            .parse()
            .map_err(|_| format!("invalid version `{tag}` (expected vX.Y.Z)"))?;
    }
    Ok((out[0], out[1], out[2]))
}

/// Asset file name for a release: `zz-0.1.6-linux-x86_64.zip`.
fn asset_name(ver: &str, os: &str, arch: &str) -> String {
    format!("zz-{ver}-{os}-{arch}.zip")
}

/// Installed binary file name: `zz` on Unix, `zz.exe` on Windows.
fn exe_name(base: &str) -> String {
    if cfg!(windows) {
        format!("{base}.exe")
    } else {
        base.to_string()
    }
}

/// Extract a release zip into `dest` (`unzip` on Unix, `Expand-Archive`
/// on Windows — neither ships on the other platform).
fn extract_release_zip(zip: &std::path::Path, dest: &std::path::Path) -> Result<(), String> {
    #[cfg(windows)]
    let out = std::process::Command::new("powershell")
        .args(["-NoProfile", "-NonInteractive", "-Command"])
        .arg(format!(
            // Single quotes are literal in PowerShell: double them, or a
            // path containing `'` breaks the command (and could inject).
            "Expand-Archive -LiteralPath '{}' -DestinationPath '{}' -Force",
            zip.display().to_string().replace('\'', "''"),
            dest.display().to_string().replace('\'', "''")
        ))
        .output();
    #[cfg(not(windows))]
    let out = std::process::Command::new("unzip")
        .arg("-q")
        .arg("-o")
        .arg(zip)
        .arg("-d")
        .arg(dest)
        .output();
    match out {
        Ok(o) if o.status.success() => Ok(()),
        Ok(o) => Err(format!(
            "cannot extract release zip: {}",
            String::from_utf8_lossy(&o.stderr).trim()
        )),
        Err(e) => Err(format!("cannot extract release zip: {e}")),
    }
}

/// Download URL for a release tag: `.../releases/download/v0.1.6/<asset>`.
fn asset_url(tag: &str, asset: &str) -> String {
    format!("https://github.com/{REPO}/releases/download/{tag}/{asset}")
}

/// HTTP agent with an explicit global timeout (bare `ureq::get` has none —
/// a blackhole network would hang the command forever).
fn agent(timeout_secs: u64) -> ureq::Agent {
    ureq::Agent::new_with_config(
        ureq::config::Config::builder()
            .timeout_global(Some(std::time::Duration::from_secs(timeout_secs)))
            .user_agent(format!("zz-upgrade/{}", env!("CARGO_PKG_VERSION")))
            .build(),
    )
}

/// Resolve the latest release tag via the GitHub API.
fn resolve_latest() -> Result<String, String> {
    let mut resp = agent(30)
        .get(API_LATEST)
        .header("Accept", "application/vnd.github+json")
        .call()
        .map_err(|e| format!("cannot reach api.github.com: {e}"))?;
    let text = resp
        .body_mut()
        .read_to_string()
        .map_err(|e| format!("cannot read release metadata: {e}"))?;
    let body: serde_json::Value =
        serde_json::from_str(&text).map_err(|e| format!("cannot parse release metadata: {e}"))?;
    body.get("tag_name")
        .and_then(|t| t.as_str())
        .map(str::to_string)
        .filter(|t| !t.is_empty())
        .ok_or_else(|| "release metadata has no tag_name".to_string())
}

/// Directory holding the running `zz` binary (the install to upgrade).
fn install_dir() -> Result<std::path::PathBuf, String> {
    let exe = std::env::current_exe().map_err(|e| format!("cannot locate current binary: {e}"))?;
    exe.parent()
        .map(|p| p.to_path_buf())
        .ok_or_else(|| "cannot locate install directory".to_string())
}

/// `zz upgrade [--dry-run] [--version vX.Y.Z] [--keep-backup]`.
/// `--check` is a deprecated alias for `--dry-run` until 0.3.
pub fn run(args: &[String]) -> Result<(), String> {
    let check_only = args.iter().any(|a| a == "--dry-run" || a == "--check");
    if args.iter().any(|a| a == "--check") {
        eprintln!("warning: `--check` is deprecated; use `--dry-run` instead");
    }
    let keep_backup = args.iter().any(|a| a == "--keep-backup");
    let pinned = args
        .iter()
        .position(|a| a == "--version")
        .and_then(|i| args.get(i + 1).cloned())
        .or_else(|| {
            args.iter()
                .find_map(|a| a.strip_prefix("--version=").map(str::to_string))
        });

    let (os, arch) = platform_slug()?;
    let current = env!("CARGO_PKG_VERSION");

    ui::header("zz upgrade");
    ui::step(1, 4, "Resolving version");
    let tag = match pinned {
        Some(v) => {
            if !v.starts_with('v') {
                return Err(format!(
                    "invalid version `{v}`\n\
                      hint: usage: zz upgrade --version vX.Y.Z"
                ));
            }
            parse_version(&v)?;
            v
        }
        None => resolve_latest()
            .map_err(|e| format!("{e}\nhint: check your network, or pin with --version vX.Y.Z"))?,
    };
    let ver = tag.trim_start_matches('v');
    ui::ok(&format!("latest: {tag} (installed: v{current})"));

    let cur = parse_version(current).unwrap_or((0, 0, 0));
    let want = parse_version(&tag)?;
    if want == cur {
        println!("zz {tag} already installed — nothing to do.");
        return Ok(());
    }
    if check_only {
        return Err(format!(
            "zz {tag} available (installed v{current})\n\
              hint: run `zz upgrade` to install it"
        ));
    }
    if want < cur {
        ui::warn(&format!("downgrading v{current} → {tag}"));
    }

    let dir = install_dir()?;
    let zz_bin = exe_name("zz");
    if !dir.join(&zz_bin).exists() {
        return Err(format!(
            "no zz binary in {}\n\
              hint: install first with the script at https://zz-lang.pages.dev/getting-started",
            dir.display()
        ));
    }

    ui::step(2, 4, &format!("Downloading {tag}"));
    let asset = asset_name(ver, os, arch);
    let url = asset_url(&tag, &asset);
    let spinner = ui::Spinner::start(&format!("Fetching {asset}"));
    let bytes = match agent(300).get(&url).call() {
        Ok(mut resp) => match resp.body_mut().read_to_vec() {
            Ok(b) => {
                spinner.finish(&format!("downloaded {}", ui::human_bytes(b.len() as u64)));
                b
            }
            Err(e) => {
                drop(spinner);
                return Err(format!("download failed: {e}"));
            }
        },
        Err(e) => {
            drop(spinner);
            return Err(format!(
                "download failed: {e}\n\
                  hint: the {os}-{arch} asset may not exist for {tag}"
            ));
        }
    };

    ui::step(3, 4, "Swapping binaries + runtime");
    let tmp = std::env::temp_dir().join(format!("zz-upgrade-{}", std::process::id()));
    if tmp.exists() {
        let _ = std::fs::remove_dir_all(&tmp);
    }
    std::fs::create_dir_all(&tmp).map_err(|e| format!("cannot create staging dir: {e}"))?;
    let cleanup = |code: Result<(), String>| {
        let _ = std::fs::remove_dir_all(&tmp);
        code
    };
    let zip_path = tmp.join(&asset);
    if let Err(e) = std::fs::write(&zip_path, &bytes) {
        return cleanup(Err(format!("cannot stage download: {e}")));
    }
    let pkg = tmp.join("pkg");
    if let Err(e) = extract_release_zip(&zip_path, &pkg) {
        return cleanup(Err(e));
    }
    let staged_zz = find_staged(&pkg, &exe_name("zz")).ok_or_else(|| {
        let _ = std::fs::remove_dir_all(&tmp);
        "release zip has no zz binary".to_string()
    })?;
    let staged_lsp = find_staged(&pkg, &exe_name("zz-lsp"));
    let lsp_bin = exe_name("zz-lsp");
    if staged_lsp.is_none() && dir.join(&lsp_bin).exists() {
        return cleanup(Err(
            "release zip has no zz-lsp binary; refusing to leave a mismatched \
             compiler/LSP pair installed\n\
             hint: this is a packaging bug — report it with the release tag"
                .to_string(),
        ));
    }

    // Swap with rollback: rename current aside, move new in, verify, and
    // restore the backup if verification fails.
    let swap = |name: &str, staged: Option<std::path::PathBuf>| -> Result<(), String> {
        let live = dir.join(name);
        if !live.exists() && staged.is_none() {
            return Ok(());
        }
        let Some(src) = staged else {
            return Ok(());
        };
        let bak = dir.join(format!("{name}.bak"));
        let _ = std::fs::remove_file(&bak);
        // Stage next to the target before renaming: the download is
        // unpacked in the temp dir, which may be a different filesystem
        // (tmpfs `/tmp` on Linux, another volume on Windows) where
        // `rename` fails with EXDEV.
        let local = dir.join(format!(".{name}.new-{}", std::process::id()));
        let _ = std::fs::remove_file(&local);
        std::fs::copy(&src, &local).map_err(|e| {
            format!(
                "cannot stage {}: {e} (check write permission)",
                live.display()
            )
        })?;
        if live.exists() {
            std::fs::rename(&live, &bak).map_err(|e| {
                let _ = std::fs::remove_file(&local);
                format!(
                    "cannot back up {}: {e} (check write permission)",
                    live.display()
                )
            })?;
        }
        if let Err(e) = std::fs::rename(&local, &live) {
            let _ = std::fs::remove_file(&local);
            let _ = std::fs::rename(&bak, &live);
            return Err(format!(
                "cannot install {}: {e} (rolled back)",
                live.display()
            ));
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            if let Ok(meta) = std::fs::metadata(&live) {
                let mut perms = meta.permissions();
                perms.set_mode(0o755);
                let _ = std::fs::set_permissions(&live, perms);
            }
        }
        Ok(())
    };
    if let Err(e) = swap(&zz_bin, Some(staged_zz)) {
        let _ = std::fs::remove_dir_all(&tmp);
        return Err(e);
    }
    // The pair must never mix versions: a failed zz-lsp swap restores zz.
    if let Err(e) = swap(&exe_name("zz-lsp"), staged_lsp) {
        let _ = std::fs::rename(dir.join(format!("{zz_bin}.bak")), dir.join(&zz_bin));
        let _ = std::fs::remove_dir_all(&tmp);
        return Err(e);
    }

    // The runtime archive (and, in bundled toolchains, Zig's own `lib/`)
    // must match this exact build: FFI entry points and static libstd are
    // version-locked to the compiler, so keeping an old `lib/` next to a
    // new binary is a broken install, not a stale-but-working one.
    let rt_lib = zz_codegen::ffi::lib_file_name();
    let published_lib = match find_staged_dir(&pkg, "lib") {
        Some(staged) if staged.join(rt_lib).is_file() => {
            if let Err(e) = publish_dir(&dir, "lib", &staged) {
                // Undo the binary swap: never leave a new compiler beside
                // the previous runtime.
                for base in ["zz", "zz-lsp"] {
                    let name = exe_name(base);
                    let live = dir.join(&name);
                    let bak = dir.join(format!("{name}.bak"));
                    if bak.exists() {
                        let _ = std::fs::rename(&bak, &live);
                    } else {
                        let _ = std::fs::remove_file(&live);
                    }
                }
                let _ = std::fs::remove_dir_all(&tmp);
                return Err(e);
            }
            true
        }
        Some(_) => {
            // `lib/` is present but lacks the version-locked runtime
            // archive: publishing it would pair a new compiler with the
            // previous ABI. Fail like any other packaging defect.
            for base in ["zz", "zz-lsp"] {
                let name = exe_name(base);
                let live = dir.join(&name);
                let bak = dir.join(format!("{name}.bak"));
                if bak.exists() {
                    let _ = std::fs::rename(&bak, &live);
                } else {
                    let _ = std::fs::remove_file(&live);
                }
            }
            let _ = std::fs::remove_dir_all(&tmp);
            return Err(format!(
                "release zip ships lib/ without {rt_lib}; refusing to leave the previous \
                 runtime next to the new compiler (rolled back)\n\
                 hint: this is a packaging bug — report it with the release tag"
            ));
        }
        None => false,
    };

    ui::step(4, 4, "Verifying install");
    // The compiler and its LSP ship as one pair: the LSP embeds the
    // checker of its own build, so a version skew between the two breaks
    // diagnostics. Verify both and roll both back together.
    let zz_reported = reported_version(&dir.join(&zz_bin));
    let lsp_path = dir.join(&lsp_bin);
    let lsp_installed = lsp_path.exists();
    let lsp_reported = reported_version(&lsp_path);
    let lsp_ok = !lsp_installed || lsp_reported == Some(want);
    if zz_reported != Some(want) || !lsp_ok {
        for base in ["zz", "zz-lsp"] {
            let name = exe_name(base);
            let live = dir.join(&name);
            let bak = dir.join(format!("{name}.bak"));
            if bak.exists() {
                let _ = std::fs::rename(&bak, &live);
            } else {
                // Newly installed with no previous copy: remove it so the
                // directory never keeps a half-upgraded pair.
                let _ = std::fs::remove_file(&live);
            }
        }
        if published_lib {
            let _ = std::fs::remove_dir_all(dir.join("lib"));
            let _ = std::fs::rename(dir.join("lib.bak"), dir.join("lib"));
        }
        let _ = std::fs::remove_dir_all(&tmp);
        let mut detail = format!("zz {}", version_label(zz_reported));
        if !lsp_ok {
            detail.push_str(&format!(", zz-lsp {}", version_label(lsp_reported)));
        }
        return Err(format!(
            "upgrade verification failed: {detail}, expected {tag} (rolled back)"
        ));
    }
    if !keep_backup {
        for base in ["zz", "zz-lsp"] {
            let _ = std::fs::remove_file(dir.join(format!("{}.bak", exe_name(base))));
        }
        if published_lib {
            let _ = std::fs::remove_dir_all(dir.join("lib.bak"));
        }
    }
    let _ = std::fs::remove_dir_all(&tmp);
    ui::ok(&format!("upgraded to {tag} in {}", dir.display()));
    println!("upgraded zz to {tag}");
    Ok(())
}

/// Find `name` at most two levels deep in an extracted release.
fn find_staged(pkg: &std::path::Path, name: &str) -> Option<std::path::PathBuf> {
    if pkg.join(name).is_file() {
        return Some(pkg.join(name));
    }
    std::fs::read_dir(pkg).ok()?.flatten().find_map(|e| {
        let p = e.path();
        if p.is_file() && p.file_name().and_then(|n| n.to_str()) == Some(name) {
            return Some(p);
        }
        if p.is_dir() {
            let inner = p.join(name);
            if inner.is_file() {
                return Some(inner);
            }
        }
        None
    })
}

/// Directory `name` inside an extracted release (top level, or one wrapper
/// level down, mirroring [`find_staged`]).
fn find_staged_dir(pkg: &std::path::Path, name: &str) -> Option<std::path::PathBuf> {
    let direct = pkg.join(name);
    if direct.is_dir() {
        return Some(direct);
    }
    std::fs::read_dir(pkg).ok()?.flatten().find_map(|e| {
        let p = e.path();
        if !p.is_dir() {
            return None;
        }
        let inner = p.join(name);
        inner.is_dir().then_some(inner)
    })
}

/// Recursively copy `from` into `to` (created if needed).
fn copy_dir_all(from: &std::path::Path, to: &std::path::Path) -> Result<(), String> {
    std::fs::create_dir_all(to).map_err(|e| format!("cannot create {}: {e}", to.display()))?;
    for entry in std::fs::read_dir(from)
        .map_err(|e| format!("cannot read {}: {e}", from.display()))?
        .flatten()
    {
        let src = entry.path();
        let dst = to.join(entry.file_name());
        let kind = entry
            .file_type()
            .map_err(|e| format!("cannot stat {}: {e}", src.display()))?;
        if kind.is_dir() {
            copy_dir_all(&src, &dst)?;
        } else {
            std::fs::copy(&src, &dst).map_err(|e| format!("cannot copy {}: {e}", src.display()))?;
        }
    }
    Ok(())
}

/// Replace `<dir>/<name>` with `staged`, keeping the previous tree as
/// `<name>.bak` so a failed verification can restore it.
///
/// The staged tree is copied *next to* the target first: the download is
/// unpacked in the temp dir, which may be a different filesystem (tmpfs on
/// Linux), where `rename` cannot cross.
fn publish_dir(dir: &std::path::Path, name: &str, staged: &std::path::Path) -> Result<(), String> {
    let live = dir.join(name);
    let bak = dir.join(format!("{name}.bak"));
    let new = dir.join(format!(".{name}.new-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&new);
    copy_dir_all(staged, &new).map_err(|e| {
        let _ = std::fs::remove_dir_all(&new);
        format!("cannot stage {name}/: {e}")
    })?;
    let _ = std::fs::remove_dir_all(&bak);
    if live.exists() {
        std::fs::rename(&live, &bak).map_err(|e| {
            let _ = std::fs::remove_dir_all(&new);
            format!(
                "cannot back up {name}/: {e} (check write permission in {})",
                dir.display()
            )
        })?;
    }
    if let Err(e) = std::fs::rename(&new, &live) {
        let _ = std::fs::rename(&bak, &live);
        let _ = std::fs::remove_dir_all(&new);
        return Err(format!("cannot install {name}/: {e} (rolled back)"));
    }
    Ok(())
}

/// Run `<bin> --version` and parse the first `X.Y.Z` token it prints.
fn reported_version(bin: &std::path::Path) -> Option<(u64, u64, u64)> {
    let out = std::process::Command::new(bin)
        .arg("--version")
        .output()
        .ok()?;
    String::from_utf8(out.stdout)
        .ok()?
        .split_whitespace()
        .find_map(|tok| parse_version(tok).ok())
}

/// Human label for a [`reported_version`] result.
fn version_label(v: Option<(u64, u64, u64)>) -> String {
    match v {
        Some((a, b, c)) => format!("v{a}.{b}.{c}"),
        None => "unknown version".to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn version_parses() {
        assert_eq!(parse_version("v0.1.6").unwrap(), (0, 1, 6));
        assert_eq!(parse_version("1.2.3").unwrap(), (1, 2, 3));
        assert!(parse_version("v1.2").is_err());
        assert!(parse_version("latest").is_err());
    }

    #[test]
    fn version_orders() {
        assert!(parse_version("v0.1.7").unwrap() > parse_version("v0.1.6").unwrap());
        assert!(parse_version("v0.2.0").unwrap() > parse_version("v0.1.9").unwrap());
    }

    #[test]
    fn asset_naming_matches_release_workflow() {
        assert_eq!(
            asset_name("0.1.6", "linux", "x86_64"),
            "zz-0.1.6-linux-x86_64.zip"
        );
        assert_eq!(
            asset_url("v0.1.6", "zz-0.1.6-macos-aarch64.zip"),
            "https://github.com/zaidejjo/zz/releases/download/v0.1.6/zz-0.1.6-macos-aarch64.zip"
        );
        // Windows assets use the same platform-uniform naming.
        assert_eq!(
            asset_name("0.4.0", "windows", "aarch64"),
            "zz-0.4.0-windows-aarch64.zip"
        );
    }

    #[test]
    fn exe_name_carries_the_platform_suffix() {
        let expected = if cfg!(windows) { "zz.exe" } else { "zz" };
        assert_eq!(exe_name("zz"), expected);
        assert!(exe_name("zz-lsp").starts_with("zz-lsp"));
    }

    #[test]
    fn finds_staged_binaries() {
        let dir = std::env::temp_dir().join(format!("zz_upgrade_test_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("dist")).unwrap();
        std::fs::write(dir.join("dist").join("zz"), "x").unwrap();
        // One subdirectory level is searched.
        assert!(find_staged(&dir, "zz").is_some());
        assert!(find_staged(&dir.join("dist"), "zz").is_some());
        assert!(find_staged(&dir, "zz-lsp").is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn finds_staged_lib_dir() {
        let dir = std::env::temp_dir().join(format!("zz_upgrade_lib_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("pkg").join("lib")).unwrap();
        assert_eq!(
            find_staged_dir(&dir.join("pkg"), "lib"),
            Some(dir.join("pkg").join("lib"))
        );
        // One wrapper level (the zip's own root) is searched too.
        assert_eq!(
            find_staged_dir(&dir, "lib"),
            Some(dir.join("pkg").join("lib"))
        );
        assert!(find_staged_dir(&dir, "missing").is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn publish_dir_replaces_tree_and_keeps_backup() {
        let root = std::env::temp_dir().join(format!("zz_upgrade_publish_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let install = root.join("install");
        let staged = root.join("pkg").join("lib");
        std::fs::create_dir_all(install.join("lib")).unwrap();
        std::fs::write(install.join("lib").join("old.a"), "old").unwrap();
        std::fs::create_dir_all(staged.join("std")).unwrap();
        std::fs::write(staged.join("std").join("std.zig"), "new").unwrap();

        publish_dir(&install, "lib", &staged).unwrap();
        assert_eq!(
            std::fs::read_to_string(install.join("lib").join("std").join("std.zig")).unwrap(),
            "new"
        );
        // Stale files from the previous release must not survive.
        assert!(!install.join("lib").join("old.a").exists());
        assert_eq!(
            std::fs::read_to_string(install.join("lib.bak").join("old.a")).unwrap(),
            "old"
        );
        let _ = std::fs::remove_dir_all(&root);
    }
}
