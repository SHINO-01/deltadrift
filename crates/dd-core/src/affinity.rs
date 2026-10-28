//! CPU pinning. Best-effort: on hosts with fewer cores than requested (or
//! inside restricted containers) we report failure and continue unpinned.
//!
//! Pitfall this module guards against: `core_affinity::get_core_ids()`
//! returns the *calling thread's current mask*, not the machine's cores, and
//! threads inherit their parent's mask. Querying after any thread has pinned
//! (or from a child of a pinned thread) yields a single core, so later pins
//! silently fail and every stage time-slices on one CPU. We therefore snapshot
//! the allowed set exactly once, before any pinning, and pin by explicit id.

use std::sync::OnceLock;

static ALLOWED: OnceLock<Vec<usize>> = OnceLock::new();

/// Snapshot the process's allowed cores. Call from `main` before spawning or
/// pinning anything; later calls are no-ops.
pub fn init() -> &'static [usize] {
    ALLOWED.get_or_init(|| core_affinity::get_core_ids().unwrap_or_default().into_iter().map(|c| c.id).collect())
}

/// Pins the calling thread to logical core `core`. Returns whether it stuck.
pub fn pin_current(core: usize) -> bool {
    init().contains(&core) && core_affinity::set_for_current(core_affinity::CoreId { id: core })
}

/// Number of logical cores this process was allowed at [`init`] time.
pub fn available_cores() -> usize {
    init().len().max(1)
}

#[cfg(test)]
mod tests {
    /// Regression: a child of a pinned thread must still be able to pin elsewhere.
    #[test]
    fn child_of_pinned_thread_can_repin() {
        let cores = super::init().to_vec();
        if cores.len() < 2 {
            return;
        }
        let (a, b) = (cores[0], cores[1]);
        std::thread::spawn(move || {
            assert!(super::pin_current(a));
            let child = std::thread::spawn(move || super::pin_current(b));
            assert!(child.join().unwrap(), "child failed to pin to core {b}");
        })
        .join()
        .unwrap();
    }
}
