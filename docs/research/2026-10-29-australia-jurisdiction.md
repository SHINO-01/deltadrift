# Research: operating from Australia (owner's jurisdiction)

_Date: 2026-10-29. Not legal or tax advice; confirm with an accountant before G3 (live pilot)._

## ASIC CFD product intervention order (retail clients) [P]
- **Leverage caps:** 30:1 major FX pairs · 20:1 minor FX, gold, major stock indices ·
  10:1 other commodities, minor indices · **5:1 shares/other** · **2:1 crypto-assets**.
- Standardised **margin close-out** (positions closed before most of the account is lost) and
  **negative balance protection** (losses limited to funds in the CFD account).
- Order extended to **23 May 2027** (ASIC 22-082MR). It may be renewed or changed → re-check
  before G3; design for the caps regardless (they're sensible risk limits on their own).

### Engineering consequences
- Risk engine enforces per-asset-class leverage caps **itself** (gross notional / equity per class),
  set at or *below* ASIC caps; it doesn't rely on the broker (live blocker L12).
- 2:1 on crypto CFDs makes crypto exposure small by construction; that's acceptable.
- Margin close-out by the broker is a last resort; our daily-DD halt (2%) and stops must trigger long before it.

## Tax (ATO) [S: ATO legal database excerpts]
- CFD gains/losses are on **revenue account (ordinary income)**, not CGT (s118-20 anti-overlap).
- Either business income (s6-5 / s8-1) or a profit-making undertaking (s15-15 / s25-40),
  depending on scale/regularity and how business-like the activity is.
- Engineering: keep a complete, immutable trade + fee + swap ledger exportable per Australian
  financial year (1 Jul – 30 Jun), in AUD. → added to reporting requirements.

## Brokers / vendors available in Australia
- **Pepperstone Group Ltd**: ASIC AFSL 414530; free cTrader demo (expires after 90 days of inactivity [S]).
- OANDA Australia Pty Ltd supports the v20 API [P]: viable FX fallback.
- Coinbase/Kraken public market data: usable from Australia (no account needed for data).

## Sources
- ASIC 20-254MR (order made): https://www.asic.gov.au/about-asic/news-centre/find-a-media-release/2020-releases/20-254mr-asic-product-intervention-order-strengthens-cfd-protections
- ASIC 22-082MR (extended to 2027): https://www.asic.gov.au/about-asic/news-centre/find-a-media-release/2022-releases/22-082mr-asic-s-cfd-product-intervention-order-extended-for-five-years
- ATO legal database (CFD revenue treatment): https://www.ato.gov.au/law/view/document?docid=EV/1051705980846
- Pepperstone about (AFSL): https://pepperstone.com/au/why-pepperstone/about
- Pepperstone demo accounts: https://pepperstone.com/en-au/ways-to-trade/demo-accounts
- OANDA v20 entities: https://developer.oanda.com/rest-live-v20/introduction/
