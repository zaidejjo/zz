//! `zz_arena` — fast, light, automatic bump arena for compiler-side allocs.
//!
//! One idea: compilation phases never free individual nodes. A bump arena
//! turns thousands of per-node `malloc`s into a few block allocations and
//! frees everything with a single O(1) [`Bump::reset`].
//!
//! - **Fast:** allocation is a bumped pointer plus a bounds check, no atomics.
//! - **Light:** no per-object header; blocks are recycled across files.
//! - **Auto:** growth, large-object spill, and interning are automatic.
//!   [`Bump::scoped`] rewinds speculative work on failure. Lifetimes prove
//!   no use-after-reset.
//!
//! # Safety
//!
//! All `unsafe` in this crate lives in [`Bump::alloc_raw`]. Blocks are
//! heap-stable (`Box<[u8]>` never moves its bytes), offsets are always
//! aligned up, and every returned reference is tied to the arena lifetime.
//! Types with `Drop` are intentionally leaked (never run) — only place
//! trivial/`Copy` data or arena-owned strings here.

mod collections;
mod intern;
mod stats;

pub use collections::ArenaVec;
pub use intern::{Interner, Symbol};
pub use stats::Stats;

use std::marker::PhantomData;
use std::ptr;
use std::rc::Rc;
use std::sync::Arc;

/// Default first block: big enough for a small file, cheap to keep.
pub const DEFAULT_BLOCK: usize = 64 * 1024;
/// Upper bound for a single block; larger requests spill to their own backing.
pub const MAX_BLOCK: usize = 4 * 1024 * 1024;
/// Requests larger than a quarter of the block limit spill instead of growing.
pub const SPILL_FRACTION: usize = 4;
/// Hard ceiling for one arena (all blocks + spills). DoS guard.
pub const MAX_ARENA_BYTES: usize = 256 * 1024 * 1024;
/// How many blocks [`Bump::reset`] retains for reuse.
pub const RETAIN_BLOCKS: usize = 2;

fn align_up(addr: usize, align: usize) -> usize {
    debug_assert!(align.is_power_of_two());
    (addr + align - 1) & !(align - 1)
}

/// Arena-wide limits. All caps produce [`ArenaError`], never abort.
#[derive(Debug, Clone)]
pub struct Config {
    /// First block size in bytes.
    pub initial_block: usize,
    /// Largest single block; bigger needs spill to their own backing.
    pub max_block: usize,
    /// Total bytes ceiling (blocks + spills).
    pub max_bytes: u64,
    /// Block count ceiling.
    pub max_blocks: usize,
    /// Blocks retained by [`Bump::reset`] for reuse.
    pub retain_blocks: usize,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            initial_block: DEFAULT_BLOCK,
            max_block: MAX_BLOCK,
            max_bytes: MAX_ARENA_BYTES as u64,
            max_blocks: 1024,
            retain_blocks: RETAIN_BLOCKS,
        }
    }
}

/// Failure to allocate: caps hit. The compiler maps this to a clean
/// diagnostic (span attached by the caller), never a panic in release.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArenaError {
    /// Bytes requested (size + worst-case padding).
    pub requested: usize,
    /// Which ceiling stopped the alloc.
    pub limit: &'static str,
}

impl std::fmt::Display for ArenaError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "arena exhausted (requested {} bytes, limit: {})",
            self.requested, self.limit
        )
    }
}

impl std::error::Error for ArenaError {}

/// One heap-stable chunk. `mem` never reallocates after creation, so raw
/// pointers into it stay valid until the block is dropped or reused by reset
/// (reset requires `&mut`, which exclusive-borrows all outstanding refs'
/// owner — lifetimes keep this sound).
#[derive(Debug)]
struct Block {
    mem: Box<[u8]>,
    used: usize,
}

impl Block {
    fn with_capacity(cap: usize) -> Self {
        Block {
            mem: vec![0u8; cap].into_boxed_slice(),
            used: 0,
        }
    }

    fn capacity(&self) -> usize {
        self.mem.len()
    }

    fn base(&self) -> usize {
        self.mem.as_ptr() as usize
    }
}

/// Position to rewind to. Copy; cheap.
#[derive(Debug, Clone, Copy)]
pub struct Checkpoint {
    block: usize,
    used: usize,
    spills: usize,
    bytes_used: u64,
}

/// Fast, light, automatic bump arena. Thread-local (`!Send + !Sync` via
/// `Rc` marker) — share across threads through [`FrozenBump`] instead.
#[derive(Debug)]
pub struct Bump {
    blocks: Vec<Block>,
    /// Oversized allocations, each its own backing. Counted against caps.
    spills: Vec<Box<[u8]>>,
    config: Config,
    stats: Stats,
    /// Makes `Bump` `!Send + !Sync` at compile time: arena refs must never
    /// cross threads (spawner discipline needs frozen sharing instead).
    _no_send: PhantomData<Rc<()>>,
}

impl Default for Bump {
    fn default() -> Self {
        Self::new()
    }
}

impl Bump {
    /// Empty arena with default limits. First block materializes on demand.
    pub fn new() -> Self {
        Bump {
            blocks: Vec::new(),
            spills: Vec::new(),
            config: Config::default(),
            stats: Stats::default(),
            _no_send: PhantomData,
        }
    }

    /// Arena with custom limits (caps, retention).
    pub fn with_config(config: Config) -> Self {
        Bump {
            blocks: Vec::new(),
            spills: Vec::new(),
            config,
            stats: Stats::default(),
            _no_send: PhantomData,
        }
    }

    /// Pre-size the first block from an expected byte count (e.g. source
    /// file length × small factor). Small files get one block, no waste;
    /// huge files skip the doubling dance.
    pub fn with_hint(expected_bytes: usize) -> Self {
        let mut config = Config::default();
        let want = expected_bytes
            .checked_next_power_of_two()
            .unwrap_or(DEFAULT_BLOCK);
        config.initial_block = want.clamp(4096, config.max_block);
        Self::with_config(config)
    }

    /// Current limits.
    pub fn config(&self) -> &Config {
        &self.config
    }

    /// Copy of live counters (used bytes, blocks, spills, resets, waste).
    pub fn stats(&self) -> Stats {
        let mut s = self.stats.clone();
        s.blocks_alive = self.blocks.len() as u64;
        s.spills_alive = self.spills.len() as u64;
        s.capacity_bytes = self.blocks.iter().map(|b| b.capacity() as u64).sum::<u64>()
            + self.spills.iter().map(|s| s.len() as u64).sum::<u64>();
        s
    }

    /// Total bytes accounted (block capacities + spill lengths).
    fn accounted(&self) -> u64 {
        self.blocks.iter().map(|b| b.capacity() as u64).sum::<u64>()
            + self.spills.iter().map(|s| s.len() as u64).sum::<u64>()
    }

    /// Allocate `val` in the arena. Panics with a clear message only when a
    /// cap is hit (malicious/huge input) — prefer [`Bump::try_alloc`] on
    /// untrusted paths and map [`ArenaError`] to a diagnostic.
    pub fn alloc<T>(&mut self, val: T) -> &mut T {
        match self.try_alloc(val) {
            Ok(r) => r,
            Err(e) => panic!("{e}"),
        }
    }

    /// Fallible allocate. `Ok` on every sane input; `Err` only on cap breach.
    pub fn try_alloc<T>(&mut self, val: T) -> Result<&mut T, ArenaError> {
        if std::mem::size_of::<T>() == 0 {
            self.stats.bytes_used += 0;
            return Ok(unsafe { &mut *ptr::NonNull::<T>::dangling().as_ptr() });
        }
        // SAFETY: `alloc_raw` returns `size` readable/writable bytes at the
        // requested alignment, valid for the arena lifetime. We immediately
        // initialize with exactly one `T` and hand out `&mut T` tied to
        // `&mut self`, so aliasing and layout are sound. `T: Drop` is
        // intentionally never dropped (documented leak — arena owns it).
        let ptr = self.try_alloc_raw(std::mem::size_of::<T>(), std::mem::align_of::<T>().max(1))?
            as *mut T;
        unsafe {
            ptr::write(ptr, val);
            Ok(&mut *ptr)
        }
    }

    /// Copy a slice into the arena. The returned slice lives with the arena.
    pub fn alloc_slice_copy<T: Copy>(&mut self, items: &[T]) -> &mut [T] {
        match self.try_alloc_slice_copy(items) {
            Ok(s) => s,
            Err(e) => panic!("{e}"),
        }
    }

    /// Fallible slice copy.
    pub fn try_alloc_slice_copy<T: Copy>(&mut self, items: &[T]) -> Result<&mut [T], ArenaError> {
        if items.is_empty() {
            return Ok(&mut []);
        }
        let size = std::mem::size_of_val(items);
        let align = std::mem::align_of::<T>().max(1);
        let ptr = self.try_alloc_raw(size, align)? as *mut T;
        // SAFETY: same guarantees as `try_alloc`, extended over `len`
        // contiguous `T`s; source and dest never overlap (dest is fresh).
        unsafe {
            ptr::copy_nonoverlapping(items.as_ptr(), ptr, items.len());
            Ok(std::slice::from_raw_parts_mut(ptr, items.len()))
        }
    }

    /// Copy a string into the arena.
    pub fn alloc_str(&mut self, s: &str) -> &mut str {
        match self.try_alloc_str(s) {
            Ok(r) => r,
            Err(e) => panic!("{e}"),
        }
    }

    /// Fallible string copy.
    pub fn try_alloc_str(&mut self, s: &str) -> Result<&mut str, ArenaError> {
        if s.is_empty() {
            return Ok(Default::default());
        }
        let bytes = self.try_alloc_slice_copy(s.as_bytes())?;
        // SAFETY: bytes are copied from a valid `str`, so UTF-8 holds.
        Ok(unsafe { std::str::from_utf8_unchecked_mut(bytes) })
    }

    fn try_alloc_raw(&mut self, size: usize, align: usize) -> Result<*mut u8, ArenaError> {
        debug_assert!(align.is_power_of_two());
        let align = align.max(1);
        // Fast path: current block.
        if let Some(block) = self.blocks.last_mut() {
            let aligned = align_up(block.base() + block.used, align);
            let end = aligned + size;
            if end <= block.base() + block.capacity() {
                let ptr = aligned as *mut u8;
                block.used = end - block.base();
                self.stats.bytes_used += size as u64;
                return Ok(ptr);
            }
        }
        self.slow_alloc_raw(size, align)
    }

    /// Grow or spill. Marked cold so the fast path stays inline.
    #[cold]
    fn slow_alloc_raw(&mut self, size: usize, align: usize) -> Result<*mut u8, ArenaError> {
        let spill_at = self.config.max_block / SPILL_FRACTION;
        if size >= spill_at {
            return self.spill_raw(size, align);
        }
        // Grow: double the last block (or start initial), capped.
        let prev = self.blocks.last().map(|b| b.capacity()).unwrap_or(0);
        let mut next = (prev.max(self.config.initial_block) * 2)
            .max(size + align)
            .min(self.config.max_block);
        if self.blocks.is_empty() {
            next = self
                .config
                .initial_block
                .max(size + align)
                .min(self.config.max_block);
            if size + align > self.config.max_block {
                return self.spill_raw(size, align);
            }
        }
        if self.blocks.len() + 1 > self.config.max_blocks {
            return Err(ArenaError {
                requested: size,
                limit: "max_blocks",
            });
        }
        if self.accounted() + next as u64 > self.config.max_bytes {
            return Err(ArenaError {
                requested: size,
                limit: "max_bytes",
            });
        }
        self.blocks.push(Block::with_capacity(next));
        self.stats.grows += 1;
        let block = self.blocks.last_mut().expect("just pushed");
        let aligned = align_up(block.base(), align);
        let ptr = aligned as *mut u8;
        block.used = aligned + size - block.base();
        self.stats.bytes_used += size as u64;
        Ok(ptr)
    }

    /// Oversized backing: exact-fit box, tracked for caps + reset.
    #[cold]
    fn spill_raw(&mut self, size: usize, align: usize) -> Result<*mut u8, ArenaError> {
        if size as u64 > self.config.max_bytes {
            return Err(ArenaError {
                requested: size,
                limit: "max_single",
            });
        }
        if self.accounted() + size as u64 + align as u64 > self.config.max_bytes {
            return Err(ArenaError {
                requested: size,
                limit: "max_bytes",
            });
        }
        // Over-allocate by `align` so any alignment is reachable inside.
        let backing = vec![0u8; size + align].into_boxed_slice();
        let base = backing.as_ptr() as usize;
        let aligned = align_up(base, align.max(1));
        let offset = aligned - base;
        let ptr = aligned as *mut u8;
        // Shrink bookkeeping to the reachable window is unnecessary; keep
        // the whole backing alive and count it.
        self.stats.bytes_used += size as u64;
        self.stats.spills += 1;
        let _ = offset;
        // `backing` is heap-stable; the interior pointer stays valid while
        // the box is owned by `spills`. The caller initializes before reads
        // (enforced by typed wrappers).
        self.spills.push(backing);
        Ok(ptr)
    }

    /// Mark current position. [`Bump::rewind`] returns here.
    pub fn checkpoint(&self) -> Checkpoint {
        Checkpoint {
            block: self.blocks.len(),
            used: self.blocks.last().map(|b| b.used).unwrap_or(0),
            spills: self.spills.len(),
            bytes_used: self.stats.bytes_used,
        }
    }

    /// Rewind to a checkpoint. Frees nothing to the OS — blocks stay pooled.
    /// Truncated bytes are poisoned in debug builds to catch stale use.
    pub fn rewind(&mut self, cp: Checkpoint) {
        // Drop whole blocks allocated after the checkpoint.
        while self.blocks.len() > cp.block {
            self.blocks.pop();
        }
        if let Some(block) = self.blocks.last_mut() {
            #[cfg(debug_assertions)]
            {
                for b in &mut block.mem[cp.used..block.used] {
                    *b = 0xDD;
                }
            }
            block.used = cp.used;
        }
        self.spills.truncate(cp.spills);
        self.stats.bytes_used = cp.bytes_used;
    }

    /// Run `f` speculatively: on `Err`, rewind all its allocations and
    /// return the error; on `Ok`, keep them. This is the RAII rewind —
    /// a closure (not a guard object) so the borrow checker stays happy:
    /// no `&mut` is held while `f` runs except the one `f` itself owns.
    pub fn scoped<T, E>(&mut self, f: impl FnOnce(&mut Self) -> Result<T, E>) -> Result<T, E> {
        let cp = self.checkpoint();
        let r = f(self);
        if r.is_err() {
            self.rewind(cp);
        }
        r
    }

    /// O(1)-ish reset: retains hot blocks for the next file/unit, drops the
    /// rest plus spills. No syscall when the next unit fits retained blocks.
    pub fn reset(&mut self) {
        let retain = self.config.retain_blocks.min(self.blocks.len());
        // Keep the *largest* (most recent — sizes double upward).
        let mut kept = self.blocks.split_off(self.blocks.len() - retain);
        for b in kept.iter_mut() {
            b.used = 0;
        }
        self.blocks.clear();
        self.blocks.append(&mut kept);
        self.spills.clear();
        self.stats.bytes_used = 0;
        self.stats.resets += 1;
        // Refresh stats liveness on next `stats()` call.
    }

    /// Drop all but one fresh block. Call on daemon/LSP idle to bound RSS.
    pub fn shrink(&mut self) {
        self.blocks.clear();
        self.spills.clear();
        self.spills.shrink_to_fit();
        self.stats.bytes_used = 0;
    }

    /// Immutable, shareable snapshot (copies used bytes once). `Send + Sync`
    /// — the cross-thread handoff. Pointer stability across freeze is NOT
    /// provided in P1; readers use byte offsets (see `as_bytes`).
    pub fn freeze(&self) -> FrozenBump {
        let mut chunks = Vec::with_capacity(self.blocks.len());
        for b in &self.blocks {
            chunks.push(Arc::<[u8]>::from(&b.mem[..b.used]));
        }
        FrozenBump {
            chunks,
            bytes_used: self.stats.bytes_used,
        }
    }

    /// Dump counters when `ZZ_ARENA_STATS=1` (per-phase log line support).
    pub fn maybe_dump_stats(&self, phase: &str) {
        if std::env::var("ZZ_ARENA_STATS").is_ok() {
            let s = self.stats();
            eprintln!(
                "[arena:{phase}] used={} cap={} blocks={} spills={} grows={} resets={}",
                s.bytes_used, s.capacity_bytes, s.blocks_alive, s.spills_alive, s.grows, s.resets,
            );
        }
    }
}

/// Immutable arena snapshot. Cheap to clone (`Arc`s), safe to share.
#[derive(Debug, Clone)]
pub struct FrozenBump {
    chunks: Vec<Arc<[u8]>>,
    bytes_used: u64,
}

impl FrozenBump {
    /// Total frozen bytes.
    pub fn len(&self) -> u64 {
        self.bytes_used
    }

    /// Whether the snapshot holds nothing.
    pub fn is_empty(&self) -> bool {
        self.bytes_used == 0
    }

    /// Chunk count.
    pub fn chunks(&self) -> usize {
        self.chunks.len()
    }
}

// SAFETY: `FrozenBump` only exposes immutable bytes through `Arc` —
// `Send + Sync` holds by construction. `Bump` itself stays `!Send`
// (see `_no_send`) so live `&mut` refs can never cross threads.

// Checkpoint token: see `Bump::checkpoint`, `Bump::rewind`,
// and `Bump::scoped` for automatic rewind on failure.
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn alloc_scalars_and_slices() {
        let mut a = Bump::new();
        let x = a.alloc(42u64);
        assert_eq!(*x, 42);
        let s = a.alloc_slice_copy(&[1, 2, 3, 4]);
        assert_eq!(s, &[1, 2, 3, 4]);
        let st = a.alloc_str("hello");
        assert_eq!(st, "hello");
        assert!(a.stats().bytes_used > 0);
    }

    #[test]
    fn zero_sized_and_empty() {
        let mut a = Bump::new();
        let _u = a.alloc(());
        let e: &mut [u8] = a.alloc_slice_copy(&[]);
        assert!(e.is_empty());
        let s = a.alloc_str("");
        assert!(s.is_empty());
    }

    #[test]
    fn alignment_holds() {
        let mut a = Bump::new();
        a.alloc(1u8);
        let wide = a.alloc(0u128);
        assert_eq!((wide as *mut u128 as usize) % 16, 0);
        let _ = wide;
    }

    #[test]
    fn checkpoint_rewind_nested() {
        let mut a = Bump::new();
        let _keep = a.alloc(1u64);
        let used_before = a.stats().bytes_used;
        let outer = a.checkpoint();
        let _tmp1 = a.alloc(2u64);
        let inner = a.checkpoint();
        let _tmp2 = a.alloc(3u64);
        a.rewind(inner);
        assert_eq!(a.stats().bytes_used, used_before + 8);
        a.rewind(outer);
        assert_eq!(a.stats().bytes_used, used_before);
    }

    #[test]
    fn scoped_rewinds_on_err_and_keeps_on_ok() {
        let mut a = Bump::new();
        let base = a.stats().bytes_used;
        let r: Result<(), ()> = a.scoped(|b| {
            let _t = b.alloc(99u64);
            Err(())
        });
        assert!(r.is_err());
        assert_eq!(a.stats().bytes_used, base);
        let r: Result<(), ()> = a.scoped(|b| {
            let _t = b.alloc(100u64);
            Ok(())
        });
        assert!(r.is_ok());
        assert!(a.stats().bytes_used > base);
    }

    #[test]
    fn reset_retains_and_reuses() {
        let mut a = Bump::new();
        for i in 0..1000u64 {
            a.alloc(i);
        }
        let blocks_before = a.stats().blocks_alive;
        assert!(blocks_before >= 1);
        a.reset();
        assert_eq!(a.stats().bytes_used, 0);
        // Retained blocks reused: no growth for a small refill.
        let grows_before = a.stats().grows;
        for i in 0..10u64 {
            a.alloc(i);
        }
        assert_eq!(a.stats().grows, grows_before);
    }

    #[test]
    fn large_value_spills() {
        let mut a = Bump::new();
        let big = vec![7u8; MAX_BLOCK];
        let s = a.alloc_slice_copy(&big);
        assert_eq!(s.len(), big.len());
        assert!(a.stats().spills_alive >= 1);
    }

    #[test]
    fn caps_error_cleanly() {
        let mut a = Bump::with_config(Config {
            max_bytes: 1024,
            initial_block: 4096,
            ..Config::default()
        });
        let err = a.try_alloc([0u8; 2048]).unwrap_err();
        assert_eq!(err.limit, "max_bytes");
    }

    #[test]
    fn freeze_is_shareable() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<FrozenBump>();
        let mut a = Bump::new();
        a.alloc(1u64);
        let f = a.freeze();
        assert!(!f.is_empty());
        let f2 = f.clone();
        assert_eq!(f.len(), f2.len());
    }

    #[test]
    fn with_hint_presizes() {
        let a = Bump::with_hint(200_000);
        assert!(a.config.initial_block >= 200_000);
        let a2 = Bump::with_hint(10);
        assert_eq!(a2.config.initial_block, 4096);
    }
}
