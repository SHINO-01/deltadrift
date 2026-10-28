//! Connect to a wss:// endpoint and print messages. Dev tool.
//! `cargo run --release -p dd-net --example ws_probe -- <url> [count] [std|uring|sqpoll]`

use dd_net::{ClientOptions, IoBackend, WsClient};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = std::env::args().skip(1);
    let url = args.next().ok_or("usage: ws_probe <wss-url> [count] [std|uring|sqpoll]")?;
    let count: u64 = args.next().map_or(Ok(5), |s| s.parse())?;
    let io = match args.next().as_deref() {
        Some("uring") => IoBackend::Uring { sqpoll_cpu: None },
        Some("sqpoll") => IoBackend::Uring { sqpoll_cpu: Some(0) },
        _ => IoBackend::Std,
    };
    dd_core::clock::init();
    let t0 = std::time::Instant::now();
    let mut c = WsClient::connect(&url, &ClientOptions { io, ..Default::default() })?;
    eprintln!("connected via {io:?} in {:?}", t0.elapsed());
    let quiet = std::env::var_os("QUIET").is_some();
    let mut n = 0;
    let mut lat = Vec::new();
    while n < count {
        c.poll(|m, recv_ns| {
            lat.push(dd_core::clock::now_ns() - recv_ns);
            n += 1;
            if !quiet {
                println!("{}", String::from_utf8_lossy(&m[..m.len().min(200)]));
            }
        })?;
    }
    lat.sort_unstable();
    let q = |p: f64| lat[((lat.len() - 1) as f64 * p) as usize];
    eprintln!("read→message ns: p50 {} p90 {} p99 {} max {}", q(0.5), q(0.9), q(0.99), q(1.0));
    eprintln!("{:?}", c.stats());
    c.close();
    Ok(())
}
