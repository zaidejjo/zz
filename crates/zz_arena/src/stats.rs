//! Arena counters: allocation volume, block health, waste. Clone cheaply
//! per phase and log with `ZZ_ARENA_STATS=1`.

/// Snapshot of arena counters. All fields public for telemetry export.
#[derive(Debug, Default, Clone)]
pub struct Stats {
    /// Payload bytes handed out (excludes alignment padding).
    pub bytes_used: u64,
    /// Live block capacity (includes padding + unused tails).
    pub capacity_bytes: u64,
    /// Live block count (set by `Bump::stats()`).
    pub blocks_alive: u64,
    /// Live spill count (set by `Bump::stats()`).
    pub spills_alive: u64,
    /// Cumulative oversized allocations.
    pub spills: u64,
    /// Cumulative block growths.
    pub grows: u64,
    /// Cumulative resets.
    pub resets: u64,
}

impl Stats {
    /// Unused-but-reserved bytes (fragmentation signal).
    pub fn waste_bytes(&self) -> u64 {
        self.capacity_bytes.saturating_sub(self.bytes_used)
    }

    /// Waste as a fraction of capacity (0.0–1.0). Spikes mean blocks are
    /// badly sized for the workload — tune `initial_block` / hints.
    pub fn waste_ratio(&self) -> f64 {
        if self.capacity_bytes == 0 {
            return 0.0;
        }
        self.waste_bytes() as f64 / self.capacity_bytes as f64
    }
}
