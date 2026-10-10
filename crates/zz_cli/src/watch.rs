//! `zz run --watch`: hot-reload loop for VM runs.
//!
//! Supervision model: the watcher is the parent, every generation of the
//! program is a fresh `zz run` child (same binary, watch flags stripped).
//! A fresh child means no leaked sockets, threads, or interpreter state
//! across restarts — the price is one process spawn per generation.
//!
//! The loop: snapshot mtimes → on change, debounce → `zz check` gate →
//! restart only when clean. A broken edit never replaces a running server.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::{Child, Command};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime};

/// Tick granularity of the watch loop.
const TICK_MS: u64 = 100;
/// How long a replaced generation gets to drain before SIGKILL.
const GRACE_MS: u64 = 3000;
/// Directories never descended into while snapshotting.
const SKIP_DIRS: &[&str] = &[
    "vendor",
    ".git",
    "bin",
    "build",
    "target",
    "node_modules",
    ".zz",
    ".zzbin",
];

/// Parsed `--watch` family flags plus the scrubbed child argv.
pub struct WatchFlags {
    pub debounce_ms: u64,
    pub clear: bool,
    pub ignores: Vec<String>,
}

/// Parse `--watch` / `--clear` / `--debounce <ms>` / `--debounce=<ms>` /
/// `--watch-ignore <glob>` (repeatable, `=` form too) out of a `run` arg
/// list. Returns the flags plus the args the child `zz run` must see
/// (watch-only flags removed, everything else — including order —
/// preserved).
pub fn parse_watch_flags(args: &[String]) -> Result<(WatchFlags, Vec<String>), String> {
    let mut debounce_ms = 150u64;
    let mut clear = false;
    let mut ignores: Vec<String> = Vec::new();
    let mut child: Vec<String> = Vec::with_capacity(args.len());
    let mut skip_next = false;
    let mut saw_watch = false;
    for a in args {
        if skip_next {
            skip_next = false;
            continue;
        }
        if a == "--watch" {
            saw_watch = true;
            continue;
        }
        if a == "--clear" {
            clear = true;
            continue;
        }
        if let Some(v) = a.strip_prefix("--debounce=") {
            debounce_ms = parse_debounce(v)?;
            continue;
        }
        if a == "--debounce" {
            // Space-separated value: skipped here, collected below.
            skip_next = true;
            continue;
        }
        if let Some(v) = a.strip_prefix("--watch-ignore=") {
            ignores.push(v.to_string());
            continue;
        }
        if a == "--watch-ignore" {
            skip_next = true;
            // Defer: the value is the next arg; track pending state via a
            // sentinel round-trip below. Simpler: peek by index.
            continue;
        }
        child.push(a.clone());
    }
    // Second pass collects space-separated `--debounce` / `--watch-ignore`
    // values (their flags already set `skip_next` above, so values never
    // leak into the child args).
    let mut i = 0;
    while i < args.len() {
        if args[i] == "--debounce" {
            let v = args
                .get(i + 1)
                .ok_or("missing value for `--debounce`\n\nhint: zz run --watch --debounce 150")?;
            debounce_ms = parse_debounce(v)?;
            i += 2;
            continue;
        }
        if args[i] == "--watch-ignore" {
            let v = args.get(i + 1).ok_or(
                "missing value for `--watch-ignore`\n\nhint: zz run --watch --watch-ignore 'target/*'",
            )?;
            ignores.push(v.clone());
            i += 2;
            continue;
        }
        i += 1;
    }
    if !saw_watch {
        return Err("internal error: parse_watch_flags called without --watch".to_string());
    }
    Ok((
        WatchFlags {
            debounce_ms,
            clear,
            ignores,
        },
        child,
    ))
}

fn parse_debounce(v: &str) -> Result<u64, String> {
    v.parse::<u64>().map_err(|_| {
        format!("invalid `--debounce` value `{v}`\n\nhint: milliseconds, e.g. --debounce 150")
    })
}

/// File identity for change detection: mtime plus length (coarse mtimes
/// alone miss same-second same-size rewrites on some filesystems).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
struct FileSig {
    mtime_secs: u64,
    mtime_nanos: u32,
    len: u64,
}

fn sig_of(path: &Path) -> Option<FileSig> {
    let meta = std::fs::metadata(path).ok()?;
    if !meta.is_file() {
        return None;
    }
    let mtime = meta.modified().ok().unwrap_or(SystemTime::UNIX_EPOCH);
    let dur = mtime
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap_or_default();
    Some(FileSig {
        mtime_secs: dur.as_secs(),
        mtime_nanos: dur.subsec_nanos(),
        len: meta.len(),
    })
}

/// `*` matches any run (including `/`), `?` one char, else literal.
pub fn match_glob(pattern: &str, text: &str) -> bool {
    fn rec(p: &[u8], t: &[u8]) -> bool {
        if p.is_empty() {
            return t.is_empty();
        }
        if p[0] == b'*' {
            // Collapse runs of `*`, then try every split.
            let mut pi = 0;
            while pi < p.len() && p[pi] == b'*' {
                pi += 1;
            }
            if pi == p.len() {
                return true;
            }
            for ti in 0..=t.len() {
                if rec(&p[pi..], &t[ti..]) {
                    return true;
                }
            }
            return false;
        }
        if t.is_empty() {
            return false;
        }
        if p[0] == b'?' || p[0] == t[0] {
            return rec(&p[1..], &t[1..]);
        }
        false
    }
    rec(pattern.as_bytes(), text.as_bytes())
}

/// True when `path` (or its file name) matches any ignore glob.
pub fn is_ignored(path: &Path, ignores: &[String]) -> bool {
    let full = path.to_string_lossy();
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    ignores
        .iter()
        .any(|g| match_glob(g, &full) || match_glob(g, &name))
}

fn watched_file(path: &Path, in_embed: bool) -> bool {
    if in_embed {
        return true;
    }
    match path.extension().and_then(|e| e.to_str()) {
        Some("zz") => true,
        _ => matches!(
            path.file_name().and_then(|n| n.to_str()),
            Some("zz.toml") | Some("zz.lock")
        ),
    }
}

/// Recursively snapshot `root`. `in_embed` watches every file (assets);
/// otherwise only `*.zz` + manifests. `SKIP_DIRS` pruned, ignores honored.
fn snapshot_dir(
    root: &Path,
    in_embed: bool,
    ignores: &[String],
    out: &mut HashMap<PathBuf, FileSig>,
) {
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let rd = match std::fs::read_dir(&dir) {
            Ok(rd) => rd,
            Err(_) => continue,
        };
        for entry in rd.flatten() {
            let path = entry.path();
            if is_ignored(&path, ignores) {
                continue;
            }
            let ft = match entry.file_type() {
                Ok(ft) => ft,
                Err(_) => continue,
            };
            if ft.is_dir() {
                if ft.is_symlink() {
                    continue;
                }
                if let Some(name) = path.file_name().and_then(|n| n.to_str()) {
                    if SKIP_DIRS.contains(&name) {
                        continue;
                    }
                }
                stack.push(path);
            } else if ft.is_file() && watched_file(&path, in_embed) {
                if let Some(sig) = sig_of(&path) {
                    out.insert(path, sig);
                }
            }
        }
    }
}

/// Human-readable change between two snapshots (for logs, capped).
fn diff_snapshots(
    old: &HashMap<PathBuf, FileSig>,
    new: &HashMap<PathBuf, FileSig>,
) -> Vec<PathBuf> {
    let mut changed: Vec<PathBuf> = Vec::new();
    for (path, sig) in new {
        match old.get(path) {
            Some(prev) if prev == sig => {}
            _ => changed.push(path.clone()),
        }
    }
    for path in old.keys() {
        if !new.contains_key(path) {
            changed.push(path.clone());
        }
    }
    changed.sort();
    changed
}

/// Run the `zz check` gate in a short-lived child (inherits stdio so
/// diagnostics print exactly like a manual run). True when clean.
fn check_gate(exe: &Path, entry: &str) -> bool {
    match Command::new(exe).arg("check").arg(entry).status() {
        Ok(status) => status.success(),
        Err(e) => {
            eprintln!("zz: [watch] gate failed to run `zz check`: {e}");
            false
        }
    }
}

fn spawn_child(exe: &Path, child_argv: &[String], generation: u64) -> Result<Child, String> {
    Command::new(exe)
        .args(child_argv)
        .env("ZZ_WATCH_GEN", generation.to_string())
        .stdin(std::process::Stdio::inherit())
        .stdout(std::process::Stdio::inherit())
        .stderr(std::process::Stdio::inherit())
        .spawn()
        .map_err(|e| format!("cannot spawn `zz run` child: {e}"))
}

/// Stop a generation: SIGTERM (unix) for a graceful drain, then SIGKILL
/// after the grace period. Windows has no SIGTERM — `kill` directly.
fn stop_child(child: &mut Child) {
    #[cfg(unix)]
    {
        // Best effort: a missing/failed SIGTERM still falls through to
        // the timed wait + kill below.
        unsafe {
            libc::kill(child.id() as libc::pid_t, libc::SIGTERM);
        }
    }
    #[cfg(not(unix))]
    {
        let _ = child.kill();
    }
    let deadline = Instant::now() + Duration::from_millis(GRACE_MS);
    loop {
        match child.try_wait() {
            Ok(Some(_)) => return,
            Ok(None) => {
                if Instant::now() >= deadline {
                    break;
                }
                std::thread::sleep(Duration::from_millis(50));
            }
            Err(_) => return,
        }
    }
    let _ = child.kill();
    let _ = child.wait();
}

fn clear_screen() {
    // Reset + erase scrollback (widely supported; harmless elsewhere).
    print!("\x1Bc\x1B[3J");
    let _ = std::io::Write::flush(&mut std::io::stdout());
}

/// Run the watch loop. `entry` is the resolved run entry, `child_argv`
/// starts with `"run"`, `embed` (when set) is additionally watched.
pub fn run_watch(
    entry: String,
    child_argv: Vec<String>,
    embed: Option<PathBuf>,
    flags: WatchFlags,
) -> Result<(), String> {
    let exe = std::env::current_exe().map_err(|e| format!("cannot locate `zz` binary: {e}"))?;
    let entry_abs =
        std::path::absolute(&entry).map_err(|e| format!("cannot resolve entry `{entry}`: {e}"))?;
    if !entry_abs.is_file() {
        return Err(format!(
            "entry `{entry}` does not exist\n\nhint: `zz run --watch` needs a file to watch"
        ));
    }
    // Watch root: owning project when the entry lives in one, else the
    // entry's own directory (standalone scripts).
    let scan_root: PathBuf =
        match crate::loader::find_project_root(entry_abs.parent().unwrap_or(Path::new("."))) {
            Some(root) => root,
            None => entry_abs
                .parent()
                .map(|p| p.to_path_buf())
                .unwrap_or_else(|| PathBuf::from(".")),
        };
    let scan_embed = embed
        .as_ref()
        .map(|e| std::path::absolute(e).map_err(|err| format!("cannot resolve --embed dir: {err}")))
        .transpose()?;

    // Termination flags: SIGINT + SIGTERM set, the loop observes.
    let term = Arc::new(AtomicBool::new(false));
    if signal_hook::flag::register(signal_hook::consts::SIGINT, Arc::clone(&term)).is_err() {
        return Err(
            "cannot install SIGINT handler: refusing to watch without clean shutdown".to_string(),
        );
    }
    if signal_hook::flag::register(signal_hook::consts::SIGTERM, Arc::clone(&term)).is_err() {
        eprintln!("zz: [watch] warning: no SIGTERM handler — Ctrl+C still stops cleanly");
    }

    let scan = || {
        let mut snap = HashMap::new();
        snapshot_dir(&scan_root, false, &flags.ignores, &mut snap);
        if let Some(dir) = &scan_embed {
            snapshot_dir(dir, true, &flags.ignores, &mut snap);
        }
        snap
    };

    // Boot gate: broken code never starts a server (same as plain `run`).
    if !check_gate(&exe, &entry) {
        return Err("fix the errors above, then restart `zz run --watch`".to_string());
    }
    let mut snapshot = scan();
    let mut generation = 1u64;
    if flags.clear {
        clear_screen();
    }
    let mut child = Some(spawn_child(&exe, &child_argv, generation)?);
    eprintln!(
        "zz: [watch] generation {generation} running — watching {} files under {} (Ctrl+C to stop)",
        snapshot.len(),
        scan_root.display()
    );

    loop {
        std::thread::sleep(Duration::from_millis(TICK_MS));
        if term.load(Ordering::Relaxed) {
            if let Some(mut c) = child.take() {
                stop_child(&mut c);
            }
            eprintln!("zz: [watch] stopped");
            return Ok(());
        }
        // A generation that exits on its own (crash or plain program end)
        // is reported, not respawned: the next save restarts it. This
        // cannot spin on a crash loop.
        if let Some(c) = child.as_mut() {
            match c.try_wait() {
                Ok(Some(status)) => {
                    eprintln!("zz: [watch] process exited ({status}) — waiting for changes");
                    child = None;
                }
                Ok(None) => {}
                Err(e) => {
                    eprintln!("zz: [watch] wait failed ({e}) — waiting for changes");
                    child = None;
                }
            }
        }
        let current = scan();
        if diff_snapshots(&snapshot, &current).is_empty() {
            continue;
        }
        // Debounce: restart only after `debounce_ms` of quiet.
        let mut latest = current;
        let mut quiet_since = Instant::now();
        loop {
            std::thread::sleep(Duration::from_millis(TICK_MS));
            if term.load(Ordering::Relaxed) {
                if let Some(mut c) = child.take() {
                    stop_child(&mut c);
                }
                eprintln!("zz: [watch] stopped");
                return Ok(());
            }
            let rescan = scan();
            if !diff_snapshots(&latest, &rescan).is_empty() {
                latest = rescan;
                quiet_since = Instant::now();
                continue;
            }
            if quiet_since.elapsed() >= Duration::from_millis(flags.debounce_ms) {
                break;
            }
        }
        if !entry_abs.is_file() {
            if let Some(mut c) = child.take() {
                stop_child(&mut c);
            }
            return Err(format!(
                "entry `{entry}` was deleted — stopping the watcher"
            ));
        }
        eprintln!("zz: [watch] change detected — rechecking...");
        if !check_gate(&exe, &entry) {
            eprintln!("zz: [watch] check failed — keeping previous generation running");
            snapshot = scan();
            continue;
        }
        if let Some(mut c) = child.take() {
            stop_child(&mut c);
        }
        generation += 1;
        if flags.clear {
            clear_screen();
        }
        child = Some(spawn_child(&exe, &child_argv, generation)?);
        // Re-scan after spawning: edits made during check/gate must not
        // retrigger instantly, but must not be swallowed either — anything
        // newer than this scan fires the next tick.
        snapshot = scan();
        eprintln!("zz: [watch] restarted (generation {generation})");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn glob_star_matches_everything_including_slash() {
        assert!(match_glob("*", "src/main.zz"));
        assert!(match_glob("*.zz", "src/main.zz"));
        assert!(match_glob("src/*", "src/a/b.zz"));
        assert!(!match_glob("src/*.zz", "other/main.zz"));
    }

    #[test]
    fn glob_question_matches_single_char() {
        assert!(match_glob("?.zz", "a.zz"));
        assert!(!match_glob("?.zz", "ab.zz"));
    }

    #[test]
    fn glob_literal_and_empty() {
        assert!(match_glob("main.zz", "main.zz"));
        assert!(!match_glob("main.zz", "main2.zz"));
        assert!(match_glob("", ""));
        assert!(!match_glob("", "x"));
        assert!(!match_glob("*.zz", "main.rs"));
    }

    #[test]
    fn ignores_match_name_or_path() {
        let ignores = vec!["*.gen.zz".to_string(), "scratch".to_string()];
        assert!(is_ignored(Path::new("src/foo.gen.zz"), &ignores));
        assert!(is_ignored(Path::new("scratch"), &ignores));
        assert!(!is_ignored(Path::new("src/main.zz"), &ignores));
    }

    #[test]
    fn parse_strips_watch_flags_preserving_order() {
        let args = vec![
            "server.zz",
            "--watch",
            "--clear",
            "--debounce",
            "200",
            "--watch-ignore",
            "*.gen.zz",
            "--",
            "serve",
        ]
        .into_iter()
        .map(str::to_string)
        .collect::<Vec<_>>();
        let (flags, child) = parse_watch_flags(&args).unwrap();
        assert!(flags.clear);
        assert_eq!(flags.debounce_ms, 200);
        assert_eq!(flags.ignores, vec!["*.gen.zz".to_string()]);
        assert_eq!(
            child,
            vec![
                "server.zz".to_string(),
                "--".to_string(),
                "serve".to_string()
            ]
        );
    }

    #[test]
    fn parse_equals_forms_and_defaults() {
        let args = vec![
            "--watch".to_string(),
            "--debounce=50".to_string(),
            "--watch-ignore=a".to_string(),
            "--watch-ignore=b".to_string(),
        ];
        let (flags, child) = parse_watch_flags(&args).unwrap();
        assert_eq!(flags.debounce_ms, 50);
        assert_eq!(flags.ignores, vec!["a".to_string(), "b".to_string()]);
        assert!(child.is_empty());
        assert!(!flags.clear);
    }

    #[test]
    fn parse_bad_debounce_errors() {
        let args = vec![
            "--watch".to_string(),
            "--debounce".to_string(),
            "fast".to_string(),
        ];
        assert!(parse_watch_flags(&args).is_err());
        let args = vec!["--watch".to_string(), "--debounce".to_string()];
        assert!(parse_watch_flags(&args).is_err());
    }

    #[test]
    fn diff_detects_add_remove_modify() {
        use std::collections::HashMap;
        let a = PathBuf::from("a.zz");
        let b = PathBuf::from("b.zz");
        let s1 = |t: u64| FileSig {
            mtime_secs: t,
            mtime_nanos: 0,
            len: 10,
        };
        let mut old = HashMap::new();
        old.insert(a.clone(), s1(1));
        old.insert(b.clone(), s1(1));
        let mut new = HashMap::new();
        new.insert(a.clone(), s1(2));
        new.insert(PathBuf::from("c.zz"), s1(1));
        let diff = diff_snapshots(&old, &new);
        assert_eq!(diff, vec![a, b, PathBuf::from("c.zz")]);
    }

    #[test]
    fn snapshot_skips_ignored_and_vendor() {
        let dir = std::env::temp_dir().join(format!(
            "zz_watch_test_{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("vendor")).unwrap();
        std::fs::create_dir_all(dir.join("src")).unwrap();
        std::fs::write(dir.join("src/main.zz"), "x").unwrap();
        std::fs::write(dir.join("vendor/dep.zz"), "x").unwrap();
        std::fs::write(dir.join("notes.txt"), "x").unwrap();
        std::fs::write(dir.join("zz.toml"), "x").unwrap();
        let mut snap = HashMap::new();
        snapshot_dir(&dir, false, &["notes*".to_string()], &mut snap);
        assert!(snap.contains_key(&dir.join("src/main.zz")));
        assert!(snap.contains_key(&dir.join("zz.toml")));
        assert!(!snap.contains_key(&dir.join("vendor/dep.zz")));
        assert!(!snap.contains_key(&dir.join("notes.txt")));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
