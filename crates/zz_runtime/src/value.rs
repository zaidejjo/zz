//! Runtime values for the Phase 1 tree-walker.

use std::cell::RefCell;
use std::collections::VecDeque;
use std::collections::{HashMap, HashSet};
use std::fmt;
use std::net::{TcpListener, TcpStream};
use std::rc::Rc;
use std::sync::{
    atomic::{AtomicBool, AtomicUsize},
    Arc, Condvar, Mutex,
};
use zz_frontend::ast::{Block, Expr, Param};
use zz_frontend::span::Span;

use crate::env::EnvLink;
use crate::lf_chan::LfRing;
use crate::vm::{Chunk, Op};

/// Cached frozen parent per spawner chain: `(parent shape, reachable
/// set, frozen parent, pinned parent)`. Hits share the frozen ancestors
/// by pointer; misses freeze once. `loads` is a pure function of the
/// spawn-site chunk, so the `reachable` key covers it.
///
/// The pinned parent link is load-bearing for soundness: shape keys are
/// allocation addresses, and a freed scope's address can be reused by a
/// later scope (new call frame of the same function!). The pin keeps the
/// cached scopes alive, so an address match implies the SAME object —
/// without it, a stale frozen parent (holding the previous call's
/// channel!) would be served to the next call and workers would send
/// into a dead channel while the spawner drains an empty one.
pub type SpawnKeepCache = Option<(
    Vec<(usize, usize, u64)>,
    HashSet<String>,
    Option<EnvLink>,
    Option<EnvLink>,
)>;

/// A worker environment snapshot under copy-on-write: frozen ancestors
/// shared by pointer plus live leaf bindings. See
/// [`snapshot_env_cow`] for the exactness argument.
pub struct CowSnapshot {
    /// Frozen ancestors (shared — O(1)); `None` when the spawner leaf is
    /// its own root.
    pub parent: Option<EnvLink>,
    /// Current-leaf bindings, deep-cloned (the leaf churns per iteration
    /// and can never be shared).
    pub leaf: Vec<(String, Value)>,
}

/// Cached reachability for one spawn-site chunk: the chunk `Arc` is held
/// so its address can never be reused while cached (kills the
/// use-after-free key-collision class outright), and hits require the
/// current [`Interp::funcs_version`](crate::Interp::funcs_version) — a new
/// runtime-defined function transparently invalidates.
#[derive(Debug, Clone)]
pub struct ReachCacheEntry {
    pub chunk: std::sync::Arc<Chunk>,
    pub version: u64,
    pub reachable: std::sync::Arc<HashSet<String>>,
    pub loads: std::sync::Arc<HashSet<String>>,
}

/// Inner state for a thread-safe channel: lock-free ring fast path +
/// mutex spillover for bursts past ring capacity (+ condvar).
///
/// Two tiers, one FIFO: `ring` (Vyukov MPMC, `RING_CAP` deep) serves all
/// traffic while it fits; overflow spills to `queue` under the mutex.
/// Receivers drain the spill first whenever `spill` (atomic count) is
/// non-zero, so cross-tier order is preserved. Fast paths are allowed
/// only when no waiter can exist (`green_parked` clear and `cvar_waiters`
/// zero) — otherwise a fast pop could steal a value already promised to
/// a parked waiter (lost-wakeup hang class).
///
/// The Condvar lives outside the Mutex so `wait_while` can be called cleanly.
#[derive(Debug)]
pub struct ChanInner {
    /// Overflow queue: values that missed the full ring. Drained before
    /// the ring whenever non-empty (see `ChanState::spill`).
    pub queue: VecDeque<Value>,
    /// Green-thread waiters parked in `chan.recv`, woken (moved to the
    /// executor ready queue) by `chan.send`. Main-thread blockers use the
    /// condvar as before; both sets are served under the same mutex, so no
    /// wakeup can slip between the empty-check and the park.
    pub green_waiters: Vec<u64>,
}

/// Channel pair: lock-free ring + spill counter + waiter flags (all
/// lock-free) plus the mutex-protected spill queue and signaling condvar.
#[derive(Debug)]
pub struct ChanState {
    /// Fast path: zero-lock enqueue/dequeue while depth fits.
    pub ring: LfRing,
    /// Number of values sitting in the spill queue. Written only while
    /// holding `inner`; read lock-free by fast paths (Release/Acquire).
    pub spill: AtomicUsize,
    /// Set while `green_waiters` may be non-empty (under `inner` both
    /// ways); fast pops require it clear so no waiter is robbed.
    pub green_parked: AtomicBool,
    /// Main-thread condvar sleepers in flight. Sends notify the condvar
    /// only when this is non-zero (otherwise the futex wake is pure
    /// overhead on the fast path).
    pub cvar_waiters: AtomicUsize,
    pub inner: Mutex<ChanInner>,
    pub cvar: Condvar,
}

/// Inner state for a task join handle: outcome slot + signaling condvar.
/// One `Mutex` (not `Arc<Mutex>` — the handle itself is already an `Arc`)
/// plus a counted sleeper protocol so completions with no waiters store
/// by move and skip the condvar notify entirely (the fire-and-forget
/// fast path: no clone, no syscall).
#[derive(Debug)]
pub struct TaskJoinState {
    pub result: Mutex<TaskJoinInner>,
    /// Main-thread condvar sleepers in flight. Completions notify only
    /// when non-zero; same soundness shape as channel sleepers (announce
    /// before the predicate check under the lock).
    pub cvar_waiters: AtomicUsize,
    pub cvar: Condvar,
}

/// Mutex-guarded join payload: the task outcome plus green-thread waiters
/// parked in `task.join` / `task.try_join`-style waits.
///
/// `result` is consumed by the first main-thread `task.join`/`task.try_join`
/// take; `completed` stays set so late joiners get a loud "already consumed"
/// error instead of hanging forever on a result that will never arrive.
/// Green-thread waiters always receive clones at completion time and never
/// consume.
#[derive(Debug, Default)]
pub struct TaskJoinInner {
    pub result: Option<Result<Value, String>>,
    pub completed: bool,
    pub green_waiters: Vec<u64>,
}

/// A green-thread task identifier. Allocated from a process-wide counter by
/// the executor; `0` is reserved as "no task" (main thread).
pub type TaskId = u64;

/// Why a green-thread task yielded its executor thread. The task made no
/// progress since (parked immediately), so resumption continues right after
/// the blocking call — with the call's dummy result replaced by the real
/// value under the channel/handle lock before requeue.
#[derive(Debug, Clone)]
pub enum YieldReason {
    /// Parked in `chan.recv` on an empty channel.
    ChanWait { chan: Arc<ChanState> },
    /// Parked in `task.join` on an incomplete task.
    JoinWait { handle: Arc<TaskJoinState> },
    /// Cooperative quantum expiry at a loop safepoint (`Op::Safepoint`):
    /// the task ran longer than its timeslice without blocking. No object
    /// is involved — the executor just requeues it so siblings run.
    Timeslice,
}

// ── Executor thread-locals ────────────────────────────────────────────────
//
// Cooperatively scheduled ("green") tasks run on executor threads and must
// never block them: `chan.recv`/`task.join` yield instead of waiting when
// the conditions below hold. Main-thread execution never yields.

thread_local! {
    /// Task currently running on this executor thread (`None` on main and
    /// pool-fallback threads). Set by the executor around each task slice.
    static EXECUTOR_TASK: std::cell::Cell<Option<TaskId>> = const { std::cell::Cell::new(None) };
    /// Pending yield request, set by `chan.recv`/`task.join` instead of
    /// blocking. The VM loop takes it after the call op and suspends.
    static PENDING_YIELD: RefCell<Option<YieldReason>> = const { RefCell::new(None) };
    /// Tree-walker (interpreted) evaluation depth on this thread. Yields are
    /// only sound directly under the VM loop: interpreter frames cannot be
    /// resumed (Rust call stack), so blocking natives fall back to parking
    /// the thread whenever this is nonzero.
    static INTERP_DEPTH: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// The green-thread task running on this thread, if any.
pub fn executor_task() -> Option<TaskId> {
    EXECUTOR_TASK.with(|c| c.get())
}

/// True when this thread runs green-thread tasks (yield protocol active).
pub fn on_executor() -> bool {
    executor_task().is_some()
}

/// Request suspension of the current task. No-op semantics for the caller:
/// the VM loop converts this into `Flow::Yield` right after the call op.
pub fn request_yield(reason: YieldReason) {
    PENDING_YIELD.with(|c| *c.borrow_mut() = Some(reason));
}

/// Take a pending yield request, if the last call op left one.
pub fn take_yield() -> Option<YieldReason> {
    PENDING_YIELD.with(|c| c.borrow_mut().take())
}

/// Current interpreter-nesting depth (see `INTERP_DEPTH`).
pub fn interp_depth() -> usize {
    INTERP_DEPTH.with(|c| c.get())
}

/// Run `f` with the interpreter depth bumped (tree-walker frames above any
/// blocking native make yielding unsound — the thread parks instead).
pub fn with_interp_depth<R>(f: impl FnOnce() -> R) -> R {
    INTERP_DEPTH.with(|c| c.set(c.get() + 1));
    let r = f();
    INTERP_DEPTH.with(|c| c.set(c.get() - 1));
    r
}

/// Run `f` as green-thread `task` on this executor thread.
pub fn with_executor_task<R>(task: TaskId, f: impl FnOnce() -> R) -> R {
    EXECUTOR_TASK.with(|c| c.set(Some(task)));
    let r = f();
    EXECUTOR_TASK.with(|c| c.set(None));
    r
}

/// A struct instance payload (boxed so `Value` stays small).
#[derive(Debug, Clone, PartialEq)]
pub struct ObjectValue {
    pub name: String,
    pub fields: Vec<(String, Value)>,
}

/// An opaque SQLite database handle. The concrete connection type lives
/// in `zz_stdlib` (rusqlite) so this crate stays dependency-free; the
/// handle is type-erased here as `Arc<dyn Any + Send + Sync>`.
#[derive(Debug, Clone)]
pub struct DbHandle(pub Arc<DbHandleInner>);

/// Inner payload for [`DbHandle`]: a mutex-guarded type-erased connection.
#[derive(Debug)]
pub struct DbHandleInner {
    pub mutex: Mutex<Box<dyn std::any::Any + Send + Sync>>,
}

impl PartialEq for DbHandle {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }
}

/// An integer range `a..b` / `a..b..step` (boxed so `Value` stays small).
#[derive(Debug, Clone, PartialEq)]
pub struct RangeValue {
    pub start: i64,
    pub end: i64,
    pub step: i64,
}

/// A runtime value.
///
/// Heap-allocates (boxes) every payload larger than a word so the enum fits
/// in 16 bytes (including its discriminant byte and padding). This keeps
/// `LoadSlot`/`StoreSlot`/stack-push copies to a single 16-byte memcpy
/// instead of copying large strings, vectors, or environments inline.
///
/// # Thread safety
///
/// `Value` is `Send` because all cross-thread usage (via `spawn`) operates
/// on deep-cloned, self-contained snapshots where every `FuncValue` carries
/// its own copy of the captured env (no `Rc` back-references). The compiler
/// cannot verify this structurally, so we assert it manually.
#[derive(Debug, Clone)]
pub enum Value {
    Int(i64),
    Float(f64),
    /// A heap-allocated string (thin pointer).
    Str(Box<String>),
    Bool(bool),
    Unit,
    /// `.some(v)` / `.none`.
    Option(Option<Box<Value>>),
    /// `.ok(v)` / `.err(e)`.
    Result(Box<Result<Value, Value>>),
    /// A closure or named function, with its captured environment.
    Func(Box<FuncValue>),
    /// `[v1, v2, ...]`.
    Array(Box<Vec<Value>>),
    /// Contiguous byte buffer (produced by `fs.read_bytes`,
    /// `fs.read_chunk_bytes`). Shares its backing store on clone/slice
    /// (`Arc` + window), so slicing is O(1) and whole-file reads cost
    /// ~1 byte of RSS per byte of file instead of ~31.
    Bytes(Box<BytesData>),
    /// `{k1: v1, k2: v2, ...}` — insertion-ordered key/value pairs.
    Dict(Box<Vec<(Value, Value)>>),
    /// A native (Rust-backed) function from the standard library.
    Native(Box<NativeFunc>),
    /// A parsed JSON value (opaque to the type system).
    Json(Box<JsonValue>),
    /// An HTTP server handle with its registered routes.
    HttpServer(Box<HttpServer>),
    /// A TCP stream (opaque, wrapped in Arc<Mutex> for clone safety).
    TcpStream(Arc<Mutex<TcpStream>>),
    /// A TCP listener (opaque, wrapped in Arc<Mutex> for clone safety).
    TcpListener(Arc<Mutex<TcpListener>>),
    /// An opaque SQLite database handle (`std.sqlz.open`;
    /// `std.db.open` alias).
    Db(DbHandle),
    /// A generic opaque handle into the `zz_native_rt` pool (regex patterns,
    /// arg parsers, log spans, …). The tag selects the method namespace.
    Opaque(Box<zz_native_rt::Handle>),
    /// An HTTP response (status + body + headers).
    Response(Box<Response>),
    /// A struct instance: its type name and insertion-ordered fields.
    Object(Box<ObjectValue>),
    /// `a..b` or `a..b..step` — an integer range (used by `for` loops).
    Range(Box<RangeValue>),
    /// `(v1, v2, ...)` — tuple value.
    Tuple(Box<Vec<Value>>),
    /// Thread-safe channel (unbounded queue + condvar).
    Chan(Arc<ChanState>),
    /// Join handle from spawn — recv() blocks until task completes.
    TaskJoin(Arc<TaskJoinState>),
}

// SAFETY: `Value` is safe to send across threads when used with
// point-in-time snapshots (see `snapshot_env_cow`): frozen scopes are
// immutable and `Send + Sync` by construction. The compiler cannot verify
// the owned-scope discipline structurally, but all cross-thread usage
// operates on frozen-shared or freshly-detached chains.
unsafe impl Send for Value {}
unsafe impl Sync for Value {}
// SAFETY: `FuncValue` is safe to send across threads only when its env
// is self-contained (see `snapshot_funcs`, the sole cross-thread path:
// every captured env is flattened into a fresh, unshared copy before the
// move). Never move a `FuncValue` whose env aliases another thread's
// scope chain. Same discipline as `Send for Value` above.
unsafe impl Send for FuncValue {}

/// Flatten a captured environment into a self-contained `HashMap`.
///
/// Walks the scope chain root→leaf, deep-clones every value, and rewrites
/// nested closures so they carry their own copy of the captured env (no
/// dangling `Rc` references to the original scope chain).  This makes the
/// result safe to move across thread boundaries.
pub fn snapshot_env(env: &EnvLink) -> HashMap<String, Value> {
    let flat = env.flatten();
    // ONE memo for all entries: captured values routinely share envs (e.g.
    // every stdlib func aliases its module scope). A fresh memo per entry
    // re-clones the shared graph once per entry (measured 130ms/spawn for
    // 131 entries); sharing makes it linear.
    let mut seen: HashMap<usize, Value> = HashMap::new();
    flat.into_iter()
        .map(|(k, v)| (k, deep_clone_value(v, &mut seen)))
        .collect()
}

/// Flatten a captured environment into a self-contained `HashMap`, keeping
/// only what the worker chunk may reference.
///
/// `loads` (from [`reachable_refs`]) names every environment resolution
/// Build a worker environment snapshot under copy-on-write.
///
/// The worker gets a fresh owned leaf whose parent is the spawner's
/// frozen ancestors (shared by `Arc`, O(1)) — no per-value deep clones
/// for anything above the leaf. The current leaf's referenced bindings
/// are still deep-cloned (the leaf churns per loop iteration and can
/// never be shared).
///
/// # Exactness (why no fallback is needed)
///
/// A frozen copy is a point-in-time clone of the ancestors, exactly like
/// the old deep-clone snapshots — so every behavior matches:
/// - Later spawner writes can't disturb workers (separate maps), same
///   as before.
/// - Later spawns re-validate the parent shape, whose entries carry
///   mutation versions: any `define`/`assign` anywhere above the leaf
///   forces a re-freeze. Stale frozen copies are impossible on a hit.
/// - Unreferenced names stay visible through the frozen chain, but the
///   worker chunk can only resolve names in its `loads` set (structural
///   invariant of [`reachable_refs`]) — extra visibility is unobservable.
///   Store targets are in `loads` too, so a worker store lands on a
///   detached private copy either way (old: pre-cloned map; new:
///   clone-on-write), invisible to the spawner in both cases.
/// - `Func` entries covered by the table slice resolve to the same
///   object either way (frozen shares it; the table carries it).
pub fn snapshot_env_cow(
    env: &EnvLink,
    funcs: &HashMap<String, FuncValue>,
    reachable: &HashSet<String>,
    loads: &HashSet<String>,
    keep_cache: &mut SpawnKeepCache,
) -> CowSnapshot {
    // Empty-capture fast path: nothing reachable and nothing loadable
    // means no parent and no leaf bindings — skip everything.
    if reachable.is_empty() && loads.is_empty() {
        return CowSnapshot {
            parent: None,
            leaf: Vec::new(),
        };
    }
    // Validate the parent chain (stable across loop iterations): shape
    // entries carry mutation versions, so any write above the leaf
    // forces a re-freeze. Hits share the cached frozen parent by
    // pointer — the whole snapshot fast path.
    let parent = env.parent_link();
    let parent_shape = match &parent {
        Some(p) => crate::env::Env::chain_shape_from(p),
        None => Vec::new(),
    };
    let frozen = match keep_cache {
        // `pinned` keeps the cached scopes alive, so the address-keyed
        // shape match implies the SAME scopes (no ABA after free). The
        // pin must also match the current parent — shape equality alone
        // can't distinguish a live chain from a same-address reuse.
        Some((ps, r, frozen, pinned))
            if *ps == parent_shape
                && *r == *reachable
                && match (pinned.as_ref(), parent.as_ref()) {
                    (Some(a), Some(b)) => EnvLink::ptr_eq(a, b),
                    (None, None) => true,
                    _ => false,
                } =>
        {
            frozen.clone()
        }
        _ => {
            let frozen = parent.as_ref().map(EnvLink::frozen_view);
            *keep_cache = Some((parent_shape, reachable.clone(), frozen.clone(), parent));
            frozen
        }
    };
    // Current-leaf bindings, resolved live every spawn (the leaf is fresh
    // per loop iteration — cached holders would be stale). Filtered by
    // the same rule as before: referenced, and not table-covered `Func`s.
    // ONE shared memo: leaf values routinely share scopes.
    let mut seen: HashMap<usize, Value> = HashMap::new();
    let mut leaf = Vec::new();
    for name in env.local_names() {
        if !loads.contains(name.as_str()) {
            continue;
        }
        let Some(v) = env.get_local(&name) else {
            continue;
        };
        if let Value::Func(fv) = &v {
            let covered = reachable.contains(name.as_str())
                && funcs
                    .get(name.as_str())
                    .is_some_and(|tf| EnvLink::ptr_eq(&tf.env, &fv.env));
            if covered {
                continue;
            }
        }
        leaf.push((name, deep_clone_value(v, &mut seen)));
    }
    CowSnapshot {
        parent: frozen,
        leaf,
    }
}

/// Snapshot a function table so it is safe to send across thread boundaries.
/// Each `FuncValue`'s captured env is flattened into a self-contained copy.
pub fn snapshot_funcs(funcs: &HashMap<String, FuncValue>) -> HashMap<String, FuncValue> {
    let mut out = HashMap::new();
    // ONE memo map for the whole table: module scopes share envs, so a
    // fresh map per value re-clones shared graphs exponentially (165
    // funcs hung spawn outright). Same ptr = same object, so sharing
    // the map is exactly as correct, linear instead of exponential.
    let mut seen: HashMap<usize, Value> = HashMap::new();
    // Memoize flatten per env: many funcs alias the same module scopes, and
    // `flatten` clones every layer per call. Without this the table snapshot
    // re-walks shared chains once per func (measured 100ms+/spawn for 98
    // funcs). Same ptr = same scope chain, so reuse is exactly as correct.
    // NOTE: the cached flat maps are consumed read-only below; the per-entry
    // deep clones still produce independent worker-owned values.
    let mut flats: HashMap<usize, HashMap<String, Value>> = HashMap::new();
    for (name, fv) in funcs {
        let key = fv.env.id();
        // Borrow dance: compute the flat map only on first sight of an env.
        let flat = flats.entry(key).or_insert_with(|| fv.env.flatten());
        let mut fresh = crate::env::Env::new();
        for (k, v) in flat {
            fresh.define(k, deep_clone_value(v.clone(), &mut seen));
        }
        let new_env = EnvLink::Owned(Rc::new(RefCell::new(fresh)));
        out.insert(
            name.clone(),
            FuncValue {
                params: fv.params.clone(),
                body: Expr::Block(Block {
                    stmts: Vec::new(),
                    span: Span::new(0, 0),
                }),
                env: new_env,
                chunk: fv.chunk.clone(),
            },
        );
    }
    out
}

/// Clone a cached (already detached) function-table snapshot for one worker.
///
/// The cached entries are self-contained, so no flattening is needed — but
/// the nested `Rc` envs must still be re-detached per worker, or workers
/// would alias each other's (and the cache's) environments. A single memo
/// is shared across the whole table, same discipline as [`snapshot_funcs`].
pub fn detach_cached_funcs(cached: &HashMap<String, FuncValue>) -> HashMap<String, FuncValue> {
    let mut seen: HashMap<usize, Value> = HashMap::new();
    let mut out = HashMap::with_capacity(cached.len());
    for (name, fv) in cached {
        // Cached envs are single-scope (built by `snapshot_funcs`), so a
        // plain flatten is a one-layer copy — no chain walk.
        let flat = fv.env.flatten();
        let mut fresh = crate::env::Env::new();
        for (k, v) in flat {
            fresh.define(&k, deep_clone_value(v, &mut seen));
        }
        let new_env = EnvLink::Owned(Rc::new(RefCell::new(fresh)));
        out.insert(
            name.clone(),
            FuncValue {
                params: fv.params.clone(),
                body: Expr::Block(Block {
                    stmts: Vec::new(),
                    span: Span::new(0, 0),
                }),
                env: new_env,
                chunk: fv.chunk.clone(),
            },
        );
    }
    out
}

/// Compute the function-table names plus the environment names a worker
/// chunk may reference.
///
/// Scans the root chunk (plus transitively referenced function bodies and
/// inline closure chunks) for name operands:
/// - `funcs`: candidates that name (or method-match) table entries, for
///   the worker's table slice. Conservative: exact names plus dotted
///   joins/prefixes for paths, plus a `.{method}` suffix rule so
///   struct/impl method calls (`rect.area()` → `Shape.area`) resolve
///   without knowing the receiver's runtime type.
/// - `loads`: names the worker must resolve through its environment
///   (variable loads, path heads, store targets — a store to a missing
///   name errors, so targets are included). The env snapshot keeps exactly
///   these (plus shadowing definitions); everything else the worker never
///   asks for. Under-inclusion would be a loud resolve error, never silent
///   wrong behavior — and the scan covers every name-carrying op, including
///   method tails (env-first method fallback) and transitive bodies.
pub fn reachable_refs(
    root: &Arc<Chunk>,
    funcs: &HashMap<String, FuncValue>,
) -> (HashSet<String>, HashSet<String>) {
    let mut names: HashSet<String> = HashSet::new();
    let mut loads: HashSet<String> = HashSet::new();
    // Worklist of chunks to scan; `visited` keys chunk identity so shared
    // bodies (and cycles via MakeFunc) are scanned once.
    let mut stack: Vec<Arc<Chunk>> = vec![Arc::clone(root)];
    let mut visited: HashSet<usize> = HashSet::new();

    // Admit one candidate name: if it names (or method-matches) table
    // entries, include them and enqueue their bodies for transitives.
    //
    // Nested fn to keep borrowck happy (borrows `funcs`/`names`/`stack`
    // mutably across the scan loop).
    fn admit(
        cand: &str,
        funcs: &HashMap<String, FuncValue>,
        names: &mut HashSet<String>,
        stack: &mut Vec<Arc<Chunk>>,
    ) {
        // Suffix rule needs the dotted form: `area` also matches `Shape.area`.
        let suffix = format!(".{cand}");
        for (key, fv) in funcs.iter() {
            if (key == cand || key.ends_with(suffix.as_str())) && names.insert(key.clone()) {
                if let Some(ch) = fv.chunk.as_ref() {
                    stack.push(Arc::clone(ch));
                }
            }
        }
    }

    while let Some(ch) = stack.pop() {
        let ptr = Arc::as_ptr(&ch) as *const () as usize;
        if !visited.insert(ptr) {
            continue;
        }
        for op in ch.code.iter() {
            match op {
                // Value loads and store targets resolve through the env.
                Op::LoadVar(n, _) | Op::StoreVar(n, _) => {
                    loads.insert(n.clone());
                    admit(n, funcs, &mut names, &mut stack);
                }
                Op::LoadPath(parts, _) | Op::StorePath(parts, _) => {
                    if parts.is_empty() {
                        continue;
                    }
                    // Every component: heads resolve as values, and the tail
                    // can name a method looked up bare in the env
                    // (`lookup_method` checks the env first).
                    loads.insert(parts.join("."));
                    for p in parts {
                        loads.insert(p.clone());
                    }
                    admit(&parts.join("."), funcs, &mut names, &mut stack);
                    admit(&parts[0], funcs, &mut names, &mut stack);
                }
                Op::CallPath { parts, .. } => {
                    if parts.is_empty() {
                        continue;
                    }
                    // Callee path resolves like a load (env → funcs →
                    // natives), so its names join the load set — including
                    // the tail, which `lookup_method` may resolve bare.
                    loads.insert(parts.join("."));
                    for p in parts {
                        loads.insert(p.clone());
                    }
                    admit(&parts.join("."), funcs, &mut names, &mut stack);
                    admit(&parts[0], funcs, &mut names, &mut stack);
                }
                // Method names can resolve bare through the env
                // (`lookup_method` fallback), so they join the load set.
                // Native names resolve via the registry only.
                Op::CallMethod { name: n, .. } => {
                    loads.insert(n.clone());
                    admit(n, funcs, &mut names, &mut stack);
                }
                Op::CallNative { name: n, .. } | Op::GetField(n, _) | Op::SetField(n, _) => {
                    admit(n, funcs, &mut names, &mut stack)
                }
                // Definitions bind locally; the body chunk may reference.
                Op::MakeFunc { chunk, .. } => {
                    stack.push(Arc::clone(chunk));
                }
                Op::MakeClosure { chunk, .. } => stack.push(Arc::clone(chunk)),
                Op::DefineVar(_) => {}
                _ => {}
            }
        }
    }
    (names, loads)
}

/// Snapshot a subset of the function table (see [`snapshot_funcs`]).
///
/// Used with [`reachable_func_names`] so a spawn carries only the functions
/// its worker can call instead of the whole table.
pub fn snapshot_funcs_subset(
    funcs: &HashMap<String, FuncValue>,
    names: &HashSet<String>,
) -> HashMap<String, FuncValue> {
    let filtered: HashMap<String, FuncValue> = funcs
        .iter()
        .filter(|(k, _)| names.contains(k.as_str()))
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect();
    snapshot_funcs(&filtered)
}

/// Detach a subset of a cached table snapshot for one worker (see
/// [`detach_cached_funcs`]).
pub fn detach_cached_subset(
    cached: &HashMap<String, FuncValue>,
    names: &HashSet<String>,
) -> HashMap<String, FuncValue> {
    if names.len() == cached.len() {
        return detach_cached_funcs(cached);
    }
    let filtered: HashMap<String, FuncValue> = cached
        .iter()
        .filter(|(k, _)| names.contains(k.as_str()))
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect();
    detach_cached_funcs(&filtered)
}

/// Deep-clone a value, rewriting any `Value::Func` so its captured env is
/// self-contained (no `Rc` back-references to the original scope chain).
fn deep_clone_value(v: Value, seen: &mut HashMap<usize, Value>) -> Value {
    match v {
        Value::Func(fv) => {
            // Use pointer address as the dedup key to prevent infinite recursion
            // on cyclic closure references.
            let key = fv.env.id();
            if let Some(cloned) = seen.get(&key) {
                return cloned.clone();
            }
            // Seed with a placeholder so recursive calls return the same Arc.
            let placeholder = FuncValue {
                params: fv.params.clone(),
                body: Expr::Block(Block {
                    stmts: Vec::new(),
                    span: Span::new(0, 0),
                }),
                env: EnvLink::new(),
                chunk: fv.chunk.clone(),
            };
            let placeholder_val = Value::Func(Box::new(placeholder));
            seen.insert(key, placeholder_val.clone());

            // Flatten the captured env and create a self-contained version.
            let flat = fv.env.flatten();
            let mut fresh = crate::env::Env::new();
            for (name, val) in flat {
                fresh.define(&name, deep_clone_value(val, seen));
            }
            let new_env = EnvLink::Owned(Rc::new(RefCell::new(fresh)));
            let cloned = Value::Func(Box::new(FuncValue {
                params: fv.params,
                body: Expr::Block(Block {
                    stmts: Vec::new(),
                    span: Span::new(0, 0),
                }),
                env: new_env,
                chunk: fv.chunk,
            }));
            seen.insert(key, cloned.clone());
            cloned
        }
        Value::Array(arr) => Value::Array(Box::new(
            arr.into_iter().map(|v| deep_clone_value(v, seen)).collect(),
        )),
        Value::Dict(pairs) => Value::Dict(Box::new(
            pairs
                .into_iter()
                .map(|(k, v)| (deep_clone_value(k, seen), deep_clone_value(v, seen)))
                .collect(),
        )),
        Value::Option(opt) => Value::Option(opt.map(|v| Box::new(deep_clone_value(*v, seen)))),
        Value::Result(res) => Value::Result(Box::new(match *res {
            Ok(v) => Ok(deep_clone_value(v, seen)),
            Err(e) => Err(deep_clone_value(e, seen)),
        })),
        Value::Tuple(items) => Value::Tuple(Box::new(
            items
                .into_iter()
                .map(|v| deep_clone_value(v, seen))
                .collect(),
        )),
        Value::Object(obj) => Value::Object(Box::new(ObjectValue {
            name: obj.name,
            fields: obj
                .fields
                .into_iter()
                .map(|(n, v)| (n, deep_clone_value(v, seen)))
                .collect(),
        })),
        // Primitives and opaque Arc-wrapped types: cheap clone is fine.
        other => other,
    }
}

/// A JSON value (see [`crate::json`]).
pub use crate::json::JsonValue;

/// Contiguous byte buffer with a shared backing store: `data` is
/// reference-counted, `[start, start+len)` is this value's window.
/// Clone and slice only bump the `Arc` (no byte copies).
#[derive(Debug, Clone, PartialEq)]
pub struct BytesData {
    data: std::sync::Arc<Vec<u8>>,
    start: usize,
    len: usize,
}

impl BytesData {
    /// Take ownership of a fresh buffer (whole window).
    pub fn new(data: Vec<u8>) -> Self {
        let len = data.len();
        BytesData {
            data: std::sync::Arc::new(data),
            start: 0,
            len,
        }
    }

    /// Borrow a `Vec` without copying (used by fs reads that already own
    /// the buffer).
    pub fn from_vec(data: Vec<u8>) -> Self {
        Self::new(data)
    }

    pub fn len(&self) -> usize {
        self.len
    }

    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// Visible window as a slice (zero-copy).
    pub fn as_slice(&self) -> &[u8] {
        &self.data[self.start..self.start + self.len]
    }

    /// One byte as `u8`, or `None` out of bounds (no negative wrap here —
    /// callers normalize first).
    pub fn get(&self, i: usize) -> Option<u8> {
        if i < self.len {
            Some(self.data[self.start + i])
        } else {
            None
        }
    }

    /// O(1) sub-window sharing the same backing store.
    pub fn slice(&self, a: usize, b: usize) -> Self {
        let a = a.min(self.len);
        let b = b.min(self.len).max(a);
        BytesData {
            data: std::sync::Arc::clone(&self.data),
            start: self.start + a,
            len: b - a,
        }
    }
}

/// An HTTP server: registered (method, path, handler) routes.
#[derive(Debug, Clone, PartialEq)]
pub struct HttpServer {
    pub routes: Vec<(String, String, Value)>,
    pub middlewares: Vec<Value>,
    pub log_enabled: bool,
    pub static_dir: Option<String>,
}

/// An HTTP response: status code, body, and headers.
#[derive(Debug, Clone, PartialEq)]
pub struct Response {
    pub status: u16,
    pub body: String,
    pub headers: Vec<(String, String)>,
}

/// A native function reference: name + arity. The implementation lives in
/// the interpreter's native registry.
#[derive(Debug, Clone, PartialEq)]
pub struct NativeFunc {
    pub name: String,
    pub arity: usize,
}

/// A callable value: parameter list, body expression, and the environment
/// captured at definition time (shared by reference).
#[derive(Debug, Clone, PartialEq)]
pub struct FuncValue {
    pub params: Vec<Param>,
    pub body: Expr,
    pub env: EnvLink,
    /// Pre-compiled bytecode body, when the function was defined through the
    /// Phase 6 compiler. `None` for tree-walker-created closures.
    pub chunk: Option<std::sync::Arc<crate::vm::Chunk>>,
}

impl Value {
    /// Promote a numeric value to float (used for mixed arithmetic).
    pub fn to_float(&self) -> Option<f64> {
        match self {
            Value::Int(i) => Some(*i as f64),
            Value::Float(f) => Some(*f),
            _ => None,
        }
    }

    #[inline(always)]
    pub fn is_truthy(&self) -> bool {
        matches!(self, Value::Bool(true))
    }

    /// The runtime type name of this value (used by `typeof` and error
    /// messages). Struct instances report their type name.
    pub fn type_name(&self) -> String {
        match self {
            Value::Int(_) => "int".to_string(),
            Value::Float(_) => "float".to_string(),
            Value::Str(_) => "str".to_string(),
            Value::Bool(_) => "bool".to_string(),
            Value::Unit => "unit".to_string(),
            Value::Option(_) => "option".to_string(),
            Value::Result(_) => "result".to_string(),
            Value::Func(_) => "func".to_string(),
            Value::Array(_) => "array".to_string(),
            Value::Bytes(_) => "bytes".to_string(),
            Value::Dict(_) => "dict".to_string(),
            Value::Native(_) => "native".to_string(),
            Value::Json(_) => "json".to_string(),
            Value::HttpServer(_) => "http.server".to_string(),
            Value::TcpStream(_) => "tcp.stream".to_string(),
            Value::TcpListener(_) => "tcp.listener".to_string(),
            Value::Db(_) => "db".to_string(),
            Value::Opaque(h) => h.tag.clone(),
            Value::Response(_) => "http.response".to_string(),
            Value::Object(o) => o.name.clone(),
            Value::Range(_) => "range".to_string(),
            Value::Tuple(_) => "tuple".to_string(),
            Value::Chan(_) => "chan".to_string(),
            Value::TaskJoin(_) => "task.join".to_string(),
        }
    }

    /// The method namespace for this value type, used for method dispatch.
    /// Returns "str" for strings, "vec" for arrays, the struct namespace for
    /// objects, and None for types without method support.
    pub fn method_namespace(&self) -> Option<&'static str> {
        match self {
            Value::Str(_) => Some("str"),
            Value::Array(_) => Some("vec"),
            Value::Bytes(_) => Some("bytes"),
            Value::Option(_) => Some("option"),
            Value::Result(_) => Some("result"),
            Value::Int(_) => Some("int"),
            Value::Float(_) => Some("float"),
            Value::Bool(_) => Some("bool"),
            Value::TcpStream(_) => Some("net"),
            Value::TcpListener(_) => Some("net"),
            Value::Db(_) => Some("sqlz"),
            // Opaque handles dispatch on their tag (e.g. a `"regex"` handle
            // resolves `regex.is_match`). Tags are dynamic, so the `&str`
            // is leaked once per distinct tag — same pattern as `Object`
            // namespaces below.
            Value::Opaque(h) => Some(Box::leak(h.tag.clone().into_boxed_str()) as &str),
            Value::Response(_) => Some("http"),
            Value::Chan(_) => Some("chan"),
            Value::TaskJoin(_) => None,
            Value::Object(o) => {
                // Extract namespace from struct name (e.g., "shapes.Point" -> "shapes")
                o.name
                    .rsplit_once('.')
                    .map(|(ns, _)| Box::leak(ns.to_string().into_boxed_str()) as &str)
            }
            _ => None,
        }
    }
}

impl fmt::Display for Value {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Value::Int(i) => write!(f, "{i}"),
            // Always show a decimal point so floats are distinguishable.
            Value::Float(x) => {
                if x.is_finite() && x.fract() == 0.0 {
                    write!(f, "{x:.1}")
                } else {
                    write!(f, "{x}")
                }
            }
            Value::Str(s) => write!(f, "{s}"),
            Value::Bool(b) => write!(f, "{b}"),
            Value::Unit => write!(f, ""),
            Value::Option(Some(v)) => write!(f, ".some({v})"),
            Value::Option(None) => write!(f, ".none"),
            Value::Result(r) => match &**r {
                Ok(v) => write!(f, ".ok({v})"),
                Err(e) => write!(f, ".err({e})"),
            },
            Value::Func(_) => write!(f, "<func>"),
            Value::Array(vs) => {
                write!(f, "[")?;
                for (i, v) in vs.iter().enumerate() {
                    if i > 0 {
                        write!(f, ", ")?;
                    }
                    write!(f, "{v}")?;
                }
                write!(f, "]")
            }
            // Same shape as an int array (`[104, 105]`) so byte buffers
            // print exactly like the old boxed representation.
            Value::Bytes(b) => {
                write!(f, "[")?;
                for (i, byte) in b.as_slice().iter().enumerate() {
                    if i > 0 {
                        write!(f, ", ")?;
                    }
                    write!(f, "{byte}")?;
                }
                write!(f, "]")
            }
            Value::Dict(entries) => {
                write!(f, "{{")?;
                for (i, (k, v)) in entries.iter().enumerate() {
                    if i > 0 {
                        write!(f, ", ")?;
                    }
                    write!(f, "{k}: {v}")?;
                }
                write!(f, "}}")
            }
            Value::Native(nf) => write!(f, "<native {}>", nf.name),
            Value::Json(j) => write!(f, "{j}"),
            Value::HttpServer(_) => write!(f, "<http server>"),
            Value::TcpStream(_) => write!(f, "<tcp stream>"),
            Value::TcpListener(_) => write!(f, "<tcp listener>"),
            Value::Response(res) => write!(f, "<http response {}>", res.status),
            Value::Object(o) => {
                write!(f, "{}{{", o.name)?;
                for (i, (k, v)) in o.fields.iter().enumerate() {
                    if i > 0 {
                        write!(f, ", ")?;
                    }
                    write!(f, "{k}: {v}")?;
                }
                write!(f, "}}")
            }
            Value::Range(r) => {
                if r.step == 1 {
                    write!(f, "{}..{}", r.start, r.end)
                } else {
                    write!(f, "{}..{}..{}", r.start, r.end, r.step)
                }
            }
            Value::Tuple(vs) => {
                write!(f, "(")?;
                for (i, v) in vs.iter().enumerate() {
                    if i > 0 {
                        write!(f, ", ")?;
                    }
                    write!(f, "{v}")?;
                }
                write!(f, ")")
            }
            Value::Chan(_) => write!(f, "<chan>"),
            Value::TaskJoin(_) => write!(f, "<task.join>"),
            Value::Db(_) => write!(f, "<db>"),
            Value::Opaque(h) => write!(f, "<{} #{}>", h.tag, h.id),
        }
    }
}

impl PartialEq for Value {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (Value::Int(a), Value::Int(b)) => a == b,
            (Value::Float(a), Value::Float(b)) => a == b,
            (Value::Str(a), Value::Str(b)) => a == b,
            (Value::Bool(a), Value::Bool(b)) => a == b,
            (Value::Unit, Value::Unit) => true,
            (Value::Option(a), Value::Option(b)) => a == b,
            (Value::Result(a), Value::Result(b)) => a == b,
            (Value::Array(a), Value::Array(b)) => a == b,
            (Value::Bytes(a), Value::Bytes(b)) => a.as_slice() == b.as_slice(),
            (Value::Dict(a), Value::Dict(b)) => a == b,
            (Value::Json(a), Value::Json(b)) => a == b,
            (Value::Tuple(a), Value::Tuple(b)) => a == b,
            (Value::Response(a), Value::Response(b)) => a == b,
            (Value::HttpServer(_), Value::HttpServer(_)) => std::ptr::eq(self, other),
            (Value::Object(a), Value::Object(b)) => a == b,
            // Opaque types: compare by Arc pointer (identity, not deep equality)
            (Value::TcpStream(a), Value::TcpStream(b)) => Arc::ptr_eq(a, b),
            (Value::TcpListener(a), Value::TcpListener(b)) => Arc::ptr_eq(a, b),
            // Func and Native: compare by reference identity (not deep equality)
            (Value::Func(_), Value::Func(_)) => std::ptr::eq(self, other),
            (Value::Native(_), Value::Native(_)) => std::ptr::eq(self, other),
            (Value::Range(a), Value::Range(b)) => a == b,
            // Chan and TaskJoin: compare by Arc pointer (identity)
            (Value::Chan(a), Value::Chan(b)) => Arc::ptr_eq(a, b),
            (Value::TaskJoin(a), Value::TaskJoin(b)) => Arc::ptr_eq(a, b),
            // Db handles: identity (same connection)
            (Value::Db(a), Value::Db(b)) => a == b,
            // Opaque handles: same tag and pool id (same object).
            (Value::Opaque(a), Value::Opaque(b)) => a == b,
            _ => false,
        }
    }
}

#[cfg(test)]
mod size_assert {
    use super::*;
    #[test]
    fn value_fits_in_16_bytes() {
        assert!(
            std::mem::size_of::<Value>() <= 16,
            "Value grew to {} bytes",
            std::mem::size_of::<Value>()
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn int_displays_plain() {
        assert_eq!(Value::Int(42).to_string(), "42");
    }

    #[test]
    fn float_always_shows_decimal() {
        assert_eq!(Value::Float(3.0).to_string(), "3.0");
        assert_eq!(Value::Float(3.5).to_string(), "3.5");
    }

    #[test]
    fn unit_displays_empty() {
        assert_eq!(Value::Unit.to_string(), "");
    }

    #[test]
    fn array_displays() {
        assert_eq!(
            Value::Array(Box::new(vec![Value::Int(1), Value::Int(2)])).to_string(),
            "[1, 2]"
        );
    }

    #[test]
    fn dict_displays() {
        assert_eq!(
            Value::Dict(Box::new(vec![
                (Value::Str("a".to_string().into()), Value::Int(1)),
                (Value::Str("b".to_string().into()), Value::Int(2)),
            ]))
            .to_string(),
            "{a: 1, b: 2}"
        );
    }

    #[test]
    fn variants_display() {
        assert_eq!(
            Value::Option(Some(Box::new(Value::Int(1)))).to_string(),
            ".some(1)"
        );
        assert_eq!(Value::Option(None).to_string(), ".none");
        assert_eq!(
            Value::Result(Box::new(Ok(Value::Int(1)))).to_string(),
            ".ok(1)"
        );
        assert_eq!(
            Value::Result(Box::new(Err(Value::Str("x".to_string().into())))).to_string(),
            ".err(x)"
        );
    }

    #[test]
    fn opaque_handle_value() {
        let h = zz_native_rt::alloc("regex", std::sync::Arc::new(1u32));
        let v = Value::Opaque(Box::new(h.clone()));
        assert_eq!(v.type_name(), "regex");
        assert_eq!(v.method_namespace(), Some("regex"));
        assert_eq!(v.to_string(), format!("<regex #{}>", h.id));
        assert_eq!(v, Value::Opaque(Box::new(h.clone())));
        let other = Value::Opaque(Box::new(zz_native_rt::Handle {
            tag: "uuid".to_string(),
            id: h.id,
        }));
        assert_ne!(v, other);
        assert_ne!(v, Value::Int(1));
        assert!(zz_native_rt::drop_handle(h.id));
    }
}
