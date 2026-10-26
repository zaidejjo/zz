//! `zz cover` — line + branch coverage for VM runs.
//!
//! Two modes (VM only; AOT has no coverage — use `llvm-cov` on native
//! binaries, see `docs/coverage.md`):
//!
//! - `zz cover <file>`: executes an entry file exactly like `zz run`
//!   with the recorder enabled, then reports per-line hits.
//! - `zz cover --tests [path]`: discovers every test (same default as
//!   `zz test`) and runs each with coverage, merging all states.
//!
//! Unattributed code (stdlib, `.zzc` loads) is excluded by construction:
//! only chunks the CLI labels per module are recorded.

use zz_runtime::Value;

/// Run `zz cover` on `args`. See module docs for scope.
pub fn cover_command(args: &[String]) -> Result<(), String> {
    let mut file: Option<String> = None;
    let mut script_args: Vec<String> = Vec::new();
    let mut lcov_path: Option<String> = None;
    let mut html_path: Option<String> = None;
    let mut fail_under: Option<f64> = None;
    let mut tests_path: Option<Option<String>> = None;
    let mut script_mode = false;

    let mut i = 0;
    while i < args.len() {
        let arg = args[i].as_str();
        if script_mode {
            script_args.push(args[i].clone());
        } else if arg == "--" {
            script_mode = true;
        } else if arg == "--lcov" {
            i += 1;
            lcov_path = args.get(i).cloned();
            if lcov_path.is_none() {
                return Err("missing value for --lcov\n\nhint: use `zz cover --help`".to_string());
            }
        } else if let Some(val) = arg.strip_prefix("--lcov=") {
            lcov_path = Some(val.to_string());
        } else if arg == "--html" {
            i += 1;
            html_path = args.get(i).cloned();
            if html_path.is_none() {
                return Err("missing value for --html\n\nhint: use `zz cover --help`".to_string());
            }
        } else if let Some(val) = arg.strip_prefix("--html=") {
            html_path = Some(val.to_string());
        } else if arg == "--fail-under" {
            i += 1;
            let val = args.get(i).cloned().ok_or_else(|| {
                "missing value for --fail-under\n\nhint: use `zz cover --help`".to_string()
            })?;
            fail_under = Some(parse_percent(&val)?);
        } else if let Some(val) = arg.strip_prefix("--fail-under=") {
            fail_under = Some(parse_percent(val)?);
        } else if arg == "--tests" {
            // Optional path: next arg unless it looks like a flag.
            let path = match args.get(i + 1) {
                Some(next) if !next.starts_with('-') => {
                    i += 1;
                    Some(args[i].clone())
                }
                _ => None,
            };
            tests_path = Some(path);
        } else if let Some(val) = arg.strip_prefix("--tests=") {
            tests_path = Some(Some(val.to_string()));
        } else if arg == "--help" || arg == "-h" {
            return Err(
                "usage: zz cover [<file.zz>] [--lcov <path>] [--html <path>] [--fail-under <n>] [-- args...]\n\
                 usage: zz cover --tests [path] [--lcov <path>] [--html <path>] [--fail-under <n>]\n\n\
                 Run a program exactly like `zz run` (or every test like\n\
                 `zz test` with --tests) while recording line + branch\n\
                 coverage, then report per-file hits. Unattributed code\n\
                 (stdlib, cached bytecode) is excluded. VM only: native\n\
                 binaries have no coverage (see `docs/coverage.md`).\n\n\
                 FLAGS:\n  \
                   --tests [path]   Cover the test suite instead of a program\n  \
                   --lcov <path>    Write an LCOV report for CI/editors\n  \
                   --html <path>    Write a standalone HTML report\n  \
                   --fail-under <n> Exit 1 when total line coverage is below n%\n  \
                   -h, --help       Show this help\n\n\
                 Without a file, uses the project entry like `zz run`; with\n\
                 bare --tests, the same default as `zz test`."
                    .to_string(),
            );
        } else if arg.starts_with('-') {
            return Err(format!(
                "unknown flag: {arg}\n\nhint: use `zz cover --help` for usage"
            ));
        } else if file.is_none() {
            file = Some(args[i].clone());
        } else {
            script_args.push(args[i].clone());
        }
        i += 1;
    }

    if tests_path.is_some() {
        if file.is_some() {
            return Err(
                "cannot combine a program file with --tests\n\nhint: use `zz cover --help`"
                    .to_string(),
            );
        }
        if !script_args.is_empty() {
            return Err(
                "script args make no sense with --tests\n\nhint: use `zz cover --help`".to_string(),
            );
        }
        return cover_tests_mode(tests_path.flatten(), lcov_path, html_path, fail_under);
    }

    // Same entry resolution as `zz run` (project entry when omitted).
    let resolved = crate::resolve_run_entry(
        file.as_ref(),
        "zz cover [<file.zz>]",
        "provide the path to a .zz file to measure",
    )?;
    // Full pipeline (never the run cache: cached IR carries no coverage
    // ids, and execution recompiles anyway).
    let mut prep = crate::prepare_run(&resolved, &script_args, None)?;
    prep.interp.enable_coverage();

    let mut run_error: Option<String> = None;
    let mut last = Value::Unit;
    for (i, program) in prep.programs.iter().enumerate() {
        let (name, source) = prep
            .files
            .get(i)
            .cloned()
            .unwrap_or_else(|| (prep.entry_path.clone(), String::new()));
        prep.interp.set_coverage_source(&name, &source);
        match prep.interp.run_typed(
            program,
            prep.types.clone(),
            prep.structs.clone(),
            prep.enums.clone(),
        ) {
            Ok(v) => last = v,
            Err(e) => {
                run_error = Some(crate::render_eval_error(&e, &name, &source));
                break;
            }
        }
    }
    // `main()` auto-call participates in coverage like any other call.
    if run_error.is_none() {
        if let Err(e) = crate::run_entry_main(
            &mut prep.interp,
            last,
            &prep.entry_path,
            &script_args,
            &prep.files,
        ) {
            run_error = Some(e);
        }
    }

    let cov = prep.interp.coverage.clone().unwrap_or_default();
    emit_reports(&cov, lcov_path.as_deref(), html_path.as_deref())?;

    if let Some(e) = run_error {
        return Err(e);
    }
    if let Some(threshold) = fail_under {
        let pct = total_percent(&cov);
        if pct < threshold {
            return Err(format!(
                "coverage {pct:.1}% below --fail-under {threshold:.1}%"
            ));
        }
    }
    // Keep program-output semantics identical to `zz run`: the report
    // below always prints, even when the program itself failed.
    Ok(())
}

/// `--tests` mode: cover the whole suite, merging per-test states.
/// Failing tests still contribute partial coverage; their names report
/// on stderr and flip the exit code.
fn cover_tests_mode(
    path: Option<String>,
    lcov_path: Option<String>,
    html_path: Option<String>,
    fail_under: Option<f64>,
) -> Result<(), String> {
    let (cov, failed) = crate::test_runner::cover_tests(path.as_deref())?;
    for name in &failed {
        eprintln!("cover: test failed (coverage kept): {name}");
    }
    emit_reports(&cov, lcov_path.as_deref(), html_path.as_deref())?;
    if !failed.is_empty() {
        return Err(format!("{} test(s) failed during coverage", failed.len()));
    }
    if let Some(threshold) = fail_under {
        let pct = total_percent(&cov);
        if pct < threshold {
            return Err(format!(
                "coverage {pct:.1}% below --fail-under {threshold:.1}%"
            ));
        }
    }
    Ok(())
}

fn emit_reports(
    cov: &zz_runtime::CoverageState,
    lcov_path: Option<&str>,
    html_path: Option<&str>,
) -> Result<(), String> {
    println!("{}", render_report(cov));
    if let Some(path) = lcov_path {
        std::fs::write(path, render_lcov(cov))
            .map_err(|e| format!("cannot write LCOV report `{path}`: {e}"))?;
    }
    if let Some(path) = html_path {
        std::fs::write(path, render_html(cov))
            .map_err(|e| format!("cannot write HTML report `{path}`: {e}"))?;
    }
    Ok(())
}

fn parse_percent(val: &str) -> Result<f64, String> {
    match val.parse::<f64>() {
        Ok(n) if (0.0..=100.0).contains(&n) => Ok(n),
        _ => Err(format!(
            "invalid --fail-under `{val}` (expected 0-100)\n\nhint: use `zz cover --help`"
        )),
    }
}

fn total_percent(cov: &zz_runtime::CoverageState) -> f64 {
    let mut covered = 0usize;
    let mut coverable = 0usize;
    for i in 0..cov.files.len() {
        let (c, t) = cov.file_stats(i);
        covered += c;
        coverable += t;
    }
    if coverable == 0 {
        return 100.0;
    }
    covered as f64 / coverable as f64 * 100.0
}

/// (directions hit, directions total) for one file's branch sites.
fn file_branch_stats(cov: &zz_runtime::CoverageState, file_idx: usize) -> (usize, usize) {
    let mut hit = 0usize;
    let mut total = 0usize;
    for (si, site) in cov.branch_sites.iter().enumerate() {
        if site.file_idx != file_idx {
            continue;
        }
        total += 2;
        let (taken, fallthrough) = cov.branch_hits[si];
        if taken > 0 {
            hit += 1;
        }
        if fallthrough > 0 {
            hit += 1;
        }
    }
    (hit, total)
}

/// Uncovered branch directions as `line->taken` (jump never taken) and
/// `line` + fallthrough marker. 1-based lines.
fn uncovered_branches(cov: &zz_runtime::CoverageState, file_idx: usize) -> Vec<String> {
    let mut out = Vec::new();
    for (si, site) in cov.branch_sites.iter().enumerate() {
        if site.file_idx != file_idx {
            continue;
        }
        let (taken, fallthrough) = cov.branch_hits[si];
        let line = site.line + 1;
        if taken == 0 {
            match site.taken_line {
                Some(t) => out.push(format!("{line}->{}", t + 1)),
                None => out.push(format!("{line}->?")),
            }
        }
        if fallthrough == 0 {
            out.push(format!("{line} (fallthrough)"));
        }
    }
    out
}

/// Compact 1-based line ranges: [11, 14, 15, 16, 20] → "12, 15-17, 21".
fn compact_ranges(lines_0based: &[u32]) -> String {
    let mut out = Vec::new();
    let mut iter = lines_0based.iter().peekable();
    while let Some(&start) = iter.next() {
        let mut end = start;
        while iter.peek() == Some(&&(end + 1)) {
            end += 1;
            iter.next();
        }
        if end == start {
            out.push(format!("{}", start + 1));
        } else {
            out.push(format!("{}-{}", start + 1, end + 1));
        }
    }
    out.join(", ")
}

fn render_report(cov: &zz_runtime::CoverageState) -> String {
    if cov.files.is_empty() {
        return "Coverage: no coverable lines".to_string();
    }
    let mut labels: Vec<usize> = (0..cov.files.len()).collect();
    labels.sort_by(|a, b| cov.files[*a].label.cmp(&cov.files[*b].label));
    let mut total_covered = 0usize;
    let mut total_coverable = 0usize;
    let mut total_bhit = 0usize;
    let mut total_btotal = 0usize;
    let mut files_out = String::new();
    for i in labels {
        let (covered, coverable) = cov.file_stats(i);
        total_covered += covered;
        total_coverable += coverable;
        let pct = if coverable == 0 {
            100.0
        } else {
            covered as f64 / coverable as f64 * 100.0
        };
        files_out.push_str(&format!(
            "  {}: {pct:.1}% ({covered}/{coverable})",
            cov.files[i].label
        ));
        let (bhit, btotal) = file_branch_stats(cov, i);
        total_bhit += bhit;
        total_btotal += btotal;
        if btotal > 0 {
            let bpct = bhit as f64 / btotal as f64 * 100.0;
            files_out.push_str(&format!(", branches {bpct:.1}% ({bhit}/{btotal})"));
        }
        files_out.push('\n');
        let uncovered = cov.uncovered_lines(i);
        if !uncovered.is_empty() {
            files_out.push_str(&format!("    uncovered: {}\n", compact_ranges(&uncovered)));
        }
        let ubranches = uncovered_branches(cov, i);
        if !ubranches.is_empty() {
            files_out.push_str(&format!(
                "    uncovered branches: {}\n",
                ubranches.join(", ")
            ));
        }
    }
    let total = if total_coverable == 0 {
        100.0
    } else {
        total_covered as f64 / total_coverable as f64 * 100.0
    };
    let mut out = format!("Coverage: {total:.1}% lines ({total_covered}/{total_coverable})");
    if total_btotal > 0 {
        let bpct = total_bhit as f64 / total_btotal as f64 * 100.0;
        out.push_str(&format!(
            ", {bpct:.1}% branches ({total_bhit}/{total_btotal})"
        ));
    }
    out.push_str(&format!("\n{files_out}"));
    out.trim_end().to_string()
}

fn render_lcov(cov: &zz_runtime::CoverageState) -> String {
    let mut out = String::from("TN:\n");
    let mut labels: Vec<usize> = (0..cov.files.len()).collect();
    labels.sort_by(|a, b| cov.files[*a].label.cmp(&cov.files[*b].label));
    for i in labels {
        out.push_str(&format!("SF:{}\n", cov.files[i].label));
        // Per-line hits: sum over ids mapping to the line.
        let mut per_line: std::collections::BTreeMap<u32, u64> = Default::default();
        for (id, &(fi, line)) in cov.id_loc.iter().enumerate() {
            if fi != i {
                continue;
            }
            *per_line.entry(line).or_insert(0) += cov.hits[id];
        }
        for (line, hits) in &per_line {
            out.push_str(&format!("DA:{},{hits}\n", line + 1));
        }
        // Branch records: block = site ordinal within the file,
        // branch 0 = jump taken, 1 = fallthrough (`-` when never hit).
        let mut block = 0u32;
        let mut branches_found = 0usize;
        let mut branches_hit = 0usize;
        for (si, site) in cov.branch_sites.iter().enumerate() {
            if site.file_idx != i {
                continue;
            }
            let (taken, fallthrough) = cov.branch_hits[si];
            branches_found += 2;
            let t = if taken > 0 {
                branches_hit += 1;
                taken.to_string()
            } else {
                "-".to_string()
            };
            let f = if fallthrough > 0 {
                branches_hit += 1;
                fallthrough.to_string()
            } else {
                "-".to_string()
            };
            out.push_str(&format!("BRDA:{},{},0,{t}\n", site.line + 1, block));
            out.push_str(&format!("BRDA:{},{},1,{f}\n", site.line + 1, block));
            block += 1;
        }
        let (covered, coverable) = cov.file_stats(i);
        out.push_str(&format!("LF:{coverable}\nLH:{covered}\n"));
        if branches_found > 0 {
            out.push_str(&format!("BRF:{branches_found}\nBRH:{branches_hit}\n"));
        }
        out.push_str("end_of_record\n");
    }
    out
}

fn html_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            c => out.push(c),
        }
    }
    out
}

fn render_html(cov: &zz_runtime::CoverageState) -> String {
    let mut out = String::from(
        "<!DOCTYPE html><html><head><meta charset=\"utf-8\">\
         <title>zz cover</title><style>\
         body{font-family:monospace;max-width:72em;margin:2em auto;padding:0 1em}\
         .cov{color:#137333}.unc{color:#a50e0e;background:#fce8e6}\
         .neu{color:#666}table{border-collapse:collapse}\
         td,th{border:1px solid #ccc;padding:.1em .5em;text-align:left}\
         pre{line-height:1.3}</style></head><body><h1>zz cover</h1>\n",
    );
    out.push_str("<table><tr><th>file</th><th>lines</th><th>branches</th></tr>\n");
    let mut labels: Vec<usize> = (0..cov.files.len()).collect();
    labels.sort_by(|a, b| cov.files[*a].label.cmp(&cov.files[*b].label));
    let mut total_covered = 0usize;
    let mut total_coverable = 0usize;
    for i in &labels {
        let (covered, coverable) = cov.file_stats(*i);
        total_covered += covered;
        total_coverable += coverable;
        let pct = if coverable == 0 {
            100.0
        } else {
            covered as f64 / coverable as f64 * 100.0
        };
        let (bhit, btotal) = file_branch_stats(cov, *i);
        let bcell = if btotal == 0 {
            "-".to_string()
        } else {
            format!(
                "{:.1}% ({bhit}/{btotal})",
                bhit as f64 / btotal as f64 * 100.0
            )
        };
        out.push_str(&format!(
            "<tr><td>{}</td><td>{pct:.1}% ({covered}/{coverable})</td><td>{bcell}</td></tr>\n",
            html_escape(&cov.files[*i].label)
        ));
    }
    let total = if total_coverable == 0 {
        100.0
    } else {
        total_covered as f64 / total_coverable as f64 * 100.0
    };
    out.push_str(&format!(
        "</table><p>Total: {total:.1}% lines ({total_covered}/{total_coverable})</p>\n"
    ));
    for i in &labels {
        let src = &cov.files[*i].source;
        let covered: std::collections::HashSet<u32> = cov
            .id_loc
            .iter()
            .enumerate()
            .filter(|(id, (fi, _))| *fi == *i && cov.hits[*id] > 0)
            .map(|(_, (_, line))| *line)
            .collect();
        let coverable: std::collections::HashSet<u32> =
            cov.files[*i].coverable.iter().copied().collect();
        out.push_str(&format!(
            "<h2>{}</h2><pre>\n",
            html_escape(&cov.files[*i].label)
        ));
        for (n, line) in src.split('\n').enumerate() {
            let line_no = n as u32;
            let cls = if coverable.contains(&line_no) {
                if covered.contains(&line_no) {
                    "cov"
                } else {
                    "unc"
                }
            } else {
                "neu"
            };
            out.push_str(&format!(
                "<span class=\"{cls}\">{:>4} {}</span>\n",
                line_no + 1,
                html_escape(line)
            ));
        }
        out.push_str("</pre>\n");
    }
    out.push_str("</body></html>\n");
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ranges_compact() {
        assert_eq!(compact_ranges(&[]), "");
        assert_eq!(compact_ranges(&[11]), "12");
        assert_eq!(compact_ranges(&[11, 14, 15, 16, 20]), "12, 15-17, 21");
    }

    #[test]
    fn percent_rejects_out_of_range() {
        assert!(parse_percent("80").is_ok());
        assert!(parse_percent("0").is_ok());
        assert!(parse_percent("100").is_ok());
        assert!(parse_percent("-1").is_err());
        assert!(parse_percent("101").is_err());
        assert!(parse_percent("x").is_err());
    }
}
