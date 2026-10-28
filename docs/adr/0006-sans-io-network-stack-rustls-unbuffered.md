# ADR-0006: Sans-IO network stack — rustls unbuffered API + own WebSocket codec

- Status: Accepted · Date: 2026-10-28

## Context
Sprint 2 needs wss:// market data with no hot-path allocation (engineering invariant),
and the spec asks for io_uring. Off-the-shelf stacks (tokio-tungstenite, rustls buffered
`ClientConnection`) allocate per message/record and own the socket, which rules out
swapping in an io_uring driver.

## Decision
`dd-net` is layered as pure state machines over a pluggable driver:
- **TLS:** rustls `UnbufferedClientConnection`. App-data records are **decrypted in place**
  inside our receive buffer; no internal plaintext queues.
- **WebSocket:** own RFC 6455 client codec (`ws.rs`): borrowed `Frame<'a>` slices, masking,
  control frames, fragmentation reassembly into a pre-reserved buffer, handshake validation.
- **I/O:** `Io { be: Backend, rx: RecvBuf }` with Std and io_uring backends (ADR-0007).
- One copy remains: decrypted record → WebSocket buffer (frames can span records).

## Consequences
+ After `connect`, `poll` is allocation-free; same protocol code on every backend.
+ Verified by a loopback test (real TCP + TLS + WS: pings, interleaved control frames,
  fragmentation, 70 KB message across records, close codes) on std, io_uring, io_uring+SQPOLL.
− We own a WebSocket implementation (~300 lines incl. tests). permessage-deflate is
  intentionally unsupported (rejected at handshake).
