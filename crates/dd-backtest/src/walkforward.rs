//! Rolling walk-forward optimisation: for each window, every parameter set
//! is simulated on the *train* span (one data pass for all), the best by
//! daily Sharpe is chosen, and only that set is run on the following unseen
//! *test* span. Test spans are stitched into one out-of-sample record, which
//! is what gate G1 judges — never in-sample results.

use dd_core::MarketEvent;
use dd_risk::RiskConfig;
use dd_strategy::{ZScoreParams, ZScoreReversion};
use serde::Deserialize;

use crate::metrics::{self, Metrics};
use crate::runner;
use crate::sim::{CostModel, SimResult, Simulation};

const NS_PER_DAY: i64 = 86_400_000_000_000;

/// Parameter grid for the baseline strategy (Cartesian product).
#[derive(Debug, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Grid {
    pub entry_z: Vec<f64>,
    pub exit_z: Vec<f64>,
    pub vol_halflife_ticks: Vec<f64>,
    pub surprise_halflife_ms: Vec<u64>,
}

impl Default for Grid {
    fn default() -> Self {
        Self {
            entry_z: vec![2.0, 3.0, 4.0],
            exit_z: vec![0.0, 0.5],
            vol_halflife_ticks: vec![200.0, 1000.0],
            surprise_halflife_ms: vec![1_000, 30_000],
        }
    }
}

impl Grid {
    pub fn combos(&self, base: &ZScoreParams) -> Vec<ZScoreParams> {
        let mut out = Vec::new();
        for &e in &self.entry_z {
            for &x in &self.exit_z {
                if x >= e {
                    continue;
                }
                for &v in &self.vol_halflife_ticks {
                    for &s in &self.surprise_halflife_ms {
                        out.push(ZScoreParams {
                            entry_z: e,
                            exit_z: x,
                            vol_halflife_ticks: v,
                            surprise_halflife_ms: s,
                            ..base.clone()
                        });
                    }
                }
            }
        }
        out
    }
}

pub fn label(p: &ZScoreParams) -> String {
    format!("entry={} exit={} vol_hl={} surprise_hl={}ms", p.entry_z, p.exit_z, p.vol_halflife_ticks, p.surprise_halflife_ms)
}

#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct WfConfig {
    pub train_days: u32,
    pub test_days: u32,
}

impl Default for WfConfig {
    fn default() -> Self {
        Self { train_days: 90, test_days: 30 }
    }
}

#[derive(Debug, Clone)]
pub struct Window {
    pub train: (i64, i64),
    pub test: (i64, i64),
    pub chosen: ZScoreParams,
    pub train_metrics: Metrics,
    pub test_result: SimResult,
}

#[derive(Debug, Clone)]
pub struct WfResult {
    pub windows: Vec<Window>,
    pub oos_returns: Vec<f64>,
    pub oos_closes: Vec<f64>,
    pub oos: Metrics,
    /// Parameter sets tried per window (N for deflation).
    pub trials: usize,
    /// Mean across windows of the variance of daily Sharpe across trials.
    pub var_trials: f64,
    pub dsr: f64,
    pub events: u64,
}

/// Minimum closed trades for a trial's Sharpe to count toward the trial variance.
pub const MIN_TRADES_FOR_VARIANCE: usize = 10;

/// Variance of daily Sharpe across trials, excluding degenerate trials (too few
/// trades, or near-constant returns such as pure financing drift) whose Sharpe
/// estimates explode and would distort the deflated-Sharpe benchmark.
pub fn trial_sharpe_variance(ms: &[Metrics]) -> f64 {
    let srs: Vec<f64> = ms
        .iter()
        .filter(|m| m.round_trips >= MIN_TRADES_FOR_VARIANCE && m.sr_daily.is_finite() && m.sr_daily.abs() < 5.0)
        .map(|m| m.sr_daily)
        .collect();
    if srs.len() < 2 {
        return 0.0;
    }
    let mean = srs.iter().sum::<f64>() / srs.len() as f64;
    srs.iter().map(|s| (s - mean).powi(2)).sum::<f64>() / (srs.len() - 1) as f64
}

/// `source(from_ns, to_ns)` must replay that span.
#[allow(clippy::too_many_arguments)]
pub fn walk_forward<I, E, F>(
    source: F,
    from_ns: i64,
    to_ns: i64,
    base: &ZScoreParams,
    grid: &Grid,
    risk: &RiskConfig,
    cost: &CostModel,
    wf: WfConfig,
    threads: usize,
    mut progress: impl FnMut(&Window),
) -> Result<WfResult, String>
where
    I: Iterator<Item = Result<MarketEvent, E>>,
    E: std::fmt::Display,
    F: Fn(i64, i64) -> I,
{
    let combos = grid.combos(base);
    if combos.is_empty() {
        return Err("parameter grid is empty".into());
    }
    let (train, test) = (i64::from(wf.train_days) * NS_PER_DAY, i64::from(wf.test_days) * NS_PER_DAY);
    let mut windows = Vec::new();
    let mut var_sum = 0.0;
    let mut events = 0;
    let mut t = from_ns;
    while t + train + test <= to_ns {
        let (tr, te) = ((t, t + train), (t + train, t + train + test));
        let mut sims: Vec<_> = combos
            .iter()
            .map(|p| Simulation::new(label(p), ZScoreReversion::new(p.clone()), risk.clone(), cost.clone()))
            .collect::<Result<_, _>>()?;
        events += runner::run(source(tr.0, tr.1), &mut sims, threads).map_err(|e| e.to_string())?;
        let results: Vec<SimResult> = sims.into_iter().map(|s| s.finish(tr.1)).collect();
        let ms: Vec<Metrics> = results.iter().map(metrics::from_result).collect();
        var_sum += trial_sharpe_variance(&ms);
        // Deterministic argmax (first wins ties).
        let best = (0..ms.len()).fold(0, |b, i| if ms[i].sr_daily > ms[b].sr_daily { i } else { b });

        let p = combos[best].clone();
        let mut sim = vec![Simulation::new(label(&p), ZScoreReversion::new(p.clone()), risk.clone(), cost.clone())?];
        events += runner::run(source(te.0, te.1), &mut sim, 1).map_err(|e| e.to_string())?;
        let test_result = sim.pop().expect("one sim").finish(te.1);
        let w = Window { train: tr, test: te, chosen: p, train_metrics: ms[best].clone(), test_result };
        progress(&w);
        windows.push(w);
        t += test;
    }
    if windows.is_empty() {
        return Err(format!("range too short for one window of {} + {} days", wf.train_days, wf.test_days));
    }
    let mut oos_returns = Vec::new();
    let mut oos_closes = Vec::new();
    for w in &windows {
        oos_returns.extend(metrics::daily_returns(w.test_result.starting_equity, &w.test_result.days));
        oos_closes.extend_from_slice(&w.test_result.closes);
    }
    let oos = metrics::from_returns(&oos_returns, &oos_closes);
    let var_trials = var_sum / windows.len() as f64;
    let dsr = metrics::dsr(&oos, combos.len(), var_trials);
    Ok(WfResult { windows, oos_returns, oos_closes, oos, trials: combos.len(), var_trials, dsr, events })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn m(sr: f64, trips: usize) -> Metrics {
        Metrics { sr_daily: sr, round_trips: trips, ..Default::default() }
    }

    #[test]
    fn degenerate_trials_excluded_from_variance() {
        let v = trial_sharpe_variance(&[m(0.1, 50), m(-0.1, 50), m(-400.0, 50), m(3.0, 2)]);
        assert!((v - 0.02).abs() < 1e-12, "{v}");
        assert_eq!(trial_sharpe_variance(&[m(0.1, 50)]), 0.0);
    }

    #[test]
    fn grid_skips_exit_not_below_entry() {
        let g = Grid { entry_z: vec![1.0, 2.0], exit_z: vec![1.0], vol_halflife_ticks: vec![10.0], surprise_halflife_ms: vec![5] };
        assert_eq!(g.combos(&ZScoreParams::default()).len(), 1);
    }
}
