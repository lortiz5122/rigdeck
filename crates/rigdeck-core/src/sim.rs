//! Simulated radios for tests and for running the app without hardware.
//!
//! - [`Ic7300Sim`] answers CI-V as described in the IC-7300 manual, streams scope sweeps when
//!   the scope and its data output are on, and refuses (FA) the commands the real radio was seen
//!   refusing in AM/FM (twin PBT 14 07/14 08, filter width 1A 03) and scanning (0E).
//! - [`Ft991aSim`] answers Yaesu CAT and pushes changes when Auto Information is on.
//!
//! Run one behind a pseudo-terminal with the `rigdeck-sim` tool and connect the app to the
//! printed `/dev/ttys…` path.

use crate::bcd;
use crate::cat::{self, encode_mt};
use crate::civ::{self, Frame, FrameDecoder};
use crate::memory::{MemoryChannel, MemoryExtra};
use crate::rig::{Mode, ModeSetting, RigKind, ScopeMode};
use crate::scope::{Sweep, encode_sweep};
use crate::transport::Transport;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

pub trait SimRadio: Send {
    /// Bytes from the controller; returns bytes to send back.
    fn receive(&mut self, bytes: &[u8]) -> Vec<u8>;
    /// Called often; returns unsolicited output (scope data, pushed changes).
    fn tick(&mut self, now: Instant) -> Vec<u8>;
}

/// Simple deterministic noise so tests are repeatable.
#[derive(Debug, Clone)]
struct Lcg(u64);
impl Lcg {
    fn next(&mut self) -> u32 {
        self.0 = self.0.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        (self.0 >> 33) as u32
    }
}

#[derive(Debug, Clone)]
pub struct Ic7300Sim {
    pub address: u8,
    pub frequency_hz: u64,
    pub mode: ModeSetting,
    pub power_raw: u16,
    pub tx: bool,
    pub s_meter: u16,
    pub memories: Vec<Option<Vec<u8>>>,
    pub scope_on: bool,
    pub scope_output: bool,
    pub scope_mode: ScopeMode,
    pub span_hz: u32,
    pub sweeps_per_second: f64,
    pub pixels: usize,
    pub echo_back: bool,
    pub date: [u8; 4],
    pub time: [u8; 2],
    /// Count of commands answered with FA, for tests.
    pub refused: u64,
    pub received_commands: Vec<Vec<u8>>,
    pending_out: Vec<u8>,
    decoder: FrameDecoder,
    next_sweep: Option<Instant>,
    rng: Lcg,
}

impl Default for Ic7300Sim {
    fn default() -> Self {
        Self {
            address: 0x94,
            frequency_hz: 14_074_000,
            mode: ModeSetting { mode: Mode::Usb, filter: Some(1), data: true },
            power_raw: 128,
            tx: false,
            s_meter: 60,
            memories: vec![None; 102],
            scope_on: true,
            scope_output: false,
            scope_mode: ScopeMode::Center,
            span_hz: 25_000,
            sweeps_per_second: 20.0,
            pixels: 475,
            echo_back: false,
            date: [0x20, 0x26, 0x11, 0x14],
            time: [0x12, 0x00],
            refused: 0,
            received_commands: Vec::new(),
            pending_out: Vec::new(),
            decoder: FrameDecoder::default(),
            next_sweep: None,
            rng: Lcg(7300),
        }
    }
}

impl Ic7300Sim {
    fn reply(&self, payload: Vec<u8>) -> Vec<u8> {
        Frame { to: 0xE0, from: self.address, payload }.encode()
    }

    fn ok(&self) -> Vec<u8> {
        self.reply(vec![civ::OK])
    }

    fn ng(&mut self) -> Vec<u8> {
        self.refused += 1;
        self.reply(vec![civ::NG])
    }

    /// Operator turns the dial: the radio pushes the new frequency (CI-V transceive, address 00).
    pub fn turn_dial(&mut self, hz: u64) {
        self.frequency_hz = hz;
        let mut p = vec![0x00];
        p.extend(bcd::freq_to_civ(hz));
        self.pending_out.extend(Frame { to: civ::BROADCAST, from: self.address, payload: p }.encode());
    }

    /// Operator presses a mode key: the radio pushes mode and filter (transceive 01).
    pub fn press_mode(&mut self, mode: Mode) {
        self.mode = ModeSetting { mode, filter: Some(1), data: false };
        let p = vec![0x01, civ::mode_code(mode).unwrap_or(1), 0x01];
        self.pending_out.extend(Frame { to: civ::BROADCAST, from: self.address, payload: p }.encode());
    }

    pub fn memory(&self, n: u16) -> Option<MemoryChannel> {
        let raw = self.memories.get(n as usize)?.as_ref()?;
        civ::decode_memory(n, raw)
    }

    pub fn store(&mut self, ch: &MemoryChannel) {
        self.memories[ch.number as usize] = if ch.empty { None } else { civ::encode_memory(ch) };
    }

    fn handle(&mut self, p: &[u8]) -> Vec<u8> {
        self.received_commands.push(p.to_vec());
        let am_fm = matches!(self.mode.mode, Mode::Am | Mode::Fm);
        match p {
            [0x03] => {
                let mut r = vec![0x03];
                r.extend(bcd::freq_to_civ(self.frequency_hz));
                self.reply(r)
            }
            [0x04] => {
                let r = vec![0x04, civ::mode_code(self.mode.mode).unwrap(), bcd::byte(self.mode.filter.unwrap_or(1))];
                self.reply(r)
            }
            [0x05, f @ ..] | [0x25, 0x00, f @ ..] if f.len() == 5 => match bcd::civ_to_freq(f) {
                Ok(hz) if (30_000..=74_800_000).contains(&hz) => {
                    self.frequency_hz = hz;
                    self.ok()
                }
                _ => self.ng(),
            },
            [0x25, 0x00] => {
                let mut r = vec![0x25, 0x00];
                r.extend(bcd::freq_to_civ(self.frequency_hz));
                self.reply(r)
            }
            [0x06, m, rest @ ..] => match civ::mode_from_code(*m) {
                Some(mode) => {
                    self.mode = ModeSetting { mode, filter: rest.first().and_then(|f| bcd::from_byte(*f).ok()).or(Some(1)), data: false };
                    self.ok()
                }
                None => self.ng(),
            },
            [0x26, 0x00] => {
                let r = vec![
                    0x26,
                    0x00,
                    civ::mode_code(self.mode.mode).unwrap(),
                    u8::from(self.mode.data),
                    bcd::byte(self.mode.filter.unwrap_or(1)),
                ];
                self.reply(r)
            }
            [0x26, 0x00, m, rest @ ..] => match civ::mode_from_code(*m) {
                Some(mode) => {
                    let data = rest.first().is_some_and(|d| *d == 1);
                    if data && !matches!(mode, Mode::Lsb | Mode::Usb | Mode::Am | Mode::Fm) {
                        return self.ng();
                    }
                    self.mode = ModeSetting { mode, data, filter: rest.get(1).and_then(|f| bcd::from_byte(*f).ok()).or(Some(1)) };
                    self.ok()
                }
                None => self.ng(),
            },
            [0x14, 0x0A] => {
                let mut r = vec![0x14, 0x0A];
                r.extend(bcd::encode_be(u64::from(self.power_raw), 2));
                self.reply(r)
            }
            [0x14, 0x0A, a, b] => match bcd::decode_be(&[*a, *b]) {
                Ok(v) if v <= 255 => {
                    self.power_raw = v as u16;
                    self.ok()
                }
                _ => self.ng(),
            },
            [0x14, 0x07 | 0x08, ..] | [0x1A, 0x03, ..] if am_fm => self.ng(),
            [0x0E, ..] => self.ng(),
            [0x15, sub @ (0x02 | 0x11 | 0x12 | 0x13)] => {
                let v = match sub {
                    0x02 => {
                        let jitter = (self.rng.next() % 9) as i32 - 4;
                        (i32::from(self.s_meter) + jitter).clamp(0, 241) as u64
                    }
                    0x11 => {
                        if self.tx {
                            u64::from(self.power_raw) * 213 / 255
                        } else {
                            0
                        }
                    }
                    0x12 => {
                        if self.tx {
                            30
                        } else {
                            0
                        }
                    }
                    _ => {
                        if self.tx {
                            40
                        } else {
                            0
                        }
                    }
                };
                let mut r = vec![0x15, *sub];
                r.extend(bcd::encode_be(v, 2));
                self.reply(r)
            }
            [0x19, 0x00] => self.reply(vec![0x19, 0x00, self.address]),
            [0x1C, 0x00] => self.reply(vec![0x1C, 0x00, u8::from(self.tx)]),
            [0x1C, 0x00, v] if *v <= 1 => {
                self.tx = *v == 1;
                self.ok()
            }
            [0x1A, 0x00, a, b] => match bcd::decode_be(&[*a, *b]) {
                Ok(n) if (1..=101).contains(&n) => {
                    let mut r = vec![0x1A, 0x00, *a, *b];
                    match &self.memories[n as usize] {
                        Some(raw) => r.extend(raw),
                        None => r.push(0xFF),
                    }
                    self.reply(r)
                }
                _ => self.ng(),
            },
            [0x1A, 0x00, a, b, data @ ..] => match bcd::decode_be(&[*a, *b]) {
                Ok(n) if (1..=101).contains(&n) => {
                    if data == [0xFF] {
                        self.memories[n as usize] = None;
                        return self.ok();
                    }
                    if data.len() != civ::MEMORY_DATA_LEN || civ::decode_memory(n as u16, data).is_none() {
                        return self.ng();
                    }
                    self.memories[n as usize] = Some(data.to_vec());
                    self.ok()
                }
                _ => self.ng(),
            },
            [0x1A, 0x05, 0x00, 0x94] => {
                let mut r = p.to_vec();
                r.extend(self.date);
                self.reply(r)
            }
            [0x1A, 0x05, 0x00, 0x94, d @ ..] if d.len() == 4 => {
                self.date.copy_from_slice(d);
                self.ok()
            }
            [0x1A, 0x05, 0x00, 0x95] => {
                let mut r = p.to_vec();
                r.extend(self.time);
                self.reply(r)
            }
            [0x1A, 0x05, 0x00, 0x95, h, m] => match (bcd::from_byte(*h), bcd::from_byte(*m)) {
                (Ok(hh), Ok(mm)) if hh < 24 && mm < 60 => {
                    self.time = [*h, *m];
                    self.ok()
                }
                _ => self.ng(),
            },
            [0x27, 0x10] => self.reply(vec![0x27, 0x10, u8::from(self.scope_on)]),
            [0x27, 0x10, v] => {
                self.scope_on = *v == 1;
                self.ok()
            }
            [0x27, 0x11] => self.reply(vec![0x27, 0x11, u8::from(self.scope_output)]),
            [0x27, 0x11, v] => {
                self.scope_output = *v == 1;
                self.ok()
            }
            [0x27, 0x14, 0x00, m] if *m <= 3 => {
                self.scope_mode = match m {
                    0 => ScopeMode::Center,
                    1 => ScopeMode::Fixed,
                    2 => ScopeMode::ScrollCenter,
                    _ => ScopeMode::ScrollFixed,
                };
                self.ok()
            }
            [0x27, 0x1A, 0x00, v] if *v <= 2 => self.ok(),
            [0x27, 0x15, 0x00, s @ ..] if s.len() == 5 => match bcd::decode_le(s) {
                Ok(v) if [2_500, 5_000, 10_000, 25_000, 50_000, 100_000, 250_000, 500_000].contains(&v) => {
                    self.span_hz = v as u32;
                    self.ok()
                }
                _ => self.ng(),
            },
            _ => self.ng(),
        }
    }

    pub fn make_sweep(&mut self) -> Sweep {
        let (low, high) = match self.scope_mode {
            ScopeMode::Center | ScopeMode::ScrollCenter => {
                (self.frequency_hz - u64::from(self.span_hz), self.frequency_hz + u64::from(self.span_hz))
            }
            _ => (self.frequency_hz / 100_000 * 100_000, self.frequency_hz / 100_000 * 100_000 + 200_000),
        };
        let n = self.pixels;
        let mut pixels = Vec::with_capacity(n);
        for i in 0..n {
            let mut v = 20 + (self.rng.next() % 12) as i32;
            // Two steady carriers and one that drifts with time, so the waterfall shows something.
            for (pos, amp) in [(n / 4, 90), (n / 2, 120), ((n * 3 / 4 + (self.rng.0 as usize % 7)) % n, 70)] {
                let d = (i as i32 - pos as i32).abs();
                if d < 4 {
                    v = v.max(amp - d * 15);
                }
            }
            pixels.push(v.clamp(0, 160) as u8);
        }
        Sweep { mode: self.scope_mode, low_hz: low, high_hz: high, out_of_range: false, pixels }
    }
}

impl SimRadio for Ic7300Sim {
    fn receive(&mut self, bytes: &[u8]) -> Vec<u8> {
        let mut out = Vec::new();
        for f in self.decoder.push(bytes) {
            if self.echo_back {
                out.extend(f.encode());
            }
            if f.to != self.address && f.to != civ::BROADCAST {
                continue;
            }
            out.extend(self.handle(&f.payload));
        }
        out
    }

    fn tick(&mut self, now: Instant) -> Vec<u8> {
        let mut out = std::mem::take(&mut self.pending_out);
        if self.scope_on && self.scope_output && self.sweeps_per_second > 0.0 {
            let period = Duration::from_secs_f64(1.0 / self.sweeps_per_second);
            let due = *self.next_sweep.get_or_insert(now);
            if now >= due {
                let sweep = self.make_sweep();
                let chunk = self.pixels.div_ceil(10).max(1);
                for d in encode_sweep(&sweep, 11, chunk) {
                    let mut p = vec![0x27, 0x00];
                    p.extend(d);
                    out.extend(self.reply(p));
                }
                // Keep a steady rate without bunching up after a stall.
                self.next_sweep = Some(if now - due > period * 4 { now + period } else { due + period });
            }
        } else {
            self.next_sweep = None;
        }
        out
    }
}

// ------------------------------------------------------------------ FT-991A

#[derive(Debug, Clone)]
pub struct Ft991aSim {
    pub frequency_a: u64,
    pub frequency_b: u64,
    pub mode: ModeSetting,
    pub power_w: u16,
    pub tx: bool,
    pub s_meter: u16,
    pub auto_information: bool,
    pub memories: Vec<Option<MemoryChannel>>,
    pub refused: u64,
    buf: Vec<u8>,
    pending_out: Vec<u8>,
}

impl Default for Ft991aSim {
    fn default() -> Self {
        Self {
            frequency_a: 7_074_000,
            frequency_b: 14_074_000,
            mode: ModeSetting { mode: Mode::Usb, filter: None, data: true },
            power_w: 50,
            tx: false,
            s_meter: 40,
            auto_information: false,
            memories: vec![None; 118],
            refused: 0,
            buf: Vec::new(),
            pending_out: Vec::new(),
        }
    }
}

impl Ft991aSim {
    pub fn turn_dial(&mut self, hz: u64) {
        self.frequency_a = hz;
        if self.auto_information {
            self.pending_out.extend(format!("FA{hz:09};").bytes());
        }
    }

    fn ng(&mut self) -> String {
        self.refused += 1;
        "?;".into()
    }

    fn handle(&mut self, msg: &str) -> String {
        let code = msg.get(0..2).unwrap_or("").to_ascii_uppercase();
        let p = msg.get(2..).unwrap_or("");
        let three = |s: &str| s.len() == 3 && s.chars().all(|c| c.is_ascii_digit());
        match (code.as_str(), p) {
            ("ID", "") => "ID0670;".into(),
            ("FA", "") => format!("FA{:09};", self.frequency_a),
            ("FB", "") => format!("FB{:09};", self.frequency_b),
            ("FA" | "FB", f) if f.len() == 9 => match f.parse::<u64>() {
                Ok(hz) if (30_000..=470_000_000).contains(&hz) => {
                    if code == "FA" {
                        self.frequency_a = hz;
                    } else {
                        self.frequency_b = hz;
                    }
                    String::new()
                }
                _ => self.ng(),
            },
            ("MD", "0") => format!("MD0{};", cat::mode_char(self.mode).unwrap()),
            ("MD", m) if m.len() == 2 && m.starts_with('0') => match cat::mode_from_char(m.chars().nth(1).unwrap()) {
                Some(ms) => {
                    self.mode = ms;
                    if self.auto_information {
                        self.pending_out.extend(format!("MD0{};", cat::mode_char(ms).unwrap()).bytes());
                    }
                    String::new()
                }
                None => self.ng(),
            },
            ("SM", "0") => format!("SM0{:03};", self.s_meter.min(255)),
            ("PC", "") => format!("PC{:03};", self.power_w),
            ("PC", v) if three(v) => match v.parse::<u16>() {
                Ok(w) if (5..=100).contains(&w) => {
                    self.power_w = w;
                    String::new()
                }
                _ => self.ng(),
            },
            ("RM", m) if m.len() == 1 => {
                let v = match m {
                    "1" => self.s_meter,
                    "4" => u16::from(self.tx) * 30,
                    "5" => u16::from(self.tx) * self.power_w * 2,
                    "6" => u16::from(self.tx) * 20,
                    _ => return self.ng(),
                };
                format!("RM{m}{v:03};")
            }
            ("TX", "") => format!("TX{};", if self.tx { 2 } else { 0 }),
            ("TX", v) if v == "0" || v == "1" => {
                self.tx = v == "1";
                String::new()
            }
            ("AI", "") => format!("AI{};", u8::from(self.auto_information)),
            ("AI", v) if v == "0" || v == "1" => {
                self.auto_information = v == "1";
                String::new()
            }
            ("IF", "") => format!("IF001{:09}+000000{}00000;", self.frequency_a, cat::mode_char(self.mode).unwrap()),
            ("MT", n) if three(n) => {
                let k: usize = n.parse().unwrap_or(0);
                match self.memories.get(k).cloned().flatten() {
                    Some(ch) if (1..=117).contains(&k) => encode_mt(&ch).unwrap_or_else(|| "?;".into()),
                    _ => self.ng(),
                }
            }
            ("MT", body) if body.len() == 38 => {
                let parsed = {
                    let mut tmp = crate::cat::YaesuCat::ft991a();
                    use crate::protocol::Protocol;
                    tmp.feed(format!("MT{body};").as_bytes())
                };
                match parsed.first() {
                    Some(crate::protocol::Incoming::Message { events, .. }) => match events.first() {
                        Some(crate::rig::Event::Memory(ch)) if (1..=117).contains(&ch.number) => {
                            let n = ch.number as usize;
                            self.memories[n] = Some(ch.clone());
                            String::new()
                        }
                        _ => self.ng(),
                    },
                    _ => self.ng(),
                }
            }
            _ => self.ng(),
        }
    }

    pub fn store(&mut self, ch: MemoryChannel) {
        let n = ch.number as usize;
        let mut ch = ch;
        if matches!(ch.extra, MemoryExtra::None) {
            ch.extra = MemoryExtra::Yaesu { clar_offset_hz: 0, rx_clar: false, tx_clar: false, ctcss: 0, shift: 0 };
        }
        self.memories[n] = Some(ch);
    }
}

impl SimRadio for Ft991aSim {
    fn receive(&mut self, bytes: &[u8]) -> Vec<u8> {
        self.buf.extend_from_slice(bytes);
        let mut out = String::new();
        while let Some(end) = self.buf.iter().position(|&b| b == b';') {
            let raw: Vec<u8> = self.buf.drain(..=end).collect();
            let msg = String::from_utf8_lossy(&raw[..raw.len() - 1]).trim_start().trim_end_matches(['\r', '\n']).to_string();
            if !msg.is_empty() {
                out.push_str(&self.handle(&msg));
            }
        }
        out.into_bytes()
    }

    fn tick(&mut self, _now: Instant) -> Vec<u8> {
        std::mem::take(&mut self.pending_out)
    }
}

/// A simulator shared between its serving thread and the test or tool that drives it.
pub type Shared<T> = Arc<Mutex<T>>;

/// Serve a simulated radio on a transport until `stop` is set or the link closes.
pub fn serve<R: SimRadio + 'static>(radio: Shared<R>, mut transport: Box<dyn Transport>, stop: Arc<AtomicBool>) {
    let mut buf = [0u8; 4096];
    while !stop.load(Ordering::Relaxed) {
        let n = match transport.read(&mut buf, Duration::from_millis(2)) {
            Ok(n) => n,
            Err(_) => break,
        };
        let out = {
            let mut r = radio.lock().unwrap();
            let mut out = if n > 0 { r.receive(&buf[..n]) } else { Vec::new() };
            out.extend(r.tick(Instant::now()));
            out
        };
        if !out.is_empty() && transport.write_all(&out).is_err() {
            break;
        }
    }
}

/// A simulator running on its own thread.
pub struct SimHandle<R> {
    pub radio: Shared<R>,
    stop: Arc<AtomicBool>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl<R: SimRadio + 'static> SimHandle<R> {
    pub fn spawn(radio: R, transport: Box<dyn Transport>) -> Self {
        let radio = Arc::new(Mutex::new(radio));
        let stop = Arc::new(AtomicBool::new(false));
        let (r, s) = (radio.clone(), stop.clone());
        let thread = std::thread::Builder::new().name("rigdeck-sim".into()).spawn(move || serve(r, transport, s)).expect("spawn sim");
        Self { radio, stop, thread: Some(thread) }
    }
}

impl<R> Drop for SimHandle<R> {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

/// Fill a simulator with a few realistic channels.
pub fn sample_memories(kind: RigKind) -> Vec<MemoryChannel> {
    let mut v = vec![
        MemoryChannel::new(1, 28_400_000, Mode::Usb, "10m SSB"),
        MemoryChannel::new(2, 29_600_000, Mode::Fm, "10m FM"),
        MemoryChannel::new(3, 7_200_000, Mode::Lsb, "40m SSB"),
        MemoryChannel::new(4, 14_074_000, Mode::Usb, "FT8 20m"),
        MemoryChannel::new(5, 7_074_000, Mode::Usb, "FT8 40m"),
    ];
    v[3].mode.data = true;
    v[4].mode.data = true;
    if kind == RigKind::YaesuFt991a {
        v.push(MemoryChannel::new(6, 146_520_000, Mode::Fm, "2m Simplex"));
    }
    v
}
