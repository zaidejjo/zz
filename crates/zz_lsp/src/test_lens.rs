//! Inline test running: CodeLens `Run` buttons above `@test` functions
//! plus the `zz.runTest` command that executes them.
//!
//! Discovery is a pure AST walk (unit-tested); execution shells out to
//! the `zz` binary sitting next to the running `zz-lsp` (release
//! layout), falling back to `PATH`. Results go to the editor as a
//! notification with the tail of the test output; full output goes to
//! the server log.

use tower_lsp::lsp_types::*;
use zz_frontend::ast::{Program, Stmt};

use crate::convert::LineIndex;

/// The command editors invoke from a test lens.
pub const RUN_TEST_COMMAND: &str = "zz.runTest";

/// Discover `@test` functions in `program`: one lens per test, placed on
/// the `@test` decorator line (or the func line when undecorated text
/// cannot be located).
pub fn test_lenses_for(program: &Program, source: &str, uri: &Url) -> Vec<CodeLens> {
    let line_index = LineIndex::new(source);
    let mut lenses = Vec::new();
    for stmt in &program.stmts {
        let Stmt::Func {
            name,
            decorators,
            span,
            ..
        } = stmt
        else {
            continue;
        };
        let is_test = decorators
            .iter()
            .any(zz_frontend::test_attr::is_test_decorator);
        if !is_test {
            continue;
        }
        let test_name = name.join(".");
        // Prefer the decorator's own line so the button sits on `@test`.
        let anchor = decorators.first().map(|d| d.span).unwrap_or(*span);
        let range = line_index.span_to_range(source, anchor);
        let line_range = Range {
            start: Position {
                line: range.start.line,
                character: 0,
            },
            end: Position {
                line: range.start.line,
                character: 0,
            },
        };
        lenses.push(CodeLens {
            range: line_range,
            command: Some(Command {
                title: "▶ Run test".to_string(),
                command: RUN_TEST_COMMAND.to_string(),
                arguments: Some(vec![
                    serde_json::Value::String(uri.to_string()),
                    serde_json::Value::String(test_name),
                ]),
            }),
            data: None,
        });
    }
    lenses
}

/// Parse `zz.runTest` arguments back into (file path, test name).
pub fn parse_run_args(args: &[serde_json::Value]) -> Option<(std::path::PathBuf, String)> {
    let [uri_value, name_value] = args else {
        return None;
    };
    let uri: Url = uri_value.as_str()?.parse().ok()?;
    let path = uri.to_file_path().ok()?;
    let name = name_value.as_str()?.to_string();
    if name.is_empty() {
        return None;
    }
    Some((path, name))
}

/// Locate the `zz` driver binary: sibling of this executable first
/// (release layout ships them together), `PATH` lookup second.
pub fn zz_binary() -> Option<std::path::PathBuf> {
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            #[cfg(windows)]
            let candidate = dir.join("zz.exe");
            #[cfg(not(windows))]
            let candidate = dir.join("zz");
            if candidate.is_file() {
                return Some(candidate);
            }
        }
    }
    std::env::var_os("PATH").and_then(|paths| {
        std::env::split_paths(&paths).find_map(|dir| {
            #[cfg(windows)]
            let candidate = dir.join("zz.exe");
            #[cfg(not(windows))]
            let candidate = dir.join("zz");
            candidate.is_file().then_some(candidate)
        })
    })
}

/// Run one test and report the outcome to the editor.
///
/// Entry point used by the `execute_command` handler; factored out so
/// the reporting logic stays independent of the transport.
pub async fn run_single_test(
    client: &tower_lsp::Client,
    file: &std::path::Path,
    test_name: &str,
) -> Option<serde_json::Value> {
    let Some(zz) = zz_binary() else {
        client
            .show_message(
                MessageType::ERROR,
                "zz binary not found next to zz-lsp or on PATH",
            )
            .await;
        return None;
    };
    client
        .show_message(MessageType::INFO, format!("running test `{test_name}`…"))
        .await;
    let output = tokio::process::Command::new(&zz)
        .arg("test")
        .arg(file)
        .arg("--filter")
        .arg(test_name)
        .arg("--exact")
        .output()
        .await;
    let output = match output {
        Ok(o) => o,
        Err(e) => {
            client
                .show_message(MessageType::ERROR, format!("failed to run `zz test`: {e}"))
                .await;
            return None;
        }
    };
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    // Full output to the server log; the notification carries the tail.
    if !stdout.trim().is_empty() {
        client.log_message(MessageType::LOG, stdout.trim()).await;
    }
    if !stderr.trim().is_empty() {
        client
            .log_message(MessageType::WARNING, stderr.trim())
            .await;
    }
    let tail = stdout
        .lines()
        .chain(stderr.lines())
        .rfind(|l| !l.trim().is_empty())
        .unwrap_or("(no output)")
        .to_string();
    let summary = if output.status.success() {
        format!("✅ `{test_name}` passed")
    } else {
        format!("❌ `{test_name}` failed: {tail}")
    };
    client
        .show_message(
            if output.status.success() {
                MessageType::INFO
            } else {
                MessageType::ERROR
            },
            summary.clone(),
        )
        .await;
    Some(serde_json::Value::String(summary))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lens_titles(program: &Program, source: &str) -> Vec<(String, u32)> {
        let uri: Url = "file:///tests.zz".parse().unwrap();
        test_lenses_for(program, source, &uri)
            .into_iter()
            .map(|lens| {
                let command = lens.command.expect("lens command");
                let name = command.arguments.expect("args")[1]
                    .as_str()
                    .expect("name")
                    .to_string();
                (name, lens.range.start.line)
            })
            .collect()
    }

    #[test]
    fn discovers_test_functions_only() {
        let src = "@test\nfunc adds() {\n    assert_eq(1 + 1, 2)\n}\nfunc helper() -> int {\n    return 1\n}\n";
        let program = zz_frontend::parse(src).program;
        let found = lens_titles(&program, src);
        assert_eq!(found.len(), 1, "only @test gets a lens");
        assert_eq!(found[0].0, "adds");
        assert_eq!(found[0].1, 0, "lens sits on the @test line");
    }

    #[test]
    fn lens_command_carries_uri_and_name() {
        let src = "@test\nfunc adds() {\n}\n";
        let program = zz_frontend::parse(src).program;
        let uri: Url = "file:///tests.zz".parse().unwrap();
        let lenses = test_lenses_for(&program, src, &uri);
        assert_eq!(lenses.len(), 1);
        let command = lenses[0].command.clone().expect("command");
        assert_eq!(command.command, RUN_TEST_COMMAND);
        let args = command.arguments.expect("args");
        assert_eq!(args[0], serde_json::Value::String(uri.to_string()));
        assert_eq!(args[1], serde_json::Value::String("adds".to_string()));
        assert!(parse_run_args(&args).is_some());
    }

    #[test]
    fn rejects_malformed_run_args() {
        assert!(parse_run_args(&[]).is_none());
        assert!(parse_run_args(&[serde_json::Value::String("not-a-uri".to_string())]).is_none());
        let uri = serde_json::Value::String("file:///t.zz".to_string());
        assert!(parse_run_args(&[uri.clone(), serde_json::Value::String(String::new())]).is_none());
        assert!(parse_run_args(&[uri]).is_none());
    }
}
