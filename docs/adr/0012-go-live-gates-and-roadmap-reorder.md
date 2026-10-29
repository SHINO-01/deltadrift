# ADR-0012: Evidence-gated path to live money; reorder roadmap

- Status: **Accepted** by owner 2026-10-29 · Date: 2026-10-29
- See [LIVE_READINESS.md](../LIVE_READINESS.md)

## Context
Owner goal: live trading as an automated secondary income stream. The original plan builds
SIMD features (S3) and the self-improving ML core (S5) before execution (S4) and validation (S6).
For income, the binding constraints are (a) a validated edge after costs and (b) a system that
fails safe with real money, not inference speed or model sophistication.

## Decision
1. LIVE trading is gated by G0–G4 (LIVE_READINESS.md). LIVE mode stays hard-refused in code until G0.
2. Reorder:
   - **S3 → Backtest & cost model** (L8, L9): replay engine reusing strategy+risk code, cTrader
     historical bars/ticks, spreads/swaps/commissions, walk-forward + deflated-Sharpe evaluation.
     (PIT feature store folds in here; SIMD deferred until profiling shows need.)
   - **S4 → Execution on cTrader demo** (L1–L7, L12).
   - **S5 → Strategy research** (incl. bandits/GA/RL from the original FR-3) evaluated *only* through S3's harness.
   - **S6 → Paper run (G2)** + monitoring/alerts (L10, L11).
   - **S7–S8** unchanged (security hardening, production ops), then G3 pilot.

## Consequences
+ Every hour goes toward evidence and safety before sophistication; ML only ships if it beats
  baselines out of sample after costs.
− Original sprint numbering/content changes; the spec document should be updated to match.
