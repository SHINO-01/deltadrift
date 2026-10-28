//! Wire types passed between pipeline stages.
//!
//! Every message is `Copy`, `repr(C)`, 64-byte aligned and exactly 64 bytes,
//! so one ring slot == one cache line and no two slots ever false-share.
//! Prices and quantities are fixed-point (1e-8) to keep the hot path free of
//! float rounding drift and to allow exact equality checks.

use core::fmt;

/// Fixed-point scale: 1 unit = 1e-8.
pub const FX_SCALE: i64 = 100_000_000;
const FX_SCALE_F: f64 = FX_SCALE as f64;

macro_rules! fixed {
    ($name:ident, $doc:literal) => {
        #[doc = $doc]
        #[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
        #[repr(transparent)]
        pub struct $name(pub i64);

        impl $name {
            pub const ZERO: Self = Self(0);
            #[inline(always)]
            pub fn from_f64(v: f64) -> Self {
                Self((v * FX_SCALE_F).round() as i64)
            }
            #[inline(always)]
            pub fn to_f64(self) -> f64 {
                self.0 as f64 / FX_SCALE_F
            }
        }

        impl fmt::Debug for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                write!(f, "{}", self.to_f64())
            }
        }
    };
}

fixed!(Px, "Price in fixed-point 1e-8 units.");
fixed!(Qty, "Signed or unsigned quantity in fixed-point 1e-8 units.");

/// Dense instrument index (0..MAX_INSTRUMENTS); symbol mapping lives off the hot path.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Default, Debug)]
#[repr(transparent)]
pub struct InstrumentId(pub u32);

/// Upper bound on concurrently tracked instruments; per-instrument state is
/// stored in fixed arrays of this size so lookups are a bounds-checked index.
pub const MAX_INSTRUMENTS: usize = 256;

impl InstrumentId {
    #[inline(always)]
    pub const fn idx(self) -> usize {
        self.0 as usize
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
#[repr(u8)]
pub enum EventKind {
    #[default]
    Quote = 0,
    Trade = 1,
}

/// Normalised top-of-book / trade update. Produced by the feed core.
#[derive(Clone, Copy, Debug, Default)]
#[repr(C, align(64))]
pub struct MarketEvent {
    /// Local monotonic receive time (see [`crate::clock`]). Latency origin.
    pub recv_ns: u64,
    /// Venue timestamp, ns since Unix epoch (0 if unknown).
    pub exch_ns: u64,
    /// Per-feed sequence number for gap detection.
    pub seq: u64,
    pub instrument: InstrumentId,
    pub kind: EventKind,
    _pad: [u8; 3],
    pub bid: Px,
    pub ask: Px,
    pub last: Px,
    pub last_qty: Qty,
}

impl MarketEvent {
    #[inline(always)]
    #[allow(clippy::too_many_arguments)]
    pub const fn new(
        recv_ns: u64,
        exch_ns: u64,
        seq: u64,
        instrument: InstrumentId,
        kind: EventKind,
        bid: Px,
        ask: Px,
        last: Px,
        last_qty: Qty,
    ) -> Self {
        Self { recv_ns, exch_ns, seq, instrument, kind, _pad: [0; 3], bid, ask, last, last_qty }
    }

    #[inline(always)]
    pub fn mid(&self) -> Px {
        Px((self.bid.0 + self.ask.0) / 2)
    }
}

/// Strategy output: a *view* on the instrument, never a size.
///
/// The strategy states expected return and its variance over its horizon;
/// position sizing (fractional Kelly), limits and kill-switches are applied
/// downstream by the risk core, which the strategy cannot bypass.
#[derive(Clone, Copy, Debug, Default)]
#[repr(C, align(64))]
pub struct Signal {
    /// Propagated from the originating [`MarketEvent::recv_ns`].
    pub origin_ns: u64,
    pub seq: u64,
    pub instrument: InstrumentId,
    /// Strategy/policy id, for attribution and bandit allocation.
    pub strategy: u16,
    _pad: [u8; 2],
    /// Reference (mark) price at signal time.
    pub ref_px: Px,
    pub bid: Px,
    pub ask: Px,
    /// Expected return over the strategy horizon (fraction, e.g. 0.001 = 10 bp).
    pub mu: f64,
    /// Variance of that return. Must be > 0 when `mu != 0`.
    pub sigma2: f64,
}

impl Signal {
    #[allow(clippy::too_many_arguments)]
    #[inline(always)]
    pub const fn new(
        origin_ns: u64,
        seq: u64,
        instrument: InstrumentId,
        strategy: u16,
        ref_px: Px,
        bid: Px,
        ask: Px,
        mu: f64,
        sigma2: f64,
    ) -> Self {
        Self { origin_ns, seq, instrument, strategy, _pad: [0; 2], ref_px, bid, ask, mu, sigma2 }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
#[repr(u8)]
pub enum Side {
    #[default]
    Buy = 0,
    Sell = 1,
}

impl Side {
    #[inline(always)]
    pub const fn sign(self) -> i64 {
        match self {
            Side::Buy => 1,
            Side::Sell => -1,
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
#[repr(u8)]
pub enum TimeInForce {
    #[default]
    Ioc = 0,
    Gtc = 1,
    Day = 2,
}

/// Pre-formatted order. Templates are built per instrument at startup; on a
/// signal only `client_id`, `side`, `qty`, `limit` and `origin_ns` change.
#[derive(Clone, Copy, Debug, Default)]
#[repr(C, align(64))]
pub struct Order {
    pub client_id: u64,
    pub origin_ns: u64,
    pub instrument: InstrumentId,
    pub side: Side,
    pub tif: TimeInForce,
    pub venue: u8,
    _pad0: u8,
    pub qty: Qty,
    pub limit: Px,
    _pad1: [u8; 24],
}

impl Order {
    pub const fn template(instrument: InstrumentId, venue: u8, tif: TimeInForce) -> Self {
        Self {
            client_id: 0,
            origin_ns: 0,
            instrument,
            side: Side::Buy,
            tif,
            venue,
            _pad0: 0,
            qty: Qty::ZERO,
            limit: Px::ZERO,
            _pad1: [0; 24],
        }
    }

    /// Stamp the mutable fields onto a template copy. Branch-free, no alloc.
    #[inline(always)]
    pub fn stamp(mut self, client_id: u64, origin_ns: u64, side: Side, qty: Qty, limit: Px) -> Self {
        self.client_id = client_id;
        self.origin_ns = origin_ns;
        self.side = side;
        self.qty = qty;
        self.limit = limit;
        self
    }
}

/// Execution report from a broker adapter.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[repr(C, align(64))]
pub struct Fill {
    pub client_id: u64,
    pub ts_ns: u64,
    pub instrument: InstrumentId,
    pub side: Side,
    _pad0: [u8; 3],
    pub qty: Qty,
    pub px: Px,
    /// Fees paid, in quote currency (fixed-point).
    pub fee: Px,
    _pad1: [u8; 16],
}

impl Fill {
    pub const fn new(
        client_id: u64,
        ts_ns: u64,
        instrument: InstrumentId,
        side: Side,
        qty: Qty,
        px: Px,
        fee: Px,
    ) -> Self {
        Self { client_id, ts_ns, instrument, side, _pad0: [0; 3], qty, px, fee, _pad1: [0; 16] }
    }
}

// Layout contract: one message == one cache line. Breaking this is a compile error.
const _: () = {
    use core::mem::{align_of, size_of};
    assert!(size_of::<MarketEvent>() == 64 && align_of::<MarketEvent>() == 64);
    assert!(size_of::<Signal>() == 64 && align_of::<Signal>() == 64);
    assert!(size_of::<Order>() == 64 && align_of::<Order>() == 64);
    assert!(size_of::<Fill>() == 64 && align_of::<Fill>() == 64);
};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fixed_point_roundtrip() {
        assert_eq!(Px::from_f64(123.456_789_01).0, 12_345_678_901);
        assert!((Px(12_345_678_901).to_f64() - 123.456_789_01).abs() < 1e-12);
    }

    #[test]
    fn order_stamp_preserves_template() {
        let t = Order::template(InstrumentId(7), 2, TimeInForce::Ioc);
        let o = t.stamp(42, 9, Side::Sell, Qty(5), Px(10));
        assert_eq!((o.instrument, o.venue, o.tif), (InstrumentId(7), 2, TimeInForce::Ioc));
        assert_eq!((o.client_id, o.side, o.qty, o.limit), (42, Side::Sell, Qty(5), Px(10)));
    }
}
