//! Idle strategies for stages that find their input ring empty.
//!
//! Production runs on isolated cores use [`BusySpin`] to keep L1/L2 warm and
//! avoid scheduler wake-up latency. [`SpinThenYield`] is for shared machines
//! (laptops, CI) where burning a whole core is antisocial.

pub trait Wait {
    /// Called each time there was no work.
    fn idle(&mut self);
    /// Called after work was found.
    fn reset(&mut self) {}
}

#[derive(Debug, Default, Clone, Copy)]
pub struct BusySpin;

impl Wait for BusySpin {
    #[inline(always)]
    fn idle(&mut self) {
        core::hint::spin_loop();
    }
}

#[derive(Debug, Clone, Copy)]
pub struct SpinThenYield {
    spins: u32,
    limit: u32,
}

impl SpinThenYield {
    pub const fn new(limit: u32) -> Self {
        Self { spins: 0, limit }
    }
}

impl Default for SpinThenYield {
    fn default() -> Self {
        Self::new(1 << 10)
    }
}

impl Wait for SpinThenYield {
    #[inline]
    fn idle(&mut self) {
        if self.spins < self.limit {
            self.spins += 1;
            core::hint::spin_loop();
        } else {
            std::thread::yield_now();
        }
    }

    #[inline(always)]
    fn reset(&mut self) {
        self.spins = 0;
    }
}
