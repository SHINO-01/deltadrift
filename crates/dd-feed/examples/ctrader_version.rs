//! Credential-free connectivity check against cTrader Open API:
//! sends ProtoOAVersionReq over WebSocket and prints the response.
//! `cargo run --release -p dd-feed --example ctrader_version [demo|live]`

use dd_feed::proto::{Reader, Writer};
use dd_net::{ClientOptions, WsClient};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let env = std::env::args().nth(1).unwrap_or_else(|| "demo".into());
    dd_core::clock::init();
    let mut c = WsClient::connect(&format!("wss://{env}.ctraderapi.com:5035"), &ClientOptions::default())?;
    // ProtoMessage { payloadType = 2104 (PROTO_OA_VERSION_REQ) }
    c.send_binary(&Writer::new().uint(1, 2104).finish())?;
    let mut done = false;
    let t = std::time::Instant::now();
    while !done && t.elapsed().as_secs() < 10 {
        c.poll(|m, _| {
            let mut pt = 0;
            let mut payload: &[u8] = &[];
            for (f, v) in Reader::new(m).flatten() {
                match f {
                    1 => pt = v.u64().unwrap_or(0),
                    2 => payload = v.bytes().unwrap_or_default(),
                    _ => {}
                }
            }
            let version = Reader::new(payload).flatten().find(|(f, _)| *f == 2).and_then(|(_, v)| v.str());
            println!("payloadType={pt} version={version:?} ({} bytes)", m.len());
            done = pt == 2105;
        })?;
    }
    c.close();
    Ok(())
}
