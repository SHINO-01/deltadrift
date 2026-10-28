//! `deltadrift` — engine entrypoint.

#![forbid(unsafe_code)]

mod config;
mod feed;
mod pipeline;
mod report;

use std::path::PathBuf;

use anyhow::{Result, bail};
use clap::{Parser, Subcommand};
use dd_exec::TradingMode;

use crate::config::{Config, IoMode, LiveVenue, WaitMode};

#[derive(Debug, Parser)]
#[command(name = "deltadrift", version, about = "Self-improving, low-latency quant trading engine")]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Debug, Subcommand)]
enum Cmd {
    /// Run the trading pipeline.
    Run(RunArgs),
    /// Print the effective default configuration as TOML-compatible text.
    Config,
}

#[derive(Debug, clap::Args)]
struct RunArgs {
    /// TOML config file. CLI flags override it.
    #[arg(short, long)]
    config: Option<PathBuf>,
    /// Offline demo: synthetic feed, paper broker, no credentials or network.
    #[arg(long, conflicts_with = "live")]
    demo: bool,
    /// Live market data from a venue (paper execution). Overrides [live].venue.
    #[arg(long, value_enum)]
    live: Option<LiveVenue>,
    /// Comma-separated venue symbols. Overrides [live].symbols.
    #[arg(long, value_delimiter = ',')]
    symbols: Option<Vec<String>>,
    /// Socket I/O backend for live feeds. Overrides [live].io.
    #[arg(long, value_enum)]
    io: Option<IoMode>,
    /// Path to a .env file with secrets (default: search upward from the CWD).
    #[arg(long)]
    env_file: Option<PathBuf>,
    /// Run time in seconds.
    #[arg(short, long)]
    duration: Option<f64>,
    /// Feed rate in events/sec (0 = unthrottled).
    #[arg(short, long)]
    rate: Option<u64>,
    #[arg(long, value_enum)]
    wait: Option<WaitMode>,
    /// Disable CPU core pinning.
    #[arg(long)]
    no_pin: bool,
    /// Emit the report as JSON on stdout.
    #[arg(long)]
    json: bool,
}

fn main() -> Result<()> {
    // Snapshot allowed cores before any thread pins itself (see dd_core::affinity).
    dd_core::affinity::init();
    let cli = Cli::parse();
    // Load secrets before spawning threads (mutating the environment is not thread-safe).
    let env_file = match &cli.cmd {
        Cmd::Run(a) => a.env_file.clone(),
        Cmd::Config => None,
    };
    dd_secrets::load_dotenv(env_file.as_deref())?;
    match cli.cmd {
        Cmd::Config => {
            println!("{:#?}", Config::default());
            Ok(())
        }
        Cmd::Run(a) => {
            let mut cfg = Config::load(a.config.as_deref())?;
            if let Some(d) = a.duration {
                cfg.engine.duration_secs = d;
            }
            if let Some(r) = a.rate {
                cfg.engine.rate_per_sec = r;
            }
            if let Some(w) = a.wait {
                cfg.engine.wait = w;
            }
            if a.no_pin {
                cfg.engine.pin = false;
            }
            if cfg.engine.mode == TradingMode::Live {
                bail!("LIVE mode is not available yet (live broker adapters ship in Sprint 4); refusing to start");
            }
            if let Some(v) = a.live {
                cfg.live.venue = v;
            }
            if let Some(s) = a.symbols {
                cfg.live.symbols = s;
            }
            if let Some(io) = a.io {
                cfg.live.io = io;
            }
            if !a.demo && a.live.is_none() {
                bail!("choose a data source: --demo (offline synthetic) or --live <venue>");
            }
            let src = feed::build(&cfg, a.live.is_some())?;
            eprintln!("feed: {}", src.describe());
            let cores = dd_core::affinity::available_cores();
            if cfg.engine.pin && cfg.engine.cores().iter().any(|&c| c >= cores) {
                eprintln!("warning: requested cores {:?} but only {cores} available; some stages will run unpinned", cfg.engine.cores());
            }
            let r = pipeline::run(&cfg, src)?;
            if a.json {
                println!("{}", serde_json::to_string_pretty(&report::json(&r, &cfg))?);
            } else {
                report::print(&r, &cfg);
            }
            Ok(())
        }
    }
}
