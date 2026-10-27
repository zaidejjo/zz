//! Shared helpers for the CLI integration tests.
//!
//! A subdirectory is not compiled as its own test target — each test file
//! that needs a helper pulls it in with `mod support;`.

#![allow(dead_code)] // every test file uses a different subset

use std::path::{Path, PathBuf};

/// Platform executable suffix (`".exe"` on Windows, empty elsewhere).
pub const EXE_SUFFIX: &str = std::env::consts::EXE_SUFFIX;

/// File name of a built binary: `name` plus the platform executable suffix.
///
/// `zz build` names its output through `bin_name`/`planned_dest_for`, which
/// append `.exe` for Windows hosts and targets; tests that execute the
/// produced file must use the same name.
pub fn exe_name(name: &str) -> String {
    format!("{name}{EXE_SUFFIX}")
}

/// Path of a built binary inside `dir`.
pub fn exe_in(dir: impl AsRef<Path>, name: &str) -> PathBuf {
    dir.as_ref().join(exe_name(name))
}

/// True when the fixture can run on this host.
///
/// A fixture whose header declares `// platform: unix` exercises Unix-only
/// stdlib surface (e.g. `net.unix_*`, which is a documented unsupported
/// stub on Windows). The harnesses skip it instead of reporting a failure
/// the platform cannot fix.
pub fn fixture_supported_here(path: &Path) -> bool {
    if cfg!(unix) {
        return true;
    }
    let text = std::fs::read_to_string(path).unwrap_or_default();
    !header_declares(&text, "platform: unix")
}

/// Scan only the leading `//` comment block, so a string in the fixture
/// body can never opt a test out by accident.
fn header_declares(text: &str, needle: &str) -> bool {
    for line in text.lines() {
        let trimmed = line.trim_start();
        if trimmed.is_empty() {
            continue;
        }
        if !trimmed.starts_with("//") {
            return false;
        }
        if trimmed.contains(needle) {
            return true;
        }
    }
    false
}
