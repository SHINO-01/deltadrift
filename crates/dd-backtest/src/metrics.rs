//! Performance statistics and overfitting-aware significance tests.
//!
//! * Probabilistic Sharpe Ratio (Bailey & López de Prado, 2012): probability
//!   that the true Sharpe exceeds a benchmark given sample length, skew and
//!   kurtosis of returns.
//! * Deflated Sharpe Ratio (Bailey & López de Prado, 2014): PSR against the
//!   Sharpe you'd expect from the *best of N* skill-less trials — corrects for
//!   parameter search (selection bias). Gate G1 uses DSR.

use crate::sim::SimResult;

/// Trading days per year for FX (Mon–Fri rollovers).
pub const PERIODS_PER_YEAR: f64 = 260.0;
const EULER_GAMMA: f64 = 0.577_215_664_901_532_9;

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Metrics {
    pub days: usize,
    pub years: f64,
    pub total_return: f64,
    pub cagr: f64,
    /// Annualised.
    pub sharpe: f64,
    pub sortino: f64,
    /// Per-period (daily) Sharpe, skewness and (non-excess) kurtosis of daily returns.
    pub sr_daily: f64,
    pub skew: f64,
    pub kurtosis: f64,
    /// Peak-to-trough on daily closes.
    pub max_drawdown: f64,
    pub round_trips: usize,
    pub hit_rate: f64,
    pub profit_factor: f64,
    /// PSR vs. Sharpe 0.
    pub psr0: f64,
}

/// Daily simple returns from equity snapshots.
pub fn daily_returns(start: f64, days: &[crate::sim::DayPoint]) -> Vec<f64> {
    let mut prev = start;
    days.iter()
        .map(|d| {
            let r = if prev > 0.0 { d.equity / prev - 1.0 } else { 0.0 };
            prev = d.equity;
            r
        })
        .collect()
}

fn moments(r: &[f64]) -> (f64, f64, f64, f64) {
    let n = r.len() as f64;
    let mean = r.iter().sum::<f64>() / n;
    let (mut m2, mut m3, mut m4) = (0.0, 0.0, 0.0);
    for x in r {
        let d = x - mean;
        m2 += d * d;
        m3 += d * d * d;
        m4 += d * d * d * d;
    }
    let (m2, m3, m4) = (m2 / n, m3 / n, m4 / n);
    let sd = m2.sqrt();
    let skew = if sd > 0.0 { m3 / sd.powi(3) } else { 0.0 };
    let kurt = if sd > 0.0 { m4 / (m2 * m2) } else { 3.0 };
    (mean, sd, skew, kurt)
}

pub fn from_returns(r: &[f64], closes: &[f64]) -> Metrics {
    let mut m = Metrics { days: r.len(), ..Default::default() };
    m.round_trips = closes.len();
    if !closes.is_empty() {
        let wins: f64 = closes.iter().filter(|x| **x > 0.0).sum();
        let losses: f64 = -closes.iter().filter(|x| **x < 0.0).sum::<f64>();
        m.hit_rate = closes.iter().filter(|x| **x > 0.0).count() as f64 / closes.len() as f64;
        m.profit_factor = if losses > 0.0 { wins / losses } else { f64::INFINITY };
    }
    if r.len() < 2 {
        return m;
    }
    let (mean, sd, skew, kurt) = moments(r);
    m.years = r.len() as f64 / PERIODS_PER_YEAR;
    let mut eq = 1.0f64;
    let mut peak = 1.0f64;
    for x in r {
        eq *= 1.0 + x;
        peak = peak.max(eq);
        m.max_drawdown = m.max_drawdown.max(1.0 - eq / peak);
    }
    m.total_return = eq - 1.0;
    m.cagr = if eq > 0.0 { eq.powf(1.0 / m.years) - 1.0 } else { -1.0 };
    m.sr_daily = if sd > 0.0 { mean / sd } else { 0.0 };
    m.sharpe = m.sr_daily * PERIODS_PER_YEAR.sqrt();
    let downside = (r.iter().map(|x| x.min(0.0).powi(2)).sum::<f64>() / r.len() as f64).sqrt();
    m.sortino = if downside > 0.0 { mean / downside * PERIODS_PER_YEAR.sqrt() } else { 0.0 };
    m.skew = skew;
    m.kurtosis = kurt;
    m.psr0 = psr(m.sr_daily, 0.0, r.len(), skew, kurt);
    m
}

pub fn from_result(r: &SimResult) -> Metrics {
    from_returns(&daily_returns(r.starting_equity, &r.days), &r.closes)
}

/// Probabilistic Sharpe ratio. `sr`, `benchmark` are per-period (not annualised).
pub fn psr(sr: f64, benchmark: f64, n: usize, skew: f64, kurtosis: f64) -> f64 {
    if n < 2 {
        return 0.0;
    }
    let denom = 1.0 - skew * sr + (kurtosis - 1.0) / 4.0 * sr * sr;
    if denom <= 0.0 {
        return 0.0;
    }
    norm_cdf((sr - benchmark) * ((n - 1) as f64).sqrt() / denom.sqrt())
}

/// Expected maximum per-period Sharpe among `trials` skill-less strategies whose
/// Sharpe estimates have variance `var_trials` (False Strategy Theorem).
pub fn expected_max_sharpe(trials: usize, var_trials: f64) -> f64 {
    if trials < 2 || var_trials <= 0.0 {
        return 0.0;
    }
    let n = trials as f64;
    var_trials.sqrt() * ((1.0 - EULER_GAMMA) * norm_inv(1.0 - 1.0 / n) + EULER_GAMMA * norm_inv(1.0 - 1.0 / (n * std::f64::consts::E)))
}

/// Deflated Sharpe ratio: PSR against the expected best-of-N skill-less Sharpe.
pub fn dsr(m: &Metrics, trials: usize, var_trials: f64) -> f64 {
    psr(m.sr_daily, expected_max_sharpe(trials, var_trials), m.days, m.skew, m.kurtosis)
}

/// Standard normal CDF via `erfc` (Numerical Recipes 3rd ed. Chebyshev fit,
/// fractional error < 1.2e-7: ample for gate decisions; verified in tests).
pub fn norm_cdf(x: f64) -> f64 {
    0.5 * erfc(-x / std::f64::consts::SQRT_2)
}

// Coefficients kept exactly as published so they can be checked against the source.
#[allow(clippy::excessive_precision)]
fn erfc(x: f64) -> f64 {
    let z = x.abs();
    let t = 2.0 / (2.0 + z);
    let ty = 4.0 * t - 2.0;
    const COF: [f64; 28] = [
        -1.3026537197817094, 6.4196979235649026e-1, 1.9476473204185836e-2, -9.561514786808631e-3,
        -9.46595344482036e-4, 3.66839497852761e-4, 4.2523324806907e-5, -2.0278578112534e-5,
        -1.624290004647e-6, 1.303655835580e-6, 1.5626441722e-8, -8.5238095915e-8, 6.529054439e-9,
        5.059343495e-9, -9.91364156e-10, -2.27365122e-10, 9.6467911e-11, 2.394038e-12, -6.886027e-12,
        8.94487e-13, 3.13092e-13, -1.12708e-13, 3.81e-16, 7.106e-15, -1.523e-15, -9.4e-17, 1.21e-16, -2.8e-17,
    ];
    let (mut d, mut dd) = (0.0f64, 0.0f64);
    for c in COF.iter().skip(1).rev() {
        let tmp = d;
        d = ty * d - dd + c;
        dd = tmp;
    }
    let r = t * (-z * z + 0.5 * (COF[0] + ty * d) - dd).exp();
    if x >= 0.0 { r } else { 2.0 - r }
}

/// Inverse standard normal CDF (Acklam's algorithm, rel. error < 1.15e-9).
#[allow(clippy::excessive_precision)]
pub fn norm_inv(p: f64) -> f64 {
    assert!(p > 0.0 && p < 1.0, "p must be in (0,1)");
    const A: [f64; 6] = [-3.969683028665376e1, 2.209460984245205e2, -2.759285104469687e2, 1.383577518672690e2, -3.066479806614716e1, 2.506628277459239];
    const B: [f64; 5] = [-5.447609879822406e1, 1.615858368580409e2, -1.556989798598866e2, 6.680131188771972e1, -1.328068155288572e1];
    const C: [f64; 6] = [-7.784894002430293e-3, -3.223964580411365e-1, -2.400758277161838, -2.549732539343734, 4.374664141464968, 2.938163982698783];
    const D: [f64; 4] = [7.784695709041462e-3, 3.224671290700398e-1, 2.445134137142996, 3.754408661907416];
    let pl = 0.02425;
    if p < pl {
        let q = (-2.0 * p.ln()).sqrt();
        (((((C[0] * q + C[1]) * q + C[2]) * q + C[3]) * q + C[4]) * q + C[5]) / ((((D[0] * q + D[1]) * q + D[2]) * q + D[3]) * q + 1.0)
    } else if p <= 1.0 - pl {
        let q = p - 0.5;
        let r = q * q;
        (((((A[0] * r + A[1]) * r + A[2]) * r + A[3]) * r + A[4]) * r + A[5]) * q
            / (((((B[0] * r + B[1]) * r + B[2]) * r + B[3]) * r + B[4]) * r + 1.0)
    } else {
        -norm_inv(1.0 - p)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normal_functions() {
        assert!((norm_cdf(0.0) - 0.5).abs() < 1e-7);
        assert!((norm_cdf(1.96) - 0.975_002_1).abs() < 1e-6);
        assert!((norm_cdf(-3.0) - 0.001_349_898).abs() < 1e-7);
        for p in [0.001, 0.025, 0.5, 0.9, 0.999] {
            assert!((norm_cdf(norm_inv(p)) - p).abs() < 1e-7, "p={p}");
        }
    }

    #[test]
    fn psr_matches_closed_form_for_normal_returns() {
        // Normal returns: skew 0, kurtosis 3 → denom = 1 + SR²/2.
        let sr = 0.1;
        let n = 501;
        let expect = norm_cdf(sr * 500f64.sqrt() / (1.0 + sr * sr / 2.0).sqrt());
        assert!((psr(sr, 0.0, n, 0.0, 3.0) - expect).abs() < 1e-12);
    }

    #[test]
    fn deflation_raises_the_bar_with_more_trials() {
        let a = expected_max_sharpe(10, 0.01);
        let b = expected_max_sharpe(1000, 0.01);
        assert!(a > 0.0 && b > a);
        // Known value: N=100, var=1 → ≈ 2.5306 (Bailey & López de Prado).
        assert!((expected_max_sharpe(100, 1.0) - 2.5306).abs() < 2e-3, "{}", expected_max_sharpe(100, 1.0));
    }

    #[test]
    fn metrics_from_returns() {
        let r = [0.01, -0.005, 0.02, -0.01, 0.0];
        let m = from_returns(&r, &[5.0, -2.0, 3.0]);
        assert!((m.total_return - (1.01 * 0.995 * 1.02 * 0.99 - 1.0)).abs() < 1e-12);
        assert!((m.max_drawdown - 0.01).abs() < 1e-12);
        assert!((m.hit_rate - 2.0 / 3.0).abs() < 1e-12);
        assert!((m.profit_factor - 4.0).abs() < 1e-12);
    }
}
