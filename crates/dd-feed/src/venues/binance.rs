//! Binance Spot combined `bookTicker` stream (public, no credentials).
//!
//! `wss://stream.binance.com:9443/stream?streams=btcusdt@bookTicker/...`
//! `{"stream":"btcusdt@bookTicker","data":{"u":400900217,"s":"BNBUSDT","b":"25.35190000","B":"31.21000000","a":"25.36520000","A":"40.66000000"}}`
//!
//! `u` (order-book update id) is monotonic per symbol and used as `seq`.

use dd_core::decimal::{parse_px, parse_qty};
use dd_core::{MarketEvent, venue};
use serde::Deserialize;

use super::{Venue, VenueError};
use crate::SymbolTable;

#[derive(Debug, Clone)]
pub struct Binance {
    symbols: SymbolTable,
    base_url: String,
}

impl Binance {
    pub const DEFAULT_URL: &'static str = "wss://stream.binance.com:9443";

    pub fn new(symbols: SymbolTable) -> Self {
        Self::with_base_url(symbols, Self::DEFAULT_URL)
    }

    /// e.g. `wss://data-stream.binance.vision` (market-data-only endpoint).
    pub fn with_base_url(symbols: SymbolTable, base_url: &str) -> Self {
        Self { symbols, base_url: base_url.trim_end_matches('/').to_string() }
    }

    pub fn symbols(&self) -> &SymbolTable {
        &self.symbols
    }
}

#[derive(Deserialize)]
struct Combined<'a> {
    #[serde(borrow)]
    data: BookTicker<'a>,
}

#[derive(Deserialize)]
struct BookTicker<'a> {
    u: u64,
    s: &'a str,
    b: &'a str,
    #[serde(rename = "B")]
    bq: &'a str,
    a: &'a str,
    #[serde(rename = "A")]
    aq: &'a str,
}

impl Venue for Binance {
    const ID: u8 = venue::BINANCE;
    const NAME: &'static str = "binance";

    fn url(&self) -> String {
        let streams: Vec<String> = self.symbols.symbols().iter().map(|s| format!("{}@bookTicker", s.to_lowercase())).collect();
        format!("{}/stream?streams={}", self.base_url, streams.join("/"))
    }

    fn on_connect(&mut self) -> Vec<String> {
        Vec::new() // subscriptions are encoded in the URL
    }

    #[inline]
    fn on_message<E: FnMut(MarketEvent)>(&mut self, msg: &[u8], recv_ns: u64, emit: &mut E) -> Result<Option<String>, VenueError> {
        let m: Combined<'_> = serde_json::from_slice(msg).map_err(|_| VenueError::Skip("not a bookTicker message"))?;
        let t = m.data;
        let id = self.symbols.lookup(t.s.as_bytes()).ok_or(VenueError::Skip("unknown symbol"))?;
        let (Some(bid), Some(ask), Some(bq), Some(aq)) =
            (parse_px(t.b.as_bytes()), parse_px(t.a.as_bytes()), parse_qty(t.bq.as_bytes()), parse_qty(t.aq.as_bytes()))
        else {
            return Err(VenueError::Skip("bad number"));
        };
        emit(MarketEvent::quote(recv_ns, 0, t.u, id, Self::ID, bid, ask, bq, aq));
        Ok(None)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use dd_core::{InstrumentId, Px, Qty};

    pub(crate) const SAMPLE: &[u8] = br#"{"stream":"btcusdt@bookTicker","data":{"u":101994177945,"s":"BTCUSDT","b":"80739.42000000","B":"4.57716000","a":"80739.43000000","A":"2.46635000"}}"#;

    #[test]
    fn decodes_book_ticker_exactly() {
        let mut v = Binance::new(SymbolTable::new(&["ETHUSDT", "BTCUSDT"]).unwrap());
        assert_eq!(
            v.url(),
            "wss://stream.binance.com:9443/stream?streams=ethusdt@bookTicker/btcusdt@bookTicker"
        );
        let mut out = vec![];
        assert!(v.on_message(SAMPLE, 42, &mut |e| out.push(e)).unwrap().is_none());
        let e = out[0];
        assert_eq!((e.instrument, e.seq, e.recv_ns, e.venue), (InstrumentId(1), 101_994_177_945, 42, venue::BINANCE));
        assert_eq!((e.bid, e.ask), (Px(8_073_942_000_000), Px(8_073_943_000_000)));
        assert_eq!((e.bid_qty, e.ask_qty), (Qty(457_716_000), Qty(246_635_000)));
    }

    #[test]
    fn skips_unknown_and_malformed() {
        let mut v = Binance::new(SymbolTable::new(&["ETHUSDT"]).unwrap());
        let mut n = 0;
        assert!(matches!(v.on_message(SAMPLE, 0, &mut |_| n += 1), Err(VenueError::Skip(_))));
        assert!(matches!(v.on_message(br#"{"result":null,"id":1}"#, 0, &mut |_| n += 1), Err(VenueError::Skip(_))));
        assert_eq!(n, 0);
    }
}
