//! A TCI server (Expert Electronics "Transceiver Control Interface", text commands over a
//! WebSocket) for logging and digital-mode programs that speak TCI instead of rigctld.
//!
//! Subset implemented, from the public TCI protocol description: on connect the server sends
//! `protocol`, `device`, `receive_only`, `trx_count`, `channels_count`, `vfo_limits`,
//! `modulations_list` and `ready`; clients may read or set `vfo:0,0[,hz];`,
//! `modulation:0[,mode];` and `trx:0[,true|false];`; the server pushes `vfo`, `modulation` and
//! `trx` when they change. Safe defaults as for rigctld: loopback only, transmit refused unless
//! allowed. wfview's TCI server listens on every interface with no authentication; this one does not.

use crate::engine::Engine;
use crate::rig::{Mode, ModeSetting, RigKind};
use crate::rigctld::EngineSlot;
use std::net::{IpAddr, Ipv4Addr, SocketAddr, TcpListener};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;
use tungstenite::Message;

#[derive(Debug, Clone)]
pub struct TciConfig {
    pub port: u16,
    pub lan: bool,
    pub allow: Vec<IpAddr>,
    pub allow_tx: bool,
    pub tx_limit: Duration,
}

impl Default for TciConfig {
    fn default() -> Self {
        Self { port: 40001, lan: false, allow: Vec::new(), allow_tx: false, tx_limit: Duration::from_secs(120) }
    }
}

pub struct TciServer {
    pub local_addr: SocketAddr,
    stop: Arc<AtomicBool>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl TciServer {
    pub fn start(slot: EngineSlot, cfg: TciConfig) -> std::io::Result<Self> {
        let ip: IpAddr = if cfg.lan { Ipv4Addr::UNSPECIFIED.into() } else { Ipv4Addr::LOCALHOST.into() };
        let listener = TcpListener::bind(SocketAddr::new(ip, cfg.port))?;
        listener.set_nonblocking(true)?;
        let local_addr = listener.local_addr()?;
        let stop = Arc::new(AtomicBool::new(false));
        let s = stop.clone();
        let thread = std::thread::Builder::new().name("tci".into()).spawn(move || {
            while !s.load(Ordering::Relaxed) {
                match listener.accept() {
                    Ok((stream, peer)) => {
                        if !(peer.ip().is_loopback() || (cfg.lan && cfg.allow.contains(&peer.ip()))) {
                            continue;
                        }
                        let (slot, cfg, s) = (slot.clone(), cfg.clone(), s.clone());
                        let _ = std::thread::Builder::new().name("tci-client".into()).spawn(move || {
                            let _ = stream.set_nonblocking(false);
                            serve(stream, slot, cfg, s)
                        });
                    }
                    Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => std::thread::sleep(Duration::from_millis(20)),
                    Err(_) => std::thread::sleep(Duration::from_millis(100)),
                }
            }
        })?;
        Ok(Self { local_addr, stop, thread: Some(thread) })
    }

    pub fn stop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

impl Drop for TciServer {
    fn drop(&mut self) {
        self.stop();
    }
}

fn tci_mode(m: ModeSetting) -> &'static str {
    match (m.mode, m.data) {
        (Mode::Lsb, true) => "digl",
        (Mode::Usb, true) => "digu",
        (Mode::Lsb, _) => "lsb",
        (Mode::Usb, _) => "usb",
        (Mode::Am | Mode::AmN, _) => "am",
        (Mode::Cw | Mode::CwR, _) => "cw",
        (Mode::Fm | Mode::FmN | Mode::C4fm, _) => "nfm",
        (Mode::Rtty | Mode::RttyR, _) => "rtty",
    }
}

fn from_tci_mode(s: &str) -> Option<ModeSetting> {
    let (mode, data) = match s.trim().to_ascii_lowercase().as_str() {
        "lsb" => (Mode::Lsb, false),
        "usb" => (Mode::Usb, false),
        "digl" => (Mode::Lsb, true),
        "digu" => (Mode::Usb, true),
        "am" | "sam" => (Mode::Am, false),
        "cw" => (Mode::Cw, false),
        "nfm" | "fm" => (Mode::Fm, false),
        "rtty" => (Mode::Rtty, false),
        _ => return None,
    };
    Some(ModeSetting { mode, filter: None, data })
}

fn greeting(engine: Option<&Engine>) -> Vec<String> {
    let caps = engine.map(|e| e.capabilities().clone()).unwrap_or_else(|| RigKind::IcomIc7300.capabilities());
    vec![
        "protocol:RigDeck,1.8;".into(),
        format!("device:{};", caps.kind.name().split(' ').next_back().unwrap_or("radio")),
        "receive_only:false;".into(),
        "trx_count:1;".into(),
        "channels_count:1;".into(),
        format!("vfo_limits:{},{};", caps.min_freq_hz, caps.max_freq_hz),
        "modulations_list:am,lsb,usb,cw,nfm,digl,digu,rtty;".into(),
        "ready;".into(),
    ]
}

/// Handle one TCI command (without the trailing `;`). Returns the reply, if any.
pub fn handle(cmd: &str, engine: Option<&Engine>, cfg: &TciConfig) -> Option<String> {
    let (name, args) = cmd.split_once(':').unwrap_or((cmd, ""));
    let args: Vec<&str> = if args.is_empty() { vec![] } else { args.split(',').map(str::trim).collect() };
    let e = engine.filter(|e| e.is_running())?;
    match name.trim().to_ascii_lowercase().as_str() {
        "vfo" => {
            if let Some(hz) = args.get(2).and_then(|v| v.parse::<f64>().ok()) {
                e.set_frequency(hz.round() as u64).ok()?;
            }
            let f = e.state().frequency_hz.map(Ok).unwrap_or_else(|| e.read_frequency()).ok()?;
            Some(format!("vfo:0,0,{f};"))
        }
        "modulation" => {
            if let Some(m) = args.get(1).and_then(|m| from_tci_mode(m)) {
                let filter = e.state().mode.and_then(|c| c.filter);
                e.set_mode(ModeSetting { filter, ..m }).ok()?;
            }
            let m = e.state().mode.map(Ok).unwrap_or_else(|| e.read_mode()).ok()?;
            Some(format!("modulation:0,{};", tci_mode(m)))
        }
        "trx" => {
            match args.get(1).map(|v| v.to_ascii_lowercase()) {
                Some(v) if v == "true" && cfg.allow_tx => {
                    let _ = e.transmit(true, cfg.tx_limit);
                }
                Some(v) if v == "false" => {
                    let _ = e.transmit(false, Duration::ZERO);
                }
                _ => {}
            }
            Some(format!("trx:0,{};", e.state().tx))
        }
        _ => None,
    }
}

fn serve(stream: std::net::TcpStream, slot: EngineSlot, cfg: TciConfig, stop: Arc<AtomicBool>) {
    let Ok(mut ws) = tungstenite::accept(stream) else { return };
    let _ = ws.get_ref().set_read_timeout(Some(Duration::from_millis(200)));
    let engine = slot.read().unwrap().clone();
    for g in greeting(engine.as_deref()) {
        if ws.send(Message::text(g)).is_err() {
            return;
        }
    }
    let mut last = (None, None, None);
    while !stop.load(Ordering::Relaxed) {
        match ws.read() {
            Ok(Message::Text(t)) => {
                let engine = slot.read().unwrap().clone();
                for cmd in t.as_str().split(';').map(str::trim).filter(|c| !c.is_empty()) {
                    if let Some(r) = handle(cmd, engine.as_deref(), &cfg)
                        && ws.send(Message::text(r)).is_err()
                    {
                        return;
                    }
                }
            }
            Ok(Message::Close(_)) => return,
            Ok(_) => {}
            Err(tungstenite::Error::Io(e)) if matches!(e.kind(), std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut) => {}
            Err(_) => return,
        }
        // Push state changes.
        if let Some(e) = slot.read().unwrap().clone() {
            let st = e.state();
            let now = (st.frequency_hz, st.mode.map(tci_mode), Some(st.tx));
            let mut out = Vec::new();
            if now.0 != last.0
                && let Some(f) = now.0
            {
                out.push(format!("vfo:0,0,{f};"));
            }
            if now.1 != last.1
                && let Some(m) = now.1
            {
                out.push(format!("modulation:0,{m};"));
            }
            if now.2 != last.2 {
                out.push(format!("trx:0,{};", st.tx));
            }
            last = now;
            for m in out {
                if ws.send(Message::text(m)).is_err() {
                    return;
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn modes_round_trip() {
        for s in ["lsb", "usb", "digl", "digu", "am", "cw", "nfm", "rtty"] {
            assert_eq!(tci_mode(from_tci_mode(s).unwrap()), s);
        }
    }

    #[test]
    fn greeting_ends_with_ready() {
        let g = greeting(None);
        assert_eq!(g.last().unwrap(), "ready;");
        assert!(g.iter().any(|l| l == "device:IC-7300;"));
    }
}
