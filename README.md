# DeltaDrift

Low-latency, self-improving (non-LLM) quantitative trading engine. Rust core, lock-free
core-pinned pipeline, hard risk guardrails independent of strategy code.

```bash
cargo build --release
./target/release/deltadrift run --demo -c config/default.toml -d 10   # offline demo, paper broker
./target/release/deltadrift run --demo -r 0 --wait spin --json          # max throughput, JSON report
cp .env.example .env   # add CTRADER_* (free demo account) — see .env.example for the 3 steps
./target/release/deltadrift run --live ctrader --symbols EURUSD,BTCUSD -d 60   # default free vendor
./target/release/deltadrift run --live binance --symbols BTCUSDT,ETHUSDT -d 60   # public crypto, no keys
cargo run --release -p dd-feed --example ctrader_version   # cTrader connectivity check, no keys
cargo test --release --workspace
cargo bench -p dd-core --bench ring
```

`--wait yield` is kinder to shared machines; use `spin` on isolated cores.
Live *market data* works (Binance public, Alpaca with keys); order execution stays PAPER
until live broker adapters land (Sprint 4). Secrets live only in a git-ignored `.env`.

Docs: [docs/README.md](docs/README.md) (architecture, ADRs, sprint log, requirement traceability).
