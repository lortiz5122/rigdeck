//! Yaesu CAT (ASCII) and the FT-991A driver.
//!
//! Written from the Yaesu FT-991A CAT Operation Reference Manual (see `catalogue/ft991a.json`).
//! Commands are two letters, fixed-width parameters and `;`. A refused command answers `?;`
//! (standard Yaesu CAT behaviour; not printed in the FT-991A manual, so the driver treats any
//! `?` message as a refusal).

use crate::catalogue::{self, CatCatalogue};
use crate::memory::{MemoryChannel, MemoryExtra};
use crate::protocol::{Encoded, Expect, Incoming, Protocol};
use crate::rig::{Event, Mode, ModeSetting, Request, RigKind};

const MAX_MESSAGE: usize = 128;
pub const TAG_LEN: usize = 12;

pub fn mode_char(m: ModeSetting) -> Option<char> {
    Some(match (m.mode, m.data) {
        (Mode::Lsb, true) => '8',
        (Mode::Usb, true) => 'C',
        (Mode::Fm, true) => 'A',
        (Mode::Lsb, false) => '1',
        (Mode::Usb, false) => '2',
        (Mode::Cw, _) => '3',
        (Mode::Fm, false) => '4',
        (Mode::Am, _) => '5',
        (Mode::Rtty, _) => '6',
        (Mode::CwR, _) => '7',
        (Mode::RttyR, _) => '9',
        (Mode::FmN, _) => 'B',
        (Mode::AmN, _) => 'D',
        (Mode::C4fm, _) => 'E',
    })
}

pub fn mode_from_char(c: char) -> Option<ModeSetting> {
    let (mode, data) = match c.to_ascii_uppercase() {
        '1' => (Mode::Lsb, false),
        '2' => (Mode::Usb, false),
        '3' => (Mode::Cw, false),
        '4' => (Mode::Fm, false),
        '5' => (Mode::Am, false),
        '6' => (Mode::Rtty, false),
        '7' => (Mode::CwR, false),
        '8' => (Mode::Lsb, true),
        '9' => (Mode::RttyR, false),
        'A' => (Mode::Fm, true),
        'B' => (Mode::FmN, false),
        'C' => (Mode::Usb, true),
        'D' => (Mode::AmN, false),
        'E' => (Mode::C4fm, false),
        _ => return None,
    };
    Some(ModeSetting { mode, filter: None, data })
}

/// The MT/MR/IF body after the two-letter command:
/// channel(3) freq(9) clar(5) rxclar(1) txclar(1) mode(1) vfo/mem(1) ctcss(1) 00(2) shift(1)
/// and, for MT only, 0(1) tag(12).
fn parse_channel_body(body: &str, with_tag: bool) -> Option<MemoryChannel> {
    let b = body.as_bytes();
    if b.len() < 25 || !b.is_ascii() {
        return None;
    }
    let number: u16 = body.get(0..3)?.parse().ok()?;
    let freq: u64 = body.get(3..12)?.parse().ok()?;
    let clar_sign = if &body[12..13] == "-" { -1 } else { 1 };
    let clar: i32 = body.get(13..17)?.trim().parse().unwrap_or(0);
    let rx_clar = &body[17..18] == "1";
    let tx_clar = &body[18..19] == "1";
    let mode = mode_from_char(body[19..20].chars().next()?)?;
    let ctcss = body[21..22].parse().unwrap_or(0);
    let shift = body[24..25].parse().unwrap_or(0);
    let name = if with_tag { body.get(26..).unwrap_or("").trim_end().to_string() } else { String::new() };
    Some(MemoryChannel {
        number,
        empty: false,
        freq_hz: freq,
        mode,
        name,
        extra: MemoryExtra::Yaesu { clar_offset_hz: clar_sign * clar, rx_clar, tx_clar, ctcss, shift },
    })
}

pub fn encode_mt(ch: &MemoryChannel) -> Option<String> {
    let (clar, rx, tx, ctcss, shift) = match ch.extra {
        MemoryExtra::Yaesu { clar_offset_hz, rx_clar, tx_clar, ctcss, shift } => (clar_offset_hz, rx_clar, tx_clar, ctcss, shift),
        _ => (0, false, false, 0, 0),
    };
    let mut tag: String = ch.name.chars().filter(|c| c.is_ascii() && !c.is_ascii_control() && *c != ';').take(TAG_LEN).collect();
    while tag.len() < TAG_LEN {
        tag.push(' ');
    }
    Some(format!(
        "MT{:03}{:09}{}{:04}{}{}{}0{}00{}0{};",
        ch.number,
        ch.freq_hz,
        if clar < 0 { '-' } else { '+' },
        clar.unsigned_abs().min(9999),
        u8::from(rx),
        u8::from(tx),
        mode_char(ch.mode)?,
        ctcss.min(4),
        shift.min(2),
        tag
    ))
}

pub struct YaesuCat {
    kind: RigKind,
    cat: &'static CatCatalogue,
    buf: Vec<u8>,
    pub discarded_bytes: u64,
}

impl YaesuCat {
    pub fn ft991a() -> Self {
        Self { kind: RigKind::YaesuFt991a, cat: catalogue::ft991a(), buf: Vec::new(), discarded_bytes: 0 }
    }

    fn read(&self, name: &str, p: &str) -> Encoded {
        let key = format!("{}{p}", self.cat.code(name));
        Encoded { bytes: format!("{key};").into_bytes(), expect: Expect::Reply(key.into_bytes()) }
    }

    fn set(&self, name: &str, p: &str) -> Encoded {
        Encoded { bytes: format!("{}{p};", self.cat.code(name)).into_bytes(), expect: Expect::Nothing }
    }

    fn decode(&self, msg: &str) -> Vec<Event> {
        let num = |s: Option<&str>| -> Option<u16> { s?.trim().parse().ok() };
        let code = msg.get(0..2).unwrap_or("");
        let rest = msg.get(2..).unwrap_or("");
        let ev = match code {
            "FA" => rest.parse().ok().map(Event::Frequency),
            "MD" => rest.get(1..2).and_then(|c| c.chars().next()).and_then(mode_from_char).map(Event::Mode),
            "SM" => num(rest.get(1..)).map(Event::SMeter),
            "PC" => num(Some(rest)).map(Event::PowerLevel),
            "RM" => {
                let v = num(rest.get(1..4));
                match rest.get(0..1) {
                    Some("1") => v.map(Event::SMeter),
                    Some("4") => v.map(Event::Alc),
                    Some("5") => v.map(Event::Po),
                    Some("6") => v.map(Event::Swr),
                    _ => None,
                }
            }
            "TX" => rest.get(0..1).map(|c| Event::Tx(c != "0")),
            "ID" => rest.parse().ok().map(Event::Id),
            "AI" => rest.get(0..1).map(|c| Event::AutoInformation(c == "1")),
            "MT" => parse_channel_body(rest, true).map(Event::Memory),
            "IF" => {
                return match parse_channel_body(rest, false) {
                    Some(ch) => vec![Event::Frequency(ch.freq_hz), Event::Mode(ch.mode)],
                    None => vec![],
                };
            }
            _ => None,
        };
        ev.into_iter().collect()
    }
}

impl Protocol for YaesuCat {
    fn kind(&self) -> RigKind {
        self.kind
    }

    fn encode(&self, req: &Request) -> Option<Encoded> {
        Some(match req {
            Request::ReadId => self.read("identification", ""),
            Request::ReadFrequency => self.read("frequency_vfo_a", ""),
            Request::SetFrequency(hz) => {
                let caps = self.capabilities();
                if *hz < caps.min_freq_hz || *hz > caps.max_freq_hz {
                    return None;
                }
                self.set("frequency_vfo_a", &format!("{hz:09}"))
            }
            Request::ReadMode => self.read("mode", "0"),
            Request::SetMode(m) => self.set("mode", &format!("0{}", mode_char(*m)?)),
            Request::ReadSMeter => self.read("s_meter", "0"),
            Request::ReadPowerLevel => self.read("power_control", ""),
            Request::SetPowerLevel(pct) => self.set("power_control", &format!("{:03}", (*pct).clamp(5, 100))),
            Request::ReadPo => self.read("read_meter", "5"),
            Request::ReadSwr => self.read("read_meter", "6"),
            Request::ReadAlc => self.read("read_meter", "4"),
            Request::ReadTx => self.read("tx_set", ""),
            Request::SetTx(on) => self.set("tx_set", if *on { "1" } else { "0" }),
            Request::ReadMemory(n) => self.read("memory_write_tag", &format!("{n:03}")),
            Request::WriteMemory(ch) => {
                if ch.empty {
                    return None; // The CAT manual has no clear-channel command.
                }
                Encoded { bytes: encode_mt(ch)?.into_bytes(), expect: Expect::Nothing }
            }
            Request::AutoInformation(on) => self.set("auto_information", if *on { "1" } else { "0" }),
            Request::ScopeOn(_)
            | Request::ScopeDataOutput(_)
            | Request::ScopeSetMode(_)
            | Request::ScopeSetSpan(_)
            | Request::ScopeSetSpeed(_)
            | Request::SetDate { .. }
            | Request::SetTime { .. }
            | Request::ReadClockTime => return None,
        })
    }

    fn feed(&mut self, bytes: &[u8]) -> Vec<Incoming> {
        self.buf.extend_from_slice(bytes);
        let mut out = Vec::new();
        while let Some(end) = self.buf.iter().position(|&b| b == b';') {
            let raw: Vec<u8> = self.buf.drain(..=end).collect();
            let text: String =
                String::from_utf8_lossy(&raw[..raw.len() - 1]).chars().filter(|c| !c.is_control()).collect::<String>().to_ascii_uppercase();
            let text = text.trim_start().to_string();
            if text.is_empty() {
                continue;
            }
            if text.starts_with('?') {
                out.push(Incoming::Ng);
                continue;
            }
            let events = self.decode(&text);
            // Keep the tag's original case: decode from the raw text for MT.
            let events = if text.starts_with("MT") {
                let orig = String::from_utf8_lossy(&raw[..raw.len() - 1]).trim_start().to_string();
                self.decode(&format!("MT{}", orig.get(2..).unwrap_or("")))
            } else {
                events
            };
            out.push(Incoming::Message { payload: text.into_bytes(), events });
        }
        if self.buf.len() > MAX_MESSAGE {
            self.discarded_bytes += self.buf.len() as u64;
            self.buf.clear();
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn manual_example() {
        // "FA014250000;" sets VFO-A to 14.250000 MHz (manual pdf page 3).
        let y = YaesuCat::ft991a();
        let e = y.encode(&Request::SetFrequency(14_250_000)).unwrap();
        assert_eq!(e.bytes, b"FA014250000;");
        assert_eq!(e.expect, Expect::Nothing);
        let r = y.encode(&Request::ReadFrequency).unwrap();
        assert_eq!(r.bytes, b"FA;");
        assert_eq!(r.expect, Expect::Reply(b"FA".to_vec()));
        assert!(y.encode(&Request::SetFrequency(500_000_000)).is_none());
    }

    #[test]
    fn answers_decode() {
        let mut y = YaesuCat::ft991a();
        let got = y.feed(b"FA014250000;MD0C;SM0128;ID0670;PC050;RM6012;TX0;AI1;?;");
        let events: Vec<_> = got
            .iter()
            .filter_map(|m| match m {
                Incoming::Message { events, .. } => Some(events.clone()),
                _ => None,
            })
            .flatten()
            .collect();
        assert_eq!(
            events,
            vec![
                Event::Frequency(14_250_000),
                Event::Mode(ModeSetting { mode: Mode::Usb, filter: None, data: true }),
                Event::SMeter(128),
                Event::Id(670),
                Event::PowerLevel(50),
                Event::Swr(12),
                Event::Tx(false),
                Event::AutoInformation(true),
            ]
        );
        assert_eq!(got.last(), Some(&Incoming::Ng));
    }

    #[test]
    fn split_reads_and_lowercase() {
        let mut y = YaesuCat::ft991a();
        assert!(y.feed(b"fa0142").is_empty());
        let got = y.feed(b"50000;");
        assert_eq!(got, vec![Incoming::Message { payload: b"FA014250000".to_vec(), events: vec![Event::Frequency(14_250_000)] }]);
    }

    #[test]
    fn memory_tag_round_trip() {
        let mut ch = MemoryChannel::new(12, 146_520_000, Mode::Fm, "Simplex Call");
        ch.extra = MemoryExtra::Yaesu { clar_offset_hz: -120, rx_clar: true, tx_clar: false, ctcss: 2, shift: 1 };
        let s = encode_mt(&ch).unwrap();
        assert_eq!(s.len(), 2 + 38 + 1);
        assert_eq!(&s[..5], "MT012");
        let mut y = YaesuCat::ft991a();
        let got = y.feed(s.as_bytes());
        let Incoming::Message { events, .. } = &got[0] else { panic!() };
        assert_eq!(events, &vec![Event::Memory(ch)]);
    }

    #[test]
    fn information_gives_frequency_and_mode() {
        let mut y = YaesuCat::ft991a();
        let got = y.feed(b"IF001007074000+000000C00000;");
        let Incoming::Message { events, .. } = &got[0] else { panic!() };
        assert_eq!(events[0], Event::Frequency(7_074_000));
        assert_eq!(events[1], Event::Mode(ModeSetting { mode: Mode::Usb, filter: None, data: true }));
    }

    #[test]
    fn modes_round_trip() {
        for m in RigKind::YaesuFt991a.capabilities().modes {
            let s = ModeSetting::new(m);
            assert_eq!(mode_from_char(mode_char(s).unwrap()), Some(s));
        }
        for m in [Mode::Lsb, Mode::Usb, Mode::Fm] {
            let s = ModeSetting { mode: m, filter: None, data: true };
            assert_eq!(mode_from_char(mode_char(s).unwrap()), Some(s));
        }
    }
}
