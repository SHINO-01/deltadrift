//! Hard risk guardrails (FR-4) and portfolio accounting.
//!
//! This crate deliberately does **not** depend on `dd-strategy`: strategy code
//! cannot reach in and loosen a limit. Strategies supply a view
//! (`mu`, `sigma2`); this crate alone turns it into a size, and every order
//! the system emits passes through [`RiskEngine::on_signal`].
//!
//! Guardrails, in evaluation order:
//! 1. Signal validation (finite, positive variance, sane price).
//! 2. Daily drawdown kill-switch (latched for the trading day; flatten-only).
//! 3. Per-position stop-loss with cooldown.
//! 4. Fractional-Kelly sizing `f = k * mu / sigma2`, clamped to per-position
//!    and gross notional caps.
//! 5. Minimum order notional (hysteresis; prevents churn on tiny deltas).
//! 6. Order-rate token bucket.

#![forbid(unsafe_code)]

mod portfolio;

pub use portfolio::{Portfolio, Position};

use dd_core::{Fill, InstrumentId, MAX_INSTRUMENTS, Px, Qty, Side, Signal};
use serde::Deserialize;

#[derive(Debug, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct RiskConfig {
    /// Starting account equity in quote currency.
    pub starting_equity: f64,
    /// Max intraday loss as a fraction of day-start equity before halting (e.g. 0.02).
    pub max_daily_drawdown: f64,
    /// Max unrealised loss on one position as a fraction of its cost basis.
    pub stop_loss: f64,
    /// After a stop-out, ignore new entries on that instrument for this long.
    pub stop_cooldown_ms: u64,
    /// Kelly multiplier (0.25 = quarter Kelly).
    pub kelly_fraction: f64,
    /// Absolute cap on any single position's notional.
    pub max_position_notional: f64,
    /// Cap on the sum of absolute notionals across all positions.
    pub max_gross_notional: f64,
    /// Orders smaller than this notional are suppressed.
    pub min_order_notional: f64,
    /// Sustained order rate limit and burst size.
    pub max_orders_per_sec: u32,
    pub order_burst: u32,
    /// Marketable-limit aggressiveness beyond the touch, in basis points.
    pub limit_slippage_bps: f64,
}

impl Default for RiskConfig {
    fn default() -> Self {
        Self {
            starting_equity: 100_000.0,
            max_daily_drawdown: 0.02,
            stop_loss: 0.01,
            stop_cooldown_ms: 5_000,
            kelly_fraction: 0.25,
            max_position_notional: 10_000.0,
            max_gross_notional: 50_000.0,
            min_order_notional: 50.0,
            max_orders_per_sec: 50,
            order_burst: 10,
            limit_slippage_bps: 5.0,
        }
    }
}

impl RiskConfig {
    pub fn validate(&self) -> Result<(), String> {
        let checks: [(bool, &str); 8] = [
            (self.starting_equity > 0.0, "starting_equity must be > 0"),
            ((0.0..1.0).contains(&self.max_daily_drawdown) && self.max_daily_drawdown > 0.0, "max_daily_drawdown must be in (0,1)"),
            (self.stop_loss > 0.0 && self.stop_loss < 1.0, "stop_loss must be in (0,1)"),
            (self.kelly_fraction > 0.0 && self.kelly_fraction <= 1.0, "kelly_fraction must be in (0,1]"),
            (self.max_position_notional > 0.0, "max_position_notional must be > 0"),
            (self.max_gross_notional >= self.max_position_notional, "max_gross_notional must be >= max_position_notional"),
            (self.max_orders_per_sec > 0 && self.order_burst > 0, "rate limits must be > 0"),
            (self.limit_slippage_bps >= 0.0, "limit_slippage_bps must be >= 0"),
        ];
        match checks.iter().find(|(ok, _)| !ok) {
            Some((_, msg)) => Err((*msg).to_string()),
            None => Ok(()),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum RejectReason {
    InvalidSignal = 0,
    UnknownInstrument = 1,
    RateLimited = 2,
}

impl RejectReason {
    pub const COUNT: usize = 3;
    pub const ALL: [Self; Self::COUNT] = [Self::InvalidSignal, Self::UnknownInstrument, Self::RateLimited];
    pub const fn name(self) -> &'static str {
        match self {
            Self::InvalidSignal => "invalid_signal",
            Self::UnknownInstrument => "unknown_instrument",
            Self::RateLimited => "rate_limited",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Decision {
    /// Nothing to do (already at target, or delta below minimum).
    Hold,
    Order { side: Side, qty: Qty, limit: Px },
    Reject(RejectReason),
}

/// Integer-nanosecond token bucket; no floats, no syscalls.
#[derive(Debug, Clone, Copy)]
struct TokenBucket {
    /// Tokens scaled by 1e9 so refill is exact integer math.
    tokens: u64,
    cap: u64,
    per_ns: u64,
    last_ns: u64,
}

impl TokenBucket {
    const UNIT: u64 = 1_000_000_000;
    fn new(rate_per_sec: u32, burst: u32) -> Self {
        let cap = u64::from(burst) * Self::UNIT;
        Self { tokens: cap, cap, per_ns: u64::from(rate_per_sec), last_ns: 0 }
    }
    #[inline]
    fn try_take(&mut self, now_ns: u64) -> bool {
        let dt = now_ns.saturating_sub(self.last_ns);
        self.last_ns = now_ns.max(self.last_ns);
        self.tokens = self.tokens.saturating_add(dt.saturating_mul(self.per_ns)).min(self.cap);
        if self.tokens >= Self::UNIT {
            self.tokens -= Self::UNIT;
            true
        } else {
            false
        }
    }
}

#[derive(Debug, Default, Clone, Copy)]
pub struct RiskStats {
    pub signals: u64,
    pub orders: u64,
    pub holds: u64,
    pub stop_outs: u64,
    pub kelly_capped: u64,
    pub gross_capped: u64,
    pub rejects: [u64; RejectReason::COUNT],
    pub halted: bool,
    pub halt_ns: u64,
}

#[derive(Debug)]
pub struct RiskEngine {
    cfg: RiskConfig,
    pub portfolio: Portfolio,
    /// Signed quantity sent but not yet filled or cancelled, per instrument.
    pending: Box<[f64]>,
    cooldown_until: Box<[u64]>,
    day_start_equity: f64,
    peak_equity: f64,
    max_drawdown: f64,
    halted: bool,
    bucket: TokenBucket,
    stats: RiskStats,
}

impl RiskEngine {
    pub fn new(cfg: RiskConfig) -> Result<Self, String> {
        cfg.validate()?;
        Ok(Self {
            portfolio: Portfolio::new(cfg.starting_equity),
            pending: vec![0.0; MAX_INSTRUMENTS].into_boxed_slice(),
            cooldown_until: vec![0; MAX_INSTRUMENTS].into_boxed_slice(),
            day_start_equity: cfg.starting_equity,
            peak_equity: cfg.starting_equity,
            max_drawdown: 0.0,
            halted: false,
            bucket: TokenBucket::new(cfg.max_orders_per_sec, cfg.order_burst),
            stats: RiskStats::default(),
            cfg,
        })
    }

    pub fn config(&self) -> &RiskConfig {
        &self.cfg
    }
    pub fn stats(&self) -> RiskStats {
        self.stats
    }
    pub fn is_halted(&self) -> bool {
        self.halted
    }
    /// Peak-to-trough drawdown of equity over the whole session, as a fraction.
    pub fn max_drawdown(&self) -> f64 {
        self.max_drawdown
    }

    /// Start a new trading day: re-base the daily drawdown and clear the halt.
    pub fn roll_day(&mut self) {
        self.day_start_equity = self.portfolio.equity();
        self.halted = false;
    }

    /// Evaluate one signal. The only path by which orders are created.
    #[inline]
    pub fn on_signal(&mut self, s: &Signal, now_ns: u64) -> Decision {
        self.stats.signals += 1;
        let i = s.instrument.idx();
        if i >= MAX_INSTRUMENTS {
            return self.reject(RejectReason::UnknownInstrument);
        }
        let px = s.ref_px.to_f64();
        let valid_view = s.mu.is_finite() && s.sigma2.is_finite() && (s.mu == 0.0 || s.sigma2 > 0.0);
        if !(px > 0.0 && valid_view && s.bid.0 > 0 && s.ask.0 >= s.bid.0) {
            return self.reject(RejectReason::InvalidSignal);
        }

        // 1. Mark-to-market and update drawdown state.
        self.portfolio.mark(s.instrument, px);
        let equity = self.portfolio.equity();
        self.peak_equity = self.peak_equity.max(equity);
        self.max_drawdown = self.max_drawdown.max(1.0 - equity / self.peak_equity);
        if !self.halted && equity <= self.day_start_equity * (1.0 - self.cfg.max_daily_drawdown) {
            self.halted = true;
            self.stats.halted = true;
            self.stats.halt_ns = now_ns;
        }

        // 2. Stop-loss.
        let pos = self.portfolio.position(s.instrument);
        let stopped = pos.qty != 0.0 && pos.unrealized() < -self.cfg.stop_loss * (pos.qty * pos.avg_px).abs();
        if stopped {
            // Count the trigger, not every tick spent flattening (e.g. while rate-limited).
            if now_ns >= self.cooldown_until[i] {
                self.stats.stop_outs += 1;
            }
            self.cooldown_until[i] = now_ns.saturating_add(self.cfg.stop_cooldown_ms * 1_000_000);
        }

        // 3. Target notional: fractional Kelly, clamped. Flatten when halted/stopped/cooling down.
        let target_notional = if self.halted || stopped || now_ns < self.cooldown_until[i] || s.mu == 0.0 {
            0.0
        } else {
            let raw = self.cfg.kelly_fraction * (s.mu / s.sigma2) * equity;
            let capped = raw.clamp(-self.cfg.max_position_notional, self.cfg.max_position_notional);
            if capped != raw {
                self.stats.kelly_capped += 1;
            }
            // Gross cap: room left after every *other* position.
            let others = self.portfolio.gross_notional() - (pos.qty * px).abs();
            let room = (self.cfg.max_gross_notional - others).max(0.0);
            if capped.abs() > room {
                self.stats.gross_capped += 1;
                capped.signum() * room
            } else {
                capped
            }
        };

        // 4. Delta vs. current + in-flight.
        let delta = target_notional / px - (pos.qty + self.pending[i]);
        // Reducing to exactly flat is always allowed through the min-notional gate.
        let flattening = target_notional == 0.0 && pos.qty != 0.0;
        if delta == 0.0 || (!flattening && (delta * px).abs() < self.cfg.min_order_notional) {
            self.stats.holds += 1;
            return Decision::Hold;
        }

        // 5. Rate limit (risk-reducing orders are not exempt: a broken loop must not spam).
        if !self.bucket.try_take(now_ns) {
            return self.reject(RejectReason::RateLimited);
        }

        let side = if delta > 0.0 { Side::Buy } else { Side::Sell };
        let slip = self.cfg.limit_slippage_bps * 1e-4;
        let limit = match side {
            Side::Buy => s.ask.to_f64() * (1.0 + slip),
            Side::Sell => s.bid.to_f64() * (1.0 - slip),
        };
        let qty = Qty::from_f64(delta.abs());
        if qty.0 == 0 {
            self.stats.holds += 1;
            return Decision::Hold;
        }
        self.pending[i] += qty.to_f64() * side.sign() as f64;
        self.stats.orders += 1;
        Decision::Order { side, qty, limit: Px::from_f64(limit) }
    }

    /// Apply an execution report from any broker.
    #[inline]
    pub fn on_fill(&mut self, f: &Fill) {
        let i = f.instrument.idx();
        if i >= MAX_INSTRUMENTS {
            return;
        }
        let signed = f.qty.to_f64() * f.side.sign() as f64;
        self.pending[i] -= signed;
        if self.pending[i].abs() < 1e-9 {
            self.pending[i] = 0.0;
        }
        self.portfolio.apply_fill(f.instrument, signed, f.px.to_f64(), f.fee.to_f64());
    }

    /// Release in-flight quantity for an order that was cancelled/expired unfilled.
    pub fn on_cancel(&mut self, instrument: InstrumentId, side: Side, unfilled: Qty) {
        if let Some(p) = self.pending.get_mut(instrument.idx()) {
            *p -= unfilled.to_f64() * side.sign() as f64;
        }
    }

    fn reject(&mut self, r: RejectReason) -> Decision {
        self.stats.rejects[r as usize] += 1;
        Decision::Reject(r)
    }
}

#[cfg(test)]
mod tests;
