//! Minimal `wss://host[:port][/path][?query]` parser (no userinfo, no IPv6 literals).

use crate::NetError;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WsUrl {
    pub host: String,
    pub port: u16,
    /// Path including query, always starting with `/`.
    pub path: String,
}

impl WsUrl {
    pub fn parse(s: &str) -> Result<Self, NetError> {
        let rest = s.strip_prefix("wss://").ok_or_else(|| NetError::Url(format!("{s}: only wss:// is supported")))?;
        let (authority, path) = match rest.find(['/', '?']) {
            Some(i) if rest.as_bytes()[i] == b'/' => (&rest[..i], rest[i..].to_string()),
            Some(i) => (&rest[..i], format!("/{}", &rest[i..])),
            None => (rest, "/".to_string()),
        };
        let (host, port) = match authority.rsplit_once(':') {
            Some((h, p)) => (h, p.parse::<u16>().map_err(|_| NetError::Url(format!("{s}: bad port")))?),
            None => (authority, 443),
        };
        if host.is_empty() || host.contains(['@', '[', ']']) {
            return Err(NetError::Url(format!("{s}: unsupported host")));
        }
        Ok(Self { host: host.to_string(), port, path })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses() {
        let u = WsUrl::parse("wss://stream.binance.com:9443/stream?streams=a/b").unwrap();
        assert_eq!((u.host.as_str(), u.port, u.path.as_str()), ("stream.binance.com", 9443, "/stream?streams=a/b"));
        let u = WsUrl::parse("wss://example.com").unwrap();
        assert_eq!((u.port, u.path.as_str()), (443, "/"));
        let u = WsUrl::parse("wss://example.com?x=1").unwrap();
        assert_eq!(u.path, "/?x=1");
        assert!(WsUrl::parse("ws://example.com").is_err());
        assert!(WsUrl::parse("wss://:443/").is_err());
    }
}
