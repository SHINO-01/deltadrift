# DeltaDrift

Low-latency, self-improving (non-LLM) quantitative trading engine. Rust core, lock-free
core-pinned pipeline, hard risk guardrails independent of strategy code.

```bash
cargo build --release
./target/release/deltadrift run --demo -c config/default.toml -d 10   # offline demo, paper broker
./target/release/deltadrift run --demo -r 0 --wait spin --json          # max throughput, JSON report
cargo test --release --workspace
cargo bench -p dd-core --bench ring
```

`--wait yield` is kinder to shared machines; use `spin` on isolated cores.
LIVE mode and live feeds are refused until their sprints land.

Docs: [docs/README.md](docs/README.md) (architecture, ADRs, sprint log, requirement traceability).
