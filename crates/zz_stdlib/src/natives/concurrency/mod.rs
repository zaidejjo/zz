//! Channel concurrency primitives for ZZ.
//!
//! Provides `chan()`, `chan.send()`, `chan.recv()`, `chan.try_recv()`,
//! `spawn()`, `task.join()`, and `task.try_join()`.
//!
//! Tasks are *green threads*: `spawn` builds an owned task (VM +
//! interpreter + chunk, ~microseconds) and hands it to the [`executor`]
//! (one parked OS thread per CPU). Tasks run until they complete or yield
//! at a blocking call; the executor parks and resumes them as other tasks
//! (or the main thread) make progress. Green threads therefore multiplex
//! onto a few OS threads and never block them on ZZ synchronization.

pub(crate) mod executor;

use std::collections::{HashMap, VecDeque};
use std::sync::{atomic::AtomicUsize, Arc, Condvar, Mutex};

use zz_runtime::{EvalError, Interp, Span, Value};

use executor::Executor;
use zz_runtime::value::snapshot_env_pruned;
use zz_runtime::value::{ChanInner, ChanState, TaskJoinState};

/// `chan()` — create a new unbounded channel.
pub(crate) fn chan_new(
    _interp: &mut Interp,
    _args: &mut Vec<Value>,
    _span: Span,
) -> Result<Value, EvalError> {
    use std::sync::atomic::{AtomicBool, AtomicUsize};
    use zz_runtime::lf_chan::LfRing;
    #[allow(clippy::arc_with_non_send_sync)]
    let state = Arc::new(ChanState {
        ring: LfRing::new(),
        spill: AtomicUsize::new(0),
        green_parked: AtomicBool::new(false),
        cvar_waiters: AtomicUsize::new(0),
        inner: Mutex::new(ChanInner {
            queue: VecDeque::new(),
            green_waiters: Vec::new(),
        }),
        cvar: Condvar::new(),
    });
    Ok(Value::Chan(state))
}

/// `chan.send(ch, v)` — push a value into the channel.
pub(crate) fn chan_send(
    _interp: &mut Interp,
    args: &mut Vec<Value>,
    span: Span,
) -> Result<Value, EvalError> {
    let ch = args
        .first()
        .cloned()
        .ok_or_else(|| EvalError::new("missing argument `ch` for chan.send", span))?;
    let v = args
        .get(1)
        .cloned()
        .ok_or_else(|| EvalError::new("missing argument `v` for chan.send", span))?;

    match ch {
        Value::Chan(state) => {
            use std::sync::atomic::Ordering;
            // Enqueue lock-free; waiter service always takes the mutex.
            // (An earlier revision skipped servicing when the parked flag
            // read clear — unsound: a lock-free enqueue racing a park
            // registration can strand the waiter with a value present but
            // nobody coming. The mutex pairs registration against drain,
            // closing the window; uncontended it costs ~25ns, no futex.)
            // `try_enqueue` hands the value back on failure, so the spill
            // path below moves the *returned* value, never a copy.
            if let Err(back) = state.ring.try_enqueue(v) {
                // Ring full — spill under the mutex (channels stay
                // unbounded no matter how deep the burst).
                let mut guard = state
                    .inner
                    .lock()
                    .map_err(|e| EvalError::new(format!("chan.send: lock poisoned: {e}"), span))?;
                guard.queue.push_back(back);
                state.spill.fetch_add(1, Ordering::Release);
                service_chan_waiters(&state, guard, span);
                return Ok(Value::Unit);
            }
            let guard = state
                .inner
                .lock()
                .map_err(|e| EvalError::new(format!("chan.send: lock poisoned: {e}"), span))?;
            service_chan_waiters(&state, guard, span);
            Ok(Value::Unit)
        }
        other => Err(EvalError::new(
            format!("chan.send: expected a channel, found `{other}`"),
            span,
        )),
    }
}

/// Serve channel waiters after an enqueue (caller holds no lock on entry;
/// takes it): wake every parked green waiter (each takes one value at
/// park time or via its deliver slot) and notify main-thread sleepers.
/// Clears `green_parked` when the list drains empty — both directions
/// hold the mutex, so no registration slips between the take and the
/// clear. Called with the value already published (ring or spill), which
/// is what makes the wakeup sound.
fn service_chan_waiters(
    state: &Arc<ChanState>,
    mut guard: std::sync::MutexGuard<'_, ChanInner>,
    _span: Span,
) {
    use std::sync::atomic::Ordering;
    // The take empties the list while we hold the mutex, so no
    // registration slips between: the flag can go clear unconditionally.
    let waiters = std::mem::take(&mut guard.green_waiters);
    state.green_parked.store(false, Ordering::Release);
    drop(guard);
    for wid in waiters {
        Executor::wake_chan_waiter(state, wid);
    }
    // Notify main-thread sleepers only when some exist: an empty futex
    // wake is pure overhead on the fast path.
    if state.cvar_waiters.load(Ordering::Acquire) > 0 {
        state.cvar.notify_one();
    }
}

/// `chan.recv(ch)` — blocking receive. Waits until a value is available.
///
/// On an executor thread running compiled code, an empty channel suspends
/// the green task instead of parking the thread (the executor replaces the
/// dummy `Unit` below with the real value before resume). Everywhere else (main thread, nested interpreter frames) the
/// thread parks on the condvar as before; a replacement executor thread
/// keeps throughput up in the nested case.
pub(crate) fn chan_recv(
    _interp: &mut Interp,
    args: &mut Vec<Value>,
    span: Span,
) -> Result<Value, EvalError> {
    use zz_runtime::value::{interp_depth, on_executor, request_yield, YieldReason};

    let ch = args
        .first()
        .cloned()
        .ok_or_else(|| EvalError::new("missing argument `ch` for chan.recv", span))?;

    match ch {
        Value::Chan(state) => {
            use std::sync::atomic::Ordering;
            // Fast path: no waiter can exist and the spill is empty — pop
            // straight from the ring, zero locks. Sound because waiters
            // only register on empty (under the mutex), and every enqueue
            // after a registration wakes or notifies before returning: a
            // fast pop can never steal a waiter-promised value.
            if !state.green_parked.load(Ordering::Acquire)
                && state.cvar_waiters.load(Ordering::Acquire) == 0
                && state.spill.load(Ordering::Acquire) == 0
            {
                if let Some(v) = state.ring.try_dequeue() {
                    return Ok(v);
                }
            }
            let mut guard = state
                .inner
                .lock()
                .map_err(|e| EvalError::new(format!("chan.recv: lock poisoned: {e}"), span))?;
            // Slow loop: spill first (older than anything that arrived
            // while the ring was full), then the ring. A ring miss with
            // both tiers nominally non-empty is transient (a publisher
            // mid-claim) — loop and re-check rather than parking on a
            // value that is already on its way.
            loop {
                if let Some(v) = guard.queue.pop_front() {
                    state.spill.fetch_sub(1, Ordering::Release);
                    return Ok(v);
                }
                if let Some(v) = state.ring.try_dequeue() {
                    return Ok(v);
                }
                if on_executor() && interp_depth() == 0 {
                    // Suspend: the executor re-checks under this same lock
                    // at park time, so no send can slip between the check
                    // above and the park. Dummy replaced before resume.
                    request_yield(YieldReason::ChanWait {
                        chan: Arc::clone(&state),
                    });
                    return Ok(Value::Unit);
                }
                // Main thread (or nested interpreter): spin briefly for a
                // value already on its way before paying a condvar sleep.
                // Same no-steal gate as the fast path — a concurrent green
                // registration aborts the spin into the locked path below.
                if let Some(v) = Executor::spin_for_value(|| {
                    if state.green_parked.load(Ordering::Acquire)
                        || state.cvar_waiters.load(Ordering::Acquire) != 0
                        || state.spill.load(Ordering::Acquire) != 0
                    {
                        return None;
                    }
                    state.ring.try_dequeue()
                }) {
                    return Ok(v);
                }
                // Legacy park. On an executor thread (nested interpreter
                // frames above us) top up a replacement so throughput
                // never collapses.
                if on_executor() {
                    Executor::top_up();
                }
                // Counted sleepers: sends notify the condvar only when
                // this is non-zero, so waiter-free traffic pays no futex
                // wake. Re-checked after every wake (spurious wakeups and
                // racing consumers just loop).
                state.cvar_waiters.fetch_add(1, Ordering::AcqRel);
                let waited = state.cvar.wait_while(guard, |inner| {
                    inner.queue.is_empty() && state.ring.len_estimate() == 0
                });
                state.cvar_waiters.fetch_sub(1, Ordering::AcqRel);
                guard = waited.map_err(|e| {
                    EvalError::new(format!("chan.recv: condvar wait failed: {e}"), span)
                })?;
            }
        }
        other => Err(EvalError::new(
            format!("chan.recv: expected a channel, found `{other}`"),
            span,
        )),
    }
}

/// `chan.try_recv(ch)` — non-blocking receive. Returns `.some(v)` or `.none`.
pub(crate) fn chan_try_recv(
    _interp: &mut Interp,
    args: &mut Vec<Value>,
    span: Span,
) -> Result<Value, EvalError> {
    let ch = args
        .first()
        .cloned()
        .ok_or_else(|| EvalError::new("missing argument `ch` for chan.try_recv", span))?;

    match ch {
        Value::Chan(state) => {
            use std::sync::atomic::Ordering;
            let mut guard = state
                .inner
                .lock()
                .map_err(|e| EvalError::new(format!("chan.try_recv: lock poisoned: {e}"), span))?;
            // Spill first (older), then the ring — same order as recv.
            if let Some(v) = guard.queue.pop_front() {
                state.spill.fetch_sub(1, Ordering::Release);
                return Ok(Value::Option(Some(Box::new(v))));
            }
            Ok(match state.ring.try_dequeue() {
                Some(v) => Value::Option(Some(Box::new(v))),
                None => Value::Option(None),
            })
        }
        other => Err(EvalError::new(
            format!("chan.try_recv: expected a channel, found `{other}`"),
            span,
        )),
    }
}

// ── TaskJoin methods ────────────────────────────────────────────────────────

/// `spawn(closure)` — spawn a closure on a new OS thread.
///
/// The closure's captured environment is deep-cloned (snapshot) so the
/// spawned thread gets its own independent copy of all captured variables.
/// Returns a `TaskJoin` handle whose `.recv()` method blocks until the
/// task completes and yields the closure's return value.
pub(crate) fn spawn(
    interp: &mut Interp,
    args: &mut Vec<Value>,
    span: Span,
) -> Result<Value, EvalError> {
    let fv = match args.first() {
        Some(Value::Func(f)) => (**f).clone(),
        Some(other) => {
            return Err(EvalError::new(
                format!("spawn: expected a closure, found `{other}`"),
                span,
            ))
        }
        None => {
            return Err(EvalError::new(
                "spawn: expected a closure, found nothing",
                span,
            ))
        }
    };

    let chunk = fv.chunk.ok_or_else(|| {
        EvalError::new(
            "spawn: closure has no compiled chunk (tree-walker closures cannot be spawned)",
            span,
        )
    })?;
    let param_count = fv.params.len();

    // Debug aid: ZZ_SPAWN_PROFILE=1 prints per-spawn timing breakdowns.
    // Used by perf investigations (W2); no production code depends on it.
    let profiling = std::env::var("ZZ_SPAWN_PROFILE").is_ok();
    let t_all = profiling.then(std::time::Instant::now);
    // 1. Snapshot captured env → self-contained flat map.
    // Reachability is computed first (it only needs the table for
    // membership, not a snapshot) so prunable function entries — same
    // object the table slice carries, or never referenced — are dropped
    // before cloning.
    let t0 = profiling.then(std::time::Instant::now);
    // Reachability is cached per spawn-site chunk (the same `Arc` serves
    // every loop iteration): steady state skips the op walk + table scan.
    // The entry holds the chunk `Arc` (address reuse impossible) and the
    // table version (new runtime functions invalidate).
    let chunk_key = Arc::as_ptr(&chunk) as *const () as usize;
    let (reachable, loads) = match interp.reach_cache.get(&chunk_key) {
        Some(e) if e.version == interp.funcs_version && Arc::ptr_eq(&e.chunk, &chunk) => {
            // Arc clones: steady state shares the sets, never recopies
            // (deref coercion passes them as `&HashSet` below).
            (Arc::clone(&e.reachable), Arc::clone(&e.loads))
        }
        _ => {
            if interp.reach_cache.len() >= 64 {
                interp.reach_cache.clear();
            }
            let (reachable, loads) = zz_runtime::value::reachable_refs(&chunk, &interp.funcs);
            let (reachable, loads) = (Arc::new(reachable), Arc::new(loads));
            interp.reach_cache.insert(
                chunk_key,
                zz_runtime::value::ReachCacheEntry {
                    chunk: Arc::clone(&chunk),
                    version: interp.funcs_version,
                    reachable: Arc::clone(&reachable),
                    loads: Arc::clone(&loads),
                },
            );
            (reachable, loads)
        }
    };
    let dt_reach = t0.map(|t| t.elapsed());
    let t0 = profiling.then(std::time::Instant::now);
    let snapshot = snapshot_env_pruned(
        &fv.env,
        &interp.funcs,
        &reachable,
        &loads,
        &mut interp.spawn_keep_cache,
    );
    let n_snapshot = snapshot.len();
    let dt_env = t0.map(|t| t.elapsed());
    let t0 = profiling.then(std::time::Instant::now);

    // 2. Clone read-only globals + snapshot the ZZ function table so
    // workers can call ordinary ZZ functions (not just natives).
    // `snapshot_funcs` flattens every captured env into a fresh copy;
    // moving those across threads is covered by `Send for FuncValue`.
    //
    // Versioned cache: the table only changes when a function is defined
    // (`MakeFunc` bumps `funcs_version`). The cached base grows by the
    // missing reachable subset per spawn, so steady-state spawn cost is
    // one detach-clone of (usually few) functions instead of a full
    // re-snapshot. Each worker still receives fully independent envs —
    // the cache itself is never mutated or shared.
    let natives = interp.natives.clone();
    let n_natives = natives.len();
    let dt_natives = t0.map(|t| t.elapsed());
    let t0 = profiling.then(std::time::Instant::now);
    let structs = interp.structs.clone();
    let dt_structs = t0.map(|t| t.elapsed());
    let t0 = profiling.then(std::time::Instant::now);
    // Reachable-only table slice: the worker carries just the functions
    // its chunk (transitively) references. Env entries pruned above fall
    // through to these table copies; shadowing definitions were kept.
    //
    // Incremental base cache: the cached base holds every func snapshotted
    // so far at the current table version and grows by the missing subset
    // per spawn. The first spawn therefore pays only for its own reachable
    // set (often empty) instead of a full-table snapshot, and later spawns
    // with new reachability extend rather than rebuild. A version change
    // (runtime-defined function) resets the base.
    // Empty-reachable fast path: no table entries can be referenced, so
    // the detached subset is empty — skip the version check, the
    // per-spawn `missing` set allocation, and the detach walk.
    let funcs = if reachable.is_empty() {
        HashMap::new()
    } else {
        if interp.spawn_funcs_cache.as_ref().map(|(v, _)| *v) != Some(interp.funcs_version) {
            interp.spawn_funcs_cache = Some((interp.funcs_version, HashMap::new()));
        }
        {
            let base = &mut interp.spawn_funcs_cache.as_mut().expect("just reset").1;
            let missing: std::collections::HashSet<String> = reachable
                .iter()
                .filter(|n| !base.contains_key(n.as_str()))
                .cloned()
                .collect();
            if !missing.is_empty() {
                let extra = zz_runtime::value::snapshot_funcs_subset(&interp.funcs, &missing);
                base.extend(extra);
            }
        }
        zz_runtime::value::detach_cached_subset(
            &interp.spawn_funcs_cache.as_ref().expect("reset above").1,
            &reachable,
        )
    };
    let dt_funcs = t0.map(|t| t.elapsed());
    if profiling {
        let mut loads_sorted: Vec<&String> = loads.iter().collect();
        loads_sorted.sort();
        eprintln!(
            "[spawn] nfuncs={} reach={} loads={loads_sorted:?} snap_entries={} natives={} reach_tm={:?} env={:?} natives_tm={:?} structs_tm={:?} funcs_tm={:?} total={:?}",
            interp.funcs.len(),
            reachable.len(),
            n_snapshot,
            n_natives,
            dt_reach,
            dt_env,
            dt_natives,
            dt_structs,
            dt_funcs,
            t_all.map(|t| t.elapsed()),
        );
    }

    // 3. Shared result state. The executor publishes the outcome here on
    // completion and wakes every waiter (green waiters via the ready queue,
    // main-thread joiners via the condvar).
    let state = Arc::new(TaskJoinState {
        result: Mutex::new(zz_runtime::value::TaskJoinInner::default()),
        cvar_waiters: AtomicUsize::new(0),
        cvar: Condvar::new(),
    });

    // 4. Build the green task: an owned VM + task-mode interpreter seeded
    // from the snapshots, then hand it to the executor. The snapshots are
    // fully detached, so no state is shared with the spawner or other
    // tasks (`Send` rests on that construction — see `GreenTask`).
    let mut new_interp = Interp::with_natives_shared(natives);
    new_interp.structs = structs;
    new_interp.funcs = funcs;
    new_interp.task_mode = true;

    // Seat one Unit arg per closure param at the stack bottom:
    // slot-indexed locals address base+slot, so base must precede
    // the seated args (running on an empty stack shifts every slot
    // and corrupts locals or panics out of bounds). `spawn` takes
    // no inputs by contract, so Unit is the only sane default;
    // `|_|` closures ignore it.
    let mut vm = zz_runtime::vm::Vm::new();
    for _ in 0..param_count {
        vm.push(Value::Unit);
    }

    // Seed env from snapshot.
    {
        let mut env = new_interp.env.borrow_mut();
        for (name, val) in snapshot {
            env.define(&name, val);
        }
    }

    Executor::spawn_task(vm, new_interp, chunk, Arc::clone(&state));

    #[allow(clippy::arc_with_non_send_sync)]
    Ok(Value::TaskJoin(state))
}

/// `task.join(handle)` — blocking receive on a task join handle.
/// Returns the task's result, or `.err(msg)` if the task panicked.
///
/// Green tasks suspend instead of parking (same protocol as `chan.recv`);
/// everywhere else the thread parks on the condvar, topping up the
/// executor when nested on one of its threads.
pub(crate) fn task_join(
    _interp: &mut Interp,
    args: &mut Vec<Value>,
    span: Span,
) -> Result<Value, EvalError> {
    use zz_runtime::value::{interp_depth, on_executor, request_yield, YieldReason};

    let handle = args
        .first()
        .cloned()
        .ok_or_else(|| EvalError::new("missing argument `handle` for task.join", span))?;

    match handle {
        Value::TaskJoin(state) => {
            let mut guard = state
                .result
                .lock()
                .map_err(|e| EvalError::new(format!("task.join: lock poisoned: {e}"), span))?;
            if let Some(outcome) = guard.result.take() {
                return Ok(executor::outcome_to_value(outcome));
            }
            if on_executor() && interp_depth() == 0 {
                request_yield(YieldReason::JoinWait {
                    handle: Arc::clone(&state),
                });
                return Ok(Value::Unit);
            }
            if on_executor() {
                Executor::top_up();
            }
            // Wait for completion (not just result presence): a consumed
            // result (`try_join` took it) never comes back, and waiting on
            // it would hang forever. Counted sleepers let completions
            // skip the condvar notify when nobody waits (same protocol
            // as channel sleepers).
            use std::sync::atomic::Ordering;
            state.cvar_waiters.fetch_add(1, Ordering::AcqRel);
            let waited = state
                .cvar
                .wait_while(guard, |inner| inner.result.is_none() && !inner.completed);
            state.cvar_waiters.fetch_sub(1, Ordering::AcqRel);
            let mut guard = waited.map_err(|e| {
                EvalError::new(format!("task.join: condvar wait failed: {e}"), span)
            })?;
            match guard.result.take() {
                Some(Ok(v)) => Ok(v),
                Some(Err(msg)) => Ok(Value::Result(Box::new(Err(Value::Str(Box::new(msg)))))),
                None => Err(EvalError::new(
                    "task.join: result already consumed (try_join took it)",
                    span,
                )),
            }
        }
        other => Err(EvalError::new(
            format!("task.join: expected a task join handle, found `{other}`"),
            span,
        )),
    }
}

/// `task.try_join(handle)` — non-blocking check on a task join handle.
/// Returns `.some(result)` or `.none`. Never blocks or yields, and never
/// consumes: any number of `try_join` calls (plus one taking `task.join`)
/// resolve from a single completion.
pub(crate) fn task_try_join(
    _interp: &mut Interp,
    args: &mut Vec<Value>,
    span: Span,
) -> Result<Value, EvalError> {
    let handle = args
        .first()
        .cloned()
        .ok_or_else(|| EvalError::new("missing argument `handle` for task.try_join", span))?;

    match handle {
        Value::TaskJoin(state) => {
            let guard = state
                .result
                .lock()
                .map_err(|e| EvalError::new(format!("task.try_join: lock poisoned: {e}"), span))?;
            match guard.result.clone() {
                Some(Ok(v)) => Ok(Value::Option(Some(Box::new(Value::Result(Box::new(Ok(
                    v,
                ))))))),
                Some(Err(msg)) => Ok(Value::Option(Some(Box::new(Value::Result(Box::new(Err(
                    Value::Str(Box::new(msg)),
                ))))))),
                None => Ok(Value::Option(None)),
            }
        }
        other => Err(EvalError::new(
            format!("task.try_join: expected a task join handle, found `{other}`"),
            span,
        )),
    }
}
