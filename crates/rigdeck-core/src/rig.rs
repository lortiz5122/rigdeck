//! Radio-neutral types shared by every driver, the engine, the servers and the UI.

use serde::{Deserialize, Serialize};

/// Operating mode, independent of any radio's encoding.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Mode {
    Lsb,
    Usb,
    Am,
    AmN,
    Cw,
    CwR,
    Rtty,
    RttyR,
    Fm,
    FmN,
    C4fm,
}

impl Mode {
    pub const ALL: [Mode; 11] =
        [Mode::Lsb, Mode::Usb, Mode::Am, Mode::AmN, Mode::Cw, Mode::CwR, Mode::Rtty, Mode::RttyR, Mode::Fm, Mode::FmN, Mode::C4fm];

    pub fn label(self) -> &'static str {
        match self {
            Mode::Lsb => "LSB",
            Mode::Usb => "USB",
            Mode::Am => "AM",
            Mode::AmN => "AM-N",
            Mode::Cw => "CW",
            Mode::CwR => "CW-R",
            Mode::Rtty => "RTTY",
            Mode::RttyR => "RTTY-R",
            Mode::Fm => "FM",
            Mode::FmN => "FM-N",
            Mode::C4fm => "C4FM",
        }
    }

    pub fn parse(s: &str) -> Option<Mode> {
        let u = s.trim().to_ascii_uppercase();
        Some(match u.as_str() {
            "LSB" => Mode::Lsb,
            "USB" => Mode::Usb,
            "AM" => Mode::Am,
            "AM-N" | "AMN" => Mode::AmN,
            "CW" | "CW-U" => Mode::Cw,
            "CW-R" | "CWR" | "CW-L" => Mode::CwR,
            "RTTY" | "RTTY-LSB" => Mode::Rtty,
            "RTTY-R" | "RTTYR" | "RTTY-USB" => Mode::RttyR,
            "FM" => Mode::Fm,
            "FM-N" | "FMN" => Mode::FmN,
            "C4FM" => Mode::C4fm,
            _ => return None,
        })
    }

    /// Hamlib mode name, used by the rigctld server.
    pub fn hamlib(self, data: bool) -> &'static str {
        match (self, data) {
            (Mode::Lsb, true) => "PKTLSB",
            (Mode::Usb, true) => "PKTUSB",
            (Mode::Fm, true) => "PKTFM",
            (Mode::Lsb, _) => "LSB",
            (Mode::Usb, _) => "USB",
            (Mode::Am, _) => "AM",
            (Mode::AmN, _) => "AMN",
            (Mode::Cw, _) => "CW",
            (Mode::CwR, _) => "CWR",
            (Mode::Rtty, _) => "RTTY",
            (Mode::RttyR, _) => "RTTYR",
            (Mode::Fm, _) => "FM",
            (Mode::FmN, _) => "FMN",
            (Mode::C4fm, _) => "C4FM",
        }
    }

    /// Hamlib mode name to (mode, data flag).
    pub fn from_hamlib(s: &str) -> Option<(Mode, bool)> {
        Some(match s.trim().to_ascii_uppercase().as_str() {
            "PKTLSB" => (Mode::Lsb, true),
            "PKTUSB" => (Mode::Usb, true),
            "PKTFM" => (Mode::Fm, true),
            "AMN" => (Mode::AmN, false),
            "FMN" => (Mode::FmN, false),
            other => (Mode::parse(other)?, false),
        })
    }
}

impl std::fmt::Display for Mode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.label())
    }
}

/// Mode with the filter and DATA settings that go with it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModeSetting {
    pub mode: Mode,
    /// Filter number 1–3 where the radio has numbered filters (Icom FIL1–FIL3).
    pub filter: Option<u8>,
    /// DATA mode (Icom DATA on, Yaesu DATA-LSB/USB/FM).
    pub data: bool,
}

impl ModeSetting {
    pub fn new(mode: Mode) -> Self {
        Self { mode, filter: None, data: false }
    }
    pub fn label(&self) -> String {
        let mut s = self.mode.label().to_string();
        if self.data {
            s = format!("{s}-D");
        }
        if let Some(f) = self.filter {
            s = format!("{s} FIL{f}");
        }
        s
    }
}

/// Spectrum scope display mode (Icom 27 14).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ScopeMode {
    Center,
    Fixed,
    ScrollCenter,
    ScrollFixed,
}

/// Which radios this app knows how to drive.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum RigKind {
    IcomIc7300,
    YaesuFt991a,
}

impl RigKind {
    pub const ALL: [RigKind; 2] = [RigKind::IcomIc7300, RigKind::YaesuFt991a];

    pub fn name(self) -> &'static str {
        match self {
            RigKind::IcomIc7300 => "Icom IC-7300",
            RigKind::YaesuFt991a => "Yaesu FT-991A",
        }
    }

    pub fn default_baud(self) -> u32 {
        match self {
            // CI-V USB Baud Rate default "Auto"; 115200 is needed for scope data (manual p. 19-8 *4).
            RigKind::IcomIc7300 => 115_200,
            // Menu 031 CAT RATE: 4800/9600/19200/38400. 38400 is the fastest; set the radio to match.
            RigKind::YaesuFt991a => 38_400,
        }
    }

    pub fn capabilities(self) -> Capabilities {
        match self {
            RigKind::IcomIc7300 => Capabilities {
                kind: self,
                has_scope: true,
                memory_channels: 1..=101,
                memory_name_len: 10,
                min_freq_hz: 30_000,
                max_freq_hz: 74_800_000,
                modes: vec![Mode::Lsb, Mode::Usb, Mode::Am, Mode::Cw, Mode::CwR, Mode::Rtty, Mode::RttyR, Mode::Fm],
                has_filters: true,
                bands: HF6_BANDS.to_vec(),
                has_transceive: true,
                has_clock: true,
            },
            RigKind::YaesuFt991a => Capabilities {
                kind: self,
                has_scope: false,
                memory_channels: 1..=117,
                memory_name_len: 12,
                min_freq_hz: 30_000,
                max_freq_hz: 470_000_000,
                modes: vec![
                    Mode::Lsb,
                    Mode::Usb,
                    Mode::Cw,
                    Mode::CwR,
                    Mode::Am,
                    Mode::AmN,
                    Mode::Fm,
                    Mode::FmN,
                    Mode::Rtty,
                    Mode::RttyR,
                    Mode::C4fm,
                ],
                has_filters: false,
                bands: {
                    let mut b = HF6_BANDS.to_vec();
                    b.push(Band { name: "2m", start_hz: 144_000_000, end_hz: 148_000_000, default_hz: 146_520_000 });
                    b.push(Band { name: "70cm", start_hz: 420_000_000, end_hz: 450_000_000, default_hz: 446_000_000 });
                    b
                },
                has_transceive: true,
                has_clock: false,
            },
        }
    }
}

/// An amateur band button.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Band {
    pub name: &'static str,
    pub start_hz: u64,
    pub end_hz: u64,
    pub default_hz: u64,
}

/// US amateur HF and 6 m bands (band plan facts, not radio facts).
pub const HF6_BANDS: [Band; 11] = [
    Band { name: "160m", start_hz: 1_800_000, end_hz: 2_000_000, default_hz: 1_840_000 },
    Band { name: "80m", start_hz: 3_500_000, end_hz: 4_000_000, default_hz: 3_573_000 },
    Band { name: "60m", start_hz: 5_330_500, end_hz: 5_406_400, default_hz: 5_357_000 },
    Band { name: "40m", start_hz: 7_000_000, end_hz: 7_300_000, default_hz: 7_074_000 },
    Band { name: "30m", start_hz: 10_100_000, end_hz: 10_150_000, default_hz: 10_136_000 },
    Band { name: "20m", start_hz: 14_000_000, end_hz: 14_350_000, default_hz: 14_074_000 },
    Band { name: "17m", start_hz: 18_068_000, end_hz: 18_168_000, default_hz: 18_100_000 },
    Band { name: "15m", start_hz: 21_000_000, end_hz: 21_450_000, default_hz: 21_074_000 },
    Band { name: "12m", start_hz: 24_890_000, end_hz: 24_990_000, default_hz: 24_915_000 },
    Band { name: "10m", start_hz: 28_000_000, end_hz: 29_700_000, default_hz: 28_074_000 },
    Band { name: "6m", start_hz: 50_000_000, end_hz: 54_000_000, default_hz: 50_313_000 },
];

/// What a connected radio can do. The UI hides what a radio lacks.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Capabilities {
    pub kind: RigKind,
    pub has_scope: bool,
    pub memory_channels: std::ops::RangeInclusive<u16>,
    pub memory_name_len: usize,
    pub min_freq_hz: u64,
    pub max_freq_hz: u64,
    pub modes: Vec<Mode>,
    pub has_filters: bool,
    pub bands: Vec<Band>,
    pub has_transceive: bool,
    pub has_clock: bool,
}

/// Radio-neutral requests. Each driver encodes the ones it supports.
#[derive(Debug, Clone, PartialEq)]
pub enum Request {
    ReadId,
    ReadFrequency,
    SetFrequency(u64),
    ReadMode,
    SetMode(ModeSetting),
    ReadSMeter,
    ReadPowerLevel,
    /// RF power in percent of the radio's maximum (Icom 0–255 scale, Yaesu 5–100 W).
    SetPowerLevel(u16),
    ReadPo,
    ReadSwr,
    ReadAlc,
    ReadTx,
    SetTx(bool),
    ReadMemory(u16),
    WriteMemory(crate::memory::MemoryChannel),
    ScopeOn(bool),
    ScopeDataOutput(bool),
    ScopeSetMode(ScopeMode),
    /// Half-span in Hz (Icom 27 15 is ± span).
    ScopeSetSpan(u32),
    /// Sweep speed: 0 fast, 1 mid, 2 slow (Icom 27 1A).
    ScopeSetSpeed(u8),
    /// Ask the radio to push changes (Icom CI-V transceive is a menu item; Yaesu AI1).
    AutoInformation(bool),
    /// Set the radio's date (in whatever time zone the caller chose).
    SetDate {
        year: u16,
        month: u8,
        day: u8,
    },
    /// Set the radio's time of day.
    SetTime {
        hour: u8,
        minute: u8,
    },
    ReadClockTime,
}

impl Request {
    /// True if this request makes the radio transmit.
    pub fn keys_transmitter(&self) -> bool {
        matches!(self, Request::SetTx(true))
    }

    /// Short name used for statistics and the FA/backoff table.
    pub fn name(&self) -> &'static str {
        match self {
            Request::ReadId => "read_id",
            Request::ReadFrequency => "read_frequency",
            Request::SetFrequency(_) => "set_frequency",
            Request::ReadMode => "read_mode",
            Request::SetMode(_) => "set_mode",
            Request::ReadSMeter => "read_s_meter",
            Request::ReadPowerLevel => "read_power",
            Request::SetPowerLevel(_) => "set_power",
            Request::ReadPo => "read_po",
            Request::ReadSwr => "read_swr",
            Request::ReadAlc => "read_alc",
            Request::ReadTx => "read_tx",
            Request::SetTx(_) => "set_tx",
            Request::ReadMemory(_) => "read_memory",
            Request::WriteMemory(_) => "write_memory",
            Request::ScopeOn(_) => "scope_on",
            Request::ScopeDataOutput(_) => "scope_data_output",
            Request::ScopeSetMode(_) => "scope_mode",
            Request::ScopeSetSpan(_) => "scope_span",
            Request::ScopeSetSpeed(_) => "scope_speed",
            Request::AutoInformation(_) => "auto_information",
            Request::SetDate { .. } => "set_date",
            Request::SetTime { .. } => "set_time",
            Request::ReadClockTime => "read_clock_time",
        }
    }
}

/// What the radio told us.
#[derive(Debug, Clone, PartialEq)]
pub enum Event {
    Id(u32),
    Frequency(u64),
    Mode(ModeSetting),
    /// A mode change pushed by the radio that does not say whether DATA is on (Icom transceive 01).
    /// The engine keeps the last DATA state and re-reads the mode.
    ModeHint(ModeSetting),
    /// Raw S-meter 0–255. Icom: 0=S0, 120=S9, 241=S9+60 dB (manual p. 19-3). Yaesu: 0–255.
    SMeter(u16),
    /// RF power setting in percent.
    PowerLevel(u16),
    Po(u16),
    Swr(u16),
    Alc(u16),
    Tx(bool),
    Memory(crate::memory::MemoryChannel),
    Sweep(crate::scope::Sweep),
    ScopeOn(bool),
    ClockTime {
        hour: u8,
        minute: u8,
    },
    AutoInformation(bool),
}

/// Approximate S-units text for a raw Icom-scale S-meter value.
pub fn s_meter_text(raw: u16) -> String {
    // Icom: 0 = S0, 120 = S9 (linear in between, 13.3 per S-unit), 241 = S9+60 dB.
    if raw <= 120 {
        let s = (f32::from(raw) / 120.0 * 9.0).round() as u16;
        format!("S{s}")
    } else {
        let db = ((f32::from(raw) - 120.0) / 121.0 * 60.0).round() as u16;
        format!("S9+{db}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mode_names_round_trip() {
        for m in Mode::ALL {
            assert_eq!(Mode::parse(m.label()), Some(m));
            assert_eq!(Mode::from_hamlib(m.hamlib(false)), Some((m, false)));
        }
        assert_eq!(Mode::from_hamlib("PKTUSB"), Some((Mode::Usb, true)));
    }

    #[test]
    fn s_meter_scale() {
        assert_eq!(s_meter_text(0), "S0");
        assert_eq!(s_meter_text(120), "S9");
        assert_eq!(s_meter_text(241), "S9+60");
    }

    #[test]
    fn capabilities_are_sane() {
        for k in RigKind::ALL {
            let c = k.capabilities();
            assert!(c.min_freq_hz < c.max_freq_hz);
            for b in &c.bands {
                assert!(b.start_hz <= b.default_hz && b.default_hz <= b.end_hz, "{}", b.name);
                assert!(b.end_hz <= c.max_freq_hz);
            }
        }
    }
}
