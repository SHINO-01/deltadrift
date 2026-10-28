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

/// Chooses `n` logical CPUs for pinned stages: one per *physical* core
/// (never two SMT siblings, which would halve each busy-spinning stage),
/// fastest cores first (hybrid P/E-core CPUs), then by id. Falls back to
/// siblings only if there are fewer physical cores than `n`.
///
/// Reads Linux sysfs topology; elsewhere returns the allowed set in order.
pub fn plan_cores(n: usize) -> Vec<usize> {
    let allowed = init();
    let read = |cpu: usize, f: &str| std::fs::read_to_string(format!("/sys/devices/system/cpu/cpu{cpu}/{f}")).ok();
    // (group key = sibling list, max freq, cpu)
    let mut groups: Vec<(String, u64, usize)> = Vec::new();
    let mut extras: Vec<usize> = Vec::new();
    for &cpu in allowed {
        let sib = read(cpu, "topology/thread_siblings_list").map_or_else(|| cpu.to_string(), |s| s.trim().to_string());
        let freq = read(cpu, "cpufreq/cpuinfo_max_freq").and_then(|s| s.trim().parse().ok()).unwrap_or(0);
        if groups.iter().any(|g| g.0 == sib) {
            extras.push(cpu);
        } else {
            groups.push((sib, freq, cpu));
        }
    }
    groups.sort_by(|a, b| b.1.cmp(&a.1).then(a.2.cmp(&b.2)));
    groups.into_iter().map(|g| g.2).chain(extras).take(n).collect()
}

/// Number of logical cores this process was allowed at [`init`] time.
pub fn available_cores() -> usize {
    init().len().max(1)
}

#[cfg(test)]
mod tests {
    #[test]
    fn plan_cores_distinct_and_allowed() {
        let plan = super::plan_cores(4);
        let allowed = super::init();
        assert_eq!(plan.len(), 4.min(allowed.len()));
        let mut d = plan.clone();
        d.sort_unstable();
        d.dedup();
        assert_eq!(d.len(), plan.len());
        assert!(plan.iter().all(|c| allowed.contains(c)));
    }

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
