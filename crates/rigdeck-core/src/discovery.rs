//! Find radios on USB serial ports.
//!
//! - Icom: the IC-7300's USB bridge reports a serial number starting with `IC-` (for example
//!   `IC-7300 01234567`, Silicon Labs CP2102).
//! - Yaesu FT-991A: a Silicon Labs CP2105 dual UART. The CAT manual says CAT runs over its
//!   "Enhanced" COM port; which macOS device that is depends on the driver, so the lower-numbered
//!   interface is offered first and both are listed (guess until tested with the radio).

use crate::rig::RigKind;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PortInfo {
    pub path: String,
    pub description: String,
    pub serial_number: Option<String>,
    pub guess: Option<RigKind>,
}

pub fn classify(serial: Option<&str>, product: Option<&str>) -> Option<RigKind> {
    if let Some(s) = serial
        && s.starts_with("IC-")
    {
        return Some(RigKind::IcomIc7300);
    }
    if let Some(p) = product
        && p.contains("CP2105")
    {
        return Some(RigKind::YaesuFt991a);
    }
    None
}

/// All `/dev/cu.*` serial ports, radios first.
pub fn list_ports() -> Vec<PortInfo> {
    let mut out: Vec<(PortInfo, u8)> = serialport::available_ports()
        .unwrap_or_default()
        .into_iter()
        .filter(|p| !p.port_name.starts_with("/dev/tty."))
        .map(|p| match p.port_type {
            serialport::SerialPortType::UsbPort(u) => {
                let guess = classify(u.serial_number.as_deref(), u.product.as_deref());
                let desc =
                    format!("{} {}", u.manufacturer.clone().unwrap_or_default(), u.product.clone().unwrap_or_default()).trim().to_string();
                let iface = u.interface.unwrap_or(0);
                (PortInfo { path: p.port_name, description: desc, serial_number: u.serial_number, guess }, iface)
            }
            _ => (PortInfo { path: p.port_name, description: String::new(), serial_number: None, guess: None }, 0),
        })
        .collect();
    out.sort_by(|a, b| (a.0.guess.is_none(), a.1, &a.0.path).cmp(&(b.0.guess.is_none(), b.1, &b.0.path)));
    out.into_iter().map(|(p, _)| p).collect()
}

/// The first port that looks like the given radio.
pub fn find(kind: RigKind) -> Option<PortInfo> {
    list_ports().into_iter().find(|p| p.guess == Some(kind))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classifies_known_bridges() {
        assert_eq!(classify(Some("IC-7300 01234567"), Some("CP2102 USB to UART Bridge Controller")), Some(RigKind::IcomIc7300));
        assert_eq!(classify(Some("00A50791"), Some("CP2105 Dual USB to UART Bridge Controller")), Some(RigKind::YaesuFt991a));
        assert_eq!(classify(Some("123"), Some("FT232R")), None);
    }

    #[test]
    fn listing_does_not_panic() {
        let _ = list_ports();
    }
}
