//! Strategy layer. Strategies output *views* (`mu`, `sigma2`) in [`Signal`]s;
//! they have no access to position sizing, limits, or brokers. The
//! self-improving engine (RL / GA / bandits, Sprint 5) plugs in here.

#![forbid(unsafe_code)]

use dd_core::{MAX_INSTRUMENTS, MarketEvent, Signal};
use dd_features::{DecayedSurprise, Ewma};
use serde::Deserialize;

pub trait Strategy {
    /// Called for every market event on the strategy core. Must not block or
    /// allocate. Return `None` to emit nothing for this event.
    fn on_event(&mut self, ev: &MarketEvent) -> Option<Signal>;
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ZScoreParams {
    pub id: u16,
    /// Half-life (in ticks) of the return mean/variance estimator.
    pub vol_halflife_ticks: f64,
    /// Half-life of the accumulated-surprise state, in milliseconds.
    pub surprise_halflife_ms: u64,
    /// Enter when |surprise| exceeds this.
    pub entry_z: f64,
    /// Exit when |surprise| falls below this.
    pub exit_z: f64,
    /// Expected fraction of one-tick sigma recovered per unit surprise.
    pub beta: f64,
    /// Holding horizon in ticks (scales variance).
    pub horizon_ticks: f64,
    pub warmup_ticks: u64,
}

impl Default for ZScoreParams {
    fn default() -> Self {
        Self {
            id: 1,
            vol_halflife_ticks: 500.0,
            surprise_halflife_ms: 50,
            entry_z: 2.5,
            exit_z: 0.5,
            beta: 0.05,
            horizon_ticks: 50.0,
            warmup_ticks: 1_000,
        }
    }
}

#[derive(Debug, Clone, Copy)]
struct InstState {
    last_mid: f64,
    ret: Ewma,
    surprise: DecayedSurprise,
    /// -1 short view, 0 flat, +1 long view.
    dir: i8,
}

/// Reference policy: short-horizon mean reversion on decayed return surprise.
/// Deliberately simple; it exists to exercise the pipeline end to end and to
/// serve as the baseline arm the Sprint 5 bandit must beat.
#[derive(Debug)]
pub struct ZScoreReversion {
    p: ZScoreParams,
    state: Box<[InstState]>,
}

impl ZScoreReversion {
    pub fn new(p: ZScoreParams) -> Self {
        assert!(p.exit_z < p.entry_z, "exit_z must be < entry_z");
        let init = InstState {
            last_mid: 0.0,
            ret: Ewma::from_halflife(p.vol_halflife_ticks),
            surprise: DecayedSurprise::new(p.surprise_halflife_ms * 1_000_000),
            dir: 0,
        };
        Self { state: vec![init; MAX_INSTRUMENTS].into_boxed_slice(), p }
    }
}

impl Strategy for ZScoreReversion {
    #[inline]
    fn on_event(&mut self, ev: &MarketEvent) -> Option<Signal> {
        let st = self.state.get_mut(ev.instrument.idx())?;
        let mid = ev.mid().to_f64();
        if mid <= 0.0 {
            return None;
        }
        let prev = core::mem::replace(&mut st.last_mid, mid);
        let (mu, sigma2) = if prev > 0.0 {
            let z = st.ret.update((mid / prev).ln());
            let s = st.surprise.update(ev.recv_ns, z);
            let p = &self.p;
            if st.ret.count() < p.warmup_ticks {
                (0.0, 0.0)
            } else {
                let a = s.abs();
                st.dir = match st.dir {
                    0 if a > p.entry_z => -(s.signum() as i8),
                    d if d != 0 && a < p.exit_z => 0,
                    d => d,
                };
                let sigma1 = st.ret.var().sqrt();
                let mu = f64::from(st.dir) * p.beta * a.max(p.exit_z) * sigma1;
                (mu, sigma1 * sigma1 * p.horizon_ticks)
            }
        } else {
            (0.0, 0.0)
        };
        Some(Signal::new(ev.recv_ns, ev.seq, ev.instrument, self.p.id, ev.mid(), ev.bid, ev.ask, mu, sigma2))
    }
}
