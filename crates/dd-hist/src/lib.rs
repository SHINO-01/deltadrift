//! Historical market data for backtesting.
//!
//! * [`dukascopy`] — free FX tick history: URL scheme, `.bi5` (LZMA) decoding,
//!   resumable polite downloader. Raw `.bi5` files are kept verbatim as the
//!   canonical, reproducible store (`data/dukascopy/<SYM>/<YYYY>/<MM>/<DD>/<HH>h_ticks.bi5`).
//! * [`source`] — replay sources yielding [`MarketEvent`](dd_core::MarketEvent)s in
//!   strict time order (k-way merge across instruments), with background
//!   decoding of upcoming days.
//!
//! Timestamps: `recv_ns = exch_ns = tick time` (Unix ns). Simulated latency is
//! applied by the backtester, never baked into the data.

#![forbid(unsafe_code)]

pub mod dukascopy;
pub mod source;

/// Static description of a tradable instrument for data and costs.
#[derive(Debug, Clone, PartialEq)]
pub struct InstrumentSpec {
    pub symbol: String,
    /// Price scale of the source integers (EURUSD 1e5, USDJPY 1e3).
    pub point: u32,
}

impl InstrumentSpec {
    /// Dukascopy's conventional scale for common symbols.
    pub fn dukascopy(symbol: &str) -> Self {
        let s = symbol.to_ascii_uppercase();
        let point = if s.ends_with("JPY") || s.starts_with("XAU") || s.starts_with("XAG") { 1_000 } else { 100_000 };
        Self { symbol: s, point }
    }
}
