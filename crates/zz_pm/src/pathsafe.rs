//! Host-independent path containment checks.
//!
//! Package paths come from untrusted input (`zz.toml` written by someone
//! else, registry tarballs, publish payloads), so the answer must not depend
//! on which platform evaluates it:
//!
//! - Unix: `C:\abs.c` and `\\server\share` are ordinary relative names;
//!   `Path::is_absolute()` says no.
//! - Windows: `/abs.c` and `\abs.c` are rooted (they resolve against the
//!   current drive) but `Path::is_absolute()` says no either, because there
//!   is no drive prefix.
//!
//! Lexical checks over the string form close both gaps, so a manifest that
//! is rejected on Linux is rejected on Windows too — and vice versa.

use std::path::{Component, Path, PathBuf};

/// Drop Windows verbatim (`\\?\`) prefixes so a path produced by
/// `canonicalize()` or `read_link()` matches the spelling users, logs and
/// external tools expect.
///
/// `\\?\C:\x` → `C:\x`, `\\?\UNC\server\share` → `\\server\share`.
/// A no-op on Unix and on paths without the prefix.
pub fn strip_verbatim(p: &Path) -> PathBuf {
    let s = p.to_string_lossy();
    if let Some(rest) = s.strip_prefix(r"\\?\UNC\") {
        return PathBuf::from(format!(r"\\{rest}"));
    }
    match s.strip_prefix(r"\\?\") {
        Some(rest) => PathBuf::from(rest),
        None => p.to_path_buf(),
    }
}

/// True when `p` addresses something outside the current directory on any
/// supported host.
///
/// Accepts as absolute:
/// - `Path::is_absolute()` on the running host (`/x`, `C:\x`, `\\s\s\x`)
/// - anything with a leading `/` or `\` (`\x`, `\\server\share`)
/// - a Windows drive designator (`C:\x`, `C:/x`, and the drive-relative
///   `C:x`, which still resolves through `GetFullPathName` outside the root)
pub fn is_absolute(p: &Path) -> bool {
    let s = p.to_string_lossy();
    if s.starts_with('/') || s.starts_with('\\') || p.is_absolute() {
        return true;
    }
    let bytes = s.as_bytes();
    bytes.len() >= 2 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':'
}

/// True when `p` is absolute or walks upward, i.e. it can leave the
/// directory it is resolved against. Also rejects `Component::Prefix`, which
/// only appears on Windows but must be refused everywhere.
pub fn escapes_root(p: &Path) -> bool {
    is_absolute(p)
        || p.components()
            .any(|c| matches!(c, Component::ParentDir | Component::Prefix(_)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn absolute_spellings_detected_on_every_host() {
        for hostile in [
            "/abs",
            "\\abs",
            "C:\\abs",
            "C:/abs",
            "C:abs",
            "\\\\server\\share",
            "\\\\?\\C:\\verbatim",
        ] {
            assert!(is_absolute(Path::new(hostile)), "`{hostile}` absolute");
        }
    }

    #[test]
    fn relative_spellings_pass() {
        for ok in ["a", "a/b", "csrc/ok.c", "./a", "C-notes/x.c", "a-b: c"] {
            assert!(!is_absolute(Path::new(ok)), "`{ok}` relative");
            assert!(!escapes_root(Path::new(ok)), "`{ok}` contained");
        }
    }

    #[test]
    fn parent_components_escape() {
        for hostile in ["..", "../x", "a/../../x", "a/../.."] {
            assert!(escapes_root(Path::new(hostile)), "`{hostile}` escapes");
            assert!(!is_absolute(Path::new(hostile)), "`{hostile}` not absolute");
        }
    }

    #[test]
    fn single_dot_is_contained() {
        assert!(!escapes_root(Path::new(".")));
        assert!(!escapes_root(Path::new("a/./b")));
    }

    #[test]
    fn verbatim_prefixes_stripped() {
        // Unix paths never carry the prefix, so the round-trip is identity.
        assert_eq!(strip_verbatim(Path::new("/tmp/a")), Path::new("/tmp/a"));
        // The Windows spellings are handled lexically on every host, which is
        // what lets logs and tool argv stay clean on Windows.
        assert_eq!(
            strip_verbatim(Path::new(r"\\?\C:\proj\src")),
            Path::new(r"C:\proj\src")
        );
        assert_eq!(
            strip_verbatim(Path::new(r"\\?\UNC\server\share\x")),
            Path::new(r"\\server\share\x")
        );
    }
}
