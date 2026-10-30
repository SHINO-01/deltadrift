# Path to Live Trading

**Goal (owner, 2026-10-29):** trade live money and build an automated secondary income stream
whose profits are reinvested into future projects.

**Principle:** capital preservation first. Real money is only risked when *evidence* says the
strategy has an edge after all costs and the system has proven it fails safe. Engineering
guarantees safety, not profitability: most strategies (and, per broker disclosures, most retail
CFD accounts) lose money. Nothing here is financial advice.

## Go-live gates (all must pass, in order; each is recorded in SPRINT_LOG with evidence)

| Gate | Pass criteria | Status |
|---|---|---|
| **G0 Engineering** | Every "Live blocker" below closed; LIVE requires an explicit `--i-accept-live-risk` flag plus config cap | ⬜ |
| **G1 Backtest** | Walk-forward, out-of-sample, **after spreads, commissions, swaps/financing and slippage**; ≥ 2 years OOS (≥ 2.25 y data); ≥ 300 round trips; deflated Sharpe > 0.95; max drawdown < 15%; positive return; **win rate > 70% net of costs (owner rule)**; profit factor ≥ 1.3. Evaluated automatically by `deltadrift backtest` | ⬜ (harness ✅; awaiting real data) |
| **G2 Paper (demo)** | ≥ 8 weeks continuous on the cTrader demo, ≥ 100 trades; **win rate > 70% net (owner rule)**; live fills vs. backtest within agreed tolerance; zero unhandled incidents; restarts/reconnects recovered automatically | ⬜ |
| **G3 Live pilot** | Capital the owner can afford to lose **entirely**, hard-capped in config; daily DD halt 2%; kill switch tested; 8+ weeks results consistent with G2 | ⬜ |
| **G4 Scale** | Gradual increases only after G3 consistency; re-run G1 on any strategy change | ⬜ |

Any gate failure → back to research. A changed strategy restarts at G1.

## Live blockers (engineering, G0)

| # | Blocker | Why it matters | Where |
|---|---|---|---|
| L1 | cTrader **execution** adapter (orders, fills, cancels) + order state machine | can't trade without it | Sprint 4 |
| L2 | **Reconciliation** with broker positions/balance on start and periodically; halt on mismatch | engine view ≠ reality = uncontrolled risk | Sprint 4 |
| L3 | **Persistent state** + crash/restart recovery (positions, pending orders, day-start equity, halt latch) | a restart must not reset the drawdown guard | Sprint 4 |
| L4 | **Broker-side protective stops** on every position | survives engine/network death | Sprint 4 |
| L5 | Risk-reducing orders exempt from the strategy rate limit (separate reduce-only budget) | stop-outs must never be throttled (today they can be) | Sprint 4 |
| L6 | **Integer cash ledger** (ADR-0005) | float drift in P&L/limits | Sprint 4 |
| L7 | `roll_day` scheduled on the broker's trading-day boundary | daily DD guard currently never resets/rebases | calendar + backtest ✅ (`dd_core::calendar`); live engine wiring Sprint 4 |
| L8 | **Cost model**: spreads, commissions, swaps/overnight financing, CFD margin/leverage | edge must survive real costs | ✅ backtest (placeholder rates); real broker rates from cTrader symbol data when credentials land |
| L9 | Backtest/replay engine using the same strategy+risk code (no look-ahead) | G1 evidence | ✅ `dd-backtest` |
| L10 | Monitoring + alerting (phone push/email) on halt, disconnect, reconciliation failure, P&L | someone must know when it breaks | Sprint 6 |
| L11 | Token refresh with secure persistence; encrypted secrets at rest (NFR-3) | 30-day token expiry must not stop a live system unnoticed | Sprint 6–7 |
| L12 | Hard capital caps in config (max equity used, max notional) + **per-asset-class leverage caps ≤ ASIC retail limits** (FX major 30:1, minor/gold/major index 20:1, commodities 10:1, shares 5:1, crypto 2:1) | bounds the worst case; jurisdiction | Sprint 4 |
| L15 | **FX conversion** of P&L/notional to account currency (AUD/USD) for non-USD-quoted instruments | USDJPY P&L is in JPY; risk limits would be wrong | Sprint 4 |
| L14 | Tax-ready ledger: every trade, fee, swap in AUD, exportable per AU financial year (1 Jul–30 Jun) | ATO treats CFD P&L as ordinary income | Sprint 6 |
| L13 | 24/7 hosting with auto-restart (systemd) on OCI + clock sync | unattended operation | Sprint 8 |

## Income & reinvestment (operational, not automated)
- Profits stay in the broker account until the owner withdraws; the engine never moves money
  out (withdrawals are a manual, human action by design).
- Reporting: monthly realised P&L, fees, swaps, drawdown, normalised to $100 base (FR-6) to
  support withdraw/reinvest decisions and tax records (jurisdiction-specific; owner's responsibility).

## Owner decisions (resolved 2026-10-29)
1. Jurisdiction: **Australia** → ASIC retail CFD rules, ATO revenue-account tax treatment.
   See [research/2026-10-29-australia-jurisdiction.md](research/2026-10-29-australia-jurisdiction.md).
   Before G3: re-check the ASIC order (expires 23 May 2027 unless renewed); confirm tax with an accountant.
2. Roadmap reorder (ADR-0012): **approved**.
