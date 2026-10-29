//! Venue protocol adapters: URL, session control flow (auth/subscribe), and
//! message decoding into [`MarketEvent`]s.

pub mod alpaca;
pub mod binance;
pub mod ctrader;

use std::fmt;

use dd_core::MarketEvent;

#[derive(Debug)]
pub enum VenueError {
    /// Malformed or unexpected message; count it and continue.
    Skip(&'static str),
    /// Unrecoverable session error (e.g. authentication rejected); stop the feed.
    Fatal(String),
}

impl fmt::Display for VenueError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Skip(m) => write!(f, "skipped message: {m}"),
            Self::Fatal(m) => write!(f, "fatal venue error: {m}"),
        }
    }
}

/// A control message for the venue (cold path).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outbound {
    Text(String),
    Binary(Vec<u8>),
}

/// A market-data venue. Methods returning `String`s are cold path (session
/// control); [`on_message`](Venue::on_message) is hot path and must not
/// allocate when it returns `Ok(None)`.
pub trait Venue {
    /// One of [`dd_core::venue`].
    const ID: u8;
    const NAME: &'static str;

    fn url(&self) -> String;

    /// Largest message the venue can send (e.g. full symbol lists).
    const MAX_MESSAGE: usize = 1 << 20;

    /// Called after every (re)connect; messages to send immediately.
    fn on_connect(&mut self) -> Vec<Outbound>;

    /// Decodes one WebSocket message, emitting zero or more events. May return
    /// a control message to send (e.g. subscribe after auth succeeds).
    fn on_message<E: FnMut(MarketEvent)>(
        &mut self,
        msg: &[u8],
        recv_ns: u64,
        emit: &mut E,
    ) -> Result<Option<Outbound>, VenueError>;

    /// Called on every loop iteration of the feed core with the current
    /// monotonic time; used for client heartbeats. Must be cheap.
    #[inline(always)]
    fn on_tick(&mut self, _now_ns: u64) -> Option<Outbound> {
        None
    }
}
