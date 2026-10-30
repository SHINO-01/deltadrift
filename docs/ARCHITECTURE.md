# Architecture (current state)

_Last updated: 2026-10-30, Sprint 3 in progress._

## Pipeline

```
ctrl   control / cold path (startup, config, secrets, reporting; optional io_uring SQ-poll thread)
feed   socket ─▶ TLS (decrypt in place) ─▶ WS frames ─▶ venue decoder ─[SPSC ring<MarketEvent, 65536>]──▶
strat  strategy ──[SPSC ring<Signal, 65536>]──▶
risk   risk + router ──▶ Broker (PaperBroker | live adapters from Sprint 4)
                    ◀── Exec reports (Fill | Cancelled), same core, synchronous poll

Core assignment: `engine.cores` or auto = one logical CPU per physical core, fastest first
(ADR-0010). On the dev box: ctrl 0, feed 2, strategy 4, risk 6. On OCI A1: 0,1,2,3.
```

- Each stage is one OS thread pinned to one core and busy-spins (`wait = "spin"`)
  or spin-then-yields (`wait = "yield"`, for shared machines). The wait policy is a
  generic parameter, so it's monomorphised: zero runtime dispatch.
- Stages own their state outright (moved into the thread, handed back on join);
  nothing is shared except the rings.
- **Shutdown is lossless and cascading**: the feed stops at the deadline and drops
  its `Producer`, which sets the ring's `closed` flag. Each consumer reads `closed`
  *before* attempting a drain; if the drain is empty and `closed` was already set,
  nothing can remain, so it exits and drops its own producer. Verified: `dropped 0`.

## Crates

| Crate | Role | `unsafe` |
|---|---|---|
| `dd-core` | Wire types, SPSC ring, `CachePadded`, wait strategies, affinity, clock, PRNG | ring only |
| `dd-features` | PIT incremental estimators: `Ewma` (surprise z), `DecayedSurprise` (time decay) | forbidden |
| `dd-strategy` | `Strategy` trait; `ZScoreReversion` baseline | forbidden |
| `dd-risk` | `RiskEngine` guardrails + `Portfolio` accounting. **No dep on dd-strategy.** | forbidden |
| `dd-exec` | `Broker` trait, `Exec` reports, `OrderTemplates`, `PaperBroker`, `TradingMode` | forbidden |
| `dd-net` | Sans-IO TLS (rustls unbuffered) + WebSocket client over std / io_uring drivers | buffers + io_uring |
| `dd-feed` | `SyntheticFeed`; live runner; venues `Ctrader` (default), `Binance`, `Alpaca`; proto2 codec; `SymbolTable`; RFC 3339 | forbidden |
| `dd-hist` | Historical data: Dukascopy `.bi5` decode + polite resumable fetcher; time-ordered multi-instrument `HistSource` | forbidden |
| `dd-backtest` | Simulated broker (latency, spread, slippage, commission, financing), `Simulation`, parallel `runner`, metrics (PSR/DSR), walk-forward | forbidden |
| `dd-secrets` | `.env` loading, redacting/zeroizing `Secret` | forbidden |
| `dd-engine` | `deltadrift` binary: config, pipeline wiring, report | forbidden |

## Wire types (`dd-core/src/types.rs`)

All `Copy`, `repr(C, align(64))`, exactly 64 B, compile-time asserted.
Prices/quantities are fixed-point `i64` at 1e-8 (`Px`, `Qty`). Instruments are dense
`u32` indices `< MAX_INSTRUMENTS (256)`; per-instrument state is a boxed array.

- `MarketEvent` — `recv_ns` (socket-read time; latency origin), `exch_ns`, `seq`, instrument, kind,
  venue, bid/ask, bid_qty/ask_qty. Trades encode `bid = ask = px`, `bid_qty = size` (`MarketEvent::trade`).
- `Signal` — `origin_ns`, instrument, strategy id, ref/bid/ask, **`mu`, `sigma2`** (a view, never a size)
- `Order` — pre-formatted template per instrument; `stamp()` writes id, origin, side, qty, limit
- `Fill` — execution report

## SPSC ring (`dd-core/src/ring.rs`)

Rigtorp/Disruptor style: power-of-two const capacity (mask indexing), monotonically
increasing `usize` cursors on separate 128 B-padded lines, each side caches the other's
cursor. `consume(max, f)` drains a batch and publishes the head once. Producer `Drop`
sets `closed`. Unconsumed items are dropped correctly. Measured: 133 M msg/s,
156 ns RTT (see SPRINT_LOG).

## Risk guardrails (`dd-risk`) — evaluation order per signal

1. Validate (finite `mu`/`sigma2`, `sigma2 > 0` if `mu ≠ 0`, positive px, bid ≤ ask) → `Reject(InvalidSignal)`
2. Mark-to-market (O(1) incremental equity + gross); track session max drawdown
3. **Daily drawdown** ≥ `max_daily_drawdown` → latch halt; target 0 everywhere until `roll_day()`
4. **Stop-loss** per position (unrealised loss > `stop_loss` × cost basis) → flatten + cooldown
5. **Fractional Kelly**: `target = kelly_fraction × mu/sigma2 × equity`, clamped to
   `max_position_notional`, then to remaining `max_gross_notional` room
6. Delta vs. position **+ in-flight** (prevents duplicate orders before fills arrive);
   suppressed below `min_order_notional` unless flattening
7. **Token-bucket rate limit** (integer ns math) — applies to risk-reducing orders too
8. Marketable limit at touch ± `limit_slippage_bps`

In-flight quantity is released by `on_fill` or `on_cancel` (`Exec::Cancelled`).

## Live market data (`dd-net` + `dd-feed`)

```
Io.poll_recv ─▶ RecvBuf (ciphertext, 256 KiB, raw alloc)
  ─▶ rustls UnbufferedClientConnection::process_tls_records  (decrypt in place, discard consumed)
  ─▶ ByteBuf (plaintext)  ─▶ ws::parse_frame (borrowed slices; ping→pong queued; fragments reassembled)
  ─▶ Venue::on_message(msg, recv_ns, emit)  (borrowed serde JSON; exact decimal → Px/Qty)
  ─▶ stale filter (seq ≤ last seen per instrument → dropped) ─▶ ring push
```

- `WsClient::connect` is cold path (DNS, TCP, TLS handshake, HTTP upgrade); messages that
  arrive with the upgrade response are retained. `poll` is non-blocking and allocation-free.
- Drivers (ADR-0007): `std` (non-blocking recv), `uring` (CQ polled from shared memory),
  `sqpoll` (kernel SQ thread on the control core). Probed at startup; falls back to std.
- `live::run` reconnects with exponential backoff (100 ms → 5 s), replays the venue session
  (`on_connect` + replies from `on_message`, e.g. Alpaca auth → subscribe), resets stale filters.
  `VenueError::Skip` is counted; `Fatal` (e.g. auth rejected) stops the feed.
- Venues (`dd_feed::venues::Venue`: `url`, `on_connect`, `on_message`, `on_tick` → `Outbound::{Text,Binary}`):
  - **cTrader Open API — default** (ADR-0011): binary protobuf over WS; session AppAuth →
    (account list →) AccountAuth → SymbolsList → SubscribeSpots; heartbeat every 10 s via `on_tick`;
    partial spot updates merged per symbol; 1e-5 price units ×1000 → `Px`; `IdMap` symbolId → instrument.
    Hand-rolled proto2 codec in `dd_feed::proto`.
  - **Binance** combined `bookTicker` (public; `u` as seq).
  - **Alpaca** crypto/stocks quotes (streaming array visitor; venue timestamps → `exch_ns`).
- `FeedStats.wire_to_event` histogram: socket read → event emitted.

## Backtesting (`dd-hist` + `dd-backtest`, ADR-0013)

```
HistSource (per-instrument decoder threads, k-way merge on (ts, instrument))
  ─▶ runner::run: batches of 32k events ─▶ N × Simulation (parallel across sims)
       Simulation.on_event: roll_to(t) [financing, day snapshot, risk.roll_day at 17:00 NY]
                            ─▶ SimBroker fills eligible orders (t ≥ decision + latency) ─▶ RiskEngine.on_fill
                            ─▶ Strategy.on_event ─▶ RiskEngine.on_signal ─▶ SimBroker.submit
  ─▶ SimResult (daily equity, net-of-cost closes, cost breakdown) ─▶ metrics ─▶ walk-forward ─▶ G1 gates
```

- `deltadrift data fetch --symbols EURUSD --from … --to …` (resumable, shared cooldown on failure).
- `deltadrift backtest --from … --to … [--json] [--synthetic]` prints OOS metrics and the G1 verdict.
  `--synthetic` validates the pipeline only and can never pass G1.
- USD-quoted symbols only until FX conversion (L15).

## Secrets

`.env` (git-ignored) → environment → `dd_secrets::Secret` (redacted, zeroized). Loaded once in
`main` before threads. `.env.example` lists names. See ADR-0008.

## Clock & pinning

- `clock::now_ns()` = `Instant` since a process epoch (vDSO, no syscall).
- `affinity::init()` snapshots allowed cores **once, before any pinning**: see ADR-0004
  for the bug this prevents. The control thread pins itself only after spawning stages.
- `affinity::plan_cores(n)` picks distinct physical cores, fastest first (ADR-0010).

## Build profile

`lto = "fat"`, `codegen-units = 1`, `panic = "abort"`. aarch64 builds use
`target-cpu=neoverse-n1` (`.cargo/config.toml`). MSRV 1.88 (edition 2024, let-chains).
