//! `deltadrift data fetch` and `deltadrift backtest`.

use anyhow::{Context, Result, bail};
use dd_backtest::metrics::{self, Metrics};
use dd_backtest::walkforward::{self, WfResult};
use dd_core::calendar::days_from_civil;
use dd_hist::InstrumentSpec;
use dd_hist::source::HistSource;
use serde_json::json;

use crate::config::Config;

const NS_PER_DAY: i64 = 86_400_000_000_000;

/// "YYYY-MM-DD" → Unix ns at 00:00 UTC.
pub fn parse_date(s: &str) -> Result<i64> {
    let p: Vec<&str> = s.split('-').collect();
    let [y, m, d] = p.as_slice() else { bail!("date `{s}` must be YYYY-MM-DD") };
    let (y, m, d): (i64, u32, u32) = (y.parse()?, m.parse()?, d.parse()?);
    if !(1..=12).contains(&m) || !(1..=31).contains(&d) {
        bail!("date `{s}` out of range");
    }
    Ok(days_from_civil(y, m, d) * NS_PER_DAY)
}

fn threads(n: usize) -> usize {
    if n == 0 { std::thread::available_parallelism().map_or(1, |n| n.get()) } else { n }
}

pub fn fetch(cfg: &Config, symbols: &[String], from: i64, to: i64, concurrency: usize) -> Result<()> {
    let root = &cfg.backtest.data_root;
    for s in symbols {
        let sym = s.to_ascii_uppercase();
        let (h0, h1) = (from / 3_600_000_000_000, to / 3_600_000_000_000);
        eprintln!("fetching {sym}: {} hour files into {}", h1 - h0, root.display());
        let st = dd_hist::dukascopy::fetch(root, &sym, h0, h1, concurrency, |st| {
            eprintln!("  {sym}: {}/{} hours ({} cached, {:.1} MiB new)", st.requested, h1 - h0, st.cached, st.bytes as f64 / 1048576.0);
        })
        .with_context(|| format!("fetching {sym}"))?;
        eprintln!(
            "  {sym}: done — {} downloaded ({} empty), {} already cached, {:.1} MiB",
            st.downloaded,
            st.empty,
            st.cached,
            st.bytes as f64 / 1048576.0
        );
    }
    Ok(())
}

pub struct Gate {
    pub name: &'static str,
    pub pass: bool,
    pub detail: String,
}

pub fn g1_gates(cfg: &Config, m: &Metrics, dsr: f64) -> Vec<Gate> {
    let b = &cfg.backtest;
    vec![
        Gate { name: "history ≥ min years", pass: m.years >= b.g1_min_years, detail: format!("{:.2} y (need {:.1})", m.years, b.g1_min_years) },
        Gate { name: "round trips ≥ min", pass: m.round_trips >= b.g1_min_round_trips, detail: format!("{} (need {})", m.round_trips, b.g1_min_round_trips) },
        Gate { name: "deflated Sharpe > min", pass: dsr > b.g1_min_dsr, detail: format!("{dsr:.3} (need > {})", b.g1_min_dsr) },
        Gate { name: "max drawdown < limit", pass: m.max_drawdown < b.g1_max_drawdown, detail: format!("{:.2}% (limit {:.0}%)", m.max_drawdown * 100.0, b.g1_max_drawdown * 100.0) },
        Gate { name: "positive after costs", pass: m.total_return > 0.0, detail: format!("{:+.2}%", m.total_return * 100.0) },
        Gate { name: "win rate > min (owner)", pass: m.hit_rate > b.g1_min_win_rate, detail: format!("{:.1}% net of costs (need > {:.0}%)", m.hit_rate * 100.0, b.g1_min_win_rate * 100.0) },
        Gate { name: "profit factor ≥ min", pass: m.profit_factor >= b.g1_min_profit_factor, detail: format!("{:.2} (need ≥ {:.2})", m.profit_factor, b.g1_min_profit_factor) },
    ]
}

/// Synthetic replay for pipeline validation: the demo generator at a fixed
/// 1-second cadence, skipping FX weekends. Never evidence for gate G1.
fn synthetic_source(cfg: &Config, from: i64, to: i64) -> impl Iterator<Item = Result<dd_core::MarketEvent, std::convert::Infallible>> + use<> {
    let mut feed = dd_feed::SyntheticFeed::new(dd_feed::SyntheticConfig { seed: cfg.feed.seed ^ from as u64, ..cfg.feed.clone() });
    let step = 1_000_000_000i64;
    let mut t = from;
    std::iter::from_fn(move || {
        while t < to {
            let cur = t;
            t += step;
            // Skip Sat and Sun before 22:00 UTC (FX closed).
            let day = cur.div_euclid(NS_PER_DAY);
            let wd = dd_core::calendar::weekday(day);
            let hour = cur.rem_euclid(NS_PER_DAY) / 3_600_000_000_000;
            if wd == 5 || (wd == 6 && hour < 22) || (wd == 4 && hour >= 22) {
                continue;
            }
            return Some(Ok(feed.next_event(cur as u64)));
        }
        None
    })
}

pub fn backtest(cfg: &Config, from: i64, to: i64, as_json: bool, synthetic: bool) -> Result<()> {
    if synthetic {
        eprintln!("*** SYNTHETIC DATA — validates the pipeline only; NOT evidence for gate G1 ***");
        let r = walkforward::walk_forward(
            |f, t| synthetic_source(cfg, f, t),
            from,
            to,
            &cfg.strategy,
            &cfg.backtest.grid,
            &cfg.risk,
            &cfg.backtest.costs,
            cfg.backtest.walk_forward,
            threads(cfg.backtest.threads),
            |w| eprintln!("window test {} → {}: OOS {:+.3}%", fmt_day(w.test.0), fmt_day(w.test.1), metrics::from_result(&w.test_result).total_return * 100.0),
        )
        .map_err(anyhow::Error::msg)?;
        return report(cfg, &r, from, to, as_json, true);
    }
    let b = &cfg.backtest;
    for s in &b.symbols {
        if !s.to_ascii_uppercase().ends_with("USD") {
            bail!("{s}: only USD-quoted symbols are supported until FX conversion to account currency lands (live blocker L15)");
        }
    }
    let specs: Vec<InstrumentSpec> = b.symbols.iter().map(|s| InstrumentSpec::dukascopy(s)).collect();
    let root = b.data_root.clone();
    let source = |f: i64, t: i64| HistSource::dukascopy(root.clone(), &specs, f, t);
    let started = std::time::Instant::now();
    let r: WfResult = walkforward::walk_forward(
        source,
        from,
        to,
        &cfg.strategy,
        &b.grid,
        &cfg.risk,
        &b.costs,
        b.walk_forward,
        threads(b.threads),
        |w| {
            let m = metrics::from_result(&w.test_result);
            eprintln!(
                "window test {} → {}: OOS {:+.3}%  sharpe {:+.2}  trips {:>4}  | train sharpe {:+.2} [{}]",
                fmt_day(w.test.0),
                fmt_day(w.test.1),
                m.total_return * 100.0,
                m.sharpe,
                m.round_trips,
                w.train_metrics.sharpe,
                walkforward::label(&w.chosen)
            );
        },
    )
    .map_err(anyhow::Error::msg)?;
    eprintln!("({:.1}s)", started.elapsed().as_secs_f64());
    report(cfg, &r, from, to, as_json, false)
}

fn report(cfg: &Config, r: &WfResult, from: i64, to: i64, as_json: bool, synthetic: bool) -> Result<()> {
    let b = &cfg.backtest;
    let mut gates = g1_gates(cfg, &r.oos, r.dsr);
    if synthetic {
        gates.push(Gate { name: "real market data", pass: false, detail: "synthetic run — never eligible".into() });
    }
    let costs = r.windows.iter().fold((0.0, 0.0, 0.0), |a, w| {
        let c = w.test_result.costs;
        (a.0 + c.spread, a.1 + c.commission, a.2 + c.financing)
    });
    let base = cfg.risk.starting_equity;
    if as_json {
        let j = json!({
            "symbols": b.symbols, "from": fmt_day(from), "to": fmt_day(to),
            "synthetic": synthetic,
            "windows": r.windows.len(), "trials_per_window": r.trials, "events": r.events,
            "oos": {
                "years": r.oos.years, "total_return": r.oos.total_return, "cagr": r.oos.cagr, "sharpe": r.oos.sharpe,
                "sortino": r.oos.sortino, "max_drawdown": r.oos.max_drawdown, "round_trips": r.oos.round_trips,
                "hit_rate": r.oos.hit_rate, "profit_factor": r.oos.profit_factor, "psr0": r.oos.psr0, "dsr": r.dsr,
                "skew": r.oos.skew, "kurtosis": r.oos.kurtosis,
            },
            "costs_per_window_base_normalised": {
                "spread": costs.0 / base * 100.0, "commission": costs.1 / base * 100.0, "financing": costs.2 / base * 100.0,
            },
            "g1": gates.iter().map(|g| json!({"check": g.name, "pass": g.pass, "detail": g.detail})).collect::<Vec<_>>(),
            "g1_pass": gates.iter().all(|g| g.pass),
        });
        println!("{}", serde_json::to_string_pretty(&j)?);
        return Ok(());
    }
    let m = &r.oos;
    let what = if synthetic { "SYNTHETIC".to_string() } else { b.symbols.join(",") };
    println!("\nWalk-forward OOS — {what} {} → {} ({} windows × {} param sets, {} events)", fmt_day(from), fmt_day(to), r.windows.len(), r.trials, r.events);
    println!("  return   {:+.2}% total, CAGR {:+.2}%, {:.2} years", m.total_return * 100.0, m.cagr * 100.0, m.years);
    println!("  risk     Sharpe {:+.2}  Sortino {:+.2}  max DD {:.2}%  skew {:+.2}  kurt {:.1}", m.sharpe, m.sortino, m.max_drawdown * 100.0, m.skew, m.kurtosis);
    println!("  trades   {} round trips, hit {:.1}%, profit factor {:.2}", m.round_trips, m.hit_rate * 100.0, m.profit_factor);
    println!("  costs    spread {:.3}  commission {:.3}  financing {:.3}  (sum over windows, $ per $100 base)", costs.0 / base * 100.0, costs.1 / base * 100.0, costs.2 / base * 100.0);
    println!("  stats    PSR(SR>0) {:.3}   DSR {:.3} (N={}, var={:.2e})", m.psr0, r.dsr, r.trials, r.var_trials);
    println!("\nGate G1:");
    for g in &gates {
        println!("  [{}] {:<22} {}", if g.pass { "PASS" } else { "FAIL" }, g.name, g.detail);
    }
    println!("  => {}", if gates.iter().all(|g| g.pass) { "G1 PASSED" } else { "G1 NOT PASSED — not eligible for paper/live" });
    Ok(())
}

fn fmt_day(ns: i64) -> String {
    let (y, m, d) = dd_core::calendar::civil_from_days(ns.div_euclid(NS_PER_DAY));
    format!("{y}-{m:02}-{d:02}")
}
