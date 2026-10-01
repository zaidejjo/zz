//! Terminal UI for the `zz` CLI: colors, staged progress, step headers.
//!
//! Rules:
//! - All progress/chatter goes to **stderr** so program output on stdout
//!   stays clean (pipes, `zz run` captures, scripts).
//! - Colors are enabled only when stderr is a TTY, `NO_COLOR` is unset,
//!   and `TERM` is not `dumb`.
//! - Non-TTY output degrades to plain `[n/m]` step lines — no ANSI, no
//!   carriage-return tricks, so logs stay greppable.

use std::io::IsTerminal;

/// True when styled output is appropriate on stderr.
pub fn color_enabled() -> bool {
    if std::env::var_os("NO_COLOR").is_some() {
        return false;
    }
    if std::env::var("TERM").as_deref() == Ok("dumb") {
        return false;
    }
    std::io::stderr().is_terminal()
}

fn paint(code: &str, msg: &str) -> String {
    if color_enabled() {
        format!("\x1b[{code}m{msg}\x1b[0m")
    } else {
        msg.to_string()
    }
}

pub fn bold(msg: &str) -> String {
    paint("1", msg)
}

fn colored(prefix: &str, code: &str, msg: &str) {
    eprintln!("{} {msg}", paint(code, prefix));
}

/// `✓ message` (green).
pub fn ok(msg: &str) {
    colored("✓", "32", msg);
}

/// `!! message` (yellow).
pub fn warn(msg: &str) {
    colored("!!", "33", msg);
}

/// `==> [2/5] message` staged step header (cyan bar scope).
pub fn step(current: usize, total: usize, msg: &str) {
    let tag = format!("[{current}/{total}]");
    eprintln!("{} {} {msg}", paint("36", "==>"), paint("1", &tag));
}

/// Render a fixed-width progress bar: `[██████──────] 3/8`.
pub fn bar(current: usize, total: usize, width: usize) -> String {
    let width = width.max(4);
    let filled = current
        .min(total)
        .saturating_mul(width)
        .checked_div(total)
        .unwrap_or(width)
        .min(width);
    let mut s = String::with_capacity(width + 8);
    s.push('[');
    for _ in 0..filled {
        s.push('█');
    }
    for _ in filled..width {
        s.push('─');
    }
    s.push(']');
    s.push(' ');
    s.push_str(&format!("{current}/{total}"));
    if color_enabled() {
        format!("\x1b[36m{s}\x1b[0m")
    } else {
        s
    }
}

/// `bar` line for a labeled item: `[███───] 1/4 fetching foo @ 1.2.0`.
pub fn progress(current: usize, total: usize, label: &str) {
    eprintln!("{} {label}", bar(current, total, 18));
}

/// Banner block for command headers.
pub fn header(title: &str) {
    eprintln!("{}", bold(title));
}

/// Format bytes as `1.2 MB` / `345.6 KB`.
pub fn human_bytes(bytes: u64) -> String {
    const KB: f64 = 1024.0;
    const MB: f64 = 1024.0 * 1024.0;
    let b = bytes as f64;
    if b >= MB {
        format!("{:.1} MB", b / MB)
    } else if b >= KB {
        format!("{:.1} KB", b / KB)
    } else {
        format!("{bytes} B")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bar_full_when_total_zero() {
        std::env::set_var("NO_COLOR", "1");
        let b = bar(0, 0, 8);
        assert_eq!(b, "[████████] 0/0");
        std::env::remove_var("NO_COLOR");
    }

    #[test]
    fn bar_scales_with_progress() {
        std::env::set_var("NO_COLOR", "1");
        assert_eq!(bar(0, 4, 8), "[────────] 0/4");
        assert_eq!(bar(2, 4, 8), "[████────] 2/4");
        assert_eq!(bar(4, 4, 8), "[████████] 4/4");
        // Over-full clamps.
        assert_eq!(bar(9, 4, 8), "[████████] 9/4");
        std::env::remove_var("NO_COLOR");
    }

    #[test]
    fn no_color_env_disables_styling() {
        std::env::set_var("NO_COLOR", "1");
        assert!(!color_enabled());
        assert_eq!(bold("x"), "x");
        std::env::remove_var("NO_COLOR");
    }

    #[test]
    fn human_bytes_formats() {
        std::env::set_var("NO_COLOR", "1");
        assert_eq!(human_bytes(512), "512 B");
        assert_eq!(human_bytes(2048), "2.0 KB");
        assert_eq!(human_bytes(2 * 1024 * 1024), "2.0 MB");
        std::env::remove_var("NO_COLOR");
    }
}
