use super::*;
use dd_core::{Fill, InstrumentId, Px, Signal};

const I: InstrumentId = InstrumentId(3);

fn sig(px: f64, mu: f64, sigma2: f64) -> Signal {
    Signal::new(0, 0, I, 1, Px::from_f64(px), Px::from_f64(px - 0.01), Px::from_f64(px + 0.01), mu, sigma2)
}

fn engine() -> RiskEngine {
    RiskEngine::new(RiskConfig { max_orders_per_sec: 1_000_000, order_burst: 1_000_000, ..Default::default() }).unwrap()
}

/// Fill any order the engine emits at `px`.
fn act(r: &mut RiskEngine, s: &Signal, now: u64, fill_px: f64) -> Decision {
    let d = r.on_signal(s, now);
    if let Decision::Order { side, qty, .. } = d {
        r.on_fill(&Fill::new(0, now, I, side, qty, Px::from_f64(fill_px), Px::ZERO));
    }
    d
}

#[test]
fn kelly_sizing_is_capped_by_position_limit() {
    let mut r = engine();
    // mu/sigma2 = 1000 -> raw quarter-Kelly = 250 * equity: far beyond cap.
    let Decision::Order { side, qty, .. } = r.on_signal(&sig(100.0, 0.001, 1e-6), 1) else { panic!() };
    assert_eq!(side, Side::Buy);
    assert!((qty.to_f64() * 100.0 - r.config().max_position_notional).abs() < 1e-3);
    assert_eq!(r.stats().kelly_capped, 1);
}

#[test]
fn small_kelly_is_proportional() {
    let mut r = engine();
    // f = 0.25 * 0.0001/0.01 = 0.0025 of 100k = $250 notional
    let Decision::Order { qty, .. } = r.on_signal(&sig(100.0, 0.0001, 0.01), 1) else { panic!() };
    assert!((qty.to_f64() * 100.0 - 250.0).abs() < 1e-6);
}

#[test]
fn pending_quantity_prevents_duplicate_orders() {
    let mut r = engine();
    assert!(matches!(r.on_signal(&sig(100.0, 0.001, 1e-6), 1), Decision::Order { .. }));
    // Not filled yet: same view must not double the exposure.
    assert_eq!(r.on_signal(&sig(100.0, 0.001, 1e-6), 2), Decision::Hold);
}

#[test]
fn invalid_signals_rejected() {
    let mut r = engine();
    for s in [sig(100.0, f64::NAN, 1.0), sig(100.0, 0.01, 0.0), sig(0.0, 0.0, 0.0), sig(100.0, 0.01, f64::INFINITY)] {
        assert_eq!(r.on_signal(&s, 1), Decision::Reject(RejectReason::InvalidSignal));
    }
}

#[test]
fn stop_loss_flattens_and_cools_down() {
    let mut r = engine();
    act(&mut r, &sig(100.0, 0.001, 1e-6), 1, 100.0);
    assert!(r.portfolio.position(I).qty > 0.0);
    // -1.5% move breaches the 1% stop.
    let d = act(&mut r, &sig(98.5, 0.001, 1e-6), 2, 98.5);
    assert!(matches!(d, Decision::Order { side: Side::Sell, .. }));
    assert_eq!(r.portfolio.position(I).qty, 0.0);
    assert_eq!(r.stats().stop_outs, 1);
    // Still bullish, but inside cooldown: no re-entry.
    assert_eq!(r.on_signal(&sig(98.5, 0.001, 1e-6), 3), Decision::Hold);
    // After cooldown, re-entry allowed.
    let later = r.config().stop_cooldown_ms * 1_000_000 + 10;
    assert!(matches!(r.on_signal(&sig(98.5, 0.001, 1e-6), later), Decision::Order { side: Side::Buy, .. }));
}

#[test]
fn daily_drawdown_halts_and_only_allows_flattening() {
    let mut r = RiskEngine::new(RiskConfig {
        max_position_notional: 50_000.0,
        stop_loss: 0.5, // isolate the drawdown guard from the stop
        max_orders_per_sec: 1_000_000,
        order_burst: 1_000_000,
        ..Default::default()
    })
    .unwrap();
    act(&mut r, &sig(100.0, 0.001, 1e-6), 1, 100.0); // $50k long
    // -5% on $50k = -$2.5k = 2.5% of equity > 2% limit.
    let d = act(&mut r, &sig(95.0, 0.001, 1e-6), 2, 95.0);
    assert!(r.is_halted());
    assert!(matches!(d, Decision::Order { side: Side::Sell, .. }));
    assert_eq!(r.portfolio.position(I).qty, 0.0);
    // Halted: a fresh bullish view produces nothing.
    assert_eq!(r.on_signal(&sig(95.0, 0.01, 1e-6), 3), Decision::Hold);
    r.roll_day();
    assert!(matches!(r.on_signal(&sig(95.0, 0.001, 1e-6), 4), Decision::Order { .. }));
}

#[test]
fn rate_limit_enforced() {
    let mut r = RiskEngine::new(RiskConfig { max_orders_per_sec: 1, order_burst: 2, ..Default::default() }).unwrap();
    let ids = [InstrumentId(1), InstrumentId(2), InstrumentId(3)];
    let mut out = vec![];
    for id in ids {
        let mut s = sig(100.0, 0.0001, 0.01);
        s.instrument = id;
        out.push(r.on_signal(&s, 1));
    }
    assert!(matches!(out[0], Decision::Order { .. }));
    assert!(matches!(out[1], Decision::Order { .. }));
    assert_eq!(out[2], Decision::Reject(RejectReason::RateLimited));
}

#[test]
fn portfolio_accounting_round_trip_and_flip() {
    let mut p = Portfolio::new(1_000.0);
    p.apply_fill(I, 10.0, 10.0, 0.1);
    p.mark(I, 11.0);
    assert!((p.equity() - (1_000.0 + 10.0 - 0.1)).abs() < 1e-9);
    // Sell 15 @ 12: realise +20 on 10, open -5 @ 12.
    p.apply_fill(I, -15.0, 12.0, 0.1);
    let pos = p.position(I);
    assert_eq!((pos.qty, pos.avg_px), (-5.0, 12.0));
    assert!((p.realized() - 20.0).abs() < 1e-9);
    assert!((p.recompute_unrealized() - 0.0).abs() < 1e-9);
    p.mark(I, 10.0);
    assert!((p.equity() - (1_000.0 + 20.0 + 10.0 - 0.2)).abs() < 1e-9);
    assert!((p.gross_notional() - 50.0).abs() < 1e-9);
}

#[test]
fn config_validation() {
    assert!(RiskConfig { max_daily_drawdown: 0.0, ..Default::default() }.validate().is_err());
    assert!(RiskConfig { kelly_fraction: 1.5, ..Default::default() }.validate().is_err());
    assert!(RiskConfig::default().validate().is_ok());
}
