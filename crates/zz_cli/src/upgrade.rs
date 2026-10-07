//! `zz upgrade`: self-update the compiler from GitHub releases.
//!
//! The installer scripts (`install.sh`/`install.ps1`) are for first-time
//! setup; `upgrade` keeps an existing install current without them:
//!
//! - `zz upgrade` — latest release over the current install dir
//! - `zz upgrade --check` — report only (exit 1 when behind)
//! - `zz upgrade --version vX.Y.Z` — pin (downgrades warn)
//! - `zz upgrade --keep-backup` — retain the `.bak` binaries on success
//!
//! Safety: download → temp extract → verify → atomic swap with rollback.
//! The previous binaries are restored if anything fails after the swap.

use crate::ui;

const REPO: &str = "zaidejjo/zz";
const API_LATEST: &str = "https://api.github.com/repos/zaidejjo/zz/releases/latest";

/// `(os, arch)` asset slug, or an error for unsupported platforms.
/// The compiler ships linux/macos × x86_64/aarch64 only.
fn platform_slug() -> Result<(&'static str, &'static str), String> {
    let os = match std::env::consts::OS {
        "linux" => "linux",
        "macos" => "macos",
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

/// `zz upgrade [--check] [--version vX.Y.Z] [--keep-backup]`.
pub fn run(args: &[String]) -> Result<(), String> {
    let check_only = args.iter().any(|a| a == "--check");
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
    if !dir.join("zz").exists() {
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

    ui::step(3, 4, "Verifying + swapping binaries");
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
    let unzip = std::process::Command::new("unzip")
        .arg("-q")
        .arg("-o")
        .arg(&zip_path)
        .arg("-d")
        .arg(&pkg)
        .output();
    match unzip {
        Ok(out) if out.status.success() => {}
        _ => {
            return cleanup(Err(
                "cannot extract release zip (need the `unzip` command)".to_string()
            ));
        }
    }
    let staged_zz = find_staged(&pkg, "zz").ok_or_else(|| {
        let _ = std::fs::remove_dir_all(&tmp);
        "release zip has no zz binary".to_string()
    })?;
    let staged_lsp = find_staged(&pkg, "zz-lsp");

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
        if live.exists() {
            std::fs::rename(&live, &bak).map_err(|e| {
                format!(
                    "cannot back up {}: {e} (check write permission)",
                    live.display()
                )
            })?;
        }
        if let Err(e) = std::fs::rename(&src, &live) {
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
    if let Err(e) = swap("zz", Some(staged_zz)) {
        let _ = std::fs::remove_dir_all(&tmp);
        return Err(e);
    }
    // The pair must never mix versions: a failed zz-lsp swap restores zz.
    if let Err(e) = swap("zz-lsp", staged_lsp) {
        let _ = std::fs::rename(dir.join("zz.bak"), dir.join("zz"));
        let _ = std::fs::remove_dir_all(&tmp);
        return Err(e);
    }

    ui::step(4, 4, "Verifying install");
    let got = std::process::Command::new(dir.join("zz"))
        .arg("--version")
        .output()
        .ok()
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .unwrap_or_default();
    let reported = got
        .split_whitespace()
        .find_map(|tok| parse_version(tok).ok());
    if reported != Some(want) {
        // Roll back both binaries.
        for name in ["zz", "zz-lsp"] {
            let live = dir.join(name);
            let bak = dir.join(format!("{name}.bak"));
            if bak.exists() {
                let _ = std::fs::rename(&bak, &live);
            }
        }
        let _ = std::fs::remove_dir_all(&tmp);
        return Err(format!(
            "upgraded binary reports `{got}` instead of {tag} (rolled back)"
        ));
    }
    if !keep_backup {
        for name in ["zz", "zz-lsp"] {
            let _ = std::fs::remove_file(dir.join(format!("{name}.bak")));
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
}
