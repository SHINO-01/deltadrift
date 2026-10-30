//! Time-ordered replay of cached historical ticks.
//!
//! Each instrument gets a background thread that decodes one UTC day at a time
//! (24 hour-files) into a bounded channel, so LZMA decoding overlaps with the
//! simulation. A k-way merge on `(timestamp, instrument index)` yields a single
//! deterministic stream: identical inputs always replay in identical order.

use std::cmp::Reverse;
use std::collections::BinaryHeap;
use std::path::PathBuf;
use std::sync::mpsc::{Receiver, sync_channel};

use dd_core::{InstrumentId, MarketEvent};

use crate::InstrumentSpec;
use crate::dukascopy::{self, DataError, RawTick};

const NS_PER_HOUR: i64 = 3_600_000_000_000;
/// Days decoded ahead per instrument.
const PREFETCH_DAYS: usize = 3;

struct Cursor {
    spec: InstrumentSpec,
    id: InstrumentId,
    rx: Receiver<Result<Vec<RawTick>, DataError>>,
    buf: Vec<RawTick>,
    pos: usize,
    done: bool,
}

impl Cursor {
    /// Ensures `buf[pos]` is valid; returns false at end of data.
    fn fill(&mut self) -> Result<bool, DataError> {
        while self.pos >= self.buf.len() {
            if self.done {
                return Ok(false);
            }
            match self.rx.recv() {
                Ok(Ok(day)) => {
                    self.buf = day;
                    self.pos = 0;
                }
                Ok(Err(e)) => return Err(e),
                Err(_) => self.done = true,
            }
        }
        Ok(true)
    }
}

/// Replays `[from_ns, to_ns)` for several instruments from the Dukascopy cache.
pub struct HistSource {
    cursors: Vec<Cursor>,
    heap: BinaryHeap<Reverse<(i64, u32)>>,
    seq: u64,
    started: bool,
}

impl std::fmt::Debug for HistSource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HistSource").field("instruments", &self.cursors.len()).field("seq", &self.seq).finish()
    }
}

impl HistSource {
    /// `specs[i]` is replayed as `InstrumentId(i)`.
    pub fn dukascopy(root: impl Into<PathBuf>, specs: &[InstrumentSpec], from_ns: i64, to_ns: i64) -> Self {
        let root = root.into();
        let cursors = specs
            .iter()
            .enumerate()
            .map(|(i, spec)| {
                let (tx, rx) = sync_channel(PREFETCH_DAYS);
                let (root, sym) = (root.clone(), spec.symbol.clone());
                std::thread::Builder::new()
                    .name(format!("hist-{sym}"))
                    .spawn(move || {
                        let first_hour = from_ns.div_euclid(NS_PER_HOUR);
                        let last_hour = (to_ns - 1).div_euclid(NS_PER_HOUR);
                        let mut h = first_hour;
                        while h <= last_hour {
                            let day_end = ((h.div_euclid(24) + 1) * 24).min(last_hour + 1);
                            let mut day = Vec::with_capacity(200_000);
                            let mut res = Ok(());
                            while h < day_end {
                                if let Err(e) = dukascopy::load_hour(&root, &sym, h, &mut day) {
                                    res = Err(match e {
                                        DataError::Io(io) if io.kind() == std::io::ErrorKind::NotFound => DataError::Http(format!(
                                            "missing {} — run `deltadrift data fetch` for this range first",
                                            dukascopy::path(&root, &sym, h).display()
                                        )),
                                        e => e,
                                    });
                                    break;
                                }
                                h += 1;
                            }
                            day.retain(|t| t.ts_ns >= from_ns && t.ts_ns < to_ns);
                            let stop = res.is_err();
                            if tx.send(res.map(|()| day)).is_err() || stop {
                                return;
                            }
                        }
                    })
                    .expect("spawn decoder thread");
                Cursor { spec: spec.clone(), id: InstrumentId(i as u32), rx, buf: Vec::new(), pos: 0, done: false }
            })
            .collect();
        Self { cursors, heap: BinaryHeap::new(), seq: 0, started: false }
    }
}

impl Iterator for HistSource {
    type Item = Result<MarketEvent, DataError>;

    #[inline]
    fn next(&mut self) -> Option<Self::Item> {
        if !self.started {
            self.started = true;
            for (i, c) in self.cursors.iter_mut().enumerate() {
                match c.fill() {
                    Ok(true) => self.heap.push(Reverse((c.buf[c.pos].ts_ns, i as u32))),
                    Ok(false) => {}
                    Err(e) => return Some(Err(e)),
                }
            }
        }
        let Reverse((_, i)) = self.heap.pop()?;
        let c = &mut self.cursors[i as usize];
        let t = c.buf[c.pos];
        c.pos += 1;
        self.seq += 1;
        let ev = dukascopy::to_event(&t, &c.spec, c.id, self.seq);
        match c.fill() {
            Ok(true) => self.heap.push(Reverse((c.buf[c.pos].ts_ns, i))),
            Ok(false) => {}
            Err(e) => return Some(Err(e)),
        }
        Some(Ok(ev))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use dd_core::calendar::days_from_civil;

    fn write_hour(root: &std::path::Path, sym: &str, hour: i64, ms: &[u32], px: u32) {
        let mut raw = Vec::new();
        for &m in ms {
            for x in [m, px + 1, px, 1f32.to_bits(), 1f32.to_bits()] {
                raw.extend_from_slice(&x.to_be_bytes());
            }
        }
        let mut out = Vec::new();
        lzma_rs::lzma_compress(&mut std::io::Cursor::new(raw), &mut out).unwrap();
        let p = dukascopy::path(root, sym, hour);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, out).unwrap();
    }

    #[test]
    fn merges_instruments_in_time_order_and_respects_range() {
        let root = std::env::temp_dir().join(format!("dd-hist-test-{}", std::process::id()));
        let h0 = days_from_civil(2024, 1, 3) * 24 + 13;
        for h in [h0, h0 + 1] {
            write_hour(&root, "EURUSD", h, &[10, 20, 30], 108_000);
            write_hour(&root, "GBPUSD", h, &[15, 20, 40], 127_000);
        }
        let specs = [InstrumentSpec::dukascopy("EURUSD"), InstrumentSpec::dukascopy("GBPUSD")];
        let from = h0 * NS_PER_HOUR + 15_000_000; // skips EURUSD@10ms
        let to = (h0 + 2) * NS_PER_HOUR;
        let evs: Vec<_> = HistSource::dukascopy(&root, &specs, from, to).map(Result::unwrap).collect();
        assert_eq!(evs.len(), 11);
        assert!(evs.windows(2).all(|w| (w[0].recv_ns, w[0].instrument.0) <= (w[1].recv_ns, w[1].instrument.0)));
        assert_eq!((evs[0].instrument, evs[0].recv_ns as i64), (InstrumentId(1), from)); // GBPUSD@15ms
        // Equal timestamps → instrument order (EURUSD before GBPUSD at 20ms).
        assert_eq!((evs[1].instrument, evs[2].instrument), (InstrumentId(0), InstrumentId(1)));

        let missing = HistSource::dukascopy(&root, &specs, from, (h0 + 5) * NS_PER_HOUR).find_map(Result::err);
        assert!(missing.unwrap().to_string().contains("data fetch"));
        std::fs::remove_dir_all(root).ok();
    }
}
