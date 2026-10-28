# ADR-0003: Strategies emit views (mu, sigma2); only risk sizes orders

- Status: Accepted · Date: 2026-10-28

## Context
FR-4 requires fractional Kelly sizing *outside* the ML model. Self-improving policies
(RL/GA) will optimise whatever they control; if they control size, they can learn to
evade limits.

## Decision
`Signal` carries expected return `mu` and variance `sigma2`, never a quantity.
`RiskEngine` computes `k·mu/sigma2·equity`, clamps it, and is the only creator of orders.
Strategies emit a signal on every tick (with `mu = 0` meaning flat), so risk also
receives marks for stop-loss and drawdown without a separate price channel.

## Consequences
+ One place to audit sizing; RL action space becomes "views", which is bounded and comparable across policies.
− Risk core processes every tick (O(1), ~100 ns), not just trade signals.
