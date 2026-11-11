//! msx68k settings, kept the SX-Window way: as a resource of msx68k's own resource library
//! `A:\SYS\MSX68K.LB`, which the settings app (`SXMODE.X`) writes with `TSResOpen`,
//! `TSRscAdd`, and `TSResClose`, and which the host reads at boot.

use crate::sx_resource::{Resource, ResourceLibrary};
use crate::sx_wheel::WheelConfig;

/// Resource type and ID of the settings record.
pub const SETTINGS_TYPE: [u8; 4] = *b"MX68";
pub const SETTINGS_ID: u16 = 0;
const SETTINGS_VERSION: u16 = 1;

/// The settings record: big-endian words of the version, screen width and height, screen
/// flags (bit 0 fullscreen), wheel flags (bit 0 reverse), arrow clicks per notch, and
/// trackpad pixels per notch.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Settings {
    pub screen: (u32, u32),
    pub fullscreen: bool,
    pub wheel: WheelConfig,
}

impl Settings {
    /// Decodes a settings record, or `None` if it is short or of an unknown version.
    pub fn decode(data: &[u8]) -> Option<Self> {
        let word = |i: usize| -> Option<u16> {
            Some(u16::from_be_bytes(data.get(2 * i..2 * i + 2)?.try_into().ok()?))
        };
        if word(0)? != SETTINGS_VERSION {
            return None;
        }
        Some(Self {
            screen: (word(1)? as u32, word(2)? as u32),
            fullscreen: word(3)? & 1 != 0,
            wheel: WheelConfig {
                reverse: word(4)? & 1 != 0,
                clicks_per_notch: word(5)?.min(u8::MAX as u16) as u8,
                trackpad_pixels: word(6)?,
            }
            .clamped(),
        })
    }

    pub fn encode(&self) -> Vec<u8> {
        let words = [
            SETTINGS_VERSION,
            self.screen.0 as u16,
            self.screen.1 as u16,
            self.fullscreen as u16,
            self.wheel.reverse as u16,
            self.wheel.clicks_per_notch as u16,
            self.wheel.trackpad_pixels,
        ];
        words.iter().flat_map(|w| w.to_be_bytes()).collect()
    }

    /// Reads the settings from the bytes of `MSX68K.LB`.
    pub fn from_library(library: &[u8]) -> Option<Self> {
        let library = ResourceLibrary::parse(library)?;
        Self::decode(&library.get(&SETTINGS_TYPE, SETTINGS_ID)?.data)
    }

    /// Returns `MSX68K.LB` holding these settings.
    pub fn to_library(&self) -> Vec<u8> {
        let mut library = ResourceLibrary::new();
        library.insert(&SETTINGS_TYPE, Resource::new(SETTINGS_ID, self.encode()));
        library.to_bytes()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn settings_round_trip_through_the_library() {
        let settings = Settings {
            screen: (1920, 1080),
            fullscreen: false,
            wheel: WheelConfig {
                reverse: true,
                clicks_per_notch: 3,
                trackpad_pixels: 20,
            },
        };
        assert_eq!(settings.encode().len(), 14, "the record SXMODE.X writes");
        assert_eq!(Settings::from_library(&settings.to_library()), Some(settings));
    }

    #[test]
    fn unknown_versions_are_ignored() {
        let mut record = Settings {
            screen: (768, 512),
            fullscreen: true,
            wheel: WheelConfig::default(),
        }
        .encode();
        record[1] = 2;
        assert_eq!(Settings::decode(&record), None);
        assert_eq!(Settings::decode(&record[..6]), None);
    }
}
