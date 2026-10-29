# Research: free real-time market data for Forex, Crypto, Stocks

_Date: 2026-10-29 · Trigger: Alpaca full-market (SIP) real-time stock data requires the paid
"Algo Trader Plus" plan ($1,089/yr). Goal: sources we can rely on at $0 data cost._

Confidence key: **[P]** confirmed on the provider's own docs · **[S]** secondary sources only — verify before building.

## Key facts that shape the answer

1. **There is no free consolidated (SIP / NBBO) real-time US stock feed.** Exchanges license
   that data. Free US stock data is either *single-venue* (IEX, Cboe One) or provided by a broker
   **to its own account holders**.
2. **Forex has no central exchange.** A retail broker's stream *is* that broker's price; it's the
   correct feed when you also execute there. Rule for DeltaDrift: **data venue = execution venue** for FX.
3. **Crypto is the easy case.** Major exchanges publish full real-time public WebSockets with no key.
4. **Jurisdiction decides eligibility** (Binance, OANDA, Schwab, IG, IBKR all vary by country).

## Crypto — free, real-time, no account

| Source | Access | Notes | Conf. |
|---|---|---|---|
| **Coinbase Advanced Trade** | `wss://advanced-trade-ws.coinbase.com`, no JWT for market data | ticker, level2 (all updates guaranteed), trades; subscribe to `heartbeats` or idle channels close in 60–90 s | [P] |
| **Kraken Spot v2** | `wss://ws.kraken.com/v2`, no auth | ticker (L1), book (10–1000 levels, CRC32 checksum), trade, ohlc | [P] |
| **Binance** (adapter exists) | public combined streams | Largest liquidity. Not for US residents (Binance.US instead); reported to have ended EU services July 2026 under MiCA | [S] for EU |
| OKX, Bybit | public WS, no key | Fast pushes (Bybit spot L1 every 10 ms). Not available to US residents | [S] |
| Alpaca crypto | v1beta3 WS (Alpaca venue + Kraken US/EU) | Free-tier inclusion not stated on the page we read | [S] |

## Forex — free with a (demo) broker account

| Source | Access | Notes | Conf. |
|---|---|---|---|
| **cTrader Open API** (Spotware) | free with any cTrader broker account, **demo included** (e.g. Pepperstone, IC Markets) | Protobuf over TCP/WebSocket; real-time spot quotes + trading; 50 req/s per connection (5/s historical). Broker-agnostic, so it works in most countries. Best global option | [P] |
| **OANDA v20** | free with practice (demo) or live fxTrade account | HTTP chunked JSON stream; **capped at 4 prices/s per instrument**; heartbeat 5 s. **Not available via OANDA Global Markets or OANDA TMS Brokers** (many non-US/EU/AU/SG/JP clients) | [P] |
| IG | demo account; REST + Lightstreamer streaming | `demo-api.ig.com`; CFD prices | [S] |
| Interactive Brokers IDEALPRO | funded IBKR account | Institutional FX ECN quotes, reportedly free; TWS/Gateway socket API | [S] |
| Dukascopy | free historical tick data (no account for downloader) | **Backtesting source** (tick-level FX history), not a live feed | [S] |
| Twelve Data / FCS / Finnhub free tiers | data vendors | Free WebSocket is trial/limited; not reliable for a live engine | [S] |

## Stocks (US) — free real-time options

| Source | Coverage | Requirement | Conf. |
|---|---|---|---|
| **Alpaca Basic** (adapter exists) | **IEX only**, real-time; **30 symbols** on WebSocket | free account (paper trading free) | [P] |
| Finnhub free | US trades via WebSocket, ~50 symbols, 60 REST calls/min; non-commercial | free API key | [S] |
| **Interactive Brokers** | **Cboe One + IEX** real-time streaming, free, non-consolidated (no NBBO) | IBKR account (Lite or Pro). Paid subscriptions need ≥ USD 500 equity; demo accounts can't subscribe | [S] (IBKR site blocked automated fetch) |
| **Charles Schwab Trader API** | real-time streamer (LEVELONE quotes, book) for account holders | Schwab brokerage account + approved Individual Developer app; 120 req/min | [S] |
| **tastytrade** | dxFeed via DXLink WebSocket: equities, options (Greeks), futures | "fully onboarded" customer (account required) | [P] for requirement; cost [S] |
| Tradier | real-time streaming for brokerage account holders | **sandbox is delayed and has no streaming** | [S] |
| Massive (ex-Polygon) free | end-of-day / 15-min delayed, no WebSocket | — not real-time | [S] |

## Recommendation

| Asset | Primary (now) | Secondary / upgrade |
|---|---|---|
| **Crypto** | **Coinbase** + **Kraken** public WS (regulated, US/EU-friendly, no keys) | Binance / OKX / Bybit where legal for the operator |
| **Forex** | **cTrader Open API** with a demo account at a cTrader broker | OANDA v20 if eligible (simpler, but 4 Hz cap); IBKR IDEALPRO once funded |
| **Stocks** | **Alpaca IEX** (free, adapter exists, free paper trading) | With a brokerage account: **IBKR** (Cboe One + IEX, more venue coverage), **Schwab** or **tastytrade** (account-holder real-time) |

Engineering implications:
- Multi-venue crypto lets us cross-check feeds (stale/outlier detection) at no cost.
- cTrader is Protobuf over TCP/TLS: fits `dd-net` (TLS + our framing) better than JSON WebSockets; needs an OAuth app.
- OANDA streams over HTTP chunked transfer, not WebSocket: `dd-net` needs an HTTP/1.1 chunked reader.
- IEX-only quotes are thin; strategies on stocks must tolerate wider/less representative quotes until a broker feed is used.

## Decision (2026-10-29)
Owner asked for a single, fully free vendor → **cTrader Open API** with a demo account
(see ADR-0011). Pepperstone's demo offers FX, crypto and share CFDs among ~1,200 instruments
[S]; cTrader demos reportedly expire after 90 days of inactivity [S] (Pepperstone's own page
states 60 days for MT4/MT5 demos only [P]).

## Open questions for the owner
1. **Country of residence**: determines Binance, OANDA entity, IG, Schwab International, IBKR eligibility.
2. Willing to open (and possibly fund) a brokerage account? IBKR/Schwab/tastytrade unlock much better free stock data.

## Sources
- Alpaca market data plans: https://docs.alpaca.markets/docs/about-market-data-api
- Alpaca crypto data: https://docs.alpaca.markets/docs/real-time-crypto-pricing-data
- Coinbase WS overview: https://docs.cdp.coinbase.com/coinbase-app/advanced-trade-apis/websocket/websocket-overview
- Kraken WS v2: https://docs.kraken.com/api/docs/websocket-v2/
- OANDA v20 intro (entity eligibility): https://developer.oanda.com/rest-live-v20/introduction/
- OANDA pricing stream: https://developer.oanda.com/rest-live-v20/pricing-ep/
- cTrader Open API: https://help.ctrader.com/open-api/
- IBKR market data pricing: https://www.interactivebrokers.com/en/pricing/market-data-pricing.php
- IBKR market data requirements: https://www.interactivebrokers.com/docs/general/market-data-subscriptions/market-data-requirements
- IBKR subscription guide (2026, secondary): https://dev.to/xqliu/interactive-brokers-market-data-subscription-which-one-do-i-actually-need-in-2026-164g
- tastytrade streaming: https://developer.tastytrade.com/docs/guides/stream-market-data/
- Tradier docs: https://docs.tradier.com/docs
- Schwab API wrapper (secondary): https://pypi.org/project/schwabdev
- IG streaming guide: https://net-labs.ig.com/streaming-api-guide.html
- Dukascopy historical feed: https://www.dukascopy.com/swiss/english/marketwatch/historical/
- Twelve Data pricing: https://twelvedata.com/pricing
- Massive/Polygon (secondary): https://apicostcalc.com/polygon.html
- Pepperstone demo expiry: https://pepperstone.com/en-au/help-and-support/managing-my-account/does-a-trading-account-expire-if-it-is-not-used-for-some-time
- cTrader endpoints: https://help.ctrader.com/open-api/proxies-endpoints/
- cTrader auth/tokens: https://help.ctrader.com/open-api/account-authentication/
- cTrader framing: https://help.ctrader.com/open-api/protocol-buffers-json/
- Spotware proto files: https://github.com/spotware/openapi-proto-messages
- Binance restrictions (secondary): https://www.datawallet.com/de/krypto/binance-geschützte-länder
