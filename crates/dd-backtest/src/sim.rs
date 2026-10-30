use std::collections::VecDeque;

use dd_core::calendar::{next_rollover, swap_multiplier};
use dd_core::{MAX_INSTRUMENTS, MarketEvent, Order, Px, TimeInForce};
use dd_exec::OrderTemplates;
use dd_risk::{Decision, RiskConfig, RiskEngine, RiskStats};
use dd_strategy::Strategy;
use serde::Deserialize;

/// Trading costs. Defaults are deliberately conservative placeholders until
/// replaced by the broker's real values (cTrader `ProtoOASymbol` swap/commission).
#[derive(Debug, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct CostModel {
    /// Decision → fill delay. Fills at the first quote at or after this.
    pub latency_ms: u64,
    /// Adverse slippage beyond the quoted touch, basis points.
    pub slippage_bps: f64,
    /// Commission per side, in account currency per 1,000,000 notional
    /// (e.g. 3.50 per 100k lot per side = 35.0).
    pub commission_per_million: f64,
    /// Annual financing cost on long / short notional (fraction; negative = credit).
    pub swap_long_annual: f64,
    pub swap_short_annual: f64,
}

impl Default for CostModel {
    fn default() -> Self {
        Self { latency_ms: 50, slippage_bps: 0.2, commission_per_million: 35.0, swap_long_annual: 0.02, swap_short_annual: 0.02 }
    }
}

/// Simulated execution venue.
#[derive(Debug)]
pub struct SimBroker {
    cost: CostModel,
    latency_ns: u64,
    /// Per-instrument FIFO of (eligible-from ns, order).
    pending: Box<[VecDeque<(u64, Order)>]>,
}

/// What happened to an order.
#[derive(Debug, Clone, Copy)]
pub enum SimExec {
    /// (order, fill price, fee, mid at fill)
    Fill(Order, Px, f64, f64),
    Cancel(Order),
}

impl SimBroker {
    pub fn new(cost: CostModel) -> Self {
        Self {
            latency_ns: cost.latency_ms * 1_000_000,
            cost,
            pending: (0..MAX_INSTRUMENTS).map(|_| VecDeque::new()).collect(),
        }
    }

    pub fn submit(&mut self, o: Order, now_ns: u64) {
        self.pending[o.instrument.idx()].push_back((now_ns + self.latency_ns, o));
    }

    /// Fills/cancels orders for `ev.instrument` that have become eligible.
    #[inline]
    pub fn on_event<F: FnMut(SimExec)>(&mut self, ev: &MarketEvent, mut out: F) {
        let q = &mut self.pending[ev.instrument.idx()];
        while let Some(&(at, o)) = q.front() {
            if at > ev.recv_ns {
                break;
            }
            q.pop_front();
            let slip = self.cost.slippage_bps * 1e-4;
            let (px, ok) = match o.side {
                dd_core::Side::Buy => {
                    let p = ev.ask.to_f64() * (1.0 + slip);
                    (p, p <= o.limit.to_f64())
                }
                dd_core::Side::Sell => {
                    let p = ev.bid.to_f64() * (1.0 - slip);
                    (p, p >= o.limit.to_f64())
                }
            };
            if ok {
                let fee = o.qty.to_f64() * px * self.cost.commission_per_million * 1e-6;
                out(SimExec::Fill(o, Px::from_f64(px), fee, ev.mid().to_f64()));
            } else {
                out(SimExec::Cancel(o)); // IOC: market moved through the limit during latency
            }
        }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct Costs {
    /// Paid vs. mid at fill: half-spread + slippage.
    pub spread: f64,
    pub commission: f64,
    pub financing: f64,
}

/// End-of-trading-day snapshot.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct DayPoint {
    pub rollover_ns: i64,
    pub equity: f64,
}

#[derive(Debug, Clone)]
pub struct SimResult {
    pub label: String,
    pub starting_equity: f64,
    pub final_equity: f64,
    pub days: Vec<DayPoint>,
    /// **Net** P&L of each fill that reduced/closed a position: realised P&L minus
    /// the pro-rata share of that position's commissions and financing. Wins and
    /// the win rate are computed from these (a "win" is profitable after costs).
    pub closes: Vec<f64>,
    pub fills: u64,
    pub cancels: u64,
    pub costs: Costs,
    pub risk: RiskStats,
    /// Tick-level peak-to-trough drawdown (fraction).
    pub intraday_max_dd: f64,
    pub events: u64,
}

/// One simulated account running `S` with the live risk engine.
pub struct Simulation<S: Strategy> {
    label: String,
    strategy: S,
    risk: RiskEngine,
    broker: SimBroker,
    templates: OrderTemplates,
    cost: CostModel,
    next_roll: i64,
    days: Vec<DayPoint>,
    closes: Vec<f64>,
    /// Commission + financing accumulated on each instrument's open position,
    /// not yet attributed to a close.
    open_cost: Box<[f64]>,
    fills: u64,
    cancels: u64,
    costs: Costs,
    events: u64,
}

impl<S: Strategy> std::fmt::Debug for Simulation<S> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Simulation").field("label", &self.label).field("events", &self.events).finish_non_exhaustive()
    }
}

impl<S: Strategy> Simulation<S> {
    pub fn new(label: impl Into<String>, strategy: S, risk: RiskConfig, cost: CostModel) -> Result<Self, String> {
        Ok(Self {
            label: label.into(),
            strategy,
            risk: RiskEngine::new(risk)?,
            broker: SimBroker::new(cost.clone()),
            templates: OrderTemplates::new(0, TimeInForce::Ioc, 0),
            cost,
            next_roll: i64::MIN,
            days: Vec::new(),
            closes: Vec::new(),
            open_cost: vec![0.0; MAX_INSTRUMENTS].into_boxed_slice(),
            fills: 0,
            cancels: 0,
            costs: Costs::default(),
            events: 0,
        })
    }

    /// Applies all rollovers up to `t`: financing on open positions, daily
    /// snapshot (trading days only), and the daily-drawdown re-base.
    fn roll_to(&mut self, t: i64) {
        if self.next_roll == i64::MIN {
            self.next_roll = next_rollover(t);
            return;
        }
        while t >= self.next_roll {
            let mult = swap_multiplier(self.next_roll);
            if mult > 0 {
                let mut charge = 0.0;
                for (id, p) in self.risk.portfolio.open_positions() {
                    let notional = (p.qty * p.mark).abs();
                    let rate = if p.qty > 0.0 { self.cost.swap_long_annual } else { self.cost.swap_short_annual };
                    let c = notional * rate / 365.0 * f64::from(mult);
                    self.open_cost[id.idx()] += c;
                    charge += c;
                }
                if charge != 0.0 {
                    self.risk.on_financing(charge);
                    self.costs.financing += charge;
                }
                self.days.push(DayPoint { rollover_ns: self.next_roll, equity: self.risk.portfolio.equity() });
            }
            self.risk.roll_day();
            self.next_roll = next_rollover(self.next_roll);
        }
    }

    #[inline]
    pub fn on_event(&mut self, ev: &MarketEvent) {
        self.events += 1;
        let t = ev.recv_ns as i64;
        self.roll_to(t);

        let (risk, costs, closes, open_cost, fills, cancels) =
            (&mut self.risk, &mut self.costs, &mut self.closes, &mut self.open_cost, &mut self.fills, &mut self.cancels);
        self.broker.on_event(ev, |x| match x {
            SimExec::Fill(o, px, fee, mid) => {
                *fills += 1;
                let i = o.instrument.idx();
                let qty_before = risk.portfolio.position(o.instrument).qty;
                let before = risk.portfolio.realized();
                risk.on_fill(&dd_core::Fill::new(o.client_id, ev.recv_ns, o.instrument, o.side, o.qty, px, Px::from_f64(fee)));
                let realized = risk.portfolio.realized() - before;
                open_cost[i] += fee;
                if realized != 0.0 {
                    // Attribute the closed fraction of the position's accumulated costs.
                    let frac = (o.qty.to_f64() / qty_before.abs()).min(1.0);
                    let attributed = open_cost[i] * frac;
                    open_cost[i] -= attributed;
                    closes.push(realized - attributed);
                }
                if risk.portfolio.position(o.instrument).qty == 0.0 {
                    open_cost[i] = 0.0;
                }
                costs.spread += (px.to_f64() - mid).abs() * o.qty.to_f64();
                costs.commission += fee;
            }
            SimExec::Cancel(o) => {
                *cancels += 1;
                risk.on_cancel(o.instrument, o.side, o.qty);
            }
        });

        if let Some(sig) = self.strategy.on_event(ev)
            && let Decision::Order { side, qty, limit } = self.risk.on_signal(&sig, ev.recv_ns)
            && let Some(o) = self.templates.build(sig.instrument, sig.origin_ns, side, qty, limit)
        {
            self.broker.submit(o, ev.recv_ns);
        }
    }

    pub fn finish(mut self, end_ns: i64) -> SimResult {
        self.roll_to(end_ns);
        let p = &self.risk.portfolio;
        let final_equity = p.equity();
        if self.days.last().is_none_or(|d| d.equity != final_equity) {
            self.days.push(DayPoint { rollover_ns: end_ns, equity: final_equity });
        }
        SimResult {
            label: self.label,
            starting_equity: p.starting_equity(),
            final_equity,
            days: self.days,
            closes: self.closes,
            fills: self.fills,
            cancels: self.cancels,
            costs: self.costs,
            risk: self.risk.stats(),
            intraday_max_dd: self.risk.max_drawdown(),
            events: self.events,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use dd_core::calendar::days_from_civil;
    use dd_core::{InstrumentId, Qty, Signal};

    const H: u64 = 3_600_000_000_000;

    /// Long view until `flat_from` (ns), flat afterwards.
    struct Script {
        flat_from: u64,
    }
    impl Strategy for Script {
        fn on_event(&mut self, ev: &MarketEvent) -> Option<Signal> {
            let mu = if ev.recv_ns < self.flat_from { 1e-3 } else { 0.0 };
            Some(Signal::new(ev.recv_ns, ev.seq, ev.instrument, 1, ev.mid(), ev.bid, ev.ask, mu, 1e-6))
        }
    }
    const ALWAYS_LONG: Script = Script { flat_from: u64::MAX };

    fn quote(t: u64, bid: f64, ask: f64) -> MarketEvent {
        MarketEvent::quote(t, t, 0, InstrumentId(0), 0, Px::from_f64(bid), Px::from_f64(ask), Qty::ZERO, Qty::ZERO)
    }

    fn risk() -> RiskConfig {
        RiskConfig { max_orders_per_sec: 1000, order_burst: 1000, stop_loss: 0.5, ..Default::default() }
    }

    #[test]
    fn latency_prevents_same_tick_fill_and_costs_are_booked() {
        // Tue 9 Jan 2024 12:00 UTC.
        let t0 = days_from_civil(2024, 1, 9) as u64 * 24 * H + 12 * H;
        let cost = CostModel { latency_ms: 50, slippage_bps: 0.0, commission_per_million: 35.0, swap_long_annual: 0.0365, swap_short_annual: 0.0 };
        let mut s = Simulation::new("t", ALWAYS_LONG, risk(), cost).unwrap();
        s.on_event(&quote(t0, 1.0999, 1.1001));
        assert_eq!(s.fills, 0, "order must not fill on the deciding tick");
        s.on_event(&quote(t0 + 10_000_000, 1.0999, 1.1001)); // +10ms: still inside latency
        assert_eq!(s.fills, 0);
        s.on_event(&quote(t0 + 60_000_000, 1.0999, 1.1001)); // +60ms: fills at ask
        assert_eq!(s.fills, 1);
        let qty = s.risk.portfolio.position(InstrumentId(0)).qty;
        assert!((qty * 1.1001 - 10_000.0).abs() < 1.0, "capped at max_position_notional");
        assert!((s.costs.spread - 0.0001 * qty).abs() < 1e-6);
        assert!((s.costs.commission - 10_000.0 * 35e-6).abs() < 1e-3);
        // Cross the 22:00 UTC rollover (winter): one day of 3.65%/yr on ~10k = ~1.00.
        s.on_event(&quote(t0 + 10 * H + 1, 1.0999, 1.1001));
        assert!((s.costs.financing - 1.0).abs() < 0.01, "{}", s.costs.financing);
        let r = s.finish((t0 + 11 * H) as i64);
        assert_eq!(r.days.len(), 1); // Tue rollover; equity unchanged after it
        assert!(r.final_equity < r.starting_equity);
    }

    #[test]
    fn closes_are_net_of_attributed_costs() {
        let t0 = days_from_civil(2024, 1, 9) as u64 * 24 * H + 12 * H;
        let cost = CostModel { latency_ms: 50, slippage_bps: 0.0, commission_per_million: 35.0, swap_long_annual: 0.0365, swap_short_annual: 0.0 };
        let flat_from = t0 + 10 * H + 1;
        let mut s = Simulation::new("t", Script { flat_from }, risk(), cost).unwrap();
        s.on_event(&quote(t0, 1.0999, 1.1001));
        s.on_event(&quote(t0 + 60_000_000, 1.0999, 1.1001)); // buy ~9091 @ 1.1001
        // After the 22:00 rollover (~1.00 financing) the view goes flat; sell @ bid 1.1004.
        s.on_event(&quote(flat_from, 1.1004, 1.1006));
        s.on_event(&quote(flat_from + 100_000_000, 1.1004, 1.1006));
        assert_eq!(s.closes.len(), 1);
        let qty = 10_000.0 / 1.1;
        let gross = qty * (1.1004 - 1.1001);
        let costs = qty * 1.1001 * 35e-6 + qty * 1.1004 * 35e-6 + 1.0;
        assert!((s.closes[0] - (gross - costs)).abs() < 0.01, "net {} vs {}", s.closes[0], gross - costs);
        assert!(s.closes[0] < gross, "a win must be judged after costs");
    }

    #[test]
    fn limit_breached_during_latency_cancels() {
        let t0 = days_from_civil(2024, 1, 9) as u64 * 24 * H + 12 * H;
        let mut s = Simulation::new("t", ALWAYS_LONG, risk(), CostModel::default()).unwrap();
        s.on_event(&quote(t0, 1.0999, 1.1001));
        // Price gaps up 1% before the order is eligible: limit (ask + 5bp) is exceeded.
        s.on_event(&quote(t0 + 100_000_000, 1.1109, 1.1111));
        assert_eq!((s.fills, s.cancels), (0, 1));
        assert_eq!(s.risk.portfolio.position(InstrumentId(0)).qty, 0.0);
    }
}
