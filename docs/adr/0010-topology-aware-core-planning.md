# ADR-0010: Topology-aware default core assignment

- Status: Accepted · Date: 2026-10-28

## Context
Default `cores = [0,1,2,3]` on the dev box (i5-13420H, SMT + hybrid P/E cores) placed the
feed on core 0's SMT sibling and strategy/risk on sibling threads of one physical core.
Two busy-spinning siblings halve each other's throughput.

## Decision
`engine.cores` is optional; when omitted, `dd_core::affinity::plan_cores(4)` picks one logical
CPU per physical core (via sysfs `thread_siblings_list`), fastest first (`cpuinfo_max_freq`),
then by id. Explicit `cores = [...]` still overrides. On OCI A1 (no SMT) this yields 0,1,2,3.

## Evidence
Synthetic 200k ev/s: p99 18.5 µs → **1.8 µs**, p99.9 68 µs → 22 µs. Live wire→event p50 6.7 → 4.8 µs.
