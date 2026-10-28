# ADR-0002: Own SPSC ring buffer instead of crossbeam-channel

- Status: Accepted · Date: 2026-10-28

## Context
The spec lists crossbeam-channel and the LMAX Disruptor pattern. The pipeline is a
strict chain of single-producer/single-consumer hops.

## Decision
Implement a ~150-line SPSC ring in `dd-core::ring`: const power-of-two capacity, padded
cursors, cached remote cursors, batch `consume`, close-on-drop. No MPMC machinery, no
allocation after construction, no locks.

## Consequences
+ Measured 133 M msg/s (64 B), 156 ns cross-core RTT on the dev box.
+ Batch publish and close-on-drop semantics fit the pipeline exactly.
− We own the `unsafe` code; mitigated by SAFETY comments, a 2 M-item cross-thread
  ordering test with wrap-around, and a drop-accounting test. TODO: loom model test.
