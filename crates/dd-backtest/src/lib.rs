//! Event-driven backtester (Sprint 3, live blockers L8/L9, gate G1).
//!
//! * [`sim`] — one simulated account: the **same** `Strategy` and `RiskEngine`
//!   code as the live engine, a [`SimBroker`](sim::SimBroker) with latency,
//!   spread, slippage, commission and overnight financing, and the daily
//!   drawdown guard rolled at the FX trading-day boundary.
//! * [`runner`] — feeds one historical stream to many simulations at once
//!   (parameter grids), parallel across simulations, deterministic.
//! * [`metrics`] — Sharpe/Sortino/drawdown and the probabilistic & deflated
//!   Sharpe ratios used by gate G1.
//! * [`walkforward`] — rolling train/test optimisation with out-of-sample stitching.
//!
//! No look-ahead by construction: an order decided on event *t* can only fill
//! on a later event at or after `t + latency`.

#![forbid(unsafe_code)]

pub mod metrics;
pub mod runner;
pub mod sim;
pub mod walkforward;

pub use sim::{CostModel, SimResult, Simulation};
