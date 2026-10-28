use std::fmt;

use crate::ws::WsError;

#[derive(Debug)]
pub enum NetError {
    Url(String),
    Io(std::io::Error),
    Tls(rustls::Error),
    /// A fixed buffer was too small (TLS staging, plaintext, or receive buffer).
    Buffer(&'static str),
    Ws(WsError),
    /// The peer closed the WebSocket (with optional close code) or the TLS session.
    Closed { code: Option<u16> },
    Timeout,
}

impl fmt::Display for NetError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Url(s) => write!(f, "invalid url: {s}"),
            Self::Io(e) => write!(f, "io: {e}"),
            Self::Tls(e) => write!(f, "tls: {e}"),
            Self::Buffer(what) => write!(f, "buffer too small: {what}"),
            Self::Ws(e) => write!(f, "websocket: {e}"),
            Self::Closed { code: Some(c) } => write!(f, "connection closed by peer (code {c})"),
            Self::Closed { code: None } => write!(f, "connection closed by peer"),
            Self::Timeout => write!(f, "timed out"),
        }
    }
}

impl std::error::Error for NetError {}

impl From<std::io::Error> for NetError {
    fn from(e: std::io::Error) -> Self {
        Self::Io(e)
    }
}
impl From<rustls::Error> for NetError {
    fn from(e: rustls::Error) -> Self {
        Self::Tls(e)
    }
}
impl From<WsError> for NetError {
    fn from(e: WsError) -> Self {
        Self::Ws(e)
    }
}
impl From<rustls::unbuffered::EncodeError> for NetError {
    fn from(_: rustls::unbuffered::EncodeError) -> Self {
        Self::Buffer("tls encode staging")
    }
}
impl From<rustls::unbuffered::EncryptError> for NetError {
    fn from(_: rustls::unbuffered::EncryptError) -> Self {
        Self::Buffer("tls encrypt staging")
    }
}
