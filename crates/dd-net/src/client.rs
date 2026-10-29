//! TLS + WebSocket client state machine over an [`Io`] driver.

use std::net::{TcpStream, ToSocketAddrs};
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};

use rustls::client::UnbufferedClientConnection;
use rustls::pki_types::ServerName;
use rustls::unbuffered::ConnectionState;
use rustls::{ClientConfig, RootCertStore};

use crate::buf::ByteBuf;
use crate::driver::{Io, IoBackend};
use crate::error::NetError;
use crate::url::WsUrl;
use crate::ws::{self, Opcode};

#[derive(Debug, Clone)]
pub struct ClientOptions {
    pub io: IoBackend,
    /// Largest accepted WebSocket message (after reassembly).
    pub max_message: usize,
    /// Budget for DNS + TCP + TLS + HTTP upgrade.
    pub connect_timeout: Duration,
    /// TLS config override (tests, pinned roots). Defaults to Mozilla roots.
    pub tls: Option<Arc<ClientConfig>>,
}

impl Default for ClientOptions {
    fn default() -> Self {
        Self { io: IoBackend::Std, max_message: 1 << 20, connect_timeout: Duration::from_secs(10), tls: None }
    }
}

/// Shared default TLS config: ring crypto, TLS 1.2/1.3, Mozilla roots, ALPN http/1.1.
pub fn default_tls_config() -> Arc<ClientConfig> {
    static CFG: OnceLock<Arc<ClientConfig>> = OnceLock::new();
    CFG.get_or_init(|| {
        let roots = RootCertStore { roots: webpki_roots::TLS_SERVER_ROOTS.to_vec() };
        let mut cfg = ClientConfig::builder_with_provider(Arc::new(rustls::crypto::ring::default_provider()))
            .with_safe_default_protocol_versions()
            .expect("ring supports default protocol versions")
            .with_root_certificates(roots)
            .with_no_client_auth();
        cfg.alpn_protocols = vec![b"http/1.1".to_vec()];
        Arc::new(cfg)
    })
    .clone()
}

#[derive(Debug, Default, Clone, Copy)]
pub struct NetStats {
    pub bytes_in: u64,
    pub reads: u64,
    pub messages: u64,
    pub pings: u64,
}

#[derive(Debug, PartialEq, Eq)]
enum Phase {
    Tls,
    Upgrading,
    Open,
}

/// Size of the TLS ciphertext receive buffer: room for several max-size records.
const RX_CAP: usize = 256 * 1024;
/// Staging for encrypted outbound records.
const TLS_OUT_CAP: usize = 64 * 1024;

pub struct WsClient {
    io: Io,
    tls: UnbufferedClientConnection,
    tls_out: Box<[u8]>,
    tls_out_len: usize,
    /// Decrypted bytes awaiting WebSocket framing.
    plain_in: ByteBuf,
    /// Pending plaintext to send (upgrade request, subscriptions, pongs).
    plain_out: Vec<u8>,
    /// Reassembly for fragmented messages; capacity reserved up front.
    frag: Vec<u8>,
    frag_active: bool,
    phase: Phase,
    accept: String,
    max_message: usize,
    /// Monotonic ns of the read that delivered the bytes currently being parsed.
    last_rx_ns: u64,
    stats: NetStats,
}

impl std::fmt::Debug for WsClient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WsClient").field("phase", &self.phase).field("stats", &self.stats).finish_non_exhaustive()
    }
}

fn random<const N: usize>() -> [u8; N] {
    let mut b = [0u8; N];
    rustls::crypto::ring::default_provider().secure_random.fill(&mut b).expect("system RNG available");
    b
}

impl WsClient {
    /// Cold path: resolve, connect, TLS handshake, HTTP upgrade. Blocks up to
    /// `opts.connect_timeout`. Messages that arrive with the upgrade response
    /// are kept and delivered by the first [`poll`](Self::poll).
    pub fn connect(url: &str, opts: &ClientOptions) -> Result<Self, NetError> {
        let url = WsUrl::parse(url)?;
        let deadline = Instant::now() + opts.connect_timeout;
        let sock = connect_tcp(&url, opts.connect_timeout)?;
        sock.set_nodelay(true)?;
        let io = Io::new(sock, opts.io, RX_CAP)?;
        let name = ServerName::try_from(url.host.clone()).map_err(|_| NetError::Url(format!("bad host {}", url.host)))?;
        let tls = UnbufferedClientConnection::new(opts.tls.clone().unwrap_or_else(default_tls_config), name)?;
        let key = ws::make_key(random());
        let mut plain_out = Vec::with_capacity(4096);
        ws::write_handshake_request(&url.host, url.port, &url.path, &key, &mut plain_out);

        let mut c = Self {
            io,
            tls,
            tls_out: vec![0u8; TLS_OUT_CAP].into_boxed_slice(),
            tls_out_len: 0,
            plain_in: ByteBuf::new(opts.max_message * 2 + RX_CAP),
            plain_out,
            frag: Vec::with_capacity(opts.max_message),
            frag_active: false,
            phase: Phase::Tls,
            accept: ws::accept_key(&key),
            max_message: opts.max_message,
            last_rx_ns: 0,
            stats: NetStats::default(),
        };
        while c.phase != Phase::Open {
            if Instant::now() > deadline {
                return Err(NetError::Timeout);
            }
            let before = c.stats.reads;
            c.pump()?;
            if c.phase == Phase::Upgrading
                && let Some(n) = ws::parse_handshake_response(c.plain_in.data(), &c.accept)?
            {
                c.plain_in.consume(n);
                c.phase = Phase::Open;
            }
            if c.stats.reads == before {
                std::thread::sleep(Duration::from_micros(50));
            }
        }
        Ok(c)
    }

    pub fn stats(&self) -> NetStats {
        self.stats
    }

    /// Queues a text message and flushes it.
    pub fn send_text(&mut self, msg: &str) -> Result<(), NetError> {
        ws::encode_client_frame(Opcode::Text, msg.as_bytes(), random(), &mut self.plain_out);
        self.pump()
    }

    /// Queues a binary message (e.g. a serialized protobuf) and flushes it.
    pub fn send_binary(&mut self, msg: &[u8]) -> Result<(), NetError> {
        ws::encode_client_frame(Opcode::Binary, msg, random(), &mut self.plain_out);
        self.pump()
    }

    /// Hot path. Non-blocking: reads whatever is available, decrypts in place,
    /// and invokes `on_msg(payload, recv_ns)` for each complete text/binary
    /// message. Pings are answered automatically. Returns messages delivered.
    #[inline]
    pub fn poll<F: FnMut(&[u8], u64)>(&mut self, mut on_msg: F) -> Result<usize, NetError> {
        self.pump()?;
        let n = self.deliver(&mut on_msg)?;
        if !self.plain_out.is_empty() {
            self.pump()?;
        }
        Ok(n)
    }

    /// Best-effort graceful close (WebSocket close frame + TLS close_notify).
    pub fn close(mut self) {
        ws::encode_client_frame(Opcode::Close, &1000u16.to_be_bytes(), random(), &mut self.plain_out);
        let _ = self.pump();
        let st = self.tls.process_tls_records(self.io.rx.data_mut());
        if let Ok(ConnectionState::WriteTraffic(mut w)) = st.state
            && let Ok(n) = w.queue_close_notify(&mut self.tls_out)
        {
            let _ = self.io.send_all(&self.tls_out[..n]);
        }
    }

    /// Drives the TLS state machine until it needs more input; performs at
    /// most one successful socket read per call to keep delivery prompt.
    fn pump(&mut self) -> Result<(), NetError> {
        let mut received = false;
        loop {
            let status = self.tls.process_tls_records(self.io.rx.data_mut());
            let mut discard = status.discard;
            let mut need_data = false;
            match status.state? {
                ConnectionState::ReadTraffic(mut r) => {
                    while let Some(rec) = r.next_record() {
                        let rec = rec?;
                        discard += rec.discard;
                        if !self.plain_in.extend(rec.payload) {
                            return Err(NetError::Buffer("plaintext buffer"));
                        }
                    }
                }
                ConnectionState::EncodeTlsData(mut e) => {
                    self.tls_out_len += e.encode(&mut self.tls_out[self.tls_out_len..])?;
                }
                ConnectionState::TransmitTlsData(t) => {
                    self.io.send_all(&self.tls_out[..self.tls_out_len])?;
                    self.tls_out_len = 0;
                    t.done();
                }
                ConnectionState::BlockedHandshake => need_data = true,
                ConnectionState::WriteTraffic(mut w) => {
                    if self.phase == Phase::Tls {
                        self.phase = Phase::Upgrading; // upgrade request is already queued
                    }
                    if !self.plain_out.is_empty() {
                        let n = w.encrypt(&self.plain_out, &mut self.tls_out)?;
                        self.io.send_all(&self.tls_out[..n])?;
                        self.plain_out.clear();
                    }
                    need_data = true;
                }
                ConnectionState::PeerClosed | ConnectionState::Closed => {
                    return Err(NetError::Closed { code: None });
                }
                _ => return Err(NetError::Tls(rustls::Error::General("unexpected TLS state".into()))),
            }
            self.io.rx.consume(discard);
            if need_data {
                if received {
                    return Ok(());
                }
                let n = match self.io.poll_recv() {
                    Ok(n) => n,
                    Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => return Err(NetError::Closed { code: None }),
                    Err(e) => return Err(e.into()),
                };
                if n == 0 {
                    return Ok(());
                }
                self.last_rx_ns = dd_core::clock::now_ns();
                self.stats.bytes_in += n as u64;
                self.stats.reads += 1;
                received = true;
            }
        }
    }

    /// Parses buffered plaintext into frames and dispatches them.
    fn deliver<F: FnMut(&[u8], u64)>(&mut self, on_msg: &mut F) -> Result<usize, NetError> {
        let mut delivered = 0;
        while let Some((f, used)) = ws::parse_frame(self.plain_in.data(), self.max_message)? {
            match f.opcode {
                Opcode::Text | Opcode::Binary if !self.frag_active => {
                    if f.fin {
                        on_msg(f.payload, self.last_rx_ns); // zero-copy fast path
                        delivered += 1;
                    } else {
                        self.frag.clear();
                        self.frag.extend_from_slice(f.payload);
                        self.frag_active = true;
                    }
                }
                Opcode::Text | Opcode::Binary => return Err(ws::WsError::Protocol("data frame inside fragmented message").into()),
                Opcode::Continuation => {
                    if !self.frag_active {
                        return Err(ws::WsError::Protocol("continuation without start").into());
                    }
                    if self.frag.len() + f.payload.len() > self.max_message {
                        return Err(ws::WsError::TooLarge((self.frag.len() + f.payload.len()) as u64).into());
                    }
                    self.frag.extend_from_slice(f.payload);
                    if f.fin {
                        self.frag_active = false;
                        on_msg(&self.frag, self.last_rx_ns);
                        delivered += 1;
                    }
                }
                Opcode::Ping => {
                    self.stats.pings += 1;
                    ws::encode_client_frame(Opcode::Pong, f.payload, random(), &mut self.plain_out);
                }
                Opcode::Pong => {}
                Opcode::Close => {
                    let code = f.payload.get(..2).map(|b| u16::from_be_bytes([b[0], b[1]]));
                    ws::encode_client_frame(Opcode::Close, f.payload.get(..2).unwrap_or_default(), random(), &mut self.plain_out);
                    let _ = self.pump();
                    return Err(NetError::Closed { code });
                }
            }
            self.plain_in.consume(used);
        }
        self.stats.messages += delivered as u64;
        Ok(delivered)
    }
}

fn connect_tcp(url: &WsUrl, timeout: Duration) -> Result<TcpStream, NetError> {
    let mut last = None;
    for addr in (url.host.as_str(), url.port).to_socket_addrs()? {
        match TcpStream::connect_timeout(&addr, timeout) {
            Ok(s) => return Ok(s),
            Err(e) => last = Some(e),
        }
    }
    Err(last.map_or_else(|| NetError::Url(format!("{} did not resolve", url.host)), NetError::Io))
}
