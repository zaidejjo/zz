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

use std::collections::HashMap;
use std::sync::{
    atomic::{AtomicU64, Ordering},
    Arc, Mutex, OnceLock,
};
use std::time::{Duration, Instant};

use zz_runtime::runtime::Flow;
use zz_runtime::value::{
    with_executor_task, ChanInner, ChanState, TaskId, TaskJoinState, YieldReason,
};
use zz_runtime::vm::Vm;
use zz_runtime::{Interp, Value};

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
struct TaskEntry {
    task: Mutex<Option<GreenTask>>,
    deliver: Mutex<Option<Value>>,
}

pub(crate) struct Executor {
    registry: Mutex<HashMap<TaskId, Arc<TaskEntry>>>,
    next_id: AtomicU64,
    tx: crossbeam_channel::Sender<TaskId>,
    rx: crossbeam_channel::Receiver<TaskId>,
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
            let (tx, rx) = crossbeam_channel::unbounded::<TaskId>();
            let ex = Executor {
                registry: Mutex::new(HashMap::new()),
                next_id: AtomicU64::new(1),
                tx,
                rx,
            };
            for _ in 0..size {
                std::thread::spawn(Executor::worker_loop);
            }
            ex
        })
    }

    /// Park more executor capacity: spawn a replacement thread running the
    /// worker loop. Used when an executor thread must block the old way
    /// (nested interpreter frames above a blocking native, or blocking I/O
    /// natives) so throughput never collapses to zero.
    pub(crate) fn top_up() {
        let _ = Executor::global();
        std::thread::spawn(Executor::worker_loop);
    }

    fn worker_loop() {
        // Each worker holds its own receiver clone: crossbeam recv needs
        // no global lock, so workers never serialize on the ready queue
        // (the old `Mutex<Receiver>` made every slice take the same lock).
        let rx = Executor::global().rx.clone();
        // Hot spin while work flows: a worker that just ran a slice spins
        // on `try_recv` before falling back to the blocking `recv`, so a
        // rendezvous arriving within ~100µs costs PAUSEs instead of a
        // futex sleep + wake pair (~2-4µs). Cold workers (idle >1ms)
        // block immediately — no CPU burn at rest (REPL, sleeps).
        //
        // Adaptive budget (miss-streak backoff): spinning helps only with
        // a spare core for the peer. On a loaded machine spins just steal
        // the peer's timeslice and backfire (measured worse than blocking),
        // so consecutive misses halve the budget down to near-zero and a
        // hit restores it. Self-tuning both ways, no knobs.
        thread_local! {
            static MISS_STREAK: std::cell::Cell<u32> = const { std::cell::Cell::new(0) };
        }
        let mut last_active = Instant::now();
        loop {
            let mut id = None;
            if last_active.elapsed() < Duration::from_millis(1) {
                let budget_us: u64 = 100 >> MISS_STREAK.with(|s| s.get().min(7));
                if budget_us > 0 {
                    let spin_start = Instant::now();
                    let budget = Duration::from_micros(budget_us);
                    while spin_start.elapsed() < budget {
                        match rx.try_recv() {
                            Ok(got) => {
                                id = Some(got);
                                MISS_STREAK.with(|s| s.set(0));
                                break;
                            }
                            Err(crossbeam_channel::TryRecvError::Disconnected) => return,
                            Err(crossbeam_channel::TryRecvError::Empty) => std::hint::spin_loop(),
                        }
                    }
                    if id.is_none() {
                        MISS_STREAK.with(|s| s.set(s.get() + 1));
                    }
                }
            }
            let got = match id {
                Some(got) => got,
                None => match rx.recv() {
                    Ok(got) => got,
                    Err(_) => return, // Senders gone (teardown): exit.
                },
            };
            last_active = Instant::now();
            Executor::run_slice(got);
        }
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
        let entry = Arc::new(TaskEntry {
            task: Mutex::new(Some(GreenTask {
                id,
                vm,
                interp,
                chunk,
                handle,
                started: false,
            })),
            deliver: Mutex::new(None),
        });
        ex.registry.lock().unwrap().insert(id, Arc::clone(&entry));
        // Receivers live forever (detached threads), so this cannot fail in
        // practice; if it ever does the task is stranded — loud panic, never
        // a silent hang.
        ex.tx.send(id).expect("executor queue gone");
        if std::env::var("ZZ_SPAWN_PROFILE").is_ok() {
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
        let entry = match ex.registry.lock().unwrap().get(&wid) {
            Some(e) => Arc::clone(e),
            None => return,
        };
        match value {
            Some(v) => {
                *entry.deliver.lock().unwrap() = Some(v);
                ex.tx.send(wid).ok();
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
        let profile = std::env::var("ZZ_SPAWN_PROFILE").is_ok();
        let entry = match Executor::global().registry.lock().unwrap().get(&id) {
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
                Executor::global().tx.send(id).ok();
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
                if std::env::var("ZZ_SPAWN_PROFILE").is_ok() {
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
                if std::env::var("ZZ_SPAWN_PROFILE").is_ok() {
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
        let entry = match ex.registry.lock().unwrap().get(&task.id) {
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
                Executor::global().tx.send(id).ok();
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
                Executor::global().tx.send(id).ok();
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
                Executor::global().tx.send(id).ok();
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
        ex.registry.lock().unwrap().remove(&id);
        let mut inner = handle.result.lock().unwrap();
        let waiters = std::mem::take(&mut inner.green_waiters);
        inner.result = Some(outcome.clone());
        inner.completed = true;
        drop(inner);
        // Batched wakeups: resolve every waiter under a single registry
        // lock instead of one lock per waiter, then deliver + enqueue.
        // Fan-in completions (N waiters) drop from N registry round-trips
        // to one.
        let targets: Vec<(Arc<TaskEntry>, TaskId)> = {
            let reg = ex.registry.lock().unwrap();
            waiters
                .into_iter()
                .filter_map(|wid| reg.get(&wid).map(|e| (Arc::clone(e), wid)))
                .collect()
        };
        for (wentry, wid) in targets {
            *wentry.deliver.lock().unwrap() = Some(outcome_to_value(outcome.clone()));
            ex.tx.send(wid).ok();
        }
        handle.cvar.notify_all();
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
