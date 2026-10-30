//! Embedded-C native resolution: ZZ spelling → C runtime symbol.
//!
//! This table is the AOT backend's single source of truth for which stdlib
//! natives have an implementation in the embedded C runtime. The checker's
//! signature registry is the other half of the lockstep: every key there
//! must resolve here (guard: `all_stdlib_funcs_have_c_impls` in zz_codegen)
//! or lower to an explicitly allow-listed gap.

/// Map a zz native qualified name to its C runtime implementation name.
pub fn native_impl(name: &str) -> Option<&'static str> {
    match name {
        // Builtin console I/O (no import required).
        "println" => Some("zz_io_println"),
        "print" => Some("zz_io_print"),
        "input" => Some("zz_io_input"),
        // Test assertions — success returns unit, failure aborts with
        // `error:` on stderr (mirrors the VM's EvalError).
        // Three spellings: bare (no import), `test.*` (via `import std.test`),
        // `std.test.*` (canonical).
        "assert" | "test.assert" | "std.test.assert" => Some("zz_assert"),
        "assert_eq" | "test.assert_eq" | "std.test.assert_eq" => Some("zz_assert_eq"),
        "assert_ne" | "test.assert_ne" | "std.test.assert_ne" => Some("zz_assert_ne"),
        "assert_approx_eq" | "test.assert_approx_eq" | "std.test.assert_approx_eq" => {
            Some("zz_assert_approx_eq")
        }
        "fail" | "panic" | "test.fail" | "test.panic" | "std.test.fail" | "std.test.panic" => {
            Some("zz_fail")
        }
        "assert_contains" | "test.assert_contains" | "std.test.assert_contains" => {
            Some("zz_assert_contains")
        }
        "assert_starts_with" | "test.assert_starts_with" | "std.test.assert_starts_with" => {
            Some("zz_assert_starts_with")
        }
        "assert_ends_with" | "test.assert_ends_with" | "std.test.assert_ends_with" => {
            Some("zz_assert_ends_with")
        }
        "assert_len" | "test.assert_len" | "std.test.assert_len" => Some("zz_assert_len"),
        "assert_gt" | "test.assert_gt" | "std.test.assert_gt" => Some("zz_assert_gt"),
        "assert_lt" | "test.assert_lt" | "std.test.assert_lt" => Some("zz_assert_lt"),
        "assert_is_some" | "test.assert_is_some" | "std.test.assert_is_some" => {
            Some("zz_assert_is_some")
        }
        "assert_is_none" | "test.assert_is_none" | "std.test.assert_is_none" => {
            Some("zz_assert_is_none")
        }
        "assert_is_ok" | "test.assert_is_ok" | "std.test.assert_is_ok" => Some("zz_assert_is_ok"),
        "assert_is_err" | "test.assert_is_err" | "std.test.assert_is_err" => {
            Some("zz_assert_is_err")
        }
        "temp_dir" | "test.temp_dir" | "std.test.temp_dir" => Some("zz_test_temp_dir"),
        "free_port" | "test.free_port" | "std.test.free_port" => Some("zz_test_free_port"),
        "assert_snapshot" | "test.assert_snapshot" | "std.test.assert_snapshot" => {
            Some("zz_assert_snapshot")
        }
        "black_box" | "test.black_box" | "std.test.black_box" => Some("zz_black_box"),
        // Debug print preserving Option wrappers; returns its argument.
        "dbg" => Some("zz_dbg"),
        "len" => Some("zz_len"),
        "ord" | "str.ord" | "std.str.ord" => Some("zz_ord"),
        "chr" | "str.chr" | "std.str.chr" => Some("zz_chr"),
        "map" | "vec.map" | "std.vec.map" => Some("zz_iter_map"),
        "filter" | "vec.filter" | "std.vec.filter" => Some("zz_iter_filter"),
        "enumerate" | "vec.enumerate" | "std.vec.enumerate" => Some("zz_iter_enumerate"),
        "zip" | "vec.zip" | "std.vec.zip" => Some("zz_iter_zip"),
        "clone" => Some("zz_clone_deep"),
        "range" => Some("zz_range3"),
        "typeof" => Some("zz_typeof"),
        "int" => Some("zz_int_cast"),
        "float" => Some("zz_float_cast"),
        "bool" => Some("zz_bool_cast"),
        "str" => Some("zz_str_cast"),
        // vec methods — bare names for method dispatch
        "vec.len" | "std.vec.len" | "vec_len" => Some("zz_vec_len"),
        "bytes.len" | "std.bytes.len" => Some("zz_len"),
        // `vec.append` was removed — canonical spelling is `vec.push`.
        // (Mapping deleted; the checker errors with a rename hint.)
        "vec.push" | "std.vec.push" => Some("zz_vec_push"),
        "vec.pop" | "std.vec.pop" => Some("zz_vec_pop"),
        "vec.remove" | "std.vec.remove" => Some("zz_vec_remove"),
        "vec.insert" | "std.vec.insert" => Some("zz_vec_insert"),
        "vec.join" | "std.vec.join" => Some("zz_str_join"), // join is a str function called on arrays
        "vec.contains" | "std.vec.contains" => Some("zz_vec_contains"),
        "vec.sort" | "std.vec.sort" => Some("zz_vec_sort"),
        "vec.sort_by" | "std.vec.sort_by" => Some("zz_vec_sort_by"),
        "vec.sort_by_desc" | "std.vec.sort_by_desc" => Some("zz_vec_sort_by_desc"),
        "vec.sort_key" | "std.vec.sort_key" => Some("zz_vec_sort_key"),
        "vec.reverse" | "std.vec.reverse" => Some("zz_vec_reverse"),
        // str
        "str.length" | "std.str.length" => Some("zz_str_length"),
        "str.to_lower" | "std.str.to_lower" | "str.lower" | "std.str.lower" => Some("zz_str_lower"),
        "str.to_upper" | "std.str.to_upper" | "str.upper" | "std.str.upper" => Some("zz_str_upper"),
        "str.replace" | "std.str.replace" => Some("zz_str_replace"),
        "str.count" | "std.str.count" => Some("zz_str_count"),
        "str.contains" | "std.str.contains" => Some("zz_str_contains"),
        "str.starts_with" | "std.str.starts_with" | "str.startswith" | "std.str.startswith" => {
            Some("zz_str_startswith")
        }
        "str.ends_with" | "std.str.ends_with" | "str.endswith" | "std.str.endswith" => {
            Some("zz_str_endswith")
        }
        "str.trim" | "std.str.trim" => Some("zz_str_trim"),
        "str.trim_start" | "std.str.trim_start" => Some("zz_str_trim_start"),
        "str.trim_end" | "std.str.trim_end" => Some("zz_str_trim_end"),
        "str.join" | "std.str.join" => Some("zz_str_join"),
        "str.split" | "std.str.split" => Some("zz_str_split"),
        "str.find" | "std.str.find" => Some("zz_str_find"),
        "str.rfind" | "std.str.rfind" => Some("zz_str_rfind"),
        "str.starts_with_at" | "std.str.starts_with_at" => Some("zz_str_starts_with_at"),
        "str.ends_with_at" | "std.str.ends_with_at" => Some("zz_str_ends_with_at"),
        "str.trim_span" | "std.str.trim_span" => Some("zz_str_trim_span"),
        "str.find_in" | "std.str.find_in" => Some("zz_str_find_in"),
        "str.rfind_in" | "std.str.rfind_in" => Some("zz_str_rfind_in"),
        "str.count_in" | "std.str.count_in" => Some("zz_str_count_in"),
        "str.classify" | "std.str.classify" => Some("zz_str_classify"),
        "str.bytes" | "std.str.bytes" => Some("zz_str_bytes"),
        "str.byte_len" | "std.str.byte_len" => Some("zz_str_byte_len"),
        "bytes.to_str" | "std.bytes.to_str" => Some("zz_bytes_to_str"),
        "bytes.to_ints" | "std.bytes.to_ints" => Some("zz_bytes_to_ints"),
        "cell.new" | "std.cell.new" => Some("zz_cell_new"),
        "cell.get" | "std.cell.get" => Some("zz_cell_get"),
        "cell.set" | "std.cell.set" => Some("zz_cell_set"),
        "cell.drop" | "std.cell.drop" => Some("zz_cell_drop"),
        "bytes.finish" | "std.bytes.finish" => Some("zz_bytes_finish"),
        // math
        "math.abs" | "std.math.abs" => Some("zz_math_abs"),
        "math.sqrt" | "std.math.sqrt" => Some("zz_math_sqrt"),
        "math.pow" | "std.math.pow" => Some("zz_math_pow"),
        "math.floor" | "std.math.floor" => Some("zz_math_floor"),
        "math.ceil" | "std.math.ceil" => Some("zz_math_ceil"),
        "math.round" | "std.math.round" => Some("zz_math_round"),
        "math.trunc" | "std.math.trunc" => Some("zz_math_trunc"),
        "math.signum" | "std.math.signum" => Some("zz_math_signum"),
        "math.hypot" | "std.math.hypot" => Some("zz_math_hypot"),
        "math.clamp" | "std.math.clamp" => Some("zz_math_clamp"),
        "math.root" | "std.math.root" => Some("zz_math_root"),
        "math.factorial" | "std.math.factorial" => Some("zz_math_factorial"),
        "math.gcd" | "std.math.gcd" => Some("zz_math_gcd"),
        "math.lcm" | "std.math.lcm" => Some("zz_math_lcm"),
        "math.sin" | "std.math.sin" => Some("zz_math_sin"),
        "math.cos" | "std.math.cos" => Some("zz_math_cos"),
        "math.tan" | "std.math.tan" => Some("zz_math_tan"),
        "math.asin" | "std.math.asin" => Some("zz_math_asin"),
        "math.acos" | "std.math.acos" => Some("zz_math_acos"),
        "math.atan" | "std.math.atan" => Some("zz_math_atan"),
        "math.sin_deg" | "std.math.sin_deg" => Some("zz_math_sin_deg"),
        "math.cos_deg" | "std.math.cos_deg" => Some("zz_math_cos_deg"),
        "math.tan_deg" | "std.math.tan_deg" => Some("zz_math_tan_deg"),
        "math.to_radians" | "std.math.to_radians" => Some("zz_math_to_radians"),
        "math.to_degrees" | "std.math.to_degrees" => Some("zz_math_to_degrees"),
        "math.log" | "std.math.log" => Some("zz_math_log"),
        "math.log10" | "std.math.log10" => Some("zz_math_log10"),
        "math.exp" | "std.math.exp" => Some("zz_math_exp"),
        "math.random" | "std.math.random" => Some("zz_math_random"),
        "math.is_nan" | "std.math.is_nan" => Some("zz_math_is_nan"),
        "math.is_inf" | "std.math.is_inf" => Some("zz_math_is_inf"),
        "math.PI" | "std.math.PI" => Some("zz_math_pi"),
        "math.E" | "std.math.E" => Some("zz_math_e"),
        "math.TAU" | "std.math.TAU" => Some("zz_math_tau"),
        "math.INF" | "std.math.INF" => Some("zz_math_inf"),
        "math.NAN" | "std.math.NAN" => Some("zz_math_nan"),
        "math.isqrt" | "std.math.isqrt" => Some("zz_math_isqrt"),
        "math.mean" | "std.math.mean" => Some("zz_math_mean"),
        "math.median" | "std.math.median" => Some("zz_math_median"),
        "math.rand_range" | "std.math.rand_range" => Some("zz_math_rand_range"),
        "math.dot_product" | "std.math.dot_product" => Some("zz_math_dot_product"),
        "math.magnitude" | "std.math.magnitude" => Some("zz_math_magnitude"),
        // json
        "json.parse" | "std.json.parse" => Some("zz_json_parse"),
        "json.stringify" | "std.json.stringify" => Some("zz_json_stringify"),
        "json.null" | "std.json.null" => Some("zz_json_null"),
        "json.get" | "std.json.get" => Some("zz_json_get"),
        "json.as_str" | "std.json.as_str" => Some("zz_json_as_str"),
        "json.as_int" | "std.json.as_int" => Some("zz_json_as_int"),
        "json.as_float" | "std.json.as_float" => Some("zz_json_as_float"),
        "json.as_bool" | "std.json.as_bool" => Some("zz_json_as_bool"),
        "json.type" | "std.json.type" => Some("zz_json_type"),
        "json.len" | "std.json.len" => Some("zz_json_len"),
        "json.keys" | "std.json.keys" => Some("zz_json_keys"),
        "json.has" | "std.json.has" => Some("zz_json_has"),
        "json.pretty" | "std.json.pretty" => Some("zz_json_pretty"),
        "json.merge" | "std.json.merge" => Some("zz_json_merge"),
        "json.deep_get" | "std.json.deep_get" => Some("zz_json_deep_get"),
        "json.array_push" | "std.json.array_push" => Some("zz_json_array_push"),
        // env
        "env.get" | "std.env.get" | "envmod.get" | "std.envmod.get" => Some("zz_env_get"),
        "env.var" | "std.env.var" | "envmod.var" | "std.envmod.var" => Some("zz_env_var"),
        "env.set" | "std.env.set" => Some("zz_env_set"),
        "env.remove" | "std.env.remove" | "env.unset" | "std.env.unset" => Some("zz_env_remove"),
        "env.vars" | "std.env.vars" => Some("zz_env_vars"),
        "env.cwd" | "std.env.cwd" => Some("zz_env_cwd"),
        "env.set_cwd" | "std.env.set_cwd" => Some("zz_env_set_cwd"),
        "env.exe_path" | "std.env.exe_path" => Some("zz_env_exe_path"),
        "env.home_dir" | "std.env.home_dir" => Some("zz_env_home_dir"),
        "env.temp_dir" | "std.env.temp_dir" => Some("zz_env_temp_dir"),
        "env.user" | "std.env.user" => Some("zz_env_user"),
        "env.os" | "std.env.os" => Some("zz_env_os"),
        "env.arch" | "std.env.arch" => Some("zz_env_arch"),
        "env.args" | "std.env.args" | "envmod.args" | "std.envmod.args" => Some("zz_env_args"),
        // dict
        "dict.len" => Some("zz_dict_len_val"),
        "dict.keys" => Some("zz_dict_keys"),
        "dict.has" => Some("zz_dict_has"),
        // option / result
        "option.expect" | "std.option.expect" => Some("zz_option_expect"),
        "result.expect" | "std.result.expect" => Some("zz_result_expect"),
        "option.unwrap" | "std.option.unwrap" => Some("zz_option_unwrap"),
        "option.unwrap_or" | "std.option.unwrap_or" => Some("zz_option_unwrap_or"),
        "result.unwrap" | "std.result.unwrap" => Some("zz_result_unwrap"),
        "result.unwrap_or" | "std.result.unwrap_or" => Some("zz_result_unwrap_or"),
        // fs — comprehensive non-blocking filesystem (C runtime in
        // core.c; unified `fs:<op>:<code>` diagnostics match the VM).
        "fs.read" | "std.fs.read" => Some("zz_fs_read"),
        "fs.read_file" | "std.fs.read_file" => Some("zz_fs_read"),
        "fs.read_to_string" | "std.fs.read_to_string" => Some("zz_fs_read"),
        "fs.read_bytes" | "std.fs.read_bytes" => Some("zz_fs_read_bytes"),
        "fs.write" | "std.fs.write" => Some("zz_fs_write"),
        "fs.write_file" | "std.fs.write_file" => Some("zz_fs_write"),
        "fs.append" | "std.fs.append" => Some("zz_fs_append"),
        "fs.copy" | "std.fs.copy" => Some("zz_fs_copy"),
        "fs.move" | "std.fs.move" => Some("zz_fs_move"),
        "fs.rename" | "std.fs.rename" => Some("zz_fs_move"),
        "fs.exists" | "std.fs.exists" => Some("zz_fs_exists"),
        "fs.is_file" | "std.fs.is_file" => Some("zz_fs_is_file"),
        "fs.is_dir" | "std.fs.is_dir" => Some("zz_fs_is_dir"),
        "fs.remove" | "std.fs.remove" | "fs.remove_file" | "std.fs.remove_file" => {
            Some("zz_fs_remove")
        }
        "fs.mkdir" | "std.fs.mkdir" => Some("zz_fs_mkdir"),
        "fs.mkdir_all" | "std.fs.mkdir_all" => Some("zz_fs_mkdir_all"),
        "fs.readdir" | "std.fs.readdir" => Some("zz_fs_readdir"),
        "fs.read_dir" | "std.fs.read_dir" => Some("zz_fs_read_dir"),
        "fs.remove_dir_all" | "std.fs.remove_dir_all" => Some("zz_fs_remove_dir_all"),
        "fs.walk_dir" | "std.fs.walk_dir" => Some("zz_fs_walk_dir"),
        "fs.stat" | "std.fs.stat" => Some("zz_fs_stat"),
        "fs.scan_counts" | "std.fs.scan_counts" => Some("zz_fs_scan_counts"),
        "fs.is_generated" | "std.fs.is_generated" => Some("zz_fs_is_generated"),
        "fs.open" | "std.fs.open" | "File.open" => Some("zz_fs_open"),
        "fs.read_chunk" | "std.fs.read_chunk" | "file.read_chunk" => Some("zz_fs_read_chunk"),
        "fs.read_chunk_bytes" | "std.fs.read_chunk_bytes" | "file.read_chunk_bytes" => {
            Some("zz_fs_read_chunk_bytes")
        }
        "fs.write_chunk" | "std.fs.write_chunk" | "file.write_chunk" => Some("zz_fs_write_chunk"),
        "fs.seek" | "std.fs.seek" | "file.seek" => Some("zz_fs_seek"),
        "fs.flush" | "std.fs.flush" | "file.flush" => Some("zz_fs_flush"),
        "fs.close" | "std.fs.close" | "file.close" => Some("zz_fs_close"),
        "fs.normalize" | "std.fs.normalize" => Some("zz_fs_normalize"),
        "fs.join" | "std.fs.join" => Some("zz_fs_join"),
        "fs.basename" | "std.fs.basename" => Some("zz_fs_basename"),
        "fs.dirname" | "std.fs.dirname" => Some("zz_fs_dirname"),
        "fs.is_absolute" | "std.fs.is_absolute" => Some("zz_fs_is_absolute"),
        "fs.extension" | "std.fs.extension" => Some("zz_fs_extension"),
        "fs.osfs" | "std.fs.osfs" => Some("zz_fs_osfs"),
        "fs.memfs" | "std.fs.memfs" => Some("zz_fs_memfs"),
        "fs.tarfs" | "std.fs.tarfs" => Some("zz_fs_tarfs"),
        "fs.embedfs" | "std.fs.embedfs" => Some("zz_fs_embedfs"),
        "fs.read_to_string_at" | "std.fs.read_to_string_at" => Some("zz_fs_read_to_string_at"),
        "fs.read_bytes_at" | "std.fs.read_bytes_at" => Some("zz_fs_read_bytes_at"),
        "fs.write_at" | "std.fs.write_at" => Some("zz_fs_write_at"),
        "fs.append_at" | "std.fs.append_at" => Some("zz_fs_append_at"),
        "fs.exists_at" | "std.fs.exists_at" => Some("zz_fs_exists_at"),
        "fs.is_file_at" | "std.fs.is_file_at" => Some("zz_fs_is_file_at"),
        "fs.is_dir_at" | "std.fs.is_dir_at" => Some("zz_fs_is_dir_at"),
        "fs.read_dir_at" | "std.fs.read_dir_at" => Some("zz_fs_read_dir_at"),
        "fs.mkdir_all_at" | "std.fs.mkdir_all_at" => Some("zz_fs_mkdir_all_at"),
        "fs.remove_file_at" | "std.fs.remove_file_at" => Some("zz_fs_remove_file_at"),
        // encoding
        "encoding.url_encode" | "std.encoding.url_encode" => Some("zz_encoding_url_encode"),
        "encoding.url_decode" | "std.encoding.url_decode" => Some("zz_encoding_url_decode"),
        "encoding.base64_encode" | "std.encoding.base64_encode" => {
            Some("zz_encoding_base64_encode")
        }
        "encoding.base64_decode" | "std.encoding.base64_decode" => {
            Some("zz_encoding_base64_decode")
        }
        "encoding.hex_encode" | "std.encoding.hex_encode" => Some("zz_encoding_hex_encode"),
        "encoding.hex_decode" | "std.encoding.hex_decode" => Some("zz_encoding_hex_decode"),
        // time
        "time.now_ms" | "std.time.now_ms" => Some("zz_time_now_ms"),
        "time.sleep_ms" | "std.time.sleep_ms" => Some("zz_time_sleep_ms"),
        // net tcp
        "net.tcp_connect" | "std.net.tcp_connect" => Some("zz_tcp_connect"),
        "net.tcp_listen" | "std.net.tcp_listen" => Some("zz_tcp_listen"),
        "net.tcp_accept" | "std.net.tcp_accept" => Some("zz_tcp_accept"),
        "net.tcp_write" | "std.net.tcp_write" => Some("zz_tcp_write"),
        "net.tcp_read" | "std.net.tcp_read" => Some("zz_tcp_read"),
        "net.tcp_readline" | "std.net.tcp_readline" => Some("zz_tcp_readline"),
        "net.tcp_read_bytes" | "std.net.tcp_read_bytes" => Some("zz_tcp_read_bytes"),
        "net.tcp_read_exact" | "std.net.tcp_read_exact" => Some("zz_tcp_read_exact"),
        "net.tcp_write_bytes" | "std.net.tcp_write_bytes" => Some("zz_tcp_write_bytes"),
        "net.tcp_shutdown" | "std.net.tcp_shutdown" => Some("zz_tcp_shutdown"),
        // Ergonomic `net.*` method aliases (same C impls as the canonicals).
        "net.accept" | "std.net.accept" => Some("zz_tcp_accept"),
        "net.read" | "std.net.read" => Some("zz_tcp_read"),
        "net.read_line" | "std.net.read_line" => Some("zz_tcp_readline"),
        "net.read_bytes" | "std.net.read_bytes" => Some("zz_tcp_read_bytes"),
        "net.read_exact" | "std.net.read_exact" => Some("zz_tcp_read_exact"),
        "net.write" | "std.net.write" => Some("zz_tcp_write"),
        "net.write_bytes" | "std.net.write_bytes" => Some("zz_tcp_write_bytes"),
        "net.shutdown" | "std.net.shutdown" => Some("zz_tcp_shutdown"),
        "net.peer_addr" | "std.net.peer_addr" => Some("zz_tcp_peer_addr"),
        "net.local_addr" | "std.net.local_addr" => Some("zz_tcp_local_addr"),
        "net.set_read_timeout" | "std.net.set_read_timeout" => Some("zz_tcp_set_read_timeout"),
        "net.set_write_timeout" | "std.net.set_write_timeout" => Some("zz_tcp_set_write_timeout"),
        // sqlz (AOT: sqlite3 prepared-statement C impl + postgres via
        // the staticlib; the C handle records the backend).
        // Canonical `sqlz.*` / `std.sqlz.*` (`db` was removed);
        // `pg.*` / `std.sqlz.postgres.*` are the explicit PG spellings
        // (`pg` only exists as an import alias of the nested module).
        "sqlz.open" | "std.sqlz.open" => Some("zz_db_open"),
        "pg.connect" | "std.sqlz.postgres.connect" => Some("zz_pg_connect"),
        "sqlz.exec" | "std.sqlz.exec" => Some("zz_db_exec"),
        "pg.exec" | "std.sqlz.postgres.exec" => Some("zz_db_exec"),
        "sqlz.query" | "std.sqlz.query" => Some("zz_db_query"),
        "pg.query" | "std.sqlz.postgres.query" => Some("zz_db_query"),
        "sqlz.close" | "std.sqlz.close" => Some("zz_db_close"),
        "pg.close" | "std.sqlz.postgres.close" => Some("zz_db_close"),
        // channels
        "chan" | "std.chan" => Some("zz_chan_new"),
        "chan.send" | "std.chan.send" => Some("zz_chan_send"),
        "chan.recv" | "std.chan.recv" => Some("zz_chan_recv"),
        "chan.try_recv" | "std.chan.try_recv" => Some("zz_chan_try_recv"),
        // spawn / task join
        "spawn" | "std.spawn" | "task.spawn" | "std.task.spawn" => Some("zz_spawn"),
        "task.recv" | "std.task.recv" | "task.join" | "std.task.join" => Some("zz_task_join_recv"),
        "task.try_join" | "std.task.try_join" => Some("zz_task_try_join"),
        // http (AOT: route table with closure handlers; http.test
        // dispatches in-process; epoll workers serve static OK until Step 2)
        "http.server" | "std.http.server" => Some("zz_http_server"),
        "http.route_get" | "std.http.route_get" => Some("zz_http_route_get"),
        "http.route_post" | "std.http.route_post" => Some("zz_http_route_post"),
        "http.route_put" | "std.http.route_put" => Some("zz_http_route_put"),
        "http.route_delete" | "std.http.route_delete" => Some("zz_http_route_delete"),
        "http.route" | "std.http.route" => Some("zz_http_route"),
        "http.log" | "std.http.log" => Some("zz_http_log"),
        "http.pipe" | "std.http.pipe" => Some("zz_http_pipe"),
        "http.pipe_post" | "std.http.pipe_post" => Some("zz_http_pipe_post"),
        "http.with_headers" | "std.http.with_headers" => Some("zz_http_with_headers"),
        "http.rate_limit" | "std.http.rate_limit" => Some("zz_http_rate_limit"),
        "http.listen" | "std.http.listen" => Some("zz_http_listen"),
        "http.listen_cfg" | "std.http.listen_cfg" => Some("zz_http_listen_cfg"),
        "http.serve_dir" | "std.http.serve_dir" => Some("zz_http_serve_dir"),
        "http.serve_dir_at" | "std.http.serve_dir_at" => Some("zz_http_serve_dir_at"),
        "http.test" | "std.http.test" => Some("zz_http_test"),
        "http.test_req" | "std.http.test_req" => Some("zz_http_test_req"),
        "http.handle" | "std.http.handle" => Some("zz_http_handle"),
        "http.respond" | "std.http.respond" => Some("zz_http_respond"),
        "http.param" | "std.http.param" => Some("zz_http_param"),
        "http.query" | "std.http.query" => Some("zz_http_query"),
        "http.header" | "std.http.header" => Some("zz_http_header"),
        "http.body_json" | "std.http.body_json" => Some("zz_http_body_json"),
        "http.body_form" | "std.http.body_form" => Some("zz_http_body_form"),
        "http.body_bytes" | "std.http.body_bytes" => Some("zz_http_body_bytes"),
        // http Response methods
        "http.status" | "std.http.status" => Some("zz_http_response_status"),
        "http.text" | "std.http.text" => Some("zz_http_response_text"),
        "http.json" | "std.http.json" => Some("zz_http_response_json"),
        "http.headers" | "std.http.headers" => Some("zz_http_response_headers"),
        // http request functions
        "http.get" | "std.http.get" => Some("zz_http_get"),
        "http.post" | "std.http.post" => Some("zz_http_post"),
        "http.fetch" | "std.http.fetch" => Some("zz_http_fetch"),
        "http.post_json" | "std.http.post_json" => Some("zz_http_post_json"),
        _ => None,
    }
}

/// Whether a reachable native has a callable implementation.
///
/// Embedded-C natives resolve through [`native_impl`]; Rust-staticlib
/// natives (regex, crypto, …) through [`crate::ffi_impl`]. Anything else
/// lowers to unit (documented MVP limitation).
pub fn native_supported(name: &str) -> bool {
    native_impl(name).is_some() || crate::ffi_impl(name).is_some()
}
