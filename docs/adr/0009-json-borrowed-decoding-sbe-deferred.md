# ADR-0009: Borrowed-JSON decoding with exact decimals now; SBE deferred

- Status: Accepted · Date: 2026-10-28

## Context
Spec Sprint 2 mentions SBE zero-copy decoding. Binance's SBE market-data stream requires an
API key (Ed25519), and Alpaca/Coinbase/Kraken are JSON-only.

## Decision
- Decode JSON with serde borrowing `&str`/`&RawValue` from the WebSocket buffer — no
  allocation per message. Alpaca arrays are walked with a streaming `SeqAccess` visitor (no `Vec`).
- Prices/sizes are parsed from their **decimal text** by `dd_core::decimal::parse_fixed8`
  (exact, never via f64). Timestamps via an allocation-free RFC 3339 parser.
- SBE decoder is deferred until a venue that needs it is enabled (Binance SBE needs keys).

## Evidence
Binance bookTicker decode 297 ns, Alpaca quote 275 ns, symbol lookup 9.9 ns (criterion, dev box).
