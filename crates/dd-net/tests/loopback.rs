//! Full-stack loopback: real TCP + real TLS (self-signed) + WebSocket framing,
//! exercised over every I/O backend.

use std::io::{Read, Write};
use std::net::TcpListener;
use std::sync::Arc;
use std::time::{Duration, Instant};

use dd_net::{ClientOptions, IoBackend, NetError, WsClient, ws};
use rustls::pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer};

fn provider() -> Arc<rustls::crypto::CryptoProvider> {
    Arc::new(rustls::crypto::ring::default_provider())
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

/// Reads one masked client frame; returns (opcode, unmasked payload).
fn read_client_frame(s: &mut impl Read) -> (u8, Vec<u8>) {
    let mut h = [0u8; 2];
    s.read_exact(&mut h).unwrap();
    assert!(h[1] & 0x80 != 0, "client frames must be masked");
    let len = match h[1] & 0x7F {
        126 => {
            let mut b = [0u8; 2];
            s.read_exact(&mut b).unwrap();
            u16::from_be_bytes(b) as usize
        }
        127 => {
            let mut b = [0u8; 8];
            s.read_exact(&mut b).unwrap();
            u64::from_be_bytes(b) as usize
        }
        n => n as usize,
    };
    let mut mask = [0u8; 4];
    s.read_exact(&mut mask).unwrap();
    let mut p = vec![0u8; len];
    s.read_exact(&mut p).unwrap();
    p.iter_mut().enumerate().for_each(|(i, b)| *b ^= mask[i & 3]);
    (h[0] & 0x0F, p)
}

fn run(backend: IoBackend) {
    let ck = rcgen::generate_simple_self_signed(vec!["localhost".into()]).unwrap();
    let cert: CertificateDer<'static> = ck.cert.der().clone();
    let key = PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(ck.key_pair.serialize_der()));

    let server_cfg = Arc::new(
        rustls::ServerConfig::builder_with_provider(provider())
            .with_safe_default_protocol_versions()
            .unwrap()
            .with_no_client_auth()
            .with_single_cert(vec![cert.clone()], key)
            .unwrap(),
    );
    let mut roots = rustls::RootCertStore::empty();
    roots.add(cert).unwrap();
    let client_cfg = Arc::new(
        rustls::ClientConfig::builder_with_provider(provider())
            .with_safe_default_protocol_versions()
            .unwrap()
            .with_root_certificates(roots)
            .with_no_client_auth(),
    );

    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let big: Vec<u8> = (0..70_000u32).map(|i| (i % 251) as u8).collect();
    let big_srv = big.clone();

    let server = std::thread::spawn(move || {
        let (tcp, _) = listener.accept().unwrap();
        let conn = rustls::ServerConnection::new(server_cfg).unwrap();
        let mut s = rustls::StreamOwned::new(conn, tcp);
        // Read the upgrade request.
        let mut req = Vec::new();
        let mut b = [0u8; 1];
        while !req.ends_with(b"\r\n\r\n") {
            s.read_exact(&mut b).unwrap();
            req.push(b[0]);
        }
        let req = String::from_utf8(req).unwrap();
        assert!(req.starts_with("GET /feed?x=1 HTTP/1.1\r\n"), "{req}");
        let key = req.lines().find_map(|l| l.strip_prefix("Sec-WebSocket-Key: ")).unwrap();
        let resp = format!(
            "HTTP/1.1 101 Switching Protocols\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Accept: {}\r\n\r\n",
            ws::accept_key(key)
        );
        // Response and first message in one write: the client must keep the message.
        let mut out = resp.into_bytes();
        out.extend(server_frame(true, 1, b"hello"));
        s.write_all(&out).unwrap();
        s.flush().unwrap();

        // Client replies to "hello" with a subscription.
        assert_eq!(read_client_frame(&mut s), (1, b"subscribe".to_vec()));

        let mut out = server_frame(true, 9, b"p1");
        out.extend(server_frame(false, 1, b"frag-"));
        out.extend(server_frame(false, 0, b"ment"));
        out.extend(server_frame(true, 9, b"p2")); // control frame interleaved mid-message
        out.extend(server_frame(true, 0, b"ed"));
        out.extend(server_frame(true, 2, &big_srv)); // spans several TLS records
        s.write_all(&out).unwrap();
        s.flush().unwrap();

        assert_eq!(read_client_frame(&mut s), (0xA, b"p1".to_vec()));
        assert_eq!(read_client_frame(&mut s), (0xA, b"p2".to_vec()));

        s.write_all(&server_frame(true, 8, &1001u16.to_be_bytes())).unwrap();
        s.flush().unwrap();
        let (op, payload) = read_client_frame(&mut s);
        assert_eq!((op, payload.as_slice()), (8, &1001u16.to_be_bytes()[..]));
    });

    let opts = ClientOptions { io: backend, tls: Some(client_cfg), ..Default::default() };
    let mut c = WsClient::connect(&format!("wss://localhost:{port}/feed?x=1"), &opts).unwrap();
    let mut msgs: Vec<Vec<u8>> = Vec::new();
    let mut subscribed = false;
    let deadline = Instant::now() + Duration::from_secs(10);
    let err = loop {
        assert!(Instant::now() < deadline, "timed out; got {} msgs", msgs.len());
        match c.poll(|m, ts| {
            assert!(ts > 0);
            msgs.push(m.to_vec());
        }) {
            Ok(_) => {
                if !msgs.is_empty() && !subscribed {
                    c.send_text("subscribe").unwrap();
                    subscribed = true;
                }
            }
            Err(e) => break e,
        }
    };
    assert!(matches!(err, NetError::Closed { code: Some(1001) }), "{err}");
    assert_eq!(msgs.len(), 3);
    assert_eq!(msgs[0], b"hello");
    assert_eq!(msgs[1], b"frag-mented");
    assert_eq!(msgs[2], big);
    assert_eq!(c.stats().pings, 2);
    server.join().unwrap();
}

#[test]
fn loopback_std() {
    run(IoBackend::Std);
}

#[cfg(target_os = "linux")]
#[test]
fn loopback_uring() {
    run(IoBackend::Uring { sqpoll_cpu: None });
}

#[cfg(target_os = "linux")]
#[test]
fn loopback_uring_sqpoll() {
    run(IoBackend::Uring { sqpoll_cpu: Some(0) });
}
