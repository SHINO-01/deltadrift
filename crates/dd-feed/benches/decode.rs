//! Per-message decode cost. Run: `cargo bench -p dd-feed --bench decode`

use std::hint::black_box;

use criterion::{Criterion, criterion_group, criterion_main};
use dd_core::decimal::parse_fixed8;
use dd_feed::SymbolTable;
use dd_feed::venues::Venue;
use dd_feed::venues::alpaca::{Alpaca, AlpacaFeed};
use dd_feed::venues::binance::Binance;
use dd_secrets::Secret;

const BINANCE: &[u8] = br#"{"stream":"btcusdt@bookTicker","data":{"u":101994177945,"s":"BTCUSDT","b":"80739.42000000","B":"4.57716000","a":"80739.43000000","A":"2.46635000"}}"#;
const ALPACA: &[u8] = br#"[{"T":"q","S":"BTC/USD","bp":80739.42,"bs":0.5,"ap":80739.43,"as":0.25,"t":"2024-03-12T10:27:48.858228144Z"}]"#;

fn bench(c: &mut Criterion) {
    let syms: Vec<String> = ["ETHUSDT", "BTCUSDT", "SOLUSDT", "BNBUSDT", "XRPUSDT"].map(String::from).to_vec();
    let table = SymbolTable::new(&syms).unwrap();
    c.bench_function("symbol_lookup", |b| b.iter(|| table.lookup(black_box(b"BTCUSDT"))));
    c.bench_function("parse_fixed8", |b| b.iter(|| parse_fixed8(black_box(b"80739.42000000"))));

    let mut bn = Binance::new(table.clone());
    c.bench_function("binance_bookticker_decode", |b| {
        b.iter(|| {
            let mut out = 0u64;
            bn.on_message(black_box(BINANCE), 1, &mut |e| out += e.seq).unwrap();
            out
        })
    });

    let mut al = Alpaca::new(
        AlpacaFeed::Crypto,
        SymbolTable::new(&["BTC/USD"]).unwrap(),
        Secret::new("k".into()),
        Secret::new("s".into()),
    );
    c.bench_function("alpaca_quote_decode", |b| {
        b.iter(|| {
            let mut out = 0u64;
            let _ = al.on_message(black_box(ALPACA), 1, &mut |e| out += e.exch_ns);
            out
        })
    });
}

criterion_group!(benches, bench);
criterion_main!(benches);
