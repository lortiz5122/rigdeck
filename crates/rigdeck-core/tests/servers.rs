//! rigctld and TCI servers against the simulated IC-7300, over real TCP sockets.

use rigdeck_core::engine::{Engine, EngineConfig};
use rigdeck_core::rig::RigKind;
use rigdeck_core::rigctld::{RigctldConfig, RigctldServer};
use rigdeck_core::sim::{Ic7300Sim, SimHandle};
use rigdeck_core::tci::{TciConfig, TciServer};
use rigdeck_core::transport::pipe_pair;
use std::io::{BufRead, BufReader, Write};
use std::sync::{Arc, RwLock};
use std::time::Duration;

fn setup() -> (rigdeck_core::rigctld::EngineSlot, SimHandle<Ic7300Sim>) {
    let (a, b) = pipe_pair();
    let sim = SimHandle::spawn(Ic7300Sim::default(), Box::new(b));
    let p = rigdeck_core::driver(RigKind::IcomIc7300);
    let e = Arc::new(Engine::start(Box::new(a), p, EngineConfig::default_for(&RigKind::IcomIc7300.capabilities())));
    (Arc::new(RwLock::new(Some(e))), sim)
}

#[test]
fn rigctld_over_tcp() {
    let (slot, sim) = setup();
    let srv = RigctldServer::start(slot.clone(), RigctldConfig { port: 0, ..Default::default() }).unwrap();
    assert!(srv.local_addr.ip().is_loopback());
    let s = std::net::TcpStream::connect(srv.local_addr).unwrap();
    s.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
    let mut w = s.try_clone().unwrap();
    let mut r = BufReader::new(s);
    let mut ask = |cmd: &str, lines: usize| -> Vec<String> {
        w.write_all(format!("{cmd}\n").as_bytes()).unwrap();
        (0..lines)
            .map(|_| {
                let mut l = String::new();
                r.read_line(&mut l).unwrap();
                l.trim_end().to_string()
            })
            .collect()
    };
    assert_eq!(ask("f", 1), vec!["14074000"]);
    assert_eq!(ask("F 7074000", 1), vec!["RPRT 0"]);
    assert_eq!(sim.radio.lock().unwrap().frequency_hz, 7_074_000);
    assert_eq!(ask("M USB 3000", 1), vec!["RPRT 0"]);
    assert_eq!(ask("m", 2), vec!["USB", "3000"]);
    assert_eq!(ask("T 1", 1), vec!["RPRT -9"], "transmit must be refused by default");
    assert!(!sim.radio.lock().unwrap().tx);
    // Disconnected radio: the server stays up and reports an I/O error.
    *slot.write().unwrap() = None;
    assert_eq!(ask("f", 1), vec!["RPRT -6"]);
}

#[test]
fn rigctld_transmit_when_allowed_has_a_time_limit() {
    let (slot, sim) = setup();
    let cfg = RigctldConfig { port: 0, allow_tx: true, tx_limit: Duration::from_millis(400), ..Default::default() };
    let srv = RigctldServer::start(slot, cfg).unwrap();
    let mut s = std::net::TcpStream::connect(srv.local_addr).unwrap();
    s.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
    s.write_all(b"T 1\n").unwrap();
    let mut l = String::new();
    BufReader::new(s.try_clone().unwrap()).read_line(&mut l).unwrap();
    assert_eq!(l.trim(), "RPRT 0");
    assert!(sim.radio.lock().unwrap().tx);
    std::thread::sleep(Duration::from_millis(900));
    assert!(!sim.radio.lock().unwrap().tx, "watchdog must unkey");
}

#[test]
fn tci_over_websocket() {
    let (slot, sim) = setup();
    let srv = TciServer::start(slot, TciConfig { port: 0, ..Default::default() }).unwrap();
    let (mut ws, _) = tungstenite::connect(format!("ws://{}", srv.local_addr)).unwrap();
    let mut greeting = Vec::new();
    loop {
        let m = ws.read().unwrap().into_text().unwrap().to_string();
        greeting.push(m.clone());
        if m == "ready;" {
            break;
        }
    }
    assert!(greeting.contains(&"device:IC-7300;".to_string()));
    ws.send(tungstenite::Message::text("vfo:0,0,21074000;")).unwrap();
    let mut got = Vec::new();
    for _ in 0..6 {
        let m = ws.read().unwrap().into_text().unwrap().to_string();
        got.push(m.clone());
        if m == "vfo:0,0,21074000;" {
            break;
        }
    }
    assert!(got.contains(&"vfo:0,0,21074000;".to_string()), "{got:?}");
    assert_eq!(sim.radio.lock().unwrap().frequency_hz, 21_074_000);
    ws.send(tungstenite::Message::text("trx:0,true;")).unwrap();
    let mut refused = false;
    for _ in 0..6 {
        if ws.read().unwrap().into_text().unwrap().as_str() == "trx:0,false;" {
            refused = true;
            break;
        }
    }
    assert!(refused);
    assert!(!sim.radio.lock().unwrap().tx);
}
