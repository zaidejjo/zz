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
pub mod paths;
pub mod publish;
pub mod resolve;
