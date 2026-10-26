//! Opt-in VM counters: `ZZ_VM_STATS=1 zz run prog.zz`.
//!
//! The interpreter has no sampling profiler (and `zz profile` is AOT-PGO
//! only), so "where do the ops go" had to be reconstructed with throwaway
//! probes. These counters make it a one-liner: total dispatched ops and the
//! share per [`OpClass`], printed to **stderr** at the end of a run so
//! benchmark stdout stays byte-identical.
//!
//! Cost when disabled: one relaxed load per `run_chunk` call (hoisted into a
//! local) plus a single predictable branch per dispatched op. Nothing is
//! allocated and nothing is printed.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use super::op::{Op, OpClass};

/// `ZZ_VM_STATS` was set. Resolved once, on the first VM entry.
static ENABLED: AtomicBool = AtomicBool::new(false);
/// Guards the one-time environment read.
static INIT: AtomicBool = AtomicBool::new(false);

static OPS: AtomicU64 = AtomicU64::new(0);
// One counter per `OpClass`, in `OpClass::ALL` order. Adding a class
// without adding a counter is a compile error (length mismatch).
static CLASS: [AtomicU64; OpClass::ALL.len()] = [
    AtomicU64::new(0),
    AtomicU64::new(0),
    AtomicU64::new(0),
    AtomicU64::new(0),
    AtomicU64::new(0),
    AtomicU64::new(0),
    AtomicU64::new(0),
    AtomicU64::new(0),
    AtomicU64::new(0),
];

/// Is stats collection on? Cheap after the first call (relaxed load).
pub fn enabled() -> bool {
    if !INIT.swap(true, Ordering::Relaxed) {
        ENABLED.store(std::env::var_os("ZZ_VM_STATS").is_some(), Ordering::Relaxed);
    }
    ENABLED.load(Ordering::Relaxed)
}

/// Count one dispatched op. Only called when [`enabled`] is true.
#[inline]
pub fn record(op: &Op) {
    OPS.fetch_add(1, Ordering::Relaxed);
    CLASS[op.class().index()].fetch_add(1, Ordering::Relaxed);
}

/// Snapshot of the counters (for tests and for `zz dis --vm --stats`-style
/// reports that want numbers, not printed output).
#[derive(Debug, Clone, Copy, Default)]
pub struct Snapshot {
    pub ops: u64,
    pub class: [u64; OpClass::ALL.len()],
}

impl Snapshot {
    pub fn class_count(&self, class: OpClass) -> u64 {
        self.class[class.index()]
    }

    /// Share of executed ops on the fused/specialized fast paths, 0.0–1.0.
    pub fn fast_share(&self) -> f64 {
        if self.ops == 0 {
            return 0.0;
        }
        let fast: u64 = OpClass::ALL
            .iter()
            .filter(|c| c.is_fast())
            .map(|c| self.class_count(*c))
            .sum();
        fast as f64 / self.ops as f64
    }
}

/// Read the counters without clearing them.
pub fn snapshot() -> Snapshot {
    let mut snap = Snapshot {
        ops: OPS.load(Ordering::Relaxed),
        ..Snapshot::default()
    };
    for (i, c) in OpClass::ALL.iter().enumerate() {
        snap.class[i] = CLASS[c.index()].load(Ordering::Relaxed);
    }
    snap
}

/// Reset the counters (tests; not exposed on the CLI).
pub fn reset() {
    OPS.store(0, Ordering::Relaxed);
    for c in CLASS.iter() {
        c.store(0, Ordering::Relaxed);
    }
}

/// Print the summary to stderr if `ZZ_VM_STATS` is set.
///
/// Called by the CLI once the program has finished; a no-op otherwise.
pub fn dump_if_enabled() {
    if !enabled() {
        return;
    }
    let snap = snapshot();
    let total = snap.ops.max(1) as f64;
    eprintln!("zz: vm stats: {} ops executed", snap.ops);
    for class in OpClass::ALL {
        let n = snap.class_count(class);
        if n == 0 {
            continue;
        }
        eprintln!(
            "zz:   {:<8} {:>12}  {:>5.1}%",
            class.name(),
            n,
            n as f64 * 100.0 / total
        );
    }
    eprintln!(
        "zz:   fast share {:.1}% (fused {:.1}%, int-op {:.1}%)",
        snap.fast_share() * 100.0,
        snap.class_count(OpClass::Fused) as f64 * 100.0 / total,
        snap.class_count(OpClass::IntOp) as f64 * 100.0 / total,
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Span;

    /// The counters are process-global statics, and libtest runs tests in
    /// parallel: without this guard one test's `reset` wipes another's
    /// records mid-assertion (flaky `snapshot_counts_ops_and_classes`).
    static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    fn serialized() -> std::sync::MutexGuard<'static, ()> {
        LOCK.lock().unwrap_or_else(|e| e.into_inner())
    }

    #[test]
    fn snapshot_counts_ops_and_classes() {
        let _guard = serialized();
        reset();
        record(&Op::Pop);
        record(&Op::SlotInc { slot: 0 });
        record(&Op::IntAdd(Span::default()));
        record(&Op::CallNative {
            name: "len".to_string(),
            argc: 1,
            span: Span::default(),
        });
        let snap = snapshot();
        assert_eq!(snap.ops, 4);
        assert_eq!(snap.class_count(OpClass::Stack), 1);
        assert_eq!(snap.class_count(OpClass::Fused), 1);
        assert_eq!(snap.class_count(OpClass::IntOp), 1);
        assert_eq!(snap.class_count(OpClass::Call), 1);
        assert!((snap.fast_share() - 0.5).abs() < f64::EPSILON);
        reset();
        assert_eq!(snapshot().ops, 0);
    }

    #[test]
    fn empty_snapshot_has_no_fast_share() {
        let _guard = serialized();
        reset();
        assert_eq!(snapshot().fast_share(), 0.0);
    }
}
