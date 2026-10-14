//! `zz cover` — line coverage for VM program runs.
//!
//! Executes a `.zz` entry file exactly like `zz run` (same prepare +
//! module loop + `main()` auto-call) with the interpreter's coverage
//! recorder enabled, then reports per-line hits. Unattributed code
//! (stdlib, `.zzc` loads) is excluded by construction: only chunks the
//! CLI labels per module are recorded.
//!
//! v1 scope: single program runs. Test-suite coverage (`--tests`) is a
//! follow-up built on [`CoverageState::merge`](zz_runtime::CoverageState::merge),
//! which is already implemented and unit-tested. AOT has no coverage;
//! use `llvm-cov` on the generated C for native binaries.

use zz_runtime::Value;

/// Run `zz cover` on `args`. See module docs for scope.
pub fn cover_command(args: &[String]) -> Result<(), String> {
    let mut file: Option<String> = None;
    let mut script_args: Vec<String> = Vec::new();
    let mut lcov_path: Option<String> = None;
    let mut fail_under: Option<f64> = None;
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
        } else if arg == "--fail-under" {
            i += 1;
            let val = args.get(i).cloned().ok_or_else(|| {
                "missing value for --fail-under\n\nhint: use `zz cover --help`".to_string()
            })?;
            fail_under = Some(parse_percent(&val)?);
        } else if let Some(val) = arg.strip_prefix("--fail-under=") {
            fail_under = Some(parse_percent(val)?);
        } else if arg == "--help" || arg == "-h" {
            return Err(
                "usage: zz cover [<file.zz>] [--lcov <path>] [--fail-under <n>] [-- args...]\n\n\
                 Run a program exactly like `zz run` while recording line\n\
                 coverage, then report per-file hits. Unattributed code\n\
                 (stdlib, cached bytecode) is excluded.\n\n\
                 FLAGS:\n  \
                   --lcov <path>    Write an LCOV report for CI/editors\n  \
                   --fail-under <n> Exit 1 when total line coverage is below n%\n  \
                   -h, --help       Show this help\n\n\
                 Without a file, uses the project entry like `zz run`."
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

    let report = render_report(&prep);
    println!("{report}");
    if let Some(path) = lcov_path {
        let lcov = render_lcov(&prep);
        std::fs::write(&path, lcov)
            .map_err(|e| format!("cannot write LCOV report `{path}`: {e}"))?;
    }

    if let Some(e) = run_error {
        return Err(e);
    }
    if let Some(threshold) = fail_under {
        let pct = total_percent(&prep);
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

fn parse_percent(val: &str) -> Result<f64, String> {
    match val.parse::<f64>() {
        Ok(n) if (0.0..=100.0).contains(&n) => Ok(n),
        _ => Err(format!(
            "invalid --fail-under `{val}` (expected 0-100)\n\nhint: use `zz cover --help`"
        )),
    }
}

fn total_percent(prep: &crate::PreparedRun) -> f64 {
    let cov = match prep.interp.coverage.as_ref() {
        Some(c) => c,
        None => return 0.0,
    };
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

fn render_report(prep: &crate::PreparedRun) -> String {
    let Some(cov) = prep.interp.coverage.as_ref() else {
        return "Coverage: no data (recorder disabled)".to_string();
    };
    if cov.files.is_empty() {
        return "Coverage: no coverable lines".to_string();
    }
    let mut labels: Vec<usize> = (0..cov.files.len()).collect();
    labels.sort_by(|a, b| cov.files[*a].label.cmp(&cov.files[*b].label));
    let mut total_covered = 0usize;
    let mut total_coverable = 0usize;
    let mut out = String::new();
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
        let uncovered = cov.uncovered_lines(i);
        if uncovered.is_empty() {
            files_out.push('\n');
        } else {
            files_out.push_str(&format!(
                "\n    uncovered: {}\n",
                compact_ranges(&uncovered)
            ));
        }
    }
    let total = if total_coverable == 0 {
        100.0
    } else {
        total_covered as f64 / total_coverable as f64 * 100.0
    };
    out.push_str(&format!(
        "Coverage: {total:.1}% lines ({total_covered}/{total_coverable})\n{files_out}"
    ));
    out.trim_end().to_string()
}

fn render_lcov(prep: &crate::PreparedRun) -> String {
    let mut out = String::from("TN:\n");
    let Some(cov) = prep.interp.coverage.as_ref() else {
        return out;
    };
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
        let (covered, coverable) = cov.file_stats(i);
        out.push_str(&format!("LF:{coverable}\nLH:{covered}\nend_of_record\n"));
    }
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
