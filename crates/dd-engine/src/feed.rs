//! Feed-stage construction (cold path) and execution (feed core).

use anyhow::{Context, Result};
use dd_core::wait::Wait;
use dd_core::{MarketEvent, Producer, clock};
use dd_feed::live::{self, FeedStats};
use dd_feed::venues::alpaca::{Alpaca, AlpacaFeed};
use dd_feed::venues::binance::Binance;
use dd_feed::venues::ctrader::Ctrader;
use dd_feed::{SymbolTable, SyntheticFeed};
use dd_net::{ClientOptions, IoBackend};

use crate::config::{Config, IoMode, LiveVenue};

pub enum FeedSource {
    Synthetic(SyntheticFeed),
    Ctrader(Ctrader, ClientOptions),
    Binance(Binance, ClientOptions),
    Alpaca(Alpaca, ClientOptions),
}

impl FeedSource {
    pub fn describe(&self) -> String {
        match self {
            Self::Synthetic(_) => "synthetic (demo)".into(),
            Self::Binance(v, o) => format!("binance {:?} via {:?}", v.symbols().symbols(), o.io),
            Self::Ctrader(v, o) => format!("{v:?} via {:?}", o.io),
            Self::Alpaca(v, o) => format!("{v:?} via {:?}", o.io),
        }
    }
}

/// Builds the configured source. Fails fast on missing secrets, bad symbols,
/// or an unusable I/O backend, before any thread is spawned.
pub fn build(cfg: &Config, live: bool) -> Result<FeedSource> {
    if !live {
        return Ok(FeedSource::Synthetic(SyntheticFeed::new(cfg.feed.clone())));
    }
    let l = &cfg.live;
    let mut io = match l.io {
        IoMode::Std => IoBackend::Std,
        IoMode::Uring => IoBackend::Uring { sqpoll_cpu: None },
        IoMode::Sqpoll => IoBackend::Uring { sqpoll_cpu: Some(cfg.engine.cores()[0] as u32) },
    };
    if let Err(e) = io.probe() {
        eprintln!("warning: {io:?} unavailable ({e}); falling back to std sockets");
        io = IoBackend::Std;
    }
    let opts = ClientOptions { io, ..Default::default() };
    let symbols = SymbolTable::new(&l.symbols).map_err(anyhow::Error::msg).context("[live].symbols")?;
    Ok(match l.venue {
        LiveVenue::Ctrader => FeedSource::Ctrader(Ctrader::from_env(symbols).map_err(anyhow::Error::msg)?, opts),
        LiveVenue::Binance => {
            let v = match &l.url {
                Some(u) => Binance::with_base_url(symbols, u),
                None => Binance::new(symbols),
            };
            FeedSource::Binance(v, opts)
        }
        LiveVenue::AlpacaCrypto => FeedSource::Alpaca(Alpaca::from_env(AlpacaFeed::Crypto, symbols)?, opts),
        LiveVenue::AlpacaStocks => {
            let sip = dd_secrets::optional("ALPACA_STOCK_FEED").is_some_and(|f| f.expose().eq_ignore_ascii_case("sip"));
            FeedSource::Alpaca(Alpaca::from_env(AlpacaFeed::Stocks { sip }, symbols)?, opts)
        }
    })
}

/// Runs on the feed core until `deadline`. Returns events pushed and live stats.
/// Each venue arm is a separate monomorphised loop: no dynamic dispatch per event.
pub fn run<W: Wait + Default, const N: usize>(
    src: FeedSource,
    tx: &mut Producer<MarketEvent, N>,
    deadline: u64,
    rate: u64,
) -> (u64, Option<FeedStats>) {
    let (mut push_wait, mut idle) = (W::default(), W::default());
    let mut n = 0u64;
    let mut emit = |e: MarketEvent| {
        tx.push_with(e, &mut push_wait);
        n += 1;
    };
    let stats = match src {
        FeedSource::Synthetic(mut feed) => {
            let interval = 1_000_000_000u64.checked_div(rate).unwrap_or(0);
            let mut next = clock::now_ns();
            loop {
                let now = clock::now_ns();
                if now >= deadline {
                    break;
                }
                if interval != 0 {
                    if now < next {
                        core::hint::spin_loop();
                        continue;
                    }
                    next += interval;
                }
                emit(feed.next_event(clock::now_ns()));
            }
            None
        }
        FeedSource::Ctrader(mut v, o) => Some(live::run(&mut v, &o, deadline, &mut idle, &mut emit)),
        FeedSource::Binance(mut v, o) => Some(live::run(&mut v, &o, deadline, &mut idle, &mut emit)),
        FeedSource::Alpaca(mut v, o) => Some(live::run(&mut v, &o, deadline, &mut idle, &mut emit)),
    };
    (n, stats)
}
