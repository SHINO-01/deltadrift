//! Monotonic nanosecond timestamps relative to a process-wide epoch.
//!
//! `Instant::now()` is a vDSO `clock_gettime(CLOCK_MONOTONIC)` on Linux
//! (~15–25 ns on both x86 TSC and Arm `CNTVCT_EL0`), with no syscall.

use std::sync::OnceLock;
use std::time::Instant;

static EPOCH: OnceLock<Instant> = OnceLock::new();

/// Initialise the epoch. Call once at startup before spawning stages so
/// every core shares the same zero.
pub fn init() {
    EPOCH.get_or_init(Instant::now);
}

/// Nanoseconds since [`init`]. Saturates at `u64::MAX` (~584 years).
#[inline(always)]
pub fn now_ns() -> u64 {
    let epoch = *EPOCH.get_or_init(Instant::now);
    epoch.elapsed().as_nanos() as u64
}
