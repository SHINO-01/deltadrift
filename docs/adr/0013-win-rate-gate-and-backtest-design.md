# ADR-0013: Owner win-rate rule (> 70%) as an additional gate; backtest design

- Status: Accepted · Date: 2026-10-30 · Win-rate rule set by project owner

## Context
Owner rule: "we only use the live account when our trading bot has a win rate of more than 70%."
Win rate alone does not imply profitability: a strategy winning 90% of trades with small gains and
losing 10% with large losses (negative skew, e.g. martingale-like or tight-target mean reversion)
can have a high win rate and lose money. High win-rate targets bias research toward exactly that shape.

## Decision
1. **Win rate > 70%** is a hard G1 and G2 gate, **in addition to** (never instead of) positive
   return after costs, deflated Sharpe > 0.95, max drawdown < 15%, ≥ 300 round trips, ≥ 2 years OOS.
2. Add **profit factor ≥ 1.3** (gross wins / gross losses) so wins must outweigh losses.
3. A "win" is judged **net of costs**: each closing fill's realised P&L minus the pro-rata share of
   that position's commissions and overnight financing (spread is already in fill prices).
4. Backtester (`dd-backtest`) design:
   - Runs the **same** `Strategy` and `RiskEngine` code as live; simulated broker with latency
     (fills at first quote ≥ decision + latency → no look-ahead), slippage, commission, financing
     (triple Wednesday, FX calendar with US DST), IOC cancels when the limit is breached during latency.
   - Daily-DD guard rolled at 17:00 New York (`dd_core::calendar`), closing live blocker L7's logic.
   - Walk-forward: train 90 d / test 30 d rolling; best-by-train-Sharpe params run on unseen test;
     only stitched out-of-sample results are judged. **≥ 2.25 years of data → ≥ 2 years OOS.**
   - Deflated Sharpe uses N = grid size and the variance of daily Sharpe across trials, excluding
     degenerate trials (< 10 trades or |SR| ≥ 5/day) whose estimates explode.
   - One data pass drives all parameter sets; parallel across simulations; deterministic.

## Consequences
+ The owner's rule is enforced mechanically; it cannot be met by a strategy that loses money.
− Fewer strategies will qualify. That is the intended effect.
