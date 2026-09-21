//! Green-thread executor for ZZ tasks.
//!
//! `task.spawn` creates a [`GreenTask`] (owned VM + interpreter + chunk)
//! and hands it to a pool of executor threads (one per CPU). Tasks run
//! until they complete or *yield* at a blocking call (`chan.recv` /
//! `task.join` on an unready object); the executor parks them and resumes
//! them when another task (or the main thread) makes progress. Executor
//! threads therefore never block on ZZ synchronization — unlike the old
//! thread-per-task model — so thousands of tasks multiplex onto a few OS
//! threads and `spawn` costs microseconds, not the ~75µs of thread
//! creation.
//!
//! Soundness rests on exclusive ownership: a task is held either by the
//! registry entry (parked) or by exactly one executor thread (running),
//! transferred via the queue (which provides the happens-before edge).
//! Snapshots are fully detached before handoff (see `snapshot_funcs`).

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::sync::{
    atomic::{AtomicU64, AtomicUsize, Ordering},
    Arc, Mutex, OnceLock,
};
use std::time::{Duration, Instant};

use crossbeam_deque::{Injector, Steal, Stealer, Worker};

use zz_runtime::runtime::Flow;
use zz_runtime::value::{
    with_executor_task, ChanInner, ChanState, TaskId, TaskJoinState, YieldReason,
};
use zz_runtime::vm::Vm;
use zz_runtime::{Interp, Value};

/// Debug aid flag, cached: `std::env::var` costs a lock + allocation
/// (~150ns) per call — reading it per spawn/slice/park would tax every
/// task lifecycle ~0.7µs for a diagnostic that is almost always off.
/// `OnceLock` reads are a single atomic load after init.
pub(crate) fn profiling() -> bool {
    static PROFILING: OnceLock<bool> = OnceLock::new();
    *PROFILING.get_or_init(|| std::env::var("ZZ_SPAWN_PROFILE").is_ok())
}

/// One suspended-or-runnable green thread: everything needed to resume it.
///
/// # Thread safety
///
/// `Vm` and `Interp` contain `Rc`-shared environments and are `!Send` by
/// default. A `GreenTask` crosses threads exactly once per handoff (spawn →
/// queue → executor → park → resume), and every handoff moves *exclusive*
/// ownership: the snapshots it was built from are detached (no `Rc` aliases
/// the spawner's scopes), and the registry hands it to one thread at a time.
/// Same discipline as `Send for Value`.
pub struct GreenTask {
    id: TaskId,
    vm: Vm,
    interp: Interp,
    chunk: Arc<zz_runtime::Chunk>,
    handle: Arc<TaskJoinState>,
    /// Set after the first slice: later slices resume the existing frames
    /// instead of pushing a fresh one (which would re-execute the chunk).
    started: bool,
}

// SAFETY: see above. All cross-thread sharing inside (channels, handles)
// is via `Arc<Mutex<..>>` with atomic park/wake protocols.
unsafe impl Send for GreenTask {}

/// Registry slot for a task: the parked task (absent while running) plus a
/// delivery slot where wakers leave the blocking call's real value.
/// Shells recycle through per-shard pools (see `RegistryShard`).
struct TaskEntry {
    task: Mutex<Option<GreenTask>>,
    deliver: Mutex<Option<Value>>,
}

/// One registry shard: id-keyed entries plus a bounded pool of recycled
/// shells. Sharding (16-way by task id) turns the 3-mutex-ops-per-task
/// lifecycle (insert / lookup / remove) from one contended lock into 16
/// uncontended ones; pooling reuses the `Arc` + two `Mutex`es instead of
/// reallocating them per spawn (~0.5µs saved where it matters: bursts).
struct RegistryShard {
    map: HashMap<TaskId, Arc<TaskEntry>>,
    pool: Vec<Arc<TaskEntry>>,
}

const REGISTRY_SHARDS: usize = 16;
/// Max pooled shells per shard (16 × 64 caps retained memory while
/// covering any realistic parked-task population).
const SHARD_POOL_CAP: usize = 64;

pub(crate) struct Executor {
    registry: [Mutex<RegistryShard>; REGISTRY_SHARDS],
    next_id: AtomicU64,
    /// Global overflow queue: main-thread spawns and anything enqueued off
    /// a worker land here. Workers steal from it when their local deque
    /// runs dry (Phase 2 work-stealing).
    injector: Injector<TaskId>,
    /// One stealer per founding worker (fixed at init). Top-up threads
    /// carry private deques and steal from these, but are never stolen
    /// from themselves — their work is always theirs to run.
    stealers: Vec<Stealer<TaskId>>,
    /// All worker threads (for wakeups) + sleeping-worker count. Injector
    /// pushes unpark sleepers; the park token protocol (std
    /// `park`/`unpark`) makes missed wakeups impossible — see worker loop.
    parkers: Mutex<Vec<std::thread::Thread>>,
    sleepers: AtomicUsize,
    /// Round-robin cursor for single-sleeper wakeups (see
    /// `unpark_sleepers`).
    wake_next: AtomicUsize,
}

/// This thread's work-stealing state. `None` off workers (main thread):
/// enqueues then route to the global injector instead of a local deque.
struct LocalState {
    worker: Worker<TaskId>,
    /// xorshift64 seed for random steal-victim order (no dep, no lock).
    rng: Cell<u64>,
}

thread_local! {
    static LOCAL: RefCell<Option<LocalState>> = const { RefCell::new(None) };
}

/// Spin quantum for park paths (see `spin_for_value`): how long a worker
/// burns PAUSEs re-checking an object before registering as a waiter and
/// sleeping. Sized so a peer arriving from a fresh wakeup usually lands
/// inside the spin (peer handoff ~1µs) while a truly absent peer costs at
/// most this much extra before the futex sleep it would have paid anyway.
const SPIN_QUANTUM: Duration = Duration::from_micros(3);

impl Executor {
    fn global() -> &'static Executor {
        static EXECUTOR: OnceLock<Executor> = OnceLock::new();
        EXECUTOR.get_or_init(|| {
            let size = std::thread::available_parallelism()
                .map(|n| n.get())
                .unwrap_or(4)
                .max(2);
            // Build workers + stealers first so every thread sees the
            // full victim set from its first steal round.
            let mut workers = Vec::with_capacity(size);
            let mut stealers = Vec::with_capacity(size);
            for _ in 0..size {
                let w = Worker::<TaskId>::new_lifo();
                stealers.push(w.stealer());
                workers.push(w);
            }
            let ex = Executor {
                registry: std::array::from_fn(|_| {
                    Mutex::new(RegistryShard {
                        map: HashMap::new(),
                        pool: Vec::new(),
                    })
                }),
                next_id: AtomicU64::new(1),
                injector: Injector::new(),
                stealers,
                parkers: Mutex::new(Vec::new()),
                sleepers: AtomicUsize::new(0),
                wake_next: AtomicUsize::new(0),
            };
            for (index, worker) in workers.into_iter().enumerate() {
                std::thread::spawn(move || Self::run_worker(worker, index));
            }
            ex
        })
    }

    /// Park more executor capacity: spawn a replacement thread with a
    /// private deque. Used when an executor thread must block the old way
    /// (nested interpreter frames above a blocking native, or blocking I/O
    /// natives) so throughput never collapses to zero. The top-up deque
    /// is never stolen from — its work is always its own to run — but it
    /// steals from everyone else like any worker.
    pub(crate) fn top_up() {
        let _ = Executor::global();
        std::thread::spawn(|| Self::run_worker(Worker::new_lifo(), usize::MAX));
    }

    /// Route a task id to a queue: the current worker's local deque when
    /// running on one (LIFO: the waker thread very likely runs it next —
    /// hot cache, no cross-thread hop), else the global injector. This is
    /// the direct-handoff path: completions and wakeups land where the
    /// progress just happened.
    fn enqueue(id: TaskId) {
        let pushed_local = LOCAL
            .try_with(|cell| {
                if let Some(st) = cell.borrow().as_ref() {
                    st.worker.push(id);
                    true
                } else {
                    false
                }
            })
            .unwrap_or(false);
        if !pushed_local {
            let ex = Executor::global();
            ex.injector.push(id);
            Self::unpark_sleepers(ex);
        }
    }

    /// Wake a sleeping worker after an injector push. Round-robin ONE
    /// sleeper per push — never all: unpark-all thunders the herd
    /// (measured +10µs/spawn on bursts — every spawn woke 4 threads for
    /// 1 task). One wakeup per push is enough; woken workers that find
    /// nothing re-park via the announce-then-verify sleep path, and the
    /// pending park token makes missed wakeups impossible either way.
    /// Skipped entirely when nobody sleeps (the hot steady state).
    fn unpark_sleepers(ex: &Executor) {
        if ex.sleepers.load(Ordering::Acquire) == 0 {
            return;
        }
        let parkers = ex.parkers.lock().unwrap();
        if !parkers.is_empty() {
            // `wake_next` doubles as the round-robin cursor (wraps by
            // construction — no modulo needed on overflow).
            let cursor = ex.wake_next.fetch_add(1, Ordering::Relaxed);
            parkers[cursor % parkers.len()].unpark();
        }
    }

    /// xorshift64 step (per-worker steal order — no dep, no lock).
    fn next_rand(seed: &Cell<u64>) -> u64 {
        let mut x = seed.get().max(0x9E3779B97F4A7C15);
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        seed.set(x);
        x
    }

    /// Fetch one task: own pop first (LIFO hot), else a single steal
    /// from a random victim, else one from the injector.
    ///
    /// Siblings-before-injector: sibling deques hold requeued/wakeup
    /// tasks (cache-hot continuations), the injector holds fresh bursts.
    /// Trying siblings first keeps continuations local; bursts still
    /// drain fine one steal later. (Measured: injector-first is slower —
    /// all workers hammering one queue head beats no one.)
    ///
    /// Deliberately single-task (no batching): batch moves proved
    /// pathological under contention — a worker grabbing half a sibling's
    /// deep queue just gets re-stolen in halves by the others, so tasks
    /// migrate deque-to-deque with full CAS storms instead of running.
    /// One move per task (injector/sibling → here) keeps traffic minimal;
    /// the LIFO local deque still gives cache-hot runs whenever wakeups
    /// land directly on it (see `enqueue`).
    fn fetch(ex: &Executor, worker: &Worker<TaskId>, seed: &Cell<u64>) -> Option<TaskId> {
        if let Some(id) = worker.pop() {
            return Some(id);
        }
        let n = ex.stealers.len();
        if n > 0 {
            let start = (Self::next_rand(seed) as usize) % n;
            for k in 0..n {
                match ex.stealers[(start + k) % n].steal() {
                    Steal::Success(id) => return Some(id),
                    Steal::Empty | Steal::Retry => {}
                }
            }
        }
        match ex.injector.steal() {
            Steal::Success(id) => Some(id),
            Steal::Empty | Steal::Retry => None,
        }
    }

    fn run_worker(worker: Worker<TaskId>, index: usize) {
        let ex = Executor::global();
        ex.parkers.lock().unwrap().push(std::thread::current());
        LOCAL.with(|cell| {
            *cell.borrow_mut() = Some(LocalState {
                worker,
                rng: Cell::new(
                    (index as u64)
                        .wrapping_mul(0x9E3779B97F4A7C15)
                        .wrapping_add(1),
                ),
            });
        });
        // Hot spin while work flows: local pop + steal rounds on PAUSEs
        // before sleeping, so a rendezvous arriving within ~100µs costs
        // no futex pair. Cold workers (idle >1ms) skip to the park —
        // no CPU burn at rest. Adaptive budget (miss-streak backoff):
        // consecutive empty rounds halve the budget to near-zero and a
        // hit restores it, so loaded machines degrade to blocking
        // instead of spin-backfiring. Self-tuning, no knobs.
        thread_local! {
            static MISS_STREAK: std::cell::Cell<u32> = const { std::cell::Cell::new(0) };
        }
        let mut last_active = Instant::now();
        loop {
            let mut id = LOCAL.with(|cell| {
                let st = cell.borrow();
                st.as_ref().and_then(|st| st.worker.pop())
            });
            if id.is_none() && last_active.elapsed() < Duration::from_millis(1) {
                let budget_us: u64 = 100 >> MISS_STREAK.with(|s| s.get().min(7));
                if budget_us > 0 {
                    // Light spin: own-deque pops only (thread-private, zero
                    // shared traffic), batched fetch every 16th iteration.
                    // A fetch hammers shared atomics; doing it per PAUSE
                    // starves the very threads (main's spawn snapshots)
                    // whose progress we are waiting for.
                    let spin_start = Instant::now();
                    let budget = Duration::from_micros(budget_us);
                    let mut iters = 0u32;
                    while id.is_none() && spin_start.elapsed() < budget {
                        id = LOCAL.with(|cell| {
                            let st = cell.borrow();
                            st.as_ref().and_then(|st| {
                                st.worker.pop().or_else(|| {
                                    iters += 1;
                                    if iters & 15 == 0 {
                                        Self::fetch(ex, &st.worker, &st.rng)
                                    } else {
                                        None
                                    }
                                })
                            })
                        });
                        if id.is_none() {
                            std::hint::spin_loop();
                        }
                    }
                    if id.is_some() {
                        MISS_STREAK.with(|s| s.set(0));
                    } else {
                        MISS_STREAK.with(|s| s.set(s.get() + 1));
                    }
                }
            }
            if let Some(got) = id {
                last_active = Instant::now();
                Executor::run_slice(got);
                continue;
            }
            // Cold: announce sleep, re-verify (announce-then-verify vs
            // injector pushes — a push landing between our last steal and
            // the park is caught here), then sleep on the park token.
            ex.sleepers.fetch_add(1, Ordering::AcqRel);
            let found = LOCAL.with(|cell| {
                let st = cell.borrow();
                st.as_ref()
                    .and_then(|st| Self::fetch(ex, &st.worker, &st.rng))
            });
            match found {
                Some(got) => {
                    ex.sleepers.fetch_sub(1, Ordering::AcqRel);
                    last_active = Instant::now();
                    Executor::run_slice(got);
                }
                None => {
                    std::thread::park();
                    ex.sleepers.fetch_sub(1, Ordering::AcqRel);
                }
            }
        }
    }

    /// Shard for a task id (power-of-two mask — ids are a dense counter,
    /// so shards balance exactly).
    fn shard(id: TaskId) -> usize {
        (id as usize) & (REGISTRY_SHARDS - 1)
    }

    /// Enqueue a fresh task. Returns its id.
    pub(crate) fn spawn_task(
        vm: Vm,
        interp: Interp,
        chunk: Arc<zz_runtime::Chunk>,
        handle: Arc<TaskJoinState>,
    ) -> TaskId {
        let ex = Executor::global();
        let id = ex.next_id.fetch_add(1, Ordering::Relaxed);
        // Checkout a pooled shell when available (task/deliver are empty
        // by construction — see `complete`), else allocate fresh. Single
        // shard-lock hold for checkout + insert.
        let mut shard = ex.registry[Self::shard(id)].lock().unwrap();
        let entry = match shard.pool.pop() {
            Some(e) => {
                *e.task.lock().unwrap() = Some(GreenTask {
                    id,
                    vm,
                    interp,
                    chunk,
                    handle,
                    started: false,
                });
                e
            }
            None => Arc::new(TaskEntry {
                task: Mutex::new(Some(GreenTask {
                    id,
                    vm,
                    interp,
                    chunk,
                    handle,
                    started: false,
                })),
                deliver: Mutex::new(None),
            }),
        };
        shard.map.insert(id, Arc::clone(&entry));
        drop(shard);
        // Routed to the current worker's deque when spawning nested
        // (LIFO warmth), else the global injector. Cannot fail: queues
        // are unbounded and live for the process.
        Self::enqueue(id);
        if profiling() {
            eprintln!("[exec] spawn id={id}");
        }
        id
    }

    /// Wake one channel waiter drained by `chan.send`: pop a queued value
    /// for it (delivered over the yielded call's dummy before resume), or
    /// re-park it when the queue ran dry (more waiters than values — the
    /// next send retries).
    pub(crate) fn wake_chan_waiter(chan: &Arc<ChanState>, wid: TaskId) {
        let ex = Executor::global();
        // Spill first (older than ring arrivals during full episodes),
        // then the ring — same order as every other pop site.
        let value = {
            let mut inner = chan.inner.lock().unwrap();
            inner
                .queue
                .pop_front()
                .inspect(|_| {
                    chan.spill.fetch_sub(1, Ordering::Release);
                })
                .or_else(|| chan.ring.try_dequeue())
        };
        let entry = match ex.registry[Self::shard(wid)].lock().unwrap().map.get(&wid) {
            Some(e) => Arc::clone(e),
            None => return,
        };
        match value {
            Some(v) => {
                *entry.deliver.lock().unwrap() = Some(v);
                Self::enqueue(wid);
            }
            None => {
                // Out-raced for the value (a concurrent fast pop stole
                // it): re-park WITH the flag set, or nobody will ever
                // service this waiter again.
                let mut inner = chan.inner.lock().unwrap();
                inner.green_waiters.push(wid);
                chan.green_parked.store(true, Ordering::Release);
            }
        }
    }

    /// Run one task slice: take it, deliver any pending value, execute to
    /// completion or the next yield, then complete or park it.
    fn run_slice(id: TaskId) {
        let profile = profiling();
        let entry = match Executor::global().registry[Self::shard(id)]
            .lock()
            .unwrap()
            .map
            .get(&id)
        {
            Some(e) => Arc::clone(e),
            None => return, // Completed and reaped between enqueue and run.
        };
        let mut task = match entry.task.lock().unwrap().take() {
            Some(t) => t,
            None => {
                // Taken by nobody we know: the task is either running
                // elsewhere (shouldn't happen — single ownership) or was
                // just parked after a racing wakeup. Requeue and yield so
                // the racing park lands first.
                Self::enqueue(id);
                std::thread::yield_now();
                return;
            }
        };
        // Deliver a value left by a waker over the yielded call's dummy.
        let delivered = entry.deliver.lock().unwrap().take();
        if profile {
            eprintln!("[exec] run_slice id={id} delivered={}", delivered.is_some());
        }
        if let Some(v) = delivered {
            if !task.vm.replace_top(v) {
                Self::complete(task, Err("internal error: yield with empty stack".into()));
                return;
            }
        }
        let outcome = with_executor_task(id, || {
            std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                if task.started {
                    task.vm.resume_chunk(&mut task.interp)
                } else {
                    task.started = true;
                    task.vm
                        .run_chunk_with_base(&task.chunk, &mut task.interp, 0)
                }
            }))
        });
        match outcome {
            Ok(Ok(Flow::Value(v))) | Ok(Ok(Flow::Return(v))) => {
                if profiling() {
                    eprintln!("[exec] complete id={id}");
                }
                Self::complete(task, Ok(v));
            }
            Ok(Ok(Flow::Break(span))) => {
                Self::complete(task, Err(format!("`break` outside of a loop at {span:?}")))
            }
            Ok(Ok(Flow::Continue(span))) => Self::complete(
                task,
                Err(format!("`continue` outside of a loop at {span:?}")),
            ),
            Ok(Ok(Flow::Yield(reason))) => {
                if profiling() {
                    eprintln!("[exec] park id={id} reason={reason:?}");
                }
                Self::park(task, reason);
            }
            Ok(Err(e)) => Self::complete(task, Err(e.message)),
            Err(_) => Self::complete(task, Err("spawned task panicked".into())),
        }
        let _ = id;
    }

    /// Suspend a task: re-check the blocking condition under the object's
    /// lock (atomic with wakeup registration), then either resume inline
    /// with the real value or register + put back.
    fn park(task: GreenTask, reason: YieldReason) {
        let ex = Executor::global();
        let entry = match ex.registry[Self::shard(task.id)]
            .lock()
            .unwrap()
            .map
            .get(&task.id)
        {
            Some(e) => Arc::clone(e),
            None => {
                // Reaped?? Only completion reaps, and completion implies the
                // task ran — impossible while it just yielded. Loud error.
                Self::complete(task, Err("internal error: yield for unknown task".into()));
                return;
            }
        };
        match reason {
            YieldReason::ChanWait { chan } => {
                Self::park_chan(&chan, &entry, task);
            }
            YieldReason::JoinWait { handle } => {
                Self::park_join(&handle, &entry, task);
            }
            YieldReason::Timeslice => {
                // Cooperative quantum expiry: no object involved, no lock
                // protocol — put back and requeue so siblings run.
                let id = task.id;
                *entry.task.lock().unwrap() = Some(task);
                Self::enqueue(id);
            }
        }
    }

    /// Spin-then-check: burn PAUSEs re-trying a non-blocking `check`
    /// until it yields `Some` or the quantum expires. `check` must be
    /// lock-free on the fast path (`try_lock`-based, never blocking):
    /// each attempt is ~25ns uncontended, so a peer arriving from a
    /// wakeup is usually caught with zero futex ops and zero registry
    /// churn. Returns the value on a hit, `None` on quantum expiry
    /// (caller falls back to registering + sleeping).
    ///
    /// Shared with the main-thread legacy park (`chan.recv`): the main
    /// thread has no slice to run while waiting, so spinning briefly
    /// before the condvar sleep avoids the futex pair the same way.
    pub(crate) fn spin_for_value<F>(mut check: F) -> Option<Value>
    where
        F: FnMut() -> Option<Value>,
    {
        // Cheap attempts first: no clock read at all — an immediately
        // ready peer resolves here in nanoseconds.
        for _ in 0..64 {
            if let Some(v) = check() {
                return Some(v);
            }
            std::hint::spin_loop();
        }
        // Then clock-gated spinning to the quantum: one ~20ns
        // `Instant::now` amortized over 64 attempts.
        let start = Instant::now();
        loop {
            for _ in 0..64 {
                if let Some(v) = check() {
                    return Some(v);
                }
                std::hint::spin_loop();
            }
            if start.elapsed() >= SPIN_QUANTUM {
                return None;
            }
        }
    }

    /// Park on a channel: spin for an arriving value first (fast path),
    /// else pop under the queue lock when possible (immediate resume),
    /// else register as a waiter and put back (slow path: sleep).
    fn park_chan(chan: &Arc<ChanState>, entry: &Arc<TaskEntry>, mut task: GreenTask) {
        let id = task.id;
        // Lock-free spin: only while no waiter can exist and the spill
        // is empty — a fast pop here must never steal a value already
        // promised to a parked waiter (see `ChanState` docs).
        let value = Self::spin_for_value(|| {
            if chan.green_parked.load(Ordering::Acquire)
                || chan.cvar_waiters.load(Ordering::Acquire) != 0
                || chan.spill.load(Ordering::Acquire) != 0
            {
                return None;
            }
            chan.ring.try_dequeue()
        });
        let value = value.or_else(|| {
            let mut inner = chan.inner.lock().unwrap();
            // Announce-then-verify: register FIRST, then check the tiers
            // under the same lock hold. Sends publish lock-free and skip
            // servicing when the parked flag reads clear, so checking
            // tiers before registering leaves a lost-wakeup window (send
            // enqueues + skips between our check and our store). With the
            // flag stored first, every later send drains us, and every
            // earlier send is visible to the check below; a hit unparks
            // (fast pops back off the moment the flag is set, so nothing
            // can steal between our store and our check).
            inner.green_waiters.push(id);
            chan.green_parked.store(true, Ordering::Release);
            if let Some(v) = inner.queue.pop_front() {
                chan.spill.fetch_sub(1, Ordering::Release);
                Self::unpark_chan_waiter(&mut inner, chan, id);
                Some(v)
            } else if let Some(v) = chan.ring.try_dequeue() {
                Self::unpark_chan_waiter(&mut inner, chan, id);
                Some(v)
            } else {
                None
            }
        });
        match value {
            Some(v) => {
                // Value arrived between the native's check and the park:
                // deliver inline and requeue without parking.
                if !task.vm.replace_top(v) {
                    Self::complete(task, Err("internal error: yield with empty stack".into()));
                    return;
                }
                *entry.task.lock().unwrap() = Some(task);
                Self::enqueue(id);
            }
            None => {
                *entry.task.lock().unwrap() = Some(task);
            }
        }
    }

    /// Remove a self-registration made moments ago (park re-check hit):
    /// caller holds the channel lock; clears the parked flag when the
    /// list drains empty so fast paths resume.
    fn unpark_chan_waiter(inner: &mut ChanInner, chan: &Arc<ChanState>, id: TaskId) {
        inner.green_waiters.retain(|&w| w != id);
        if inner.green_waiters.is_empty() {
            chan.green_parked.store(false, Ordering::Release);
        }
    }

    /// Park on a join handle: take a completed result when present
    /// (immediate resume), else register as a waiter and put back.
    ///
    /// The stored outcome is cloned, never consumed: any number of green
    /// joiners (plus one main-thread take) resolve from a single completion.
    fn park_join(handle: &Arc<TaskJoinState>, entry: &Arc<TaskEntry>, mut task: GreenTask) {
        let id = task.id;
        // Fast path: completions usually land within microseconds of the
        // wait — spin first, register only on a genuinely slow task.
        // Green waiters clone, never consume (see below), so the inline
        // hit needs no bookkeeping beyond the resume itself.
        let outcome = Self::spin_for_value(|| {
            handle
                .result
                .try_lock()
                .ok()?
                .result
                .clone()
                .map(outcome_to_value)
        });
        let outcome = outcome.or_else(|| {
            let mut inner = handle.result.lock().unwrap();
            match inner.result.clone() {
                Some(o) => Some(outcome_to_value(o)),
                None => {
                    inner.green_waiters.push(id);
                    None
                }
            }
        });
        match outcome {
            Some(o) => {
                if !task.vm.replace_top(o) {
                    Self::complete(task, Err("internal error: yield with empty stack".into()));
                    return;
                }
                *entry.task.lock().unwrap() = Some(task);
                Self::enqueue(id);
            }
            None => {
                *entry.task.lock().unwrap() = Some(task);
            }
        }
    }

    /// Finish a task: publish its outcome, wake every waiter, drop heavy
    /// state. Green waiters each receive a clone; main-thread joiners take
    /// the stored outcome via the condvar protocol. The registry entry is
    /// reaped here: after completion no new wakeups can arrive (all waiters
    /// were just served) and the id is never enqueued again, so late pops
    /// safely resolve to "reaped".
    fn complete(task: GreenTask, outcome: Result<Value, String>) {
        let ex = Executor::global();
        let handle = task.handle.clone();
        let id = task.id;
        // Heavy per-task state (VM stacks, interpreter envs) dies here;
        // only the slim outcome + handle survive for joiners.
        drop(task.vm);
        drop(task.interp);
        drop(task.chunk);
        // Reap + recycle: remove the entry, scrub any deliver residue
        // (defense: a stale deliver would corrupt the next occupant's
        // stack via `replace_top`), and pool the shell when there is
        // room. The task slot is already None (taken by the final slice).
        let mut shard = ex.registry[Self::shard(id)].lock().unwrap();
        if let Some(entry) = shard.map.remove(&id) {
            *entry.deliver.lock().unwrap() = None;
            if shard.pool.len() < SHARD_POOL_CAP {
                shard.pool.push(entry);
            }
        }
        drop(shard);
        let mut inner = handle.result.lock().unwrap();
        let waiters = std::mem::take(&mut inner.green_waiters);
        // Fast path: nobody ever waited — store by move (no clone) and
        // skip the condvar notify (no syscall). Fire-and-forget tasks
        // (channel fan-in) always land here.
        if waiters.is_empty() && handle.cvar_waiters.load(Ordering::Acquire) == 0 {
            inner.result = Some(outcome);
            inner.completed = true;
            return;
        }
        inner.result = Some(outcome.clone());
        inner.completed = true;
        drop(inner);
        // Batched wakeups: resolve every waiter (per-shard locks instead
        // of one lock per waiter), then deliver + enqueue. Fan-in
        // completions (N waiters) drop from N global round-trips to
        // uncontended shard hits.
        let targets: Vec<(Arc<TaskEntry>, TaskId)> = waiters
            .into_iter()
            .filter_map(|wid| {
                ex.registry[Self::shard(wid)]
                    .lock()
                    .unwrap()
                    .map
                    .get(&wid)
                    .map(|e| (Arc::clone(e), wid))
            })
            .collect();
        for (wentry, wid) in targets {
            *wentry.deliver.lock().unwrap() = Some(outcome_to_value(outcome.clone()));
            // Direct handoff: waiters land on the completing worker's own
            // deque (LIFO) — the thread that made progress very likely
            // runs them next, hot cache, no global-queue hop.
            Self::enqueue(wid);
        }
        // Notify main-thread joiners only when some exist (same counted-
        // sleeper protocol as channels).
        if handle.cvar_waiters.load(Ordering::Acquire) > 0 {
            handle.cvar.notify_all();
        }
    }
}

/// Map a task outcome to its join-visible value (shared by the executor
/// completion path and the `task.join` / `task.try_join` natives).
pub(crate) fn outcome_to_value(outcome: Result<Value, String>) -> Value {
    match outcome {
        Ok(v) => v,
        Err(msg) => Value::Result(Box::new(Err(Value::Str(Box::new(msg))))),
    }
}
