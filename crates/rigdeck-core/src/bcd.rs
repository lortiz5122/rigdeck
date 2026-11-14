//! Binary-coded decimal helpers for Icom CI-V.
//!
//! Source: Icom IC-7300 Full Manual (IC-7300_ENG_FM_12b.pdf), section 19, "Data content
//! description": frequencies are five bytes, two decimal digits per byte, lowest digits first
//! ("Operating frequency", p. 19-9). Levels are two bytes, most significant digits first
//! ("00 00 to 02 55", command table p. 19-3).

/// Error for byte strings that are not valid BCD.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BcdError(pub String);

impl std::fmt::Display for BcdError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "invalid BCD: {}", self.0)
    }
}
impl std::error::Error for BcdError {}

fn digit_pair(b: u8) -> Result<(u8, u8), BcdError> {
    let hi = b >> 4;
    let lo = b & 0x0F;
    if hi > 9 || lo > 9 {
        return Err(BcdError(format!("byte {b:02X}")));
    }
    Ok((hi, lo))
}

/// Encode a value as `bytes` bytes of BCD, least significant pair first (Icom frequency order).
pub fn encode_le(mut value: u64, bytes: usize) -> Vec<u8> {
    let mut out = Vec::with_capacity(bytes);
    for _ in 0..bytes {
        let lo = (value % 10) as u8;
        value /= 10;
        let hi = (value % 10) as u8;
        value /= 10;
        out.push((hi << 4) | lo);
    }
    out
}

/// Decode little-endian (lowest digits first) BCD.
pub fn decode_le(data: &[u8]) -> Result<u64, BcdError> {
    let mut value: u64 = 0;
    for &b in data.iter().rev() {
        let (hi, lo) = digit_pair(b)?;
        value = value * 100 + u64::from(hi) * 10 + u64::from(lo);
    }
    Ok(value)
}

/// Encode a value as `bytes` bytes of BCD, most significant pair first (levels, channel numbers).
pub fn encode_be(value: u64, bytes: usize) -> Vec<u8> {
    let mut v = encode_le(value, bytes);
    v.reverse();
    v
}

/// Decode big-endian (highest digits first) BCD.
pub fn decode_be(data: &[u8]) -> Result<u64, BcdError> {
    let mut value: u64 = 0;
    for &b in data {
        let (hi, lo) = digit_pair(b)?;
        value = value * 100 + u64::from(hi) * 10 + u64::from(lo);
    }
    Ok(value)
}

/// Frequency in Hz to the five-byte CI-V form (10 digits, up to 9,999,999,999 Hz).
pub fn freq_to_civ(hz: u64) -> Vec<u8> {
    encode_le(hz, 5)
}

/// Five-byte CI-V frequency to Hz.
pub fn civ_to_freq(data: &[u8]) -> Result<u64, BcdError> {
    if data.len() != 5 {
        return Err(BcdError(format!("frequency needs 5 bytes, got {}", data.len())));
    }
    decode_le(data)
}

/// A single BCD byte (00–99), used for sequence numbers, hours, minutes and so on.
pub fn byte(value: u8) -> u8 {
    ((value / 10) << 4) | (value % 10)
}

/// Decode a single BCD byte.
pub fn from_byte(b: u8) -> Result<u8, BcdError> {
    let (hi, lo) = digit_pair(b)?;
    Ok(hi * 10 + lo)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn manual_example_frequency() {
        // 14.074 MHz is stored as 00 40 07 14 00 (lowest digits first).
        assert_eq!(freq_to_civ(14_074_000), vec![0x00, 0x40, 0x07, 0x14, 0x00]);
        assert_eq!(civ_to_freq(&[0x00, 0x40, 0x07, 0x14, 0x00]).unwrap(), 14_074_000);
    }

    #[test]
    fn edges() {
        for hz in [0u64, 1, 9, 10, 99, 100, 30_000, 1_800_000, 74_800_000, 9_999_999_999] {
            assert_eq!(civ_to_freq(&freq_to_civ(hz)).unwrap(), hz, "{hz}");
        }
        // Value wider than the field is truncated to its low digits, not wrapped into hex.
        assert_eq!(decode_le(&encode_le(12_345, 2)).unwrap(), 2_345);
    }

    #[test]
    fn levels_big_endian() {
        assert_eq!(encode_be(255, 2), vec![0x02, 0x55]);
        assert_eq!(encode_be(128, 2), vec![0x01, 0x28]);
        assert_eq!(decode_be(&[0x01, 0x20]).unwrap(), 120);
        assert_eq!(decode_be(&[0x00, 0x00]).unwrap(), 0);
    }

    #[test]
    fn rejects_hex_digits() {
        assert!(decode_le(&[0x0A]).is_err());
        assert!(decode_be(&[0xF0]).is_err());
        assert!(civ_to_freq(&[0, 0, 0, 0]).is_err());
        assert!(from_byte(0x1F).is_err());
    }

    #[test]
    fn single_bytes() {
        assert_eq!(byte(11), 0x11);
        assert_eq!(from_byte(0x11).unwrap(), 11);
        for v in 0..=99u8 {
            assert_eq!(from_byte(byte(v)).unwrap(), v);
        }
    }
}
