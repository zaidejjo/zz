//! `zz build` step renderer (#629).
//!
//! One [`zz_build::BuildObserver`] implementation formats the five build
//! steps for a terminal: numbered, timed, colourised, and rewritten in
//! place on a TTY; exactly one plain line per step when piped.
//!
//! The labels come from [`zz_build::STEP_LABELS`] (backend-neutral) and the
//! detail payloads from [`zz_build::StepDetail`], so this module never
//! mentions a pipeline internal.

use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Mutex;

use zz_build::{BuildTarget, StepDetail, STEP_LABELS};

use crate::ui::{self, Palette, Phase};

/// Where the steps are rendered.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Style {
    /// Top-level `zz build`: `==> [1/5] Checking`.
    Top,
    /// Inside another numbered pipeline (PGO legs, `zz install`):
    /// `    └ [1/5] Checking`, so the outer numbering stays unambiguous.
    Nested,
}

impl Style {
    fn indent(self) -> &'static str {
        match self {
            Style::Top => "",
            Style::Nested => "    └ ",
        }
    }

    /// Prefix for a step line. Nested lines drop `==>` (the arrow would
    /// compete with the outer pipeline's).
    fn prefix(self, palette: &Palette, step: usize, total: usize, label: &str) -> String {
        let counter = palette.accent(&format!("[{step}/{total}]"));
        let label = palette.bold(&format!("{label:<10}"));
        match self {
            Style::Top => format!("{} {counter} {label} ", palette.accent("==>")),
            Style::Nested => format!("{}{counter} {label} ", self.indent()),
        }
    }
}

/// Renderer for one release build.
///
/// Construct with [`BuildProgress::new`], wrap in
/// [`zz_build::ObserverSlot`] and put it in `ReleaseOptions::observer`.
pub struct BuildProgress {
    style: Style,
    labels: Mutex<Vec<&'static str>>,
    phase: Mutex<Option<Phase>>,
    /// Step whose phase is open (`0` when none). Guards against a detail
    /// arriving for a step that already closed (worker-pool tail calls).
    current_step: AtomicUsize,
    palette: Palette,
    modules_cached: AtomicUsize,
    files_cached: AtomicUsize,
    /// Highest finished-unit count seen. `file_done` arrives from the
    /// compiler worker pool, so ticks can land out of order; the bar must
    /// never move backwards.
    files_done: AtomicUsize,
    cached_hit: AtomicBool,
}

impl BuildProgress {
    /// Renderer for a top-level `zz build`.
    pub fn new(style: Style) -> Self {
        Self {
            style,
            labels: Mutex::new(STEP_LABELS.to_vec()),
            phase: Mutex::new(None),
            current_step: AtomicUsize::new(0),
            palette: Palette::new(),
            modules_cached: AtomicUsize::new(0),
            files_cached: AtomicUsize::new(0),
            files_done: AtomicUsize::new(0),
            cached_hit: AtomicBool::new(false),
        }
    }

    /// Print the `building <target> (<mode>)` header line.
    pub fn header(&self, target: &BuildTarget, mode: &str) {
        let indent = self.style.indent();
        let name = self.palette.bold(&target.label());
        match target.detail() {
            Some(entry) => eprintln!(
                "{indent}building {name} ({mode}) {}",
                self.palette.dim(&format!("· {entry}"))
            ),
            None => eprintln!("{indent}building {name} ({mode})"),
        }
    }

    /// Print the `[5/5] Publishing` step after a cache short-circuit, where
    /// the observer never saw a real publish.
    pub fn after_cache_hit(&self, dest: &std::path::Path, bytes: u64) {
        if !self.cache_hit() {
            return;
        }
        let phase = self.phase(STEP_LABELS.len());
        phase.done(Some(&self.published_text(dest, bytes)));
    }

    /// A phase handle for `step` in this renderer's style.
    fn phase(&self, step: usize) -> Phase {
        let total = STEP_LABELS.len();
        let label = self.label_for(step);
        match self.style {
            Style::Top => Phase::start(step, total, &label),
            Style::Nested => {
                Phase::start_with(self.style.prefix(&self.palette, step, total, &label))
            }
        }
    }

    /// True when this build was served from the cache.
    pub fn cache_hit(&self) -> bool {
        self.cached_hit.load(Ordering::Relaxed)
    }

    fn label_for(&self, step: usize) -> String {
        let labels = self.labels.lock().ok();
        labels
            .as_ref()
            .and_then(|l| l.get(step.wrapping_sub(1)).copied())
            .unwrap_or("")
            .to_string()
    }

    fn published_text(&self, dest: &std::path::Path, bytes: u64) -> String {
        format!(
            "{} {}",
            self.palette.blue(&dest.display().to_string()),
            self.palette.dim(&format!("({})", ui::human_bytes(bytes)))
        )
    }

    /// Close the current phase, if any (defensive: a step that never saw
    /// `done` must not leak an animation thread).
    fn take_phase(&self) -> Option<Phase> {
        self.phase.lock().ok().and_then(|mut p| p.take())
    }

    /// Live text for a step detail (`None` when the detail has no live
    /// form, e.g. the final `Linked` count).
    fn live_text(&self, step: usize, detail: StepDetail<'_>) -> Option<String> {
        match detail {
            StepDetail::Module {
                name,
                cached,
                done,
                total,
            } => {
                if cached {
                    self.modules_cached.fetch_add(1, Ordering::Relaxed);
                }
                let bar = ui::bar(done, total, 12);
                let cached = self.modules_cached.load(Ordering::Relaxed);
                let suffix = if cached > 0 {
                    format!(" · {cached} cached")
                } else {
                    String::new()
                };
                Some(format!("{bar} modules{suffix} · {name}"))
            }
            StepDetail::Compiling { files, cached } => {
                self.files_cached.store(cached, Ordering::Relaxed);
                Some(format!("{files} files"))
            }
            StepDetail::Files { done, total } => {
                let done = self.files_done.fetch_max(done, Ordering::Relaxed).max(done);
                let bar = ui::bar(done, total, 12);
                let cached = self.files_cached.load(Ordering::Relaxed);
                let suffix = if cached > 0 {
                    format!(" · {cached} cached")
                } else {
                    String::new()
                };
                Some(format!("{bar} files{suffix}"))
            }
            _ => {
                let _ = step;
                None
            }
        }
    }

    /// Final text for a finished step.
    fn done_text(&self, detail: StepDetail<'_>) -> String {
        match detail {
            StepDetail::Checked { modules, cached } => format!(
                "{modules} {} ({cached} cached)",
                if modules == 1 { "module" } else { "modules" }
            ),
            StepDetail::Optimized { functions } => format!(
                "{functions} {}",
                if functions == 1 {
                    "function"
                } else {
                    "functions"
                }
            ),
            StepDetail::Files { done, .. } => {
                let cached = self.files_cached.load(Ordering::Relaxed);
                let suffix = if cached > 0 {
                    format!(" ({cached} cached)")
                } else {
                    String::new()
                };
                format!(
                    "{done} {}{suffix}",
                    if done == 1 { "file" } else { "files" }
                )
            }
            // The link consumes the same files; repeating the cache ratio
            // there would read as if linking cached anything.
            StepDetail::Linked { files } => {
                format!("{files} {}", if files == 1 { "file" } else { "files" })
            }
            StepDetail::Published { path, bytes } => self.published_text(path, bytes),
            StepDetail::Compiling { files, .. } => {
                format!("{files} {}", if files == 1 { "file" } else { "files" })
            }
            StepDetail::Module { .. } => String::new(),
        }
    }
}

impl zz_build::BuildObserver for BuildProgress {
    fn plan(&self, labels: &[&'static str]) {
        if let Ok(mut current) = self.labels.lock() {
            if !labels.is_empty() {
                *current = labels.to_vec();
            }
        }
        // `plan` runs once per build; a nested pipeline reuses one renderer
        // across legs, so per-build counters must reset here.
        self.current_step.store(0, Ordering::Relaxed);
        self.modules_cached.store(0, Ordering::Relaxed);
        self.files_cached.store(0, Ordering::Relaxed);
        self.files_done.store(0, Ordering::Relaxed);
        self.cached_hit.store(false, Ordering::Relaxed);
    }

    fn start(&self, step: usize) {
        // A previous step that never closed (error path) must not leak.
        let _ = self.take_phase();
        let phase = self.phase(step);
        self.current_step.store(step, Ordering::Relaxed);
        if let Ok(mut slot) = self.phase.lock() {
            *slot = Some(phase);
        }
    }

    fn detail(&self, step: usize, detail: StepDetail<'_>) {
        if self.current_step.load(Ordering::Relaxed) != step {
            return;
        }
        let Some(text) = self.live_text(step, detail) else {
            return;
        };
        if let Ok(slot) = self.phase.lock() {
            if let Some(phase) = slot.as_ref() {
                phase.set_detail(&text);
            }
        }
    }

    fn done(&self, step: usize, detail: StepDetail<'_>) {
        if self.current_step.load(Ordering::Relaxed) != step {
            return;
        }
        self.current_step.store(0, Ordering::Relaxed);
        let Some(phase) = self.take_phase() else {
            return;
        };
        let text = self.done_text(detail);
        phase.done(Some(&text));
    }

    fn failed(&self, step: usize, _message: &str) {
        if self.current_step.load(Ordering::Relaxed) != step {
            return;
        }
        self.current_step.store(0, Ordering::Relaxed);
        if let Some(phase) = self.take_phase() {
            phase.fail();
        }
    }

    fn cached(&self, _label: &str) {
        self.cached_hit.store(true, Ordering::Relaxed);
        eprintln!(
            "{}{} {}",
            self.style.indent(),
            self.palette.accent("==>"),
            self.palette.dim("cached · source unchanged")
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use zz_build::BuildObserver;

    /// Thread-scoped env for a test (never touches the process env).
    fn scope(vars: &[(&str, Option<std::ffi::OsString>)]) -> zz_pm::paths::EnvScope {
        zz_pm::paths::env_scope(vars)
    }

    #[test]
    fn labels_come_from_the_plan() {
        let _c = scope(&[("NO_COLOR", Some("1".into()))]);
        let progress = BuildProgress::new(Style::Top);
        progress.plan(&["One", "Two", "Three", "Four", "Five"]);
        assert_eq!(progress.label_for(3), "Three");
        // Out-of-range indices stay empty instead of panicking.
        assert_eq!(progress.label_for(0), "");
        assert_eq!(progress.label_for(99), "");
    }

    #[test]
    fn detail_text_pluralises() {
        let _c = scope(&[("NO_COLOR", Some("1".into()))]);
        let progress = BuildProgress::new(Style::Top);
        assert_eq!(
            progress.done_text(StepDetail::Checked {
                modules: 1,
                cached: 0
            }),
            "1 module (0 cached)"
        );
        assert_eq!(
            progress.done_text(StepDetail::Checked {
                modules: 12,
                cached: 10
            }),
            "12 modules (10 cached)"
        );
        assert_eq!(
            progress.done_text(StepDetail::Optimized { functions: 1 }),
            "1 function"
        );
        assert_eq!(
            progress.done_text(StepDetail::Optimized { functions: 512 }),
            "512 functions"
        );
        assert_eq!(
            progress.done_text(StepDetail::Linked { files: 14 }),
            "14 files"
        );
    }

    #[test]
    fn cache_hit_flag_tracks_cached_callbacks() {
        let _c = scope(&[("NO_COLOR", Some("1".into()))]);
        let progress = BuildProgress::new(Style::Top);
        assert!(!progress.cache_hit());
        progress.cached("demopkg");
        assert!(progress.cache_hit());
    }

    #[test]
    fn nested_indent_is_used_for_prefixes() {
        let _c = scope(&[("NO_COLOR", Some("1".into()))]);
        let palette = Palette::plain();
        assert!(Style::Nested
            .prefix(&palette, 2, 5, "Optimizing")
            .starts_with("    └ "));
        assert!(Style::Top
            .prefix(&palette, 2, 5, "Optimizing")
            .starts_with("==> "));
    }

    #[test]
    fn piped_steps_print_one_plain_line() {
        let _c = scope(&[("NO_COLOR", Some("1".into()))]);
        let progress = BuildProgress::new(Style::Top);
        progress.start(1);
        progress.detail(
            1,
            StepDetail::Module {
                name: "std.http",
                cached: false,
                done: 1,
                total: 2,
            },
        );
        progress.done(
            1,
            StepDetail::Checked {
                modules: 2,
                cached: 1,
            },
        );
    }
}
