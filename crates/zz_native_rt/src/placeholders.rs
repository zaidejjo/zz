//! Universal placeholder transpiler — "write once, run anywhere".
//!
//! Users write one interpolation syntax (`{expr}` inside SQL strings).
//! The VM's `DbQuery` op lowers each `{expr}` to a positional `?N` marker
//! in a static template (values travel separately and are bound — never
//! concatenated). Each backend then renders the markers in its own
//! dialect via [`render`]:
//! - SQLite → `?1`, `?2` ([`PlaceholderStyle::Numbered`], identity),
//! - PostgreSQL → `$1`, `$2` ([`PlaceholderStyle::Dollar`]),
//! - MySQL → `?`, `?` ([`PlaceholderStyle::Plain`], positional).
//!
//! The scan is digit-aware: `?10` stays one marker (naive replacement
//! would corrupt it into `$1` + `0`), and a `?` not followed by a digit
//! passes through untouched.

/// Target placeholder dialect.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlaceholderStyle {
    /// `?1`, `?2` — SQLite (and the VM's canonical template form).
    Numbered,
    /// `$1`, `$2` — PostgreSQL extended protocol.
    Dollar,
    /// `?`, `?` — MySQL binary protocol (positional).
    Plain,
}

/// Render a `?N`-marker template into a backend dialect.
pub fn render(template: &str, style: PlaceholderStyle) -> String {
    match style {
        PlaceholderStyle::Numbered => template.to_string(),
        PlaceholderStyle::Dollar => map_markers(template, |out, digits| {
            out.push('$');
            out.push_str(digits);
        }),
        PlaceholderStyle::Plain => map_markers(template, |out, _| {
            out.push('?');
        }),
    }
}

/// Count `?N` markers in a template (each `{expr}` produces exactly one).
/// Reserved for driver diagnostics.
#[allow(dead_code)]
pub fn count_params(template: &str) -> usize {
    let mut n = 0;
    each_marker(template, |_| n += 1);
    n
}

/// Highest marker index (`?1..?N` → `N`, 0 when marker-free).
/// Reserved for driver diagnostics.
#[allow(dead_code)]
pub fn max_param_index(template: &str) -> usize {
    let mut max = 0;
    each_marker(template, |digits| {
        if let Ok(i) = digits.parse::<usize>() {
            max = max.max(i);
        }
    });
    max
}

fn each_marker(template: &str, mut f: impl FnMut(&str)) {
    let mut chars = template.chars().peekable();
    let mut buf = String::new();
    while let Some(c) = chars.next() {
        if c == '?' && chars.peek().is_some_and(|p| p.is_ascii_digit()) {
            buf.clear();
            while let Some(d) = chars.peek() {
                if d.is_ascii_digit() {
                    buf.push(*d);
                    chars.next();
                } else {
                    break;
                }
            }
            f(&buf);
        }
    }
}

fn map_markers(template: &str, mut f: impl FnMut(&mut String, &str)) -> String {
    let mut out = String::with_capacity(template.len());
    let mut chars = template.chars().peekable();
    let mut buf = String::new();
    while let Some(c) = chars.next() {
        if c == '?' && chars.peek().is_some_and(|p| p.is_ascii_digit()) {
            buf.clear();
            while let Some(d) = chars.peek() {
                if d.is_ascii_digit() {
                    buf.push(*d);
                    chars.next();
                } else {
                    break;
                }
            }
            f(&mut out, &buf);
        } else {
            out.push(c);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn one_template_three_dialects() {
        let t = "SELECT * FROM t WHERE a = ?1 AND b = ?2";
        assert_eq!(render(t, PlaceholderStyle::Numbered), t);
        assert_eq!(
            render(t, PlaceholderStyle::Dollar),
            "SELECT * FROM t WHERE a = $1 AND b = $2"
        );
        assert_eq!(
            render(t, PlaceholderStyle::Plain),
            "SELECT * FROM t WHERE a = ? AND b = ?"
        );
    }

    #[test]
    fn multi_digit_markers_stay_atomic() {
        assert_eq!(render("SELECT ?10", PlaceholderStyle::Dollar), "SELECT $10");
        assert_eq!(render("SELECT ?10", PlaceholderStyle::Plain), "SELECT ?");
        assert_eq!(count_params("SELECT ?1, ?10"), 2);
        assert_eq!(max_param_index("SELECT ?1, ?10"), 10);
    }

    #[test]
    fn bare_question_marks_pass_through() {
        assert_eq!(render("a ? b", PlaceholderStyle::Dollar), "a ? b");
        assert_eq!(render("no params", PlaceholderStyle::Plain), "no params");
        assert_eq!(count_params("SELECT 1"), 0);
        assert_eq!(max_param_index("SELECT 1"), 0);
    }
}
