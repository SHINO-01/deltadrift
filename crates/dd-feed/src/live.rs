//! Reconnecting live feed runner. Runs on the feed core.
//!
//! Hot loop: `WsClient::poll` → `Venue::on_message` → stale-update filter →
//! `emit(MarketEvent)`. Connection loss triggers exponential backoff
//! (100 ms → 5 s) and a fresh session (auth/subscribe replayed by the venue).

use std::time::Duration;

use dd_core::wait::Wait;
use dd_core::{MAX_INSTRUMENTS, MarketEvent, clock};
use dd_net::{ClientOptions, NetError, WsClient};
use hdrhistogram::Histogram;

use crate::venues::{Venue, VenueError};

#[derive(Debug, Clone)]
pub struct FeedStats {
    /// Socket read → event emitted (TLS decrypt + framing + decode), ns.
    pub wire_to_event: Histogram<u64>,
    pub connects: u64,
    pub disconnects: u64,
    pub messages: u64,
    pub events: u64,
    pub skipped: u64,
    /// Updates older than one already seen for the instrument (dropped).
    pub stale: u64,
    pub pings: u64,
    pub bytes_in: u64,
    pub last_error: Option<String>,
    /// Set if the feed stopped because of an unrecoverable error.
    pub fatal: Option<String>,
}

impl Default for FeedStats {
    fn default() -> Self {
        Self {
            wire_to_event: Histogram::new_with_bounds(1, 10_000_000_000, 3).expect("valid bounds"),
            connects: 0,
            disconnects: 0,
            messages: 0,
            events: 0,
            skipped: 0,
            stale: 0,
            pings: 0,
            bytes_in: 0,
            last_error: None,
            fatal: None,
        }
    }
}

const BACKOFF_MIN: Duration = Duration::from_millis(100);
const BACKOFF_MAX: Duration = Duration::from_secs(5);

/// Runs until `clock::now_ns() >= deadline_ns` or a fatal error.
pub fn run<V: Venue, W: Wait, E: FnMut(MarketEvent)>(
    venue: &mut V,
    opts: &ClientOptions,
    deadline_ns: u64,
    idle: &mut W,
    mut emit: E,
) -> FeedStats {
    let mut st = FeedStats::default();
    let mut last_seq = vec![0u64; MAX_INSTRUMENTS].into_boxed_slice();
    let mut backoff = BACKOFF_MIN;
    let url = venue.url();

    'session: while clock::now_ns() < deadline_ns {
        let mut client = match WsClient::connect(&url, opts) {
            Ok(c) => c,
            Err(e) => {
                eprintln!("[feed:{}] connect failed: {e}; retrying in {backoff:?}", V::NAME);
                st.last_error = Some(e.to_string());
                std::thread::sleep(backoff);
                backoff = (backoff * 2).min(BACKOFF_MAX);
                continue;
            }
        };
        st.connects += 1;
        backoff = BACKOFF_MIN;
        eprintln!("[feed:{}] connected", V::NAME);
        for m in venue.on_connect() {
            if let Err(e) = client.send_text(&m) {
                st.last_error = Some(e.to_string());
                st.disconnects += 1;
                continue 'session;
            }
        }

        let mut reply: Option<String> = None;
        let mut fatal: Option<String> = None;
        let net_before = client.stats();
        let result = loop {
            if clock::now_ns() >= deadline_ns {
                break Ok(());
            }
            let r = client.poll(|msg, recv_ns| {
                st.messages += 1;
                let mut on_event = |e: MarketEvent| {
                    let slot = &mut last_seq[e.instrument.idx()];
                    if e.seq != 0 && e.seq <= *slot {
                        st.stale += 1;
                        return;
                    }
                    *slot = e.seq;
                    st.events += 1;
                    st.wire_to_event.saturating_record(clock::now_ns().saturating_sub(e.recv_ns));
                    emit(e);
                };
                match venue.on_message(msg, recv_ns, &mut on_event) {
                    Ok(None) => {}
                    Ok(Some(r)) => reply = Some(r),
                    Err(VenueError::Skip(_)) => st.skipped += 1,
                    Err(VenueError::Fatal(f)) => fatal = Some(f),
                }
            });
            match r {
                Ok(0) => idle.idle(),
                Ok(_) => idle.reset(),
                Err(e) => break Err(e),
            }
            if let Some(f) = fatal.take() {
                eprintln!("[feed:{}] {f}", V::NAME);
                st.fatal = Some(f);
                break Ok(());
            }
            if let Some(m) = reply.take()
                && let Err(e) = client.send_text(&m)
            {
                break Err(e);
            }
        };
        let net = client.stats();
        st.bytes_in += net.bytes_in - net_before.bytes_in;
        st.pings += net.pings - net_before.pings;
        match result {
            Ok(()) => {
                client.close();
                if st.fatal.is_some() {
                    break;
                }
            }
            Err(e) => {
                st.disconnects += 1;
                if !matches!(e, NetError::Closed { .. }) || clock::now_ns() < deadline_ns {
                    eprintln!("[feed:{}] disconnected: {e}; reconnecting", V::NAME);
                }
                st.last_error = Some(e.to_string());
                // Instruments may resync with lower ids after reconnect (e.g. venue restarts).
                last_seq.fill(0);
            }
        }
    }
    st
}
