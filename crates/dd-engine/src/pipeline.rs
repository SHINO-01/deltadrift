//! Four-stage, core-pinned pipeline:
//!
//! ```text
//! core0 control ─┐
//! core1 feed ──[SPSC ring: MarketEvent]──▶ core2 strategy ──[SPSC ring: Signal]──▶ core3 risk + router ──▶ broker
//! ```
//!
//! Shutdown is cooperative and lossless: the feed stops at the deadline and
//! drops its producer; each downstream stage drains its ring after observing
//! the close flag, then drops its own producer, cascading to the end.

use std::time::Duration;

use dd_core::wait::{BusySpin, SpinThenYield, Wait};
use dd_core::{MarketEvent, Signal, TimeInForce, affinity, clock, ring};
use dd_exec::{Broker, Exec, OrderTemplates, PaperBroker};
use dd_feed::live::FeedStats;
use dd_risk::{Decision, RiskEngine, RiskStats};
use dd_strategy::{Strategy, ZScoreReversion};
use hdrhistogram::Histogram;

use crate::config::{Config, WaitMode};
use crate::feed::{self, FeedSource};

/// Ring capacities. 64 KiB slots * 64 B = 4 MiB each: comfortably absorbs
/// bursts while staying well inside the 8 GB budget (NFR-2).
const EVENT_RING: usize = 1 << 16;
const SIGNAL_RING: usize = 1 << 16;
/// Max items handled per cursor publish.
const BATCH: usize = 256;

#[derive(Debug)]
pub struct StageReport {
    pub pinned: bool,
    pub items: u64,
}

#[derive(Debug)]
pub struct RunReport {
    pub wall: Duration,
    pub feed: StageReport,
    /// Live-feed connection/decoding stats (None for the synthetic feed).
    pub live: Option<FeedStats>,
    pub strategy: StageReport,
    pub risk: StageReport,
    /// Event receive → risk decision, every event.
    pub decision_latency: Histogram<u64>,
    /// Event receive → order handed to broker, orders only.
    pub order_latency: Histogram<u64>,
    pub risk_stats: RiskStats,
    pub fills: u64,
    pub cancels: u64,
    pub equity: f64,
    pub starting_equity: f64,
    pub realized: f64,
    pub fees: f64,
    pub max_drawdown: f64,
    pub broker_errors: u64,
}

pub fn run(cfg: &Config, src: FeedSource) -> anyhow::Result<RunReport> {
    match cfg.engine.wait {
        WaitMode::Spin => run_with::<BusySpin>(cfg, src),
        WaitMode::Yield => run_with::<SpinThenYield>(cfg, src),
    }
}

fn pin(cfg: &Config, stage: usize) -> bool {
    cfg.engine.pin && affinity::pin_current(cfg.engine.cores()[stage])
}

fn run_with<W: Wait + Default>(cfg: &Config, src: FeedSource) -> anyhow::Result<RunReport> {
    clock::init();
    let risk = RiskEngine::new(cfg.risk.clone()).map_err(anyhow::Error::msg)?;
    let strategy = ZScoreReversion::new(cfg.strategy.clone());
    let broker = PaperBroker::new(cfg.paper.clone(), clock::now_ns);
    let templates = OrderTemplates::new(0, TimeInForce::Ioc, session_id());
    // 1 ns .. 10 s at 3 significant figures; allocated now, never on the hot path.
    let decision_latency = Histogram::<u64>::new_with_bounds(1, 10_000_000_000, 3)?;
    let order_latency = decision_latency.clone();

    let (ev_tx, ev_rx) = ring::<MarketEvent, EVENT_RING>();
    let (sig_tx, sig_rx) = ring::<Signal, SIGNAL_RING>();

    let rate = cfg.engine.rate_per_sec;
    let duration_ns = (cfg.engine.duration_secs * 1e9) as u64;
    affinity::init();
    let start_ns = clock::now_ns();
    let deadline = start_ns + duration_ns;

    std::thread::scope(|s| {
        let feed_h = std::thread::Builder::new().name("dd-feed".into()).spawn_scoped(s, move || {
            let mut ev_tx = ev_tx;
            let pinned = pin(cfg, 1);
            let (n, live) = feed::run::<W, EVENT_RING>(src, &mut ev_tx, deadline, rate);
            drop(ev_tx);
            (StageReport { pinned, items: n }, live)
        })?;

        let strat_h = std::thread::Builder::new().name("dd-strategy".into()).spawn_scoped(s, move || {
            let (mut ev_rx, mut sig_tx, mut strategy) = (ev_rx, sig_tx, strategy);
            let pinned = pin(cfg, 2);
            let (mut idle, mut push_wait) = (W::default(), W::default());
            let mut n = 0u64;
            loop {
                let closed = ev_rx.is_closed();
                let got = ev_rx.consume(BATCH, |ev| {
                    if let Some(sig) = strategy.on_event(&ev) {
                        sig_tx.push_with(sig, &mut push_wait);
                    }
                });
                n += got as u64;
                if got == 0 {
                    // `closed` was read *before* the empty consume, so nothing can be left.
                    if closed {
                        break;
                    }
                    idle.idle();
                } else {
                    idle.reset();
                }
            }
            drop(sig_tx);
            StageReport { pinned, items: n }
        })?;

        let risk_h = std::thread::Builder::new().name("dd-risk".into()).spawn_scoped(s, move || {
            let (mut sig_rx, mut risk, mut broker, mut templates) = (sig_rx, risk, broker, templates);
            let (mut decision_latency, mut order_latency) = (decision_latency, order_latency);
            let pinned = pin(cfg, 3);
            let mut idle = W::default();
            let (mut n, mut fills, mut cancels, mut broker_errors) = (0u64, 0u64, 0u64, 0u64);
            loop {
                let closed = sig_rx.is_closed();
                let got = sig_rx.consume(BATCH, |sig| {
                    broker.on_quote(sig.instrument, sig.bid, sig.ask);
                    let now = clock::now_ns();
                    if let Decision::Order { side, qty, limit } = risk.on_signal(&sig, now)
                        && let Some(order) = templates.build(sig.instrument, sig.origin_ns, side, qty, limit)
                    {
                        if broker.submit(&order).is_err() {
                            broker_errors += 1;
                            risk.on_cancel(sig.instrument, side, qty);
                        }
                        order_latency.saturating_record(clock::now_ns() - sig.origin_ns);
                    }
                    broker.poll(|e| match e {
                        Exec::Fill(f) => {
                            fills += 1;
                            risk.on_fill(&f);
                        }
                        Exec::Cancelled { instrument, side, unfilled, .. } => {
                            cancels += 1;
                            risk.on_cancel(instrument, side, unfilled);
                        }
                    });
                    decision_latency.saturating_record(now - sig.origin_ns);
                });
                n += got as u64;
                if got == 0 {
                    if closed {
                        break;
                    }
                    idle.idle();
                } else {
                    idle.reset();
                }
            }
            (StageReport { pinned, items: n }, fills, cancels, broker_errors, risk, decision_latency, order_latency)
        })?;

        // Pin the control thread only now: children inherit their parent's mask.
        pin(cfg, 0);
        let (feed_r, live) = feed_h.join().expect("feed stage panicked");
        let strat_r = strat_h.join().expect("strategy stage panicked");
        let (risk_r, fills, cancels, broker_errors, risk, decision_latency, order_latency) = risk_h.join().expect("risk stage panicked");
        let p = &risk.portfolio;
        Ok(RunReport {
            wall: Duration::from_nanos(clock::now_ns() - start_ns),
            feed: feed_r,
            live,
            strategy: strat_r,
            risk: risk_r,
            decision_latency,
            order_latency,
            risk_stats: risk.stats(),
            fills,
            cancels,
            equity: p.equity(),
            starting_equity: p.starting_equity(),
            realized: p.realized(),
            fees: p.fees(),
            max_drawdown: risk.max_drawdown(),
            broker_errors,
        })
    })
}

fn session_id() -> u16 {
    let t = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default();
    (t.as_secs() & 0xFFFF) as u16
}
