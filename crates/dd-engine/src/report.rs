//! Run summary. Equity is reported normalised to a $100.00 base so results
//! can be shared without revealing account size (FR-6).

use dd_risk::RejectReason;
use hdrhistogram::Histogram;
use serde_json::{Value, json};

use crate::config::Config;
use crate::pipeline::RunReport;

const QUANTILES: [(f64, &str); 5] = [(0.5, "p50"), (0.9, "p90"), (0.99, "p99"), (0.999, "p99.9"), (1.0, "max")];

fn norm(r: &RunReport, v: f64) -> f64 {
    v / r.starting_equity * 100.0
}

fn lat_json(h: &Histogram<u64>) -> Value {
    let mut m = serde_json::Map::new();
    m.insert("count".into(), h.len().into());
    for (q, name) in QUANTILES {
        m.insert(format!("{name}_ns"), h.value_at_quantile(q).into());
    }
    m.insert("mean_ns".into(), json!(h.mean().round()));
    Value::Object(m)
}

pub fn json(r: &RunReport, cfg: &Config) -> Value {
    let secs = r.wall.as_secs_f64();
    let rejects: serde_json::Map<_, _> =
        RejectReason::ALL.iter().map(|x| (x.name().to_string(), r.risk_stats.rejects[*x as usize].into())).collect();
    json!({
        "mode": "paper-demo",
        "wall_secs": secs,
        "wait": format!("{:?}", cfg.engine.wait).to_lowercase(),
        "pinned": { "feed": r.feed.pinned, "strategy": r.strategy.pinned, "risk": r.risk.pinned },
        "events": r.feed.items,
        "throughput_eps": r.feed.items as f64 / secs,
        "dropped": r.feed.items - r.risk.items,
        "latency": { "tick_to_decision": lat_json(&r.decision_latency), "tick_to_order": lat_json(&r.order_latency) },
        "risk": {
            "orders": r.risk_stats.orders, "fills": r.fills, "cancels": r.cancels,
            "stop_outs": r.risk_stats.stop_outs, "kelly_capped": r.risk_stats.kelly_capped,
            "gross_capped": r.risk_stats.gross_capped, "rejects": rejects,
            "halted": r.risk_stats.halted, "broker_errors": r.broker_errors,
        },
        "performance_normalized": {
            "base": 100.0,
            "equity": norm(r, r.equity),
            "realized": norm(r, r.realized),
            "fees": norm(r, r.fees),
            "max_drawdown_pct": r.max_drawdown * 100.0,
        }
    })
}

pub fn print(r: &RunReport, cfg: &Config) {
    let secs = r.wall.as_secs_f64();
    let pin = |b: bool| if b { "pinned" } else { "unpinned" };
    println!("DeltaDrift run — paper/demo, wait={:?}, {:.2}s", cfg.engine.wait, secs);
    println!(
        "  stages   feed[{}] {} | strategy[{}] {} | risk[{}] {}",
        cfg.engine.cores[1], pin(r.feed.pinned), cfg.engine.cores[2], pin(r.strategy.pinned), cfg.engine.cores[3], pin(r.risk.pinned)
    );
    println!(
        "  events   {} in, {} processed ({:.0} ev/s), dropped {}",
        r.feed.items,
        r.risk.items,
        r.feed.items as f64 / secs,
        r.feed.items - r.risk.items
    );
    for (name, h) in [("tick→decision", &r.decision_latency), ("tick→order", &r.order_latency)] {
        let qs: Vec<String> = QUANTILES.iter().map(|(q, n)| format!("{n} {}", fmt_ns(h.value_at_quantile(*q)))).collect();
        println!("  {name:<14} n={:<10} {}", h.len(), qs.join("  "));
    }
    let s = &r.risk_stats;
    println!(
        "  risk     orders {}  fills {}  cancels {}  stop-outs {}  kelly-capped {}  gross-capped {}  halted {}",
        s.orders, r.fills, r.cancels, s.stop_outs, s.kelly_capped, s.gross_capped, s.halted
    );
    let rej: Vec<String> = RejectReason::ALL.iter().map(|x| format!("{} {}", x.name(), s.rejects[*x as usize])).collect();
    println!("  rejects  {}", rej.join("  "));
    println!(
        "  equity   ${:.4} (base $100.00)  realized {:+.4}  fees {:.4}  max DD {:.3}%",
        norm(r, r.equity),
        norm(r, r.realized),
        norm(r, r.fees),
        r.max_drawdown * 100.0
    );
}

fn fmt_ns(ns: u64) -> String {
    match ns {
        n if n < 1_000 => format!("{n}ns"),
        n if n < 1_000_000 => format!("{:.1}µs", n as f64 / 1e3),
        n => format!("{:.2}ms", n as f64 / 1e6),
    }
}
