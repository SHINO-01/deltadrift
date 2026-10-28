# Glossary

- **SPSC ring** — single-producer/single-consumer bounded queue; the only inter-core channel.
- **Cursor caching** — each ring side keeps a stale copy of the other side's index, refreshing only when it looks full/empty (or short of a batch).
- **False sharing** — two cores writing different variables on one cache line, bouncing it; prevented with `CachePadded` / 64 B messages.
- **Hot path** — feed → strategy → risk → broker; no allocation, locks or syscalls allowed.
- **Cold path** — everything else (config, reporting, world-data ingest, training).
- **PIT (point-in-time)** — a value computed only from data available strictly before the moment it's used; no look-ahead.
- **Surprise z-score** — `(x − prior_mean) / prior_std`, using estimator state *before* `x`.
- **Decayed surprise** — accumulated z with exponential time decay (half-life in ns).
- **View** — a strategy's `(mu, sigma2)` for an instrument; never a size.
- **Fractional Kelly** — size `f = k · mu/sigma2` of equity, with `k < 1` (default 0.25).
- **In-flight / pending qty** — quantity sent to a broker but not yet filled or cancelled.
- **Halt (daily DD latch)** — after the daily drawdown limit is hit, only flattening is allowed until `roll_day()`.
- **Stop-out** — position closed by the per-position stop-loss; triggers a cooldown.
- **Normalised equity** — equity rescaled to a $100.00 starting base for shareable reporting (FR-6).
- **Px / Qty** — fixed-point i64 at 1e-8 scale.
- **IOC** — immediate-or-cancel; unfilled remainder is reported as `Exec::Cancelled`.
