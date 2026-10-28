//! Incremental, point-in-time feature estimators.
//!
//! Every estimator follows the same contract: `update(x)` returns a value
//! computed **only from observations strictly before `x`** and *then* folds
//! `x` into its state. This makes look-ahead leakage structurally impossible
//! (NFR-4) and keeps live and backtest code paths identical.
//!
//! All operations are O(1), branch-light and allocation-free.

#![forbid(unsafe_code)]

/// Exponentially-weighted mean and variance (West's incremental form).
#[derive(Debug, Clone, Copy)]
pub struct Ewma {
    alpha: f64,
    mean: f64,
    var: f64,
    n: u64,
}

impl Ewma {
    /// `alpha` in (0, 1]; equivalently a span of `2/alpha - 1` observations.
    pub fn new(alpha: f64) -> Self {
        assert!(alpha > 0.0 && alpha <= 1.0, "alpha must be in (0, 1]");
        Self { alpha, mean: 0.0, var: 0.0, n: 0 }
    }

    pub fn from_halflife(obs: f64) -> Self {
        Self::new(1.0 - (-core::f64::consts::LN_2 / obs).exp())
    }

    /// Returns the surprise z-score of `x` against the *prior* state, then
    /// updates. Returns 0 until two observations have been seen.
    #[inline]
    pub fn update(&mut self, x: f64) -> f64 {
        let z = if self.n >= 2 && self.var > 0.0 { (x - self.mean) / self.var.sqrt() } else { 0.0 };
        if self.n == 0 {
            self.mean = x;
        } else {
            let d = x - self.mean;
            let incr = self.alpha * d;
            self.mean += incr;
            self.var = (1.0 - self.alpha) * (self.var + d * incr);
        }
        self.n += 1;
        z
    }

    #[inline(always)]
    pub fn mean(&self) -> f64 {
        self.mean
    }
    #[inline(always)]
    pub fn var(&self) -> f64 {
        self.var
    }
    #[inline(always)]
    pub fn count(&self) -> u64 {
        self.n
    }
}

/// Accumulated surprise with continuous-time exponential decay:
/// `s(t) = s(t_prev) * 2^(-(t - t_prev)/halflife) + z`.
///
/// Time-based (not tick-based) decay means bursts of ticks and quiet periods
/// are weighted by wall-clock relevance, which is what macro surprises and
/// event shocks require.
#[derive(Debug, Clone, Copy)]
pub struct DecayedSurprise {
    neg_ln2_over_hl: f64,
    value: f64,
    last_ns: u64,
}

impl DecayedSurprise {
    pub fn new(halflife_ns: u64) -> Self {
        assert!(halflife_ns > 0);
        Self { neg_ln2_over_hl: -core::f64::consts::LN_2 / halflife_ns as f64, value: 0.0, last_ns: 0 }
    }

    /// Decays to `now_ns`, adds `z`, returns the new value.
    #[inline]
    pub fn update(&mut self, now_ns: u64, z: f64) -> f64 {
        let dt = now_ns.saturating_sub(self.last_ns) as f64;
        self.value = self.value * (self.neg_ln2_over_hl * dt).exp() + z;
        self.last_ns = now_ns;
        self.value
    }

    /// Value decayed to `now_ns` without adding anything (read-only peek).
    #[inline]
    pub fn peek(&self, now_ns: u64) -> f64 {
        let dt = now_ns.saturating_sub(self.last_ns) as f64;
        self.value * (self.neg_ln2_over_hl * dt).exp()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ewma_converges_and_is_point_in_time() {
        let mut e = Ewma::new(0.05);
        // The first z is always 0: nothing in the past to compare against.
        assert_eq!(e.update(10.0), 0.0);
        for i in 0..5_000 {
            e.update(if i % 2 == 0 { 1.0 } else { -1.0 });
        }
        assert!(e.mean().abs() < 0.05);
        assert!((e.var() - 1.0).abs() < 0.1);
        // A big shock is scored against the *previous* variance, not including itself.
        let z = e.update(10.0);
        assert!(z > 8.0, "z={z}");
    }

    #[test]
    fn surprise_halves_after_one_halflife() {
        let mut s = DecayedSurprise::new(1_000);
        s.update(0, 4.0);
        assert!((s.peek(1_000) - 2.0).abs() < 1e-12);
        assert!((s.update(2_000, 0.0) - 1.0).abs() < 1e-12);
    }
}
