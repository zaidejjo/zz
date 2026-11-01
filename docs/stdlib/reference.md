# Stdlib reference — generated

Exhaustive signature index: **696 entries across 42 groups**.
Generated from compiled `FuncSig` data (`crates/zz_stdlib/src/funcs.rs`
via `dump_sigs`) — do not hand-edit. Regenerate with
`python3 scripts/gen-stdlib-ref.py`. Semantics: per-module guides linked below.
A `= …` param has a default and may be omitted. Bare names (`print`,
`assert_eq`, `map`) need no import; `std.*` names need their module import.

## `args` — 14 entries ([guide](env-process.md))

| Function | Signature |
|---|---|
| `std.args.bool_flag` | `bool_flag(p: args, name: str) -> unit` |
| `std.args.error` | `error(p: args) -> str` |
| `std.args.get_bool` | `get_bool(p: args, name: str) -> bool` |
| `std.args.get_int` | `get_int(p: args, name: str) -> int` |
| `std.args.get_raw` | `get_raw() -> [str]` |
| `std.args.get_str` | `get_str(p: args, name: str) -> str` |
| `std.args.help` | `help(p: args, prog: str) -> str` |
| `std.args.int_flag` | `int_flag(p: args, name: str, default: int) -> unit` |
| `std.args.parse` | `parse(p: args, argv: [str]) -> bool` |
| `std.args.parser` | `parser() -> args` |
| `std.args.positional` | `positional(p: args, i: int) -> Option<str>` |
| `std.args.str_flag` | `str_flag(p: args, name: str, default: str) -> unit` |
| `std.args.subcommand` | `subcommand(p: args) -> str` |
| `std.args.was_help` | `was_help(p: args) -> bool` |

## `argsparser` — 1 entries ([guide](overview.md))

| Function | Signature |
|---|---|
| `ArgsParser.new` | `new() -> args` |

## `builtins` — 39 entries ([guide](overview.md))

| Function | Signature |
|---|---|
| `assert` | `assert(cond: bool) -> unit` |
| `assert_approx_eq` | `assert_approx_eq(left: float, right: float, epsilon: float) -> unit` |
| `assert_contains` | `assert_contains(hay: str, needle: str) -> unit` |
| `assert_ends_with` | `assert_ends_with(s: str, suffix: str) -> unit` |
| `assert_eq` | `assert_eq<T>(left: T, right: T) -> unit` |
| `assert_gt` | `assert_gt<T,U>(left: T, right: U) -> unit` |
| `assert_is_err` | `assert_is_err<T,E>(val: Result<T, E>) -> unit` |
| `assert_is_none` | `assert_is_none<T>(val: Option<T>) -> unit` |
| `assert_is_ok` | `assert_is_ok<T,E>(val: Result<T, E>) -> unit` |
| `assert_is_some` | `assert_is_some<T>(val: Option<T>) -> unit` |
| `assert_len` | `assert_len<T>(val: T, n: int) -> unit` |
| `assert_lt` | `assert_lt<T,U>(left: T, right: U) -> unit` |
| `assert_ne` | `assert_ne<T>(left: T, right: T) -> unit` |
| `assert_snapshot` | `assert_snapshot(actual: str, name: str) -> unit` |
| `assert_starts_with` | `assert_starts_with(s: str, prefix: str) -> unit` |
| `black_box` | `black_box<T>(val: T) -> T` |
| `chr` | `chr(cp: int) -> str` |
| `clone` | `clone<T>(v: T) -> T` |
| `dbg` | `dbg<T>(v: T) -> T` |
| `enumerate` | `enumerate<T>(arr: [T] | T..) -> [(int, T)]` |
| `fail` | `fail(msg: str) -> unit` |
| `filter` | `filter<T>(arr: [T] | T.., f: func(T) -> bool) -> [T]` |
| `float` | `float<T>(v: T) -> float` |
| `free_port` | `free_port() -> Result<int, str>` |
| `input` | `input<T>(prompt: str) -> str` |
| `int` | `int<T>(v: T) -> Option<int>` |
| `len` | `len<T>(v: T) -> int` |
| `map` | `map<T,U>(arr: [T] | T.., f: func(T) -> U) -> [U]` |
| `ord` | `ord(ch: str) -> int` |
| `panic` | `panic(msg: str) -> unit` |
| `print` | `print<T>(v: T) -> unit` |
| `println` | `println<T>(v: T) -> unit` |
| `range` | `range(start: int, stop: int, step: int) -> int..` |
| `str` | `str<T>(v: T) -> str` |
| `temp_dir` | `temp_dir(prefix: str) -> Result<str, str>` |
| `typeof` | `typeof<T>(v: T) -> str` |
| `upgrade` | `upgrade<T>(v: T) -> T` |
| `weak` | `weak<T>(v: T) -> T` |
| `zip` | `zip<T,U>(a: [T] | T.., b: [U] | U..) -> [(T, U)]` |

## `bytes` — 9 entries ([guide](str-vec.md))

| Function | Signature |
|---|---|
| `std.bytes.builder` | `builder() -> [int]` |
| `std.bytes.extend` | `extend(b: [int], vs: [int]) -> [int]` |
| `std.bytes.finish` | `finish(vs: [int]) -> Result<bytes, str>` |
| `std.bytes.from_ints` | `from_ints(vs: [int]) -> [int]` |
| `std.bytes.len` | `len(v: bytes) -> int` |
| `std.bytes.len_of` | `len_of(vs: [int]) -> int` |
| `std.bytes.push_byte` | `push_byte(b: [int], v: int) -> [int]` |
| `std.bytes.to_ints` | `to_ints(b: bytes) -> [int]` |
| `std.bytes.to_str` | `to_str(vs: [int]) -> Result<str, str>` |

## `cell` — 4 entries ([guide](cell-path-sys.md))

| Function | Signature |
|---|---|
| `std.cell.drop` | `drop(c: cell) -> bool` |
| `std.cell.get` | `get<T>(c: cell) -> Result<T, str>` |
| `std.cell.new` | `new<T>(v: T) -> cell` |
| `std.cell.set` | `set<T>(c: cell, v: T) -> Result<bool, str>` |

## `chan` — 3 entries ([guide](concurrency.md))

| Function | Signature |
|---|---|
| `std.chan.recv` | `recv<T>(ch: chan) -> T` |
| `std.chan.send` | `send<T>(ch: chan, v: T) -> unit` |
| `std.chan.try_recv` | `try_recv<T>(ch: chan) -> Option<T>` |

## `colors` — 51 entries ([guide](terminal.md))

| Function | Signature |
|---|---|
| `std.colors.bg_256` | `bg_256(s: str, n: int) -> str` |
| `std.colors.bg_black` | `bg_black(s: str) -> str` |
| `std.colors.bg_blue` | `bg_blue(s: str) -> str` |
| `std.colors.bg_bright_black` | `bg_bright_black(s: str) -> str` |
| `std.colors.bg_bright_blue` | `bg_bright_blue(s: str) -> str` |
| `std.colors.bg_bright_cyan` | `bg_bright_cyan(s: str) -> str` |
| `std.colors.bg_bright_green` | `bg_bright_green(s: str) -> str` |
| `std.colors.bg_bright_magenta` | `bg_bright_magenta(s: str) -> str` |
| `std.colors.bg_bright_red` | `bg_bright_red(s: str) -> str` |
| `std.colors.bg_bright_white` | `bg_bright_white(s: str) -> str` |
| `std.colors.bg_bright_yellow` | `bg_bright_yellow(s: str) -> str` |
| `std.colors.bg_cyan` | `bg_cyan(s: str) -> str` |
| `std.colors.bg_green` | `bg_green(s: str) -> str` |
| `std.colors.bg_magenta` | `bg_magenta(s: str) -> str` |
| `std.colors.bg_red` | `bg_red(s: str) -> str` |
| `std.colors.bg_rgb` | `bg_rgb(s: str, r: int, g: int, b: int) -> str` |
| `std.colors.bg_white` | `bg_white(s: str) -> str` |
| `std.colors.bg_yellow` | `bg_yellow(s: str) -> str` |
| `std.colors.black` | `black(s: str) -> str` |
| `std.colors.blink` | `blink(s: str) -> str` |
| `std.colors.blue` | `blue(s: str) -> str` |
| `std.colors.bold` | `bold(s: str) -> str` |
| `std.colors.bright_black` | `bright_black(s: str) -> str` |
| `std.colors.bright_blue` | `bright_blue(s: str) -> str` |
| `std.colors.bright_cyan` | `bright_cyan(s: str) -> str` |
| `std.colors.bright_green` | `bright_green(s: str) -> str` |
| `std.colors.bright_magenta` | `bright_magenta(s: str) -> str` |
| `std.colors.bright_red` | `bright_red(s: str) -> str` |
| `std.colors.bright_white` | `bright_white(s: str) -> str` |
| `std.colors.bright_yellow` | `bright_yellow(s: str) -> str` |
| `std.colors.clamp255` | `clamp255(v: int) -> int` |
| `std.colors.color256` | `color256(s: str, n: int) -> str` |
| `std.colors.cyan` | `cyan(s: str) -> str` |
| `std.colors.dim` | `dim(s: str) -> str` |
| `std.colors.green` | `green(s: str) -> str` |
| `std.colors.hex` | `hex(s: str, code: str) -> str` |
| `std.colors.hex3` | `hex3(s: str, digits: [str]) -> str` |
| `std.colors.hex6` | `hex6(s: str, digits: [str]) -> str` |
| `std.colors.hex_byte` | `hex_byte(hi: str, lo: str) -> int` |
| `std.colors.hex_val` | `hex_val(c: str) -> int` |
| `std.colors.italic` | `italic(s: str) -> str` |
| `std.colors.magenta` | `magenta(s: str) -> str` |
| `std.colors.red` | `red(s: str) -> str` |
| `std.colors.reset` | `reset(s: str) -> str` |
| `std.colors.reverse` | `reverse(s: str) -> str` |
| `std.colors.rgb` | `rgb(s: str, r: int, g: int, b: int) -> str` |
| `std.colors.strikethrough` | `strikethrough(s: str) -> str` |
| `std.colors.strip` | `strip(s: str) -> str` |
| `std.colors.underline` | `underline(s: str) -> str` |
| `std.colors.white` | `white(s: str) -> str` |
| `std.colors.yellow` | `yellow(s: str) -> str` |

## `crypto` — 20 entries ([guide](encoding-crypto.md))

| Function | Signature |
|---|---|
| `std.crypto.argon2_hash` | `argon2_hash(pw: str) -> str` |
| `std.crypto.argon2_verify` | `argon2_verify(hash: str, pw: str) -> bool` |
| `std.crypto.bcrypt_hash` | `bcrypt_hash(pw: str) -> str` |
| `std.crypto.bcrypt_verify` | `bcrypt_verify(hash: str, pw: str) -> bool` |
| `std.crypto.ct_eq` | `ct_eq(a: str, b: str) -> bool` |
| `std.crypto.ed25519_keypair` | `ed25519_keypair() -> [str]` |
| `std.crypto.ed25519_sign` | `ed25519_sign(sk: str, msg: str) -> Result<str, str>` |
| `std.crypto.ed25519_verify` | `ed25519_verify(pk: str, msg: str, sig: str) -> bool` |
| `std.crypto.hmac_sha256` | `hmac_sha256(key: str, msg: str) -> str` |
| `std.crypto.jwt_decode` | `jwt_decode(token: str, secret: str) -> Result<str, str>` |
| `std.crypto.jwt_decode_ed` | `jwt_decode_ed(token: str, pk: str) -> Result<str, str>` |
| `std.crypto.jwt_encode` | `jwt_encode(payload: str, secret: str) -> str` |
| `std.crypto.jwt_encode_ed` | `jwt_encode_ed(payload: str, sk: str) -> Result<str, str>` |
| `std.crypto.random_bytes` | `random_bytes(n: int) -> str` |
| `std.crypto.rsa_keypair` | `rsa_keypair() -> Result<[str], str>` |
| `std.crypto.rsa_sign` | `rsa_sign(sk: str, msg: str) -> Result<str, str>` |
| `std.crypto.rsa_verify` | `rsa_verify(pk: str, msg: str, sig: str) -> bool` |
| `std.crypto.sha256` | `sha256(s: str) -> str` |
| `std.crypto.sha256_bytes` | `sha256_bytes(b: bytes) -> str` |
| `std.crypto.sha512` | `sha512(s: str) -> str` |

## `csv` — 14 entries ([guide](data.md))

| Function | Signature |
|---|---|
| `std.csv.delim_first` | `delim_first(delimiter: str) -> str` |
| `std.csv.escape_cell` | `escape_cell(cell: str) -> str` |
| `std.csv.get_cell` | `get_cell(rows: [[str]], r: int, c: int, default: str) -> str` |
| `std.csv.header` | `header(rows: [[str]]) -> [str]` |
| `std.csv.json_escape` | `json_escape(s: str) -> str` |
| `std.csv.len` | `len(rows: [[str]]) -> int` |
| `std.csv.needs_quote` | `needs_quote(cell: str, d: str) -> bool` |
| `std.csv.parse` | `parse(src: str) -> [[str]]` |
| `std.csv.parse_delim` | `parse_delim(src: str, delimiter: str) -> [[str]]` |
| `std.csv.records` | `records(rows: [[str]]) -> [[str]]` |
| `std.csv.stringify` | `stringify(rows: [[str]]) -> str` |
| `std.csv.stringify_delim` | `stringify_delim(rows: [[str]], delimiter: str) -> str` |
| `std.csv.to_json` | `to_json(rows: [[str]]) -> json` |
| `std.csv.validate` | `validate(src: str) -> bool` |

## `dec` — 14 entries ([guide](data.md))

| Function | Signature |
|---|---|
| `std.dec.add` | `add(a: str, b: str) -> str` |
| `std.dec.cmp` | `cmp(a: str, b: str) -> int` |
| `std.dec.eq` | `eq(a: str, b: str) -> bool` |
| `std.dec.format` | `format(s: str, places: int) -> str` |
| `std.dec.from_scaled` | `from_scaled(v: int, scale: int) -> str` |
| `std.dec.gt` | `gt(a: str, b: str) -> bool` |
| `std.dec.is_valid` | `is_valid(s: str) -> bool` |
| `std.dec.lt` | `lt(a: str, b: str) -> bool` |
| `std.dec.mul` | `mul(a: str, b: str) -> str` |
| `std.dec.pow10` | `pow10(n: int) -> int` |
| `std.dec.scale_of` | `scale_of(s: str) -> int` |
| `std.dec.sub` | `sub(a: str, b: str) -> str` |
| `std.dec.to_scaled` | `to_scaled(s: str, scale: int) -> int` |
| `std.dec.trim_zeros` | `trim_zeros(s: str) -> str` |

## `encoding` — 7 entries ([guide](encoding-crypto.md))

| Function | Signature |
|---|---|
| `std.encoding.base64_decode` | `base64_decode(encoded: str) -> Result<str, str>` |
| `std.encoding.base64_decode_bytes` | `base64_decode_bytes(encoded: str) -> Result<bytes, str>` |
| `std.encoding.base64_encode` | `base64_encode(data: str) -> str` |
| `std.encoding.hex_decode` | `hex_decode(encoded: str) -> Result<str, str>` |
| `std.encoding.hex_encode` | `hex_encode(data: str) -> str` |
| `std.encoding.url_decode` | `url_decode(encoded: str) -> Result<str, str>` |
| `std.encoding.url_encode` | `url_encode(data: str) -> str` |

## `env` — 14 entries ([guide](env-process.md))

| Function | Signature |
|---|---|
| `std.env.arch` | `arch() -> str` |
| `std.env.args` | `args() -> [str]` |
| `std.env.cwd` | `cwd() -> Result<str, str>` |
| `std.env.exe_path` | `exe_path() -> Result<str, str>` |
| `std.env.get` | `get(key: str) -> Option<str>` |
| `std.env.home_dir` | `home_dir() -> Option<str>` |
| `std.env.os` | `os() -> str` |
| `std.env.remove` | `remove(key: str) -> unit` |
| `std.env.set` | `set(key: str, val: str) -> Result<unit, str>` |
| `std.env.set_cwd` | `set_cwd(path: str) -> Result<unit, str>` |
| `std.env.temp_dir` | `temp_dir() -> str` |
| `std.env.user` | `user() -> Option<str>` |
| `std.env.var` | `var(name: str) -> Result<str, str>` |
| `std.env.vars` | `vars() -> {str: str}` |

## `file` — 7 entries ([guide](overview.md))

| Function | Signature |
|---|---|
| `File.open` | `open(path: str, mode: str) -> Result<file, str>` |
| `file.close` | `close(f: file) -> Result<unit, str>` |
| `file.flush` | `flush(f: file) -> Result<unit, str>` |
| `file.read_chunk` | `read_chunk(f: file, n: int) -> Result<str, str>` |
| `file.read_chunk_bytes` | `read_chunk_bytes(f: file, n: int) -> Result<bytes, str>` |
| `file.seek` | `seek(f: file, pos: int) -> Result<int, str>` |
| `file.write_chunk` | `write_chunk(f: file, data: str) -> Result<int, str>` |

## `fs` — 45 entries ([guide](fs-bytes.md))

| Function | Signature |
|---|---|
| `std.fs.append` | `append(path: str, content: str) -> Result<unit, str>` |
| `std.fs.append_at` | `append_at(fsys: zzfs, path: str, content: str) -> Result<unit, str>` |
| `std.fs.basename` | `basename(path: str) -> str` |
| `std.fs.close` | `close(f: file) -> Result<unit, str>` |
| `std.fs.copy` | `copy(src: str, dst: str) -> Result<unit, str>` |
| `std.fs.dirname` | `dirname(path: str) -> str` |
| `std.fs.embedfs` | `embedfs() -> Result<zzfs, str>` |
| `std.fs.exists` | `exists(path: str) -> bool` |
| `std.fs.exists_at` | `exists_at(fsys: zzfs, path: str) -> bool` |
| `std.fs.extension` | `extension(path: str) -> str` |
| `std.fs.flush` | `flush(f: file) -> Result<unit, str>` |
| `std.fs.is_absolute` | `is_absolute(path: str) -> bool` |
| `std.fs.is_dir` | `is_dir(path: str) -> bool` |
| `std.fs.is_dir_at` | `is_dir_at(fsys: zzfs, path: str) -> bool` |
| `std.fs.is_file` | `is_file(path: str) -> bool` |
| `std.fs.is_file_at` | `is_file_at(fsys: zzfs, path: str) -> bool` |
| `std.fs.is_generated` | `is_generated(path: str, markers: [str]) -> Result<bool, str>` |
| `std.fs.join` | `join(a: str, b: str) -> str` |
| `std.fs.memfs` | `memfs() -> Result<zzfs, str>` |
| `std.fs.mkdir` | `mkdir(path: str) -> Result<unit, str>` |
| `std.fs.mkdir_all` | `mkdir_all(path: str) -> Result<unit, str>` |
| `std.fs.mkdir_all_at` | `mkdir_all_at(fsys: zzfs, path: str) -> Result<unit, str>` |
| `std.fs.move` | `move(src: str, dst: str) -> Result<unit, str>` |
| `std.fs.normalize` | `normalize(path: str) -> str` |
| `std.fs.open` | `open(path: str, mode: str) -> Result<file, str>` |
| `std.fs.osfs` | `osfs() -> Result<zzfs, str>` |
| `std.fs.read` | `read(path: str) -> Result<str, str>` |
| `std.fs.read_bytes` | `read_bytes(path: str) -> Result<bytes, str>` |
| `std.fs.read_bytes_at` | `read_bytes_at(fsys: zzfs, path: str) -> Result<bytes, str>` |
| `std.fs.read_chunk` | `read_chunk(f: file, n: int) -> Result<str, str>` |
| `std.fs.read_chunk_bytes` | `read_chunk_bytes(f: file, n: int) -> Result<bytes, str>` |
| `std.fs.read_dir` | `read_dir(path: str) -> Result<[str], str>` |
| `std.fs.read_dir_at` | `read_dir_at(fsys: zzfs, path: str) -> Result<[str], str>` |
| `std.fs.read_to_string_at` | `read_to_string_at(fsys: zzfs, path: str) -> Result<str, str>` |
| `std.fs.remove` | `remove(path: str) -> Result<unit, str>` |
| `std.fs.remove_dir_all` | `remove_dir_all(path: str) -> Result<unit, str>` |
| `std.fs.remove_file_at` | `remove_file_at(fsys: zzfs, path: str) -> Result<unit, str>` |
| `std.fs.scan_counts` | `scan_counts(path: str, markers: [str], bstart: str, bend: str, nested: bool, whole: bool) -> Result<[int], str>` |
| `std.fs.seek` | `seek(f: file, pos: int) -> Result<int, str>` |
| `std.fs.stat` | `stat(path: str) -> Result<{str: str}, str>` |
| `std.fs.tarfs` | `tarfs(path: str) -> Result<zzfs, str>` |
| `std.fs.walk_dir` | `walk_dir(path: str) -> Result<[str], str>` |
| `std.fs.write` | `write(path: str, content: str) -> Result<unit, str>` |
| `std.fs.write_at` | `write_at(fsys: zzfs, path: str, content: str) -> Result<unit, str>` |
| `std.fs.write_chunk` | `write_chunk(f: file, data: str) -> Result<int, str>` |

## `http` — 49 entries ([guide](http.md))

| Function | Signature |
|---|---|
| `std.http.body_bytes` | `body_bytes(req: http.request) -> bytes` |
| `std.http.body_form` | `body_form(req: http.request) -> {str: str}` |
| `std.http.body_json` | `body_json(req: http.request) -> Result<json, str>` |
| `std.http.cors` | `cors(server: http.server, origins: [str]) -> http.server` |
| `std.http.created` | `created(body: str) -> http.response` |
| `std.http.csrf_check` | `csrf_check(a: str, b: str) -> bool` |
| `std.http.csrf_token` | `csrf_token() -> str` |
| `std.http.delete` | `delete(url: str, headers: {str: str} = …) -> Result<http.response, str>` |
| `std.http.fetch` | `fetch(url: str, method: str = …, headers: {str: str} = …, body: str | bytes = …, timeout_ms: int = …) -> Result<http.response, str>` |
| `std.http.fetch_insecure` | `fetch_insecure(url: str, method: str = …, headers: {str: str} = …, body: str | bytes = …, timeout_ms: int = …) -> Result<http.response, str>` |
| `std.http.get` | `get(url: str, headers: {str: str} = …) -> Result<http.response, str>` |
| `std.http.handle` | `handle(server: http.server, method: str, path: str, body: str) -> Result<str, str>` |
| `std.http.header` | `header(req: http.request, name: str) -> Result<str, str>` |
| `std.http.headers` | `headers(res: http.response) -> {str: str}` |
| `std.http.hijack` | `hijack(server: http.server, path: str, handler: func(http.request, tcp.stream) -> str | unit) -> http.server` |
| `std.http.json` | `json(res: http.response) -> Result<json, str>` |
| `std.http.listen` | `listen(server: http.server, port: int) -> unit` |
| `std.http.listen_cfg` | `listen_cfg(server: http.server, port: int, opts: {str: int}) -> unit` |
| `std.http.listen_tls` | `listen_tls(server: http.server, port: int, cert_path: str, key_path: str) -> unit` |
| `std.http.listen_tls_cfg` | `listen_tls_cfg(server: http.server, port: int, cert_path: str, key_path: str, opts: {str: int}) -> unit` |
| `std.http.log` | `log(server: http.server, enabled: bool) -> http.server` |
| `std.http.not_found` | `not_found() -> http.response` |
| `std.http.ok` | `ok(body: str) -> http.response` |
| `std.http.param` | `param(req: http.request, name: str) -> Result<str, str>` |
| `std.http.pipe` | `pipe(server: http.server, middleware: func(http.request) -> Result<http.request, http.response>) -> http.server` |
| `std.http.pipe_post` | `pipe_post(server: http.server, post: func(http.request, http.response) -> http.response) -> http.server` |
| `std.http.post` | `post(url: str, body: str | bytes, headers: {str: str} = …) -> Result<http.response, str>` |
| `std.http.post_json` | `post_json<T>(url: str, body: T, headers: {str: str} = …) -> Result<http.response, str>` |
| `std.http.put` | `put(url: str, body: str | bytes, headers: {str: str} = …) -> Result<http.response, str>` |
| `std.http.query` | `query(req: http.request) -> {str: str}` |
| `std.http.rate_limit` | `rate_limit(server: http.server, max_requests: int, window_ms: int) -> http.server` |
| `std.http.redirect` | `redirect(url: str) -> http.response` |
| `std.http.request_id` | `request_id(server: http.server) -> http.server` |
| `std.http.respond` | `respond(status: int, body: str, headers: {str: str} = …) -> http.response` |
| `std.http.route` | `route(server: http.server, method: str, path: str, handler: func(http.request) -> str | http.response) -> http.server` |
| `std.http.route_delete` | `route_delete(server: http.server, path: str, handler: func(http.request) -> str | http.response) -> http.server` |
| `std.http.route_get` | `route_get(server: http.server, path: str, handler: func(http.request) -> str | http.response) -> http.server` |
| `std.http.route_post` | `route_post(server: http.server, path: str, handler: func(http.request) -> str | http.response) -> http.server` |
| `std.http.route_put` | `route_put(server: http.server, path: str, handler: func(http.request) -> str | http.response) -> http.server` |
| `std.http.secure_header_dict` | `secure_header_dict() -> {str: str}` |
| `std.http.secure_headers` | `secure_headers(server: http.server) -> http.server` |
| `std.http.serve_dir` | `serve_dir(server: http.server, dir: str) -> http.server` |
| `std.http.serve_dir_at` | `serve_dir_at(server: http.server, prefix: str, dir: str) -> http.server` |
| `std.http.server` | `server() -> http.server` |
| `std.http.status` | `status(res: http.response) -> int` |
| `std.http.test` | `test(server: http.server, method: str, path: str, body: str) -> http.response` |
| `std.http.test_req` | `test_req(server: http.server, method: str, path: str, headers: {str: str}, body: str) -> http.response` |
| `std.http.text` | `text(res: http.response) -> str` |
| `std.http.with_headers` | `with_headers(res: http.response, extra: {str: str}) -> http.response` |

## `json` — 28 entries ([guide](data.md))

| Function | Signature |
|---|---|
| `std.json.array_push` | `array_push<T>(j: json, val: T) -> json` |
| `std.json.as_bool` | `as_bool(j: json) -> bool` |
| `std.json.as_float` | `as_float(j: json) -> float` |
| `std.json.as_int` | `as_int(j: json) -> int` |
| `std.json.as_str` | `as_str(j: json) -> str` |
| `std.json.deep_get` | `deep_get(j: json, path: str) -> Result<json, str>` |
| `std.json.get` | `get(j: json, key: str) -> Result<json, str>` |
| `std.json.has` | `has(j: json, key: str) -> bool` |
| `std.json.is_array` | `is_array(j: json) -> bool` |
| `std.json.is_bool` | `is_bool(j: json) -> bool` |
| `std.json.is_empty` | `is_empty(j: json) -> bool` |
| `std.json.is_null` | `is_null(j: json) -> bool` |
| `std.json.is_number` | `is_number(j: json) -> bool` |
| `std.json.is_object` | `is_object(j: json) -> bool` |
| `std.json.is_string` | `is_string(j: json) -> bool` |
| `std.json.keys` | `keys(j: json) -> [str]` |
| `std.json.len` | `len(j: json) -> int` |
| `std.json.merge` | `merge(a: json, b: json) -> json` |
| `std.json.null` | `null() -> json` |
| `std.json.parse` | `parse(s: str) -> Result<json, str>` |
| `std.json.parse_or` | `parse_or(s: str, default: json) -> json` |
| `std.json.parse_or_null` | `parse_or_null(s: str) -> json` |
| `std.json.path_exists` | `path_exists(j: json, path: str) -> bool` |
| `std.json.path_get_or` | `path_get_or(j: json, path: str, default: json) -> json` |
| `std.json.pretty` | `pretty(j: json) -> str` |
| `std.json.stringify` | `stringify<T>(v: T) -> Result<str, str>` |
| `std.json.type` | `type(j: json) -> str` |
| `std.json.validate` | `validate(s: str) -> bool` |

## `log` — 11 entries ([guide](env-process.md))

| Function | Signature |
|---|---|
| `std.log.debug` | `debug(msg: str) -> unit` |
| `std.log.error` | `error(msg: str) -> unit` |
| `std.log.get_level` | `get_level() -> str` |
| `std.log.info` | `info(msg: str) -> unit` |
| `std.log.set_format` | `set_format(name: str) -> bool` |
| `std.log.set_level` | `set_level(name: str) -> bool` |
| `std.log.span_begin` | `span_begin(name: str) -> span` |
| `std.log.to_file` | `to_file(path: str) -> bool` |
| `std.log.to_stderr` | `to_stderr() -> unit` |
| `std.log.trace` | `trace(msg: str) -> unit` |
| `std.log.warn` | `warn(msg: str) -> unit` |

## `map` — 12 entries ([guide](data.md))

| Function | Signature |
|---|---|
| `std.map.get_or` | `get_or(d: {str: int}, key: str, default: int) -> int` |
| `std.map.get_str` | `get_str(d: {str: str}, key: str, default: str) -> str` |
| `std.map.has` | `has(d: {str: int}, key: str) -> bool` |
| `std.map.is_empty` | `is_empty(d: {str: int}) -> bool` |
| `std.map.keys` | `keys(d: {str: int}) -> [str]` |
| `std.map.keys_str` | `keys_str(d: {str: str}) -> [str]` |
| `std.map.len` | `len(d: {str: int}) -> int` |
| `std.map.merge` | `merge(a: {str: int}, b: {str: int}) -> {str: int}` |
| `std.map.merge_str` | `merge_str(a: {str: str}, b: {str: str}) -> {str: str}` |
| `std.map.remove` | `remove(d: {str: int}, key: str) -> {str: int}` |
| `std.map.values` | `values(d: {str: int}) -> [int]` |
| `std.map.values_str` | `values_str(d: {str: str}) -> [str]` |

## `math` — 73 entries ([guide](math-time.md))

| Function | Signature |
|---|---|
| `std.math.E` | `E() -> float` |
| `std.math.E` | `E(value = 2.718281828459045) -> float` |
| `std.math.INF` | `INF() -> float` |
| `std.math.INF` | `INF(value = inf) -> float` |
| `std.math.LN_10` | `LN_10() -> float` |
| `std.math.LN_10` | `LN_10(value = 2.302585092994046) -> float` |
| `std.math.LN_2` | `LN_2() -> float` |
| `std.math.LN_2` | `LN_2(value = 0.6931471805599453) -> float` |
| `std.math.LOG10_E` | `LOG10_E() -> float` |
| `std.math.LOG10_E` | `LOG10_E(value = 0.4342944819032518) -> float` |
| `std.math.LOG2_E` | `LOG2_E() -> float` |
| `std.math.LOG2_E` | `LOG2_E(value = 1.4426950408889634) -> float` |
| `std.math.NAN` | `NAN() -> float` |
| `std.math.NAN` | `NAN(value = NaN) -> float` |
| `std.math.PI` | `PI() -> float` |
| `std.math.PI` | `PI(value = 3.141592653589793) -> float` |
| `std.math.SQRT_1_2` | `SQRT_1_2() -> float` |
| `std.math.SQRT_1_2` | `SQRT_1_2(value = 0.7071067811865476) -> float` |
| `std.math.SQRT_2` | `SQRT_2() -> float` |
| `std.math.SQRT_2` | `SQRT_2(value = 1.4142135623730951) -> float` |
| `std.math.TAU` | `TAU() -> float` |
| `std.math.TAU` | `TAU(value = 6.283185307179586) -> float` |
| `std.math.abs` | `abs<T>(v: T) -> T` |
| `std.math.acos` | `acos<T>(x: T) -> float` |
| `std.math.asin` | `asin<T>(x: T) -> float` |
| `std.math.atan` | `atan<T>(x: T) -> float` |
| `std.math.ceil` | `ceil(v: float) -> int` |
| `std.math.clamp` | `clamp<T>(val: T, min: T, max: T) -> float` |
| `std.math.cos` | `cos<T>(x: T) -> float` |
| `std.math.cos_deg` | `cos_deg<T>(x: T) -> float` |
| `std.math.count` | `count(arr: [int], target: int) -> int` |
| `std.math.dot_product` | `dot_product(v1: [float], v2: [float]) -> Result<float, str>` |
| `std.math.exp` | `exp<T>(x: T) -> float` |
| `std.math.factorial` | `factorial(n: int) -> Result<int, str>` |
| `std.math.floor` | `floor(v: float) -> int` |
| `std.math.gcd` | `gcd(a: int, b: int) -> int` |
| `std.math.hypot` | `hypot<T>(x: T, y: T) -> float` |
| `std.math.is_even` | `is_even(n: int) -> bool` |
| `std.math.is_inf` | `is_inf<T>(x: T) -> bool` |
| `std.math.is_nan` | `is_nan<T>(x: T) -> bool` |
| `std.math.is_odd` | `is_odd(n: int) -> bool` |
| `std.math.isqrt` | `isqrt(n: int) -> Result<int, str>` |
| `std.math.lcm` | `lcm(a: int, b: int) -> int` |
| `std.math.log` | `log<T>(x: T) -> float` |
| `std.math.log10` | `log10<T>(x: T) -> float` |
| `std.math.magnitude` | `magnitude(v: [float]) -> float` |
| `std.math.matrix_mul` | `matrix_mul(m1: [[float]], m2: [[float]]) -> Result<[[float]], str>` |
| `std.math.max` | `max<T>(a: T, b: T) -> T` |
| `std.math.max_arr` | `max_arr<T>(arr: [T]) -> Option<T>` |
| `std.math.mean` | `mean(list: [float]) -> Result<float, str>` |
| `std.math.mean_f` | `mean_f(arr: [float]) -> Result<float, str>` |
| `std.math.median` | `median(list: [float]) -> Result<float, str>` |
| `std.math.median_f` | `median_f(arr: [float]) -> Result<float, str>` |
| `std.math.min` | `min<T>(a: T, b: T) -> T` |
| `std.math.min_arr` | `min_arr<T>(arr: [T]) -> Option<T>` |
| `std.math.pow` | `pow<T>(base: T, exp: T) -> float` |
| `std.math.product` | `product(arr: [int]) -> int` |
| `std.math.product_f` | `product_f(arr: [float]) -> float` |
| `std.math.rand_range` | `rand_range<T>(min: T, max: T) -> float` |
| `std.math.random` | `random() -> float` |
| `std.math.root` | `root<T>(x: T, n: T) -> float` |
| `std.math.round` | `round<T>(x: T) -> float` |
| `std.math.signum` | `signum<T>(x: T) -> T` |
| `std.math.sin` | `sin<T>(x: T) -> float` |
| `std.math.sin_deg` | `sin_deg<T>(x: T) -> float` |
| `std.math.sqrt` | `sqrt<T>(v: T) -> float` |
| `std.math.sum` | `sum(arr: [int]) -> int` |
| `std.math.sum_f` | `sum_f(arr: [float]) -> float` |
| `std.math.tan` | `tan<T>(x: T) -> float` |
| `std.math.tan_deg` | `tan_deg<T>(x: T) -> float` |
| `std.math.to_degrees` | `to_degrees<T>(rad: T) -> float` |
| `std.math.to_radians` | `to_radians<T>(deg: T) -> float` |
| `std.math.trunc` | `trunc<T>(x: T) -> float` |

## `net` — 54 entries ([guide](net.md))

| Function | Signature |
|---|---|
| `std.net.accept` | `accept(listener: tcp.listener) -> Result<tcp.stream, str>` |
| `std.net.close` | `close(stream: tcp.stream) -> Result<unit, str>` |
| `std.net.local_addr` | `local_addr(stream: tcp.stream) -> Result<str, str>` |
| `std.net.peer_addr` | `peer_addr(stream: tcp.stream) -> Result<str, str>` |
| `std.net.read` | `read(stream: tcp.stream, max_bytes: int) -> Result<str, str>` |
| `std.net.read_bytes` | `read_bytes(stream: tcp.stream, max_bytes: int) -> Result<bytes, str>` |
| `std.net.read_exact` | `read_exact(stream: tcp.stream, n: int) -> Result<bytes, str>` |
| `std.net.read_line` | `read_line(stream: tcp.stream) -> Result<str, str>` |
| `std.net.set_read_timeout` | `set_read_timeout(stream: tcp.stream, ms: int) -> Result<bool, str>` |
| `std.net.set_write_timeout` | `set_write_timeout(stream: tcp.stream, ms: int) -> Result<bool, str>` |
| `std.net.shutdown` | `shutdown(stream: tcp.stream) -> Result<unit, str>` |
| `std.net.tcp_accept` | `tcp_accept(listener: tcp.listener) -> Result<tcp.stream, str>` |
| `std.net.tcp_connect` | `tcp_connect(addr: str, timeout_ms: int) -> Result<tcp.stream, str>` |
| `std.net.tcp_listen` | `tcp_listen(addr: str) -> Result<tcp.listener, str>` |
| `std.net.tcp_read` | `tcp_read(stream: tcp.stream, max_bytes: int) -> Result<str, str>` |
| `std.net.tcp_read_bytes` | `tcp_read_bytes(stream: tcp.stream, max_bytes: int) -> Result<bytes, str>` |
| `std.net.tcp_read_exact` | `tcp_read_exact(stream: tcp.stream, n: int) -> Result<bytes, str>` |
| `std.net.tcp_readline` | `tcp_readline(stream: tcp.stream) -> Result<str, str>` |
| `std.net.tcp_shutdown` | `tcp_shutdown(stream: tcp.stream) -> Result<unit, str>` |
| `std.net.tcp_write` | `tcp_write(stream: tcp.stream, data: str) -> Result<int, str>` |
| `std.net.tcp_write_bytes` | `tcp_write_bytes(stream: tcp.stream, data: bytes) -> Result<int, str>` |
| `std.net.tls_accept` | `tls_accept(listener: tls_listener) -> Result<tls, str>` |
| `std.net.tls_close` | `tls_close(stream: tls) -> Result<unit, str>` |
| `std.net.tls_connect` | `tls_connect(addr: str, server_name: str, timeout_ms: int) -> Result<tls, str>` |
| `std.net.tls_connect_insecure` | `tls_connect_insecure(addr: str, server_name: str, timeout_ms: int) -> Result<tls, str>` |
| `std.net.tls_connect_with_ca` | `tls_connect_with_ca(addr: str, server_name: str, ca_file: str, timeout_ms: int) -> Result<tls, str>` |
| `std.net.tls_listen` | `tls_listen(addr: str, cert_file: str, key_file: str) -> Result<tls_listener, str>` |
| `std.net.tls_listener_close` | `tls_listener_close(listener: tls_listener) -> Result<bool, str>` |
| `std.net.tls_peer_addr` | `tls_peer_addr(stream: tls) -> Result<str, str>` |
| `std.net.tls_read` | `tls_read(stream: tls, max_bytes: int) -> Result<str, str>` |
| `std.net.tls_read_bytes` | `tls_read_bytes(stream: tls, max_bytes: int) -> Result<bytes, str>` |
| `std.net.tls_read_exact` | `tls_read_exact(stream: tls, n: int) -> Result<bytes, str>` |
| `std.net.tls_readline` | `tls_readline(stream: tls) -> Result<str, str>` |
| `std.net.tls_set_read_timeout` | `tls_set_read_timeout(stream: tls, ms: int) -> Result<bool, str>` |
| `std.net.tls_set_write_timeout` | `tls_set_write_timeout(stream: tls, ms: int) -> Result<bool, str>` |
| `std.net.tls_shutdown` | `tls_shutdown(stream: tls) -> Result<unit, str>` |
| `std.net.tls_write` | `tls_write(stream: tls, data: str) -> Result<int, str>` |
| `std.net.tls_write_bytes` | `tls_write_bytes(stream: tls, data: bytes) -> Result<int, str>` |
| `std.net.unix_accept` | `unix_accept(listener: unix_listener) -> Result<unix, str>` |
| `std.net.unix_close` | `unix_close(stream: unix) -> Result<unit, str>` |
| `std.net.unix_connect` | `unix_connect(path: str, timeout_ms: int) -> Result<unix, str>` |
| `std.net.unix_listen` | `unix_listen(path: str) -> Result<unix_listener, str>` |
| `std.net.unix_listener_close` | `unix_listener_close(listener: unix_listener) -> Result<bool, str>` |
| `std.net.unix_read` | `unix_read(stream: unix, max_bytes: int) -> Result<str, str>` |
| `std.net.unix_read_bytes` | `unix_read_bytes(stream: unix, max_bytes: int) -> Result<bytes, str>` |
| `std.net.unix_read_exact` | `unix_read_exact(stream: unix, n: int) -> Result<bytes, str>` |
| `std.net.unix_readline` | `unix_readline(stream: unix) -> Result<str, str>` |
| `std.net.unix_set_read_timeout` | `unix_set_read_timeout(stream: unix, ms: int) -> Result<bool, str>` |
| `std.net.unix_set_write_timeout` | `unix_set_write_timeout(stream: unix, ms: int) -> Result<bool, str>` |
| `std.net.unix_shutdown` | `unix_shutdown(stream: unix) -> Result<unit, str>` |
| `std.net.unix_write` | `unix_write(stream: unix, data: str) -> Result<int, str>` |
| `std.net.unix_write_bytes` | `unix_write_bytes(stream: unix, data: bytes) -> Result<int, str>` |
| `std.net.write` | `write(stream: tcp.stream, data: str) -> Result<int, str>` |
| `std.net.write_bytes` | `write_bytes(stream: tcp.stream, data: bytes) -> Result<int, str>` |

## `option` — 3 entries ([guide](overview.md))

| Function | Signature |
|---|---|
| `option.expect` | `expect<T>(opt: Option<T>, msg: str) -> T` |
| `option.unwrap` | `unwrap<T>(opt: Option<T>) -> T` |
| `option.unwrap_or` | `unwrap_or<T>(opt: Option<T>, default: T) -> T` |

## `path` — 7 entries ([guide](cell-path-sys.md))

| Function | Signature |
|---|---|
| `std.path.basename` | `basename(path: str) -> str` |
| `std.path.dirname` | `dirname(path: str) -> str` |
| `std.path.extension` | `extension(path: str) -> str` |
| `std.path.is_absolute` | `is_absolute(path: str) -> bool` |
| `std.path.join` | `join(a: str, b: str) -> str` |
| `std.path.join_all` | `join_all(parts: [str]) -> str` |
| `std.path.normalize` | `normalize(path: str) -> str` |

## `process` — 6 entries ([guide](env-process.md))

| Function | Signature |
|---|---|
| `std.process.exit` | `exit(code: int) -> unit` |
| `std.process.pid` | `pid() -> int` |
| `std.process.run` | `run(cmd: str, argv: [str]) -> Result<[int | str | str], str>` |
| `std.process.run_with_env` | `run_with_env(cmd: str, argv: [str], env: [str]) -> Result<[int | str | str], str>` |
| `std.process.spawn` | `spawn(cmd: str, argv: [str]) -> Result<process, str>` |
| `std.process.wait` | `wait(child: process) -> Result<[int | str | str], str>` |

## `property` — 14 entries ([guide](data.md))

| Function | Signature |
|---|---|
| `std.property.alpha_char` | `alpha_char(seed: int, i: int) -> str` |
| `std.property.alpha_str` | `alpha_str(seed: int, i: int, len: int) -> str` |
| `std.property.boolean` | `boolean(seed: int, i: int) -> bool` |
| `std.property.check` | `check<T>(gen: func(int, int) -> T, shrink: func(T) -> [T], prop: func(T) -> bool, iters: int, seed: int) -> unit` |
| `std.property.for_all_int` | `for_all_int(lo: int, hi: int, prop: func(int) -> bool, iters: int, seed: int) -> unit` |
| `std.property.int_in` | `int_in(seed: int, i: int, lo: int, hi: int) -> int` |
| `std.property.norm_state` | `norm_state(s: int) -> int` |
| `std.property.pick` | `pick(seed: int, i: int, n: int) -> int` |
| `std.property.seed` | `seed(n: int) -> int` |
| `std.property.shrink_int` | `shrink_int(v: int) -> [int]` |
| `std.property.shrink_str` | `shrink_str(s: str) -> [str]` |
| `std.property.shrink_value` | `shrink_value<T>(prop: func(T) -> bool, shrink: func(T) -> [T], v: T) -> T` |
| `std.property.step` | `step(state: int) -> int` |
| `std.property.stream` | `stream(seed: int, i: int) -> int` |

## `regexp` — 7 entries ([guide](regexp.md))

| Function | Signature |
|---|---|
| `Regexp.new` | `new(pat: str) -> Result<regexp, str>` |
| `std.regexp.captures` | `captures(re: regexp, s: str) -> [str]` |
| `std.regexp.compile` | `compile(pat: str) -> Result<regexp, str>` |
| `std.regexp.find` | `find(re: regexp, s: str) -> Option<str>` |
| `std.regexp.is_email` | `is_email(s: str) -> bool` |
| `std.regexp.is_match` | `is_match(re: regexp, s: str) -> bool` |
| `std.regexp.replace_all` | `replace_all(re: regexp, s: str, rep: str) -> str` |

## `result` — 3 entries ([guide](overview.md))

| Function | Signature |
|---|---|
| `result.expect` | `expect<T,E>(res: Result<T, E>, msg: str) -> T` |
| `result.unwrap` | `unwrap<T,E>(res: Result<T, E>) -> T` |
| `result.unwrap_or` | `unwrap_or<T,E>(res: Result<T, E>, default: T) -> T` |

## `set` — 13 entries ([guide](data.md))

| Function | Signature |
|---|---|
| `std.set.diff` | `diff(a: [str], b: [str]) -> [str]` |
| `std.set.has` | `has(s: [str], x: str) -> bool` |
| `std.set.has_int` | `has_int(s: [int], x: int) -> bool` |
| `std.set.insert` | `insert(s: [str], x: str) -> [str]` |
| `std.set.insert_int` | `insert_int(s: [int], x: int) -> [int]` |
| `std.set.intersect` | `intersect(a: [str], b: [str]) -> [str]` |
| `std.set.intersect_int` | `intersect_int(a: [int], b: [int]) -> [int]` |
| `std.set.is_empty` | `is_empty(s: [str]) -> bool` |
| `std.set.len` | `len(s: [str]) -> int` |
| `std.set.remove` | `remove(s: [str], x: str) -> [str]` |
| `std.set.remove_int` | `remove_int(s: [int], x: int) -> [int]` |
| `std.set.union` | `union(a: [str], b: [str]) -> [str]` |
| `std.set.union_int` | `union_int(a: [int], b: [int]) -> [int]` |

## `span` — 2 entries ([guide](overview.md))

| Function | Signature |
|---|---|
| `span.end` | `end(sp: span) -> int` |
| `std.span.end` | `end(sp: span) -> int` |

## `sqlz` — 13 entries ([guide](sql-db.md))

| Function | Signature |
|---|---|
| `std.sqlz.close` | `close(db: db) -> unit` |
| `std.sqlz.exec` | `exec(db: db, sql: str) -> int` |
| `std.sqlz.mysql.close` | `close(db: db) -> unit` |
| `std.sqlz.mysql.connect` | `connect(conninfo: str) -> db` |
| `std.sqlz.mysql.exec` | `exec(db: db, sql: str) -> int` |
| `std.sqlz.mysql.query` | `query<T>(db: db, sql: str) -> [T]` |
| `std.sqlz.open` | `open(path: str) -> db` |
| `std.sqlz.postgres.close` | `close(db: db) -> unit` |
| `std.sqlz.postgres.connect` | `connect(conninfo: str) -> db` |
| `std.sqlz.postgres.exec` | `exec(db: db, sql: str) -> int` |
| `std.sqlz.postgres.query` | `query<T>(db: db, sql: str) -> [T]` |
| `std.sqlz.query` | `query<T>(db: db, sql: str) -> [T]` |
| `std.sqlz.transaction` | `transaction<T>(db: db, f: func(db) -> T) -> Result<T, str>` |

## `std` — 1 entries ([guide](overview.md))

| Function | Signature |
|---|---|
| `std.chan` | `chan() -> chan` |

## `str` — 36 entries ([guide](str-vec.md))

| Function | Signature |
|---|---|
| `std.str.builder` | `builder() -> [str]` |
| `std.str.builder_len` | `builder_len(b: [str]) -> int` |
| `std.str.byte_len` | `byte_len(s: str) -> int` |
| `std.str.bytes` | `bytes(s: str) -> [int]` |
| `std.str.chr` | `chr(cp: int) -> str` |
| `std.str.classify` | `classify(text: str, markers: [str], bstart: str, bend: str, nested: bool, whole: bool) -> [int]` |
| `std.str.contains` | `contains(s: str, sub: str) -> bool` |
| `std.str.count` | `count(s: str, sub: str) -> int` |
| `std.str.count_in` | `count_in(s: str, sub: str, start: int, end: int) -> int` |
| `std.str.ends_with` | `ends_with(s: str, suffix: str) -> bool` |
| `std.str.ends_with_at` | `ends_with_at(s: str, sub: str, pos: int) -> bool` |
| `std.str.find` | `find(s: str, sub: str, from: int) -> int` |
| `std.str.find_in` | `find_in(s: str, sub: str, start: int, end: int) -> int` |
| `std.str.finish` | `finish(b: [str]) -> str` |
| `std.str.is_empty` | `is_empty(s: str) -> bool` |
| `std.str.join` | `join(items: [str], sep: str) -> str` |
| `std.str.join_parts` | `join_parts(parts: [str], sep: str) -> str` |
| `std.str.length` | `length(s: str) -> int` |
| `std.str.ord` | `ord(ch: str) -> int` |
| `std.str.pad_left` | `pad_left(s: str, len: int, pad: str) -> str` |
| `std.str.pad_right` | `pad_right(s: str, len: int, pad: str) -> str` |
| `std.str.push_part` | `push_part(b: [str], s: str) -> [str]` |
| `std.str.repeat` | `repeat(s: str, n: int) -> str` |
| `std.str.replace` | `replace(s: str, old: str, new: str) -> str` |
| `std.str.reverse` | `reverse(s: str) -> str` |
| `std.str.rfind` | `rfind(s: str, sub: str, from: int) -> int` |
| `std.str.rfind_in` | `rfind_in(s: str, sub: str, start: int, end: int) -> int` |
| `std.str.split` | `split(s: str, sep: str) -> [str]` |
| `std.str.starts_with` | `starts_with(s: str, prefix: str) -> bool` |
| `std.str.starts_with_at` | `starts_with_at(s: str, sub: str, pos: int) -> bool` |
| `std.str.to_lower` | `to_lower(s: str) -> str` |
| `std.str.to_upper` | `to_upper(s: str) -> str` |
| `std.str.trim` | `trim(s: str) -> str` |
| `std.str.trim_end` | `trim_end(s: str) -> str` |
| `std.str.trim_span` | `trim_span(s: str, start: int, end: int) -> [int]` |
| `std.str.trim_start` | `trim_start(s: str) -> str` |

## `sys` — 6 entries ([guide](cell-path-sys.md))

| Function | Signature |
|---|---|
| `std.sys.arch` | `arch() -> str` |
| `std.sys.avail_mem` | `avail_mem() -> int` |
| `std.sys.cpu_count` | `cpu_count() -> int` |
| `std.sys.hostname` | `hostname() -> str` |
| `std.sys.os` | `os() -> str` |
| `std.sys.total_mem` | `total_mem() -> int` |

## `task` — 3 entries ([guide](concurrency.md))

| Function | Signature |
|---|---|
| `std.task.join` | `join<T>(handle: task.join) -> T` |
| `std.task.spawn` | `spawn<T>(f: func(T) -> T) -> task.join` |
| `std.task.try_join` | `try_join<T>(handle: task.join) -> Option<Result<T, str>>` |

## `term` — 7 entries ([guide](terminal.md))

| Function | Signature |
|---|---|
| `std.term.disable_raw` | `disable_raw() -> Result<unit, str>` |
| `std.term.enable_raw` | `enable_raw() -> Result<unit, str>` |
| `std.term.flush` | `flush() -> unit` |
| `std.term.get_size` | `get_size() -> Result<[int], str>` |
| `std.term.is_tty` | `is_tty() -> bool` |
| `std.term.poll` | `poll(ms: int) -> Result<bool, str>` |
| `std.term.read_key` | `read_key() -> Result<int, str>` |

## `test` — 20 entries ([guide](testing-lib.md))

| Function | Signature |
|---|---|
| `std.test.assert` | `assert(cond: bool) -> unit` |
| `std.test.assert_approx_eq` | `assert_approx_eq(left: float, right: float, epsilon: float) -> unit` |
| `std.test.assert_contains` | `assert_contains(hay: str, needle: str) -> unit` |
| `std.test.assert_ends_with` | `assert_ends_with(s: str, suffix: str) -> unit` |
| `std.test.assert_eq` | `assert_eq<T>(left: T, right: T) -> unit` |
| `std.test.assert_gt` | `assert_gt<T,U>(left: T, right: U) -> unit` |
| `std.test.assert_is_err` | `assert_is_err<T,E>(val: Result<T, E>) -> unit` |
| `std.test.assert_is_none` | `assert_is_none<T>(val: Option<T>) -> unit` |
| `std.test.assert_is_ok` | `assert_is_ok<T,E>(val: Result<T, E>) -> unit` |
| `std.test.assert_is_some` | `assert_is_some<T>(val: Option<T>) -> unit` |
| `std.test.assert_len` | `assert_len<T>(val: T, n: int) -> unit` |
| `std.test.assert_lt` | `assert_lt<T,U>(left: T, right: U) -> unit` |
| `std.test.assert_ne` | `assert_ne<T>(left: T, right: T) -> unit` |
| `std.test.assert_snapshot` | `assert_snapshot(actual: str, name: str) -> unit` |
| `std.test.assert_starts_with` | `assert_starts_with(s: str, prefix: str) -> unit` |
| `std.test.black_box` | `black_box<T>(val: T) -> T` |
| `std.test.fail` | `fail(msg: str) -> unit` |
| `std.test.free_port` | `free_port() -> Result<int, str>` |
| `std.test.panic` | `panic(msg: str) -> unit` |
| `std.test.temp_dir` | `temp_dir(prefix: str) -> Result<str, str>` |

## `time` — 28 entries ([guide](math-time.md))

| Function | Signature |
|---|---|
| `std.time.add_days` | `add_days(d: {str: int}, n: int) -> {str: int}` |
| `std.time.civil_from_days` | `civil_from_days(z: int) -> {str: int}` |
| `std.time.date_valid` | `date_valid(y: int, m: int, d: int) -> bool` |
| `std.time.days_from_civil` | `days_from_civil(y: int, m: int, d: int) -> int` |
| `std.time.days_in_month` | `days_in_month(y: int, m: int) -> int` |
| `std.time.diff_days` | `diff_days(a: {str: int}, b: {str: int}) -> int` |
| `std.time.epoch_fallback` | `epoch_fallback() -> {str: int}` |
| `std.time.format_rfc3339` | `format_rfc3339(d: {str: int}) -> str` |
| `std.time.from_epoch_days` | `from_epoch_days(z: int) -> {str: int}` |
| `std.time.is_leap` | `is_leap(y: int) -> bool` |
| `std.time.make_date` | `make_date(y: int, mo: int, d: int, h: int, mi: int, se: int) -> {str: int}` |
| `std.time.monotonic_nanos` | `monotonic_nanos() -> int` |
| `std.time.now_micros` | `now_micros() -> int` |
| `std.time.now_ms` | `now_ms() -> int` |
| `std.time.now_nanos` | `now_nanos() -> int` |
| `std.time.pad2` | `pad2(n: int) -> str` |
| `std.time.parse_rfc3339` | `parse_rfc3339(s: str) -> {str: int}` |
| `std.time.sleep_micros` | `sleep_micros(micros: int) -> unit` |
| `std.time.sleep_ms` | `sleep_ms(ms: int) -> unit` |
| `std.time.to_epoch_days` | `to_epoch_days(d: {str: int}) -> int` |
| `time.micros` | `micros(us: int) -> int` |
| `time.millis` | `millis(ms: int) -> int` |
| `time.secs` | `secs(s: int) -> int` |
| `time.sleep` | `sleep(d: int) -> unit` |
| `time.to_micros` | `to_micros(d: int) -> int` |
| `time.to_millis` | `to_millis(d: int) -> int` |
| `time.to_nanos` | `to_nanos(d: int) -> int` |
| `time.to_secs` | `to_secs(d: int) -> int` |

## `tls` — 14 entries ([guide](overview.md))

| Function | Signature |
|---|---|
| `std.tls.close` | `close(stream: tls) -> Result<unit, str>` |
| `std.tls.connect` | `connect(addr: str, server_name: str, timeout_ms: int) -> Result<tls, str>` |
| `std.tls.connect_insecure` | `connect_insecure(addr: str, server_name: str, timeout_ms: int) -> Result<tls, str>` |
| `std.tls.connect_with_ca` | `connect_with_ca(addr: str, server_name: str, ca_file: str, timeout_ms: int) -> Result<tls, str>` |
| `std.tls.peer_addr` | `peer_addr(stream: tls) -> Result<str, str>` |
| `std.tls.read` | `read(stream: tls, max_bytes: int) -> Result<str, str>` |
| `std.tls.read_bytes` | `read_bytes(stream: tls, max_bytes: int) -> Result<bytes, str>` |
| `std.tls.read_exact` | `read_exact(stream: tls, n: int) -> Result<bytes, str>` |
| `std.tls.read_line` | `read_line(stream: tls) -> Result<str, str>` |
| `std.tls.set_read_timeout` | `set_read_timeout(stream: tls, ms: int) -> Result<bool, str>` |
| `std.tls.set_write_timeout` | `set_write_timeout(stream: tls, ms: int) -> Result<bool, str>` |
| `std.tls.shutdown` | `shutdown(stream: tls) -> Result<unit, str>` |
| `std.tls.write` | `write(stream: tls, data: str) -> Result<int, str>` |
| `std.tls.write_bytes` | `write_bytes(stream: tls, data: bytes) -> Result<int, str>` |

## `tls_listener` — 3 entries ([guide](overview.md))

| Function | Signature |
|---|---|
| `std.tls_listener.accept` | `accept(listener: tls_listener) -> Result<tls, str>` |
| `std.tls_listener.close` | `close(listener: tls_listener) -> Result<bool, str>` |
| `std.tls_listener.shutdown` | `shutdown(listener: tls_listener) -> Result<bool, str>` |

## `unix` — 10 entries ([guide](overview.md))

| Function | Signature |
|---|---|
| `std.unix.close` | `close(stream: unix) -> Result<unit, str>` |
| `std.unix.read` | `read(stream: unix, max_bytes: int) -> Result<str, str>` |
| `std.unix.read_bytes` | `read_bytes(stream: unix, max_bytes: int) -> Result<bytes, str>` |
| `std.unix.read_exact` | `read_exact(stream: unix, n: int) -> Result<bytes, str>` |
| `std.unix.read_line` | `read_line(stream: unix) -> Result<str, str>` |
| `std.unix.set_read_timeout` | `set_read_timeout(stream: unix, ms: int) -> Result<bool, str>` |
| `std.unix.set_write_timeout` | `set_write_timeout(stream: unix, ms: int) -> Result<bool, str>` |
| `std.unix.shutdown` | `shutdown(stream: unix) -> Result<unit, str>` |
| `std.unix.write` | `write(stream: unix, data: str) -> Result<int, str>` |
| `std.unix.write_bytes` | `write_bytes(stream: unix, data: bytes) -> Result<int, str>` |

## `unix_listener` — 3 entries ([guide](overview.md))

| Function | Signature |
|---|---|
| `std.unix_listener.accept` | `accept(listener: unix_listener) -> Result<unix, str>` |
| `std.unix_listener.close` | `close(listener: unix_listener) -> Result<bool, str>` |
| `std.unix_listener.shutdown` | `shutdown(listener: unix_listener) -> Result<bool, str>` |

## `uuid` — 4 entries ([guide](encoding-crypto.md))

| Function | Signature |
|---|---|
| `std.uuid.is_valid` | `is_valid(s: str) -> bool` |
| `std.uuid.parse` | `parse(s: str) -> Result<str, str>` |
| `std.uuid.v4` | `v4() -> str` |
| `std.uuid.v7` | `v7() -> str` |

## `vec` — 24 entries ([guide](str-vec.md))

| Function | Signature |
|---|---|
| `std.vec.concat` | `concat(a: [int], b: [int]) -> [int]` |
| `std.vec.contains` | `contains<T>(v: [T], x: T) -> bool` |
| `std.vec.enumerate` | `enumerate<T>(v: [T]) -> [(int, T)]` |
| `std.vec.flatten` | `flatten(arr: [[int]]) -> [int]` |
| `std.vec.fold` | `fold(arr: [int], init: int) -> int` |
| `std.vec.index_of` | `index_of(arr: [int], target: int) -> int` |
| `std.vec.insert` | `insert<T>(v: [T], idx: int, x: T) -> [T]` |
| `std.vec.join` | `join<T>(v: [T], sep: str) -> str` |
| `std.vec.last_index_of` | `last_index_of(arr: [int], target: int) -> int` |
| `std.vec.len` | `len<T>(v: [T]) -> int` |
| `std.vec.max_val` | `max_val<T>(arr: [T]) -> Option<T>` |
| `std.vec.min_val` | `min_val<T>(arr: [T]) -> Option<T>` |
| `std.vec.pop` | `pop<T>(v: [T]) -> [T]` |
| `std.vec.product` | `product(arr: [int]) -> int` |
| `std.vec.product_f` | `product_f(arr: [float]) -> float` |
| `std.vec.push` | `push<T>(v: [T], x: T) -> [T]` |
| `std.vec.remove` | `remove<T>(v: [T], idx: int) -> [T]` |
| `std.vec.reverse` | `reverse<T>(v: [T]) -> [T]` |
| `std.vec.sort` | `sort<T>(v: [T]) -> [T]` |
| `std.vec.sort_by` | `sort_by<T>(v: [T], cmp: func(T, T) -> int) -> [T]` |
| `std.vec.sort_by_desc` | `sort_by_desc<T>(v: [T], cmp: func(T, T) -> int) -> [T]` |
| `std.vec.sort_key` | `sort_key<T,U>(v: [T], key: func(T) -> U) -> [T]` |
| `std.vec.sum` | `sum(arr: [int]) -> int` |
| `std.vec.sum_f` | `sum_f(arr: [float]) -> float` |

Top-level built-ins live under `builtins` above. Method aliases (`File.open`, `Regexp.is_match`) dispatch on receivers — see [Built-ins](builtins.md) and [Regexp](regexp.md).
