//! Spectrum scope sweep reassembly for Icom CI-V command 27 00.
//!
//! Source: IC-7300 Full Manual p. 19-14, "Scope waveform data": each division starts with
//! `00` (main scope), the division number and the division count (both BCD, 01–11 over USB).
//! Division 1 carries the scope mode, the waveform information (centre + span, or lower + upper
//! edge, five BCD bytes each) and an out-of-range flag. Divisions 2–11 carry pixels 00–A0.
//! The pixel count is "judged by the controller" (475 in the manual; 425 has been seen over USB),
//! so the assembler accepts whatever arrives.

use crate::bcd;
use crate::rig::ScopeMode;
use serde::{Deserialize, Serialize};

/// One complete sweep.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Sweep {
    pub mode: ScopeMode,
    /// Lower edge in Hz.
    pub low_hz: u64,
    /// Upper edge in Hz.
    pub high_hz: u64,
    pub out_of_range: bool,
    /// Amplitudes 0–160.
    pub pixels: Vec<u8>,
}

/// Maximum pixel value (A0).
pub const AMP_MAX: u8 = 160;

/// Counters exposed to the UI and to the stage 4 check.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct ScopeStats {
    pub sweeps: u64,
    /// Sweeps abandoned because a division was missing or out of order.
    pub dropped: u64,
    pub out_of_range: u64,
}

#[derive(Debug, Default)]
pub struct SweepAssembler {
    header: Option<(ScopeMode, u64, u64, bool)>,
    expected_next: u8,
    count: u8,
    pixels: Vec<u8>,
    pub stats: ScopeStats,
}

#[derive(Debug, PartialEq)]
pub enum ScopeError {
    Short,
    Bcd,
    BadMode(u8),
}

impl SweepAssembler {
    pub fn new() -> Self {
        Self::default()
    }

    fn abandon(&mut self) {
        if self.header.is_some() {
            self.stats.dropped += 1;
        }
        self.header = None;
        self.pixels.clear();
        self.expected_next = 0;
    }

    /// Feed the data that follows `27 00` in one frame. Returns a sweep when the last division
    /// of a complete sequence arrives.
    pub fn push(&mut self, data: &[u8]) -> Result<Option<Sweep>, ScopeError> {
        if data.len() < 3 {
            return Err(ScopeError::Short);
        }
        let seq = bcd::from_byte(data[1]).map_err(|_| ScopeError::Bcd)?;
        let max = bcd::from_byte(data[2]).map_err(|_| ScopeError::Bcd)?;
        if seq == 1 {
            // A new sweep. Anything half-built is lost.
            self.abandon();
            if data.len() < 4 {
                return Err(ScopeError::Short);
            }
            let mode = match data[3] {
                0 => ScopeMode::Center,
                1 => ScopeMode::Fixed,
                2 => ScopeMode::ScrollCenter,
                3 => ScopeMode::ScrollFixed,
                m => return Err(ScopeError::BadMode(m)),
            };
            if data.len() < 15 {
                return Err(ScopeError::Short);
            }
            let a = &data[4..9];
            let b = &data[9..14];
            let oor = data[14] == 1;
            let (low, high) = match mode {
                ScopeMode::Center | ScopeMode::ScrollCenter => {
                    let centre = bcd::decode_le(a).map_err(|_| ScopeError::Bcd)?;
                    let span = bcd::decode_le(b).map_err(|_| ScopeError::Bcd)?;
                    (centre.saturating_sub(span), centre + span)
                }
                ScopeMode::Fixed | ScopeMode::ScrollFixed => {
                    // A lower edge whose 1 GHz digit is F is negative (manual note); treat it as 0.
                    let low = if a[4] & 0xF0 == 0xF0 { 0 } else { bcd::decode_le(a).map_err(|_| ScopeError::Bcd)? };
                    (low, bcd::decode_le(b).map_err(|_| ScopeError::Bcd)?)
                }
            };
            self.header = Some((mode, low, high, oor));
            self.count = max;
            self.expected_next = 2;
            if oor {
                self.stats.out_of_range += 1;
            }
            if max <= 1 || oor {
                // Out of range: the waveform data is omitted.
                return Ok(self.finish());
            }
            return Ok(None);
        }
        if self.header.is_none() {
            // Joined mid-sweep; wait for division 1. Not counted as a drop.
            return Ok(None);
        }
        if seq != self.expected_next || max != self.count {
            self.abandon();
            return Ok(None);
        }
        self.pixels.extend(data[3..].iter().map(|&p| p.min(AMP_MAX)));
        self.expected_next += 1;
        if seq == self.count {
            return Ok(self.finish());
        }
        Ok(None)
    }

    fn finish(&mut self) -> Option<Sweep> {
        let (mode, low_hz, high_hz, out_of_range) = self.header.take()?;
        self.stats.sweeps += 1;
        self.expected_next = 0;
        Some(Sweep { mode, low_hz, high_hz, out_of_range, pixels: std::mem::take(&mut self.pixels) })
    }
}

/// Build the 27 00 data for one sweep (used by the simulator and tests).
pub fn encode_sweep(sweep: &Sweep, divisions: u8, chunk: usize) -> Vec<Vec<u8>> {
    let mut out = Vec::new();
    let mode = match sweep.mode {
        ScopeMode::Center => 0,
        ScopeMode::Fixed => 1,
        ScopeMode::ScrollCenter => 2,
        ScopeMode::ScrollFixed => 3,
    };
    let mut first = vec![0x00, bcd::byte(1), bcd::byte(divisions), mode];
    match sweep.mode {
        ScopeMode::Center | ScopeMode::ScrollCenter => {
            let centre = (sweep.low_hz + sweep.high_hz) / 2;
            let span = (sweep.high_hz - sweep.low_hz) / 2;
            first.extend(bcd::encode_le(centre, 5));
            first.extend(bcd::encode_le(span, 5));
        }
        _ => {
            first.extend(bcd::encode_le(sweep.low_hz, 5));
            first.extend(bcd::encode_le(sweep.high_hz, 5));
        }
    }
    first.push(u8::from(sweep.out_of_range));
    out.push(first);
    if sweep.out_of_range {
        return out;
    }
    let mut chunks = sweep.pixels.chunks(chunk);
    for seq in 2..=divisions {
        let mut d = vec![0x00, bcd::byte(seq), bcd::byte(divisions)];
        if let Some(c) = chunks.next() {
            d.extend_from_slice(c);
        }
        out.push(d);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample(n: usize) -> Sweep {
        Sweep {
            mode: ScopeMode::Center,
            low_hz: 14_049_000,
            high_hz: 14_099_000,
            out_of_range: false,
            pixels: (0..n).map(|i| (i % 161) as u8).collect(),
        }
    }

    #[test]
    fn reassembles_475_and_425() {
        for n in [475usize, 425] {
            let s = sample(n);
            let mut a = SweepAssembler::new();
            let mut got = None;
            for d in encode_sweep(&s, 11, 50) {
                if let Some(sw) = a.push(&d).unwrap() {
                    got = Some(sw);
                }
            }
            assert_eq!(got.unwrap(), s);
            assert_eq!(a.stats, ScopeStats { sweeps: 1, dropped: 0, out_of_range: 0 });
        }
    }

    #[test]
    fn missing_division_is_dropped_and_next_sweep_recovers() {
        let s = sample(475);
        let parts = encode_sweep(&s, 11, 50);
        let mut a = SweepAssembler::new();
        for (i, d) in parts.iter().enumerate() {
            if i == 5 {
                continue;
            }
            assert_eq!(a.push(d).unwrap(), None);
        }
        let mut got = None;
        for d in &parts {
            if let Some(sw) = a.push(d).unwrap() {
                got = Some(sw);
            }
        }
        assert_eq!(got.unwrap(), s);
        assert_eq!(a.stats.dropped, 1);
        assert_eq!(a.stats.sweeps, 1);
    }

    #[test]
    fn joining_mid_sweep_is_not_a_drop() {
        let parts = encode_sweep(&sample(475), 11, 50);
        let mut a = SweepAssembler::new();
        for d in &parts[4..] {
            assert_eq!(a.push(d).unwrap(), None);
        }
        assert_eq!(a.stats.dropped, 0);
    }

    #[test]
    fn fixed_mode_edges_and_out_of_range() {
        let s = Sweep { mode: ScopeMode::Fixed, low_hz: 7_000_000, high_hz: 7_300_000, out_of_range: true, pixels: vec![] };
        let parts = encode_sweep(&s, 11, 50);
        assert_eq!(parts.len(), 1);
        let mut a = SweepAssembler::new();
        assert_eq!(a.push(&parts[0]).unwrap().unwrap(), s);
        assert_eq!(a.stats.out_of_range, 1);
    }

    #[test]
    fn division_numbers_are_bcd() {
        // Division 10 and 11 are sent as 0x10 and 0x11, not 0x0A and 0x0B.
        let parts = encode_sweep(&sample(475), 11, 50);
        assert_eq!(parts[9][1], 0x10);
        assert_eq!(parts[10][1], 0x11);
        assert_eq!(parts[10][2], 0x11);
    }

    #[test]
    fn clamps_pixels_and_rejects_garbage() {
        let mut a = SweepAssembler::new();
        assert_eq!(a.push(&[0x00]), Err(ScopeError::Short));
        assert_eq!(a.push(&[0x00, 0x1A, 0x11]), Err(ScopeError::Bcd));
        let mut first = encode_sweep(&sample(10), 2, 50);
        first[1][3] = 0xFF;
        a.push(&first[0]).unwrap();
        let sw = a.push(&first[1]).unwrap().unwrap();
        assert_eq!(sw.pixels[0], AMP_MAX);
    }
}
