# Stdlib reference — generated

Exhaustive name index: **465 functions across 34 modules**.
Generated from `crates/zz_stdlib/src/funcs.rs` — do not hand-edit.
Regenerate with `python3 scripts/gen-stdlib-ref.py`.
Signatures: see `funcs.rs`; semantics: per-module guides linked below.

## `std.args` — 14 fns ([guide](env-process.md))

| Function |
|---|
| `std.args.bool_flag` |
| `std.args.error` |
| `std.args.get_bool` |
| `std.args.get_int` |
| `std.args.get_raw` |
| `std.args.get_str` |
| `std.args.help` |
| `std.args.int_flag` |
| `std.args.parse` |
| `std.args.parser` |
| `std.args.positional` |
| `std.args.str_flag` |
| `std.args.subcommand` |
| `std.args.was_help` |

## `std.bytes` — 6 fns ([guide](str-vec.md))

| Function |
|---|
| `std.bytes.builder` |
| `std.bytes.finish` |
| `std.bytes.len` |
| `std.bytes.push_byte` |
| `std.bytes.to_ints` |
| `std.bytes.to_str` |

## `std.cell` — 4 fns ([guide](cell-path-sys.md))

| Function |
|---|
| `std.cell.drop` |
| `std.cell.get` |
| `std.cell.new` |
| `std.cell.set` |

## `std.chan` — 3 fns ([guide](concurrency.md))

| Function |
|---|
| `std.chan.recv` |
| `std.chan.send` |
| `std.chan.try_recv` |

## `std.colors` — 12 fns ([guide](terminal.md))

| Function |
|---|
| `std.colors.bg_256` |
| `std.colors.bg_bright_red` |
| `std.colors.bg_bright_white` |
| `std.colors.blink` |
| `std.colors.bold` |
| `std.colors.color256` |
| `std.colors.hex` |
| `std.colors.red` |
| `std.colors.reverse` |
| `std.colors.rgb` |
| `std.colors.strikethrough` |
| `std.colors.strip` |

## `std.crypto` — 20 fns ([guide](encoding-crypto.md))

| Function |
|---|
| `std.crypto.argon2_hash` |
| `std.crypto.argon2_verify` |
| `std.crypto.bcrypt_hash` |
| `std.crypto.bcrypt_verify` |
| `std.crypto.ct_eq` |
| `std.crypto.ed25519_keypair` |
| `std.crypto.ed25519_sign` |
| `std.crypto.ed25519_verify` |
| `std.crypto.hmac_sha256` |
| `std.crypto.jwt_decode` |
| `std.crypto.jwt_decode_ed` |
| `std.crypto.jwt_encode` |
| `std.crypto.jwt_encode_ed` |
| `std.crypto.random_bytes` |
| `std.crypto.rsa_keypair` |
| `std.crypto.rsa_sign` |
| `std.crypto.rsa_verify` |
| `std.crypto.sha256` |
| `std.crypto.sha256_bytes` |
| `std.crypto.sha512` |

## `std.csv` — 3 fns ([guide](data.md))

| Function |
|---|
| `std.csv.parse` |
| `std.csv.stringify` |
| `std.csv.to_json` |

## `std.dec` — 3 fns ([guide](data.md))

| Function |
|---|
| `std.dec.add` |
| `std.dec.cmp` |
| `std.dec.format` |

## `std.encoding` — 7 fns ([guide](encoding-crypto.md))

| Function |
|---|
| `std.encoding.base64_decode` |
| `std.encoding.base64_decode_bytes` |
| `std.encoding.base64_encode` |
| `std.encoding.hex_decode` |
| `std.encoding.hex_encode` |
| `std.encoding.url_decode` |
| `std.encoding.url_encode` |

## `std.env` — 15 fns ([guide](env-process.md))

| Function |
|---|
| `std.env.arch` |
| `std.env.args` |
| `std.env.cwd` |
| `std.env.exe_path` |
| `std.env.get` |
| `std.env.get_var` |
| `std.env.home_dir` |
| `std.env.os` |
| `std.env.remove` |
| `std.env.set` |
| `std.env.set_cwd` |
| `std.env.temp_dir` |
| `std.env.user` |
| `std.env.var` |
| `std.env.vars` |

## `std.fs` — 46 fns ([guide](fs-bytes.md))

| Function |
|---|
| `std.fs.append` |
| `std.fs.append_at` |
| `std.fs.basename` |
| `std.fs.close` |
| `std.fs.copy` |
| `std.fs.dirname` |
| `std.fs.embedfs` |
| `std.fs.exists` |
| `std.fs.exists_at` |
| `std.fs.extension` |
| `std.fs.flush` |
| `std.fs.is_absolute` |
| `std.fs.is_dir` |
| `std.fs.is_dir_at` |
| `std.fs.is_file` |
| `std.fs.is_file_at` |
| `std.fs.is_generated` |
| `std.fs.join` |
| `std.fs.memfs` |
| `std.fs.mkdir` |
| `std.fs.mkdir_all` |
| `std.fs.mkdir_all_at` |
| `std.fs.move` |
| `std.fs.normalize` |
| `std.fs.open` |
| `std.fs.osfs` |
| `std.fs.read` |
| `std.fs.read_bytes` |
| `std.fs.read_bytes_at` |
| `std.fs.read_chunk` |
| `std.fs.read_chunk_bytes` |
| `std.fs.read_dir` |
| `std.fs.read_dir_at` |
| `std.fs.read_to_string_at` |
| `std.fs.remove` |
| `std.fs.remove_dir_all` |
| `std.fs.remove_file_at` |
| `std.fs.rename` |
| `std.fs.scan_counts` |
| `std.fs.seek` |
| `std.fs.stat` |
| `std.fs.tarfs` |
| `std.fs.walk_dir` |
| `std.fs.write` |
| `std.fs.write_at` |
| `std.fs.write_chunk` |

## `std.http` — 48 fns ([guide](http.md))

| Function |
|---|
| `std.http.body_bytes` |
| `std.http.body_form` |
| `std.http.body_json` |
| `std.http.cors` |
| `std.http.created` |
| `std.http.delete` |
| `std.http.fetch` |
| `std.http.fetch_insecure` |
| `std.http.get` |
| `std.http.handle` |
| `std.http.header` |
| `std.http.headers` |
| `std.http.hijack` |
| `std.http.json` |
| `std.http.listen` |
| `std.http.listen_cfg` |
| `std.http.listen_tls` |
| `std.http.listen_tls_cfg` |
| `std.http.log` |
| `std.http.not_found` |
| `std.http.ok` |
| `std.http.param` |
| `std.http.pipe` |
| `std.http.pipe_post` |
| `std.http.post` |
| `std.http.post_json` |
| `std.http.put` |
| `std.http.query` |
| `std.http.rate_limit` |
| `std.http.redirect` |
| `std.http.request_id` |
| `std.http.respond` |
| `std.http.route` |
| `std.http.route_delete` |
| `std.http.route_get` |
| `std.http.route_post` |
| `std.http.route_put` |
| `std.http.secure_header_dict` |
| `std.http.secure_headers` |
| `std.http.serve_dir` |
| `std.http.serve_dir_at` |
| `std.http.server` |
| `std.http.status` |
| `std.http.test` |
| `std.http.test_req` |
| `std.http.text` |
| `std.http.use` |
| `std.http.with_headers` |

## `std.json` — 28 fns ([guide](data.md))

| Function |
|---|
| `std.json.array_push` |
| `std.json.as_bool` |
| `std.json.as_float` |
| `std.json.as_int` |
| `std.json.as_str` |
| `std.json.deep_get` |
| `std.json.get` |
| `std.json.has` |
| `std.json.is_array` |
| `std.json.is_bool` |
| `std.json.is_empty` |
| `std.json.is_null` |
| `std.json.is_number` |
| `std.json.is_object` |
| `std.json.is_string` |
| `std.json.keys` |
| `std.json.len` |
| `std.json.merge` |
| `std.json.null` |
| `std.json.parse` |
| `std.json.parse_or` |
| `std.json.parse_or_null` |
| `std.json.path_exists` |
| `std.json.path_get_or` |
| `std.json.pretty` |
| `std.json.stringify` |
| `std.json.type` |
| `std.json.validate` |

## `std.log` — 11 fns ([guide](env-process.md))

| Function |
|---|
| `std.log.debug` |
| `std.log.error` |
| `std.log.get_level` |
| `std.log.info` |
| `std.log.set_format` |
| `std.log.set_level` |
| `std.log.span_begin` |
| `std.log.to_file` |
| `std.log.to_stderr` |
| `std.log.trace` |
| `std.log.warn` |

## `std.map` — 4 fns ([guide](data.md))

| Function |
|---|
| `std.map.get_or` |
| `std.map.has` |
| `std.map.keys` |
| `std.map.merge` |

## `std.math` — 51 fns ([guide](math-time.md))

| Function |
|---|
| `std.math.abs` |
| `std.math.acos` |
| `std.math.asin` |
| `std.math.atan` |
| `std.math.ceil` |
| `std.math.clamp` |
| `std.math.cos` |
| `std.math.cos_deg` |
| `std.math.count` |
| `std.math.dot_product` |
| `std.math.exp` |
| `std.math.factorial` |
| `std.math.floor` |
| `std.math.gcd` |
| `std.math.hypot` |
| `std.math.is_even` |
| `std.math.is_inf` |
| `std.math.is_nan` |
| `std.math.is_odd` |
| `std.math.isqrt` |
| `std.math.lcm` |
| `std.math.log` |
| `std.math.log10` |
| `std.math.magnitude` |
| `std.math.matrix_mul` |
| `std.math.max` |
| `std.math.max_arr` |
| `std.math.mean` |
| `std.math.mean_f` |
| `std.math.median` |
| `std.math.median_f` |
| `std.math.min` |
| `std.math.min_arr` |
| `std.math.pow` |
| `std.math.product` |
| `std.math.product_f` |
| `std.math.rand_range` |
| `std.math.random` |
| `std.math.root` |
| `std.math.round` |
| `std.math.signum` |
| `std.math.sin` |
| `std.math.sin_deg` |
| `std.math.sqrt` |
| `std.math.sum` |
| `std.math.sum_f` |
| `std.math.tan` |
| `std.math.tan_deg` |
| `std.math.to_degrees` |
| `std.math.to_radians` |
| `std.math.trunc` |

## `std.net` — 44 fns ([guide](net.md))

| Function |
|---|
| `std.net.accept` |
| `std.net.close` |
| `std.net.local_addr` |
| `std.net.peer_addr` |
| `std.net.read` |
| `std.net.read_bytes` |
| `std.net.read_line` |
| `std.net.set_read_timeout` |
| `std.net.set_write_timeout` |
| `std.net.shutdown` |
| `std.net.tcp_accept` |
| `std.net.tcp_connect` |
| `std.net.tcp_listen` |
| `std.net.tcp_read` |
| `std.net.tcp_read_bytes` |
| `std.net.tcp_read_exact` |
| `std.net.tcp_readline` |
| `std.net.tcp_shutdown` |
| `std.net.tcp_write` |
| `std.net.tcp_write_bytes` |
| `std.net.tls_accept` |
| `std.net.tls_connect` |
| `std.net.tls_connect_insecure` |
| `std.net.tls_connect_with_ca` |
| `std.net.tls_listen` |
| `std.net.tls_listener_close` |
| `std.net.tls_readline` |
| `std.net.tls_shutdown` |
| `std.net.unix_accept` |
| `std.net.unix_close` |
| `std.net.unix_connect` |
| `std.net.unix_listen` |
| `std.net.unix_listener_close` |
| `std.net.unix_read` |
| `std.net.unix_read_bytes` |
| `std.net.unix_read_exact` |
| `std.net.unix_readline` |
| `std.net.unix_set_read_timeout` |
| `std.net.unix_set_write_timeout` |
| `std.net.unix_shutdown` |
| `std.net.unix_write` |
| `std.net.unix_write_bytes` |
| `std.net.write` |
| `std.net.write_bytes` |

## `std.path` — 7 fns ([guide](cell-path-sys.md))

| Function |
|---|
| `std.path.basename` |
| `std.path.dirname` |
| `std.path.extension` |
| `std.path.is_absolute` |
| `std.path.join` |
| `std.path.join_all` |
| `std.path.normalize` |

## `std.process` — 6 fns ([guide](env-process.md))

| Function |
|---|
| `std.process.exit` |
| `std.process.pid` |
| `std.process.run` |
| `std.process.run_with_env` |
| `std.process.spawn` |
| `std.process.wait` |

## `std.property` — 4 fns ([guide](data.md))

| Function |
|---|
| `std.property.check` |
| `std.property.for_all_int` |
| `std.property.int_in` |
| `std.property.seed` |

## `std.regexp` — 6 fns ([guide](regexp.md))

| Function |
|---|
| `std.regexp.captures` |
| `std.regexp.compile` |
| `std.regexp.find` |
| `std.regexp.is_email` |
| `std.regexp.is_match` |
| `std.regexp.replace_all` |

## `std.set` — 3 fns ([guide](data.md))

| Function |
|---|
| `std.set.has` |
| `std.set.intersect` |
| `std.set.union` |

## `std.span` — 1 fns ([guide](overview.md))

| Function |
|---|
| `std.span.end` |

## `std.sqlz` — 13 fns ([guide](sql-db.md))

| Function |
|---|
| `std.sqlz.close` |
| `std.sqlz.exec` |
| `std.sqlz.mysql.close` |
| `std.sqlz.mysql.connect` |
| `std.sqlz.mysql.exec` |
| `std.sqlz.mysql.query` |
| `std.sqlz.open` |
| `std.sqlz.postgres.close` |
| `std.sqlz.postgres.connect` |
| `std.sqlz.postgres.exec` |
| `std.sqlz.postgres.query` |
| `std.sqlz.query` |
| `std.sqlz.transaction` |

## `std.str` — 33 fns ([guide](str-vec.md))

| Function |
|---|
| `std.str.builder` |
| `std.str.byte_len` |
| `std.str.bytes` |
| `std.str.chr` |
| `std.str.classify` |
| `std.str.contains` |
| `std.str.count` |
| `std.str.count_in` |
| `std.str.ends_with` |
| `std.str.ends_with_at` |
| `std.str.find` |
| `std.str.find_in` |
| `std.str.finish` |
| `std.str.is_empty` |
| `std.str.join` |
| `std.str.length` |
| `std.str.ord` |
| `std.str.pad_left` |
| `std.str.pad_right` |
| `std.str.repeat` |
| `std.str.replace` |
| `std.str.reverse` |
| `std.str.rfind` |
| `std.str.rfind_in` |
| `std.str.split` |
| `std.str.starts_with` |
| `std.str.starts_with_at` |
| `std.str.to_lower` |
| `std.str.to_upper` |
| `std.str.trim` |
| `std.str.trim_end` |
| `std.str.trim_span` |
| `std.str.trim_start` |

## `std.sys` — 6 fns ([guide](cell-path-sys.md))

| Function |
|---|
| `std.sys.arch` |
| `std.sys.avail_mem` |
| `std.sys.cpu_count` |
| `std.sys.hostname` |
| `std.sys.os` |
| `std.sys.total_mem` |

## `std.task` — 3 fns ([guide](concurrency.md))

| Function |
|---|
| `std.task.join` |
| `std.task.spawn` |
| `std.task.try_join` |

## `std.term` — 7 fns ([guide](terminal.md))

| Function |
|---|
| `std.term.disable_raw` |
| `std.term.enable_raw` |
| `std.term.flush` |
| `std.term.get_size` |
| `std.term.is_tty` |
| `std.term.poll` |
| `std.term.read_key` |

## `std.test` — 20 fns ([guide](testing-lib.md))

| Function |
|---|
| `std.test.assert` |
| `std.test.assert_approx_eq` |
| `std.test.assert_contains` |
| `std.test.assert_ends_with` |
| `std.test.assert_eq` |
| `std.test.assert_gt` |
| `std.test.assert_is_err` |
| `std.test.assert_is_none` |
| `std.test.assert_is_ok` |
| `std.test.assert_is_some` |
| `std.test.assert_len` |
| `std.test.assert_lt` |
| `std.test.assert_ne` |
| `std.test.assert_snapshot` |
| `std.test.assert_starts_with` |
| `std.test.black_box` |
| `std.test.fail` |
| `std.test.free_port` |
| `std.test.panic` |
| `std.test.temp_dir` |

## `std.time` — 9 fns ([guide](math-time.md))

| Function |
|---|
| `std.time.add_days` |
| `std.time.format_rfc3339` |
| `std.time.monotonic_nanos` |
| `std.time.now_micros` |
| `std.time.now_ms` |
| `std.time.now_nanos` |
| `std.time.parse_rfc3339` |
| `std.time.sleep_micros` |
| `std.time.sleep_ms` |

## `std.tls` — 1 fns ([guide](overview.md))

| Function |
|---|
| `std.tls.connect_with_ca` |

## `std.unix_listener` — 2 fns ([guide](overview.md))

| Function |
|---|
| `std.unix_listener.accept` |
| `std.unix_listener.close` |

## `std.uuid` — 4 fns ([guide](encoding-crypto.md))

| Function |
|---|
| `std.uuid.is_valid` |
| `std.uuid.parse` |
| `std.uuid.v4` |
| `std.uuid.v7` |

## `std.vec` — 21 fns ([guide](str-vec.md))

| Function |
|---|
| `std.vec.concat` |
| `std.vec.contains` |
| `std.vec.enumerate` |
| `std.vec.flatten` |
| `std.vec.fold` |
| `std.vec.index_of` |
| `std.vec.insert` |
| `std.vec.join` |
| `std.vec.last_index_of` |
| `std.vec.len` |
| `std.vec.max_val` |
| `std.vec.min_val` |
| `std.vec.pop` |
| `std.vec.product` |
| `std.vec.product_f` |
| `std.vec.push` |
| `std.vec.remove` |
| `std.vec.reverse` |
| `std.vec.sort` |
| `std.vec.sum` |
| `std.vec.sum_f` |

Total: 465 functions. Top-level built-ins: see [Built-ins](builtins.md).
