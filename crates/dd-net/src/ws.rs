//! RFC 6455 WebSocket codec (client side), sans-IO.
//!
//! Parsing returns borrowed [`Frame`]s pointing into the caller's buffer —
//! no copying, no allocation. Server→client frames must be unmasked; client→
//! server frames are always masked.

use std::fmt;

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as B64;

const GUID: &[u8] = b"258EAFA5-E914-47DA-95CA-C5AB0DC85B11";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum Opcode {
    Continuation = 0x0,
    Text = 0x1,
    Binary = 0x2,
    Close = 0x8,
    Ping = 0x9,
    Pong = 0xA,
}

impl Opcode {
    #[inline(always)]
    fn from_u8(b: u8) -> Option<Self> {
        Some(match b {
            0x0 => Self::Continuation,
            0x1 => Self::Text,
            0x2 => Self::Binary,
            0x8 => Self::Close,
            0x9 => Self::Ping,
            0xA => Self::Pong,
            _ => return None,
        })
    }
    #[inline(always)]
    pub fn is_control(self) -> bool {
        (self as u8) & 0x8 != 0
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Frame<'a> {
    pub fin: bool,
    pub opcode: Opcode,
    pub payload: &'a [u8],
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WsError {
    Protocol(&'static str),
    TooLarge(u64),
    Handshake(String),
}

impl fmt::Display for WsError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Protocol(m) => write!(f, "protocol violation: {m}"),
            Self::TooLarge(n) => write!(f, "message of {n} bytes exceeds limit"),
            Self::Handshake(m) => write!(f, "handshake failed: {m}"),
        }
    }
}

impl std::error::Error for WsError {}

/// `Sec-WebSocket-Accept` value for a given `Sec-WebSocket-Key`.
pub fn accept_key(key: &str) -> String {
    let mut h = sha1_smol::Sha1::new();
    h.update(key.as_bytes());
    h.update(GUID);
    B64.encode(h.digest().bytes())
}

/// Base64 `Sec-WebSocket-Key` from 16 random bytes.
pub fn make_key(nonce: [u8; 16]) -> String {
    B64.encode(nonce)
}

/// Parses one server frame from the front of `buf`.
/// `Ok(None)` means more bytes are needed. On success returns the frame and
/// the number of bytes it occupied.
#[inline]
pub fn parse_frame(buf: &[u8], max_payload: usize) -> Result<Option<(Frame<'_>, usize)>, WsError> {
    let [b0, b1, ..] = *buf else { return Ok(None) };
    if b0 & 0x70 != 0 {
        return Err(WsError::Protocol("reserved bits set without negotiated extension"));
    }
    let opcode = Opcode::from_u8(b0 & 0x0F).ok_or(WsError::Protocol("unknown opcode"))?;
    let fin = b0 & 0x80 != 0;
    if b1 & 0x80 != 0 {
        return Err(WsError::Protocol("server frames must not be masked"));
    }
    let (len, hdr) = match b1 & 0x7F {
        126 => match buf.get(2..4) {
            Some(b) => (u64::from(u16::from_be_bytes([b[0], b[1]])), 4),
            None => return Ok(None),
        },
        127 => match buf.get(2..10) {
            Some(b) => {
                let n = u64::from_be_bytes(b.try_into().expect("8 bytes"));
                if n >> 63 != 0 {
                    return Err(WsError::Protocol("64-bit length with MSB set"));
                }
                (n, 10)
            }
            None => return Ok(None),
        },
        n => (u64::from(n), 2),
    };
    if opcode.is_control() && (!fin || len > 125) {
        return Err(WsError::Protocol("fragmented or oversized control frame"));
    }
    if len > max_payload as u64 {
        return Err(WsError::TooLarge(len));
    }
    let total = hdr + len as usize;
    if buf.len() < total {
        return Ok(None);
    }
    Ok(Some((Frame { fin, opcode, payload: &buf[hdr..total] }, total)))
}

/// Appends a masked, final client frame to `out`.
pub fn encode_client_frame(opcode: Opcode, payload: &[u8], mask: [u8; 4], out: &mut Vec<u8>) {
    out.push(0x80 | opcode as u8);
    let n = payload.len();
    if n < 126 {
        out.push(0x80 | n as u8);
    } else if n <= usize::from(u16::MAX) {
        out.push(0x80 | 126);
        out.extend_from_slice(&(n as u16).to_be_bytes());
    } else {
        out.push(0x80 | 127);
        out.extend_from_slice(&(n as u64).to_be_bytes());
    }
    out.extend_from_slice(&mask);
    out.extend(payload.iter().enumerate().map(|(i, b)| b ^ mask[i & 3]));
}

/// Appends the HTTP/1.1 upgrade request.
pub fn write_handshake_request(host: &str, port: u16, path: &str, key: &str, out: &mut Vec<u8>) {
    use std::io::Write;
    let host_hdr = if port == 443 { host.to_string() } else { format!("{host}:{port}") };
    write!(
        out,
        "GET {path} HTTP/1.1\r\nHost: {host_hdr}\r\nUpgrade: websocket\r\nConnection: Upgrade\r\n\
         Sec-WebSocket-Key: {key}\r\nSec-WebSocket-Version: 13\r\nUser-Agent: deltadrift/{}\r\n\r\n",
        env!("CARGO_PKG_VERSION")
    )
    .expect("writing to Vec cannot fail");
}

/// Validates the server's upgrade response at the front of `buf`.
/// Returns bytes consumed once the full header block is present.
pub fn parse_handshake_response(buf: &[u8], expected_accept: &str) -> Result<Option<usize>, WsError> {
    let Some(end) = buf.windows(4).position(|w| w == b"\r\n\r\n") else {
        if buf.len() > 16 * 1024 {
            return Err(WsError::Handshake("response header too large".into()));
        }
        return Ok(None);
    };
    let head = std::str::from_utf8(&buf[..end]).map_err(|_| WsError::Handshake("non-UTF-8 header".into()))?;
    let mut lines = head.split("\r\n");
    let status = lines.next().unwrap_or_default();
    if !status.starts_with("HTTP/1.1 101") {
        return Err(WsError::Handshake(format!("unexpected status: {status}")));
    }
    let (mut upgrade, mut connection, mut accept) = (false, false, false);
    for line in lines {
        let Some((k, v)) = line.split_once(':') else { continue };
        let v = v.trim();
        match k.trim().to_ascii_lowercase().as_str() {
            "upgrade" => upgrade = v.eq_ignore_ascii_case("websocket"),
            "connection" => connection = v.split(',').any(|t| t.trim().eq_ignore_ascii_case("upgrade")),
            "sec-websocket-accept" => accept = v == expected_accept,
            "sec-websocket-extensions" if !v.is_empty() => {
                return Err(WsError::Handshake("server negotiated an unrequested extension".into()));
            }
            _ => {}
        }
    }
    if !(upgrade && connection && accept) {
        return Err(WsError::Handshake("missing/invalid Upgrade, Connection or Sec-WebSocket-Accept".into()));
    }
    Ok(Some(end + 4))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rfc6455_accept_example() {
        assert_eq!(accept_key("dGhlIHNhbXBsZSBub25jZQ=="), "s3pPLMBiTxaQ9kYGzzhZRbK+xOo=");
    }

    fn server_frame(fin: bool, op: u8, payload: &[u8]) -> Vec<u8> {
        let mut v = vec![(u8::from(fin) << 7) | op];
        match payload.len() {
            n if n < 126 => v.push(n as u8),
            n if n <= 65535 => {
                v.push(126);
                v.extend_from_slice(&(n as u16).to_be_bytes());
            }
            n => {
                v.push(127);
                v.extend_from_slice(&(n as u64).to_be_bytes());
            }
        }
        v.extend_from_slice(payload);
        v
    }

    #[test]
    fn parses_all_length_encodings_and_partials() {
        for n in [0usize, 5, 125, 126, 65535, 65536, 100_000] {
            let p = vec![b'x'; n];
            let f = server_frame(true, 1, &p);
            for cut in [0, 1, f.len().min(3), f.len() - 1] {
                if cut < f.len() {
                    assert_eq!(parse_frame(&f[..cut], 1 << 20).unwrap(), None, "n={n} cut={cut}");
                }
            }
            let (fr, used) = parse_frame(&f, 1 << 20).unwrap().unwrap();
            assert_eq!((fr.payload.len(), used, fr.opcode, fr.fin), (n, f.len(), Opcode::Text, true));
        }
    }

    #[test]
    fn rejects_violations() {
        assert!(parse_frame(&[0x81, 0x80, 0, 0, 0, 0], 100).is_err()); // masked
        assert!(parse_frame(&[0xC1, 0x00], 100).is_err()); // rsv1
        assert!(parse_frame(&[0x83, 0x00], 100).is_err()); // opcode 3
        assert!(parse_frame(&[0x09, 0x00], 100).is_err()); // fragmented ping
        assert_eq!(parse_frame(&server_frame(true, 2, &[0; 200]), 100), Err(WsError::TooLarge(200)));
    }

    #[test]
    fn client_frame_is_masked_and_reversible() {
        let mut out = vec![];
        let mask = [1, 2, 3, 4];
        encode_client_frame(Opcode::Text, b"hello", mask, &mut out);
        assert_eq!(&out[..2], &[0x81, 0x85]);
        assert_eq!(&out[2..6], &mask);
        let unmasked: Vec<u8> = out[6..].iter().enumerate().map(|(i, b)| b ^ mask[i & 3]).collect();
        assert_eq!(unmasked, b"hello");
    }

    #[test]
    fn handshake_response_validation() {
        let acc = accept_key("dGhlIHNhbXBsZSBub25jZQ==");
        let ok = format!("HTTP/1.1 101 Switching Protocols\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Accept: {acc}\r\n\r\nEXTRA");
        assert_eq!(parse_handshake_response(ok.as_bytes(), &acc).unwrap(), Some(ok.len() - 5));
        assert_eq!(parse_handshake_response(&ok.as_bytes()[..20], &acc).unwrap(), None);
        assert!(parse_handshake_response(ok.as_bytes(), "wrong").is_err());
        assert!(parse_handshake_response(b"HTTP/1.1 403 Forbidden\r\n\r\n", &acc).is_err());
    }
}
