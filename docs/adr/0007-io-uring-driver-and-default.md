# ADR-0007: io_uring driver design; `uring` default, SQPOLL opt-in

- Status: Accepted · Date: 2026-10-28

## Context
Spec calls for io_uring. With one socket per feed, io_uring's benefit is not throughput
but how the feed core waits: std recv busy-polling makes a syscall per spin.

## Decision
- One recv SQE armed at a time into `RecvBuf`'s spare region; completion detected by reading
  the shared CQ ring (**no syscall while waiting**). Not using COOP/DEFER_TASKRUN so
  completions post while the thread spins in userspace.
- `RecvBuf` uses a raw allocation; compaction only when no recv is in flight; `Io` field
  order + `Drop` cancel/reap guarantee the kernel never writes freed memory.
- `--io sqpoll` adds a kernel SQ-poll thread pinned to the control core → syscall-free steady state.
- Default `io = "uring"`; engine probes at startup and falls back to std with a warning.

## Evidence (live Binance, ~200–340 msg/s, dev box)
std / uring / sqpoll showed no consistent latency difference: time is dominated by
in-batch processing (~5.6 msgs per read), not syscalls. `uring` is default because idling
costs no kernel transitions (less OS jitter on the feed core), not because it is faster today.

## Consequences
Revisit with multishot recv + provided buffer rings when multiple sockets share a core (Sprint 8).
