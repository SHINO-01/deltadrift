# ADR-0005: Fixed-point on the wire, f64 inside risk/accounting

- Status: Accepted · Date: 2026-10-28

## Context
Prices must compare exactly (crossing checks, dedupe) and serialise deterministically;
Kelly/variance math is naturally floating point.

## Decision
`Px`/`Qty` are `i64` at 1e-8 on all wire types. `dd-risk` converts to `f64` for sizing
and P&L. Portfolio aggregates (unrealised, gross) are maintained incrementally and
tested against full recomputation.

## Consequences
+ Exact price comparisons; 64 B messages.
− P&L accumulates float error over very long sessions. Revisit (i128 cash ledger) before
  LIVE in Sprint 4, alongside broker reconciliation.
