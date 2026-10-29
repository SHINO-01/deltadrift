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

## Sprint 2 — Zero-copy networking (2026-10-28 → 2026-10-29)

### Shipped
- **`dd-net`**: sans-IO TLS (rustls *unbuffered*, decrypt in place) + own RFC 6455 client codec
  over pluggable drivers: `std`, `uring`, `sqpoll` (ADR-0006, ADR-0007). `ws_probe` dev example.
- **`dd-feed`**: reconnecting `live::run` (backoff, session replay, stale-update filter, stats +
  wire→event histogram); `Binance` bookTicker and `Alpaca` crypto/stocks venues; `SymbolTable`
  (packed-u128 open addressing); allocation-free RFC 3339 parser. Synthetic feed moved to its own module.
- **`dd-core`**: exact decimal → fixed-point parser; `MarketEvent` now carries bid/ask sizes + venue;
  `affinity::plan_cores` topology-aware planning (ADR-0010).
- **`dd-secrets`**: `.env` loading, redacting/zeroizing `Secret`; `.env.example`; hardened `.gitignore` (ADR-0008).
- **Engine**: `--live binance|alpaca-crypto|alpaca-stocks`, `--symbols`, `--io std|uring|sqpoll`,
  `--env-file`; io_uring probe + fallback; feed stats in text/JSON report; cores auto by default.
- Tests: 43 total (+22), incl. full-stack loopback (TCP+TLS+WS) on all three backends. Clippy clean.

### Measurements (dev box i5-13420H, not isolated; live = Binance, 6 symbols, `--wait spin`)
| What | Result |
|---|---|
| Binance bookTicker decode | 297 ns/msg |
| Alpaca quote decode | 275 ns/msg |
| Symbol lookup | 9.9 ns |
| Live, std, auto cores (2/4/6) | wire→event p50 **4.8 µs**, p99 40 µs; tick→decision p50 5.5 µs; 0 dropped, pings answered |
| Live, read→WS message only (`ws_probe`, 4 symbols) | p50 1.6 µs, p90 7.7 µs; ~5.6 messages per socket read |
| Synthetic 200k ev/s after topology fix | p50 484 ns, **p99 1.8 µs** (was 18.5 µs), p99.9 22 µs (was 68 µs) |

Analysis: live latency is dominated by **intra-batch queueing** inside the feed stage
(several TLS records/messages per read; each waits for those ahead of it), not by syscalls
or the strategy/risk hops (~0.7 µs). std / uring / sqpoll were indistinguishable at
~200–340 msg/s (ADR-0007).

### Bugs found & root causes
1. **Stages pinned to SMT siblings** — default `[0,1,2,3]` = 2 physical cores on the dev box. Fixed by auto planning (ADR-0010).
2. Two loopback-test bugs (double "subscribe" trigger; wrong expected reassembly string). The stack was correct both times.
3. Draft feed-stage design routed every event through `Box<dyn FnMut>`; replaced with monomorphised per-venue loops before merge.

### Open items / carried forward
- **Interleave decrypt→deliver per TLS record** (today all records in a read are decrypted before any message is delivered). Needs a deterministic replay benchmark first (loopback server replaying captured bursts).
- Alpaca not yet live-verified (needs keys in `.env`); fixture-tested.
- SBE decoding (Binance SBE needs API key) — ADR-0009. IBKR adapter → Sprint 4 (not WebSocket). FRED → Sprint 3 (REST, cold path).
- NIC/kernel receive timestamps (SO_TIMESTAMPING) for a truer `recv_ns`.
- Strategy params are tuned for the synthetic feed; live demo P&L is meaningless.
- Still open from Sprint 1: loom test for the ring, ARM64 measurements, `roll_day` scheduling, integer cash ledger.

### Next: Sprint 3 — SIMD feature engine + PIT store
NEON/SIMD batch z-scores and decay, point-in-time feature store (TimescaleDB per spec — revisit
vs. embedded store given the 4-core free-tier budget), FRED macro ingestion on the cold path.

## Sprint 2.5 — Free data vendor (2026-10-29)

Owner constraint: all data must be free (Alpaca full real-time stock data costs $1,089/yr).

### Shipped
- Research note [research/2026-10-29-free-market-data-sources.md](research/2026-10-29-free-market-data-sources.md) (FX / crypto / stocks, provenance-tagged).
- **cTrader Open API** adapter (ADR-0011), now the default live venue: full session state machine, account
  auto-selection, symbol resolution with suggestions, partial-spot merging, heartbeats, exact price conversion.
- `dd_feed::proto`: zero-alloc proto2 reader + writer. `dd-net`: `send_binary`.
- `Venue` trait: `Outbound::{Text, Binary}`, `on_tick` hook, per-venue `MAX_MESSAGE`.
- `ctrader_version` example: credential-free live connectivity check.
- Tests: 51 total (+8). Clippy clean.

### Verified
- Live: `wss://demo.ctraderapi.com:5035` → `ProtoOAVersionRes` "102" over our TLS/WS/protobuf stack.
- Session flow, partial updates, errors, heartbeats: fixture tests built from Spotware's `.proto` field numbers.
- **Not yet verified live:** authenticated session + spot stream (needs owner's demo credentials in `.env`).

### Open items
- Live-verify with credentials; record symbol names for FX/crypto/stocks on the chosen broker.
- Token refresh with secure persistence of rotated refresh tokens.
- Spot stream has no sizes; consider depth (`ProtoOASubscribeDepthQuotesReq`) if strategies need size.

### 2026-10-29 — Owner decisions
- Goal set: live-money automated income stream → [LIVE_READINESS.md](LIVE_READINESS.md) gates G0–G4.
- ADR-0012 **accepted**: S3 = backtest + cost model, S4 = cTrader execution + live blockers,
  S5 = strategy research, S6 = paper run + monitoring.
- Jurisdiction **Australia**: ASIC leverage caps → blocker L12; AU-FY tax ledger → blocker L14.
