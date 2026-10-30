//! ZZ package manager library.
//!
//! Core modules (M1):
//! - `paths` — ZZ_HOME resolution, directory layout
//! - `hash` — SHA-256 content hashing with normalization
//! - `manifest` — zz.toml parse/emit
//! - `lock` — zz.lock parse/emit with checksum verification
//! - `cas` — content-addressable storage with read-only enforcement
//!
//! Resolution modules (M2):
//! - `resolve` — dependency resolution with cycle detection (git + path only in M2)
//! - `git` — git-URL dependency fetching (parallel-safe via rayon)
//! - `link` — project-side symlink management with integrity checks
//!
//! Build cache modules (M3):
//! - `cache_key` — typed cache key struct with live path-dep hashing
//! - `build_cache` — per-module build artifact cache
//!
//! Publish/auth modules (M4):
//! - `auth` — login, credentials.toml with 0600 permissions
//! - `publish` — validate (no path deps), run zz test, pack artifact
//! - `gc` — reverse-refs.json-driven CAS garbage collection
//!
//! Local alias modules:
//! - `registry` — ~/.zz/registry.toml name → {path|git} aliases (no server)
//!
//! Remote registry modules:
//! - `remote` — HTTP client for the hosted registry (search/info/download/publish)
//!
//! Native build modules:
//! - `native_build` — declarative C/Rust plugin builds (no shell, no scripts)
//! - `rust_lock` — focused Cargo manifest/lockfile model for Rust plugins

/// Host-independent path helpers shared with the CLI and the AOT backends:
/// `ZZ_HOME`/cache layout ([`paths`]) and containment checks ([`pathsafe`]).
///
/// Both live in `zz_paths` so the Clang driver can use them without pulling
/// in the package manager; these re-exports keep existing call sites working.
pub use zz_paths::{paths, pathsafe};

pub mod auth;
pub mod build_cache;
pub mod cache_key;
pub mod cas;
pub mod gc;
pub mod git;
pub mod hash;
pub mod link;
pub mod lock;
pub mod manifest;
pub mod native_build;
pub mod publish;
pub mod registry;
pub mod remote;
pub mod resolve;
pub mod rust_lock;

/// Serialize the tests that still mutate process-global env (`ZZ_HOME`).
///
/// The path helpers moved to `zz_paths`, so the lock lives here: test
/// binaries are separate processes, and the suites that mutate `ZZ_HOME`
/// process-wide are all in this crate.
#[cfg(test)]
pub(crate) mod test_sync {
    static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    pub(crate) fn lock_env() -> std::sync::MutexGuard<'static, ()> {
        ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner())
    }
}
