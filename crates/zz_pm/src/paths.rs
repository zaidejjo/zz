//! ZZ_HOME resolution and directory layout.
//!
//! All paths respect the `ZZ_HOME` env var. Default:
//! - Linux/macOS: `$HOME/.zz/`
//! - Windows: `%USERPROFILE%\.zz\`

use std::path::PathBuf;

/// Resolve the ZZ home directory.
///
/// Priority: `ZZ_HOME` env var > platform default (`~/.zz/`).
pub fn zz_home() -> PathBuf {
    if let Ok(home) = std::env::var("ZZ_HOME") {
        return PathBuf::from(home);
    }
    let base = dirs::home_dir().unwrap_or_else(|| PathBuf::from("."));
    base.join(".zz")
}

/// `~/.zz/packages/` — CAS source storage.
pub fn packages_dir() -> PathBuf {
    zz_home().join("packages")
}

/// `~/.zz/cache/objects/` — per-module build artifact cache.
pub fn cache_objects_dir() -> PathBuf {
    zz_home().join("cache").join("objects")
}

/// `~/.zz/credentials.toml` — auth tokens (chmod 0600).
pub fn credentials_path() -> PathBuf {
    zz_home().join("credentials.toml")
}

/// `~/.zz/reverse-refs.json` — GC reference index.
pub fn reverse_refs_path() -> PathBuf {
    zz_home().join("reverse-refs.json")
}

/// `~/.zz/known-projects.json` — machine-wide project registry for safe GC.
pub fn known_projects_path() -> PathBuf {
    zz_home().join("known-projects.json")
}

/// CAS entry path for a given content hash/commit.
///
/// Returns `~/.zz/packages/<hash>` — the directory where extracted
/// source content lives after fetching into the CAS.
pub fn cas_entry(hash: &str) -> PathBuf {
    packages_dir().join(hash)
}

#[cfg(test)]
pub(crate) mod test_sync {
    //! Serialize tests that mutate process-global env (`ZZ_HOME`).
    //!
    //! `std::env::set_var` is process-wide, so tests touching `ZZ_HOME`
    //! must hold this lock for their whole body — otherwise two tests
    //! redirect each other's CAS/home paths mid-flight.
    static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    pub(crate) fn lock_env() -> std::sync::MutexGuard<'static, ()> {
        ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::env;

    #[test]
    fn zz_home_respects_env_var() {
        let _guard = crate::paths::test_sync::lock_env();
        let tmp = std::env::temp_dir().join("zz_pm_test_home");
        env::set_var("ZZ_HOME", &tmp);
        assert_eq!(zz_home(), tmp);
        env::remove_var("ZZ_HOME");
    }

    #[test]
    fn zz_home_default() {
        let _guard = crate::paths::test_sync::lock_env();
        env::remove_var("ZZ_HOME");
        let home = zz_home();
        assert!(home.ends_with(".zz"), "should end with .zz: {home:?}");
    }

    #[test]
    fn packages_dir_under_home() {
        let _guard = crate::paths::test_sync::lock_env();
        env::remove_var("ZZ_HOME");
        assert!(packages_dir().ends_with("packages"));
    }

    #[test]
    fn cache_objects_dir_under_home() {
        let _guard = crate::paths::test_sync::lock_env();
        env::remove_var("ZZ_HOME");
        let p = cache_objects_dir();
        assert!(p.ends_with("objects"), "should end with objects: {p:?}");
    }
}
