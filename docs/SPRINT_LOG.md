# Sprint Log

## Sprint 1 — Architecture & Memory Layout (2026-10-28)

### Shipped
- Cargo workspace, 7 crates, release profile (fat LTO, cgu=1, panic=abort), aarch64 Neoverse-N1 target flags.
- `dd-core`: 64 B cache-aligned wire types (compile-time asserted), SPSC ring, `CachePadded`, wait strategies, affinity, clock, SplitMix64.
- `dd-features`: PIT `Ewma` surprise z-score, `DecayedSurprise` (time-based half-life).
- `dd-strategy`: `Strategy` trait, `ZScoreReversion` baseline.
- `dd-risk`: full guardrail chain (FR-4) + incremental portfolio accounting.
- `dd-exec`: `Broker` trait with `Exec::{Fill, Cancelled}`, order templates with session-prefixed ids, `PaperBroker`.
- `dd-feed`: deterministic regime-switching synthetic feed for `--demo`.
- `dd-engine`: `deltadrift run --demo` with four pinned stages, lossless cascading shutdown, HDR latency histograms, $100-normalised report, `--json`.
- Reference config `config/default.toml` (unknown keys rejected).
- Tests: 21 (ring ordering/wrap/drop/close, affinity regression, PIT estimators, risk guardrails, accounting incl. position flip, paper broker). Clippy clean.

### Measurements
Hardware: dev box, x86_64, 12 logical cores, **not** isolated (desktop + other apps running), `wait=spin`, pinned cores 1/2/3.

| Scenario | Command | Result |
|---|---|---|
| Ring throughput (64 B msgs, cross-core) | `cargo bench -p dd-core --bench ring` | **133 M msg/s** |
| Ring round trip (ping-pong) | same | **156 ns** RTT |
| Pipeline, paced 200 k ev/s | `deltadrift run --demo -d 5 -r 200000 --wait spin` | tick→decision p50 **365 ns**, p90 432 ns, p99 18.5 µs, p99.9 68 µs; tick→order p50 635 ns; 0 dropped |
| Pipeline, unthrottled | `deltadrift run --demo -d 5 -r 0 --wait spin` | **~8.0 M ev/s** sustained, p50 294 ns, 0 dropped |

NFR-1 (< 50 ms) met by >3 orders of magnitude at p99.9. Tails (p99+) are OS jitter on
non-isolated cores; expect improvement with `isolcpus`/`nohz_full` on the OCI host.

### Bugs found & root causes
1. **All stages unpinned, ~6–11 ms latency** — `get_core_ids()` returns the caller's mask; children inherited main's `{0}` pin. Fixed via ADR-0004. 1000× latency improvement.
2. **Batch `consume` under-filled** — consumer refreshed the producer cursor only when its cached view was empty, so batches stopped at the stale tail. Now refreshes when cached count < requested batch.
3. **Unfilled IOC leaked in-flight qty** — initial design reported misses as zero-qty fills. Replaced with explicit `Exec::Cancelled { unfilled }` → `RiskEngine::on_cancel`.
4. **`stop_outs` over-counted** while a flatten was rate-limited; now counts triggers only.

### Open items / carried forward
- Baseline strategy is a pipeline exerciser, not alpha. Demo P&L is noise; do not read into it.
- Rate limiting also throttles stop-loss flattens (deliberate, conservative); revisit a separate "reduce-only" budget in Sprint 4.
- Loom model test for the ring (ADR-0002 TODO).
- No ARM64 measurement yet: needs OCI A1 host or `aarch64` cross toolchain + qemu (qemu timings not representative).
- Daily roll (`roll_day`) not yet scheduled by the engine; needs session calendar (Sprint 4/6).
- Integer cash ledger before LIVE (ADR-0005).

### Next: Sprint 2 — Zero-copy networking
io_uring WebSocket ingestion, zero-copy/SBE-style decoding into `MarketEvent`, symbol→`InstrumentId` mapping on the cold path, gap detection via `seq`.
