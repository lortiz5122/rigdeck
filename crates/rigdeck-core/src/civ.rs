//! Icom CI-V: frame codec and the IC-7300 driver.
//!
//! Written from the Icom IC-7300 Full Manual, section 19 (see `catalogue/ic7300.json` for the
//! command bytes and the page each one comes from).

use crate::bcd;
use crate::catalogue::{self, CivCatalogue};
use crate::memory::{MemoryChannel, MemoryExtra};
use crate::protocol::{Encoded, Expect, Incoming, Protocol};
use crate::rig::{Event, Mode, ModeSetting, Request, RigKind, ScopeMode};
use crate::scope::{ScopeStats, SweepAssembler};

pub const PREAMBLE: u8 = 0xFE;
pub const END: u8 = 0xFD;
pub const OK: u8 = 0xFB;
pub const NG: u8 = 0xFA;
/// Collision ("jammer") code sent on a shared bus.
pub const COLLISION: u8 = 0xFC;
/// Broadcast address used by transceive output.
pub const BROADCAST: u8 = 0x00;
/// Longest frame accepted before the buffer is treated as noise (scope divisions are ~60 bytes).
const MAX_FRAME: usize = 512;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Frame {
    pub to: u8,
    pub from: u8,
    pub payload: Vec<u8>,
}

impl Frame {
    pub fn encode(&self) -> Vec<u8> {
        let mut v = Vec::with_capacity(self.payload.len() + 5);
        v.extend([PREAMBLE, PREAMBLE, self.to, self.from]);
        v.extend(&self.payload);
        v.push(END);
        v
    }
}

/// Splits a byte stream into frames. Tolerates noise, repeated preambles and split reads.
#[derive(Debug, Default, Clone)]
pub struct FrameDecoder {
    buf: Vec<u8>,
    pub discarded_bytes: u64,
}

impl FrameDecoder {
    pub fn push(&mut self, bytes: &[u8]) -> Vec<Frame> {
        self.buf.extend_from_slice(bytes);
        let mut out = Vec::new();
        loop {
            // Find a preamble pair.
            let Some(start) = self.buf.windows(2).position(|w| w == [PREAMBLE, PREAMBLE]) else {
                // Keep a trailing single FE; drop the rest.
                let keep = usize::from(self.buf.last() == Some(&PREAMBLE));
                self.discarded_bytes += (self.buf.len() - keep) as u64;
                self.buf.drain(..self.buf.len() - keep);
                break;
            };
            if start > 0 {
                self.discarded_bytes += start as u64;
                self.buf.drain(..start);
            }
            // Skip any extra FE bytes (power-on wake-up sequences repeat FE).
            let mut body = 2;
            while self.buf.get(body) == Some(&PREAMBLE) {
                body += 1;
            }
            let Some(end_rel) = self.buf[body..].iter().position(|&b| b == END) else {
                if self.buf.len() > MAX_FRAME {
                    self.discarded_bytes += self.buf.len() as u64;
                    self.buf.clear();
                }
                break;
            };
            let end = body + end_rel;
            // A preamble inside the body means that frame was cut off; restart from it.
            if let Some(q) = self.buf[body..end].iter().position(|&b| b == PREAMBLE) {
                self.discarded_bytes += (body + q) as u64;
                self.buf.drain(..body + q);
                continue;
            }
            let inner: Vec<u8> = self.buf[body..end].to_vec();
            self.buf.drain(..=end);
            if inner.len() >= 2 {
                out.push(Frame { to: inner[0], from: inner[1], payload: inner[2..].to_vec() });
            } else {
                self.discarded_bytes += inner.len() as u64 + 3;
            }
        }
        out
    }
}

pub fn mode_code(m: Mode) -> Option<u8> {
    Some(match m {
        Mode::Lsb => 0x00,
        Mode::Usb => 0x01,
        Mode::Am => 0x02,
        Mode::Cw => 0x03,
        Mode::Rtty => 0x04,
        Mode::Fm => 0x05,
        Mode::CwR => 0x07,
        Mode::RttyR => 0x08,
        _ => return None,
    })
}

pub fn mode_from_code(c: u8) -> Option<Mode> {
    Some(match c {
        0x00 => Mode::Lsb,
        0x01 => Mode::Usb,
        0x02 => Mode::Am,
        0x03 => Mode::Cw,
        0x04 => Mode::Rtty,
        0x05 => Mode::Fm,
        0x07 => Mode::CwR,
        0x08 => Mode::RttyR,
        _ => return None,
    })
}

/// Channel number (1–99, 100 = P1, 101 = P2) to its two BCD bytes.
pub fn channel_bytes(n: u16) -> Vec<u8> {
    bcd::encode_be(u64::from(n), 2)
}

/// Length of the 1A 00 data after the channel number: 1 + 14 + 14 + 10.
pub const MEMORY_DATA_LEN: usize = 39;
/// Default tone bytes (88.5 Hz) used when building a channel from scratch.
const DEFAULT_TONE: [u8; 3] = [0x00, 0x08, 0x85];

/// Decode the data that follows `1A 00 <channel>`.
pub fn decode_memory(number: u16, data: &[u8]) -> Option<MemoryChannel> {
    if data.first() == Some(&0xFF) {
        return Some(MemoryChannel::empty(number));
    }
    if data.len() < MEMORY_DATA_LEN {
        return None;
    }
    let freq = bcd::civ_to_freq(&data[1..6]).ok()?;
    let mode = mode_from_code(data[6])?;
    let filter = bcd::from_byte(data[7]).ok().filter(|f| (1..=3).contains(f));
    let data_on = data[8] >> 4 == 1;
    let name: String = data[29..39].iter().map(|&b| if b.is_ascii() && b >= 0x20 { b as char } else { ' ' }).collect();
    Some(MemoryChannel {
        number,
        empty: false,
        freq_hz: freq,
        mode: ModeSetting { mode, filter, data: data_on },
        name: name.trim_end().to_string(),
        extra: MemoryExtra::Icom { raw: data[..MEMORY_DATA_LEN].to_vec() },
    })
}

/// Encode the data that follows `1A 00 <channel>`. Settings we do not edit (split, select,
/// tones) come from the channel's raw bytes when it has them. Both VFO halves get the same
/// frequency and mode, as the manual recommends (p. 19-11 NOTE).
pub fn encode_memory(ch: &MemoryChannel) -> Option<Vec<u8>> {
    if ch.empty {
        return Some(vec![0xFF]);
    }
    let mode = mode_code(ch.mode.mode)?;
    let filter = bcd::byte(ch.mode.filter.unwrap_or(1));
    let mut d = match &ch.extra {
        MemoryExtra::Icom { raw } if raw.len() >= MEMORY_DATA_LEN && raw[0] != 0xFF => raw[..MEMORY_DATA_LEN].to_vec(),
        _ => {
            let mut v = vec![0x00];
            for _ in 0..2 {
                v.extend([0, 0, 0, 0, 0, 0, 0, 0x00]);
                v.extend(DEFAULT_TONE);
                v.extend(DEFAULT_TONE);
            }
            v.extend([b' '; 10]);
            v
        }
    };
    for base in [1usize, 15] {
        d[base..base + 5].copy_from_slice(&bcd::freq_to_civ(ch.freq_hz));
        d[base + 5] = mode;
        d[base + 6] = filter;
        d[base + 7] = (u8::from(ch.mode.data) << 4) | (d[base + 7] & 0x0F);
    }
    let mut name: Vec<u8> = ch.name.bytes().filter(|b| (0x20..0x7F).contains(b)).take(10).collect();
    name.resize(10, b' ');
    d[29..39].copy_from_slice(&name);
    Some(d)
}

pub struct IcomCiv {
    kind: RigKind,
    cat: &'static CivCatalogue,
    pub radio: u8,
    pub controller: u8,
    decoder: FrameDecoder,
    scope: SweepAssembler,
}

impl IcomCiv {
    pub fn ic7300() -> Self {
        let cat = catalogue::ic7300();
        Self {
            kind: RigKind::IcomIc7300,
            cat,
            radio: cat.radio_address(),
            controller: cat.controller_address(),
            decoder: FrameDecoder::default(),
            scope: SweepAssembler::new(),
        }
    }

    fn frame(&self, payload: Vec<u8>) -> Vec<u8> {
        Frame { to: self.radio, from: self.controller, payload }.encode()
    }

    fn get(&self, name: &str) -> Encoded {
        let key = self.cat.bytes(name);
        Encoded { bytes: self.frame(key.clone()), expect: Expect::Reply(key) }
    }

    fn set(&self, name: &str, data: &[u8]) -> Encoded {
        let mut p = self.cat.bytes(name);
        p.extend_from_slice(data);
        Encoded { bytes: self.frame(p), expect: Expect::Ack }
    }

    fn starts(&self, payload: &[u8], name: &str) -> Option<usize> {
        let k = self.cat.bytes(name);
        payload.starts_with(&k).then_some(k.len())
    }

    fn decode_payload(&mut self, p: &[u8]) -> Option<Event> {
        let level = |d: &[u8]| -> Option<u16> { d.get(..2).and_then(|b| bcd::decode_be(b).ok()).map(|v| v as u16) };
        if let Some(n) = self.starts(p, "scope_wave_data") {
            return match self.scope.push(&p[n..]) {
                Ok(Some(s)) => Some(Event::Sweep(s)),
                _ => None,
            };
        }
        if let Some(n) = self.starts(p, "s_meter") {
            return level(&p[n..]).map(Event::SMeter);
        }
        if let Some(n) = self.starts(p, "po_meter") {
            return level(&p[n..]).map(Event::Po);
        }
        if let Some(n) = self.starts(p, "swr_meter") {
            return level(&p[n..]).map(Event::Swr);
        }
        if let Some(n) = self.starts(p, "alc_meter") {
            return level(&p[n..]).map(Event::Alc);
        }
        if let Some(n) = self.starts(p, "rf_power") {
            return level(&p[n..]).map(|v| Event::PowerLevel(((u32::from(v) * 100 + 127) / 255) as u16));
        }
        if let Some(n) = self.starts(p, "read_id") {
            return p.get(n).map(|&id| Event::Id(u32::from(id)));
        }
        if let Some(n) = self.starts(p, "tx_status") {
            return p.get(n).map(|&b| Event::Tx(b == 1));
        }
        if let Some(n) = self.starts(p, "selected_vfo_frequency") {
            return bcd::civ_to_freq(p.get(n..n + 5)?).ok().map(Event::Frequency);
        }
        if let Some(n) = self.starts(p, "selected_vfo_mode") {
            let mode = mode_from_code(*p.get(n)?)?;
            let data = p.get(n + 1).is_some_and(|&d| d == 1);
            let filter = p.get(n + 2).and_then(|&f| bcd::from_byte(f).ok());
            return Some(Event::Mode(ModeSetting { mode, filter, data }));
        }
        if let Some(n) = self.starts(p, "memory_contents") {
            let ch = bcd::decode_be(p.get(n..n + 2)?).ok()? as u16;
            return decode_memory(ch, &p[n + 2..]).map(Event::Memory);
        }
        if let Some(n) = self.starts(p, "clock_time") {
            let d = p.get(n..n + 2)?;
            return Some(Event::ClockTime { hour: bcd::from_byte(d[0]).ok()?, minute: bcd::from_byte(d[1]).ok()? });
        }
        if let Some(n) = self.starts(p, "scope_on") {
            return p.get(n).map(|&b| Event::ScopeOn(b == 1));
        }
        // Single-byte commands last, so 1A/1C/25/26/27 above win.
        match p.first()? {
            0x00 | 0x03 => bcd::civ_to_freq(p.get(1..6)?).ok().map(Event::Frequency),
            0x01 | 0x04 => {
                let mode = mode_from_code(*p.get(1)?)?;
                let filter = p.get(2).and_then(|&f| bcd::from_byte(f).ok());
                Some(Event::ModeHint(ModeSetting { mode, filter, data: false }))
            }
            _ => None,
        }
    }
}

impl Protocol for IcomCiv {
    fn kind(&self) -> RigKind {
        self.kind
    }

    fn encode(&self, req: &Request) -> Option<Encoded> {
        Some(match req {
            Request::ReadId => self.get("read_id"),
            Request::ReadFrequency => self.get("selected_vfo_frequency"),
            Request::SetFrequency(hz) => self.set("selected_vfo_frequency", &bcd::freq_to_civ(*hz)),
            Request::ReadMode => self.get("selected_vfo_mode"),
            Request::SetMode(m) => {
                let code = mode_code(m.mode)?;
                self.set("selected_vfo_mode", &[code, u8::from(m.data), bcd::byte(m.filter.unwrap_or(1))])
            }
            Request::ReadSMeter => self.get("s_meter"),
            Request::ReadPowerLevel => self.get("rf_power"),
            Request::SetPowerLevel(pct) => {
                let raw = (u64::from((*pct).min(100)) * 255 + 50) / 100;
                self.set("rf_power", &bcd::encode_be(raw, 2))
            }
            Request::ReadPo => self.get("po_meter"),
            Request::ReadSwr => self.get("swr_meter"),
            Request::ReadAlc => self.get("alc_meter"),
            Request::ReadTx => self.get("tx_status"),
            Request::SetTx(on) => self.set("tx_status", &[u8::from(*on)]),
            Request::ReadMemory(n) => {
                let mut key = self.cat.bytes("memory_contents");
                key.extend(channel_bytes(*n));
                Encoded { bytes: self.frame(key.clone()), expect: Expect::Reply(key) }
            }
            Request::WriteMemory(ch) => {
                let mut d = channel_bytes(ch.number);
                d.extend(encode_memory(ch)?);
                self.set("memory_contents", &d)
            }
            Request::ScopeOn(on) => self.set("scope_on", &[u8::from(*on)]),
            Request::ScopeDataOutput(on) => self.set("scope_data_output", &[u8::from(*on)]),
            Request::ScopeSetMode(m) => {
                let c = match m {
                    ScopeMode::Center => 0,
                    ScopeMode::Fixed => 1,
                    ScopeMode::ScrollCenter => 2,
                    ScopeMode::ScrollFixed => 3,
                };
                self.set("scope_mode", &[0x00, c])
            }
            Request::ScopeSetSpan(hz) => {
                let mut d = vec![0x00];
                d.extend(bcd::encode_le(u64::from(*hz), 5));
                self.set("scope_span", &d)
            }
            Request::ScopeSetSpeed(s) => self.set("scope_speed", &[0x00, (*s).min(2)]),
            Request::SetDate { year, month, day } => {
                self.set("clock_date", &[bcd::byte((year / 100) as u8), bcd::byte((year % 100) as u8), bcd::byte(*month), bcd::byte(*day)])
            }
            Request::SetTime { hour, minute } => self.set("clock_time", &[bcd::byte(*hour), bcd::byte(*minute)]),
            Request::ReadClockTime => self.get("clock_time"),
            // CI-V transceive is a radio menu setting; nothing to send.
            Request::AutoInformation(_) => return None,
        })
    }

    fn feed(&mut self, bytes: &[u8]) -> Vec<Incoming> {
        let frames = self.decoder.push(bytes);
        let mut out = Vec::with_capacity(frames.len());
        for f in frames {
            // Our own frames echoed back (CI-V USB Echo Back) and other stations' traffic.
            if f.from != self.radio || (f.to != self.controller && f.to != BROADCAST) {
                continue;
            }
            match f.payload.as_slice() {
                [OK] => out.push(Incoming::Ok),
                [NG] => out.push(Incoming::Ng),
                [COLLISION, ..] | [] => {}
                p => {
                    let events = self.decode_payload(p).into_iter().collect();
                    out.push(Incoming::Message { payload: f.payload, events });
                }
            }
        }
        out
    }

    fn scope_stats(&self) -> ScopeStats {
        self.scope.stats
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn radio_frame(payload: &[u8]) -> Vec<u8> {
        Frame { to: 0xE0, from: 0x94, payload: payload.to_vec() }.encode()
    }

    #[test]
    fn manual_frame_layout() {
        // "FE FE 94 E0 Cn Sc Data area FD" (p. 19-2).
        let civ = IcomCiv::ic7300();
        let e = civ.encode(&Request::ReadSMeter).unwrap();
        assert_eq!(e.bytes, vec![0xFE, 0xFE, 0x94, 0xE0, 0x15, 0x02, 0xFD]);
        assert_eq!(e.expect, Expect::Reply(vec![0x15, 0x02]));
        let e = civ.encode(&Request::SetFrequency(14_074_000)).unwrap();
        assert_eq!(e.bytes, vec![0xFE, 0xFE, 0x94, 0xE0, 0x25, 0x00, 0x00, 0x40, 0x07, 0x14, 0x00, 0xFD]);
        assert_eq!(e.expect, Expect::Ack);
    }

    #[test]
    fn decoder_handles_noise_split_reads_and_repeated_preambles() {
        let mut d = FrameDecoder::default();
        let f = radio_frame(&[0x03, 0x00, 0x40, 0x07, 0x14, 0x00]);
        let mut stream = vec![0x12, 0x34, 0xFE, 0xFE, 0xFE];
        stream.extend(&f[2..]);
        stream.extend(radio_frame(&[OK]));
        let (a, b) = stream.split_at(7);
        let mut frames = d.push(a);
        frames.extend(d.push(b));
        assert_eq!(frames.len(), 2);
        assert_eq!(frames[0].payload, vec![0x03, 0x00, 0x40, 0x07, 0x14, 0x00]);
        assert_eq!(frames[1].payload, vec![OK]);
        assert!(d.discarded_bytes >= 2);
    }

    #[test]
    fn decoder_resyncs_after_truncated_frame() {
        let mut d = FrameDecoder::default();
        let mut stream = vec![0xFE, 0xFE, 0xE0, 0x94, 0x03, 0x00];
        stream.extend(radio_frame(&[OK]));
        let frames = d.push(&stream);
        // The cut-off frame is dropped and the OK frame after it still decodes.
        assert_eq!(frames.len(), 1);
        assert_eq!(frames[0].payload, vec![OK]);
        assert_eq!(d.push(&radio_frame(&[NG]))[0].payload, vec![NG]);
    }

    #[test]
    fn every_frame_type_round_trips_through_the_driver() {
        let mut civ = IcomCiv::ic7300();
        let cases: Vec<(Vec<u8>, Option<Event>)> = vec![
            (vec![0x25, 0x00, 0x00, 0x40, 0x07, 0x14, 0x00], Some(Event::Frequency(14_074_000))),
            (vec![0x03, 0x00, 0x40, 0x07, 0x14, 0x00], Some(Event::Frequency(14_074_000))),
            (vec![0x00, 0x00, 0x40, 0x07, 0x14, 0x00], Some(Event::Frequency(14_074_000))),
            (vec![0x26, 0x00, 0x01, 0x01, 0x01], Some(Event::Mode(ModeSetting { mode: Mode::Usb, filter: Some(1), data: true }))),
            (vec![0x04, 0x02, 0x02], Some(Event::ModeHint(ModeSetting { mode: Mode::Am, filter: Some(2), data: false }))),
            (vec![0x15, 0x02, 0x01, 0x20], Some(Event::SMeter(120))),
            (vec![0x15, 0x11, 0x02, 0x13], Some(Event::Po(213))),
            (vec![0x15, 0x12, 0x00, 0x48], Some(Event::Swr(48))),
            (vec![0x15, 0x13, 0x00, 0x10], Some(Event::Alc(10))),
            (vec![0x14, 0x0A, 0x02, 0x55], Some(Event::PowerLevel(100))),
            (vec![0x19, 0x00, 0x94], Some(Event::Id(0x94))),
            (vec![0x1C, 0x00, 0x01], Some(Event::Tx(true))),
            (vec![0x1A, 0x05, 0x00, 0x95, 0x17, 0x02], Some(Event::ClockTime { hour: 17, minute: 2 })),
            (vec![0x1A, 0x00, 0x00, 0x07, 0xFF], Some(Event::Memory(MemoryChannel::empty(7)))),
            (vec![0x27, 0x10, 0x01], Some(Event::ScopeOn(true))),
        ];
        for (payload, want) in cases {
            let got = civ.feed(&radio_frame(&payload));
            assert_eq!(got, vec![Incoming::Message { payload: payload.clone(), events: want.into_iter().collect() }], "{payload:02X?}");
        }
        assert_eq!(civ.feed(&radio_frame(&[OK])), vec![Incoming::Ok]);
        assert_eq!(civ.feed(&radio_frame(&[NG])), vec![Incoming::Ng]);
    }

    #[test]
    fn echo_and_foreign_frames_are_ignored() {
        let mut civ = IcomCiv::ic7300();
        let echo = civ.encode(&Request::ReadFrequency).unwrap().bytes;
        assert!(civ.feed(&echo).is_empty());
        let other = Frame { to: 0xE0, from: 0x88, payload: vec![0x03, 0, 0, 0, 0, 0] }.encode();
        assert!(civ.feed(&other).is_empty());
        let broadcast = Frame { to: 0x00, from: 0x94, payload: vec![0x00, 0x00, 0x40, 0x07, 0x14, 0x00] }.encode();
        assert_eq!(civ.feed(&broadcast).len(), 1);
    }

    #[test]
    fn memory_record_round_trips() {
        // A full 1A 00 channel record: 29.600 MHz FM, filter 1, 88.5 Hz tones, name "10M FM".
        let raw: Vec<u8> =
            "00 00 00 60 29 00 05 01 00 00 08 85 00 08 85 00 00 60 29 00 05 01 00 00 08 85 00 08 85 31 30 4D 20 46 4D 20 20 20 20"
                .split(' ')
                .map(|h| u8::from_str_radix(h, 16).unwrap())
                .collect();
        let ch = decode_memory(1, &raw).unwrap();
        assert_eq!(ch.freq_hz, 29_600_000);
        assert_eq!(ch.mode.mode, Mode::Fm);
        assert_eq!(ch.name, "10M FM");
        // Re-encoding with no edits gives the same bytes.
        assert_eq!(encode_memory(&ch).unwrap(), raw);
        // Editing keeps the tones and writes both VFO halves.
        let mut edited = ch.clone();
        edited.freq_hz = 29_000_000;
        edited.mode.mode = Mode::Am;
        edited.name = "10M AM".into();
        let enc = encode_memory(&edited).unwrap();
        let back = decode_memory(1, &enc).unwrap();
        assert!(back.same_content(&edited));
        assert_eq!(&enc[15..20], &enc[1..6]);
        assert_eq!(enc[20], 0x02);
        assert_eq!(&enc[9..12], &raw[9..12]);
    }

    #[test]
    fn memory_from_scratch_and_clear() {
        let ch = MemoryChannel::new(42, 7_074_000, Mode::Usb, "a very long name");
        let enc = encode_memory(&ch).unwrap();
        assert_eq!(enc.len(), MEMORY_DATA_LEN);
        let back = decode_memory(42, &enc).unwrap();
        assert_eq!(back.name, "a very lon");
        assert_eq!(encode_memory(&MemoryChannel::empty(3)).unwrap(), vec![0xFF]);
        let civ = IcomCiv::ic7300();
        let e = civ.encode(&Request::ReadMemory(101)).unwrap();
        assert_eq!(e.expect, Expect::Reply(vec![0x1A, 0x00, 0x01, 0x01]));
        assert!(civ.encode(&Request::SetMode(ModeSetting::new(Mode::C4fm))).is_none());
    }

    #[test]
    fn power_percent_scale() {
        let civ = IcomCiv::ic7300();
        let e = civ.encode(&Request::SetPowerLevel(100)).unwrap();
        assert_eq!(&e.bytes[6..8], &[0x02, 0x55]);
        let e = civ.encode(&Request::SetPowerLevel(50)).unwrap();
        assert_eq!(&e.bytes[6..8], &[0x01, 0x28]);
    }
}
