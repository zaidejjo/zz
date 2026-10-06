//! End-to-end integration tests for the ZZ language.
//!
//! Discovers all `.zz` fixture files under `tests/fixtures/` and runs them
//! through the `zz` binary, asserting expected behavior:
//!
//! - `syntax/`, `types/`, `stdlib/` → must exit 0 (success)
//! - `errors/` → must exit 1 (compile or runtime error)
//!
//! Each success fixture must print a final line matching its filename stem
//! (e.g., `declarations.zz` → `declarations_ok` or just the stem as a marker).

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

/// Locate the workspace root (where `tests/fixtures/` lives).
fn fixtures_dir() -> PathBuf {
    // When running `cargo test -p zz_cli`, CARGO_MANIFEST_DIR is crates/zz_cli/.
    // fixtures/ is at ../../tests/fixtures relative to that.
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures")
}

/// Run `zz run <file>` and return (exit_code, stdout, stderr).
/// Stdin comes from the `<stem>.stdin` sibling file when present,
/// otherwise closed (EOF): fixtures calling `input()` behave
/// identically everywhere instead of hanging on a TTY.
fn run_zz(file: &Path) -> (i32, String, String) {
    use std::io::Write as _;
    let zz_bin = env!("CARGO_BIN_EXE_zz");
    let stdin_path = file.with_extension("stdin");
    let input = std::fs::read(stdin_path).unwrap_or_default();
    let mut child = Command::new(zz_bin)
        .arg("run")
        .arg(file)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .current_dir(Path::new(env!("CARGO_MANIFEST_DIR")).join("../.."))
        .spawn()
        .unwrap_or_else(|e| panic!("failed to exec `zz run {file:?}`: {e}"));
    if !input.is_empty() {
        if let Some(stdin) = child.stdin.as_mut() {
            let _ = stdin.write_all(&input);
        }
    }
    // Always close the pipe so the child sees EOF (never blocks on a
    // held-open stdin when no `.stdin` file exists).
    drop(child.stdin.take());
    let output = child
        .wait_with_output()
        .unwrap_or_else(|e| panic!("failed to wait `zz run {file:?}`: {e}"));

    let exit_code = output.status.code().unwrap_or(-1);
    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).to_string();
    (exit_code, stdout, stderr)
}

/// Run `zz eval <src>` and return (exit_code, stdout, stderr).
fn run_zz_eval(src: &str) -> (i32, String, String) {
    let zz_bin = env!("CARGO_BIN_EXE_zz");
    let output = Command::new(zz_bin)
        .arg("eval")
        .arg(src)
        .current_dir(Path::new(env!("CARGO_MANIFEST_DIR")).join("../.."))
        .output()
        .unwrap_or_else(|e| panic!("failed to exec `zz eval`: {e}"));

    let exit_code = output.status.code().unwrap_or(-1);
    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).to_string();
    (exit_code, stdout, stderr)
}

/// Run `zz check <file>` and return (exit_code, stdout, stderr).
fn run_zz_check(file: &Path) -> (i32, String, String) {
    let zz_bin = env!("CARGO_BIN_EXE_zz");
    let output = Command::new(zz_bin)
        .arg("check")
        .arg(file)
        .current_dir(Path::new(env!("CARGO_MANIFEST_DIR")).join("../.."))
        .output()
        .unwrap_or_else(|e| panic!("failed to exec `zz check {file:?}`: {e}"));

    let exit_code = output.status.code().unwrap_or(-1);
    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).to_string();
    (exit_code, stdout, stderr)
}

/// Discover all `.zz` files in a directory (non-recursive).
fn find_fixtures(dir: &Path) -> Vec<PathBuf> {
    let mut files: Vec<PathBuf> = std::fs::read_dir(dir)
        .unwrap_or_else(|e| panic!("cannot read dir {dir:?}: {e}"))
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.extension().and_then(|s| s.to_str()) == Some("zz"))
        .collect();
    files.sort();
    files
}

/// Run `zz test <file>` and return (exit_code, stdout, stderr).
fn run_zz_test(file: &Path) -> (i32, String, String) {
    let zz_bin = env!("CARGO_BIN_EXE_zz");
    let output = Command::new(zz_bin)
        .arg("test")
        .arg(file)
        .current_dir(Path::new(env!("CARGO_MANIFEST_DIR")).join("../.."))
        .output()
        .unwrap_or_else(|e| panic!("failed to exec `zz test {file:?}`: {e}"));

    let exit_code = output.status.code().unwrap_or(-1);
    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).to_string();
    (exit_code, stdout, stderr)
}

/// Run `zz run --embed <dir> <file>` and return (exit_code, stdout, stderr).
fn run_zz_embed(embed_dir: &Path, file: &Path) -> (i32, String, String) {
    let zz_bin = env!("CARGO_BIN_EXE_zz");
    let output = Command::new(zz_bin)
        .arg("run")
        .arg("--embed")
        .arg(embed_dir)
        .arg(file)
        .current_dir(Path::new(env!("CARGO_MANIFEST_DIR")).join("../.."))
        .output()
        .unwrap_or_else(|e| panic!("failed to exec `zz run --embed {file:?}`: {e}"));

    let exit_code = output.status.code().unwrap_or(-1);
    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).to_string();
    (exit_code, stdout, stderr)
}

// ---------------------------------------------------------------------------
// Success fixtures: must exit 0
// ---------------------------------------------------------------------------

macro_rules! e2e_success_test {
    ($name:ident, $category:expr, $file:expr) => {
        #[test]
        fn $name() {
            let fixtures = fixtures_dir();
            let path = fixtures.join($category).join($file);
            assert!(path.exists(), "fixture not found: {}", path.display());

            let (exit, stdout, stderr) = run_zz(&path);
            assert_eq!(
                exit,
                0,
                "fixture {} should exit 0 but got {}.\nstdout:\n{stdout}\nstderr:\n{stderr}",
                path.display(),
                exit,
            );

            // Verify the last line contains a success marker.
            let last = stdout.lines().last().unwrap_or("");
            let _stem = path.file_stem().unwrap().to_string_lossy();
            assert!(
                !last.is_empty(),
                "fixture {} produced no output.\nstderr:\n{stderr}",
                path.display(),
            );
        }
    };
}

// Syntax fixtures
e2e_success_test!(e2e_syntax_declarations, "syntax", "declarations.zz");
e2e_success_test!(e2e_syntax_const, "syntax", "const.zz");
e2e_success_test!(e2e_syntax_functions, "syntax", "functions.zz");
e2e_success_test!(e2e_syntax_control_flow, "syntax", "control_flow.zz");
e2e_success_test!(e2e_syntax_range_var_bounds, "syntax", "range_var_bounds.zz");
e2e_success_test!(e2e_syntax_scope_collision, "syntax", "scope_collision.zz");
e2e_success_test!(
    e2e_syntax_for_annotated_decl,
    "syntax",
    "for_annotated_decl.zz"
);
e2e_success_test!(
    e2e_syntax_struct_scalar_fields,
    "syntax",
    "struct_scalar_fields.zz"
);
e2e_success_test!(
    e2e_syntax_hex_escape_bounds,
    "syntax",
    "hex_escape_bounds.zz"
);
e2e_success_test!(e2e_syntax_pipelines, "syntax", "pipelines.zz");
e2e_success_test!(e2e_syntax_hof, "syntax", "hof.zz");
e2e_success_test!(e2e_syntax_match, "syntax", "match.zz");
e2e_success_test!(e2e_syntax_match_assign, "syntax", "match_assign.zz");
e2e_success_test!(
    e2e_syntax_match_bare_return,
    "syntax",
    "match_bare_return.zz"
);
e2e_success_test!(e2e_syntax_frame_slots, "syntax", "frame_slots.zz");
e2e_success_test!(e2e_syntax_operators, "syntax", "operators.zz");
e2e_success_test!(e2e_syntax_fstrings, "syntax", "fstrings.zz");
e2e_success_test!(e2e_syntax_brace_escapes, "syntax", "brace_escapes.zz");
e2e_success_test!(
    e2e_syntax_brace_escapes_multiline,
    "syntax",
    "brace_escapes_multiline.zz"
);
e2e_success_test!(e2e_syntax_arrays, "syntax", "arrays.zz");
e2e_success_test!(e2e_syntax_dicts, "syntax", "dicts.zz");
e2e_success_test!(e2e_syntax_defer, "syntax", "defer.zz");
e2e_success_test!(e2e_syntax_string_blocks, "syntax", "string_blocks.zz");
e2e_success_test!(
    e2e_syntax_multiline_strings,
    "syntax",
    "multiline_strings.zz"
);
e2e_success_test!(e2e_syntax_return_in_loops, "syntax", "return_in_loops.zz");
e2e_success_test!(e2e_syntax_dict_iteration, "syntax", "dict_iteration.zz");
e2e_success_test!(e2e_syntax_pipe_elvis, "syntax", "pipe_elvis.zz");
e2e_success_test!(e2e_syntax_scalar_copy, "syntax", "scalar_copy.zz");
e2e_success_test!(e2e_syntax_elif_chain, "syntax", "elif_chain.zz");
e2e_success_test!(e2e_syntax_top_level_elif, "syntax", "top_level_elif.zz");
e2e_success_test!(e2e_syntax_chained_calls, "syntax", "chained_calls.zz");
e2e_success_test!(e2e_syntax_empty_infer, "syntax", "empty_infer.zz");
e2e_success_test!(
    e2e_syntax_closure_annotations,
    "syntax",
    "closure_annotations.zz"
);
e2e_success_test!(e2e_syntax_destructuring, "syntax", "destructuring.zz");
e2e_success_test!(
    e2e_regression_shadow_destructure_fn,
    "regression",
    "shadow_destructure_fn.zz"
);
e2e_success_test!(
    e2e_regression_shadow_destructure,
    "regression",
    "shadow_destructure.zz"
);
e2e_success_test!(
    e2e_regression_func_capture_destructure,
    "regression",
    "func_capture_destructure.zz"
);
e2e_success_test!(
    e2e_regression_closure_capture_destructure,
    "regression",
    "closure_capture_destructure.zz"
);
e2e_success_test!(
    e2e_regression_trailing_if_in_else,
    "regression",
    "trailing_if_in_else.zz"
);
e2e_success_test!(
    e2e_regression_match_return_in_if,
    "regression",
    "match_return_in_if.zz"
);
e2e_success_test!(
    e2e_regression_tuple_destructure,
    "regression",
    "tuple_destructure.zz"
);
e2e_success_test!(
    e2e_regression_neg_after_loop,
    "regression",
    "neg_after_loop.zz"
);
e2e_success_test!(
    e2e_regression_branch_call_returns,
    "regression",
    "branch_call_returns.zz"
);
e2e_success_test!(
    e2e_regression_branch_call_tails,
    "regression",
    "branch_call_tails.zz"
);
e2e_success_test!(
    e2e_regression_branch_early_return_calls,
    "regression",
    "branch_early_return_calls.zz"
);
e2e_success_test!(
    e2e_regression_string_accum_loop,
    "regression",
    "string_accum_loop.zz"
);
e2e_success_test!(
    e2e_regression_string_store_across_iter,
    "regression",
    "string_store_across_iter.zz"
);
e2e_success_test!(
    e2e_regression_string_index_set_binop,
    "regression",
    "string_index_set_binop.zz"
);
e2e_success_test!(
    e2e_regression_move_append_shapes,
    "regression",
    "move_append_shapes.zz"
);
e2e_success_test!(
    e2e_regression_move_append_field,
    "regression",
    "move_append_field.zz"
);
e2e_success_test!(
    e2e_regression_move_append_append,
    "regression",
    "move_append_append.zz"
);
e2e_success_test!(
    e2e_regression_move_append_alias,
    "regression",
    "move_append_alias.zz"
);
e2e_success_test!(
    e2e_regression_move_append_early_exit,
    "regression",
    "move_append_early_exit.zz"
);
e2e_success_test!(
    e2e_regression_move_append_spawn,
    "regression",
    "move_append_spawn.zz"
);
e2e_success_test!(
    e2e_regression_move_append_struct_copy,
    "regression",
    "move_append_struct_copy.zz"
);
e2e_success_test!(
    e2e_regression_fuzz_smoke_00,
    "regression",
    "fuzz_smoke_00.zz"
);
e2e_success_test!(
    e2e_regression_fuzz_smoke_01,
    "regression",
    "fuzz_smoke_01.zz"
);
e2e_success_test!(
    e2e_regression_fuzz_smoke_02,
    "regression",
    "fuzz_smoke_02.zz"
);
e2e_success_test!(
    e2e_regression_fuzz_smoke_03,
    "regression",
    "fuzz_smoke_03.zz"
);
e2e_success_test!(
    e2e_regression_fuzz_smoke_04,
    "regression",
    "fuzz_smoke_04.zz"
);
e2e_success_test!(
    e2e_regression_fuzz_smoke_05,
    "regression",
    "fuzz_smoke_05.zz"
);
e2e_success_test!(
    e2e_regression_fuzz_smoke_06,
    "regression",
    "fuzz_smoke_06.zz"
);
e2e_success_test!(
    e2e_regression_fuzz_smoke_07,
    "regression",
    "fuzz_smoke_07.zz"
);
e2e_success_test!(
    e2e_regression_fuzz_smoke_08,
    "regression",
    "fuzz_smoke_08.zz"
);
e2e_success_test!(
    e2e_regression_fuzz_smoke_09,
    "regression",
    "fuzz_smoke_09.zz"
);
e2e_success_test!(
    e2e_regression_fuzz_smoke_10,
    "regression",
    "fuzz_smoke_10.zz"
);
e2e_success_test!(
    e2e_regression_fuzz_smoke_11,
    "regression",
    "fuzz_smoke_11.zz"
);
// v4 fuzzer shapes (fixed seeds; all strict on both engines).
e2e_success_test!(
    e2e_regression_fuzz_smoke_12,
    "regression",
    "fuzz_smoke_12.zz"
);
e2e_success_test!(
    e2e_regression_fuzz_smoke_13,
    "regression",
    "fuzz_smoke_13.zz"
);
e2e_success_test!(
    e2e_regression_fuzz_smoke_14,
    "regression",
    "fuzz_smoke_14.zz"
);
// M0 edge corpus: strict-parity probes (both engines agree today).
e2e_success_test!(
    e2e_regression_edge_shift_mask,
    "regression",
    "edge_shift_mask.zz"
);
e2e_success_test!(
    e2e_regression_edge_cast_float_int,
    "regression",
    "edge_cast_float_int.zz"
);
e2e_success_test!(
    e2e_regression_edge_cast_str_int,
    "regression",
    "edge_cast_str_int.zz"
);
// M0 edge corpus: known-divergence probes (VM vs native differ; the
// parity harness tracks them as known failures, see M0 report).
e2e_success_test!(
    e2e_regression_edge_chained_store,
    "regression",
    "edge_chained_store.zz"
);
e2e_success_test!(
    e2e_regression_eval_order_store,
    "regression",
    "eval_order_store.zz"
);
e2e_success_test!(
    e2e_regression_edge_float_nan_display,
    "regression",
    "edge_float_nan_display.zz"
);
// Canonical float formatting (spec §4): exact-string pins on both legs.
e2e_success_test!(
    e2e_regression_edge_float_format,
    "regression",
    "edge_float_format.zz"
);
e2e_success_test!(
    e2e_regression_edge_slice_clamp,
    "regression",
    "edge_slice_clamp.zz"
);
e2e_success_test!(
    e2e_regression_edge_cast_float_nan,
    "regression",
    "edge_cast_float_nan.zz"
);
// Wrap-spec PR: overflow now wraps on all legs (was VM-trap errors).
e2e_success_test!(
    e2e_regression_edge_overflow_add,
    "regression",
    "edge_int_overflow_add.zz"
);
e2e_success_test!(
    e2e_regression_edge_overflow_mul,
    "regression",
    "edge_int_overflow_mul.zz"
);
e2e_success_test!(
    e2e_regression_edge_neg_min,
    "regression",
    "edge_int_neg_min.zz"
);
// Write-through family (value semantics for stores through clones,
// nested bases, temps, and struct paths).
e2e_success_test!(
    e2e_regression_edge_array_alias,
    "regression",
    "edge_array_alias.zz"
);
e2e_success_test!(
    e2e_regression_edge_dict_alias,
    "regression",
    "edge_dict_alias.zz"
);
e2e_success_test!(
    e2e_regression_edge_temp_index_drop,
    "regression",
    "edge_temp_index_drop.zz"
);
e2e_success_test!(
    e2e_regression_edge_field_index_store,
    "regression",
    "edge_field_index_store.zz"
);
e2e_success_test!(
    e2e_regression_edge_struct_path_store,
    "regression",
    "edge_struct_path_store.zz"
);
e2e_success_test!(
    e2e_regression_edge_negative_index,
    "regression",
    "edge_negative_index.zz"
);
// Evaluation order probes (call-order audit close-out).
e2e_success_test!(
    e2e_regression_edge_eval_order,
    "regression",
    "edge_eval_order.zz"
);
e2e_success_test!(
    e2e_regression_edge_compound_index_eval,
    "regression",
    "edge_compound_index_eval.zz"
);
e2e_success_test!(e2e_syntax_main_entrypoint, "syntax", "main_entrypoint.zz");
e2e_success_test!(e2e_syntax_match_guards, "syntax", "match_guards.zz");
e2e_success_test!(
    e2e_syntax_question_operator_newline,
    "syntax",
    "question_operator_newline.zz"
);
e2e_success_test!(
    e2e_syntax_struct_array_push,
    "syntax",
    "struct_array_push.zz"
);
e2e_success_test!(e2e_syntax_struct_impl, "syntax", "struct_impl.zz");
e2e_success_test!(e2e_syntax_method_free_fn, "syntax", "method_free_fn.zz");
e2e_success_test!(
    e2e_syntax_method_chain_recv,
    "syntax",
    "method_chain_recv.zz"
);
e2e_success_test!(e2e_syntax_function_types, "syntax", "function_types.zz");
e2e_success_test!(e2e_syntax_decorators, "syntax", "decorators.zz");
e2e_success_test!(
    e2e_syntax_extension_methods,
    "syntax",
    "extension_methods.zz"
);
e2e_success_test!(e2e_syntax_main_result, "syntax", "main_result.zz");
e2e_success_test!(e2e_syntax_bitwise_ops, "syntax", "bitwise_ops.zz");
e2e_success_test!(e2e_syntax_tuple_ops, "syntax", "tuple_ops.zz");
e2e_success_test!(
    e2e_syntax_tuple_unboxed_struct,
    "syntax",
    "tuple_unboxed_struct.zz"
);
e2e_success_test!(e2e_syntax_compound_assign, "syntax", "compound_assign.zz");
e2e_success_test!(e2e_syntax_generic_structs, "syntax", "generic_structs.zz");

// Type fixtures
e2e_success_test!(e2e_types_structs, "types", "structs.zz");
e2e_success_test!(e2e_types_aliases, "types", "aliases.zz");
e2e_success_test!(e2e_types_alias_import, "types", "alias_import.zz");
e2e_success_test!(e2e_types_enums, "types", "enums.zz");
e2e_success_test!(e2e_types_enum_import, "types", "enum_import.zz");
e2e_success_test!(e2e_types_enum_generics, "types", "enum_generics.zz");
e2e_success_test!(e2e_types_struct_embedding, "types", "struct_embedding.zz");
e2e_success_test!(e2e_types_generics, "types", "generics.zz");
e2e_success_test!(e2e_types_generic_bounds, "types", "generic_bounds.zz");
e2e_success_test!(e2e_types_variants, "types", "variants.zz");
e2e_success_test!(e2e_types_type_inference, "types", "type_inference.zz");
e2e_success_test!(e2e_types_smart_try, "types", "smart_try.zz");
e2e_success_test!(e2e_types_smart_try_convert, "types", "smart_try_convert.zz");

// Stdlib fixtures
e2e_success_test!(e2e_stdlib_strings, "stdlib", "strings.zz");
e2e_success_test!(e2e_stdlib_vectors, "stdlib", "vectors.zz");
e2e_success_test!(e2e_stdlib_str_utf8_parity, "stdlib", "str_utf8_parity.zz");
e2e_success_test!(
    e2e_stdlib_vec_nested_str_parity,
    "stdlib",
    "vec_nested_str_parity.zz"
);
e2e_success_test!(e2e_stdlib_math_ops, "stdlib", "math_ops.zz");
e2e_success_test!(e2e_stdlib_math_consts, "stdlib", "math_consts.zz");
e2e_success_test!(e2e_stdlib_enumerate_loop, "stdlib", "enumerate_loop.zz");
e2e_success_test!(e2e_stdlib_path_join, "stdlib", "path_join.zz");
e2e_success_test!(e2e_stdlib_map_set, "stdlib", "map_set.zz");
e2e_success_test!(e2e_stdlib_dec_ops, "stdlib", "dec_ops.zz");
e2e_success_test!(e2e_stdlib_csv_test, "stdlib", "csv_test.zz");
e2e_success_test!(e2e_stdlib_builders, "stdlib", "builders.zz");
e2e_success_test!(e2e_stdlib_time_date, "stdlib", "time_date.zz");
e2e_success_test!(e2e_stdlib_jsonmod, "stdlib", "jsonmod.zz");
e2e_success_test!(e2e_stdlib_filesystem, "stdlib", "filesystem.zz");
e2e_success_test!(e2e_stdlib_console, "stdlib", "console.zz");
e2e_success_test!(e2e_stdlib_envmod, "stdlib", "envmod.zz");
e2e_success_test!(e2e_stdlib_time_ops, "stdlib", "time_ops.zz");
e2e_success_test!(e2e_stdlib_fs_test, "stdlib", "fs_test.zz");
e2e_success_test!(e2e_stdlib_fs_comprehensive, "stdlib", "fs_comprehensive.zz");
e2e_success_test!(e2e_stdlib_result_print, "stdlib", "result_print.zz");
e2e_success_test!(e2e_stdlib_import_alias, "stdlib", "import_alias.zz");
e2e_success_test!(e2e_stdlib_fs_path, "stdlib", "fs_path.zz");
e2e_success_test!(e2e_stdlib_fs_vfs, "stdlib", "fs_vfs.zz");
e2e_success_test!(e2e_stdlib_bytes, "stdlib", "bytes.zz");
e2e_success_test!(e2e_stdlib_env_full, "stdlib", "env_full.zz");
e2e_success_test!(e2e_stdlib_env_test, "stdlib", "env_test.zz");
e2e_success_test!(e2e_stdlib_time_test, "stdlib", "time_test.zz");
e2e_success_test!(e2e_stdlib_math_extended, "stdlib", "math_extended_test.zz");
e2e_success_test!(e2e_stdlib_json_test, "stdlib", "json_test.zz");
e2e_success_test!(e2e_stdlib_encoding_test, "stdlib", "encoding_test.zz");
e2e_success_test!(
    e2e_stdlib_str_extended_test,
    "stdlib",
    "str_extended_test.zz"
);
e2e_success_test!(e2e_stdlib_net_tcp_test, "stdlib", "net_tcp_test.zz");
e2e_success_test!(e2e_stdlib_input_chained, "stdlib", "input_chained.zz");
e2e_success_test!(e2e_stdlib_http_client_test, "stdlib", "http_client_test.zz");
e2e_success_test!(e2e_stdlib_http_server_test, "stdlib", "http_server_test.zz");
e2e_success_test!(
    e2e_stdlib_http_phase5b_test,
    "stdlib",
    "http_phase5b_test.zz"
);
e2e_success_test!(
    e2e_stdlib_http_request_response,
    "stdlib",
    "http_request_response.zz"
);
e2e_success_test!(e2e_stdlib_http_fetch_test, "stdlib", "http_fetch_test.zz");
e2e_success_test!(e2e_stdlib_http_router_p1, "stdlib", "http_router_p1.zz");
e2e_success_test!(
    e2e_stdlib_http_keepalive_p1,
    "stdlib",
    "http_keepalive_p1.zz"
);
e2e_success_test!(e2e_stdlib_http_static_p2, "stdlib", "http_static_p2.zz");
e2e_success_test!(
    e2e_stdlib_http_middleware_p2,
    "stdlib",
    "http_middleware_p2.zz"
);
e2e_success_test!(e2e_stdlib_http_limits_p2, "stdlib", "http_limits_p2.zz");
e2e_success_test!(e2e_stdlib_http_bytes_p3, "stdlib", "http_bytes_p3.zz");
e2e_success_test!(e2e_stdlib_http_tls_p3, "stdlib", "http_tls_p3.zz");
e2e_success_test!(e2e_stdlib_http_hijack_p3, "stdlib", "http_hijack_p3.zz");
e2e_success_test!(e2e_stdlib_ns_collision, "stdlib", "ns_collision.zz");
e2e_success_test!(
    e2e_stdlib_bench_memory_arena,
    "stdlib",
    "bench_memory_arena.zz"
);
e2e_success_test!(
    e2e_stdlib_concurrency_spawn_test,
    "stdlib",
    "concurrency_spawn_test.zz"
);
e2e_success_test!(e2e_stdlib_channel_test, "stdlib", "channel_test.zz");
e2e_success_test!(
    e2e_stdlib_concurrency_tasks_test,
    "stdlib",
    "concurrency_tasks_test.zz"
);
e2e_success_test!(
    e2e_stdlib_concurrency_stress_test,
    "stdlib",
    "concurrency_stress_test.zz"
);
e2e_success_test!(
    e2e_stdlib_concurrency_panic_test,
    "stdlib",
    "concurrency_panic_test.zz"
);
e2e_success_test!(
    e2e_stdlib_concurrency_try_join_test,
    "stdlib",
    "concurrency_try_join_test.zz"
);
e2e_success_test!(
    e2e_stdlib_concurrency_capture_test,
    "stdlib",
    "concurrency_capture_test.zz"
);
e2e_success_test!(
    e2e_stdlib_concurrency_recall_test,
    "stdlib",
    "concurrency_recall_test.zz"
);
e2e_success_test!(
    e2e_stdlib_concurrency_verdict_chain_test,
    "stdlib",
    "concurrency_verdict_chain_test.zz"
);
e2e_success_test!(
    e2e_stdlib_concurrency_join_chain_test,
    "stdlib",
    "concurrency_join_chain_test.zz"
);
e2e_success_test!(
    e2e_stdlib_concurrency_mpmc_test,
    "stdlib",
    "concurrency_mpmc_test.zz"
);
e2e_success_test!(
    e2e_stdlib_concurrency_relay_test,
    "stdlib",
    "concurrency_relay_test.zz"
);
e2e_success_test!(
    e2e_stdlib_concurrency_matchbind_test,
    "stdlib",
    "concurrency_matchbind_test.zz"
);
e2e_success_test!(
    e2e_stdlib_concurrency_vartrip_test,
    "stdlib",
    "concurrency_vartrip_test.zz"
);
e2e_success_test!(e2e_stdlib_regexp_test, "stdlib", "regexp_test.zz");
e2e_success_test!(e2e_stdlib_crypto_test, "stdlib", "crypto_test.zz");
e2e_success_test!(
    e2e_stdlib_crypto_passwords_test,
    "stdlib",
    "crypto_passwords_test.zz"
);
e2e_success_test!(e2e_stdlib_crypto_jwt_test, "stdlib", "crypto_jwt_test.zz");
e2e_success_test!(e2e_stdlib_time_ext_test, "stdlib", "time_ext_test.zz");
e2e_success_test!(e2e_stdlib_log_test, "stdlib", "log_test.zz");
e2e_success_test!(e2e_stdlib_sys_test, "stdlib", "sys_test.zz");
e2e_success_test!(e2e_stdlib_term_test, "stdlib", "term_test.zz");
e2e_success_test!(e2e_stdlib_args_test, "stdlib", "args_test.zz");
e2e_success_test!(e2e_stdlib_process_test, "stdlib", "process_test.zz");
e2e_success_test!(e2e_stdlib_uuid_test, "stdlib", "uuid_test.zz");
e2e_success_test!(
    e2e_stdlib_concurrent_http_test,
    "stdlib",
    "concurrent_http_test.zz"
);
e2e_success_test!(
    e2e_stdlib_json_extended_test,
    "stdlib",
    "json_extended_test.zz"
);
e2e_success_test!(e2e_stdlib_selective_import, "stdlib", "selective_import.zz");
e2e_success_test!(e2e_stdlib_selective_calls, "stdlib", "selective_calls.zz");
e2e_success_test!(
    e2e_stdlib_alias_module_calls,
    "stdlib",
    "alias_module_calls.zz"
);
e2e_success_test!(
    e2e_stdlib_str_selective_methods,
    "stdlib",
    "str_selective_methods.zz"
);
e2e_success_test!(e2e_stdlib_wildcard_import, "stdlib", "wildcard_import.zz");
e2e_success_test!(e2e_stdlib_symbol_alias, "stdlib", "symbol_alias.zz");
e2e_success_test!(e2e_stdlib_multi_selective, "stdlib", "multi_selective.zz");
e2e_success_test!(e2e_stdlib_local_selective, "stdlib", "local_selective.zz");
e2e_success_test!(
    e2e_stdlib_generic_selective,
    "stdlib",
    "generic_selective.zz"
);
e2e_success_test!(e2e_stdlib_local_wildcard, "stdlib", "local_wildcard.zz");
e2e_success_test!(e2e_stdlib_sqlz_sqlite, "stdlib", "sqlz_sqlite.zz");
e2e_success_test!(e2e_stdlib_sqlz_transaction, "stdlib", "sqlz_transaction.zz");
e2e_success_test!(e2e_stdlib_colors_demo, "stdlib", "colors_demo.zz");
e2e_success_test!(
    e2e_stdlib_option_interpolation,
    "stdlib",
    "option_interpolation.zz"
);

// ---------------------------------------------------------------------------
// Error fixtures: must exit 1
// ---------------------------------------------------------------------------

macro_rules! e2e_error_test {
    ($name:ident, $file:expr) => {
        #[test]
        fn $name() {
            let fixtures = fixtures_dir();
            let path = fixtures.join("errors").join($file);
            assert!(path.exists(), "fixture not found: {}", path.display());

            let (exit, stdout, stderr) = run_zz(&path);
            assert_ne!(
                exit,
                0,
                "error fixture {} should fail but exited 0.\nstdout:\n{stdout}",
                path.display(),
            );
            // stderr should contain an error diagnostic.
            assert!(
                stderr.contains("error") || !stderr.is_empty(),
                "error fixture {} should produce diagnostics.\nstderr:\n{stderr}",
                path.display(),
            );
        }
    };
}

e2e_error_test!(e2e_err_type_mismatch, "type_mismatch.zz");
e2e_error_test!(e2e_err_alias_cycle, "alias_cycle.zz");
e2e_error_test!(e2e_err_alias_dup, "alias_dup.zz");
e2e_error_test!(e2e_err_alias_arity, "alias_arity.zz");
e2e_error_test!(e2e_err_enum_nonexhaustive, "enum_nonexhaustive.zz");
e2e_error_test!(e2e_err_enum_unknown_variant, "enum_unknown_variant.zz");
e2e_error_test!(e2e_err_enum_missing_payload, "enum_missing_payload.zz");
e2e_error_test!(e2e_err_enum_extra_arg, "enum_extra_arg.zz");
e2e_error_test!(e2e_err_enum_dup, "enum_dup.zz");
e2e_error_test!(e2e_err_enum_generic_mismatch, "enum_generic_mismatch.zz");
e2e_error_test!(
    e2e_err_return_fallthrough_mismatch,
    "return_fallthrough_mismatch.zz"
);
e2e_error_test!(e2e_err_return_missing_else, "return_missing_else.zz");
e2e_error_test!(
    e2e_err_return_match_fallthrough_mismatch,
    "return_match_fallthrough_mismatch.zz"
);
e2e_error_test!(
    e2e_err_return_annotated_join_mismatch,
    "return_annotated_join_mismatch.zz"
);
e2e_error_test!(e2e_err_const_reassign, "const_reassign.zz");
e2e_error_test!(e2e_err_undefined_var, "undefined_var.zz");
e2e_error_test!(e2e_err_missing_field, "missing_field.zz");
e2e_error_test!(e2e_err_arity, "arity.zz");
e2e_error_test!(e2e_err_http_route_dup, "http_route_dup.zz");
e2e_error_test!(e2e_err_http_route_path, "http_route_path.zz");
e2e_error_test!(e2e_err_http_route_method, "http_route_method.zz");
e2e_error_test!(e2e_err_http_tls_cert, "http_tls_cert.zz");
e2e_error_test!(e2e_err_http_hijack_arity, "http_hijack_arity.zz");
e2e_error_test!(e2e_err_parse_error, "parse_error.zz");
e2e_error_test!(e2e_err_div_by_zero, "div_by_zero.zz");
e2e_error_test!(e2e_err_unknown_field, "unknown_field.zz");
e2e_error_test!(e2e_err_struct_init_assign, "struct_init_assign_error.zz");
e2e_error_test!(e2e_err_int_float_cmp, "int_float_cmp.zz");
e2e_error_test!(e2e_err_bare_import_std, "bare_import_std.zz");
e2e_error_test!(e2e_err_assign_in_condition, "assign_in_condition.zz");
e2e_error_test!(e2e_err_generic_bracket_params, "generic_bracket_params.zz");
e2e_error_test!(e2e_err_bitwise_float, "bitwise_float.zz");
e2e_error_test!(e2e_err_bitwise_neg_shift, "bitwise_neg_shift.zz");
e2e_error_test!(e2e_err_tuple_index_oob, "tuple_index_oob.zz");
e2e_error_test!(e2e_err_tuple_index_dynamic, "tuple_index_dynamic.zz");
e2e_error_test!(e2e_err_compound_type_mismatch, "compound_type_mismatch.zz");
e2e_error_test!(e2e_err_compound_const, "compound_const.zz");
e2e_error_test!(e2e_err_compound_chain, "compound_chain.zz");
e2e_error_test!(
    e2e_err_generic_struct_mismatch,
    "generic_struct_mismatch.zz"
);
e2e_error_test!(e2e_err_generic_struct_arity, "generic_struct_arity.zz");
e2e_error_test!(e2e_err_generic_unbound, "generic_unbound.zz");
e2e_error_test!(e2e_err_pg_connect_refused, "pg_connect_refused.zz");
e2e_error_test!(e2e_err_mysql_connect_refused, "mysql_connect_refused.zz");
e2e_error_test!(e2e_err_decorator_mismatch, "decorator_mismatch.zz");
e2e_error_test!(e2e_err_try_outside_result, "try_outside_result.zz");
e2e_error_test!(e2e_err_try_no_convert, "try_no_convert.zz");
e2e_error_test!(e2e_err_try_ambiguous_convert, "try_ambiguous_convert.zz");
e2e_error_test!(e2e_err_try_closure_no_annot, "try_closure_no_annot.zz");
e2e_error_test!(e2e_err_ext_orphan_dup, "ext_orphan_dup.zz");
e2e_error_test!(e2e_err_ext_builtin_collision, "ext_builtin_collision.zz");
e2e_error_test!(e2e_err_main_result_err, "main_result_err.zz");
e2e_error_test!(e2e_err_try_double_unwrap, "try_double_unwrap.zz");
e2e_error_test!(e2e_err_spawn_non_closure, "spawn_non_closure.zz");
e2e_error_test!(e2e_err_chan_send_non_chan, "chan_send_non_chan.zz");
// M0 edge corpus: both engines fail (messages differ; error parity only
// requires failure on both sides).
e2e_error_test!(e2e_err_edge_neg_shift, "edge_neg_shift_err.zz");
e2e_error_test!(e2e_err_edge_rem_zero, "edge_rem_zero_err.zz");
// Edge error probes whose fixes land in later PRs (still VM-fail).
e2e_error_test!(e2e_err_edge_min_div_neg1, "edge_int_min_div_neg1.zz");
e2e_error_test!(e2e_err_edge_min_rem_neg1, "edge_int_min_rem_neg1.zz");
e2e_error_test!(e2e_err_edge_pow_neg, "edge_int_pow_neg.zz");
e2e_error_test!(e2e_err_edge_index_oob, "edge_index_oob.zz");

// ---------------------------------------------------------------------------
// Eval tests: inline code via `zz eval`
// ---------------------------------------------------------------------------

#[test]
fn e2e_embed_vm_serves_assets() {
    // `fs_embed.zz` is intentionally NOT a plain success fixture (without
    // `--embed` its reads are correctly not_found); it runs here with an
    // asset dir, asserting the full EmbedFS surface in the VM.
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let dir = root.join("tests/fixtures/stdlib/data/embed_demo");
    let file = root.join("tests/fixtures/stdlib/fs_embed.zz");
    let (exit, stdout, stderr) = run_zz_embed(&dir, &file);
    assert_eq!(exit, 0, "stderr: {stderr}");
    let lines: Vec<&str> = stdout.lines().collect();
    assert!(
        lines.iter().any(|l| l.trim() == "index: hello embedded"),
        "{stdout}"
    );
    assert!(
        lines.iter().any(|l| l.trim() == "css: body{color:red}"),
        "{stdout}"
    );
    assert!(
        lines.iter().any(|l| l.trim() == "css_dir: [site.css]"),
        "{stdout}"
    );
    assert!(lines.iter().any(|l| l.trim() == "true"), "{stdout}");
    assert!(lines.iter().any(|l| l.trim() == "false"), "{stdout}");
    assert!(
        lines
            .iter()
            .any(|l| l.starts_with("embed_readonly: fs:write:invalid_input:")),
        "{stdout}"
    );
    assert!(lines.iter().any(|l| l.trim() == "fs_embed_ok"), "{stdout}");
}

#[test]
fn e2e_eval_basic_arithmetic() {
    let (exit, stdout, _) = run_zz_eval("1 + 2");
    assert_eq!(exit, 0);
    assert_eq!(stdout.trim(), "3");
}

#[test]
fn e2e_eval_string_interpolation() {
    let (exit, stdout, _) = run_zz_eval("name := \"ZZ\"; \"Hello, {name}!\"");
    assert_eq!(exit, 0);
    assert_eq!(stdout.trim(), "Hello, ZZ!");
}

#[test]
fn e2e_eval_closure() {
    let (exit, stdout, _) =
        run_zz_eval("double := |x: int| x * 2; nums := [1, 2, 3]; map(nums, double)");
    assert_eq!(exit, 0);
    assert_eq!(stdout.trim(), "[2, 4, 6]");
}

#[test]
fn e2e_eval_pipeline() {
    let (exit, stdout, _) =
        run_zz_eval("inc := |x: int| x + 1; dbl := |x: int| x * 2; 5 |> inc |> dbl");
    assert_eq!(exit, 0);
    assert_eq!(stdout.trim(), "12");
}

#[test]
fn e2e_eval_option() {
    let (exit, stdout, _) =
        run_zz_eval("val := .some(42); match val { .some(v) => v, .none => 0 }");
    assert_eq!(exit, 0);
    assert_eq!(stdout.trim(), "42");
}

#[test]
fn e2e_eval_elvis() {
    let (exit, stdout, _) = run_zz_eval("none_val: Option<int> = .none; none_val ?? 99");
    assert_eq!(exit, 0);
    assert_eq!(stdout.trim(), "99");
}

#[test]
fn e2e_eval_elvis_result() {
    let (exit, stdout, _) = run_zz_eval(".ok(42) ?? -1");
    assert_eq!(exit, 0);
    assert_eq!(stdout.trim(), "42");
}

#[test]
fn e2e_eval_elvis_result_err() {
    let (exit, stdout, _) = run_zz_eval(".err(\"boom\") ?? -1");
    assert_eq!(exit, 0);
    assert_eq!(stdout.trim(), "-1");
}

#[test]
fn e2e_eval_pipe_elvis_precedence() {
    // | > binds tighter than ??, so `val ?? 0 |> double()` = `val ?? (0 |> double())`
    let (exit, stdout, _) =
        run_zz_eval("double := |x: int| x * 2; val: Option<int> = .some(21); val ?? 0 |> double()");
    assert_eq!(exit, 0);
    // val is Some(21), so ?? returns 21 directly (pipe is on the fallback side)
    assert_eq!(stdout.trim(), "21");
}

#[test]
fn e2e_eval_int_float_promotion() {
    let (exit, stdout, _) = run_zz_eval("1 + 2.5");
    assert_eq!(exit, 0);
    assert_eq!(stdout.trim(), "3.5");
}

#[test]
fn e2e_eval_typeof() {
    let (exit, stdout, _) = run_zz_eval("typeof(42)");
    assert_eq!(exit, 0);
    assert_eq!(stdout.trim(), "int");
}

#[test]
fn e2e_eval_list_comprehension() {
    let (exit, stdout, _) = run_zz_eval("[x ** 2 for x in range(0, 5)]");
    assert_eq!(exit, 0);
    assert_eq!(stdout.trim(), "[0, 1, 4, 9, 16]");
}

#[test]
fn e2e_eval_default_params() {
    let (exit, stdout, _) =
        run_zz_eval("func greet(name: str, greeting: str = \"Hello\") -> str { \"{greeting}, {name}\" }; greet(\"ZZ\")");
    assert_eq!(exit, 0);
    assert_eq!(stdout.trim(), "Hello, ZZ");
}

// Note: struct definitions/mutation don't work in eval mode (parser limitation).
// Struct tests are covered by `e2e_types_structs` run fixture.

#[test]
fn e2e_eval_string_comparison_if() {
    // Verify string comparison before block parses AND runs correctly.
    let (exit, stdout, _) = run_zz_eval(r#"x := "hello"; if x == "hello" { "yes" } else { "no" }"#);
    assert_eq!(exit, 0);
    assert_eq!(stdout.trim(), "yes");
}

#[test]
fn e2e_eval_string_interpolation_still_works() {
    let (exit, stdout, _) = run_zz_eval(r#"greeting := "Hi"; name := "ZZ"; "{greeting}, {name}!""#);
    assert_eq!(exit, 0);
    assert_eq!(stdout.trim(), "Hi, ZZ!");
}

#[test]
fn e2e_eval_unspaced_string_block() {
    let (exit, stdout, _) = run_zz_eval(r#"x := "hello"; if x == "hello"{ "yes" }else{ "no" }"#);
    assert_eq!(exit, 0);
    assert_eq!(stdout.trim(), "yes");
}

#[test]
fn e2e_eval_multi_interpolation() {
    let (exit, stdout, _) = run_zz_eval(r#"a := "Hello"; b := "World"; "{a} {b}!""#);
    assert_eq!(exit, 0);
    assert_eq!(stdout.trim(), "Hello World!");
}

#[test]
fn e2e_check_string_before_while_block() {
    let tmp = std::env::temp_dir().join("zz_str_while_test.zz");
    std::fs::write(
        &tmp,
        r#"s := "test"
while s == "test" {
    println(s)
}"#,
    )
    .unwrap();
    let (exit, _, stderr) = run_zz_check(&tmp);
    assert_eq!(
        exit, 0,
        "check should pass for string-before-while-block.\nstderr: {stderr}"
    );
    std::fs::remove_file(&tmp).ok();
}

// ---------------------------------------------------------------------------
// Dynamic discovery: run every .zz under fixtures/success/
// ---------------------------------------------------------------------------
//
// These tests are redundant with the individually-registered `e2e_*` tests
// above (each fixture is run twice in `cargo test`). They exist as a safety
// net to catch fixtures added without an explicit registration. Marked
// `#[ignore]` so the default `cargo test` stays fast. Run explicitly with:
//   cargo test -p zz_cli --test e2e -- --ignored

/// Exhaustive VM sweep: EVERY success fixture must exit 0.
/// Runs un-ignored as the registration-backstop gate: a new fixture with
/// no `e2e_success_test!` still runs here (stdin via `<stem>.stdin`).
#[test]
fn e2e_discover_all_success_fixtures() {
    let fixtures = fixtures_dir();
    let success_dirs = ["syntax", "types", "stdlib"];
    let mut failures = Vec::new();

    for dir_name in &success_dirs {
        let dir = fixtures.join(dir_name);
        if !dir.is_dir() {
            continue;
        }
        for file in find_fixtures(&dir) {
            let (exit, stdout, stderr) = run_zz(&file);
            if exit != 0 {
                failures.push(format!(
                    "FAIL {} (exit {exit})\nstdout: {stdout}\nstderr: {stderr}",
                    file.display(),
                ));
            }
        }
    }

    if !failures.is_empty() {
        panic!(
            "{} fixture(s) failed:\n{}",
            failures.len(),
            failures.join("\n\n"),
        );
    }
}

/// Exhaustive VM sweep: EVERY error fixture must fail.
/// Runs un-ignored alongside the success sweep above.
#[test]
fn e2e_discover_all_error_fixtures() {
    let fixtures = fixtures_dir();
    let err_dir = fixtures.join("errors");
    if !err_dir.is_dir() {
        return; // no error fixtures, skip
    }

    let mut failures = Vec::new();
    for file in find_fixtures(&err_dir) {
        let (exit, stdout, _stderr) = run_zz(&file);
        if exit == 0 {
            failures.push(format!(
                "FAIL {} — should error but exited 0\nstdout: {stdout}",
                file.display(),
            ));
        }
    }

    if !failures.is_empty() {
        panic!(
            "{} error fixture(s) failed:\n{}",
            failures.len(),
            failures.join("\n\n"),
        );
    }
}

/// Real-argv flow: `args.get_raw()` must see CLI args after the fixture
/// path (the standard harness passes none, so this test drives `zz` with
/// explicit extra args). Covers the VM; AOT real-argv is verified by the
/// same fixture via `zz run --native` during development.
#[test]
fn e2e_stdlib_args_raw_argv() {
    let zz_bin = env!("CARGO_BIN_EXE_zz");
    let fixture = fixtures_dir().join("stdlib/args_raw_test.zz");
    let output = Command::new(zz_bin)
        .arg("run")
        .arg(&fixture)
        .arg("--output")
        .arg("x.out")
        .arg("--verbose")
        .current_dir(Path::new(env!("CARGO_MANIFEST_DIR")).join("../.."))
        .output()
        .unwrap_or_else(|e| panic!("failed to exec `zz run args_raw_test.zz`: {e}"));
    assert_eq!(output.status.code(), Some(0));
    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    for expected in [
        "nargs: 3",
        "a0: --output",
        "a1: x.out",
        "a2: --verbose",
        "args_raw_ok",
    ] {
        assert!(
            stdout.contains(expected),
            "missing `{expected}` in:\n{stdout}"
        );
    }
}

// ---------------------------------------------------------------------------
// Test fixtures: `zz test` must exit 0 (all @test functions pass)
// ---------------------------------------------------------------------------

macro_rules! e2e_test_success {
    ($name:ident, $file:expr) => {
        #[test]
        fn $name() {
            let fixtures = fixtures_dir();
            let path = fixtures.join("test").join($file);
            assert!(path.exists(), "fixture not found: {}", path.display());

            let (exit, stdout, stderr) = run_zz_test(&path);
            assert_eq!(
                exit,
                0,
                "fixture {} should exit 0 but got {}.\nstdout:\n{stdout}\nstderr:\n{stderr}",
                path.display(),
                exit,
            );
        }
    };
}

e2e_test_success!(e2e_test_basic_assertions, "basic_assertions.zz");

// --- Test framework feature tests ---

e2e_test_success!(e2e_test_setup_teardown, "setup_teardown.zz");
e2e_test_success!(e2e_test_cases, "cases.zz");
e2e_test_success!(e2e_test_should_panic, "should_panic.zz");
e2e_test_success!(e2e_test_ignore, "ignore.zz");
e2e_test_success!(e2e_test_tags, "tags.zz");
e2e_test_success!(e2e_test_assertions, "assertions.zz");
e2e_test_success!(e2e_test_timeout, "timeout.zz");
e2e_test_success!(e2e_test_retry, "retry.zz");

// --- CLI flag integration tests ---

#[test]
fn e2e_test_list_flag() {
    let fixtures = fixtures_dir();
    let path = fixtures.join("test").join("tags.zz");
    assert!(path.exists(), "fixture not found: {}", path.display());

    let zz_bin = env!("CARGO_BIN_EXE_zz");
    let output = Command::new(zz_bin)
        .arg("test")
        .arg(&path)
        .arg("--list")
        .current_dir(Path::new(env!("CARGO_MANIFEST_DIR")).join("../.."))
        .output()
        .expect("failed to exec zz test --list");

    let exit = output.status.code().unwrap_or(-1);
    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    assert_eq!(
        exit,
        0,
        "--list should exit 0. stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        stdout.contains("test_fast_1"),
        "--list should show test_fast_1. stdout: {stdout}"
    );
    assert!(
        stdout.contains("test_slow_1"),
        "--list should show test_slow_1. stdout: {stdout}"
    );
    assert!(
        stdout.contains("4 tests found"),
        "--list should show count. stdout: {stdout}"
    );
}

#[test]
fn e2e_test_filter_flag() {
    let fixtures = fixtures_dir();
    let path = fixtures.join("test").join("tags.zz");
    assert!(path.exists(), "fixture not found: {}", path.display());

    let zz_bin = env!("CARGO_BIN_EXE_zz");
    let output = Command::new(zz_bin)
        .arg("test")
        .arg(&path)
        .arg("--filter")
        .arg("fast")
        .current_dir(Path::new(env!("CARGO_MANIFEST_DIR")).join("../.."))
        .output()
        .expect("failed to exec zz test --filter");

    let exit = output.status.code().unwrap_or(-1);
    let stderr = String::from_utf8_lossy(&output.stderr).to_string();
    assert_eq!(exit, 0, "--filter fast should exit 0. stderr: {stderr}");
    assert!(
        stderr.contains("2 passed"),
        "should pass 2 fast tests. stderr: {stderr}"
    );
}

#[test]
fn e2e_test_tag_flag() {
    let fixtures = fixtures_dir();
    let path = fixtures.join("test").join("tags.zz");
    assert!(path.exists(), "fixture not found: {}", path.display());

    let zz_bin = env!("CARGO_BIN_EXE_zz");
    let output = Command::new(zz_bin)
        .arg("test")
        .arg(&path)
        .arg("--tag")
        .arg("slow")
        .current_dir(Path::new(env!("CARGO_MANIFEST_DIR")).join("../.."))
        .output()
        .expect("failed to exec zz test --tag");

    let exit = output.status.code().unwrap_or(-1);
    let stderr = String::from_utf8_lossy(&output.stderr).to_string();
    assert_eq!(exit, 0, "--tag slow should exit 0. stderr: {stderr}");
    assert!(
        stderr.contains("1 passed"),
        "should pass 1 slow test. stderr: {stderr}"
    );
}

#[test]
fn e2e_test_skip_flag() {
    let fixtures = fixtures_dir();
    let path = fixtures.join("test").join("tags.zz");
    assert!(path.exists(), "fixture not found: {}", path.display());

    let zz_bin = env!("CARGO_BIN_EXE_zz");
    let output = Command::new(zz_bin)
        .arg("test")
        .arg(&path)
        .arg("--skip")
        .arg("slow")
        .current_dir(Path::new(env!("CARGO_MANIFEST_DIR")).join("../.."))
        .output()
        .expect("failed to exec zz test --skip");

    let exit = output.status.code().unwrap_or(-1);
    let stderr = String::from_utf8_lossy(&output.stderr).to_string();
    assert_eq!(exit, 0, "--skip slow should exit 0. stderr: {stderr}");
    // Should pass 3 tests (2 fast + 1 untagged), skip 1 slow
    assert!(
        stderr.contains("3 passed"),
        "should pass 3 tests. stderr: {stderr}"
    );
}

#[test]
fn e2e_test_repeat_flag() {
    let fixtures = fixtures_dir();
    let path = fixtures.join("test").join("assertions.zz");
    assert!(path.exists(), "fixture not found: {}", path.display());

    let zz_bin = env!("CARGO_BIN_EXE_zz");
    let output = Command::new(zz_bin)
        .arg("test")
        .arg(&path)
        .arg("--repeat")
        .arg("2")
        .current_dir(Path::new(env!("CARGO_MANIFEST_DIR")).join("../.."))
        .output()
        .expect("failed to exec zz test --repeat");

    let exit = output.status.code().unwrap_or(-1);
    let stderr = String::from_utf8_lossy(&output.stderr).to_string();
    assert_eq!(exit, 0, "--repeat 2 should exit 0. stderr: {stderr}");
    assert!(
        stderr.contains("iteration 1/2"),
        "should show iteration. stderr: {stderr}"
    );
    assert!(
        stderr.contains("aggregate"),
        "should show aggregate. stderr: {stderr}"
    );
}

/// Run `zz run <file>` with controlled stdin (VM engine only).
///
/// `input()` reads the real process stdin: inheriting it hangs the suite on
/// interactive terminals (TTY) while passing under CI (/dev/null). These
/// tests pin stdin explicitly so they behave identically everywhere.
fn run_zz_with_stdin(file: &Path, stdin: Stdio) -> (i32, String, String) {
    let zz_bin = env!("CARGO_BIN_EXE_zz");
    let output = Command::new(zz_bin)
        .arg("run")
        .arg(file)
        .stdin(stdin)
        .current_dir(Path::new(env!("CARGO_MANIFEST_DIR")).join("../.."))
        .output()
        .unwrap_or_else(|e| panic!("failed to exec `zz run {file:?}`: {e}"));

    let exit_code = output.status.code().unwrap_or(-1);
    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).to_string();
    (exit_code, stdout, stderr)
}

const INPUT_PROG: &str =
    "func main() {\n    line := input(\"\")\n    println(\"got:\" + line)\n}\n";

fn write_input_prog(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("zz-e2e-input-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join(name);
    std::fs::write(&file, INPUT_PROG).unwrap();
    file
}

#[test]
fn e2e_input_closed_stdin_yields_empty() {
    let file = write_input_prog("closed.zz");
    let (exit, stdout, stderr) = run_zz_with_stdin(&file, Stdio::null());
    assert_eq!(exit, 0, "exit {exit}. stderr: {stderr}");
    assert_eq!(stdout.trim_end(), "got:", "stdout: {stdout:?}");
}

#[test]
fn e2e_input_reads_piped_line() {
    use std::io::Write as _;

    let file = write_input_prog("piped.zz");
    let zz_bin = env!("CARGO_BIN_EXE_zz");
    let mut child = Command::new(zz_bin)
        .arg("run")
        .arg(&file)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .current_dir(Path::new(env!("CARGO_MANIFEST_DIR")).join("../.."))
        .spawn()
        .expect("failed to spawn zz run");
    child
        .stdin
        .as_mut()
        .unwrap()
        .write_all(b"hello\n")
        .expect("failed to write stdin");
    let output = child.wait_with_output().expect("failed to wait");

    let exit = output.status.code().unwrap_or(-1);
    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).to_string();
    assert_eq!(exit, 0, "exit {exit}. stderr: {stderr}");
    assert_eq!(stdout.trim_end(), "got:hello", "stdout: {stdout:?}");
}
