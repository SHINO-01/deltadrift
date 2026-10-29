//! cTrader Open API (Spotware): real-time spot quotes for every instrument a
//! cTrader broker offers (forex, crypto, share/index/commodity CFDs) — free
//! with a demo account.
//!
//! Transport: `wss://{demo|live}.ctraderapi.com:5035`, one binary WebSocket
//! frame per `ProtoMessage { payloadType = 1, payload = 2, clientMsgId = 3 }`.
//! Field numbers/ids from github.com/spotware/openapi-proto-messages.
//!
//! Session: ApplicationAuth → (GetAccountListByAccessToken →) AccountAuth →
//! SymbolsList → SubscribeSpots → SpotEvents. A heartbeat is sent every 10 s
//! (the proxy drops clients silent for > 30 s).
//!
//! Spot events carry only the side(s) that changed, in 1/100000 price units;
//! the last bid/ask per symbol is kept and a quote is emitted once both are known.
//! Sizes are not provided by this stream (`bid_qty = ask_qty = 0`).

use std::fmt;

use dd_core::{InstrumentId, MAX_INSTRUMENTS, MarketEvent, Px, Qty, venue};
use dd_secrets::Secret;

use super::{Outbound, Venue, VenueError};
use crate::SymbolTable;
use crate::proto::{Reader, Value, Writer};

mod pt {
    pub const ERROR_RES: u64 = 50;
    pub const HEARTBEAT: u64 = 51;
    pub const APP_AUTH_REQ: u64 = 2100;
    pub const APP_AUTH_RES: u64 = 2101;
    pub const ACCOUNT_AUTH_REQ: u64 = 2102;
    pub const ACCOUNT_AUTH_RES: u64 = 2103;
    pub const SYMBOLS_LIST_REQ: u64 = 2114;
    pub const SYMBOLS_LIST_RES: u64 = 2115;
    pub const SUBSCRIBE_SPOTS_REQ: u64 = 2127;
    pub const SUBSCRIBE_SPOTS_RES: u64 = 2128;
    pub const SPOT_EVENT: u64 = 2131;
    pub const OA_ERROR_RES: u64 = 2142;
    pub const TOKEN_INVALIDATED: u64 = 2147;
    pub const CLIENT_DISCONNECT: u64 = 2148;
    pub const ACCOUNTS_BY_TOKEN_REQ: u64 = 2149;
    pub const ACCOUNTS_BY_TOKEN_RES: u64 = 2150;
    pub const ACCOUNT_DISCONNECT: u64 = 2164;
}

/// 1/100000 price units → 1e-8 fixed point.
const PX_MULT: i64 = 1_000;
const HEARTBEAT_NS: u64 = 10_000_000_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CtraderEnv {
    Demo,
    Live,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Session {
    AppAuth,
    Accounts,
    AccountAuth,
    Symbols,
    Subscribing,
    Streaming,
}

pub struct Ctrader {
    env: CtraderEnv,
    client_id: Secret,
    client_secret: Secret,
    access_token: Secret,
    /// Fixed account, or `None` = first account of the right env on the token.
    account_cfg: Option<i64>,
    account: i64,
    symbols: SymbolTable,
    /// cTrader symbolId → our instrument, filled from SymbolsListRes.
    ids: IdMap,
    last: Box<[(u64, u64)]>,
    session: Session,
    seq: u64,
    last_sent_ns: u64,
}

impl fmt::Debug for Ctrader {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Ctrader")
            .field("env", &self.env)
            .field("account", &self.account)
            .field("session", &self.session)
            .field("symbols", &self.symbols.symbols())
            .finish_non_exhaustive()
    }
}

impl Ctrader {
    pub fn new(
        env: CtraderEnv,
        symbols: SymbolTable,
        client_id: Secret,
        client_secret: Secret,
        access_token: Secret,
        account: Option<i64>,
    ) -> Self {
        Self {
            env,
            client_id,
            client_secret,
            access_token,
            account_cfg: account,
            account: 0,
            ids: IdMap::new(symbols.len()),
            symbols,
            last: vec![(0, 0); MAX_INSTRUMENTS].into_boxed_slice(),
            session: Session::AppAuth,
            seq: 0,
            last_sent_ns: 0,
        }
    }

    /// Reads `CTRADER_CLIENT_ID`, `CTRADER_CLIENT_SECRET`, `CTRADER_ACCESS_TOKEN`,
    /// optional `CTRADER_ACCOUNT_ID` and `CTRADER_ENV` (`demo` default | `live`).
    pub fn from_env(symbols: SymbolTable) -> Result<Self, String> {
        let req = |k: &str| dd_secrets::require(k).map_err(|e| e.to_string());
        let env = match dd_secrets::optional("CTRADER_ENV").as_ref().map(|s| s.expose().to_ascii_lowercase()) {
            None => CtraderEnv::Demo,
            Some(s) if s == "demo" => CtraderEnv::Demo,
            Some(s) if s == "live" => CtraderEnv::Live,
            Some(s) => return Err(format!("CTRADER_ENV must be `demo` or `live`, got `{s}`")),
        };
        let account = match dd_secrets::optional("CTRADER_ACCOUNT_ID") {
            None => None,
            Some(s) => Some(s.expose().trim().parse::<i64>().map_err(|_| "CTRADER_ACCOUNT_ID must be an integer".to_string())?),
        };
        Ok(Self::new(env, symbols, req("CTRADER_CLIENT_ID")?, req("CTRADER_CLIENT_SECRET")?, req("CTRADER_ACCESS_TOKEN")?, account))
    }

    fn msg(&mut self, payload_type: u64, payload: &[u8]) -> Outbound {
        Outbound::Binary(Writer::new().uint(1, payload_type).bytes(2, payload).finish())
    }

    fn account_auth(&mut self) -> Outbound {
        let p = Writer::new().int(2, self.account).str(3, self.access_token.expose()).finish();
        self.session = Session::AccountAuth;
        self.msg(pt::ACCOUNT_AUTH_REQ, &p)
    }

    /// Picks the first account matching our environment from
    /// ProtoOAGetAccountListByAccessTokenRes. Returns (ctidTraderAccountId, broker login, broker).
    fn pick_account(&self, payload: &[u8]) -> Option<(i64, i64, String)> {
        let want_live = self.env == CtraderEnv::Live;
        Reader::new(payload).flatten().filter(|(f, _)| *f == 4).find_map(|(_, v)| {
            let (mut id, mut live, mut login, mut broker) = (None, false, 0, "");
            for (f, v) in Reader::new(v.bytes()?).flatten() {
                match f {
                    1 => id = v.i64(),
                    2 => live = v.u64() == Some(1),
                    3 => login = v.i64().unwrap_or(0),
                    6 => broker = v.str().unwrap_or(""),
                    _ => {}
                }
            }
            id.filter(|_| live == want_live).map(|id| (id, login, broker.to_string()))
        })
    }

    /// Maps our configured names to cTrader symbol ids (case-insensitive) and
    /// builds the SubscribeSpotsReq. Unknown names are fatal, with suggestions.
    fn subscribe(&mut self, payload: &[u8]) -> Result<Outbound, VenueError> {
        let mut found = vec![None; self.symbols.len()];
        let mut all: Vec<String> = Vec::new();
        for (_, v) in Reader::new(payload).flatten().filter(|(f, _)| *f == 3) {
            let (mut id, mut name, mut enabled) = (None, None, true);
            for (f, v) in Reader::new(v.bytes().unwrap_or_default()).flatten() {
                match f {
                    1 => id = v.i64(),
                    2 => name = v.str(),
                    3 => enabled = v.u64() != Some(0),
                    _ => {}
                }
            }
            let (Some(id), Some(name)) = (id, name) else { continue };
            all.push(name.to_string());
            if let Some(i) = self.symbols.symbols().iter().position(|s| s.eq_ignore_ascii_case(name))
                && enabled
            {
                found[i] = Some(id);
            }
        }
        let missing: Vec<&String> = self.symbols.symbols().iter().zip(&found).filter(|(_, f)| f.is_none()).map(|(s, _)| s).collect();
        if !missing.is_empty() {
            let hints: Vec<String> = missing
                .iter()
                .map(|m| {
                    let key = m.to_ascii_uppercase();
                    let near: Vec<&str> = all.iter().filter(|n| n.to_ascii_uppercase().contains(&key)).take(8).map(String::as_str).collect();
                    format!("`{m}` (similar: {})", if near.is_empty() { "none".into() } else { near.join(", ") })
                })
                .collect();
            return Err(VenueError::Fatal(format!(
                "symbols not offered by this cTrader account: {}. {} symbols available",
                hints.join("; "),
                all.len()
            )));
        }
        let mut w = Writer::new();
        w.int(2, self.account);
        for (i, id) in found.iter().enumerate() {
            let id = id.expect("checked above");
            self.ids.insert(id, InstrumentId(i as u32));
            w.int(3, id);
        }
        w.bool(4, true); // subscribeToSpotTimestamp
        let p = w.finish();
        self.session = Session::Subscribing;
        Ok(self.msg(pt::SUBSCRIBE_SPOTS_REQ, &p))
    }

    #[inline]
    fn on_spot<E: FnMut(MarketEvent)>(&mut self, payload: &[u8], recv_ns: u64, emit: &mut E) -> Result<(), VenueError> {
        let (mut sym, mut bid, mut ask, mut ts) = (None, None, None, 0i64);
        for f in Reader::new(payload) {
            let (f, v) = f.map_err(|()| VenueError::Skip("malformed spot event"))?;
            match f {
                3 => sym = v.i64(),
                4 => bid = v.u64(),
                5 => ask = v.u64(),
                8 => ts = v.i64().unwrap_or(0),
                _ => {}
            }
        }
        let id = sym.and_then(|s| self.ids.get(s)).ok_or(VenueError::Skip("spot for unsubscribed symbol"))?;
        let slot = &mut self.last[id.idx()];
        if let Some(b) = bid {
            slot.0 = b;
        }
        if let Some(a) = ask {
            slot.1 = a;
        }
        let (b, a) = *slot;
        if b == 0 || a == 0 {
            return Ok(()); // waiting for the first full quote
        }
        self.seq += 1;
        let exch_ns = u64::try_from(ts).unwrap_or(0).saturating_mul(1_000_000);
        emit(MarketEvent::quote(
            recv_ns,
            exch_ns,
            self.seq,
            id,
            venue::CTRADER,
            Px(b as i64 * PX_MULT),
            Px(a as i64 * PX_MULT),
            Qty::ZERO,
            Qty::ZERO,
        ));
        Ok(())
    }

    fn error_text(payload: &[u8], code_field: u32, desc_field: u32) -> String {
        let (mut code, mut desc) = ("?", "");
        for (f, v) in Reader::new(payload).flatten() {
            if f == code_field {
                code = v.str().unwrap_or("?");
            } else if f == desc_field {
                desc = v.str().unwrap_or("");
            }
        }
        format!("{code}: {desc}")
    }
}

impl Venue for Ctrader {
    const ID: u8 = venue::CTRADER;
    const NAME: &'static str = "ctrader";
    /// Full symbol lists from large brokers run to hundreds of KiB.
    const MAX_MESSAGE: usize = 8 << 20;

    fn url(&self) -> String {
        match self.env {
            CtraderEnv::Demo => "wss://demo.ctraderapi.com:5035".into(),
            CtraderEnv::Live => "wss://live.ctraderapi.com:5035".into(),
        }
    }

    fn on_connect(&mut self) -> Vec<Outbound> {
        self.session = Session::AppAuth;
        self.last.fill((0, 0));
        let p = Writer::new().str(2, self.client_id.expose()).str(3, self.client_secret.expose()).finish();
        vec![self.msg(pt::APP_AUTH_REQ, &p)]
    }

    #[inline]
    fn on_message<E: FnMut(MarketEvent)>(&mut self, msg: &[u8], recv_ns: u64, emit: &mut E) -> Result<Option<Outbound>, VenueError> {
        let (mut kind, mut payload) = (0u64, &[][..]);
        for f in Reader::new(msg) {
            match f.map_err(|()| VenueError::Skip("malformed ProtoMessage"))? {
                (1, v) => kind = v.u64().unwrap_or(0),
                (2, Value::Bytes(b)) => payload = b,
                _ => {}
            }
        }
        match kind {
            pt::SPOT_EVENT => self.on_spot(payload, recv_ns, emit).map(|()| None),
            pt::HEARTBEAT => Ok(None),
            pt::SUBSCRIBE_SPOTS_RES => {
                if self.session == Session::Subscribing {
                    self.session = Session::Streaming;
                }
                Ok(None)
            }
            pt::APP_AUTH_RES => Ok(Some(match self.account_cfg {
                Some(a) => {
                    self.account = a;
                    self.account_auth()
                }
                None => {
                    self.session = Session::Accounts;
                    let p = Writer::new().str(2, self.access_token.expose()).finish();
                    self.msg(pt::ACCOUNTS_BY_TOKEN_REQ, &p)
                }
            })),
            pt::ACCOUNTS_BY_TOKEN_RES => {
                let (id, login, broker) = self.pick_account(payload).ok_or_else(|| {
                    VenueError::Fatal(format!(
                        "no {:?} account is authorised for this access token; create one (free) and re-authorise the app in the Playground",
                        self.env
                    ))
                })?;
                eprintln!(
                    "[feed:ctrader] using {:?} account: CTRADER_ACCOUNT_ID={id} (broker {broker}, login {login})",
                    self.env
                );
                self.account = id;
                Ok(Some(self.account_auth()))
            }
            pt::ACCOUNT_AUTH_RES => {
                self.session = Session::Symbols;
                let p = Writer::new().int(2, self.account).finish();
                Ok(Some(self.msg(pt::SYMBOLS_LIST_REQ, &p)))
            }
            pt::SYMBOLS_LIST_RES => self.subscribe(payload).map(Some),
            pt::ACCOUNT_DISCONNECT => Ok(Some(self.account_auth())),
            pt::CLIENT_DISCONNECT => Err(VenueError::Skip("server announced disconnect")),
            pt::TOKEN_INVALIDATED => Err(VenueError::Fatal(
                "cTrader access token invalidated; generate a new one in the Open API Playground and update CTRADER_ACCESS_TOKEN".into(),
            )),
            pt::OA_ERROR_RES => Err(VenueError::Fatal(format!("cTrader error {}", Self::error_text(payload, 3, 4)))),
            pt::ERROR_RES => Err(VenueError::Fatal(format!("cTrader proxy error {}", Self::error_text(payload, 2, 3)))),
            _ => Err(VenueError::Skip("unhandled cTrader message")),
        }
    }

    #[inline(always)]
    fn on_tick(&mut self, now_ns: u64) -> Option<Outbound> {
        if now_ns.saturating_sub(self.last_sent_ns) < HEARTBEAT_NS {
            return None;
        }
        self.last_sent_ns = now_ns;
        Some(Outbound::Binary(Writer::new().uint(1, pt::HEARTBEAT).finish()))
    }
}

/// Open-addressed `i64 → InstrumentId` map, built on the cold path.
#[derive(Debug, Clone)]
struct IdMap {
    keys: Box<[i64]>,
    vals: Box<[u32]>,
    mask: usize,
}

impl IdMap {
    const EMPTY: i64 = i64::MIN;

    fn new(n: usize) -> Self {
        let cap = (n * 4).next_power_of_two().max(16);
        Self { keys: vec![Self::EMPTY; cap].into_boxed_slice(), vals: vec![0; cap].into_boxed_slice(), mask: cap - 1 }
    }

    #[inline(always)]
    fn slot(&self, k: i64) -> usize {
        ((k as u64).wrapping_mul(0x9E37_79B9_7F4A_7C15) >> 32) as usize & self.mask
    }

    fn insert(&mut self, k: i64, v: InstrumentId) {
        let mut s = self.slot(k);
        while self.keys[s] != Self::EMPTY && self.keys[s] != k {
            s = (s + 1) & self.mask;
        }
        self.keys[s] = k;
        self.vals[s] = v.0;
    }

    #[inline(always)]
    fn get(&self, k: i64) -> Option<InstrumentId> {
        let mut s = self.slot(k);
        loop {
            match self.keys[s] {
                Self::EMPTY => return None,
                x if x == k => return Some(InstrumentId(self.vals[s])),
                _ => s = (s + 1) & self.mask,
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn wrap(kind: u64, payload: &[u8]) -> Vec<u8> {
        Writer::new().uint(1, kind).bytes(2, payload).finish()
    }

    fn unwrap(o: Option<Outbound>) -> (u64, Vec<(u32, Value<'static>)>) {
        let Some(Outbound::Binary(b)) = o else { panic!("expected binary outbound, got {o:?}") };
        let b: &'static [u8] = Box::leak(b.into_boxed_slice());
        let mut kind = 0;
        let mut payload: &'static [u8] = &[];
        for (f, v) in Reader::new(b).flatten() {
            match f {
                1 => kind = v.u64().unwrap(),
                2 => payload = v.bytes().unwrap(),
                _ => {}
            }
        }
        (kind, Reader::new(payload).flatten().collect())
    }

    fn venue(account: Option<i64>) -> Ctrader {
        Ctrader::new(
            CtraderEnv::Demo,
            SymbolTable::new(&["EURUSD", "BTCUSD", "AAPL.US"]).unwrap(),
            Secret::new("cid".into()),
            Secret::new("csecret".into()),
            Secret::new("tok".into()),
            account,
        )
    }

    fn light_symbol(id: i64, name: &str) -> Vec<u8> {
        Writer::new().int(1, id).str(2, name).bool(3, true).finish()
    }

    fn symbols_res() -> Vec<u8> {
        let mut w = Writer::new();
        w.int(2, 77);
        for (id, n) in [(1, "EURUSD"), (5, "GBPUSD"), (101, "BTCUSD"), (9001, "aapl.us"), (9002, "AAPL.US_old")] {
            w.bytes(3, &light_symbol(id, n));
        }
        w.finish()
    }

    /// Drives the full session and returns a streaming venue.
    fn streaming() -> Ctrader {
        let mut v = venue(None);
        let mut sink = |_e: MarketEvent| {};
        let (k, f) = unwrap(v.on_connect().pop());
        assert_eq!(k, pt::APP_AUTH_REQ);
        assert_eq!((f[0].1.str(), f[1].1.str()), (Some("cid"), Some("csecret")));

        let (k, f) = unwrap(v.on_message(&wrap(pt::APP_AUTH_RES, &[]), 0, &mut sink).unwrap());
        assert_eq!((k, f[0].1.str()), (pt::ACCOUNTS_BY_TOKEN_REQ, Some("tok")));

        let live = Writer::new().uint(1, 11).bool(2, true).finish();
        let demo = Writer::new().uint(1, 77).bool(2, false).int(3, 5_123_456).str(6, "Pepperstone").finish();
        let res = Writer::new().str(2, "tok").bytes(4, &live).bytes(4, &demo).finish();
        let (k, f) = unwrap(v.on_message(&wrap(pt::ACCOUNTS_BY_TOKEN_RES, &res), 0, &mut sink).unwrap());
        assert_eq!((k, f[0].1.i64(), f[1].1.str()), (pt::ACCOUNT_AUTH_REQ, Some(77), Some("tok")));

        let (k, f) = unwrap(v.on_message(&wrap(pt::ACCOUNT_AUTH_RES, &Writer::new().int(2, 77).finish()), 0, &mut sink).unwrap());
        assert_eq!((k, f[0].1.i64()), (pt::SYMBOLS_LIST_REQ, Some(77)));

        let (k, f) = unwrap(v.on_message(&wrap(pt::SYMBOLS_LIST_RES, &symbols_res()), 0, &mut sink).unwrap());
        assert_eq!(k, pt::SUBSCRIBE_SPOTS_REQ);
        let ids: Vec<i64> = f.iter().filter(|(n, _)| *n == 3).map(|(_, v)| v.i64().unwrap()).collect();
        assert_eq!(ids, [1, 101, 9001]); // in config order; case-insensitive match
        assert!(v.on_message(&wrap(pt::SUBSCRIBE_SPOTS_RES, &[]), 0, &mut sink).unwrap().is_none());
        assert_eq!(v.session, Session::Streaming);
        v
    }

    fn spot(sym: i64, bid: Option<u64>, ask: Option<u64>, ts_ms: i64) -> Vec<u8> {
        let mut w = Writer::new();
        w.int(2, 77).int(3, sym);
        if let Some(b) = bid {
            w.uint(4, b);
        }
        if let Some(a) = ask {
            w.uint(5, a);
        }
        w.int(8, ts_ms);
        wrap(pt::SPOT_EVENT, &w.finish())
    }

    #[test]
    fn session_then_exact_quotes_with_partial_updates() {
        let mut v = streaming();
        let mut out = vec![];
        // Bid only: no full quote yet.
        v.on_message(&spot(1, Some(108_512), None, 1_700_000_000_123), 5, &mut |e| out.push(e)).unwrap();
        assert!(out.is_empty());
        v.on_message(&spot(1, None, Some(108_514), 1_700_000_000_124), 6, &mut |e| out.push(e)).unwrap();
        // Ask-only update reuses the remembered bid.
        v.on_message(&spot(1, None, Some(108_515), 1_700_000_000_125), 7, &mut |e| out.push(e)).unwrap();
        v.on_message(&spot(9001, Some(23_412_000), Some(23_415_000), 0), 8, &mut |e| out.push(e)).unwrap();
        assert_eq!(out.len(), 3);
        assert_eq!((out[0].bid, out[0].ask), (Px::from_f64(1.08512), Px::from_f64(1.08514)));
        assert_eq!((out[1].bid, out[1].ask), (Px::from_f64(1.08512), Px::from_f64(1.08515)));
        assert_eq!((out[0].exch_ns, out[0].recv_ns, out[0].venue), (1_700_000_000_124_000_000, 6, venue::CTRADER));
        assert_eq!((out[2].instrument, out[2].bid), (InstrumentId(2), Px::from_f64(234.12)));
        assert!(out.windows(2).all(|w| w[1].seq > w[0].seq));
        assert!(matches!(v.on_message(&spot(5, Some(1), Some(2), 0), 0, &mut |_| {}), Err(VenueError::Skip(_))));
    }

    #[test]
    fn fixed_account_skips_listing() {
        let mut v = venue(Some(42));
        v.on_connect();
        let (k, f) = unwrap(v.on_message(&wrap(pt::APP_AUTH_RES, &[]), 0, &mut |_| {}).unwrap());
        assert_eq!((k, f[0].1.i64()), (pt::ACCOUNT_AUTH_REQ, Some(42)));
    }

    #[test]
    fn unknown_symbol_is_fatal_with_suggestions() {
        let mut v = Ctrader::new(
            CtraderEnv::Demo,
            SymbolTable::new(&["AAPL"]).unwrap(),
            Secret::new("a".into()),
            Secret::new("b".into()),
            Secret::new("c".into()),
            Some(77),
        );
        v.on_connect();
        let r = v.on_message(&wrap(pt::SYMBOLS_LIST_RES, &symbols_res()), 0, &mut |_| {});
        let Err(VenueError::Fatal(m)) = r else { panic!("{r:?}") };
        assert!(m.contains("aapl.us") && m.contains("5 symbols"), "{m}");
    }

    #[test]
    fn errors_are_fatal_and_secrets_not_debug_printed() {
        let mut v = venue(None);
        let e = Writer::new().str(3, "CH_CLIENT_AUTH_FAILURE").str(4, "bad secret").finish();
        let r = v.on_message(&wrap(pt::OA_ERROR_RES, &e), 0, &mut |_| {});
        assert!(matches!(r, Err(VenueError::Fatal(ref m)) if m.contains("CH_CLIENT_AUTH_FAILURE")));
        let dbg = format!("{v:?}");
        assert!(!dbg.contains("csecret") && !dbg.contains("tok"), "{dbg}");
    }

    #[test]
    fn server_heartbeat_is_not_counted_as_skip() {
        let mut v = venue(None);
        assert!(v.on_message(&wrap(pt::HEARTBEAT, &[]), 0, &mut |_| {}).unwrap().is_none());
    }

    #[test]
    fn heartbeat_every_10s() {
        let mut v = venue(None);
        assert!(v.on_tick(HEARTBEAT_NS).is_some());
        assert!(v.on_tick(HEARTBEAT_NS + 1).is_none());
        let (k, _) = unwrap(v.on_tick(2 * HEARTBEAT_NS));
        assert_eq!(k, pt::HEARTBEAT);
    }
}
