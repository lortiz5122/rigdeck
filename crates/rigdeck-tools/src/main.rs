//! rigdeck-tools: command-line helpers for testing RigDeck against simulated and real radios.
//!
//! ```text
//! rigdeck-tools ports
//! rigdeck-tools sim ic7300|ft991a [--sweeps N] [--pixels N]
//! rigdeck-tools probe   --rig R [--port P] [--baud B]
//! rigdeck-tools control-check --rig R [--port P]
//! rigdeck-tools cycles  --rig R [--port P] [--count 50]
//! rigdeck-tools memories-read   --rig R [--port P] --range 1-55 --out file.csv
//! rigdeck-tools memories-verify --rig R [--port P] --csv a.csv [--csv b.csv ...]
//! rigdeck-tools memories-write  --rig R [--port P] --csv a.csv [--csv b.csv ...] --backup-dir DIR [--rewrite-unchanged]
//! rigdeck-tools scope-soak --rig R [--port P] --seconds N [--speed fast|mid|slow]
//! rigdeck-tools record  --rig R [--port P] --seconds N --out file.hex [--scope]
//! rigdeck-tools rigctld --rig R [--port P] [--listen 4533] [--lan] [--allow IP] [--allow-tx]
//! rigdeck-tools clock   --rig R [--port P] [--utc-offset-hours H]
//! ```
//! `R` is `ic7300` or `ft991a`. Without `--port` the radio is found by its USB description.

use rigdeck_core::engine::{Engine, EngineConfig};
use rigdeck_core::memory::{self, MemoryChannel};
use rigdeck_core::rig::{Event, Request, RigKind};
use rigdeck_core::rigctld::{RigctldConfig, RigctldServer};
use rigdeck_core::sim::{Ft991aSim, Ic7300Sim, SimHandle};
use rigdeck_core::transport::{PtyMaster, RecordingTransport, SerialTransport, Transport};
use rigdeck_core::{discovery, driver};
use std::collections::HashMap;
use std::process::ExitCode;
use std::sync::{Arc, RwLock};
use std::time::{Duration, Instant};

struct Args {
    cmd: String,
    pos: Vec<String>,
    opts: HashMap<String, Vec<String>>,
    flags: Vec<String>,
}

impl Args {
    fn parse() -> Args {
        let mut it = std::env::args().skip(1);
        let cmd = it.next().unwrap_or_default();
        let (mut pos, mut opts, mut flags) = (Vec::new(), HashMap::<String, Vec<String>>::new(), Vec::new());
        let rest: Vec<String> = it.collect();
        let mut i = 0;
        while i < rest.len() {
            let a = &rest[i];
            if let Some(k) = a.strip_prefix("--") {
                if matches!(k, "lan" | "allow-tx" | "scope" | "rewrite-unchanged") {
                    flags.push(k.to_string());
                } else if i + 1 < rest.len() {
                    opts.entry(k.to_string()).or_default().push(rest[i + 1].clone());
                    i += 1;
                }
            } else {
                pos.push(a.clone());
            }
            i += 1;
        }
        Args { cmd, pos, opts, flags }
    }
    fn get(&self, k: &str) -> Option<&str> {
        self.opts.get(k).and_then(|v| v.last()).map(String::as_str)
    }
    fn all(&self, k: &str) -> Vec<String> {
        self.opts.get(k).cloned().unwrap_or_default()
    }
    fn flag(&self, k: &str) -> bool {
        self.flags.iter().any(|f| f == k)
    }
    fn num<T: std::str::FromStr>(&self, k: &str, default: T) -> T {
        self.get(k).and_then(|v| v.parse().ok()).unwrap_or(default)
    }
}

fn rig_kind(s: &str) -> Result<RigKind, String> {
    match s.to_ascii_lowercase().as_str() {
        "ic7300" | "ic-7300" => Ok(RigKind::IcomIc7300),
        "ft991a" | "ft-991a" => Ok(RigKind::YaesuFt991a),
        _ => Err(format!("unknown radio {s:?}: use ic7300 or ft991a")),
    }
}

fn open_port(args: &Args, kind: RigKind) -> Result<Box<dyn Transport>, String> {
    let path = match args.get("port") {
        Some(p) => p.to_string(),
        None => discovery::find(kind).map(|p| p.path).ok_or_else(|| format!("no {} found on USB; pass --port", kind.name()))?,
    };
    let baud = args.num("baud", kind.default_baud());
    SerialTransport::open(&path, baud).map(|t| Box::new(t) as Box<dyn Transport>).map_err(|e| e.to_string())
}

fn connect(args: &Args, polling: bool) -> Result<Engine, String> {
    let kind = rig_kind(args.get("rig").ok_or("--rig is required")?)?;
    let t = open_port(args, kind)?;
    let p = driver(kind);
    let mut cfg = EngineConfig::default_for(&p.capabilities());
    if !polling {
        cfg = cfg.without_polling();
    }
    Ok(Engine::start(t, p, cfg))
}

fn now_est() -> String {
    std::process::Command::new("date")
        .env("TZ", "America/New_York")
        .arg("+%Y-%m-%d %H:%M:%S %Z")
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .unwrap_or_default()
}

fn parse_range(s: &str) -> Result<std::ops::RangeInclusive<u16>, String> {
    let (a, b) = s.split_once('-').ok_or("range must look like 1-55")?;
    Ok(a.parse().map_err(|_| "bad range")?..=b.parse().map_err(|_| "bad range")?)
}

fn read_channels(e: &Engine, range: std::ops::RangeInclusive<u16>) -> Result<Vec<MemoryChannel>, String> {
    let mut out = Vec::new();
    for n in range {
        out.push(e.read_memory(n).map_err(|err| format!("channel {n}: {err}"))?);
    }
    Ok(out)
}

fn run(args: &Args) -> Result<(), String> {
    match args.cmd.as_str() {
        "ports" => {
            for p in discovery::list_ports() {
                println!(
                    "{}\t{}\t{}\t{}",
                    p.path,
                    p.guess.map(|k| k.name()).unwrap_or("-"),
                    p.serial_number.unwrap_or_default(),
                    p.description
                );
            }
            Ok(())
        }
        "sim" => {
            let kind = rig_kind(args.pos.first().map(String::as_str).unwrap_or("ic7300"))?;
            let master = PtyMaster::open().map_err(|e| e.to_string())?;
            println!("{} simulator on {}", kind.name(), master.slave_path);
            let _h: Box<dyn std::any::Any> = match kind {
                RigKind::IcomIc7300 => {
                    let mut s = Ic7300Sim::default();
                    s.sweeps_per_second = args.num("sweeps", 20.0);
                    s.pixels = args.num("pixels", 475);
                    for ch in rigdeck_core::sim::sample_memories(kind) {
                        s.store(&ch);
                    }
                    Box::new(SimHandle::spawn(s, Box::new(master)))
                }
                RigKind::YaesuFt991a => {
                    let mut s = Ft991aSim::default();
                    for ch in rigdeck_core::sim::sample_memories(kind) {
                        s.store(ch);
                    }
                    Box::new(SimHandle::spawn(s, Box::new(master)))
                }
            };
            loop {
                std::thread::sleep(Duration::from_secs(3600));
            }
        }
        "probe" => {
            let e = connect(args, false)?;
            println!("link      {}", e.link());
            println!("time      {}", now_est());
            println!("id        {:#04x}", e.read_id().map_err(|x| x.to_string())?);
            println!("frequency {} Hz", e.read_frequency().map_err(|x| x.to_string())?);
            println!("mode      {}", e.read_mode().map_err(|x| x.to_string())?.label());
            let s = e.read_s_meter().map_err(|x| x.to_string())?;
            println!("s-meter   {s} ({})", rigdeck_core::rig::s_meter_text(s));
            Ok(())
        }
        "cycles" => {
            let count: u32 = args.num("count", 50);
            let mut fails = 0;
            let t0 = Instant::now();
            for i in 1..=count {
                let r = (|| -> Result<(u32, u64), String> {
                    let e = connect(args, true)?;
                    let id = e.read_id().map_err(|x| x.to_string())?;
                    let f = e.read_frequency().map_err(|x| x.to_string())?;
                    std::thread::sleep(Duration::from_millis(200));
                    e.shutdown();
                    Ok((id, f))
                })();
                match r {
                    Ok((id, f)) => println!("cycle {i:>3}: ok id={id:#04x} freq={f}"),
                    Err(err) => {
                        fails += 1;
                        println!("cycle {i:>3}: FAIL {err}");
                    }
                }
            }
            println!("{count} connect/disconnect cycles in {:.1} s, failures = {fails}", t0.elapsed().as_secs_f64());
            if fails > 0 { Err(format!("{fails} failures")) } else { Ok(()) }
        }
        "control-check" => {
            // Stage 3: every set is confirmed by reading it back; the radio is restored afterwards.
            let e = connect(args, false)?;
            let caps = e.capabilities().clone();
            let f0 = e.read_frequency().map_err(|x| x.to_string())?;
            let m0 = e.read_mode().map_err(|x| x.to_string())?;
            let p0 = match e.request(Request::ReadPowerLevel).map_err(|x| x.to_string())?.first_event() {
                Some(Event::PowerLevel(p)) => *p,
                other => return Err(format!("power read: {other:?}")),
            };
            println!("start: {f0} Hz, {}, power {p0}%  ({})", m0.label(), now_est());
            let mut fails = 0;
            let mut check = |what: String, r: Result<String, String>| match r {
                Ok(v) => println!("OK    {what:<34} -> {v}"),
                Err(err) => {
                    fails += 1;
                    println!("FAIL  {what:<34} -> {err}");
                }
            };
            for b in &caps.bands {
                let mode = if b.default_hz < 10_000_000 { rigdeck_core::Mode::Lsb } else { rigdeck_core::Mode::Usb };
                let r = e.set_frequency(b.default_hz).map_err(|x| x.to_string()).and_then(|f| {
                    e.set_mode(rigdeck_core::ModeSetting { mode, filter: None, data: false }).map(|m| format!("{f} Hz {}", m.label())).map_err(|x| x.to_string())
                });
                check(format!("{} {} Hz {}", b.name, b.default_hz, mode), r);
            }
            for m in &caps.modes {
                let r = e.set_mode(rigdeck_core::ModeSetting { mode: *m, filter: None, data: false }).map(|m| m.label()).map_err(|x| x.to_string());
                check(format!("mode {m}"), r);
            }
            if caps.has_filters {
                for f in [2u8, 3, 1] {
                    let ms = rigdeck_core::ModeSetting { mode: rigdeck_core::Mode::Usb, filter: Some(f), data: false };
                    check(format!("filter FIL{f}"), e.set_mode(ms).map(|m| m.label()).map_err(|x| x.to_string()));
                }
            }
            let ms = rigdeck_core::ModeSetting { mode: rigdeck_core::Mode::Usb, filter: Some(1), data: true };
            check("DATA on (USB-D)".into(), e.set_mode(ms).map(|m| m.label()).map_err(|x| x.to_string()));
            for p in [10u16, 50, 100] {
                check(format!("power {p}%"), e.set_power(p).map(|v| format!("{v}%")).map_err(|x| x.to_string()));
            }
            check("frequency outside the radio".into(), match e.set_frequency(caps.max_freq_hz + 1_000_000) {
                Err(rigdeck_core::EngineError::Unsupported(_)) => Ok("refused before sending".into()),
                other => Err(format!("{other:?}")),
            });
            // Restore.
            let r = e.set_frequency(f0).map_err(|x| x.to_string()).and_then(|_| e.set_mode(m0).map_err(|x| x.to_string())).and_then(|_| e.set_power(p0).map_err(|x| x.to_string()));
            check(format!("restore {f0} Hz {} {p0}%", m0.label()), r.map(|p| format!("power {p}%")));
            println!("control check: failures = {fails}  ({})", now_est());
            if fails == 0 { Ok(()) } else { Err(format!("{fails} failures")) }
        }
        "memories-read" => {
            let kind = rig_kind(args.get("rig").ok_or("--rig is required")?)?;
            let e = connect(args, false)?;
            let range = parse_range(args.get("range").unwrap_or("1-99"))?;
            let ch = read_channels(&e, range)?;
            let out = args.get("out").ok_or("--out is required")?;
            std::fs::write(out, memory::export_csv(kind, &ch)).map_err(|x| x.to_string())?;
            println!("read {} channels to {out}", ch.len());
            Ok(())
        }
        "memories-verify" | "memories-write" => {
            let kind = rig_kind(args.get("rig").ok_or("--rig is required")?)?;
            let mut planned = Vec::new();
            for f in args.all("csv") {
                let text = std::fs::read_to_string(&f).map_err(|x| format!("{f}: {x}"))?;
                planned.extend(memory::import_csv(kind, &text).map_err(|x| format!("{f}: {x}"))?);
            }
            if planned.is_empty() {
                return Err("no channels: pass --csv".into());
            }
            let e = connect(args, false)?;
            let numbers: Vec<u16> = planned.iter().map(|c| c.number).collect();
            let lo = *numbers.iter().min().unwrap();
            let hi = *numbers.iter().max().unwrap();
            if args.cmd == "memories-write" {
                let dir = args.get("backup-dir").ok_or("--backup-dir is required before writing")?;
                let caps = e.capabilities().clone();
                let all = read_channels(&e, caps.memory_channels.clone())?;
                let stamp = std::process::Command::new("date").env("TZ", "America/New_York").arg("+%Y%m%d-%H%M%S").output().map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string()).unwrap_or_default();
                let path = format!("{dir}/{}-memories-backup-{stamp}.csv", match kind { RigKind::IcomIc7300 => "ic7300", RigKind::YaesuFt991a => "ft991a" });
                std::fs::write(&path, memory::export_csv(kind, &all)).map_err(|x| x.to_string())?;
                println!("backup of {} channels: {path}", all.len());
                let changes = memory::preview(&all, &planned);
                for c in &changes {
                    if c.kind != memory::ChangeKind::Unchanged {
                        println!("{:>4} {:?}", c.number, c.kind);
                    }
                }
                if args.flag("rewrite-unchanged") {
                    // Exercise the write path without changing anything: write each unchanged
                    // channel's own bytes back, and check the radio returns identical bytes.
                    for c in changes.iter().filter(|c| c.kind == memory::ChangeKind::Unchanged) {
                        let Some(before) = &c.before else { continue };
                        let back = e.write_memory(before).map_err(|x| format!("channel {}: {x}", before.number))?;
                        if back.extra != before.extra {
                            return Err(format!("channel {}: bytes differ after rewrite", before.number));
                        }
                        println!("{:>4} rewritten with its own contents, bytes identical", before.number);
                    }
                }
                for c in changes.iter().filter(|c| c.kind != memory::ChangeKind::Unchanged) {
                    let mut ch = c.after.clone();
                    // Keep settings we do not edit from what is in the radio.
                    if let Some(b) = &c.before
                        && !b.empty
                        && matches!(ch.extra, memory::MemoryExtra::None)
                    {
                        ch.extra = b.extra.clone();
                    }
                    e.write_memory(&ch).map_err(|x| format!("channel {}: {x}", ch.number))?;
                }
            }
            // Independent read-back pass.
            let back = read_channels(&e, lo..=hi)?;
            let mismatches = memory::verify(&planned, &back);
            for m in &mismatches {
                println!("{:>4} MISMATCH expected {:?} found {:?}", m.number, m.expected, m.found);
            }
            for p in &planned {
                if let Some(b) = back.iter().find(|b| b.number == p.number) {
                    println!("{:>4} {} {} {:?} {}", p.number, b.freq_hz, b.mode.mode, b.name, if b.same_content(p) { "OK" } else { "MISMATCH" });
                }
            }
            println!("independent readback: {} channels, mismatches = {}  ({})", planned.len(), mismatches.len(), now_est());
            if mismatches.is_empty() { Ok(()) } else { Err("mismatches".into()) }
        }
        "scope-soak" => {
            let e = connect(args, true)?;
            if !e.capabilities().has_scope {
                return Err("this radio has no scope over its control port".into());
            }
            let rx = e.subscribe();
            e.request(Request::ScopeOn(true)).map_err(|x| x.to_string())?;
            if let Some(sp) = args.get("speed") {
                let v = match sp { "fast" => 0, "mid" => 1, _ => 2 };
                e.request(Request::ScopeSetSpeed(v)).map_err(|x| x.to_string())?;
            }
            e.request(Request::ScopeDataOutput(true)).map_err(|x| x.to_string())?;
            let secs: u64 = args.num("seconds", 60);
            let end = Instant::now() + Duration::from_secs(secs);
            let mut lens = HashMap::<usize, u64>::new();
            let mut n = 0u64;
            let mut last_report = Instant::now();
            while Instant::now() < end {
                if let Ok(Event::Sweep(s)) = rx.recv_timeout(Duration::from_millis(200)) {
                    n += 1;
                    *lens.entry(s.pixels.len()).or_default() += 1;
                }
                if last_report.elapsed() > Duration::from_secs(60) {
                    let st = e.state().stats;
                    println!("{} sweeps={} dropped={} timeouts={}", now_est(), st.sweeps, st.sweeps_dropped, st.timeouts);
                    last_report = Instant::now();
                }
            }
            let _ = e.request(Request::ScopeDataOutput(false));
            let st = e.state().stats;
            println!("{secs} s: sweeps received={n} ({:.1}/s), assembler sweeps={}, dropped={}, pixel lengths={lens:?}, timeouts={}, refused={}", n as f64 / secs as f64, st.sweeps, st.sweeps_dropped, st.timeouts, st.refused);
            if st.sweeps_dropped > 0 { Err("dropped sweeps".into()) } else { Ok(()) }
        }
        "record" => {
            let kind = rig_kind(args.get("rig").ok_or("--rig is required")?)?;
            let t = RecordingTransport::new(open_port(args, kind)?);
            let rec = t.received.clone();
            let p = driver(kind);
            let e = Engine::start(Box::new(t), p, EngineConfig::default_for(&kind.capabilities()));
            if args.flag("scope") {
                e.request(Request::ScopeOn(true)).map_err(|x| x.to_string())?;
                e.request(Request::ScopeDataOutput(true)).map_err(|x| x.to_string())?;
            }
            std::thread::sleep(Duration::from_secs(args.num("seconds", 3)));
            if args.flag("scope") {
                let _ = e.request(Request::ScopeDataOutput(false));
            }
            e.shutdown();
            let bytes = rec.lock().unwrap().clone();
            let hex: Vec<String> = bytes.chunks(32).map(|c| c.iter().map(|b| format!("{b:02X}")).collect::<Vec<_>>().join(" ")).collect();
            let out = args.get("out").ok_or("--out is required")?;
            std::fs::write(out, hex.join("\n") + "\n").map_err(|x| x.to_string())?;
            println!("recorded {} bytes to {out}", bytes.len());
            Ok(())
        }
        "rigctld" => {
            let e = Arc::new(connect(args, true)?);
            let slot = Arc::new(RwLock::new(Some(e.clone())));
            let cfg = RigctldConfig {
                port: args.num("listen", 4533),
                lan: args.flag("lan"),
                allow: args.all("allow").iter().filter_map(|a| a.parse().ok()).collect(),
                allow_tx: args.flag("allow-tx"),
                ..Default::default()
            };
            let srv = RigctldServer::start(slot, cfg).map_err(|x| x.to_string())?;
            println!("rigctld for {} on {}", e.capabilities().kind.name(), srv.local_addr);
            loop {
                std::thread::sleep(Duration::from_secs(1));
                if !e.is_running() {
                    return Err(format!("radio link lost: {:?}", e.state().link_error));
                }
            }
        }
        "clock" => {
            let e = connect(args, false)?;
            if !e.capabilities().has_clock {
                return Err("this radio's clock cannot be set over its control port".into());
            }
            let offset_h: i64 = args.num("utc-offset-hours", 0);
            // Wait for the top of the minute so the radio's seconds start at zero.
            let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap();
            let wait = 60.0 - (now.as_secs() % 60) as f64 - f64::from(now.subsec_millis()) / 1000.0;
            println!("waiting {wait:.1} s for the top of the minute");
            std::thread::sleep(Duration::from_secs_f64(wait));
            let t = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs() as i64 + offset_h * 3600;
            let (y, mo, d, h, mi) = civil(t);
            e.request(Request::SetTime { hour: h, minute: mi }).map_err(|x| x.to_string())?;
            e.request(Request::SetDate { year: y, month: mo, day: d }).map_err(|x| x.to_string())?;
            match e.request(Request::ReadClockTime).map_err(|x| x.to_string())?.first_event() {
                Some(Event::ClockTime { hour, minute }) if *hour == h && *minute == mi => {
                    println!("radio clock {hour:02}:{minute:02}, date {y}-{mo:02}-{d:02} (UTC{offset_h:+})");
                    Ok(())
                }
                other => Err(format!("radio clock reads {other:?}, expected {h:02}:{mi:02}")),
            }
        }
        _ => Err("usage: rigdeck-tools ports|sim|probe|control-check|cycles|memories-read|memories-verify|memories-write|scope-soak|record|rigctld|clock (see source header)".into()),
    }
}

/// Unix seconds to (year, month, day, hour, minute), proleptic Gregorian (Howard Hinnant's algorithm).
fn civil(t: i64) -> (u16, u8, u8, u8, u8) {
    let days = t.div_euclid(86_400);
    let secs = t.rem_euclid(86_400);
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    (y as u16, m as u8, d as u8, (secs / 3600) as u8, ((secs % 3600) / 60) as u8)
}

fn main() -> ExitCode {
    let args = Args::parse();
    match run(&args) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("error: {e}");
            ExitCode::FAILURE
        }
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn civil_dates() {
        assert_eq!(super::civil(0), (1970, 1, 1, 0, 0));
        // 2026-11-14 17:02 UTC
        assert_eq!(super::civil(1_794_675_720), (2026, 11, 14, 17, 2));
    }
}
