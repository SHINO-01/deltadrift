//! Alpaca market data v2 (stocks) and v1beta3 (crypto) quote streams.
//! Requires `ALPACA_API_KEY_ID` / `ALPACA_API_SECRET_KEY` (see `.env.example`).
//!
//! Session: server `[{"T":"success","msg":"connected"}]` → we send auth →
//! `[{"T":"success","msg":"authenticated"}]` → we send subscribe →
//! `[{"T":"subscription",...}]` → quotes
//! `[{"T":"q","S":"BTC/USD","bp":67000.1,"bs":0.5,"ap":67001.2,"as":0.3,"t":"2024-03-12T10:27:48.858228144Z"}, ...]`.
//!
//! Messages are JSON arrays; they're walked element by element with a
//! streaming visitor (no `Vec`), and prices are read as raw JSON number text
//! so they convert to fixed point exactly.

use std::fmt;

use dd_core::decimal::{parse_px, parse_qty};
use dd_core::{MarketEvent, venue};
use dd_secrets::Secret;
use serde::de::{DeserializeSeed, SeqAccess, Visitor};
use serde::{Deserialize, Deserializer};
use serde_json::value::RawValue;

use super::{Venue, VenueError};
use crate::SymbolTable;
use crate::time::parse_rfc3339_ns;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AlpacaFeed {
    Crypto,
    /// `iex` (free) or `sip` (paid).
    Stocks { sip: bool },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Session {
    Connecting,
    Authenticating,
    Subscribing,
    Streaming,
}

pub struct Alpaca {
    feed: AlpacaFeed,
    symbols: SymbolTable,
    key: Secret,
    secret: Secret,
    session: Session,
    seq: u64,
}

impl fmt::Debug for Alpaca {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Alpaca").field("feed", &self.feed).field("session", &self.session).finish_non_exhaustive()
    }
}

impl Alpaca {
    pub fn new(feed: AlpacaFeed, symbols: SymbolTable, key: Secret, secret: Secret) -> Self {
        Self { feed, symbols, key, secret, session: Session::Connecting, seq: 0 }
    }

    /// Reads `ALPACA_API_KEY_ID` / `ALPACA_API_SECRET_KEY` from the environment (.env).
    pub fn from_env(feed: AlpacaFeed, symbols: SymbolTable) -> Result<Self, dd_secrets::SecretError> {
        Ok(Self::new(feed, symbols, dd_secrets::require("ALPACA_API_KEY_ID")?, dd_secrets::require("ALPACA_API_SECRET_KEY")?))
    }

    fn venue_id(&self) -> u8 {
        match self.feed {
            AlpacaFeed::Crypto => venue::ALPACA_CRYPTO,
            AlpacaFeed::Stocks { .. } => venue::ALPACA_STOCKS,
        }
    }
}

#[derive(Deserialize)]
struct Msg<'a> {
    #[serde(rename = "T")]
    kind: &'a str,
    #[serde(rename = "S", default)]
    sym: Option<&'a str>,
    #[serde(borrow, default)]
    bp: Option<&'a RawValue>,
    #[serde(borrow, default)]
    bs: Option<&'a RawValue>,
    #[serde(borrow, default)]
    ap: Option<&'a RawValue>,
    #[serde(rename = "as", borrow, default)]
    asz: Option<&'a RawValue>,
    #[serde(rename = "t", default)]
    ts: Option<&'a str>,
    #[serde(default)]
    msg: Option<&'a str>,
    #[serde(default)]
    code: Option<i64>,
}

/// Streams the elements of a JSON array into a callback without collecting.
struct ForEach<F>(F);

impl<'de, F: FnMut(Msg<'de>)> DeserializeSeed<'de> for ForEach<F> {
    type Value = ();
    fn deserialize<D: Deserializer<'de>>(self, d: D) -> Result<(), D::Error> {
        d.deserialize_seq(self)
    }
}

impl<'de, F: FnMut(Msg<'de>)> Visitor<'de> for ForEach<F> {
    type Value = ();
    fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("an array of Alpaca messages")
    }
    fn visit_seq<A: SeqAccess<'de>>(mut self, mut seq: A) -> Result<(), A::Error> {
        while let Some(m) = seq.next_element::<Msg<'de>>()? {
            (self.0)(m);
        }
        Ok(())
    }
}

fn raw_px(v: Option<&RawValue>) -> Option<dd_core::Px> {
    parse_px(v?.get().as_bytes())
}
fn raw_qty(v: Option<&RawValue>) -> Option<dd_core::Qty> {
    parse_qty(v?.get().as_bytes())
}

impl Venue for Alpaca {
    const ID: u8 = venue::ALPACA_STOCKS; // per-instance id via `venue_id()`
    const NAME: &'static str = "alpaca";

    fn url(&self) -> String {
        match self.feed {
            AlpacaFeed::Crypto => "wss://stream.data.alpaca.markets/v1beta3/crypto/us".into(),
            AlpacaFeed::Stocks { sip } => format!("wss://stream.data.alpaca.markets/v2/{}", if sip { "sip" } else { "iex" }),
        }
    }

    fn on_connect(&mut self) -> Vec<String> {
        self.session = Session::Connecting;
        Vec::new() // wait for the server's "connected" greeting
    }

    fn on_message<E: FnMut(MarketEvent)>(&mut self, msg: &[u8], recv_ns: u64, emit: &mut E) -> Result<Option<String>, VenueError> {
        let venue_id = self.venue_id();
        let (symbols, seq) = (&self.symbols, &mut self.seq);
        let mut session = self.session;
        let mut fatal = None;
        let mut skipped = None;
        let mut de = serde_json::Deserializer::from_slice(msg);
        ForEach(|m: Msg<'_>| match m.kind {
            "q" => {
                let (Some(id), Some(bid), Some(ask), Some(bq), Some(aq)) = (
                    m.sym.and_then(|s| symbols.lookup(s.as_bytes())),
                    raw_px(m.bp),
                    raw_px(m.ap),
                    raw_qty(m.bs),
                    raw_qty(m.asz),
                ) else {
                    skipped = Some("bad or unknown quote");
                    return;
                };
                *seq += 1;
                let exch = m.ts.and_then(|t| parse_rfc3339_ns(t.as_bytes())).unwrap_or(0);
                emit(MarketEvent::quote(recv_ns, exch, *seq, id, venue_id, bid, ask, bq, aq));
            }
            "success" => match m.msg {
                Some("connected") => session = Session::Authenticating,
                Some("authenticated") => session = Session::Subscribing,
                _ => {}
            },
            "subscription" => session = Session::Streaming,
            "error" => fatal = Some(format!("alpaca error {}: {}", m.code.unwrap_or(0), m.msg.unwrap_or("?"))),
            _ => {} // trades/bars/status we didn't subscribe to
        })
        .deserialize(&mut de)
        .map_err(|_| VenueError::Skip("not a JSON array of messages"))?;

        if let Some(f) = fatal {
            return Err(VenueError::Fatal(f));
        }
        let reply = match (self.session, session) {
            (Session::Connecting, Session::Authenticating) => Some(format!(
                r#"{{"action":"auth","key":"{}","secret":"{}"}}"#,
                self.key.expose(),
                self.secret.expose()
            )),
            (Session::Authenticating, Session::Subscribing) => {
                let syms: Vec<String> = self.symbols.symbols().iter().map(|s| format!("\"{s}\"")).collect();
                Some(format!(r#"{{"action":"subscribe","quotes":[{}]}}"#, syms.join(",")))
            }
            _ => None,
        };
        self.session = session;
        match skipped {
            Some(why) if reply.is_none() => Err(VenueError::Skip(why)),
            _ => Ok(reply),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use dd_core::{InstrumentId, Px, Qty};

    fn alpaca() -> Alpaca {
        Alpaca::new(
            AlpacaFeed::Crypto,
            SymbolTable::new(&["BTC/USD", "ETH/USD"]).unwrap(),
            Secret::new("KEY".into()),
            Secret::new("SEC".into()),
        )
    }

    #[test]
    fn session_flow_and_quotes() {
        let mut a = alpaca();
        let mut ev = vec![];
        a.on_connect();
        let r = a.on_message(br#"[{"T":"success","msg":"connected"}]"#, 0, &mut |e| ev.push(e)).unwrap().unwrap();
        assert_eq!(r, r#"{"action":"auth","key":"KEY","secret":"SEC"}"#);
        let r = a.on_message(br#"[{"T":"success","msg":"authenticated"}]"#, 0, &mut |e| ev.push(e)).unwrap().unwrap();
        assert_eq!(r, r#"{"action":"subscribe","quotes":["BTC/USD","ETH/USD"]}"#);
        assert!(a.on_message(br#"[{"T":"subscription","quotes":["BTC/USD","ETH/USD"]}]"#, 0, &mut |e| ev.push(e)).unwrap().is_none());
        assert_eq!(a.session, Session::Streaming);

        let msg = br#"[{"T":"q","S":"ETH/USD","bp":2338.31,"bs":1.5e-3,"ap":2338.32,"as":12,"t":"2024-03-12T10:27:48.858228144Z"},{"T":"q","S":"BTC/USD","bp":80739.42,"bs":0.5,"ap":80739.43,"as":0.25,"t":"2024-03-12T10:27:48Z","c":["R"]}]"#;
        assert!(a.on_message(msg, 7, &mut |e| ev.push(e)).unwrap().is_none());
        assert_eq!(ev.len(), 2);
        assert_eq!((ev[0].instrument, ev[0].bid, ev[0].ask), (InstrumentId(1), Px(233_831_000_000), Px(233_832_000_000)));
        assert_eq!((ev[0].bid_qty, ev[0].ask_qty), (Qty(150_000), Qty(1_200_000_000)));
        assert_eq!((ev[0].exch_ns, ev[0].recv_ns, ev[0].venue), (1_710_239_268_858_228_144, 7, venue::ALPACA_CRYPTO));
        assert_eq!((ev[1].instrument, ev[1].seq), (InstrumentId(0), 2));
    }

    #[test]
    fn auth_failure_is_fatal_and_secret_not_debug_printed() {
        let mut a = alpaca();
        let r = a.on_message(br#"[{"T":"error","code":402,"msg":"auth failed"}]"#, 0, &mut |_| {});
        assert!(matches!(r, Err(VenueError::Fatal(ref m)) if m.contains("402")));
        assert!(!format!("{a:?}").contains("SEC"));
    }
}
