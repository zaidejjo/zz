//! Pure cross-platform path lexing for `std.fs` (`normalize`, `join`,
//! `basename`, `dirname`, `is_absolute`, `extension`).
//!
//! Zero-copy where it matters: the normalizer works on byte slices and
//! builds the output in a single pass. No I/O, no allocation beyond the
//! result — safe to call on hot paths. The AOT C runtime (`zz_path_*` in
//! `core.c`) implements the identical algorithm; the unit tests below pin
//! the shared contract (including Windows/UNC shapes, which are exercised
//! through the explicit `windows` flag regardless of host).

/// Separator style. Selected from `sys.os()` at the call site
/// (`"windows"` → backslash, everything else → slash).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Style {
    Unix,
    Windows,
}

/// Map `sys.os()` (`"linux"`, `"macos"`, `"windows"`, …) to a style.
pub fn style_for_os(os: &str) -> Style {
    if os == "windows" {
        Style::Windows
    } else {
        Style::Unix
    }
}

/// Split off a Windows drive (`C:`) or UNC (`\\server\share`) prefix.
/// Returns `(prefix, rest)` where `prefix` already uses `/` separators.
fn split_win_prefix(p: &str) -> (&str, &str) {
    let b = p.as_bytes();
    // UNC: `\\server\share\...` or `//server/share/...`.
    if b.len() >= 2 && (b[0] == b'\\' || b[0] == b'/') && (b[1] == b'\\' || b[1] == b'/') {
        // Find end of `\\server\share`.
        let mut idx = 2;
        for _ in 0..2 {
            while idx < b.len() && (b[idx] == b'\\' || b[idx] == b'/') {
                idx += 1;
            }
            let start = idx;
            while idx < b.len() && b[idx] != b'\\' && b[idx] != b'/' {
                idx += 1;
            }
            if start == idx {
                break;
            }
        }
        return (&p[..idx], &p[idx..]);
    }
    // Drive: `C:...` or `C:/...`.
    if b.len() >= 2 && b[0].is_ascii_alphabetic() && b[1] == b':' {
        return (&p[..2], &p[2..]);
    }
    ("", p)
}

/// Normalize a path lexically (no I/O, no symlink resolution):
/// - separators become the platform one (`\` on Windows, `/` elsewhere;
///   both are accepted as input on either),
/// - redundant separators collapse, `.` segments drop,
/// - `..` pops the previous segment (clamped at the root),
/// - a trailing separator is stripped (except a bare root),
/// - `""` becomes `"."`.
///
/// Windows drive (`C:`), drive-rooted (`C:/`), absolute (`/`), and UNC
/// (`//server/share`) prefixes survive verbatim (slash-normalized).
pub fn normalize(path: &str, style: Style) -> String {
    let windows = style == Style::Windows;
    let sep = if windows { '\\' } else { '/' };
    if path.is_empty() {
        return ".".to_string();
    }
    // Prefix: UNC / rooted / drive on Windows, `/` root on Unix.
    let (prefix, rest_raw) = if windows {
        let b = path.as_bytes();
        if b.len() >= 2 && (b[0] == b'\\' || b[0] == b'/') && (b[1] == b'\\' || b[1] == b'/') {
            let (pre, rest) = split_win_prefix(path);
            // Re-emit the UNC prefix canonically (`\\server\share`).
            let mut canon = String::with_capacity(pre.len() + 1);
            canon.push(sep);
            canon.push(sep);
            let inner: Vec<&str> = pre.split(['/', '\\']).filter(|s| !s.is_empty()).collect();
            canon.push_str(&inner.join(&sep.to_string()));
            (canon, rest)
        } else if !b.is_empty() && (b[0] == b'\\' || b[0] == b'/') {
            // Rooted on the current drive (`\x`).
            (sep.to_string(), &path[1..])
        } else {
            let (pre, rest) = split_win_prefix(path);
            let mut canon = String::new();
            if !pre.is_empty() {
                canon.push_str(&pre[..1]);
                canon.push(':');
                if rest.starts_with('/') || rest.starts_with('\\') {
                    canon.push(sep);
                }
            }
            (canon, rest)
        }
    } else if path.starts_with('/') || path.starts_with('\\') {
        (sep.to_string(), &path[1..])
    } else {
        (String::new(), path)
    };
    // `C:` (drive-relative) keeps `..` even though a prefix exists.
    let drive_relative = windows && prefix.len() == 2 && prefix.ends_with(':');
    let mut rest = rest_raw;
    if windows && !prefix.is_empty() && (rest.starts_with('/') || rest.starts_with('\\')) {
        rest = &rest[1..];
    }
    let rooted = !prefix.is_empty() && !drive_relative;
    let mut out: Vec<&str> = Vec::new();
    for seg in rest.split(['/', '\\']) {
        match seg {
            "" | "." => {}
            ".." => {
                // Only pop a real segment; `..` never cancels `..`.
                let popped = out.last().is_some_and(|s| *s != "..") && out.pop().is_some();
                if !popped && !rooted {
                    out.push("..");
                }
            }
            s => out.push(s),
        }
    }
    let mut s = prefix;
    if !out.is_empty() {
        // Avoid `//x` when prefix already ends with the separator.
        if !s.is_empty() && !s.ends_with(sep) {
            s.push(sep);
        }
        s.push_str(&out.join(&sep.to_string()));
    } else if s.is_empty() {
        s.push('.');
    }
    s
}

/// Join two paths (`b` wins when absolute) and normalize the result.
pub fn join(a: &str, b: &str, style: Style) -> String {
    if b.is_empty() {
        return normalize(a, style);
    }
    if is_absolute(b, style) {
        return normalize(b, style);
    }
    if a.is_empty() {
        return normalize(b, style);
    }
    let sep = if style == Style::Windows { '\\' } else { '/' };
    let mut s = String::with_capacity(a.len() + b.len() + 1);
    s.push_str(a);
    s.push(sep);
    s.push_str(b);
    normalize(&s, style)
}

/// Final segment after the last separator (separator-normalized first).
/// A trailing separator is ignored (`/a/b/` → `b`).
pub fn basename(path: &str, style: Style) -> String {
    let n = normalize(path, style);
    if n == "." {
        return ".".to_string();
    }
    let sep = if style == Style::Windows { '\\' } else { '/' };
    // Roots have no basename.
    if n == sep.to_string() || (style == Style::Windows && n.len() == 3 && n.ends_with(":\\")) {
        return n;
    }
    match n.rfind(sep) {
        Some(i) => n[i + sep.len_utf8()..].to_string(),
        None => n,
    }
}

/// Directory part (everything before the final segment), normalized.
/// Returns `"."` when there is no directory part, the root for rooted paths.
pub fn dirname(path: &str, style: Style) -> String {
    let n = normalize(path, style);
    let sep = if style == Style::Windows { '\\' } else { '/' };
    match n.rfind(sep) {
        Some(0) => sep.to_string(),
        Some(i) => {
            // `C:\x` → `C:\`; UNC server/share roots keep their prefix.
            if style == Style::Windows && i == 2 && n.as_bytes()[1] == b':' {
                return n[..3].to_string();
            }
            n[..i].to_string()
        }
        None => ".".to_string(),
    }
}

/// True for rooted paths: `/x` (unix), `C:/x`, `C:\x`, `\\unc\...`
/// (windows). Bare `C:x` is drive-relative, not absolute.
pub fn is_absolute(path: &str, style: Style) -> bool {
    match style {
        Style::Unix => path.starts_with('/') || path.starts_with('\\'),
        Style::Windows => {
            let b = path.as_bytes();
            // UNC (`\\server\...`) or rooted (`\x`): absolute.
            if !b.is_empty() && (b[0] == b'\\' || b[0] == b'/') {
                return true;
            }
            // Drive-absolute: `C:/` or `C:\` (bare `C:x` is drive-relative).
            b.len() > 2
                && b[0].is_ascii_alphabetic()
                && b[1] == b':'
                && (b[2] == b'/' || b[2] == b'\\')
        }
    }
}

/// Extension without the dot (`archive.tar.gz` → `gz`, `.gitignore` →
/// `""`, `README` → `""`).
pub fn extension(path: &str, style: Style) -> String {
    let base = basename(path, style);
    if base == "." || base == ".." {
        return String::new();
    }
    match base.rfind('.') {
        Some(0) | None => String::new(),
        Some(i) => base[i + 1..].to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unix_basics() {
        assert_eq!(normalize("/a//b/./c/../d", Style::Unix), "/a/b/d");
        assert_eq!(normalize("a/b/../../c", Style::Unix), "c");
        assert_eq!(normalize("../../x", Style::Unix), "../../x");
        assert_eq!(normalize("/../..", Style::Unix), "/");
        assert_eq!(normalize("", Style::Unix), ".");
        assert_eq!(normalize("a/b/", Style::Unix), "a/b");
        assert_eq!(normalize("/", Style::Unix), "/");
    }

    #[test]
    fn backslash_accepted_on_unix() {
        assert_eq!(normalize("a\\b\\c", Style::Unix), "a/b/c");
    }

    #[test]
    fn windows_shapes() {
        assert_eq!(normalize("C:\\a\\b\\..\\c", Style::Windows), "C:\\a\\c");
        assert_eq!(normalize("C:/a//b", Style::Windows), "C:\\a\\b");
        assert_eq!(normalize("a/b/c", Style::Windows), "a\\b\\c");
        assert_eq!(
            normalize("\\\\server\\share\\a\\..\\b", Style::Windows),
            "\\\\server\\share\\b"
        );
        assert_eq!(
            normalize("//server/share/x", Style::Windows),
            "\\\\server\\share\\x"
        );
        assert_eq!(normalize("C:", Style::Windows), "C:");
        assert_eq!(normalize("\\x\\y", Style::Windows), "\\x\\y");
        assert!(is_absolute("\\x", Style::Windows));
    }

    #[test]
    fn join_wins_on_absolute() {
        assert_eq!(join("/a/b", "c/d", Style::Unix), "/a/b/c/d");
        assert_eq!(join("/a/b", "/c", Style::Unix), "/c");
        assert_eq!(join("/a", "../x", Style::Unix), "/x");
        assert_eq!(join("C:\\a", "b", Style::Windows), "C:\\a\\b");
        assert_eq!(join("C:\\a", "D:\\b", Style::Windows), "D:\\b");
    }

    #[test]
    fn parts() {
        assert_eq!(basename("/a/b/c.txt", Style::Unix), "c.txt");
        assert_eq!(basename("/a/b/", Style::Unix), "b");
        assert_eq!(basename("/", Style::Unix), "/");
        assert_eq!(dirname("/a/b/c.txt", Style::Unix), "/a/b");
        assert_eq!(dirname("c.txt", Style::Unix), ".");
        assert_eq!(dirname("/", Style::Unix), "/");
        assert_eq!(basename("C:\\a\\b.txt", Style::Windows), "b.txt");
        assert_eq!(dirname("C:\\a\\b.txt", Style::Windows), "C:\\a");
        assert_eq!(extension("archive.tar.gz", Style::Unix), "gz");
        assert_eq!(extension(".gitignore", Style::Unix), "");
        assert_eq!(extension("README", Style::Unix), "");
        assert!(!is_absolute("a/b", Style::Unix));
        assert!(is_absolute("/a", Style::Unix));
        assert!(is_absolute("C:\\a", Style::Windows));
        assert!(is_absolute("\\\\s\\share\\x", Style::Windows));
        assert!(!is_absolute("C:x", Style::Windows));
    }

    #[test]
    fn style_for_os_map() {
        assert_eq!(style_for_os("windows"), Style::Windows);
        assert_eq!(style_for_os("linux"), Style::Unix);
        assert_eq!(style_for_os("macos"), Style::Unix);
    }
}
