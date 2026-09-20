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
use std::sync::mpsc;
use std::sync::{
    atomic::{AtomicU64, Ordering},
    Arc, Mutex, OnceLock,
};

use zz_runtime::runtime::Flow;
use zz_runtime::value::{with_executor_task, ChanState, TaskId, TaskJoinState, YieldReason};
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
    tx: mpsc::Sender<TaskId>,
    rx: Mutex<mpsc::Receiver<TaskId>>,
}

impl Executor {
    fn global() -> &'static Executor {
        static EXECUTOR: OnceLock<Executor> = OnceLock::new();
        EXECUTOR.get_or_init(|| {
            let size = std::thread::available_parallelism()
                .map(|n| n.get())
                .unwrap_or(4)
                .max(2);
            let (tx, rx) = mpsc::channel::<TaskId>();
            let ex = Executor {
                registry: Mutex::new(HashMap::new()),
                next_id: AtomicU64::new(1),
                tx,
                rx: Mutex::new(rx),
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
        loop {
            let id = {
                let guard = Executor::global().rx.lock().unwrap();
                guard.recv()
            };
            match id {
                Ok(id) => Executor::run_slice(id),
                Err(_) => return, // Senders gone (process teardown): exit.
            }
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
        let value = chan.inner.lock().unwrap().queue.pop_front();
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
                chan.inner.lock().unwrap().green_waiters.push(wid);
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
                if profile {
                    eprintln!("[exec] run_slice id={id} EMPTY (requeue)");
                }
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

    /// Park on a channel: pop under the queue lock when possible (immediate
    /// resume), else register as a waiter and put back.
    fn park_chan(chan: &Arc<ChanState>, entry: &Arc<TaskEntry>, mut task: GreenTask) {
        let id = task.id;
        let value = {
            let mut inner = chan.inner.lock().unwrap();
            match inner.queue.pop_front() {
                Some(v) => Some(v),
                None => {
                    inner.green_waiters.push(id);
                    None
                }
            }
        };
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

    /// Park on a join handle: take a completed result when present
    /// (immediate resume), else register as a waiter and put back.
    ///
    /// The stored outcome is cloned, never consumed: any number of green
    /// joiners (plus one main-thread take) resolve from a single completion.
    fn park_join(handle: &Arc<TaskJoinState>, entry: &Arc<TaskEntry>, mut task: GreenTask) {
        let id = task.id;
        let outcome = {
            let mut inner = handle.result.lock().unwrap();
            if inner.result.is_some() {
                inner.result.clone()
            } else {
                inner.green_waiters.push(id);
                None
            }
        };
        match outcome {
            Some(o) => {
                if !task.vm.replace_top(outcome_to_value(o)) {
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
        for wid in waiters {
            let wentry = match ex.registry.lock().unwrap().get(&wid) {
                Some(e) => Arc::clone(e),
                None => continue,
            };
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
