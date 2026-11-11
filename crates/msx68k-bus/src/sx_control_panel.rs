//! Adds the msx68k settings app to the SX-Window control panel.
//!
//! `コントロール.X` opens `コントロール.LB`, lists its `'CODE'` resource IDs, and shows each
//! ID that also has a `'PAT2'` icon and a `'CPNM'` module name as an item. Clicking an item
//! runs its `'CODE'`, an ordinary `HU` or `OBJR` executable, as a task with `TSFock`. So the
//! settings app goes in as those three resources, as add-ons of the time did through the
//! installer. The library is changed on the disk being built, never shipped changed.

use crate::sx_resource::{Resource, ResourceLibrary};

/// Resource ID of the settings item, next to Sharp's items (`$F3A0`..`$F3AD`).
pub const SETTINGS_PANEL_ID: u16 = 0xF3B0;

/// Module name of the settings item.
const SETTINGS_MODULE_NAME: &[u8] = b"MSX68K.X\0\0";

/// Icon of the settings item, a mouse with a wheel: `.` is the button face, `#` black, and
/// `o` white.
const SETTINGS_ICON: [&str; 30] = [
    "..............................",
    "..............................",
    "..........##########..........",
    "........##....##....##........",
    ".......#......##......#.......",
    "......#......####......#......",
    "......#......#oo#......#......",
    ".....#.......####.......#.....",
    ".....#.......#oo#.......#.....",
    ".....#.......####.......#.....",
    ".....#.......#oo#.......#.....",
    ".....#.......####.......#.....",
    ".....#........##........#.....",
    ".....####################.....",
    ".....#..................#.....",
    ".....#..................#.....",
    ".....#..................#.....",
    ".....#.....##....##.....#.....",
    ".....#....#..#..#..#....#.....",
    ".....#....#..#..#..#....#.....",
    ".....#.....##....##.....#.....",
    ".....#..................#.....",
    "......#................#......",
    "......#................#......",
    ".......#..............#.......",
    "........##..........##........",
    "..........##########..........",
    "..............................",
    "..............................",
    "..............................",
];

/// Encodes an icon as a `'PAT2'` resource: its rectangle, then two planes of 32-bit rows.
/// As in Sharp's icons, the button face sets plane 0 only, black neither plane, and white
/// plane 1 only.
fn pat2(icon: &[&str; 30]) -> Vec<u8> {
    let mut data = vec![0, 0, 0, 0, 0, 30, 0, 30];
    for plane in 0..2 {
        for row in icon {
            let bits = row.bytes().enumerate().fold(0u32, |bits, (x, dot)| {
                let set = match dot {
                    b'.' => plane == 0,
                    b'o' => plane == 1,
                    _ => false,
                };
                bits | ((set as u32) << (31 - x))
            });
            data.extend_from_slice(&bits.to_be_bytes());
        }
    }
    data
}

/// Returns `library` (`コントロール.LB`) with the settings app `code` added as an item, or
/// `None` if the library cannot be parsed.
pub fn add_settings_panel(library: &[u8], code: &[u8]) -> Option<Vec<u8>> {
    let mut library = ResourceLibrary::parse(library)?;
    library.insert(b"CODE", Resource::new(SETTINGS_PANEL_ID, code.to_vec()));
    library.insert(b"PAT2", Resource::new(SETTINGS_PANEL_ID, pat2(&SETTINGS_ICON)));
    library.insert(
        b"CPNM",
        Resource::new(SETTINGS_PANEL_ID, SETTINGS_MODULE_NAME.to_vec()),
    );
    Some(library.to_bytes())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn icon_is_a_sharp_style_pat2() {
        let data = pat2(&SETTINGS_ICON);
        assert_eq!(data.len(), 0xF8, "same size as Sharp's 30x30 icons");
        // The top rows are background: plane 0 set over the 30 dots, plane 1 clear.
        assert_eq!(&data[8..12], &0xFFFF_FFFCu32.to_be_bytes());
        assert_eq!(&data[8 + 120..12 + 120], &[0; 4]);
    }

    #[test]
    fn adds_the_three_resources() {
        let mut empty = ResourceLibrary::new();
        empty.insert(b"CODE", Resource::new(0xF3AD, b"OBJR".to_vec()));
        let library = add_settings_panel(&empty.to_bytes(), b"HU..").unwrap();
        let library = ResourceLibrary::parse(&library).unwrap();
        assert_eq!(library.get(b"CODE", SETTINGS_PANEL_ID).unwrap().data, b"HU..");
        assert!(library.get(b"PAT2", SETTINGS_PANEL_ID).is_some());
        assert_eq!(library.get(b"CPNM", SETTINGS_PANEL_ID).unwrap().data, SETTINGS_MODULE_NAME);
        assert!(library.get(b"CODE", 0xF3AD).is_some(), "existing items stay");
    }
}
