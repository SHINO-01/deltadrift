//! Cross-core SPSC throughput and round-trip latency.
//! Run: `cargo bench -p dd-core`

use std::hint::black_box;
use std::time::{Duration, Instant};

use criterion::{Criterion, Throughput, criterion_group, criterion_main};
use dd_core::wait::BusySpin;
use dd_core::{MarketEvent, ring};

fn throughput(c: &mut Criterion) {
    let mut g = c.benchmark_group("spsc");
    const BATCH: u64 = 1 << 20;
    g.throughput(Throughput::Elements(BATCH));
    g.bench_function("throughput_64B_msgs", |b| {
        b.iter_custom(|iters| {
            let mut total = Duration::ZERO;
            for _ in 0..iters {
                let (mut p, mut c) = ring::<MarketEvent, 4096>();
                let consumer = std::thread::spawn(move || {
                    dd_core::affinity::pin_current(2);
                    let mut n = 0u64;
                    while n < BATCH {
                        n += c.consume(256, |e| {
                            black_box(e);
                        }) as u64;
                    }
                });
                dd_core::affinity::pin_current(1);
                let start = Instant::now();
                let mut w = BusySpin;
                for i in 0..BATCH {
                    let mut e = MarketEvent::default();
                    e.seq = i;
                    p.push_with(e, &mut w);
                }
                consumer.join().unwrap();
                total += start.elapsed();
            }
            total
        });
    });

    g.throughput(Throughput::Elements(1));
    g.bench_function("round_trip_ping_pong", |b| {
        let (mut ping_tx, mut ping_rx) = ring::<u64, 64>();
        let (mut pong_tx, mut pong_rx) = ring::<u64, 64>();
        let echo = std::thread::spawn(move || {
            dd_core::affinity::pin_current(2);
            let mut w = BusySpin;
            loop {
                match ping_rx.pop() {
                    Some(u64::MAX) => return,
                    Some(v) => pong_tx.push_with(v, &mut w),
                    None => core::hint::spin_loop(),
                }
            }
        });
        dd_core::affinity::pin_current(1);
        let mut w = BusySpin;
        b.iter(|| {
            ping_tx.push_with(1, &mut w);
            loop {
                if let Some(v) = pong_rx.pop() {
                    break black_box(v);
                }
                core::hint::spin_loop();
            }
        });
        ping_tx.push_with(u64::MAX, &mut w);
        echo.join().unwrap();
    });
    g.finish();
}

criterion_group!(benches, throughput);
criterion_main!(benches);
