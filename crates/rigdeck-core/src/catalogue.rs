//! Command catalogues kept as data (`catalogue/*.json`), written from the radio makers' manuals.
//! Drivers look up command bytes here by name, so a wrong byte is fixed in one place.

use serde::Deserialize;
use std::sync::OnceLock;

#[derive(Debug, Deserialize)]
pub struct CivCatalogue {
    pub radio: String,
    pub source: String,
    pub default_radio_address: String,
    pub default_controller_address: String,
    pub commands: Vec<CivCommand>,
}

#[derive(Debug, Deserialize)]
pub struct CivCommand {
    pub name: String,
    pub bytes: String,
    pub data: String,
    pub access: String,
    pub page: String,
    #[serde(default)]
    pub note: String,
}

#[derive(Debug, Deserialize)]
pub struct CatCatalogue {
    pub radio: String,
    pub source: String,
    pub commands: Vec<CatCommand>,
}

#[derive(Debug, Deserialize)]
pub struct CatCommand {
    pub name: String,
    pub code: String,
    pub params: String,
    pub access: String,
    pub page: String,
}

pub fn parse_hex(s: &str) -> Vec<u8> {
    s.split_whitespace().map(|h| u8::from_str_radix(h, 16).unwrap_or_else(|_| panic!("bad hex {h:?} in catalogue"))).collect()
}

impl CivCatalogue {
    /// Command bytes for a catalogue entry. Panics on an unknown name: that is a programming error
    /// caught by the catalogue tests.
    pub fn bytes(&self, name: &str) -> Vec<u8> {
        let c = self
            .commands
            .iter()
            .find(|c| c.name == name)
            .unwrap_or_else(|| panic!("command {name:?} missing from {} catalogue", self.radio));
        parse_hex(&c.bytes)
    }
    pub fn radio_address(&self) -> u8 {
        parse_hex(&self.default_radio_address)[0]
    }
    pub fn controller_address(&self) -> u8 {
        parse_hex(&self.default_controller_address)[0]
    }
}

impl CatCatalogue {
    pub fn code(&self, name: &str) -> &str {
        &self
            .commands
            .iter()
            .find(|c| c.name == name)
            .unwrap_or_else(|| panic!("command {name:?} missing from {} catalogue", self.radio))
            .code
    }
}

pub fn ic7300() -> &'static CivCatalogue {
    static CAT: OnceLock<CivCatalogue> = OnceLock::new();
    CAT.get_or_init(|| serde_json::from_str(include_str!("../catalogue/ic7300.json")).expect("ic7300.json is valid"))
}

pub fn ft991a() -> &'static CatCatalogue {
    static CAT: OnceLock<CatCatalogue> = OnceLock::new();
    CAT.get_or_init(|| serde_json::from_str(include_str!("../catalogue/ft991a.json")).expect("ft991a.json is valid"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn catalogues_load_and_are_unique() {
        let c = ic7300();
        assert_eq!(c.radio_address(), 0x94);
        assert_eq!(c.controller_address(), 0xE0);
        let mut names: Vec<_> = c.commands.iter().map(|c| &c.name).collect();
        names.sort();
        names.dedup();
        assert_eq!(names.len(), c.commands.len());
        for cmd in &c.commands {
            assert!(!parse_hex(&cmd.bytes).is_empty());
            assert!(!cmd.page.is_empty(), "{} has no source page", cmd.name);
        }
        let y = ft991a();
        for cmd in &y.commands {
            assert_eq!(cmd.code.len(), 2);
            assert!(cmd.code.chars().all(|c| c.is_ascii_uppercase()));
            assert!(!cmd.page.is_empty());
        }
    }

    #[test]
    fn key_bytes_match_manual() {
        let c = ic7300();
        assert_eq!(c.bytes("read_frequency"), vec![0x03]);
        assert_eq!(c.bytes("s_meter"), vec![0x15, 0x02]);
        assert_eq!(c.bytes("memory_contents"), vec![0x1A, 0x00]);
        assert_eq!(c.bytes("scope_wave_data"), vec![0x27, 0x00]);
        assert_eq!(ft991a().code("frequency_vfo_a"), "FA");
    }
}
