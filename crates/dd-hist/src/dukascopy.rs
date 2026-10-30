//! Dukascopy historical tick feed.
//!
//! `https://datafeed.dukascopy.com/datafeed/{SYM}/{YYYY}/{MM0}/{DD}/{HH}h_ticks.bi5`
//! where `MM0` is the **zero-based** month. Each file is LZMA ("alone" format)
//! of 20-byte big-endian records: `u32 ms-since-hour, u32 ask, u32 bid,
//! f32 ask_volume, f32 bid_volume` (volumes in millions). Empty files mean no ticks
//! (weekends/holidays). Prices are integers in units of `1/point`.

use std::io::Read;
use std::path::{Path, PathBuf};
use std::time::Duration;

use dd_core::calendar::civil_from_days;
use dd_core::{FX_SCALE, InstrumentId, MarketEvent, Px, Qty, venue};

use crate::InstrumentSpec;

pub const VENUE_ID: u8 = venue::SYNTHETIC; // replayed data; venue tag is informational
const NS_PER_HOUR: i64 = 3_600_000_000_000;
const REC: usize = 20;

/// Hour index (hours since Unix epoch) → (year, month 1..=12, day, hour).
pub fn hour_parts(hour: i64) -> (i64, u32, u32, u32) {
    let (y, m, d) = civil_from_days(hour.div_euclid(24));
    (y, m, d, hour.rem_euclid(24) as u32)
}

pub fn url(symbol: &str, hour: i64) -> String {
    let (y, m, d, h) = hour_parts(hour);
    format!("https://datafeed.dukascopy.com/datafeed/{symbol}/{y}/{:02}/{d:02}/{h:02}h_ticks.bi5", m - 1)
}

/// Local cache path (month is 1-based here for humans).
pub fn path(root: &Path, symbol: &str, hour: i64) -> PathBuf {
    let (y, m, d, h) = hour_parts(hour);
    root.join("dukascopy").join(symbol).join(format!("{y}")).join(format!("{m:02}")).join(format!("{d:02}")).join(format!("{h:02}h_ticks.bi5"))
}

/// One decoded tick, prices already in the source's integer points.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct RawTick {
    pub ts_ns: i64,
    pub ask: u32,
    pub bid: u32,
    pub ask_vol: f32,
    pub bid_vol: f32,
}

#[derive(Debug)]
pub enum DataError {
    Io(std::io::Error),
    Lzma(String),
    Corrupt(&'static str),
    Http(String),
}

impl std::fmt::Display for DataError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(e) => write!(f, "io: {e}"),
            Self::Lzma(e) => write!(f, "lzma: {e}"),
            Self::Corrupt(m) => write!(f, "corrupt data: {m}"),
            Self::Http(e) => write!(f, "http: {e}"),
        }
    }
}

impl std::error::Error for DataError {}

impl From<std::io::Error> for DataError {
    fn from(e: std::io::Error) -> Self {
        Self::Io(e)
    }
}

/// Decompresses and decodes one hour file. `hour` is hours since epoch.
pub fn decode_bi5(compressed: &[u8], hour: i64, out: &mut Vec<RawTick>) -> Result<(), DataError> {
    if compressed.is_empty() {
        return Ok(());
    }
    let mut raw = Vec::with_capacity(compressed.len() * 5);
    lzma_rs::lzma_decompress(&mut std::io::BufReader::new(compressed), &mut raw).map_err(|e| DataError::Lzma(e.to_string()))?;
    if raw.len() % REC != 0 {
        return Err(DataError::Corrupt("record stream not a multiple of 20 bytes"));
    }
    let base = hour * NS_PER_HOUR;
    let mut last_ms = 0u32;
    for r in raw.chunks_exact(REC) {
        let be = |i: usize| u32::from_be_bytes([r[i], r[i + 1], r[i + 2], r[i + 3]]);
        let ms = be(0);
        if ms >= 3_600_000 || ms < last_ms {
            return Err(DataError::Corrupt("tick time outside hour or not monotonic"));
        }
        last_ms = ms;
        out.push(RawTick {
            ts_ns: base + i64::from(ms) * 1_000_000,
            ask: be(4),
            bid: be(8),
            ask_vol: f32::from_bits(be(12)),
            bid_vol: f32::from_bits(be(16)),
        });
    }
    Ok(())
}

/// Converts a raw tick to a [`MarketEvent`]. Points → 1e-8 fixed point is exact
/// because `point` divides `FX_SCALE`. Volumes are f32 millions in the source;
/// they're rounded to whole units to strip f32 representation noise (0.9 → 899,999.976).
#[inline]
pub fn to_event(t: &RawTick, spec: &InstrumentSpec, instrument: InstrumentId, seq: u64) -> MarketEvent {
    let mult = FX_SCALE / i64::from(spec.point);
    let ts = t.ts_ns as u64;
    MarketEvent::quote(
        ts,
        ts,
        seq,
        instrument,
        VENUE_ID,
        Px(i64::from(t.bid) * mult),
        Px(i64::from(t.ask) * mult),
        Qty::from_f64((f64::from(t.bid_vol) * 1e6).round()),
        Qty::from_f64((f64::from(t.ask_vol) * 1e6).round()),
    )
}

/// Reads and decodes a cached hour; a missing file is an error (fetch first).
pub fn load_hour(root: &Path, symbol: &str, hour: i64, out: &mut Vec<RawTick>) -> Result<(), DataError> {
    let bytes = std::fs::read(path(root, symbol, hour))?;
    decode_bi5(&bytes, hour, out)
}

#[derive(Debug, Default, Clone, Copy)]
pub struct FetchStats {
    pub requested: u64,
    pub downloaded: u64,
    pub cached: u64,
    pub empty: u64,
    pub bytes: u64,
}

/// Downloads `[from_hour, to_hour)` into the cache, skipping files already
/// present (resumable). Polite by design: few workers, a pause between
/// requests, and a **shared cooldown** — any failure pauses *all* workers with
/// exponential backoff (10 s → 5 min) so a throttling server is never hammered.
/// Gives up after `MAX_CONSECUTIVE_FAILURES`. Writes atomically (temp + rename).
pub fn fetch(
    root: &Path,
    symbol: &str,
    from_hour: i64,
    to_hour: i64,
    concurrency: usize,
    progress: impl Fn(&FetchStats) + Sync,
) -> Result<FetchStats, DataError> {
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicI64, AtomicU32, Ordering};
    use std::time::Instant;

    const MAX_CONSECUTIVE_FAILURES: u32 = 20;
    const PAUSE: Duration = Duration::from_millis(250);

    let agent: ureq::Agent = ureq::Agent::config_builder().timeout_global(Some(Duration::from_secs(15))).build().into();
    let next = AtomicI64::new(from_hour);
    let stats = Mutex::new(FetchStats::default());
    let failures = AtomicU32::new(0);
    let cooldown_until = Mutex::new(Instant::now());
    let last_err = Mutex::new(String::new());
    // Hours that failed and must be retried before moving on.
    let retry: Mutex<Vec<i64>> = Mutex::new(Vec::new());

    std::thread::scope(|s| {
        for _ in 0..concurrency.max(1) {
            s.spawn(|| {
                loop {
                    if failures.load(Ordering::Relaxed) >= MAX_CONSECUTIVE_FAILURES {
                        return;
                    }
                    let wait = cooldown_until.lock().expect("poisoned").saturating_duration_since(Instant::now());
                    if !wait.is_zero() {
                        std::thread::sleep(wait);
                    }
                    let h = match retry.lock().expect("poisoned").pop() {
                        Some(h) => h,
                        None => {
                            let h = next.fetch_add(1, Ordering::Relaxed);
                            if h >= to_hour {
                                return;
                            }
                            h
                        }
                    };
                    let p = path(root, symbol, h);
                    let mut st = FetchStats { requested: 1, ..Default::default() };
                    if p.exists() {
                        st.cached = 1;
                    } else {
                        match get(&agent, &url(symbol, h)).and_then(|b| write_atomic(&p, &b).map(|()| b.len())) {
                            Ok(n) => {
                                failures.store(0, Ordering::Relaxed);
                                st.downloaded = 1;
                                st.bytes = n as u64;
                                st.empty = u64::from(n == 0);
                            }
                            Err(e) => {
                                let k = failures.fetch_add(1, Ordering::Relaxed) + 1;
                                let backoff = Duration::from_secs((10u64 << (k - 1).min(5)).min(300));
                                *cooldown_until.lock().expect("poisoned") = Instant::now() + backoff;
                                *last_err.lock().expect("poisoned") = e.to_string();
                                eprintln!("  {symbol}: request failed ({e}); all workers cooling down {backoff:?} [{k}/{MAX_CONSECUTIVE_FAILURES}]");
                                retry.lock().expect("poisoned").push(h);
                                continue;
                            }
                        }
                        std::thread::sleep(PAUSE);
                    }
                    let mut g = stats.lock().expect("poisoned");
                    g.requested += st.requested;
                    g.downloaded += st.downloaded;
                    g.cached += st.cached;
                    g.empty += st.empty;
                    g.bytes += st.bytes;
                    if g.requested % 200 == 0 {
                        progress(&g);
                    }
                }
            });
        }
    });
    if failures.load(Ordering::Relaxed) >= MAX_CONSECUTIVE_FAILURES {
        return Err(DataError::Http(format!(
            "giving up after {MAX_CONSECUTIVE_FAILURES} consecutive failures (last: {}); the server may be throttling — rerun later, completed hours are kept",
            last_err.into_inner().expect("poisoned")
        )));
    }
    Ok(stats.into_inner().expect("poisoned"))
}

fn get(agent: &ureq::Agent, url: &str) -> Result<Vec<u8>, DataError> {
    match agent.get(url).call() {
        Ok(mut r) => {
            let mut b = Vec::new();
            r.body_mut().as_reader().read_to_end(&mut b)?;
            Ok(b)
        }
        // Dukascopy answers 404 for hours that will never exist.
        Err(ureq::Error::StatusCode(404)) => Ok(Vec::new()),
        Err(e) => Err(DataError::Http(format!("{url}: {e}"))),
    }
}

fn write_atomic(p: &Path, bytes: &[u8]) -> Result<(), DataError> {
    if let Some(dir) = p.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let tmp = p.with_extension("bi5.part");
    std::fs::write(&tmp, bytes)?;
    std::fs::rename(&tmp, p)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use dd_core::calendar::days_from_civil;

    fn encode(ticks: &[(u32, u32, u32, f32, f32)]) -> Vec<u8> {
        let mut raw = Vec::new();
        for &(ms, a, b, av, bv) in ticks {
            for x in [ms, a, b, av.to_bits(), bv.to_bits()] {
                raw.extend_from_slice(&x.to_be_bytes());
            }
        }
        let mut out = Vec::new();
        lzma_rs::lzma_compress(&mut std::io::Cursor::new(raw), &mut out).unwrap();
        out
    }

    #[test]
    fn url_uses_zero_based_month() {
        let hour = days_from_civil(2024, 1, 3) * 24 + 13;
        assert_eq!(url("EURUSD", hour), "https://datafeed.dukascopy.com/datafeed/EURUSD/2024/00/03/13h_ticks.bi5");
        assert!(path(Path::new("data"), "EURUSD", hour).ends_with("dukascopy/EURUSD/2024/01/03/13h_ticks.bi5"));
    }

    #[test]
    fn decodes_and_converts_exactly() {
        let hour = days_from_civil(2024, 1, 3) * 24 + 13;
        let bi5 = encode(&[(95, 109_169, 109_168, 0.9, 0.9), (198, 109_169, 109_166, 0.9, 4.5)]);
        let mut out = vec![];
        decode_bi5(&bi5, hour, &mut out).unwrap();
        assert_eq!(out.len(), 2);
        assert_eq!(out[1].ts_ns, hour * NS_PER_HOUR + 198_000_000);
        let e = to_event(&out[0], &InstrumentSpec::dukascopy("EURUSD"), InstrumentId(0), 1);
        assert_eq!((e.bid, e.ask), (Px::from_f64(1.09168), Px::from_f64(1.09169)));
        assert_eq!(e.bid_qty, Qty::from_f64(900_000.0));
        let j = to_event(&RawTick { ts_ns: 0, ask: 142_938, bid: 142_934, ask_vol: 1.0, bid_vol: 1.0 }, &InstrumentSpec::dukascopy("USDJPY"), InstrumentId(1), 1);
        assert_eq!(j.bid, Px::from_f64(142.934));
    }

    #[test]
    fn rejects_corruption_and_accepts_empty() {
        let mut out = vec![];
        assert!(decode_bi5(&[], 0, &mut out).is_ok() && out.is_empty());
        assert!(decode_bi5(&encode(&[(5, 1, 1, 0.0, 0.0), (4, 1, 1, 0.0, 0.0)]), 0, &mut out).is_err());
        assert!(decode_bi5(b"not lzma", 0, &mut out).is_err());
    }
}
