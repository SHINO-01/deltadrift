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
    pub cores: [usize; 4],
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
            cores: [0, 1, 2, 3],
            pin: true,
            wait: WaitMode::Yield,
            rate_per_sec: 200_000,
            duration_secs: 10.0,
        }
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
}

impl Config {
    pub fn load(path: Option<&Path>) -> Result<Self> {
        let cfg: Self = match path {
            Some(p) => {
                let s = std::fs::read_to_string(p).with_context(|| format!("reading {}", p.display()))?;
                toml::from_str(&s).with_context(|| format!("parsing {}", p.display()))?
            }
            None => Self::default(),
        };
        cfg.risk.validate().map_err(anyhow::Error::msg).context("invalid [risk] config")?;
        if cfg.engine.duration_secs <= 0.0 {
            bail!("engine.duration_secs must be > 0");
        }
        Ok(cfg)
    }
}
