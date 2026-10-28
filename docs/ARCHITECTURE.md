# Architecture (current state)

_Last updated: 2026-10-28, end of Sprint 1._

## Pipeline

```
core0  control / cold path (startup, config, reporting; later: world-data ingest)
core1  feed      ──[SPSC ring<MarketEvent, 65536>]──▶
core2  strategy  ──[SPSC ring<Signal, 65536>]──▶
core3  risk + router ──▶ Broker (PaperBroker | live adapters from Sprint 4)
                    ◀── Exec reports (Fill | Cancelled), same core, synchronous poll
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
| `dd-feed` | `SyntheticFeed` (demo); live adapters Sprint 2 | forbidden |
| `dd-engine` | `deltadrift` binary: config, pipeline wiring, report | forbidden |

## Wire types (`dd-core/src/types.rs`)

All `Copy`, `repr(C, align(64))`, exactly 64 B, compile-time asserted.
Prices/quantities are fixed-point `i64` at 1e-8 (`Px`, `Qty`). Instruments are dense
`u32` indices `< MAX_INSTRUMENTS (256)`; per-instrument state is a boxed array.

- `MarketEvent` — `recv_ns` (latency origin), `exch_ns`, `seq`, instrument, kind, bid/ask/last/last_qty
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

## Clock & pinning

- `clock::now_ns()` = `Instant` since a process epoch (vDSO, no syscall).
- `affinity::init()` snapshots allowed cores **once, before any pinning**: see ADR-0004
  for the bug this prevents. The control thread pins itself only after spawning stages.

## Build profile

`lto = "fat"`, `codegen-units = 1`, `panic = "abort"`. aarch64 builds use
`target-cpu=neoverse-n1` (`.cargo/config.toml`). MSRV 1.88 (edition 2024, let-chains).
