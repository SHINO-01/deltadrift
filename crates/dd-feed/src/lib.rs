//! Market data sources.
//!
//! [`SyntheticFeed`] powers `--demo` mode (FR-6): a deterministic,
//! regime-switching price generator that needs no network or credentials.
//! Live WebSocket adapters (io_uring) land in Sprint 2 and will emit the same
//! [`MarketEvent`]s.

#![forbid(unsafe_code)]

use dd_core::rng::SplitMix64;
use dd_core::{EventKind, InstrumentId, MarketEvent, Px, Qty};
use serde::Deserialize;

#[derive(Debug, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct SyntheticConfig {
    pub instruments: u32,
    pub seed: u64,
    pub start_px: f64,
    /// Per-tick log-return stdev in the calm regime.
    pub calm_vol: f64,
    /// Multiplier applied to `calm_vol` in the stressed regime.
    pub stress_mult: f64,
    /// Per-tick probability of switching regime.
    pub regime_switch_p: f64,
    /// Strength of the Ornstein–Uhlenbeck pull back to the fair-value walk.
    pub mean_reversion: f64,
    pub spread_bps: f64,
}

impl Default for SyntheticConfig {
    fn default() -> Self {
        Self {
            instruments: 8,
            seed: 0x00D3_17A0_D1F7,
            start_px: 100.0,
            calm_vol: 1e-4,
            stress_mult: 4.0,
            regime_switch_p: 1e-5,
            mean_reversion: 0.02,
            spread_bps: 1.0,
        }
    }
}

#[derive(Debug, Clone, Copy)]
struct Inst {
    /// Log of slowly-drifting fair value.
    fair: f64,
    /// Log deviation of the traded price from fair value (OU process).
    dev: f64,
    stressed: bool,
}

#[derive(Debug)]
pub struct SyntheticFeed {
    cfg: SyntheticConfig,
    rng: SplitMix64,
    insts: Vec<Inst>,
    seq: u64,
    half_spread: f64,
}

impl SyntheticFeed {
    pub fn new(cfg: SyntheticConfig) -> Self {
        assert!(cfg.instruments > 0 && cfg.instruments as usize <= dd_core::MAX_INSTRUMENTS);
        let insts = vec![Inst { fair: cfg.start_px.ln(), dev: 0.0, stressed: false }; cfg.instruments as usize];
        Self { rng: SplitMix64::new(cfg.seed), insts, seq: 0, half_spread: cfg.spread_bps * 0.5e-4, cfg }
    }

    /// Generates the next quote, round-robin across instruments.
    #[inline]
    pub fn next_event(&mut self, recv_ns: u64) -> MarketEvent {
        let n = self.insts.len() as u64;
        let id = (self.seq % n) as u32;
        let c = &self.cfg;
        let st = &mut self.insts[id as usize];
        if self.rng.next_f64() < c.regime_switch_p {
            st.stressed = !st.stressed;
        }
        let vol = if st.stressed { c.calm_vol * c.stress_mult } else { c.calm_vol };
        st.fair += 0.2 * vol * self.rng.next_gaussian();
        st.dev += -c.mean_reversion * st.dev + vol * self.rng.next_gaussian();
        let mid = (st.fair + st.dev).exp();
        let hs = mid * self.half_spread * if st.stressed { 3.0 } else { 1.0 };
        self.seq += 1;
        MarketEvent::new(
            recv_ns,
            0,
            self.seq,
            InstrumentId(id),
            EventKind::Quote,
            Px::from_f64(mid - hs),
            Px::from_f64(mid + hs),
            Px::from_f64(mid),
            Qty::ZERO,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deterministic_and_well_formed() {
        let mut a = SyntheticFeed::new(SyntheticConfig::default());
        let mut b = SyntheticFeed::new(SyntheticConfig::default());
        for _ in 0..10_000 {
            let (x, y) = (a.next_event(0), b.next_event(0));
            assert_eq!((x.bid, x.ask, x.seq), (y.bid, y.ask, y.seq));
            assert!(x.bid.0 > 0 && x.ask > x.bid);
        }
    }
}
