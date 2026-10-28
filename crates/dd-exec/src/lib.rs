//! Execution layer (FR-5): broker abstraction, pre-formatted order templates,
//! and a deterministic paper broker.
//!
//! Brokers are used through generics (static dispatch) on the hot path. Live
//! adapters (Alpaca, IBKR, crypto venues) arrive in Sprint 4 behind the same
//! [`Broker`] trait, so switching PAPER -> LIVE is a config change, not a code change.

#![forbid(unsafe_code)]

use dd_core::{Fill, InstrumentId, MAX_INSTRUMENTS, Order, Px, Qty, Side, TimeInForce};
use serde::Deserialize;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TradingMode {
    Paper,
    Live,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BrokerError {
    QueueFull,
    UnknownInstrument,
}

/// Execution report delivered by [`Broker::poll`].
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Exec {
    Fill(Fill),
    /// Order closed with `unfilled` quantity never executed (IOC miss, venue reject, cancel).
    Cancelled { client_id: u64, instrument: InstrumentId, side: Side, unfilled: Qty },
}

pub trait Broker {
    /// Update the broker's view of the touch (used by simulators; live brokers ignore it).
    fn on_quote(&mut self, _instrument: InstrumentId, _bid: Px, _ask: Px) {}
    /// Submit an order. Must not block.
    fn submit(&mut self, order: &Order) -> Result<(), BrokerError>;
    /// Drain execution reports.
    fn poll<F: FnMut(Exec)>(&mut self, on_exec: F);
}

/// One pre-built order per instrument; only five fields change per order.
#[derive(Debug)]
pub struct OrderTemplates {
    templates: Box<[Order]>,
    next_id: u64,
}

impl OrderTemplates {
    /// `session` occupies the top 16 bits of every client order id so ids
    /// are unique across restarts without coordination.
    pub fn new(venue: u8, tif: TimeInForce, session: u16) -> Self {
        let templates = (0..MAX_INSTRUMENTS as u32).map(|i| Order::template(InstrumentId(i), venue, tif)).collect();
        Self { templates, next_id: u64::from(session) << 48 }
    }

    #[inline(always)]
    pub fn build(&mut self, instrument: InstrumentId, origin_ns: u64, side: Side, qty: Qty, limit: Px) -> Option<Order> {
        let t = *self.templates.get(instrument.idx())?;
        self.next_id += 1;
        Some(t.stamp(self.next_id, origin_ns, side, qty, limit))
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct PaperConfig {
    /// Taker fee in basis points of notional.
    pub fee_bps: f64,
    /// Adverse slippage beyond the touch, in basis points.
    pub slippage_bps: f64,
}

impl Default for PaperConfig {
    fn default() -> Self {
        Self { fee_bps: 1.0, slippage_bps: 0.5 }
    }
}

/// Immediate-fill simulator: marketable orders fill fully at the touch plus
/// slippage, capped at the order's limit. Orders that would not cross are
/// reported as [`Exec::Cancelled`] so in-flight state is released.
#[derive(Debug)]
pub struct PaperBroker {
    cfg: PaperConfig,
    touch: Box<[(Px, Px)]>,
    out: Vec<Exec>,
    clock: fn() -> u64,
}

impl PaperBroker {
    const QUEUE: usize = 4096;

    pub fn new(cfg: PaperConfig, clock: fn() -> u64) -> Self {
        Self {
            cfg,
            touch: vec![(Px::ZERO, Px::ZERO); MAX_INSTRUMENTS].into_boxed_slice(),
            out: Vec::with_capacity(Self::QUEUE),
            clock,
        }
    }
}

impl Broker for PaperBroker {
    #[inline]
    fn on_quote(&mut self, instrument: InstrumentId, bid: Px, ask: Px) {
        if let Some(t) = self.touch.get_mut(instrument.idx()) {
            *t = (bid, ask);
        }
    }

    #[inline]
    fn submit(&mut self, o: &Order) -> Result<(), BrokerError> {
        if self.out.len() == Self::QUEUE {
            return Err(BrokerError::QueueFull);
        }
        let (bid, ask) = *self.touch.get(o.instrument.idx()).ok_or(BrokerError::UnknownInstrument)?;
        let slip = self.cfg.slippage_bps * 1e-4;
        let (touch, crosses) = match o.side {
            Side::Buy => (ask.to_f64() * (1.0 + slip), ask <= o.limit),
            Side::Sell => (bid.to_f64() * (1.0 - slip), bid >= o.limit),
        };
        let report = if crosses {
            let px = match o.side {
                Side::Buy => touch.min(o.limit.to_f64()),
                Side::Sell => touch.max(o.limit.to_f64()),
            };
            let fee = o.qty.to_f64() * px * self.cfg.fee_bps * 1e-4;
            Exec::Fill(Fill::new(o.client_id, (self.clock)(), o.instrument, o.side, o.qty, Px::from_f64(px), Px::from_f64(fee)))
        } else {
            Exec::Cancelled { client_id: o.client_id, instrument: o.instrument, side: o.side, unfilled: o.qty }
        };
        self.out.push(report);
        Ok(())
    }

    #[inline]
    fn poll<F: FnMut(Exec)>(&mut self, on_exec: F) {
        self.out.drain(..).for_each(on_exec);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paper_fills_at_touch_within_limit() {
        let mut b = PaperBroker::new(PaperConfig { fee_bps: 10.0, slippage_bps: 0.0 }, || 7);
        let mut t = OrderTemplates::new(1, TimeInForce::Ioc, 9);
        let i = InstrumentId(0);
        b.on_quote(i, Px::from_f64(99.0), Px::from_f64(101.0));
        let o = t.build(i, 0, Side::Buy, Qty::from_f64(2.0), Px::from_f64(102.0)).unwrap();
        assert_eq!(o.client_id >> 48, 9);
        b.submit(&o).unwrap();
        let mut fills = vec![];
        b.poll(|e| match e {
            Exec::Fill(f) => fills.push(f),
            Exec::Cancelled { .. } => panic!("expected fill"),
        });
        assert_eq!(fills.len(), 1);
        assert_eq!(fills[0].px, Px::from_f64(101.0));
        assert_eq!(fills[0].qty, Qty::from_f64(2.0));
        assert!((fills[0].fee.to_f64() - 0.202).abs() < 1e-8);
    }

    #[test]
    fn non_crossing_ioc_reports_cancel() {
        let mut b = PaperBroker::new(PaperConfig::default(), || 0);
        let i = InstrumentId(0);
        b.on_quote(i, Px::from_f64(99.0), Px::from_f64(101.0));
        let o = Order::template(i, 0, TimeInForce::Ioc).stamp(1, 0, Side::Buy, Qty::from_f64(1.0), Px::from_f64(100.0));
        b.submit(&o).unwrap();
        let mut got = None;
        b.poll(|e| got = Some(e));
        assert_eq!(
            got,
            Some(Exec::Cancelled { client_id: 1, instrument: i, side: Side::Buy, unfilled: Qty::from_f64(1.0) })
        );
    }
}
