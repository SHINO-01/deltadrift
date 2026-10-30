//! Feeds one event stream to many simulations (e.g. a parameter grid) in a
//! single pass over the data. Events are batched; within a batch, simulations
//! are partitioned across threads. Each simulation sees every event in the
//! same order, so results are identical to running them one by one.

use dd_core::MarketEvent;
use dd_strategy::Strategy;

use crate::sim::Simulation;

const BATCH: usize = 1 << 15;

/// Returns the number of events replayed.
pub fn run<S, I, E>(source: I, sims: &mut [Simulation<S>], threads: usize) -> Result<u64, E>
where
    S: Strategy + Send,
    I: Iterator<Item = Result<MarketEvent, E>>,
{
    let threads = threads.clamp(1, sims.len().max(1));
    let mut batch: Vec<MarketEvent> = Vec::with_capacity(BATCH);
    let mut total = 0u64;
    let mut source = source.peekable();
    loop {
        batch.clear();
        while batch.len() < BATCH {
            match source.next() {
                Some(Ok(e)) => batch.push(e),
                Some(Err(e)) => return Err(e),
                None => break,
            }
        }
        if batch.is_empty() {
            return Ok(total);
        }
        total += batch.len() as u64;
        if threads == 1 {
            for s in sims.iter_mut() {
                batch.iter().for_each(|e| s.on_event(e));
            }
        } else {
            let chunk = sims.len().div_ceil(threads);
            let b = &batch;
            std::thread::scope(|sc| {
                for part in sims.chunks_mut(chunk) {
                    sc.spawn(move || {
                        for s in part {
                            b.iter().for_each(|e| s.on_event(e));
                        }
                    });
                }
            });
        }
        if source.peek().is_none() {
            return Ok(total);
        }
    }
}
