//! Memory channels: a radio-neutral record, CSV import and export, change preview and
//! read-back verification.
//!
//! Icom layout source: IC-7300 Full Manual p. 19-11, "Memory content" (command 1A 00):
//! byte 3 split/select, 4–8 frequency, 9–10 mode and filter, 11 data mode and tone type,
//! 12–14 repeater tone, 15–17 tone squelch, then 4–17 again for the second VFO, then a
//! 10-character name. FF in byte 3 clears a channel.
//! Yaesu layout source: FT-991A CAT manual, MT command (pdf page 13).

use crate::rig::{Mode, ModeSetting, RigKind};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// Driver-specific fields kept so a read-modify-write does not lose settings we do not edit.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum MemoryExtra {
    #[default]
    None,
    /// The Icom 1A 00 data after the channel number (39 bytes).
    Icom { raw: Vec<u8> },
    /// Yaesu MT fields other than frequency, mode and tag.
    Yaesu { clar_offset_hz: i32, rx_clar: bool, tx_clar: bool, ctcss: u8, shift: u8 },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MemoryChannel {
    /// Icom: 1–99, 100 = P1, 101 = P2. Yaesu: 1–99, 100–117 = P-1L … P-9U.
    pub number: u16,
    pub empty: bool,
    pub freq_hz: u64,
    pub mode: ModeSetting,
    /// Name or tag, without trailing padding.
    pub name: String,
    pub extra: MemoryExtra,
}

impl MemoryChannel {
    pub fn empty(number: u16) -> Self {
        Self { number, empty: true, freq_hz: 0, mode: ModeSetting::new(Mode::Usb), name: String::new(), extra: MemoryExtra::None }
    }

    pub fn new(number: u16, freq_hz: u64, mode: Mode, name: &str) -> Self {
        Self { number, empty: false, freq_hz, mode: ModeSetting::new(mode), name: name.trim_end().to_string(), extra: MemoryExtra::None }
    }

    /// The channel number as the radio displays it.
    pub fn label(&self, kind: RigKind) -> String {
        channel_label(kind, self.number)
    }

    /// True when frequency, mode and name match (what the verify step compares).
    pub fn same_content(&self, other: &MemoryChannel) -> bool {
        if self.empty || other.empty {
            return self.empty == other.empty;
        }
        self.freq_hz == other.freq_hz && self.mode.mode == other.mode.mode && self.name.trim_end() == other.name.trim_end()
    }
}

pub fn channel_label(kind: RigKind, n: u16) -> String {
    match (kind, n) {
        (RigKind::IcomIc7300, 100) => "P1".into(),
        (RigKind::IcomIc7300, 101) => "P2".into(),
        (RigKind::YaesuFt991a, 100..=117) => {
            let k = n - 100;
            format!("P-{}{}", k / 2 + 1, if k.is_multiple_of(2) { 'L' } else { 'U' })
        }
        _ => n.to_string(),
    }
}

/// Parse a channel label ("12", "P1", "P-3U").
pub fn parse_channel_label(kind: RigKind, s: &str) -> Option<u16> {
    let t = s.trim().trim_matches('"');
    if let Ok(n) = t.parse::<u16>() {
        return Some(n);
    }
    let u = t.to_ascii_uppercase();
    match kind {
        RigKind::IcomIc7300 => match u.as_str() {
            "P1" => Some(100),
            "P2" => Some(101),
            _ => None,
        },
        RigKind::YaesuFt991a => {
            let rest = u.strip_prefix("P-")?;
            let (d, lu) = rest.split_at(rest.len().checked_sub(1)?);
            let k: u16 = d.parse().ok()?;
            let off = match lu {
                "L" => 0,
                "U" => 1,
                _ => return None,
            };
            Some(100 + (k - 1) * 2 + off)
        }
    }
}

// ---------------------------------------------------------------- CSV

#[derive(Debug, Clone, PartialEq)]
pub struct CsvError(pub String);
impl std::fmt::Display for CsvError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}
impl std::error::Error for CsvError {}

fn find_col(headers: &csv::StringRecord, names: &[&str]) -> Option<usize> {
    headers.iter().position(|h| names.iter().any(|n| h.trim().eq_ignore_ascii_case(n)))
}

/// Read channels from CSV. Accepts RigDeck's own export, planning sheets
/// (`Memory, Freq_MHz, Mode_Written, Name`), raw backups (`Raw_1A00_hex`) and Icom CS-7300
/// exports (`Num, Name, Freq, Mode, Filter, Data`).
pub fn import_csv(kind: RigKind, text: &str) -> Result<Vec<MemoryChannel>, CsvError> {
    let mut rdr = csv::ReaderBuilder::new().flexible(true).from_reader(text.as_bytes());
    let headers = rdr.headers().map_err(|e| CsvError(e.to_string()))?.clone();
    let c_num = find_col(&headers, &["Memory", "Num", "Channel_Number"]).ok_or_else(|| CsvError("no Memory or Num column".into()))?;
    let c_freq = find_col(&headers, &["Freq_MHz", "Freq"]).ok_or_else(|| CsvError("no Freq_MHz or Freq column".into()))?;
    let c_mode = find_col(&headers, &["Mode_Written", "Mode"]).ok_or_else(|| CsvError("no Mode column".into()))?;
    let c_name = find_col(&headers, &["Name", "Tag"]);
    let c_data = find_col(&headers, &["Data"]);
    let c_filter = find_col(&headers, &["Filter"]);
    let c_raw = find_col(&headers, &["Raw_1A00_hex", "Raw_hex"]);
    let mut out = Vec::new();
    for (i, rec) in rdr.records().enumerate() {
        let line = i + 2;
        let rec = rec.map_err(|e| CsvError(format!("line {line}: {e}")))?;
        let get = |c: usize| rec.get(c).unwrap_or("").trim().to_string();
        let num = parse_channel_label(kind, &get(c_num)).ok_or_else(|| CsvError(format!("line {line}: bad channel {:?}", get(c_num))))?;
        let freq_s = get(c_freq);
        if freq_s.is_empty() {
            out.push(MemoryChannel::empty(num));
            continue;
        }
        let mhz: f64 = freq_s.parse().map_err(|_| CsvError(format!("line {line}: bad frequency {freq_s:?}")))?;
        let freq_hz = (mhz * 1_000_000.0).round() as u64;
        let mode_s = get(c_mode);
        let mode = Mode::parse(&mode_s)
            .or_else(|| Mode::from_hamlib(&mode_s).map(|m| m.0))
            .ok_or_else(|| CsvError(format!("line {line}: unknown mode {mode_s:?}")))?;
        let mut ch = MemoryChannel::new(num, freq_hz, mode, &c_name.map(get).unwrap_or_default());
        if let Some(c) = c_data {
            let d = get(c).to_ascii_uppercase();
            ch.mode.data = d == "ON" || d == "1" || d.starts_with("DATA") || d == "TRUE";
        }
        if let Some(c) = c_filter {
            let f = get(c).to_ascii_uppercase();
            ch.mode.filter = f.trim_start_matches("FIL").parse::<u8>().ok().filter(|v| (1..=3).contains(v));
        }
        if let Some(c) = c_raw {
            let hex = get(c);
            if !hex.is_empty() {
                let raw: Result<Vec<u8>, _> = hex.split_whitespace().map(|h| u8::from_str_radix(h, 16)).collect();
                let raw = raw.map_err(|_| CsvError(format!("line {line}: bad raw hex")))?;
                ch.extra = MemoryExtra::Icom { raw };
            }
        }
        out.push(ch);
    }
    Ok(out)
}

/// Write channels as CSV (RigDeck format). Empty channels are written with blank fields.
pub fn export_csv(kind: RigKind, channels: &[MemoryChannel]) -> String {
    let mut w = csv::Writer::from_writer(Vec::new());
    w.write_record(["Memory", "Freq_MHz", "Mode", "Data", "Filter", "Name", "Raw_hex"]).unwrap();
    for ch in channels {
        if ch.empty {
            w.write_record([channel_label(kind, ch.number).as_str(), "", "", "", "", "", ""]).unwrap();
            continue;
        }
        let raw = match &ch.extra {
            MemoryExtra::Icom { raw } => raw.iter().map(|b| format!("{b:02X}")).collect::<Vec<_>>().join(" "),
            _ => String::new(),
        };
        w.write_record([
            channel_label(kind, ch.number),
            format!("{:.6}", ch.freq_hz as f64 / 1e6),
            ch.mode.mode.label().to_string(),
            if ch.mode.data { "ON".into() } else { "OFF".into() },
            ch.mode.filter.map(|f| format!("FIL{f}")).unwrap_or_default(),
            ch.name.clone(),
            raw,
        ])
        .unwrap();
    }
    String::from_utf8(w.into_inner().unwrap()).unwrap()
}

// ---------------------------------------------------------------- preview and verify

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ChangeKind {
    Unchanged,
    New,
    Changed,
    Cleared,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Change {
    pub number: u16,
    pub kind: ChangeKind,
    pub before: Option<MemoryChannel>,
    pub after: MemoryChannel,
}

/// Compare planned channels with what is in the radio now.
pub fn preview(current: &[MemoryChannel], planned: &[MemoryChannel]) -> Vec<Change> {
    let now: BTreeMap<u16, &MemoryChannel> = current.iter().map(|c| (c.number, c)).collect();
    planned
        .iter()
        .map(|p| {
            let before = now.get(&p.number).map(|c| (*c).clone());
            let kind = match &before {
                None => ChangeKind::New,
                Some(b) if b.same_content(p) => ChangeKind::Unchanged,
                Some(_) if p.empty => ChangeKind::Cleared,
                Some(b) if b.empty => ChangeKind::New,
                Some(_) => ChangeKind::Changed,
            };
            Change { number: p.number, kind, before, after: p.clone() }
        })
        .collect()
}

#[derive(Debug, Clone, PartialEq)]
pub struct Mismatch {
    pub number: u16,
    pub expected: MemoryChannel,
    pub found: Option<MemoryChannel>,
}

/// Read-back check: every expected channel must be present with the same frequency, mode and name.
pub fn verify(expected: &[MemoryChannel], read_back: &[MemoryChannel]) -> Vec<Mismatch> {
    let got: BTreeMap<u16, &MemoryChannel> = read_back.iter().map(|c| (c.number, c)).collect();
    expected
        .iter()
        .filter_map(|e| match got.get(&e.number) {
            Some(g) if g.same_content(e) => None,
            other => Some(Mismatch { number: e.number, expected: e.clone(), found: other.map(|c| (*c).clone()) }),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn labels() {
        let k = RigKind::IcomIc7300;
        assert_eq!(channel_label(k, 100), "P1");
        assert_eq!(parse_channel_label(k, "P2"), Some(101));
        let y = RigKind::YaesuFt991a;
        assert_eq!(channel_label(y, 100), "P-1L");
        assert_eq!(channel_label(y, 117), "P-9U");
        for n in 1..=117 {
            assert_eq!(parse_channel_label(y, &channel_label(y, n)), Some(n));
        }
    }

    #[test]
    fn imports_sheet_with_extra_columns() {
        let text = "Memory,Rank,Channel,Freq_MHz,Mode_Written,Primary_Modes_As_Pasted,Name,Usage\n\
                    1,1,20,14.074,USB,USB,FT8 20m,\"Digital, FT8\"\n\
                    3,3,40,7.200,LSB,LSB (USB),40m SSB,SSB calling\n";
        let ch = import_csv(RigKind::IcomIc7300, text).unwrap();
        assert_eq!(ch.len(), 2);
        assert_eq!(ch[0].number, 1);
        assert_eq!(ch[0].freq_hz, 14_074_000);
        assert_eq!(ch[0].mode.mode, Mode::Usb);
        assert_eq!(ch[0].name, "FT8 20m");
        assert_eq!(ch[1].mode.mode, Mode::Lsb);
    }

    #[test]
    fn export_import_round_trip() {
        let mut a = MemoryChannel::new(5, 14_074_000, Mode::Usb, "FT8");
        a.mode.data = true;
        a.mode.filter = Some(1);
        a.extra = MemoryExtra::Icom { raw: vec![0, 1, 2] };
        let chans = vec![a, MemoryChannel::empty(6), MemoryChannel::new(100, 7_000_000, Mode::Cw, "")];
        let text = export_csv(RigKind::IcomIc7300, &chans);
        let back = import_csv(RigKind::IcomIc7300, &text).unwrap();
        assert_eq!(back, chans);
    }

    #[test]
    fn preview_and_verify() {
        let current = vec![
            MemoryChannel::new(1, 14_074_000, Mode::Usb, "FT8 20m"),
            MemoryChannel::empty(2),
            MemoryChannel::new(3, 1, Mode::Usb, "x"),
        ];
        let planned = vec![
            MemoryChannel::new(1, 14_074_000, Mode::Usb, "FT8 20m   "),
            MemoryChannel::new(2, 7_074_000, Mode::Usb, "FT8 40m"),
            MemoryChannel::new(3, 2, Mode::Usb, "x"),
            MemoryChannel::empty(3),
        ];
        let p = preview(&current, &planned);
        let kinds: Vec<_> = p.iter().map(|c| c.kind.clone()).collect();
        assert_eq!(kinds, vec![ChangeKind::Unchanged, ChangeKind::New, ChangeKind::Changed, ChangeKind::Cleared]);
        assert!(verify(&planned[..1], &current).is_empty());
        let m = verify(&planned[1..2], &current);
        assert_eq!(m.len(), 1);
        assert_eq!(m[0].number, 2);
    }

    #[test]
    fn imports_cs7300_export_with_padded_names() {
        let text = "\"Num\",\"Name\",\"Split\",\"Freq\",\"Mode\",\"Filter\",\"Data\"\n\"001\",\"10M FM    \",\"OFF\",\"29.60000\",\"FM\",\"FIL1\",\"OFF\"\n";
        let ch = import_csv(RigKind::IcomIc7300, text).unwrap();
        assert_eq!(ch[0], {
            let mut c = MemoryChannel::new(1, 29_600_000, Mode::Fm, "10M FM");
            c.mode.filter = Some(1);
            c
        });
    }
}
