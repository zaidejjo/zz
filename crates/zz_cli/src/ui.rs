//! Terminal UI for the `zz` CLI: colors, staged progress, step headers.
//!
//! Rules:
//! - All progress/chatter goes to **stderr** so program output on stdout
//!   stays clean (pipes, `zz run` captures, scripts).
//! - Colour follows [`ColorMode`]: `NO_COLOR` / `TERM=dumb` always disable,
//!   `ZZ_COLOR=never|always` overrides, otherwise colour only when stderr is
//!   a terminal.
//! - Non-TTY output degrades to plain `[n/m]` step lines — no in-place
//!   rewriting, no carriage returns, so logs stay greppable.

use std::io::IsTerminal;

/// Colour policy for stderr (#629).
///
/// Resolution order: `NO_COLOR` (any value) → never; `TERM=dumb` → never;
/// `ZZ_COLOR=never|always|auto` → that mode; default `auto` (colour only
/// when stderr is a terminal). `ZZ_COLOR=always` forces ANSI through a
/// pipe (CI logs, docs captures); `ZZ_COLOR=never` is the kill switch.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ColorMode {
    /// Never emit ANSI escapes.
    Never,
    /// Always emit ANSI escapes.
    Always,
    /// Emit ANSI escapes only when stderr is a terminal.
    Auto,
}

impl ColorMode {
    /// Resolve the mode from the environment.
    ///
    /// Deliberately per call (not cached): thread-scoped overrides in tests
    /// (`zz_pm::paths::env_scope`) must still work.
    pub fn resolve() -> ColorMode {
        if zz_pm::paths::env_var_os("NO_COLOR").is_some() {
            return ColorMode::Never;
        }
        if let Some(term) = zz_pm::paths::env_var_os("TERM") {
            if term == "dumb" {
                return ColorMode::Never;
            }
        }
        if let Some(value) = zz_pm::paths::env_var_os("ZZ_COLOR") {
            if value == "never" || value == "0" || value == "false" {
                return ColorMode::Never;
            }
            if value == "always" || value == "1" || value == "true" {
                return ColorMode::Always;
            }
        }
        ColorMode::Auto
    }

    /// Whether this mode produces ANSI escapes right now.
    pub fn enabled(self) -> bool {
        match self {
            ColorMode::Never => false,
            ColorMode::Always => true,
            ColorMode::Auto => stderr_is_tty(),
        }
    }
}

/// `stderr().is_terminal()`, cached: the answer cannot change during a
/// process, and every styled line would otherwise hit the same syscall.
fn stderr_is_tty() -> bool {
    static TTY: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *TTY.get_or_init(|| std::io::stderr().is_terminal())
}

/// True when styled output is appropriate on stderr.
pub fn color_enabled() -> bool {
    ColorMode::resolve().enabled()
}

/// Styling entry point: no call site hand-writes escapes (#629).
#[derive(Debug, Clone, Copy)]
pub struct Palette {
    enabled: bool,
}

impl Palette {
    /// Resolve the palette from the environment.
    pub fn new() -> Self {
        Self {
            enabled: color_enabled(),
        }
    }

    /// A palette that never emits escapes (tests and `--no-color` paths).
    #[cfg(test)]
    pub fn plain() -> Self {
        Self { enabled: false }
    }

    /// Wrap `msg` in the ANSI style `code` (SGR parameters, e.g. `"1;36"`).
    pub fn paint(&self, code: &str, msg: &str) -> String {
        if self.enabled {
            format!("\x1b[{code}m{msg}\x1b[0m")
        } else {
            msg.to_string()
        }
    }

    /// Bold.
    pub fn bold(&self, msg: &str) -> String {
        self.paint("1", msg)
    }

    /// Dim (counts, suffixes).
    pub fn dim(&self, msg: &str) -> String {
        self.paint("2", msg)
    }

    /// Green (`✓`).
    pub fn green(&self, msg: &str) -> String {
        self.paint("32", msg)
    }

    /// Red (`✗`).
    pub fn red(&self, msg: &str) -> String {
        self.paint("31", msg)
    }

    /// Cyan (bars).
    pub fn cyan(&self, msg: &str) -> String {
        self.paint("36", msg)
    }

    /// Blue (published paths).
    pub fn blue(&self, msg: &str) -> String {
        self.paint("34", msg)
    }

    /// Cyan bold (`==>` prefix, `[n/m]` counter, spinner frame).
    pub fn accent(&self, msg: &str) -> String {
        self.paint("1;36", msg)
    }
}

impl Default for Palette {
    fn default() -> Self {
        Self::new()
    }
}

fn paint(code: &str, msg: &str) -> String {
    Palette::new().paint(code, msg)
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
    Palette::new().cyan(&s)
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
///
/// The label is interior-mutable ([`Spinner::set_label`]) so a caller can
/// keep one animated line while the work it describes progresses (#629).
pub struct Spinner {
    done: std::sync::Arc<std::sync::atomic::AtomicBool>,
    handle: Option<std::thread::JoinHandle<()>>,
    label: std::sync::Arc<std::sync::Mutex<String>>,
    tty: bool,
}

impl Spinner {
    /// Start spinning with `label`. Cheap; use around any blocking call.
    pub fn start(label: &str) -> Self {
        Self::with_prefix("", label)
    }

    /// [`Spinner::start`] with a fixed prefix printed before the frame
    /// (`==> [1/5] Checking  `); the mutable label follows it.
    pub fn with_prefix(prefix: &str, label: &str) -> Self {
        use std::sync::atomic::AtomicBool;
        // Animation (in-place rewriting) needs a real terminal; colour is
        // controlled separately, so `ZZ_COLOR=always` through a pipe still
        // prints one clean line per step.
        let tty = stderr_is_tty();
        let done = std::sync::Arc::new(AtomicBool::new(false));
        let text = std::sync::Arc::new(std::sync::Mutex::new(label.to_string()));
        let handle = if tty {
            let done = done.clone();
            let label = text.clone();
            let prefix = prefix.to_string();
            Some(std::thread::spawn(move || {
                const FRAMES: &[char] = &['⠋', '⠙', '⠹', '⠸', '⠼', '⠴', '⠦', '⠧', '⠇', '⠏'];
                let start = std::time::Instant::now();
                let mut i = 0usize;
                while !done.load(std::sync::atomic::Ordering::Relaxed) {
                    let frame = FRAMES[i % FRAMES.len()];
                    let text = label.lock().map(|l| l.clone()).unwrap_or_default();
                    eprint!("\r{prefix}{frame} {text} {}s", start.elapsed().as_secs());
                    i += 1;
                    std::thread::sleep(std::time::Duration::from_millis(120));
                }
                // Clear the spinner line for whatever prints next.
                eprint!("\r\x1b[2K");
            }))
        } else {
            eprintln!("{prefix}{label}…");
            None
        };
        Self {
            done,
            handle,
            label: text,
            tty,
        }
    }

    /// Replace the animated label (no-op when piped: one line was already
    /// printed).
    pub fn set_label(&self, label: &str) {
        if let Ok(mut current) = self.label.lock() {
            *current = label.to_string();
        }
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

/// One numbered build step, rendered live (#629).
///
/// - TTY: a single line rewritten in place (`==> [1/5] Checking  ⠹ … 0.3s`),
///   then closed with `✓ detail · 0.42s`.
/// - Piped: nothing until [`Phase::done`] / [`Phase::fail`], which print
///   exactly one plain line (`==> [1/5] Checking: 12 modules (10 cached)`).
///
/// Timing is TTY-only: piped logs stay deterministic and greppable.
pub struct Phase {
    prefix: String,
    detail: std::sync::Arc<std::sync::Mutex<String>>,
    spinner: Option<Spinner>,
    tty: bool,
    started: std::time::Instant,
    palette: Palette,
}

impl Phase {
    /// Start `step` of `total` with `label`.
    pub fn start(step: usize, total: usize, label: &str) -> Phase {
        let palette = Palette::new();
        let prefix = format!(
            "{} {} {} ",
            palette.accent("==>"),
            palette.accent(&format!("[{step}/{total}]")),
            palette.bold(&format!("{label:<10}"))
        );
        Phase::start_with(prefix)
    }

    /// Start a phase with a pre-built prefix (nested pipelines supply their
    /// own indentation instead of the `==>` arrow).
    pub fn start_with(prefix: String) -> Phase {
        let palette = Palette::new();
        // Animation (in-place rewriting) needs a real terminal; colour is
        // controlled separately, so `ZZ_COLOR=always` through a pipe still
        // prints one clean line per step.
        let tty = stderr_is_tty();
        let spinner = if tty {
            Some(Spinner::with_prefix(&prefix, ""))
        } else {
            None
        };
        Phase {
            prefix,
            detail: std::sync::Arc::new(std::sync::Mutex::new(String::new())),
            spinner,
            tty,
            started: std::time::Instant::now(),
            palette,
        }
    }

    /// Update the live detail text (no-op when piped).
    pub fn set_detail(&self, detail: &str) {
        if let Some(spinner) = self.spinner.as_ref() {
            spinner.set_label(detail);
        }
        if let Ok(mut current) = self.detail.lock() {
            *current = detail.to_string();
        }
    }

    /// Close the step: `✓ detail · 0.42s` (TTY) / `: detail` (piped).
    pub fn done(self, detail: Option<&str>) {
        let detail = detail
            .map(str::to_string)
            .unwrap_or_else(|| self.detail.lock().map(|d| d.clone()).unwrap_or_default());
        if let Some(spinner) = self.spinner {
            spinner.stop_now();
        }
        if self.tty {
            let elapsed = self.started.elapsed();
            eprintln!(
                "\r\x1b[2K{}{} {}{}",
                self.prefix,
                self.palette.green("✓"),
                detail,
                self.palette
                    .dim(&format!(" · {:.2}s", elapsed.as_secs_f64()))
            );
        } else {
            eprintln!("{}: {detail}", self.prefix.trim_end());
        }
    }

    /// Close the step as failed: `✗ failed` (TTY) / `: failed` (piped).
    pub fn fail(self) {
        if let Some(spinner) = self.spinner {
            spinner.stop_now();
        }
        if self.tty {
            eprintln!("\r\x1b[2K{}{}", self.prefix, self.palette.red("✗ failed"));
        } else {
            eprintln!("{}: failed", self.prefix.trim_end());
        }
    }
}

impl Spinner {
    /// Stop the animation without printing anything (`Phase` prints its
    /// own closing line).
    fn stop_now(mut self) {
        self.stop();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Thread-scoped `NO_COLOR`: never touches the process environment, so
    /// a parallel test's styled output is unaffected.
    fn no_color() -> zz_pm::paths::EnvScope {
        zz_pm::paths::env_scope(&[("NO_COLOR", Some("1".into()))])
    }

    #[test]
    fn bar_full_when_total_zero() {
        let _c = no_color();
        let b = bar(0, 0, 8);
        assert_eq!(b, "[████████] 0/0");
    }

    #[test]
    fn bar_scales_with_progress() {
        let _c = no_color();
        assert_eq!(bar(0, 4, 8), "[────────] 0/4");
        assert_eq!(bar(2, 4, 8), "[████────] 2/4");
        assert_eq!(bar(4, 4, 8), "[████████] 4/4");
        // Over-full clamps.
        assert_eq!(bar(9, 4, 8), "[████████] 9/4");
    }

    #[test]
    fn no_color_env_disables_styling() {
        let _c = no_color();
        assert!(!color_enabled());
        assert_eq!(bold("x"), "x");
    }

    #[test]
    fn human_bytes_formats() {
        let _c = no_color();
        assert_eq!(human_bytes(512), "512 B");
        assert_eq!(human_bytes(2048), "2.0 KB");
        assert_eq!(human_bytes(2 * 1024 * 1024), "2.0 MB");
    }

    #[test]
    fn spinner_start_finish_is_quiet_when_piped() {
        // Under `cargo test` stderr is not a TTY: start prints one line,
        // finish prints the ok line, no thread is spawned.
        let spinner = Spinner::start("testing spinner");
        assert!(spinner.handle.is_none());
        spinner.finish("spinner done");
    }

    #[test]
    fn zz_color_always_forces_ansi_through_a_pipe() {
        let _c = zz_pm::paths::env_scope(&[
            ("NO_COLOR", None),
            ("TERM", Some("xterm-256color".into())),
            ("ZZ_COLOR", Some("always".into())),
        ]);
        assert_eq!(ColorMode::resolve(), ColorMode::Always);
        assert!(color_enabled());
        assert!(Palette::new().bold("x").contains("\x1b["));
    }

    #[test]
    fn zz_color_never_and_no_color_disable_styling() {
        let _c = zz_pm::paths::env_scope(&[("NO_COLOR", None), ("ZZ_COLOR", Some("never".into()))]);
        assert_eq!(ColorMode::resolve(), ColorMode::Never);
        assert!(!color_enabled());
        assert_eq!(Palette::new().bold("x"), "x");

        // NO_COLOR wins over an explicit `always`.
        let _c2 = zz_pm::paths::env_scope(&[
            ("NO_COLOR", Some("1".into())),
            ("ZZ_COLOR", Some("always".into())),
        ]);
        assert_eq!(ColorMode::resolve(), ColorMode::Never);
    }

    #[test]
    fn auto_mode_is_plain_when_piped() {
        let _c = zz_pm::paths::env_scope(&[
            ("NO_COLOR", None),
            ("ZZ_COLOR", None),
            ("TERM", Some("xterm".into())),
        ]);
        assert_eq!(ColorMode::resolve(), ColorMode::Auto);
        assert!(!color_enabled(), "cargo test stderr is not a TTY");
    }

    #[test]
    fn phase_prefix_is_numbered_and_piped_mode_is_quiet() {
        let _c = no_color();
        let phase = Phase::start(2, 5, "Optimizing");
        assert!(
            phase.prefix.starts_with("==> [2/5] Optimizing"),
            "{}",
            phase.prefix
        );
        assert!(!phase.tty, "cargo test stderr is not a TTY");
        assert!(phase.spinner.is_none(), "no animation thread when piped");
        phase.set_detail("512 functions");
        phase.done(Some("512 functions"));
    }

    #[test]
    fn phase_fail_line_is_plain_when_piped() {
        let _c = no_color();
        Phase::start(1, 5, "Checking").fail();
    }
}
