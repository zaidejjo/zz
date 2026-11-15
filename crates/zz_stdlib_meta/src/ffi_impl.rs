//! Rust-staticlib FFI resolution: ZZ spelling -> C symbol, plus the
//! matching C declarations for the generated prelude.
//!
//! Single source of truth (relocated from `zz_codegen_ffi::lib`). The
//! AOT crate re-exports both functions through a generated shim.

/// Map a zz native qualified name to its Rust-staticlib C symbol.
///
/// Populated per module as it lands (Phase 1: `std.regexp`). Both the
/// `std.<mod>.*` and bare `<mod>.*` spellings map to the same symbol,
/// mirroring the `native_impl` convention for embedded-C natives.
pub fn ffi_impl(name: &str) -> Option<&'static str> {
    match name {
        "regexp.compile" | "std.regexp.compile" => Some("zz_regexp_compile"),
        "encoding.base64_decode_bytes" | "std.encoding.base64_decode_bytes" => {
            Some("zz_encoding_base64_decode_bytes")
        }
        "regexp.is_match" | "std.regexp.is_match" => Some("zz_regexp_is_match"),
        "regexp.find" | "std.regexp.find" => Some("zz_regexp_find"),
        "regexp.replace_all" | "std.regexp.replace_all" => Some("zz_regexp_replace_all"),
        "regexp.captures" | "std.regexp.captures" => Some("zz_regexp_captures"),
        "crypto.sha256" | "std.crypto.sha256" => Some("zz_crypto_sha256"),
        "crypto.sha256_bytes" | "std.crypto.sha256_bytes" => Some("zz_crypto_sha256_bytes"),
        "crypto.sha512" | "std.crypto.sha512" => Some("zz_crypto_sha512"),
        "crypto.hmac_sha256" | "std.crypto.hmac_sha256" => Some("zz_crypto_hmac_sha256"),
        "crypto.random_bytes" | "std.crypto.random_bytes" => Some("zz_crypto_random_bytes"),
        "crypto.ct_eq" | "std.crypto.ct_eq" => Some("zz_crypto_ct_eq"),
        "crypto.argon2_hash" | "std.crypto.argon2_hash" => Some("zz_crypto_argon2_hash"),
        "crypto.argon2_verify" | "std.crypto.argon2_verify" => Some("zz_crypto_argon2_verify"),
        "crypto.bcrypt_hash" | "std.crypto.bcrypt_hash" => Some("zz_crypto_bcrypt_hash"),
        "crypto.bcrypt_verify" | "std.crypto.bcrypt_verify" => Some("zz_crypto_bcrypt_verify"),
        "crypto.ed25519_keypair" | "std.crypto.ed25519_keypair" => {
            Some("zz_crypto_ed25519_keypair")
        }
        "crypto.ed25519_sign" | "std.crypto.ed25519_sign" => Some("zz_crypto_ed25519_sign"),
        "crypto.ed25519_verify" | "std.crypto.ed25519_verify" => Some("zz_crypto_ed25519_verify"),
        "crypto.rsa_keypair" | "std.crypto.rsa_keypair" => Some("zz_crypto_rsa_keypair"),
        "crypto.rsa_sign" | "std.crypto.rsa_sign" => Some("zz_crypto_rsa_sign"),
        "crypto.rsa_verify" | "std.crypto.rsa_verify" => Some("zz_crypto_rsa_verify"),
        "crypto.jwt_encode" | "std.crypto.jwt_encode" => Some("zz_crypto_jwt_encode"),
        "crypto.jwt_decode" | "std.crypto.jwt_decode" => Some("zz_crypto_jwt_decode"),
        "crypto.jwt_encode_ed" | "std.crypto.jwt_encode_ed" => Some("zz_crypto_jwt_encode_ed"),
        "crypto.jwt_decode_ed" | "std.crypto.jwt_decode_ed" => Some("zz_crypto_jwt_decode_ed"),
        "time.now_nanos" | "std.time.now_nanos" => Some("zz_time_now_nanos"),
        "time.now_micros" | "std.time.now_micros" => Some("zz_time_now_micros"),
        "time.monotonic_nanos" | "std.time.monotonic_nanos" => Some("zz_time_monotonic_nanos"),
        "time.sleep_micros" | "std.time.sleep_micros" => Some("zz_time_sleep_micros"),
        "log.set_level" | "std.log.set_level" => Some("zz_log_set_level"),
        "log.get_level" | "std.log.get_level" => Some("zz_log_get_level"),
        "log.set_format" | "std.log.set_format" => Some("zz_log_set_format"),
        "log.to_file" | "std.log.to_file" => Some("zz_log_to_file"),
        "log.to_stderr" | "std.log.to_stderr" => Some("zz_log_to_stderr"),
        "log.trace" | "std.log.trace" => Some("zz_log_trace"),
        "log.debug" | "std.log.debug" => Some("zz_log_debug"),
        "log.info" | "std.log.info" => Some("zz_log_info"),
        "log.warn" | "std.log.warn" => Some("zz_log_warn"),
        "log.error" | "std.log.error" => Some("zz_log_error"),
        "log.span_begin" | "std.log.span_begin" => Some("zz_log_span_begin"),
        "span.end" | "std.span.end" => Some("zz_span_end"),
        "sys.os" | "std.sys.os" => Some("zz_sys_os"),
        "sys.arch" | "std.sys.arch" => Some("zz_sys_arch"),
        "sys.cpu_count" | "std.sys.cpu_count" => Some("zz_sys_cpu_count"),
        "sys.hostname" | "std.sys.hostname" => Some("zz_sys_hostname"),
        "sys.total_mem" | "std.sys.total_mem" => Some("zz_sys_total_mem"),
        "sys.avail_mem" | "std.sys.avail_mem" => Some("zz_sys_avail_mem"),
        "term.enable_raw" | "std.term.enable_raw" => Some("zz_term_enable_raw"),
        "term.disable_raw" | "std.term.disable_raw" => Some("zz_term_disable_raw"),
        "term.read_key" | "std.term.read_key" => Some("zz_term_read_key"),
        "term.poll" | "std.term.poll" => Some("zz_term_poll"),
        "term.get_size" | "std.term.get_size" => Some("zz_term_get_size"),
        "term.is_tty" | "std.term.is_tty" => Some("zz_term_is_tty"),
        "term.flush" | "std.term.flush" => Some("zz_term_flush"),
        // Raw argv shares the fixed `zz_env_args` symbol (one source of
        // truth for both spellings and both engines).
        "args.get_raw" | "std.args.get_raw" => Some("zz_env_args"),
        "args.parser" | "std.args.parser" => Some("zz_args_parser"),
        "args.str_flag" | "std.args.str_flag" => Some("zz_args_str_flag"),
        "args.int_flag" | "std.args.int_flag" => Some("zz_args_int_flag"),
        "args.bool_flag" | "std.args.bool_flag" => Some("zz_args_bool_flag"),
        "args.parse" | "std.args.parse" => Some("zz_args_parse"),
        "args.get_str" | "std.args.get_str" => Some("zz_args_get_str"),
        "args.get_int" | "std.args.get_int" => Some("zz_args_get_int"),
        "args.get_bool" | "std.args.get_bool" => Some("zz_args_get_bool"),
        "args.positional" | "std.args.positional" => Some("zz_args_positional"),
        "args.subcommand" | "std.args.subcommand" => Some("zz_args_subcommand"),
        "args.help" | "std.args.help" => Some("zz_args_help"),
        "args.error" | "std.args.error" => Some("zz_args_error"),
        "args.was_help" | "std.args.was_help" => Some("zz_args_was_help"),
        "process.run" | "std.process.run" => Some("zz_process_run"),
        "process.run_with_env" | "std.process.run_with_env" => Some("zz_process_run_with_env"),
        "process.spawn" | "std.process.spawn" => Some("zz_process_spawn"),
        "process.wait" | "std.process.wait" => Some("zz_process_wait"),
        "process.exit" | "std.process.exit" => Some("zz_process_exit"),
        "process.pid" | "std.process.pid" => Some("zz_process_pid"),
        "process.spawn_pty" | "std.process.spawn_pty" => Some("zz_process_spawn_pty"),
        "pty.open" | "std.pty.open" => Some("zz_pty_open"),
        "pty.read" | "std.pty.read" => Some("zz_pty_read"),
        "pty.write" | "std.pty.write" => Some("zz_pty_write"),
        "pty.ptsname" | "std.pty.ptsname" => Some("zz_pty_ptsname"),
        "pty.grantpt" | "std.pty.grantpt" => Some("zz_pty_grantpt"),
        "pty.unlockpt" | "std.pty.unlockpt" => Some("zz_pty_unlockpt"),
        "pty.set_winsz" | "std.pty.set_winsz" => Some("zz_pty_set_winsz"),
        "pty.isatty" | "std.pty.isatty" => Some("zz_pty_isatty"),
        "signal.install" | "std.signal.install" => Some("zz_signal_install"),
        "signal.pending" | "std.signal.pending" => Some("zz_signal_pending"),
        "signal.raise" | "std.signal.raise" => Some("zz_signal_raise"),
        "signal.run_pending" | "std.signal.run_pending" => Some("zz_signal_run_pending"),
        "uuid.v4" | "std.uuid.v4" => Some("zz_uuid_v4"),
        "uuid.v7" | "std.uuid.v7" => Some("zz_uuid_v7"),
        "uuid.parse" | "std.uuid.parse" => Some("zz_uuid_parse"),
        "uuid.is_valid" | "std.uuid.is_valid" => Some("zz_uuid_is_valid"),
        "net.tls_connect" | "std.net.tls_connect" | "tls.connect" | "std.tls.connect" => {
            Some("zz_tls_connect")
        }
        "net.tls_connect_with_ca"
        | "std.net.tls_connect_with_ca"
        | "tls.connect_with_ca"
        | "std.tls.connect_with_ca" => Some("zz_tls_connect_with_ca"),
        "net.tls_connect_insecure"
        | "std.net.tls_connect_insecure"
        | "tls.connect_insecure"
        | "std.tls.connect_insecure" => Some("zz_tls_connect_insecure"),
        "net.tls_write" | "std.net.tls_write" | "tls.write" | "std.tls.write" => {
            Some("zz_tls_write")
        }
        "net.tls_write_bytes"
        | "std.net.tls_write_bytes"
        | "tls.write_bytes"
        | "std.tls.write_bytes" => Some("zz_tls_write_bytes"),
        "net.tls_read" | "std.net.tls_read" | "tls.read" | "std.tls.read" => Some("zz_tls_read"),
        "net.tls_read_bytes"
        | "std.net.tls_read_bytes"
        | "tls.read_bytes"
        | "std.tls.read_bytes" => Some("zz_tls_read_bytes"),
        "net.tls_read_exact"
        | "std.net.tls_read_exact"
        | "tls.read_exact"
        | "std.tls.read_exact" => Some("zz_tls_read_exact"),
        "net.tls_readline" | "std.net.tls_readline" | "tls.read_line" | "std.tls.read_line" => {
            Some("zz_tls_read_line")
        }
        "net.tls_set_read_timeout"
        | "std.net.tls_set_read_timeout"
        | "tls.set_read_timeout"
        | "std.tls.set_read_timeout" => Some("zz_tls_set_read_timeout"),
        "net.tls_set_write_timeout"
        | "std.net.tls_set_write_timeout"
        | "tls.set_write_timeout"
        | "std.tls.set_write_timeout" => Some("zz_tls_set_write_timeout"),
        "net.tls_close"
        | "std.net.tls_close"
        | "tls.close"
        | "std.tls.close"
        | "tls.shutdown"
        | "std.tls.shutdown"
        | "net.tls_shutdown"
        | "std.net.tls_shutdown" => Some("zz_tls_close"),
        "net.unix_connect" | "std.net.unix_connect" => Some("zz_unix_connect"),
        "net.unix_listen" | "std.net.unix_listen" => Some("zz_unix_listen"),
        "net.unix_accept"
        | "std.net.unix_accept"
        | "unix_listener.accept"
        | "std.unix_listener.accept" => Some("zz_unix_accept"),
        "net.unix_listener_close"
        | "std.net.unix_listener_close"
        | "unix_listener.close"
        | "std.unix_listener.close"
        | "unix_listener.shutdown"
        | "std.unix_listener.shutdown" => Some("zz_unix_listener_close"),
        "net.unix_write" | "std.net.unix_write" | "unix.write" | "std.unix.write" => {
            Some("zz_unix_write")
        }
        "net.unix_write_bytes"
        | "std.net.unix_write_bytes"
        | "unix.write_bytes"
        | "std.unix.write_bytes" => Some("zz_unix_write_bytes"),
        "net.unix_read" | "std.net.unix_read" | "unix.read" | "std.unix.read" => {
            Some("zz_unix_read")
        }
        "net.unix_read_bytes"
        | "std.net.unix_read_bytes"
        | "unix.read_bytes"
        | "std.unix.read_bytes" => Some("zz_unix_read_bytes"),
        "net.unix_read_exact"
        | "std.net.unix_read_exact"
        | "unix.read_exact"
        | "std.unix.read_exact" => Some("zz_unix_read_exact"),
        "net.unix_readline" | "std.net.unix_readline" | "unix.read_line" | "std.unix.read_line" => {
            Some("zz_unix_read_line")
        }
        "net.unix_set_read_timeout"
        | "std.net.unix_set_read_timeout"
        | "unix.set_read_timeout"
        | "std.unix.set_read_timeout" => Some("zz_unix_set_read_timeout"),
        "net.unix_set_write_timeout"
        | "std.net.unix_set_write_timeout"
        | "unix.set_write_timeout"
        | "std.unix.set_write_timeout" => Some("zz_unix_set_write_timeout"),
        "net.unix_close"
        | "std.net.unix_close"
        | "unix.close"
        | "std.unix.close"
        | "unix.shutdown"
        | "std.unix.shutdown"
        | "net.unix_shutdown"
        | "std.net.unix_shutdown" => Some("zz_unix_close"),
        "net.tls_listen" | "std.net.tls_listen" => Some("zz_tls_listen"),
        "net.tls_accept"
        | "std.net.tls_accept"
        | "tls_listener.accept"
        | "std.tls_listener.accept" => Some("zz_tls_accept"),
        "net.tls_listener_close"
        | "std.net.tls_listener_close"
        | "tls_listener.close"
        | "std.tls_listener.close"
        | "tls_listener.shutdown"
        | "std.tls_listener.shutdown" => Some("zz_tls_listener_close"),
        "net.tls_peer_addr" | "std.net.tls_peer_addr" | "tls.peer_addr" | "std.tls.peer_addr" => {
            Some("zz_tls_peer_addr")
        }
        _ => None,
    }
}

/// C declaration for a staticlib symbol. Extended alongside [`ffi_impl`].
pub fn ffi_decl(symbol: &str) -> Option<&'static str> {
    match symbol {
        "zz_regexp_compile" => Some("zz_value zz_regexp_compile(zz_value pat, int *err);"),
        "zz_regexp_is_match" => {
            Some("zz_value zz_regexp_is_match(zz_value re, zz_value s, int *err);")
        }
        "zz_regexp_find" => Some("zz_value zz_regexp_find(zz_value re, zz_value s, int *err);"),
        "zz_regexp_replace_all" => {
            Some("zz_value zz_regexp_replace_all(zz_value re, zz_value s, zz_value rep, int *err);")
        }
        "zz_regexp_captures" => {
            Some("zz_value zz_regexp_captures(zz_value re, zz_value s, int *err);")
        }
        "zz_crypto_sha256" => Some("zz_value zz_crypto_sha256(zz_value s, int *err);"),
        "zz_crypto_sha256_bytes" => {
            Some("zz_value zz_crypto_sha256_bytes(zz_value b, int *err);")
        }
        "zz_encoding_base64_decode_bytes" => {
            Some("zz_value zz_encoding_base64_decode_bytes(zz_value s, int *err);")
        }
        "zz_crypto_sha512" => Some("zz_value zz_crypto_sha512(zz_value s, int *err);"),
        "zz_crypto_hmac_sha256" => {
            Some("zz_value zz_crypto_hmac_sha256(zz_value key, zz_value msg, int *err);")
        }
        "zz_crypto_random_bytes" => Some("zz_value zz_crypto_random_bytes(zz_value n, int *err);"),
        "zz_crypto_ct_eq" => Some("zz_value zz_crypto_ct_eq(zz_value a, zz_value b, int *err);"),
        "zz_crypto_argon2_hash" => Some("zz_value zz_crypto_argon2_hash(zz_value pw, int *err);"),
        "zz_crypto_argon2_verify" => {
            Some("zz_value zz_crypto_argon2_verify(zz_value hash, zz_value pw, int *err);")
        }
        "zz_crypto_bcrypt_hash" => Some("zz_value zz_crypto_bcrypt_hash(zz_value pw, int *err);"),
        "zz_crypto_bcrypt_verify" => {
            Some("zz_value zz_crypto_bcrypt_verify(zz_value hash, zz_value pw, int *err);")
        }
        "zz_crypto_ed25519_keypair" => {
            Some("zz_value zz_crypto_ed25519_keypair(zz_value unit, int *err);")
        }
        "zz_crypto_ed25519_sign" => {
            Some("zz_value zz_crypto_ed25519_sign(zz_value sk, zz_value msg, int *err);")
        }
        "zz_crypto_ed25519_verify" => Some(
            "zz_value zz_crypto_ed25519_verify(zz_value pk, zz_value msg, zz_value sig, int *err);",
        ),
        "zz_crypto_rsa_keypair" => Some("zz_value zz_crypto_rsa_keypair(zz_value unit, int *err);"),
        "zz_crypto_rsa_sign" => {
            Some("zz_value zz_crypto_rsa_sign(zz_value sk, zz_value msg, int *err);")
        }
        "zz_crypto_rsa_verify" => Some(
            "zz_value zz_crypto_rsa_verify(zz_value pk, zz_value msg, zz_value sig, int *err);",
        ),
        "zz_crypto_jwt_encode" => {
            Some("zz_value zz_crypto_jwt_encode(zz_value payload, zz_value secret, int *err);")
        }
        "zz_crypto_jwt_decode" => {
            Some("zz_value zz_crypto_jwt_decode(zz_value token, zz_value secret, int *err);")
        }
        "zz_crypto_jwt_encode_ed" => {
            Some("zz_value zz_crypto_jwt_encode_ed(zz_value payload, zz_value sk, int *err);")
        }
        "zz_crypto_jwt_decode_ed" => {
            Some("zz_value zz_crypto_jwt_decode_ed(zz_value token, zz_value pk, int *err);")
        }
        "zz_time_now_nanos" => Some("zz_value zz_time_now_nanos(zz_value unit, int *err);"),
        "zz_time_now_micros" => Some("zz_value zz_time_now_micros(zz_value unit, int *err);"),
        "zz_time_monotonic_nanos" => {
            Some("zz_value zz_time_monotonic_nanos(zz_value unit, int *err);")
        }
        "zz_time_sleep_micros" => Some("zz_value zz_time_sleep_micros(zz_value n, int *err);"),
        "zz_log_set_level" => Some("zz_value zz_log_set_level(zz_value name, int *err);"),
        "zz_log_get_level" => Some("zz_value zz_log_get_level(zz_value unit, int *err);"),
        "zz_log_set_format" => Some("zz_value zz_log_set_format(zz_value name, int *err);"),
        "zz_log_to_file" => Some("zz_value zz_log_to_file(zz_value path, int *err);"),
        "zz_log_to_stderr" => Some("zz_value zz_log_to_stderr(zz_value unit, int *err);"),
        "zz_log_trace" => Some("zz_value zz_log_trace(zz_value msg, int *err);"),
        "zz_log_debug" => Some("zz_value zz_log_debug(zz_value msg, int *err);"),
        "zz_log_info" => Some("zz_value zz_log_info(zz_value msg, int *err);"),
        "zz_log_warn" => Some("zz_value zz_log_warn(zz_value msg, int *err);"),
        "zz_log_error" => Some("zz_value zz_log_error(zz_value msg, int *err);"),
        "zz_log_span_begin" => Some("zz_value zz_log_span_begin(zz_value name, int *err);"),
        "zz_span_end" => Some("zz_value zz_span_end(zz_value id, int *err);"),
        "zz_sys_os" => Some("zz_value zz_sys_os(zz_value unit, int *err);"),
        "zz_sys_arch" => Some("zz_value zz_sys_arch(zz_value unit, int *err);"),
        "zz_sys_cpu_count" => Some("zz_value zz_sys_cpu_count(zz_value unit, int *err);"),
        "zz_sys_hostname" => Some("zz_value zz_sys_hostname(zz_value unit, int *err);"),
        "zz_sys_total_mem" => Some("zz_value zz_sys_total_mem(zz_value unit, int *err);"),
        "zz_sys_avail_mem" => Some("zz_value zz_sys_avail_mem(zz_value unit, int *err);"),
        "zz_term_enable_raw" => Some("zz_value zz_term_enable_raw(zz_value unit, int *err);"),
        "zz_term_disable_raw" => {
            Some("zz_value zz_term_disable_raw(zz_value unit, int *err);")
        }
        "zz_term_read_key" => Some("zz_value zz_term_read_key(zz_value unit, int *err);"),
        "zz_term_poll" => Some("zz_value zz_term_poll(zz_value ms, int *err);"),
        "zz_term_get_size" => Some("zz_value zz_term_get_size(zz_value unit, int *err);"),
        "zz_term_is_tty" => Some("zz_value zz_term_is_tty(zz_value unit, int *err);"),
        "zz_term_flush" => Some("zz_value zz_term_flush(zz_value unit, int *err);"),
        "zz_args_parser" => Some("zz_value zz_args_parser(zz_value unit, int *err);"),
        "zz_args_str_flag" => {
            Some("zz_value zz_args_str_flag(zz_value h, zz_value name, zz_value def, int *err);")
        }
        "zz_args_int_flag" => {
            Some("zz_value zz_args_int_flag(zz_value h, zz_value name, zz_value def, int *err);")
        }
        "zz_args_bool_flag" => {
            Some("zz_value zz_args_bool_flag(zz_value h, zz_value name, int *err);")
        }
        "zz_args_parse" => Some("zz_value zz_args_parse(zz_value h, zz_value argv, int *err);"),
        "zz_args_get_str" => Some("zz_value zz_args_get_str(zz_value h, zz_value name, int *err);"),
        "zz_args_get_int" => Some("zz_value zz_args_get_int(zz_value h, zz_value name, int *err);"),
        "zz_args_get_bool" => {
            Some("zz_value zz_args_get_bool(zz_value h, zz_value name, int *err);")
        }
        "zz_args_positional" => {
            Some("zz_value zz_args_positional(zz_value h, zz_value i, int *err);")
        }
        "zz_args_subcommand" => Some("zz_value zz_args_subcommand(zz_value h, int *err);"),
        "zz_args_help" => Some("zz_value zz_args_help(zz_value h, zz_value prog, int *err);"),
        "zz_args_error" => Some("zz_value zz_args_error(zz_value h, int *err);"),
        "zz_args_was_help" => Some("zz_value zz_args_was_help(zz_value h, int *err);"),
        "zz_process_run" => Some("zz_value zz_process_run(zz_value cmd, zz_value argv, int *err);"),
        "zz_process_run_with_env" => {
            Some("zz_value zz_process_run_with_env(zz_value cmd, zz_value argv, zz_value env, int *err);")
        }
        "zz_process_spawn" => Some("zz_value zz_process_spawn(zz_value cmd, zz_value argv, int *err);"),
        "zz_process_wait" => Some("zz_value zz_process_wait(zz_value id, int *err);"),
        "zz_process_exit" => Some("zz_value zz_process_exit(zz_value code, int *err);"),
        "zz_process_pid" => Some("zz_value zz_process_pid(zz_value unit, int *err);"),
        "zz_process_spawn_pty" => {
            Some("zz_value zz_process_spawn_pty(zz_value cmd, zz_value argv, zz_value slave_fd, int *err);")
        }
        "zz_pty_open" => Some("zz_value zz_pty_open(zz_value unit, int *err);"),
        "zz_pty_read" => Some("zz_value zz_pty_read(zz_value fd, zz_value n, int *err);"),
        "zz_pty_write" => Some("zz_value zz_pty_write(zz_value fd, zz_value data, int *err);"),
        "zz_pty_ptsname" => Some("zz_value zz_pty_ptsname(zz_value fd, int *err);"),
        "zz_pty_grantpt" => Some("zz_value zz_pty_grantpt(zz_value fd, int *err);"),
        "zz_pty_unlockpt" => Some("zz_value zz_pty_unlockpt(zz_value fd, int *err);"),
        "zz_pty_set_winsz" => {
            Some("zz_value zz_pty_set_winsz(zz_value fd, zz_value cols, zz_value rows, int *err);")
        }
        "zz_pty_isatty" => Some("zz_value zz_pty_isatty(zz_value fd, int *err);"),
        "zz_signal_install" => Some("zz_value zz_signal_install(zz_value signum, int *err);"),
        "zz_signal_pending" => Some("zz_value zz_signal_pending(zz_value signum, int *err);"),
        "zz_signal_raise" => Some("zz_value zz_signal_raise(zz_value signum, int *err);"),
        "zz_signal_run_pending" => Some("zz_value zz_signal_run_pending(zz_value unit, int *err);"),
        "zz_uuid_v4" => Some("zz_value zz_uuid_v4(zz_value unit, int *err);"),
        "zz_uuid_v7" => Some("zz_value zz_uuid_v7(zz_value unit, int *err);"),
        "zz_uuid_parse" => Some("zz_value zz_uuid_parse(zz_value s, int *err);"),
        "zz_tls_connect" => {
            Some("zz_value zz_tls_connect(zz_value addr, zz_value name, zz_value timeout, int *err);")
        }
        "zz_tls_connect_with_ca" => Some(
            "zz_value zz_tls_connect_with_ca(zz_value addr, zz_value name, zz_value ca, zz_value timeout, int *err);",
        ),
        "zz_tls_connect_insecure" => Some(
            "zz_value zz_tls_connect_insecure(zz_value addr, zz_value name, zz_value timeout, int *err);",
        ),
        "zz_tls_write" => Some("zz_value zz_tls_write(zz_value id, zz_value data, int *err);"),
        "zz_tls_write_bytes" => {
            Some("zz_value zz_tls_write_bytes(zz_value id, zz_value data, int *err);")
        }
        "zz_tls_read" => Some("zz_value zz_tls_read(zz_value id, zz_value max, int *err);"),
        "zz_tls_read_bytes" => {
            Some("zz_value zz_tls_read_bytes(zz_value id, zz_value max, int *err);")
        }
        "zz_tls_read_exact" => {
            Some("zz_value zz_tls_read_exact(zz_value id, zz_value n, int *err);")
        }
        "zz_tls_read_line" => Some("zz_value zz_tls_read_line(zz_value id, int *err);"),
        "zz_tls_set_read_timeout" => {
            Some("zz_value zz_tls_set_read_timeout(zz_value id, zz_value ms, int *err);")
        }
        "zz_tls_set_write_timeout" => {
            Some("zz_value zz_tls_set_write_timeout(zz_value id, zz_value ms, int *err);")
        }
        "zz_tls_close" => Some("zz_value zz_tls_close(zz_value id, int *err);"),
        "zz_tls_peer_addr" => Some("zz_value zz_tls_peer_addr(zz_value id, int *err);"),
        "zz_unix_connect" => {
            Some("zz_value zz_unix_connect(zz_value path, zz_value timeout, int *err);")
        }
        "zz_unix_listen" => Some("zz_value zz_unix_listen(zz_value path, int *err);"),
        "zz_unix_accept" => Some("zz_value zz_unix_accept(zz_value id, int *err);"),
        "zz_unix_listener_close" => {
            Some("zz_value zz_unix_listener_close(zz_value id, int *err);")
        }
        "zz_unix_write" => Some("zz_value zz_unix_write(zz_value id, zz_value data, int *err);"),
        "zz_unix_write_bytes" => {
            Some("zz_value zz_unix_write_bytes(zz_value id, zz_value data, int *err);")
        }
        "zz_unix_read" => Some("zz_value zz_unix_read(zz_value id, zz_value max, int *err);"),
        "zz_unix_read_bytes" => {
            Some("zz_value zz_unix_read_bytes(zz_value id, zz_value max, int *err);")
        }
        "zz_unix_read_exact" => {
            Some("zz_value zz_unix_read_exact(zz_value id, zz_value n, int *err);")
        }
        "zz_unix_read_line" => Some("zz_value zz_unix_read_line(zz_value id, int *err);"),
        "zz_unix_set_read_timeout" => {
            Some("zz_value zz_unix_set_read_timeout(zz_value id, zz_value ms, int *err);")
        }
        "zz_unix_set_write_timeout" => {
            Some("zz_value zz_unix_set_write_timeout(zz_value id, zz_value ms, int *err);")
        }
        "zz_unix_close" => Some("zz_value zz_unix_close(zz_value id, int *err);"),
        "zz_tls_listen" => Some(
            "zz_value zz_tls_listen(zz_value addr, zz_value cert, zz_value key, int *err);",
        ),
        "zz_tls_accept" => Some("zz_value zz_tls_accept(zz_value id, int *err);"),
        "zz_tls_listener_close" => Some("zz_value zz_tls_listener_close(zz_value id, int *err);"),
        "zz_uuid_is_valid" => Some("zz_value zz_uuid_is_valid(zz_value s, int *err);"),
        _ => None,
    }
}
