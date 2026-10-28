# ADR-0004: Snapshot allowed cores once, before any thread pins

- Status: Accepted · Date: 2026-10-28

## Context
First Sprint 1 run showed ~6–11 ms latency at any feed rate and every stage reporting
"unpinned". Root cause: `core_affinity::get_core_ids()` returns the **calling thread's**
mask. The main thread pinned itself to core 0 first; spawned stages inherited `{0}`, saw
only one core, failed to pin, and all four busy-spinning threads time-sliced on core 0
(scheduler quantum ≈ 1–4 ms).

## Decision
`dd_core::affinity::init()` snapshots the allowed set once in `main` before any pinning,
and `pin_current` pins by explicit id. The control thread pins itself only *after*
spawning stages. A regression test asserts a child of a pinned thread can re-pin.

## Consequences
Latency fell from ~6 ms p50 to ~0.37 µs p50. Any future thread-spawning code must call
`affinity::init()` early (it's idempotent).
