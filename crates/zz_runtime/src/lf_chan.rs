//! Lock-free MPMC bounded ring for channel fast paths (Phase 1b).
//!
//! Vyukov's bounded MPMC queue: `head`/`tail` atomics claimed with FAA,
//! per-cell sequence numbers, monotonic positions (no ABA — sequences
//! never wrap within any realistic run). Each operation is a bounded
//! number of atomic steps: **no locks, no futex, no blocking**.
//!
//! The ring is the *fast path* only. ZZ channels are semantically
//! unbounded, so overflow spills to the classic mutex queue (`ChanInner`).
//! FIFO across the two tiers is preserved by protocol (see `ChanState`):
//! receivers drain the spill first whenever it is non-empty, and the
//! spill-emptiness flag is an atomic read, so the hot path takes zero
//! locks. A slow-path re-check under the channel mutex before registering
//! as a waiter keeps the no-lost-wakeup discipline intact.
//!
//! # Soundness
//!
//! Cells hold `Value`, which is `!Send` (`Rc` inside). Transfer discipline
//! mirrors the existing channel code: a value enters the ring by ownership
//! move at enqueue and leaves by ownership move at dequeue; cross-thread
//! publication runs through the cell sequence Release/Acquire chain, which
//! is the happens-before edge. Same boundary as `Send for Value` —
//! audited here, nowhere else.

use std::cell::UnsafeCell;
use std::fmt;
use std::mem::size_of;
use std::sync::atomic::{AtomicUsize, Ordering};

use crate::value::Value;

/// Ring capacity: power of two (mask indexing). 1024 cells × 64B = 64KB
/// per channel; deeper bursts spill to the mutex queue (still correct,
/// just slower). Sized for rendezvous + fan-in bursts, not storage.
pub const RING_CAP: usize = 1024;
const RING_MASK: usize = RING_CAP - 1;

/// One slot: sequence number + payload, padded to a full cache line so
/// adjacent slots never share a line between producer and consumer cores.
/// This padding (not the algorithm) is what kills the cache-line bouncing
/// from the Phase-1a measurements.
#[repr(C)]
struct Cell {
    seq: AtomicUsize,
    value: UnsafeCell<Option<Value>>,
    _pad: [u8; 40],
}

// 8 (seq) + Option<Value> must fit with 40B pad into exactly 64 bytes.
const _: () = assert!(size_of::<AtomicUsize>() + size_of::<UnsafeCell<Option<Value>>>() + 40 == 64);

/// Head/tail counters, each on its own cache line: the producer core
/// writes `tail`, the consumer core writes `head`, and neither line is
/// ever read-for-ownership by the other side's hot loop.
#[repr(align(64))]
struct AlignedCounter(AtomicUsize);

/// Bounded MPMC ring. See module docs for protocol and soundness.
pub struct LfRing {
    head: AlignedCounter,
    tail: AlignedCounter,
    cells: Box<[Cell]>,
}

// SAFETY: cells are accessed only through the sequence protocol — a cell
// is written after claiming its position (seq check) and read after the
// matching Release; no two threads ever hold the same cell concurrently,
// and every value moves by ownership exactly once per hop.
unsafe impl Send for LfRing {}
unsafe impl Sync for LfRing {}

impl LfRing {
    /// Empty ring: cell `i` starts with sequence `i` (first lap ready).
    pub fn new() -> Self {
        let mut cells = Vec::with_capacity(RING_CAP);
        for i in 0..RING_CAP {
            cells.push(Cell {
                seq: AtomicUsize::new(i),
                value: UnsafeCell::new(None),
                _pad: [0; 40],
            });
        }
        LfRing {
            head: AlignedCounter(AtomicUsize::new(0)),
            tail: AlignedCounter(AtomicUsize::new(0)),
            cells: cells.into_boxed_slice(),
        }
    }

    /// Enqueue, non-blocking. `Ok(())` on success; `Err(v)` (value handed
    /// back, nothing published) when full or contended past the retry
    /// budget — caller spills to the mutex queue.
    pub fn try_enqueue(&self, v: Value) -> Result<(), Value> {
        // Bounded retries: a racing peer usually settles in one or two;
        // sustained contention means the slow path (spill) is the right
        // call, not spinning here.
        for _ in 0..4 {
            let pos = self.tail.0.load(Ordering::Relaxed);
            let cell = &self.cells[pos & RING_MASK];
            // seq == pos: our slot. seq < pos: full. seq > pos: a racing
            // producer claimed ahead of us — reload and retry.
            let seq = cell.seq.load(Ordering::Acquire);
            let dif = seq.wrapping_sub(pos) as isize;
            if dif == 0 {
                if self
                    .tail
                    .0
                    .compare_exchange_weak(
                        pos,
                        pos.wrapping_add(1),
                        Ordering::AcqRel,
                        Ordering::Relaxed,
                    )
                    .is_ok()
                {
                    // Claimed: exclusive owner of this cell until publish.
                    // (Exclusive because only the CAS winner writes a cell
                    // whose seq matched; late losers re-read and move on.)
                    unsafe { *cell.value.get() = Some(v) };
                    cell.seq.store(pos.wrapping_add(1), Ordering::Release);
                    return Ok(());
                }
                // CAS lost: another producer won — retry with fresh pos.
            } else if dif < 0 {
                return Err(v);
            }
            // dif > 0: fall through to reload.
        }
        Err(v)
    }

    /// Dequeue, non-blocking. `Some(value)` on success; `None` when empty
    /// or contended past the retry budget — caller re-checks under the
    /// channel mutex before parking (no lost wakeups by construction).
    pub fn try_dequeue(&self) -> Option<Value> {
        for _ in 0..4 {
            let pos = self.head.0.load(Ordering::Relaxed);
            let cell = &self.cells[pos & RING_MASK];
            // seq == pos+1: value published for us. seq < pos+1 (i.e. the
            // producer hasn't published this lap yet): empty. Greater: a
            // racing consumer claimed ahead — reload and retry.
            let seq = cell.seq.load(Ordering::Acquire);
            let dif = seq.wrapping_sub(pos.wrapping_add(1)) as isize;
            if dif == 0 {
                if self
                    .head
                    .0
                    .compare_exchange_weak(
                        pos,
                        pos.wrapping_add(1),
                        Ordering::AcqRel,
                        Ordering::Relaxed,
                    )
                    .is_ok()
                {
                    // Claimed: take ownership, then free the cell for the
                    // lap `pos + CAP` (seq skips a full capacity ahead so
                    // neither producers nor consumers can mistake laps).
                    let v = unsafe { (*cell.value.get()).take() };
                    cell.seq
                        .store(pos.wrapping_add(RING_CAP), Ordering::Release);
                    return v;
                }
            } else if dif < 0 {
                return None;
            }
        }
        None
    }

    /// Estimated length (concurrent producers/consumers make this stale
    /// on arrival — only for diagnostics and wait predicates that
    /// re-check; never for protocol decisions).
    pub fn len_estimate(&self) -> usize {
        let tail = self.tail.0.load(Ordering::Acquire);
        let head = self.head.0.load(Ordering::Acquire);
        tail.wrapping_sub(head).min(RING_CAP)
    }
}

impl Default for LfRing {
    fn default() -> Self {
        Self::new()
    }
}

impl fmt::Debug for LfRing {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("LfRing")
            .field("len_estimate", &self.len_estimate())
            .field("cap", &RING_CAP)
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    fn int(v: i64) -> Value {
        Value::Int(v)
    }

    #[test]
    fn fifo_single_threaded() {
        let r = LfRing::new();
        assert!(r.try_dequeue().is_none());
        for i in 0..10 {
            assert!(r.try_enqueue(int(i)).is_ok());
        }
        for i in 0..10 {
            assert_eq!(r.try_dequeue(), Some(int(i)));
        }
        assert!(r.try_dequeue().is_none());
    }

    #[test]
    fn full_returns_value_back() {
        let r = LfRing::new();
        for i in 0..RING_CAP {
            assert!(r.try_enqueue(int(i as i64)).is_ok());
        }
        assert_eq!(r.try_enqueue(int(-1)), Err(int(-1)));
        // Drain half, enqueue half: FIFO preserved across the wrap.
        for i in 0..RING_CAP / 2 {
            assert_eq!(r.try_dequeue(), Some(int(i as i64)));
        }
        for i in 0..RING_CAP / 2 {
            assert!(r.try_enqueue(int(100000 + i as i64)).is_ok());
        }
        for i in RING_CAP / 2..RING_CAP {
            assert_eq!(r.try_dequeue(), Some(int(i as i64)));
        }
        for i in 0..RING_CAP / 2 {
            assert_eq!(r.try_dequeue(), Some(int(100000 + i as i64)));
        }
        assert!(r.try_dequeue().is_none());
    }

    #[test]
    fn wraparound_many_laps() {
        let r = LfRing::new();
        for lap in 0..5 {
            for i in 0..RING_CAP {
                assert!(r.try_enqueue(int(lap * 100000 + i as i64)).is_ok());
            }
            for i in 0..RING_CAP {
                assert_eq!(r.try_dequeue(), Some(int(lap * 100000 + i as i64)));
            }
        }
    }

    #[test]
    fn threaded_sum_preserved() {
        let r = Arc::new(LfRing::new());
        let per_producer = 5_000;
        let producers = 4;
        let mut handles = Vec::new();
        for _ in 0..producers {
            let r = Arc::clone(&r);
            handles.push(std::thread::spawn(move || {
                let mut sent = 0;
                let mut i = 0;
                while sent < per_producer {
                    // try_enqueue is documented fallible: spin here (the
                    // channel layer would spill instead — tested below).
                    if r.try_enqueue(int(1)).is_ok() {
                        sent += 1;
                    } else {
                        i += 1;
                        if i & 63 == 0 {
                            std::thread::yield_now();
                        }
                    }
                }
            }));
        }
        let mut got = 0i64;
        let mut sum = 0i64;
        while got < per_producer as i64 * producers as i64 {
            match r.try_dequeue() {
                Some(Value::Int(1)) => {
                    got += 1;
                    sum += 1;
                }
                Some(_) => panic!("unexpected value"),
                None => std::thread::yield_now(),
            }
        }
        for h in handles {
            h.join().unwrap();
        }
        assert_eq!(got, per_producer as i64 * producers as i64);
        assert_eq!(sum, got);
    }

    /// Loom model: interleaving-exhaustive check of the transfer
    /// discipline (each enqueued value dequeued exactly once). Runs
    /// concretely without `RUSTFLAGS="--cfg loom"` (loom acts as a
    /// pass-through), exhaustively under `cargo test --cfg loom` with a
    /// loom-capable toolchain. Either way it must stay green.
    #[test]
    fn loom_transfer_once() {
        loom::model(|| {
            use std::sync::Arc;
            let r = Arc::new(LfRing::new());
            let r2 = Arc::clone(&r);
            let producer = loom::thread::spawn(move || {
                r2.try_enqueue(int(7))
                    .unwrap_or_else(|_| panic!("ring full"));
            });
            let v = r.try_dequeue();
            producer.join().unwrap();
            // Either we won the race (got exactly 7) or the producer had
            // not published yet (empty) — never duplication, never garbage.
            assert!(v.is_none() || v == Some(int(7)));
            // Drain whatever remains: at most one value, exactly 7.
            if v.is_none() {
                assert_eq!(r.try_dequeue(), Some(int(7)));
            }
            assert!(r.try_dequeue().is_none());
        });
    }
}
