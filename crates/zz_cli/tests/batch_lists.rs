//! Shared batch coverage lists (single source of truth).
//!
//! Included as a module by both `batched_parity` (which runs the
//! batches) and `dual_engine_parity` (which skips individual native
//! legs for covered fixtures when `ZZ_BATCH_NATIVE=1`). The inventory
//! test in `batched_parity` enforces eligible ∪ excluded == strict set.

/// (category, file, has_main). `has_main` fixtures get `pub` injected on
/// `func main` in the batch copy and an explicit `mod.main()` call.
pub const ELIGIBLE: &[(&str, &str, bool)] = &[
    ("syntax", "declarations.zz", false),
    ("syntax", "pipelines.zz", false),
    ("syntax", "operators.zz", false),
    ("syntax", "bitwise_ops.zz", false),
    ("syntax", "tuple_ops.zz", false),
    ("syntax", "tuple_unboxed_struct.zz", true),
    ("syntax", "compound_assign.zz", true),
    ("syntax", "generic_structs.zz", true),
    ("syntax", "fstrings.zz", false),
    ("syntax", "dicts.zz", false),
    ("syntax", "string_blocks.zz", false),
    ("syntax", "pipe_elvis.zz", false),
    ("syntax", "scalar_copy.zz", false),
    ("syntax", "elif_chain.zz", true),
    ("syntax", "top_level_elif.zz", false),
    ("syntax", "chained_calls.zz", true),
    ("syntax", "range_var_bounds.zz", false),
    ("syntax", "hex_escape_bounds.zz", false),
    ("syntax", "struct_scalar_fields.zz", true),
    ("syntax", "method_free_fn.zz", true),
    ("syntax", "method_chain_recv.zz", true),
    ("syntax", "for_annotated_decl.zz", false),
    ("modules", "diamond_import.zz", true),
    ("modules", "multi_level_pub.zz", true),
    ("modules", "private_struct_field_access.zz", true),
    ("modules", "pub_access.zz", true),
    ("modules", "pub_no_unused_warning.zz", true),
    ("modules", "pub_reexport_alias.zz", true),
    ("modules", "pub_struct_fields.zz", true),
    ("modules", "pub_struct_method.zz", true),
    ("modules", "reexports.zz", false),
    ("modules", "shadow_pub_var.zz", true),
    ("types", "generics.zz", false),
    ("types", "type_inference.zz", false),
    ("stdlib", "strings.zz", false),
    ("stdlib", "str_utf8_parity.zz", true),
    ("stdlib", "vec_nested_str_parity.zz", true),
    ("stdlib", "console.zz", false),
    ("stdlib", "str_extended_test.zz", false),
    ("stdlib", "str_find_test.zz", true),
    ("stdlib", "str_bytes_test.zz", true),
    ("stdlib", "str_ord_chr_test.zz", true),
    ("stdlib", "str_classify_test.zz", true),
    ("syntax", "match.zz", false),
    ("syntax", "match_assign.zz", true),
    ("syntax", "return_in_loops.zz", false),
    ("syntax", "control_flow.zz", false),
    ("types", "structs.zz", false),
    ("types", "aliases.zz", true),
    ("types", "enums.zz", true),
    ("types", "enum_generics.zz", true),
    ("types", "variants.zz", false),
    ("syntax", "empty_infer.zz", true),
    ("syntax", "functions.zz", false),
    ("syntax", "hof.zz", false),
    ("syntax", "arrays.zz", false),
    ("syntax", "defer.zz", false),
    ("syntax", "dict_iteration.zz", true),
    ("stdlib", "vectors.zz", false),
    ("stdlib", "enumerate_loop.zz", false),
    ("stdlib", "jsonmod.zz", false),
    ("stdlib", "json_test.zz", false),
    ("stdlib", "option_interpolation.zz", false),
    ("stdlib", "alias_module_calls.zz", false),
    ("stdlib", "selective_calls.zz", false),
    ("stdlib", "import_alias.zz", true),
    ("syntax", "scope_collision.zz", false),
    ("stdlib", "builders.zz", true),
    ("stdlib", "csv_test.zz", true),
    ("stdlib", "dec_ops.zz", true),
    ("stdlib", "map_set.zz", true),
    ("stdlib", "math_ops.zz", false),
    ("stdlib", "path_join.zz", true),
    ("stdlib", "time_date.zz", true),
    ("stdlib", "math_consts.zz", false),
    ("stdlib", "fs_path.zz", true),
    ("stdlib", "bytes.zz", true),
    ("syntax", "brace_escapes.zz", false),
    ("syntax", "brace_escapes_multiline.zz", false),
    ("syntax", "destructuring.zz", false),
    ("syntax", "short_circuit.zz", true),
    ("stdlib", "json_extended_test.zz", false),
];

/// (category/file, reason). Everything strict outside regression/errors
/// that is not batchable must name its reason here.
/// (Only read by the `batched_parity` inventory test; unused in the
/// `dual_engine_parity` target that shares this file.)
#[allow(dead_code)]
pub const EXCLUDED: &[(&str, &str)] = &[
    ("stdlib/envmod.zz", "reads env/argv (shared in batch)"),
    ("stdlib/env_test.zz", "reads env/argv (shared in batch)"),
    ("stdlib/env_full.zz", "reads env/argv (shared in batch)"),
    ("stdlib/concurrency_spawn_test.zz", "spawn/timing-sensitive"),
    ("stdlib/channel_test.zz", "spawn/timing-sensitive"),
    ("stdlib/concurrency_tasks_test.zz", "spawn/timing-sensitive"),
    (
        "stdlib/concurrency_stress_test.zz",
        "spawn/timing-sensitive",
    ),
    (
        "stdlib/concurrency_try_join_test.zz",
        "spawn/timing-sensitive",
    ),
    (
        "stdlib/concurrency_capture_test.zz",
        "spawn/timing-sensitive",
    ),
    (
        "stdlib/concurrency_recall_test.zz",
        "spawn/timing-sensitive",
    ),
    ("stdlib/fs_comprehensive.zz", "spawn/timing-sensitive"),
    ("stdlib/net_tcp_test.zz", "binds ports (collide in batch)"),
    ("stdlib/input_chained.zz", "needs .stdin (shared in batch)"),
    (
        "stdlib/http_request_response.zz",
        "binds ports (collide in batch)",
    ),
    ("stdlib/http_fetch_test.zz", "network access"),
    // Scratch-filesystem fixtures keyed by sweep token: shared batch
    // processes would collide on (and pollute from) one scratch dir.
    ("stdlib/filesystem.zz", "sweep-token scratch fs"),
    ("stdlib/fs_test.zz", "sweep-token scratch fs"),
    // Needs the helpers/ support dir sibling (batch sandbox is flat).
    ("stdlib/generic_selective.zz", "needs helpers/ support dir"),
    ("stdlib/result_print.zz", "sweep-token scratch fs"),
    ("stdlib/fs_vfs.zz", "sweep-token scratch fs"),
    // `alias_import` and `enum_import` claim the same `shapes` alias
    // for different support modules: a genuine per-program collision,
    // so they run individually (each passes alone).
    (
        "types/alias_import.zz",
        "alias `shapes` collides with enum_import's support module",
    ),
    (
        "types/enum_import.zz",
        "alias `shapes` collides with alias_import's support module",
    ),
];
