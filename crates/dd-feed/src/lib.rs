//! Market data sources.
//!
//! * [`synthetic`] — deterministic offline feed for `--demo` (FR-6).
//! * [`live`] — reconnecting WebSocket runner over `dd-net`, generic over a
//!   [`Venue`](venues::Venue) decoder. Decoders parse borrowed JSON straight into
//!   [`MarketEvent`](dd_core::MarketEvent)s with exact fixed-point prices and
//!   no heap allocation per message.

#![forbid(unsafe_code)]

pub mod live;
pub mod proto;
pub mod symbols;
pub mod synthetic;
pub mod time;
pub mod venues;

pub use symbols::SymbolTable;
pub use synthetic::{SyntheticConfig, SyntheticFeed};
