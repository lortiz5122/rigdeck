//! A Hamlib `rigctld`-compatible network server, so WSJT-X and loggers can use the radio through
//! RigDeck ("Hamlib NET rigctl", model 2).
//!
//! Protocol: one command per line, answers as plain lines, `RPRT <code>` for set commands
//! (Hamlib `rigctld` man page; the `\dump_state` layout was checked against Hamlib 4.7.2's own
//! `rigctld -m 1`). Safe defaults: loopback only, transmit off until enabled, and in LAN mode
//! only the listed addresses are served.

use crate::engine::{Engine, EngineError};
use crate::rig::{Event, Mode, ModeSetting, Request};
use std::io::{BufRead, BufReader, Write};
use std::net::{IpAddr, Ipv4Addr, SocketAddr, TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, RwLock};
use std::time::Duration;

/// The engine the servers talk to; `None` while disconnected. Swapped on reconnect.
pub type EngineSlot = Arc<RwLock<Option<Arc<Engine>>>>;

#[derive(Debug, Clone)]
pub struct RigctldConfig {
    pub port: u16,
    /// Listen on every interface (LAN). Off = 127.0.0.1 only.
    pub lan: bool,
    /// In LAN mode, the remote addresses allowed (loopback is always allowed).
    pub allow: Vec<IpAddr>,
    /// Allow `T 1` (transmit). Off by default.
    pub allow_tx: bool,
    /// Longest transmission a client can start.
    pub tx_limit: Duration,
}

impl Default for RigctldConfig {
    fn default() -> Self {
        Self { port: 4533, lan: false, allow: Vec::new(), allow_tx: false, tx_limit: Duration::from_secs(120) }
    }
}

pub struct RigctldServer {
    pub local_addr: SocketAddr,
    stop: Arc<AtomicBool>,
    pub clients_served: Arc<AtomicU64>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl RigctldServer {
    pub fn start(slot: EngineSlot, cfg: RigctldConfig) -> std::io::Result<Self> {
        let ip: IpAddr = if cfg.lan { Ipv4Addr::UNSPECIFIED.into() } else { Ipv4Addr::LOCALHOST.into() };
        let listener = TcpListener::bind(SocketAddr::new(ip, cfg.port))?;
        listener.set_nonblocking(true)?;
        let local_addr = listener.local_addr()?;
        let stop = Arc::new(AtomicBool::new(false));
        let served = Arc::new(AtomicU64::new(0));
        let (s, n) = (stop.clone(), served.clone());
        let thread = std::thread::Builder::new().name("rigctld".into()).spawn(move || {
            while !s.load(Ordering::Relaxed) {
                match listener.accept() {
                    Ok((stream, peer)) => {
                        if !allowed(&cfg, peer.ip()) {
                            drop(stream);
                            continue;
                        }
                        n.fetch_add(1, Ordering::Relaxed);
                        let (slot, cfg, s) = (slot.clone(), cfg.clone(), s.clone());
                        let _ = std::thread::Builder::new().name("rigctld-client".into()).spawn(move || serve_client(stream, slot, cfg, s));
                    }
                    Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => std::thread::sleep(Duration::from_millis(20)),
                    Err(_) => std::thread::sleep(Duration::from_millis(100)),
                }
            }
        })?;
        Ok(Self { local_addr, stop, clients_served: served, thread: Some(thread) })
    }

    pub fn stop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

impl Drop for RigctldServer {
    fn drop(&mut self) {
        self.stop();
    }
}

fn allowed(cfg: &RigctldConfig, ip: IpAddr) -> bool {
    ip.is_loopback() || (cfg.lan && cfg.allow.contains(&ip))
}

// Hamlib error codes (rig.h): RIG_OK 0, EINVAL -1, ENIMPL -4, ETIMEOUT -5, EIO -6, ERJCTED -9, ENAVAIL -11.
const OK: i32 = 0;
const EINVAL: i32 = -1;
const ENIMPL: i32 = -4;
const ETIMEOUT: i32 = -5;
const EIO: i32 = -6;
const ERJCTED: i32 = -9;
const ENAVAIL: i32 = -11;

fn code(e: &EngineError) -> i32 {
    match e {
        EngineError::Unsupported(_) => ENAVAIL,
        EngineError::Refused(_) | EngineError::Mismatch(_) => ERJCTED,
        EngineError::Timeout(_) => ETIMEOUT,
        EngineError::Io(_) | EngineError::Stopped => EIO,
    }
}

// Hamlib mode bits (rig.h RIG_MODE_*).
fn mode_bits(m: Mode, data: bool) -> u64 {
    match (m, data) {
        (Mode::Lsb, true) => 0x400,
        (Mode::Usb, true) => 0x800,
        (Mode::Fm, true) => 0x1000,
        (Mode::Am | Mode::AmN, _) => 0x1,
        (Mode::Cw, _) => 0x2,
        (Mode::Usb, _) => 0x4,
        (Mode::Lsb, _) => 0x8,
        (Mode::Rtty, _) => 0x10,
        (Mode::Fm | Mode::FmN | Mode::C4fm, _) => 0x20,
        (Mode::CwR, _) => 0x80,
        (Mode::RttyR, _) => 0x100,
    }
}

fn passband(m: Mode) -> u32 {
    match m {
        Mode::Cw | Mode::CwR | Mode::Rtty | Mode::RttyR => 500,
        Mode::Am => 6000,
        Mode::AmN => 3000,
        Mode::Fm => 15000,
        Mode::FmN | Mode::C4fm => 10000,
        Mode::Lsb | Mode::Usb => 3000,
    }
}

/// Hamlib `STRENGTH` is dB relative to S9. Icom scale: 0 = S0 (-54 dB), 120 = S9, 241 = S9+60.
pub fn strength_db(raw: u16) -> i32 {
    let r = f64::from(raw);
    if r <= 120.0 { (r / 120.0 * 54.0 - 54.0).round() as i32 } else { ((r - 120.0) / 121.0 * 60.0).round() as i32 }
}

fn dump_state(engine: Option<&Engine>) -> String {
    let caps = engine.map(|e| e.capabilities().clone()).unwrap_or_else(|| crate::rig::RigKind::IcomIc7300.capabilities());
    let modes: u64 = caps.modes.iter().map(|m| mode_bits(*m, false)).sum::<u64>() | 0x400 | 0x800 | 0x1000;
    let (lo, hi) = (caps.min_freq_hz as f64, caps.max_freq_hz as f64);
    let mut s = String::new();
    s += "1\n2\n0\n"; // protocol version, rig model (NET rigctl), ITU region
    s += &format!("{lo:.6} {hi:.6} 0x{modes:x} -1 -1 0x3 0x1\n0 0 0 0 0 0 0\n");
    s += &format!("{lo:.6} {hi:.6} 0x{modes:x} 5000 100000 0x3 0x1\n0 0 0 0 0 0 0\n");
    s += &format!("0x{modes:x} 1\n0 0\n");
    s += "0xc 3000\n0xc 2400\n0x82 500\n0x1 6000\n0x20 15000\n0x1c00 3000\n0 0\n";
    s += "9999\n9999\n0\n0\n\n\n";
    // has_get_func, has_set_func, has_get_level (STRENGTH|ALC|SWR|RAWSTR|RFPOWER), has_set_level (RFPOWER),
    // has_get_parm, has_set_parm
    s += "0x0\n0x0\n0x74001000\n0x1000\n0x0\n0x0\n";
    // Key=value section read by Hamlib 4.x clients up to "done" (layout as Hamlib 4.7.2 rigctld).
    s += "vfo_ops=0x0\nptt_type=0x1\ntargetable_vfo=0x0\nhas_set_vfo=1\nhas_get_vfo=1\nhas_set_freq=1\nhas_get_freq=1\n";
    s += "has_set_conf=0\nhas_get_conf=0\nhas_power2mW=0\nhas_mW2power=0\nhas_get_ant=0\nhas_set_ant=0\ntimeout=0\n";
    s += &format!("rig_model=2\nrigctld_version=RigDeck {}\ndone\n", env!("CARGO_PKG_VERSION"));
    s
}

fn serve_client(stream: TcpStream, slot: EngineSlot, cfg: RigctldConfig, stop: Arc<AtomicBool>) {
    // On BSD and macOS an accepted socket inherits O_NONBLOCK from the listener.
    let _ = stream.set_nonblocking(false);
    let _ = stream.set_read_timeout(Some(Duration::from_millis(500)));
    let _ = stream.set_nodelay(true);
    let Ok(mut out) = stream.try_clone() else { return };
    let mut reader = BufReader::new(stream);
    let mut line = String::new();
    while !stop.load(Ordering::Relaxed) {
        line.clear();
        match reader.read_line(&mut line) {
            Ok(0) => return,
            Ok(_) => {}
            Err(e) if matches!(e.kind(), std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut) => {
                // Keep a partial line; read_line appends.
                continue;
            }
            Err(_) => return,
        }
        let cmd = line.trim();
        if cmd.is_empty() {
            continue;
        }
        let engine = slot.read().unwrap().clone();
        let (reply, quit) = handle(cmd, engine.as_deref(), &cfg);
        if out.write_all(reply.as_bytes()).is_err() || quit {
            return;
        }
    }
}

/// Handle one command line; returns the answer and whether to close the connection.
pub fn handle(cmd: &str, engine: Option<&Engine>, cfg: &RigctldConfig) -> (String, bool) {
    let mut parts = cmd.split_whitespace();
    let verb = parts.next().unwrap_or("");
    let args: Vec<&str> = parts.collect();
    let rprt = |c: i32| format!("RPRT {c}\n");
    let need = |f: &dyn Fn(&Engine) -> String| -> String {
        match engine {
            Some(e) if e.is_running() => f(e),
            _ => rprt(EIO),
        }
    };
    let ans = match verb {
        "q" | "Q" | "\\quit" => return (String::new(), true),
        "\\dump_state" => dump_state(engine),
        "\\chk_vfo" => "0\n".into(),
        "\\get_powerstat" => "1\n".into(),
        "_" | "\\get_info" => "RigDeck\n".into(),
        "f" | "\\get_freq" => need(&|e| match e.state().frequency_hz.map(Ok).unwrap_or_else(|| e.read_frequency()) {
            Ok(f) => format!("{f}\n"),
            Err(err) => rprt(code(&err)),
        }),
        "F" | "\\set_freq" => need(&|e| match args.first().and_then(|a| a.parse::<f64>().ok()) {
            Some(hz) => match e.set_frequency(hz.round() as u64) {
                Ok(_) => rprt(OK),
                Err(err) => rprt(code(&err)),
            },
            None => rprt(EINVAL),
        }),
        "m" | "\\get_mode" => need(&|e| {
            let m = e.state().mode.map(Ok).unwrap_or_else(|| e.read_mode());
            match m {
                Ok(m) => format!("{}\n{}\n", m.mode.hamlib(m.data), passband(m.mode)),
                Err(err) => rprt(code(&err)),
            }
        }),
        "M" | "\\set_mode" => need(&|e| match args.first().and_then(|a| Mode::from_hamlib(a)) {
            Some((mode, data)) => {
                if !e.capabilities().modes.contains(&mode) {
                    return rprt(EINVAL);
                }
                // Keep the current filter; passband widths map to filters only on Icom.
                let filter = e.state().mode.and_then(|m| m.filter);
                match e.set_mode(ModeSetting { mode, data, filter }) {
                    Ok(_) => rprt(OK),
                    Err(err) => rprt(code(&err)),
                }
            }
            None => rprt(EINVAL),
        }),
        "t" | "\\get_ptt" => need(&|e| format!("{}\n", u8::from(e.state().tx))),
        "T" | "\\set_ptt" => need(&|e| match args.first().and_then(|a| a.parse::<u8>().ok()) {
            Some(0) => match e.transmit(false, Duration::ZERO) {
                Ok(_) => rprt(OK),
                Err(err) => rprt(code(&err)),
            },
            Some(_) if !cfg.allow_tx => rprt(ERJCTED),
            Some(_) => match e.transmit(true, cfg.tx_limit) {
                Ok(_) => rprt(OK),
                Err(err) => rprt(code(&err)),
            },
            None => rprt(EINVAL),
        }),
        "v" | "\\get_vfo" => "VFOA\n".into(),
        "V" | "\\set_vfo" => rprt(if args.first().is_some_and(|v| v.eq_ignore_ascii_case("VFOA") || v.eq_ignore_ascii_case("currVFO")) {
            OK
        } else {
            ENAVAIL
        }),
        "s" | "\\get_split_vfo" => "0\nVFOA\n".into(),
        "S" | "\\set_split_vfo" => rprt(if args.first() == Some(&"0") { OK } else { ENAVAIL }),
        "l" | "\\get_level" => need(&|e| {
            let st = e.state();
            match args.first().map(|s| s.to_ascii_uppercase()).as_deref() {
                Some("STRENGTH") => match st.s_meter.map(Ok).unwrap_or_else(|| e.read_s_meter()) {
                    Ok(v) => format!("{}\n", strength_db(v)),
                    Err(err) => rprt(code(&err)),
                },
                Some("RAWSTR") => format!("{}\n", st.s_meter.unwrap_or(0)),
                Some("RFPOWER") => match e.request(Request::ReadPowerLevel) {
                    Ok(r) => match r.first_event() {
                        Some(Event::PowerLevel(p)) => format!("{:.6}\n", f64::from(*p) / 100.0),
                        _ => rprt(EIO),
                    },
                    Err(err) => rprt(code(&err)),
                },
                Some("SWR") => format!("{:.6}\n", 1.0 + f64::from(st.swr.unwrap_or(0)) / 120.0 * 2.0),
                Some("ALC") => format!("{:.6}\n", f64::from(st.alc.unwrap_or(0)) / 120.0),
                _ => rprt(EINVAL),
            }
        }),
        "L" | "\\set_level" => {
            need(&|e| match (args.first().map(|s| s.to_ascii_uppercase()).as_deref(), args.get(1).and_then(|v| v.parse::<f64>().ok())) {
                (Some("RFPOWER"), Some(v)) if (0.0..=1.0).contains(&v) => match e.set_power((v * 100.0).round() as u16) {
                    Ok(_) => rprt(OK),
                    Err(err) => rprt(code(&err)),
                },
                _ => rprt(EINVAL),
            })
        }
        _ => rprt(ENIMPL),
    };
    (ans, false)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strength_scale() {
        assert_eq!(strength_db(0), -54);
        assert_eq!(strength_db(120), 0);
        assert_eq!(strength_db(241), 60);
    }

    #[test]
    fn works_without_a_radio() {
        let cfg = RigctldConfig::default();
        assert_eq!(handle("f", None, &cfg).0, "RPRT -6\n");
        assert!(handle("\\dump_state", None, &cfg).0.starts_with("1\n2\n0\n"));
        assert_eq!(handle("q", None, &cfg), (String::new(), true));
        assert_eq!(handle("\\bogus", None, &cfg).0, "RPRT -4\n");
    }

    #[test]
    fn lan_allow_list() {
        let mut cfg = RigctldConfig::default();
        let lan: IpAddr = "192.168.1.20".parse().unwrap();
        assert!(allowed(&cfg, "127.0.0.1".parse().unwrap()));
        assert!(!allowed(&cfg, lan));
        cfg.lan = true;
        assert!(!allowed(&cfg, lan));
        cfg.allow.push(lan);
        assert!(allowed(&cfg, lan));
    }
}
