# ADR-0011: cTrader Open API as the single primary (free) market-data vendor

- Status: Accepted · Date: 2026-10-29 · Requested by project owner ("keep it all free")
- Research: [research/2026-10-29-free-market-data-sources.md](../research/2026-10-29-free-market-data-sources.md)

## Context
The owner wants one vendor, at zero cost, covering Forex, Crypto and Stocks. Alpaca's full
real-time US stock data (SIP) is paid; its free tier is IEX-only with 30 symbols. Free
consolidated US stock data does not exist. Forex has no exchange feed: a broker's price is the
price you trade at.

## Options considered
| Option | FX | Crypto | Stocks | Cost | Verdict |
|---|---|---|---|---|---|
| **cTrader Open API (demo account)** | ✅ | ✅ (CFD) | ✅ share CFDs (broker-dependent) | free | **chosen: only single free API covering all three, plus execution** |
| OANDA v20 | ✅ | ❌ | ❌ | free | FX only; 4 Hz cap; not via OANDA Global Markets |
| Coinbase/Kraken/Binance | ❌ | ✅ exchange-grade | ❌ | free | crypto only |
| Alpaca Basic | ❌ | ✅ | IEX only, 30 symbols | free | stocks thin |
| IBKR | ✅ | ✅ | Cboe One + IEX | free data, but funded account | not free to start |

## Decision
- Integrate **cTrader Open API** as the default live venue (`--live ctrader`, `[live].venue = "ctrader"`).
- Transport: `wss://{demo|live}.ctraderapi.com:5035`, one binary WS frame per `ProtoMessage`,
  reusing `dd-net` unchanged (verified live: `ProtoOAVersionRes` version 102, no credentials needed).
- Protobuf: hand-rolled proto2 codec (`dd_feed::proto`), not prost: ~12 messages, zero-alloc
  `SpotEvent` decode, no `protoc` build dependency. Field numbers from Spotware's official `.proto` files.
- Credentials via `.env` (ADR-0008): `CTRADER_CLIENT_ID/SECRET/ACCESS_TOKEN`, optional
  `CTRADER_ACCOUNT_ID`, `CTRADER_ENV`. The default account is the first *demo* account on the token,
  so a replacement demo account is picked up without config changes.
- Existing Binance/Alpaca adapters remain (already built and tested) as optional/secondary feeds.

## Consequences
+ One integration; the same API carries order execution in Sprint 4 → **data venue = execution venue**,
  and demo = free paper trading on real broker prices.
− Prices are the **broker's CFD quotes**, not exchange prints; no sizes on the spot stream (`qty = 0`).
  Crypto/stock CFDs follow the broker's trading hours and spreads.
− Symbol names are broker-specific (e.g. `EURUSD`, `BTCUSD`, share names vary). Unknown names fail
  with suggestions from the broker's list.
− Demo accounts expire after inactivity (reported 90 days for cTrader demos, [S]); access tokens last ~30 days.
  Token refresh (`ProtoOARefreshTokenReq`) not yet implemented because refresh tokens rotate and need
  secure persistence. Open item.
