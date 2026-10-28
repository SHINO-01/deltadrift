//! Sans-IO TLS + WebSocket client for market-data feeds.
//!
//! ```text
//!  socket ──(Driver: io_uring | std)──▶ rx: ciphertext buffer
//!        ──(rustls *unbuffered*: decrypt in place)──▶ plaintext records
//!        ──(copy once)──▶ ws buffer ──(ws codec: zero-copy frame slices)──▶ on_msg(&[u8], recv_ns)
//! ```
//!
//! * TLS and WebSocket layers are pure state machines; only the [`IoBackend`]
//!   touches the socket, so the same protocol code runs over io_uring, plain
//!   non-blocking sockets, or (in tests) loopback.
//! * After `connect`, `poll` performs no heap allocation: all buffers are
//!   fixed-size and allocated at connect time. rustls' unbuffered API decrypts
//!   in place inside the receive buffer.
//! * Connection setup (DNS, TCP connect, TLS handshake, HTTP upgrade) is cold
//!   path and may block/allocate.

mod buf;
mod client;
mod driver;
mod error;
pub mod url;
pub mod ws;

pub use rustls;

pub use client::{ClientOptions, NetStats, WsClient, default_tls_config};
pub use driver::IoBackend;
pub use error::NetError;
