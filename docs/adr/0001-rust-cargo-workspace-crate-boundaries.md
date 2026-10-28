# ADR-0001: Rust workspace with crate boundaries enforcing the risk/strategy split

- Status: Accepted · Date: 2026-10-28

## Context
The spec requires strategy generation be decoupled from hard execution limits, and a
Rust core for low latency. Conventions alone ("don't call risk internals from strategies")
erode as ML policies are added in Sprint 5.

## Decision
One Cargo workspace, one crate per concern (`dd-core`, `dd-features`, `dd-strategy`,
`dd-risk`, `dd-exec`, `dd-feed`, `dd-engine`). `dd-risk` has no dependency on
`dd-strategy`, so the compiler enforces the boundary. `unsafe` is confined to
`dd-core`; every other crate is `#![forbid(unsafe_code)]`.

## Consequences
+ Boundaries are compile-checked; small crates build/test fast in isolation.
− More manifests to maintain. Cross-crate inlining relies on LTO (enabled: `lto = "fat"`).
