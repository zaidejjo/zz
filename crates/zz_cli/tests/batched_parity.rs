//! Batched parity: one native build per K strict fixtures.
//!
//! The individual `parity_strict!` tests each pay a cold clang compile
//! for their native leg (~5s). This target groups batchable strict
//! fixtures into import-drivers (20 per build): each fixture is imported
//! as a module (body runs at import, isolated namespace — no renames),
//! `pub`-injected mains are called explicitly, and per-case output is
//! demuxed on markers and compared VM-vs-native with the same
//! normalization as `tests/fuzz/run.sh`.
//!
//! Modes (`ZZ_BATCH_NATIVE=1` = full coverage):
//! - unset (local default): a 3-fixture smoke subset validates the
//!   machinery fast; per-fixture tests keep their native legs.
//! - set (CI): all batchable fixtures; pair with `ZZ_PARITY_VM_ONLY=1`
//!   so individuals assert VM legs while this target owns natives.
//!
//! Batchable = strict-success outside `regression/`+`errors/` (the quad
//! owns those), minus stdin/port/spawn/env-argv fixtures (explicit
//! `EXCLUDED` with reasons; `batch_inventory_complete` fails if any
//! strict fixture is neither batched nor excluded).

use std::path::{Path, PathBuf};
use std::process::Command;

fn fixtures_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures")
}

fn zz_bin() -> PathBuf {
    // Same layout as the other harness binaries: the test profile's
    // own `zz` (debug), so VM legs match `cargo test` exactly.
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../target/debug");
    let exe = format!("zz{}", std::env::consts::EXE_SUFFIX);
    dir.join(&exe)
}

fn vm_only() -> bool {
    // Mirrors dual_engine_parity: native backend unsupported here.
    std::env::var("ZZ_SKIP_NATIVE").is_ok() || std::env::var("ZZ_PARITY_VM_ONLY").is_ok()
}

fn batch_full() -> bool {
    std::env::var("ZZ_BATCH_NATIVE").is_ok_and(|v| v == "1")
}

#[path = "batch_lists.rs"]
mod batch_lists;
use batch_lists::{ELIGIBLE, EXCLUDED};

/// Every strict fixture outside the quad's scope must be batched or
/// explicitly excluded — coverage cannot silently drift.
#[test]
fn batch_inventory_complete() {
    let src = std::fs::read_to_string(
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/dual_engine_parity.rs"),
    )
    .expect("read dual_engine_parity.rs");
    // Minimal scanner for `parity_strict!(name, "cat", "file")` in both
    // multi-line and single-line form.
    let mut triples: Vec<(String, String)> = Vec::new();
    let lines: Vec<&str> = src.lines().collect();
    fn quoted_in(line: &str) -> Vec<String> {
        let mut out = Vec::new();
        let mut rest = line;
        while let Some(a) = rest.find('"') {
            let after = &rest[a + 1..];
            if let Some(b) = after.find('"') {
                out.push(after[..b].to_string());
                rest = &after[b + 1..];
            } else {
                break;
            }
        }
        out
    }
    let mut i = 0;
    while i < lines.len() {
        if lines[i].contains("parity_strict!(") {
            let mut quoted = quoted_in(lines[i]);
            if quoted.len() < 2 {
                for l in lines.iter().skip(i + 1).take(4) {
                    quoted.extend(quoted_in(l));
                    if quoted.len() >= 2 {
                        break;
                    }
                }
            }
            assert!(
                quoted.len() == 2,
                "cannot parse parity_strict! at line {}",
                i + 1
            );
            triples.push((quoted[0].clone(), quoted[1].clone()));
        }
        i += 1;
    }
    assert!(!triples.is_empty(), "no parity_strict! found");
    let mut missing: Vec<String> = Vec::new();
    for (cat, file) in &triples {
        if cat == "regression" || cat == "errors" {
            continue; // quad scope
        }
        let key = format!("{cat}/{file}");
        let batched = ELIGIBLE.iter().any(|(c, f, _)| format!("{c}/{f}") == key);
        let excluded = EXCLUDED.iter().any(|(kf, _)| *kf == key);
        if !batched && !excluded {
            missing.push(key);
        }
    }
    assert!(
        missing.is_empty(),
        "strict fixtures neither batched nor excluded: {missing:?}"
    );
    // has_main flags must match `^func main` in the fixture source.
    let fixtures = fixtures_dir();
    for (cat, file, has_main) in ELIGIBLE {
        let src =
            std::fs::read_to_string(fixtures.join(cat).join(file)).expect("read eligible fixture");
        let actual = src.lines().any(|l| l.starts_with("func main"));
        assert_eq!(*has_main, actual, "stale has_main flag for {cat}/{file}");
    }
}

/// Flat sandbox layout: each batched fixture is copied to
/// `case_<global-index>.zz` (imported as `case_<i>`). Flat names dodge
/// keyword stems (`syntax.match` is unimportable) and cross-category
/// stem collisions; per-module namespaces still isolate bindings.
/// Eligible fixtures use only `std.*` imports, so no helper dirs need
/// copying. `pub`-injects `func main` when `has_main` (mains are
/// uniformly `func main() ...`, inventory-verified).
fn stage_case(
    fixtures: &Path,
    sandbox: &Path,
    idx: usize,
    cat: &str,
    file: &str,
    has_main: bool,
) -> String {
    let modname = format!("case_{idx}");
    let src = std::fs::read_to_string(fixtures.join(cat).join(file)).expect("read fixture");
    let mut count = 0;
    let out: Vec<String> = src
        .lines()
        .map(|l| {
            if has_main && l.starts_with("func main") {
                count += 1;
                format!("pub {l}")
            } else {
                l.to_string()
            }
        })
        .collect();
    if has_main {
        assert_eq!(count, 1, "expected exactly one main in {cat}/{file}");
    }
    std::fs::write(sandbox.join(format!("{modname}.zz")), out.join("\n") + "\n")
        .expect("write case copy");
    modname
}

fn norm(out: &str) -> Vec<String> {
    // Same normalization as tests/fuzz/run.sh: strip purely-numeric
    // lines (ASCII digits only — negative numbers are COMPARED, exactly
    // like run.sh's `grep -v '^[0-9][0-9]*$'`), trim trailing whitespace.
    out.lines()
        .map(|l| l.trim_end().to_string())
        .filter(|l| {
            let t = l.trim();
            t.is_empty() || !t.bytes().all(|b| b.is_ascii_digit())
        })
        .collect()
}

fn norm_err(out: &str) -> Vec<String> {
    // run.sh norm_err: sorted stderr (warning order varies by engine).
    let mut v: Vec<String> = out.lines().map(|l| l.trim_end().to_string()).collect();
    v.sort();
    v
}

fn demux(out: &str) -> std::collections::HashMap<String, Vec<String>> {
    let mut map = std::collections::HashMap::new();
    let mut cur: Option<String> = None;
    for line in out.lines() {
        if let Some(rest) = line.strip_prefix("ZZBEGIN ") {
            cur = Some(rest.to_string());
            map.insert(rest.to_string(), Vec::new());
        } else if let Some(rest) = line.strip_prefix("ZZEND ") {
            assert_eq!(cur.as_deref(), Some(rest), "unbalanced ZZEND {rest}");
            cur = None;
        } else if let Some(k) = &cur {
            map.get_mut(k).unwrap().push(line.to_string());
        }
    }
    assert!(cur.is_none(), "unclosed ZZBEGN chunk");
    map
}

fn run_driver(driver: &Path, native: bool) -> (i32, String, String) {
    let mut cmd = Command::new(zz_bin());
    if native {
        cmd.arg("run").arg("--native");
    } else {
        cmd.arg("run");
    }
    // Generous: one cold clang -O3-scale build per driver.
    // Matches run.sh's native timeout class, above the harness 300s.
    let timeout_secs = if native { 600 } else { 120 };
    cmd.arg(driver)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    // Native builds publish standalone outputs to the invocation dir:
    // run from the driver's own directory so parallel drivers never
    // share (and race on) one destination. The VM writes nothing.
    if native {
        if let Some(parent) = driver.parent() {
            cmd.current_dir(parent);
        }
    }
    let mut child = cmd.spawn().expect("spawn zz");
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(timeout_secs);
    loop {
        match child.try_wait().expect("wait") {
            Some(status) => {
                let out = child.wait_with_output().expect("output");
                return (
                    status.code().unwrap_or(124),
                    String::from_utf8_lossy(&out.stdout).into_owned(),
                    String::from_utf8_lossy(&out.stderr).into_owned(),
                );
            }
            None => {
                if std::time::Instant::now() > deadline {
                    child.kill().ok();
                    return (124, String::new(), "batch driver timeout".to_string());
                }
                std::thread::sleep(std::time::Duration::from_millis(50));
            }
        }
    }
}

const PER_BATCH: usize = 20;

#[test]
fn batched_native_parity() {
    let all = batch_full();
    let cases: Vec<(&str, &str, bool)> = if all {
        ELIGIBLE.to_vec()
    } else {
        // Smoke subset: machinery self-check without the full cost.
        ELIGIBLE.iter().take(3).copied().collect()
    };
    assert!(!cases.is_empty());
    let sandbox = std::env::temp_dir().join(format!(
        "zz_batch_{}_{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("clock")
            .as_nanos()
    ));
    let fixtures = fixtures_dir();
    std::fs::create_dir_all(&sandbox).expect("mkdir sandbox");
    // Stage every case flat (case_<global-idx>.zz); drivers import them
    // by index. Indexing is global (not per-chunk) so aliases can never
    // collide across drivers.
    let staged: Vec<(String, bool, String, String)> = cases
        .iter()
        .enumerate()
        .map(|(idx, (cat, file, has_main))| {
            let modname = stage_case(&fixtures, &sandbox, idx, cat, file, *has_main);
            (modname, *has_main, cat.to_string(), file.to_string())
        })
        .collect();
    // Merge every category's `support/` helpers into one sandbox dir so
    // flat-staged cases keep their local imports (`support.x` resolves
    // relative to the importing file, i.e. the sandbox root). Filenames
    // must not collide across categories (asserted).
    {
        let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
        let support_dst = sandbox.join("support");
        let mut categories: Vec<&str> = cases.iter().map(|(c, _, _)| *c).collect();
        categories.sort();
        categories.dedup();
        for cat in categories {
            let dir = fixtures.join(cat).join("support");
            let Ok(entries) = std::fs::read_dir(&dir) else {
                continue;
            };
            for entry in entries.flatten() {
                let from = entry.path();
                if from.extension().and_then(|s| s.to_str()) != Some("zz") {
                    continue;
                }
                let name = entry.file_name().to_string_lossy().into_owned();
                assert!(
                    seen.insert(name.clone()),
                    "support helper collision across categories: {name}"
                );
                std::fs::create_dir_all(&support_dst).expect("mkdir support");
                std::fs::copy(&from, support_dst.join(&name)).expect("copy support helper");
            }
        }
    }
    let mut failures: Vec<String> = Vec::new();
    let per_batch_idx: Vec<Vec<usize>> = cases
        .iter()
        .enumerate()
        .map(|(idx, _)| idx)
        .collect::<Vec<_>>()
        .chunks(PER_BATCH)
        .map(|c| c.to_vec())
        .collect();
    for (b, idxs) in per_batch_idx.iter().enumerate() {
        let driver = sandbox.join(format!("driver_{b}.zz"));
        let mut src = String::new();
        // All imports first (bodies run at import, in order), then one
        // blank line: an import body may end with `print` (no trailing
        // newline — e.g. syntax/destructuring), which would otherwise
        // glue onto the first ZZBEGIN marker and break demux prefix
        // matching. The blank line is marker-free so demux ignores it,
        // and norm keeps it identically on both engines.
        for (i, idx) in idxs.iter().enumerate() {
            let (modname, _, _, _) = &staged[*idx];
            let alias = format!("c{i}");
            src.push_str(&format!("import {modname} as {alias}\n"));
        }
        src.push_str("println(\"\")\n");
        for (i, idx) in idxs.iter().enumerate() {
            let (_, has_main, _, _) = &staged[*idx];
            let alias = format!("c{i}");
            src.push_str(&format!("println(\"ZZBEGIN {alias}\")\n"));
            if *has_main {
                src.push_str(&format!("{alias}.main()\n"));
            }
            src.push_str(&format!("println(\"ZZEND {alias}\")\n"));
        }
        src.push_str("println(\"ZZDONE\")\n");
        std::fs::write(&driver, &src).expect("write driver");
        let (vm_code, vm_out, vm_err) = run_driver(&driver, false);
        if vm_code != 0 {
            failures.push(format!("driver {b}: VM exit {vm_code}:\n{vm_err}"));
            continue;
        }
        if vm_only() {
            eprintln!("SKIP native (VM-only mode): driver {b}");
            continue;
        }
        let (nat_code, nat_out, nat_err) = run_driver(&driver, true);
        if nat_code != 0 {
            failures.push(format!("driver {b}: native exit {nat_code}:\n{nat_err}"));
            continue;
        }
        let vm_chunks = demux(&vm_out);
        let nat_chunks = demux(&nat_out);
        for (i, idx) in idxs.iter().enumerate() {
            let (modname, _, cat, file) = &staged[*idx];
            let alias = format!("c{i}");
            match (vm_chunks.get(&alias), nat_chunks.get(&alias)) {
                (Some(v), Some(n)) => {
                    if norm(&v.join("\n")) != norm(&n.join("\n")) {
                        failures.push(format!(
                            "driver {b} {cat}/{file} ({modname}): output differs:\nVM:\n{}\nnative:\n{}",
                            v.join("\n"),
                            n.join("\n")
                        ));
                    }
                }
                _ => failures.push(format!("driver {b} {cat}/{file}: lost marker")),
            }
        }
        if norm_err(&vm_err) != norm_err(&nat_err) {
            failures.push(format!(
                "driver {b}: stderr differs:\nVM stderr:\n{}\n---\nnative stderr:\n{}",
                norm_err(&vm_err).join("\n"),
                norm_err(&nat_err).join("\n")
            ));
        }
    }
    if failures.is_empty() {
        let _ = std::fs::remove_dir_all(&sandbox);
    } else {
        eprintln!("batch sandbox kept at {}", sandbox.display());
    }
    assert!(
        failures.is_empty(),
        "batched parity failures:\n{}",
        failures.join("\n---\n")
    );
}
