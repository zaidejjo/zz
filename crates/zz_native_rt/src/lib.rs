//! ZZ native runtime support library (Phase 0: FFI foundation).
//!
//! A leaf crate with zero workspace dependencies. It owns the process-wide
//! opaque-handle pool shared by both execution engines:
//!
//! - the VM (`zz run`) uses the safe Rust API ([`alloc`], [`payload`],
//!   [`drop_handle`]) directly through `zz_runtime::Value::Opaque`;
//! - AOT binaries (`zz run --native`) link this crate as a static library
//!   (`libzz_native_rt.a`) and call the `extern "C"` `zz_rt_*` functions
//!   declared in the C header emitted by `zz_codegen::ffi`.
//!
//! A handle is a `(tag, id)` pair. The tag names the owning module
//! (`"regex"`, `"args"`, …) and selects the method namespace (`regex.*`);
//! the id is a process-unique `u64` (0 is reserved as invalid). Payloads are
//! type-erased (`Arc<dyn Any + Send + Sync>`) so this crate never depends on
//! module-specific types; each module downcasts payloads it owns.

use std::any::Any;
use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{LazyLock, Mutex};

/// `std.args` CLI argument parsing (raw access + flag parser).
pub mod args;
/// C ABI mirror of the AOT `zz_value` plus linkable runtime constructors.
pub mod cabi;
/// `std.crypto` asymmetric crypto + JWT (Ed25519, RSA-2048, HS256/EdDSA).
pub mod crypto_asym;
/// `std.crypto` core primitives (hashing, HMAC, CSPRNG, constant-time eq).
pub mod crypto_core;
/// `std.crypto` password hashing (Argon2id, bcrypt).
pub mod crypto_pw;
/// `std.encoding` text codecs (base64-bytes preserving binary payloads).
pub mod encoding;
/// `std.log` logging + tracing (levels, sinks, JSON, spans).
pub mod log;
/// `std.process` subprocess control (run, spawn/wait, exit, pid).
pub mod process;
/// `std.regexp` implementation (safe pool API + `extern "C"` FFI).
pub mod regexp;
/// `std.sys` system information (os, arch, cpu, hostname, memory).
pub mod sys;
/// `std.time` high-resolution extension (ns/µs clocks, micro sleeps).
pub mod time_ext;
/// `std.uuid` identifier generation (v4 random, v7 time-ordered).
pub mod uuid;

/// Opaque handle value: the tag of the owning module plus a pool id.
///
/// Cheap to clone (a `u64` and a short `String`); the heavy payload stays in
/// the pool behind an `Arc`.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Handle {
    /// Owning module tag (e.g. `"regex"`). Selects the method namespace.
    pub tag: String,
    /// Pool id. `0` is never allocated (invalid handle sentinel).
    pub id: u64,
}

impl Handle {
    /// The invalid handle (id 0). Never present in the pool.
    pub fn invalid() -> Self {
        Handle {
            tag: String::new(),
            id: 0,
        }
    }

    /// True when this handle can never resolve ([`Handle::invalid`]).
    pub fn is_invalid(&self) -> bool {
        self.id == 0
    }
}

struct Entry {
    tag: String,
    payload: ArcPayload,
}

type ArcPayload = std::sync::Arc<dyn Any + Send + Sync>;

/// Global handle pool, shared by every handle in this process.
static POOL: LazyLock<Mutex<HashMap<u64, Entry>>> = LazyLock::new(|| Mutex::new(HashMap::new()));

/// Monotonic id source. Starts at 1 so id 0 stays invalid forever.
static NEXT_ID: AtomicU64 = AtomicU64::new(1);

fn lock_pool() -> std::sync::MutexGuard<'static, HashMap<u64, Entry>> {
    POOL.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// Allocate a pool entry for `tag`, storing `payload`, and return its handle.
///
/// The caller must hand the returned [`Handle`] to ZZ code; the payload is
/// retrieved later with [`payload`] and released with [`drop_handle`].
pub fn alloc(tag: &str, payload: ArcPayload) -> Handle {
    let id = NEXT_ID.fetch_add(1, Ordering::SeqCst);
    // On counter wrap (practically impossible: 2^64 ids) reuse would collide
    // with live entries; abort loudly instead of aliasing two objects.
    assert!(id != 0, "opaque handle id counter exhausted");
    lock_pool().insert(
        id,
        Entry {
            tag: tag.to_string(),
            payload,
        },
    );
    Handle {
        tag: tag.to_string(),
        id,
    }
}

/// Look up the payload for `id`, returning the stored tag with it.
///
/// Returns `None` for unknown or already-dropped ids. Callers downcast the
/// payload to their concrete type and must check the tag matches what they
/// expect before trusting the downcast target.
pub fn lookup(id: u64) -> Option<(String, ArcPayload)> {
    lock_pool()
        .get(&id)
        .map(|e| (e.tag.clone(), e.payload.clone()))
}

/// Downcast the payload for `id` to `T`, checking the `expected_tag` first.
///
/// Returns `None` when the id is unknown/dropped, the stored tag differs
/// from `expected_tag`, or the payload is not a `T`. Tag mismatch and type
/// mismatch are both reported as `None` (no panics on hostile input).
pub fn payload<T: Any + Send + Sync>(id: u64, expected_tag: &str) -> Option<std::sync::Arc<T>> {
    let (tag, payload) = lookup(id)?;
    if tag != expected_tag {
        return None;
    }
    payload.downcast::<T>().ok()
}

/// The stored tag for `id`, or `None` when unknown/dropped.
pub fn tag_of(id: u64) -> Option<String> {
    lock_pool().get(&id).map(|e| e.tag.clone())
}

/// Release the pool entry for `id`. Returns false when unknown/dropped.
pub fn drop_handle(id: u64) -> bool {
    lock_pool().remove(&id).is_some()
}

/// Number of live pool entries (diagnostics and tests).
pub fn live_count() -> u64 {
    lock_pool().len() as u64
}

/// Protocol version of this FFI surface. AOT binaries built against a
/// different version than the linked static library must be rebuilt.
pub const FFI_VERSION: u64 = 1;

// --- extern "C" surface (linked by AOT binaries) -----------------------------

/// Read a `(ptr, len)` byte string from C. Null/empty/invalid input yields
/// `None` (never UB, never panics).
fn read_tag(ptr: *const u8, len: usize) -> Option<String> {
    if ptr.is_null() || len == 0 || len > 1024 {
        return None;
    }
    // SAFETY: guarded by the null/len checks above; the caller guarantees
    // `len` bytes are readable for the duration of this call.
    let bytes = unsafe { std::slice::from_raw_parts(ptr, len) };
    std::str::from_utf8(bytes).ok().map(|s| s.to_string())
}

/// FFI protocol version (must equal [`FFI_VERSION`]).
#[no_mangle]
pub extern "C" fn zz_rt_version() -> u64 {
    FFI_VERSION
}

/// Allocate an entry for `tag` with a unit payload; returns the id (0 on bad
/// input). Real payloads are installed by module FFI constructors in later
/// phases; this entry point covers tag-only handles and link testing.
#[no_mangle]
pub extern "C" fn zz_rt_handle_alloc(tag_ptr: *const u8, tag_len: usize) -> u64 {
    let Some(tag) = read_tag(tag_ptr, tag_len) else {
        return 0;
    };
    alloc(&tag, std::sync::Arc::new(())).id
}

/// Release the entry for `id`. Idempotent: unknown ids return false.
#[no_mangle]
pub extern "C" fn zz_rt_handle_drop(id: u64) -> bool {
    if id == 0 {
        return false;
    }
    drop_handle(id)
}

/// Number of live pool entries.
#[no_mangle]
pub extern "C" fn zz_rt_handle_live() -> u64 {
    live_count()
}

/// True when `id` is live and its stored tag equals the given tag.
#[no_mangle]
pub extern "C" fn zz_rt_handle_tag_eq(id: u64, tag_ptr: *const u8, tag_len: usize) -> bool {
    if id == 0 {
        return false;
    }
    let Some(tag) = read_tag(tag_ptr, tag_len) else {
        return false;
    };
    lock_pool().get(&id).is_some_and(|e| e.tag == tag)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn alloc_lookup_drop_roundtrip() {
        let h = alloc("regex", std::sync::Arc::new(42u32));
        assert!(!h.is_invalid());
        assert_eq!(tag_of(h.id).as_deref(), Some("regex"));
        let v: Option<std::sync::Arc<u32>> = payload(h.id, "regex");
        assert_eq!(v.as_deref(), Some(&42u32));
        assert!(drop_handle(h.id));
        assert!(!drop_handle(h.id));
        assert_eq!(tag_of(h.id), None);
    }

    #[test]
    fn tag_mismatch_rejects_downcast() {
        let h = alloc("regex", std::sync::Arc::new(7u32));
        let v: Option<std::sync::Arc<u32>> = payload(h.id, "uuid");
        assert!(v.is_none());
        assert!(drop_handle(h.id));
    }

    #[test]
    fn type_mismatch_rejects_downcast() {
        let h = alloc("regex", std::sync::Arc::new(7u32));
        let v: Option<std::sync::Arc<String>> = payload(h.id, "regex");
        assert!(v.is_none());
        assert!(drop_handle(h.id));
    }

    #[test]
    fn invalid_handle_never_resolves() {
        assert!(Handle::invalid().is_invalid());
        assert_eq!(tag_of(0), None);
        assert!(!drop_handle(0));
    }

    #[test]
    fn ffi_guards_reject_bad_input() {
        assert_eq!(zz_rt_handle_alloc(std::ptr::null(), 5), 0);
        assert_eq!(zz_rt_handle_alloc(b"x".as_ptr(), 0), 0);
        assert!(!zz_rt_handle_drop(0));
        assert!(!zz_rt_handle_tag_eq(0, b"x".as_ptr(), 1));
        assert!(!zz_rt_handle_tag_eq(12345, std::ptr::null(), 1));
        assert_eq!(zz_rt_version(), FFI_VERSION);
    }

    #[test]
    fn ffi_alloc_drop_roundtrip() {
        let tag = b"span";
        let id = zz_rt_handle_alloc(tag.as_ptr(), tag.len());
        assert_ne!(id, 0);
        assert!(zz_rt_handle_tag_eq(id, tag.as_ptr(), tag.len()));
        assert!(!zz_rt_handle_tag_eq(id, b"regex".as_ptr(), 5));
        assert!(zz_rt_handle_drop(id));
        assert!(!zz_rt_handle_tag_eq(id, tag.as_ptr(), tag.len()));
    }
}
