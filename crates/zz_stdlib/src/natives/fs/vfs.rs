//! Virtual filesystem providers for `std.fs` (`osfs`, `memfs`, `tarfs`,
//! `embedfs` + the `*_at` operation family).
//!
//! ZZ has no trait system, so the `fs.FS` interface is a handle: an
//! `Opaque("zzfs")` value naming a provider in a process-wide pool. Every
//! `*_at` op takes the handle first and dispatches identically in the VM
//! (here) and AOT (`zz_fs_at_*` in `core.c`).
//!
//! Providers:
//! - `Os` — the real OS filesystem (current `std.fs` behavior).
//! - `Mem` — thread-safe in-memory tree (tests, caches, transient data).
//! - `Tar` — read-only view over a plain (uncompressed) `.tar` archive.
//! - `Embed` — read-only view over `--embed` assets (Task 4; empty unless
//!   the CLI populated it).
//!
//! All VFS paths live in one `/`-separated virtual namespace (backslash
//! accepted as a separator, `.`/`..` resolved lexically via `path`).
//! Diagnostics reuse the unified `fs:<op>:<code>: <path>` shape; write ops
//! on read-only providers fail `invalid_input`.

use std::collections::{HashMap, HashSet};
use std::sync::{Mutex, OnceLock};

use zz_runtime::{EvalError, Interp, Span, Value};

use super::{err_str, ok_str, ok_unit, ok_value, path, run_fs};
use crate::natives::expect_str;

/// Pool tag for FS provider handles. Selects no method namespace — all
/// provider ops are free functions (`fs.read_to_string_at(fsys, path)`).
pub(crate) const VFS_TAG: &str = "zzfs";

/// In-memory file tree shared by `Mem`, `Tar`, and `Embed` providers.
#[derive(Debug, Default)]
pub(crate) struct TreeFs {
    files: HashMap<String, Vec<u8>>,
    dirs: HashSet<String>,
}

impl TreeFs {
    fn get(&self, key: &str) -> Option<&Vec<u8>> {
        self.files.get(key)
    }

    fn contains(&self, key: &str) -> bool {
        self.files.contains_key(key) || self.dirs.contains(key) || key == "/"
    }

    fn is_file(&self, key: &str) -> bool {
        self.files.contains_key(key)
    }

    fn is_dir(&self, key: &str) -> bool {
        key == "/" || self.dirs.contains(key)
    }

    /// Insert a file, creating implied parent dirs. Fails when the path
    /// itself is a directory.
    fn insert(&mut self, key: String, data: Vec<u8>) -> Result<(), &'static str> {
        if self.dirs.contains(&key) {
            return Err("io_error");
        }
        self.mkdir_parents(&key);
        self.files.insert(key, data);
        Ok(())
    }

    /// Create a dir and implied parents. Fails when a file sits at `key`.
    fn mkdir_all(&mut self, key: &str) -> Result<(), &'static str> {
        if self.files.contains_key(key) {
            return Err("already_exists");
        }
        self.mkdir_parents(key);
        if key != "/" {
            self.dirs.insert(key.to_string());
        }
        Ok(())
    }

    fn mkdir_parents(&mut self, key: &str) {
        let segs: Vec<&str> = key.split('/').filter(|s| !s.is_empty()).collect();
        let mut cur = String::new();
        // All segments but the last are implied parent dirs.
        for seg in segs.iter().take(segs.len().saturating_sub(1)) {
            cur.push('/');
            cur.push_str(seg);
            self.dirs.insert(cur.clone());
        }
    }

    fn remove_file(&mut self, key: &str) -> Result<(), &'static str> {
        if self.dirs.contains(key) {
            return Err("io_error");
        }
        self.files.remove(key).map(|_| ()).ok_or("not_found")
    }

    /// Immediate children (basenames) of a dir, sorted. Fails on files.
    fn read_dir(&self, key: &str) -> Result<Vec<String>, &'static str> {
        if self.files.contains_key(key) {
            return Err("io_error");
        }
        if key != "/" && !self.dirs.contains(key) {
            return Err("not_found");
        }
        let prefix = if key == "/" {
            "/".to_string()
        } else {
            format!("{key}/")
        };
        let mut out: Vec<String> = Vec::new();
        for k in self.files.keys().chain(self.dirs.iter()) {
            if let Some(rest) = k.strip_prefix(&prefix) {
                if !rest.is_empty() && !rest.contains('/') {
                    out.push(rest.to_string());
                }
            }
        }
        out.sort();
        out.dedup();
        Ok(out)
    }
}

/// Parse a plain (uncompressed) tar archive into a [`TreeFs`].
/// Supports regular files (`0`/`\0`) and directories (`5`); other entry
/// types (symlinks, devices, pax headers) are skipped. GNU long names
/// (`L`) apply to the following entry.
pub(crate) fn parse_tar(bytes: &[u8]) -> Result<TreeFs, String> {
    let mut tree = TreeFs::default();
    let mut i = 0;
    let mut pending_long: Option<String> = None;
    // A valid tar ends with two zero blocks; a trailing partial block is
    // tolerated (archives streamed through pipes may truncate the pad).
    while i + 512 <= bytes.len() {
        let hdr = &bytes[i..i + 512];
        if hdr.iter().all(|&b| b == 0) {
            break;
        }
        let name_raw = &hdr[..100];
        let name_len = name_raw.iter().position(|&b| b == 0).unwrap_or(100);
        let mut name = String::from_utf8_lossy(&name_raw[..name_len]).into_owned();
        // USTAR prefix (124+155…): `prefix/name`.
        let prefix_raw = &hdr[345..500];
        let prefix_len = prefix_raw.iter().position(|&b| b == 0).unwrap_or(155);
        if prefix_len > 0 {
            let prefix = String::from_utf8_lossy(&prefix_raw[..prefix_len]);
            name = format!("{prefix}/{name}");
        }
        let size_raw = &hdr[124..136];
        let size_str: String = size_raw
            .iter()
            .take_while(|&&b| b != 0 && b != b' ')
            .map(|&b| b as char)
            .collect();
        let size = usize::from_str_radix(size_str.trim(), 8)
            .map_err(|_| format!("malformed size field for entry `{name}`"))?;
        let typeflag = hdr[156];
        i += 512;
        if i + size > bytes.len() {
            return Err(format!("truncated data for entry `{name}`"));
        }
        let data = &bytes[i..i + size];
        i += size.div_ceil(512) * 512;
        match typeflag {
            b'L' => {
                // GNU long name: data block names the next entry.
                let s = String::from_utf8_lossy(data);
                pending_long = Some(s.trim_end_matches('\0').to_string());
            }
            b'0' | 0 => {
                let n = pending_long.take().unwrap_or(name);
                let key = vfs_key(&n);
                tree.insert(key, data.to_vec())
                    .map_err(|c| format!("duplicate entry `{n}` ({c})"))?;
            }
            b'5' => {
                let n = pending_long.take().unwrap_or(name);
                let key = vfs_key(&n);
                tree.mkdir_all(&key)
                    .map_err(|c| format!("conflicting entry `{n}` ({c})"))?;
            }
            _ => {
                pending_long = None;
            }
        }
    }
    Ok(tree)
}

/// Map a user path into the virtual namespace: unix-style normalize,
/// rooted at `/`.
pub(crate) fn vfs_key(p: &str) -> String {
    let n = path::normalize(p, path::Style::Unix);
    if n == "." {
        return "/".to_string();
    }
    if n.starts_with('/') {
        return n;
    }
    if n == ".." || n.starts_with("../") {
        // `..` above the virtual root clamps (matches `normalize` only
        // for rooted inputs — the VFS is always rooted).
        return "/".to_string();
    }
    format!("/{n}")
}

pub(crate) enum Provider {
    Os,
    Mem(TreeFs),
    Tar(TreeFs),
    Embed(TreeFs),
}

impl Provider {
    fn tree(&self) -> Option<&TreeFs> {
        match self {
            Provider::Mem(t) | Provider::Tar(t) | Provider::Embed(t) => Some(t),
            Provider::Os => None,
        }
    }

    fn tree_mut(&mut self) -> Option<&mut TreeFs> {
        match self {
            Provider::Mem(t) => Some(t),
            Provider::Tar(_) | Provider::Embed(_) | Provider::Os => None,
        }
    }
}

pub(crate) struct VfsHandle {
    kind: Mutex<Provider>,
}

static VFS_POOL: OnceLock<Mutex<HashMap<u64, std::sync::Arc<VfsHandle>>>> = OnceLock::new();
static NEXT_VFS_ID: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);

fn vfs_pool() -> &'static Mutex<HashMap<u64, std::sync::Arc<VfsHandle>>> {
    VFS_POOL.get_or_init(|| Mutex::new(HashMap::new()))
}

fn vfs_alloc(kind: Provider) -> u64 {
    use std::sync::atomic::Ordering;
    let id = NEXT_VFS_ID.fetch_add(1, Ordering::SeqCst);
    assert!(id != 0, "fs provider id counter exhausted");
    vfs_pool().lock().unwrap().insert(
        id,
        std::sync::Arc::new(VfsHandle {
            kind: Mutex::new(kind),
        }),
    );
    id
}

fn vfs_lookup(id: u64) -> Option<std::sync::Arc<VfsHandle>> {
    vfs_pool().lock().unwrap().get(&id).cloned()
}

fn vfs_value(id: u64) -> Value {
    ok_value(Value::Opaque(Box::new(zz_native_rt::Handle {
        tag: VFS_TAG.to_string(),
        id,
    })))
}

fn expect_vfs(
    args: &[Value],
    i: usize,
    name: &str,
) -> Result<Result<(u64, std::sync::Arc<VfsHandle>), Value>, EvalError> {
    let handle = match args.get(i) {
        Some(Value::Opaque(h)) if h.tag == VFS_TAG => h.clone(),
        Some(other) => {
            return Err(EvalError::new(
                format!("`{name}` expects an fs provider, found `{other}`"),
                Span::new(0, 0),
            ));
        }
        None => {
            return Err(EvalError::new(
                format!("missing argument for `{name}`"),
                Span::new(0, 0),
            ));
        }
    };
    match vfs_lookup(handle.id) {
        Some(f) => Ok(Ok((handle.id, f))),
        None => Ok(Err(err_str(format!("fs:{name}:closed")))),
    }
}

/// Process-wide embedded asset map, populated by the CLI `--embed` flag
/// (Task 4). Empty in tests and when no assets were embedded.
static EMBED_MAP: OnceLock<Mutex<HashMap<String, Vec<u8>>>> = OnceLock::new();

fn embed_map() -> &'static Mutex<HashMap<String, Vec<u8>>> {
    EMBED_MAP.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Replace the embedded asset map (CLI `--embed` startup).
/// Harvested by `fs.embedfs()` into a read-only snapshot.
pub fn set_embed(files: HashMap<String, Vec<u8>>) {
    *embed_map().lock().unwrap() = files;
}

fn snapshot_embed() -> TreeFs {
    let mut tree = TreeFs::default();
    for (k, v) in embed_map().lock().unwrap().iter() {
        let key = vfs_key(k);
        if tree.insert(key.clone(), v.clone()).is_err() {
            continue;
        }
        // File-only registration: materialize implied parent dirs so
        // predicates and listings agree with Mem/Tar providers.
        let mut cur = String::new();
        let segs: Vec<&str> = key.split('/').filter(|s| !s.is_empty()).collect();
        for seg in segs.iter().take(segs.len().saturating_sub(1)) {
            cur.push('/');
            cur.push_str(seg);
            tree.dirs.insert(cur.clone());
        }
    }
    tree
}

// ── Constructors ─────────────────────────────────────────────────────────────

pub(crate) fn fs_osfs(
    _interp: &mut Interp,
    _args: &mut Vec<Value>,
    _span: Span,
) -> Result<Value, EvalError> {
    Ok(vfs_value(vfs_alloc(Provider::Os)))
}

pub(crate) fn fs_memfs(
    _interp: &mut Interp,
    _args: &mut Vec<Value>,
    _span: Span,
) -> Result<Value, EvalError> {
    Ok(vfs_value(vfs_alloc(Provider::Mem(TreeFs::default()))))
}

pub(crate) fn fs_tarfs(
    interp: &mut Interp,
    args: &mut Vec<Value>,
    span: Span,
) -> Result<Value, EvalError> {
    let path = expect_str(args, 0, "std.fs.tarfs")?;
    run_fs(interp, span, move || match std::fs::read(&path) {
        Err(e) => super::fs_err("tarfs", &path, &e),
        Ok(bytes) => match parse_tar(&bytes) {
            Ok(tree) => vfs_value(vfs_alloc(Provider::Tar(tree))),
            Err(msg) => err_str(format!("fs:tarfs:invalid_input: {path} ({msg})")),
        },
    })
}

pub(crate) fn fs_embedfs(
    _interp: &mut Interp,
    _args: &mut Vec<Value>,
    _span: Span,
) -> Result<Value, EvalError> {
    Ok(vfs_value(vfs_alloc(Provider::Embed(snapshot_embed()))))
}

// ── `*_at` operations ────────────────────────────────────────────────────────
//
// Read-only providers reject writes with `invalid_input`; missing ids
// report `closed` (ids differ across engines and never leak into output).

fn readonly_err(op: &str, path: &str) -> Value {
    err_str(format!(
        "fs:{op}:invalid_input: {path} (read-only filesystem)"
    ))
}

fn code_err(op: &str, path: &str, code: &str) -> Value {
    err_str(format!("fs:{op}:{code}: {path}"))
}

pub(crate) fn fs_read_to_string_at(
    interp: &mut Interp,
    args: &mut Vec<Value>,
    span: Span,
) -> Result<Value, EvalError> {
    let (id, handle) = match expect_vfs(args, 0, "read_to_string_at")? {
        Ok(open) => open,
        Err(closed) => return Ok(closed),
    };
    let raw = expect_str(args, 1, "std.fs.read_to_string_at")?;
    let key = vfs_key(&raw);
    run_fs(interp, span, move || {
        let Some(h) = vfs_lookup(id) else {
            return err_str("fs:read_to_string_at:closed".to_string());
        };
        let _ = handle;
        let guard = h.kind.lock().unwrap();
        match &*guard {
            Provider::Os => match std::fs::read_to_string(&raw) {
                Ok(c) => ok_str(c),
                Err(e) => super::fs_err("read", &raw, &e),
            },
            Provider::Mem(_) | Provider::Tar(_) | Provider::Embed(_) => {
                match guard.tree().and_then(|t| t.get(&key)) {
                    Some(bytes) => match String::from_utf8(bytes.clone()) {
                        Ok(s) => ok_str(s),
                        Err(_) => code_err("read", &raw, "invalid_input"),
                    },
                    None => {
                        let code = if guard.tree().is_some_and(|t| t.is_dir(&key)) {
                            "io_error"
                        } else {
                            "not_found"
                        };
                        code_err("read", &raw, code)
                    }
                }
            }
        }
    })
}

pub(crate) fn fs_read_bytes_at(
    interp: &mut Interp,
    args: &mut Vec<Value>,
    span: Span,
) -> Result<Value, EvalError> {
    let (id, handle) = match expect_vfs(args, 0, "read_bytes_at")? {
        Ok(open) => open,
        Err(closed) => return Ok(closed),
    };
    let raw = expect_str(args, 1, "std.fs.read_bytes_at")?;
    let key = vfs_key(&raw);
    run_fs(interp, span, move || {
        let Some(h) = vfs_lookup(id) else {
            return err_str("fs:read_bytes_at:closed".to_string());
        };
        let _ = handle;
        let guard = h.kind.lock().unwrap();
        match &*guard {
            Provider::Os => match std::fs::read(&raw) {
                Ok(bytes) => ok_value(Value::Array(Box::new(
                    bytes.into_iter().map(|b| Value::Int(b as i64)).collect(),
                ))),
                Err(e) => super::fs_err("read_bytes", &raw, &e),
            },
            Provider::Mem(_) | Provider::Tar(_) | Provider::Embed(_) => {
                match guard.tree().and_then(|t| t.get(&key)) {
                    Some(bytes) => ok_value(Value::Array(Box::new(
                        bytes.iter().map(|b| Value::Int(*b as i64)).collect(),
                    ))),
                    None => {
                        let code = if guard.tree().is_some_and(|t| t.is_dir(&key)) {
                            "io_error"
                        } else {
                            "not_found"
                        };
                        code_err("read_bytes", &raw, code)
                    }
                }
            }
        }
    })
}

pub(crate) fn fs_write_at(
    interp: &mut Interp,
    args: &mut Vec<Value>,
    span: Span,
) -> Result<Value, EvalError> {
    let (id, handle) = match expect_vfs(args, 0, "write_at")? {
        Ok(open) => open,
        Err(closed) => return Ok(closed),
    };
    let raw = expect_str(args, 1, "std.fs.write_at")?;
    let data = expect_str(args, 2, "std.fs.write_at")?;
    let key = vfs_key(&raw);
    run_fs(interp, span, move || {
        let Some(h) = vfs_lookup(id) else {
            return err_str("fs:write_at:closed".to_string());
        };
        let _ = handle;
        let mut guard = h.kind.lock().unwrap();
        match &mut *guard {
            Provider::Os => match std::fs::write(&raw, data.as_bytes()) {
                Ok(()) => ok_unit(),
                Err(e) => super::fs_err("write", &raw, &e),
            },
            Provider::Mem(_) => match guard.tree_mut().unwrap().insert(key, data.into_bytes()) {
                Ok(()) => ok_unit(),
                Err(code) => code_err("write", &raw, code),
            },
            Provider::Tar(_) | Provider::Embed(_) => readonly_err("write", &raw),
        }
    })
}

pub(crate) fn fs_append_at(
    interp: &mut Interp,
    args: &mut Vec<Value>,
    span: Span,
) -> Result<Value, EvalError> {
    let (id, handle) = match expect_vfs(args, 0, "append_at")? {
        Ok(open) => open,
        Err(closed) => return Ok(closed),
    };
    let raw = expect_str(args, 1, "std.fs.append_at")?;
    let data = expect_str(args, 2, "std.fs.append_at")?;
    let key = vfs_key(&raw);
    run_fs(interp, span, move || {
        let Some(h) = vfs_lookup(id) else {
            return err_str("fs:append_at:closed".to_string());
        };
        let _ = handle;
        let mut guard = h.kind.lock().unwrap();
        match &mut *guard {
            Provider::Os => {
                let res = std::fs::OpenOptions::new()
                    .create(true)
                    .append(true)
                    .open(&raw)
                    .and_then(|mut f| {
                        use std::io::Write;
                        f.write_all(data.as_bytes())
                    });
                match res {
                    Ok(()) => ok_unit(),
                    Err(e) => super::fs_err("append", &raw, &e),
                }
            }
            Provider::Mem(_) => {
                let t = guard.tree_mut().unwrap();
                if t.dirs.contains(&key) {
                    return code_err("append", &raw, "io_error");
                }
                let mut cur = t.files.get(&key).cloned().unwrap_or_default();
                cur.extend_from_slice(data.as_bytes());
                match t.insert(key, cur) {
                    Ok(()) => ok_unit(),
                    Err(code) => code_err("append", &raw, code),
                }
            }
            Provider::Tar(_) | Provider::Embed(_) => readonly_err("append", &raw),
        }
    })
}

pub(crate) fn fs_exists_at(
    interp: &mut Interp,
    args: &mut Vec<Value>,
    span: Span,
) -> Result<Value, EvalError> {
    let (id, handle) = match expect_vfs(args, 0, "exists_at")? {
        Ok(open) => open,
        Err(closed) => return Ok(closed),
    };
    let raw = expect_str(args, 1, "std.fs.exists_at")?;
    let key = vfs_key(&raw);
    run_fs(interp, span, move || {
        let Some(h) = vfs_lookup(id) else {
            return Value::Bool(false);
        };
        let _ = handle;
        let guard = h.kind.lock().unwrap();
        match &*guard {
            Provider::Os => Value::Bool(std::path::Path::new(&raw).exists()),
            Provider::Mem(_) | Provider::Tar(_) | Provider::Embed(_) => {
                Value::Bool(guard.tree().is_some_and(|t| t.contains(&key)))
            }
        }
    })
}

fn is_file_inner(h: &std::sync::Arc<VfsHandle>, raw: &str, key: &str) -> Value {
    let guard = h.kind.lock().unwrap();
    match &*guard {
        Provider::Os => Value::Bool(std::path::Path::new(raw).is_file()),
        Provider::Mem(_) | Provider::Tar(_) | Provider::Embed(_) => {
            Value::Bool(guard.tree().is_some_and(|t| t.is_file(key)))
        }
    }
}

pub(crate) fn fs_is_file_at(
    interp: &mut Interp,
    args: &mut Vec<Value>,
    span: Span,
) -> Result<Value, EvalError> {
    let (id, handle) = match expect_vfs(args, 0, "is_file_at")? {
        Ok(open) => open,
        Err(closed) => return Ok(closed),
    };
    let raw = expect_str(args, 1, "std.fs.is_file_at")?;
    let key = vfs_key(&raw);
    run_fs(interp, span, move || {
        let Some(h) = vfs_lookup(id) else {
            return Value::Bool(false);
        };
        let _ = handle;
        is_file_inner(&h, &raw, &key)
    })
}

pub(crate) fn fs_is_dir_at(
    interp: &mut Interp,
    args: &mut Vec<Value>,
    span: Span,
) -> Result<Value, EvalError> {
    let (id, handle) = match expect_vfs(args, 0, "is_dir_at")? {
        Ok(open) => open,
        Err(closed) => return Ok(closed),
    };
    let raw = expect_str(args, 1, "std.fs.is_dir_at")?;
    let key = vfs_key(&raw);
    run_fs(interp, span, move || {
        let Some(h) = vfs_lookup(id) else {
            return Value::Bool(false);
        };
        let _ = handle;
        let guard = h.kind.lock().unwrap();
        match &*guard {
            Provider::Os => Value::Bool(std::path::Path::new(&raw).is_dir()),
            Provider::Mem(_) | Provider::Tar(_) | Provider::Embed(_) => {
                Value::Bool(guard.tree().is_some_and(|t| t.is_dir(&key)))
            }
        }
    })
}

pub(crate) fn fs_read_dir_at(
    interp: &mut Interp,
    args: &mut Vec<Value>,
    span: Span,
) -> Result<Value, EvalError> {
    let (id, handle) = match expect_vfs(args, 0, "read_dir_at")? {
        Ok(open) => open,
        Err(closed) => return Ok(closed),
    };
    let raw = expect_str(args, 1, "std.fs.read_dir_at")?;
    let key = vfs_key(&raw);
    run_fs(interp, span, move || {
        let Some(h) = vfs_lookup(id) else {
            return err_str("fs:read_dir_at:closed".to_string());
        };
        let _ = handle;
        let guard = h.kind.lock().unwrap();
        match &*guard {
            Provider::Os => match std::fs::read_dir(&raw) {
                Ok(rd) => {
                    let mut out: Vec<Value> = Vec::new();
                    for e in rd {
                        match e {
                            Ok(e) => out.push(Value::Str(Box::new(
                                e.file_name().to_string_lossy().into_owned(),
                            ))),
                            Err(e) => return super::fs_err("read_dir", &raw, &e),
                        }
                    }
                    out.sort_by(|a, b| format!("{a}").cmp(&format!("{b}")));
                    ok_value(Value::Array(Box::new(out)))
                }
                Err(e) => super::fs_err("read_dir", &raw, &e),
            },
            Provider::Mem(_) | Provider::Tar(_) | Provider::Embed(_) => {
                match guard.tree().unwrap().read_dir(&key) {
                    Ok(names) => ok_value(Value::Array(Box::new(
                        names.into_iter().map(|s| Value::Str(Box::new(s))).collect(),
                    ))),
                    Err(code) => code_err("read_dir", &raw, code),
                }
            }
        }
    })
}

pub(crate) fn fs_mkdir_all_at(
    interp: &mut Interp,
    args: &mut Vec<Value>,
    span: Span,
) -> Result<Value, EvalError> {
    let (id, handle) = match expect_vfs(args, 0, "mkdir_all_at")? {
        Ok(open) => open,
        Err(closed) => return Ok(closed),
    };
    let raw = expect_str(args, 1, "std.fs.mkdir_all_at")?;
    let key = vfs_key(&raw);
    run_fs(interp, span, move || {
        let Some(h) = vfs_lookup(id) else {
            return err_str("fs:mkdir_all_at:closed".to_string());
        };
        let _ = handle;
        let mut guard = h.kind.lock().unwrap();
        match &mut *guard {
            Provider::Os => match std::fs::create_dir_all(&raw) {
                Ok(()) => ok_unit(),
                Err(e) => super::fs_err("mkdir_all", &raw, &e),
            },
            Provider::Mem(_) => match guard.tree_mut().unwrap().mkdir_all(&key) {
                Ok(()) => ok_unit(),
                Err(code) => code_err("mkdir_all", &raw, code),
            },
            Provider::Tar(_) | Provider::Embed(_) => readonly_err("mkdir_all", &raw),
        }
    })
}

pub(crate) fn fs_remove_file_at(
    interp: &mut Interp,
    args: &mut Vec<Value>,
    span: Span,
) -> Result<Value, EvalError> {
    let (id, handle) = match expect_vfs(args, 0, "remove_file_at")? {
        Ok(open) => open,
        Err(closed) => return Ok(closed),
    };
    let raw = expect_str(args, 1, "std.fs.remove_file_at")?;
    let key = vfs_key(&raw);
    run_fs(interp, span, move || {
        let Some(h) = vfs_lookup(id) else {
            return err_str("fs:remove_file_at:closed".to_string());
        };
        let _ = handle;
        let mut guard = h.kind.lock().unwrap();
        match &mut *guard {
            Provider::Os => match std::fs::remove_file(&raw) {
                Ok(()) => ok_unit(),
                Err(e) => super::fs_err("remove_file", &raw, &e),
            },
            Provider::Mem(_) => match guard.tree_mut().unwrap().remove_file(&key) {
                Ok(()) => ok_unit(),
                Err(code) => code_err("remove_file", &raw, code),
            },
            Provider::Tar(_) | Provider::Embed(_) => readonly_err("remove_file", &raw),
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mem(bytes: &[(&str, &[u8])]) -> TreeFs {
        let mut t = TreeFs::default();
        for (k, v) in bytes {
            t.insert(vfs_key(k), v.to_vec()).unwrap();
        }
        t
    }

    #[test]
    fn keys_rooted() {
        assert_eq!(vfs_key("a/b"), "/a/b");
        assert_eq!(vfs_key("/a/./b/../c"), "/a/c");
        assert_eq!(vfs_key(""), "/");
        assert_eq!(vfs_key(".."), "/");
        assert_eq!(vfs_key("a\\b"), "/a/b");
    }

    #[test]
    fn mem_crud() {
        let mut t = TreeFs::default();
        t.insert(vfs_key("a/b.txt"), b"hi".to_vec()).unwrap();
        assert!(t.is_dir("/a"));
        assert!(t.is_file("/a/b.txt"));
        assert_eq!(t.read_dir("/a").unwrap(), vec!["b.txt".to_string()]);
        assert_eq!(t.read_dir("/").unwrap(), vec!["a".to_string()]);
        assert!(t.mkdir_all("/a").is_ok());
        assert!(t.mkdir_all("/a/b.txt").is_err());
        assert!(t.remove_file("/a").is_err());
        assert!(t.remove_file("/a/b.txt").is_ok());
        assert!(!t.contains("/a/b.txt"));
        let _ = mem(&[("x", b"1")]);
    }

    #[test]
    fn tar_roundtrip() {
        // Build a minimal tar in code: dir + file + long name.
        let mut bytes = Vec::new();
        let mut header = |name: &[u8], size: usize, flag: u8| {
            let mut h = vec![0u8; 512];
            let n = name.len().min(100);
            h[..n].copy_from_slice(&name[..n]);
            let s = format!("{size:011o}\0");
            h[124..124 + s.len()].copy_from_slice(s.as_bytes());
            h[156] = flag;
            h[257..262].copy_from_slice(b"ustar");
            // Checksum over header with checksum field as spaces.
            h[148..156].copy_from_slice(b"        ");
            let sum: u32 = h.iter().map(|&b| b as u32).sum();
            let c = format!("{sum:06o}\0 ");
            h[148..148 + c.len()].copy_from_slice(c.as_bytes());
            bytes.extend_from_slice(&h);
        };
        header(b"docs", 0, b'5');
        header(b"docs/hi.txt", 3, b'0');
        bytes.extend_from_slice(b"hi\n");
        bytes.extend_from_slice(&vec![0u8; 512 - 3]);
        bytes.extend_from_slice(&[0u8; 1024]);
        let t = parse_tar(&bytes).unwrap();
        assert!(t.is_dir("/docs"));
        assert_eq!(t.get("/docs/hi.txt").unwrap(), b"hi\n");
        assert_eq!(t.read_dir("/").unwrap(), vec!["docs".to_string()]);
    }

    #[test]
    fn tar_rejects_truncation() {
        assert!(parse_tar(&[1u8; 600]).is_err());
    }
}
