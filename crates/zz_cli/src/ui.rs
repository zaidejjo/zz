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

/// Live spinner for long single-shot work (e.g. the clang link step).
///
/// - TTY: animates `⠋ label… 3s` on one stderr line until [`Spinner::finish`].
/// - Piped: prints `label…` once, then the finish line — logs stay linear.
/// - Dropping without `finish` stops the thread silently (no output).
pub struct Spinner {
    done: std::sync::Arc<std::sync::atomic::AtomicBool>,
    handle: Option<std::thread::JoinHandle<()>>,
    tty: bool,
}

impl Spinner {
    /// Start spinning with `label`. Cheap; use around any blocking call.
    pub fn start(label: &str) -> Self {
        use std::sync::atomic::AtomicBool;
        let tty = color_enabled();
        let done = std::sync::Arc::new(AtomicBool::new(false));
        let handle = if tty {
            let done = done.clone();
            let label = label.to_string();
            Some(std::thread::spawn(move || {
                const FRAMES: &[char] = &['⠋', '⠙', '⠹', '⠸', '⠼', '⠴', '⠦', '⠧', '⠇', '⠏'];
                let start = std::time::Instant::now();
                let mut i = 0usize;
                while !done.load(std::sync::atomic::Ordering::Relaxed) {
                    let frame = FRAMES[i % FRAMES.len()];
                    eprint!("\r{frame} {label}… {}s", start.elapsed().as_secs());
                    i += 1;
                    std::thread::sleep(std::time::Duration::from_millis(120));
                }
                // Clear the spinner line for whatever prints next.
                eprint!("\r\x1b[2K");
            }))
        } else {
            eprintln!("{label}…");
            None
        };
        Self { done, handle, tty }
    }

    /// Stop and report success: `✓ {msg}`.
    pub fn finish(mut self, msg: &str) {
        self.stop();
        if self.tty {
            eprintln!("\r\x1b[2K{} {msg}", paint("32", "✓"));
        } else {
            ok(msg);
        }
    }

    fn stop(&mut self) {
        self.done.store(true, std::sync::atomic::Ordering::Relaxed);
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

impl Drop for Spinner {
    fn drop(&mut self) {
        self.stop();
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

    #[test]
    fn spinner_start_finish_is_quiet_when_piped() {
        // Under `cargo test` stderr is not a TTY: start prints one line,
        // finish prints the ok line, no thread is spawned.
        let spinner = Spinner::start("testing spinner");
        assert!(spinner.handle.is_none());
        spinner.finish("spinner done");
    }
}
