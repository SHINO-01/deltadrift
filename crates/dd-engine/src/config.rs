use std::path::Path;

use anyhow::{Context, Result, bail};
use dd_exec::{PaperConfig, TradingMode};
use dd_feed::SyntheticConfig;
use dd_risk::RiskConfig;
use dd_strategy::ZScoreParams;
use serde::Deserialize;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, clap::ValueEnum)]
#[serde(rename_all = "lowercase")]
pub enum WaitMode {
    /// Pure busy-spin. For isolated production cores.
    Spin,
    /// Spin briefly then yield. For shared dev machines.
    Yield,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct EngineConfig {
    pub mode: TradingMode,
    /// Logical cores for [control/cold, feed, strategy, risk+router].
    /// `None` (default) = auto: one per physical core, fastest first
    /// (see `dd_core::affinity::plan_cores`).
    pub cores: Option<[usize; 4]>,
    pub pin: bool,
    pub wait: WaitMode,
    /// Feed rate in events/sec; 0 = unthrottled (max throughput).
    pub rate_per_sec: u64,
    pub duration_secs: f64,
}

impl Default for EngineConfig {
    fn default() -> Self {
        Self {
            mode: TradingMode::Paper,
            cores: None,
            pin: true,
            wait: WaitMode::Yield,
            rate_per_sec: 200_000,
            duration_secs: 10.0,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, clap::ValueEnum)]
#[serde(rename_all = "kebab-case")]
pub enum LiveVenue {
    /// Binance Spot bookTicker (public; no credentials).
    Binance,
    /// Alpaca crypto quotes (needs ALPACA_* in .env).
    AlpacaCrypto,
    /// Alpaca stock quotes, IEX or SIP per ALPACA_STOCK_FEED (needs ALPACA_* in .env).
    AlpacaStocks,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, clap::ValueEnum)]
#[serde(rename_all = "lowercase")]
pub enum IoMode {
    /// Non-blocking sockets.
    Std,
    /// io_uring; completions polled from shared memory.
    Uring,
    /// io_uring + kernel SQ-poll thread on the control core: syscall-free steady state.
    Sqpoll,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct LiveConfig {
    pub venue: LiveVenue,
    /// Venue symbols, e.g. "BTCUSDT" (Binance), "BTC/USD" (Alpaca crypto), "AAPL" (Alpaca stocks).
    pub symbols: Vec<String>,
    pub io: IoMode,
    /// Override the venue's base WebSocket URL (Binance only).
    pub url: Option<String>,
}

impl Default for LiveConfig {
    fn default() -> Self {
        Self { venue: LiveVenue::Binance, symbols: vec!["BTCUSDT".into(), "ETHUSDT".into()], io: IoMode::Uring, url: None }
    }
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    pub engine: EngineConfig,
    pub feed: SyntheticConfig,
    pub strategy: ZScoreParams,
    pub risk: RiskConfig,
    pub paper: PaperConfig,
    pub live: LiveConfig,
}

impl EngineConfig {
    /// Resolved core assignment.
    pub fn cores(&self) -> [usize; 4] {
        self.cores.unwrap_or_else(|| {
            let p = dd_core::affinity::plan_cores(4);
            std::array::from_fn(|i| p.get(i).copied().unwrap_or(i))
        })
    }
}

impl Config {
    pub fn load(path: Option<&Path>) -> Result<Self> {
        let mut cfg: Self = match path {
            Some(p) => {
                let s = std::fs::read_to_string(p).with_context(|| format!("reading {}", p.display()))?;
                toml::from_str(&s).with_context(|| format!("parsing {}", p.display()))?
            }
            None => Self::default(),
        };
        cfg.risk.validate().map_err(anyhow::Error::msg).context("invalid [risk] config")?;
        if cfg.engine.cores.is_none() {
            cfg.engine.cores = Some(cfg.engine.cores());
        }
        if cfg.engine.duration_secs <= 0.0 {
            bail!("engine.duration_secs must be > 0");
        }
        Ok(cfg)
    }
}
