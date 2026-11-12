//! `VMX68K.LB`, VMX68K.SX's resource library, kept next to `VMX68K.X` in `\X68`
//! (`docs/VMX68K.md` Section 2, "Settings menus"): the icons of its icon bar and the submenus
//! of their menus, which SX-Window's Menu Man takes only as `'MENU'` resources of an open
//! library. VMX68K.X opens it at start. Also the entry of `\SHELL\ICON.LB` that opens VM
//! setups (`*.V68`) with VMX68K.X.

use crate::sx_resource::{Resource, ResourceLibrary};

/// `'MENU'` of the CPU's speeds, in the order of `VmSpeed`'s `D2.L`. VMX68K.X rebuilds it with
/// `MNConvert2` before showing it, to check the current speed.
pub const SPEED_MENU_ID: u16 = 1;

/// `'PLOT'` of the Configuration icon, a plot image ([`plot_image`]).
pub const CONFIGURATION_ICON_ID: u16 = 1;

/// ID of the `'ICN#'` entry and its `'PAT3'` icon that `\SHELL\ICON.LB` gets for VM setups,
/// after Sharp's entries (`$80`..`$BB`).
pub const SETUP_ICON_ID: u16 = 0xC0;

/// The file icon of a VM setup (`*.V68`), a plate as the Filer's file icons are, white at its
/// top and left with a black shadow at its bottom and right, holding an X68000 (see
/// [`plot_image`], whose layout `'PAT3'` icons share).
const SETUP_ICON: [&str; 34] = [
    "oooooooooooooooooooooooooooooooooooooooooooooo  ",
    "o.............................................##",
    "o................##############...............##",
    "o................#............#...............##",
    "o.............#########..#########............##",
    "o.............#ooooooo#..#ooooooo#............##",
    "o.............#+++++++#..#+++++++#............##",
    "o.............#+++++++#..#+++++++#............##",
    "o.............#+++++++#..#+++++++#............##",
    "o.............#+#####+#..#+#####+#............##",
    "o.............#+ooooo+#..#+ooooo+#............##",
    "o.............#+++++++#..#+++++++#............##",
    "o.............#+++++++#..#+++++++#............##",
    "o.............#+#####+#..#+#####+#............##",
    "o.............#+ooooo+#..#+ooooo+#............##",
    "o.............#+++++++#..#+++++++#............##",
    "o.............#+++++++#..#+++++++#............##",
    "o.............#+++++++#..#+++++++#............##",
    "o.............#+#####+#..#+#####+#............##",
    "o.............#+++++++#..#+++++++#............##",
    "o.............#+#####+#..#+#####+#............##",
    "o.............#+++++++#..#+++++++#............##",
    "o.............#+#####+#..#+#####+#............##",
    "o.............#+++++++#..#+++++++#............##",
    "o.............#+#####+#..#+#####+#............##",
    "o.............#+++++++#..#+++++++#............##",
    "o.............#+++++++#..#+++++++#............##",
    "o.............#########..#########............##",
    "o.............#####################...........##",
    "o.............................................##",
    "o.............................................##",
    "o.............................................##",
    " ###############################################",
    "  ##############################################",
];

/// Items of the speed menu, in Shift_JIS as SX-Window shows them: Burst is 最高速.
const SPEEDS: [&[u8]; 4] = [b"10MHz", b"16MHz", b"24MHz", b"\x8D\xC5\x8D\x82\x91\xAC"];

/// The Configuration icon (see [`plot_image`]), in the style of SX-Window's icons: a picture
/// in a rectangular recess, 3:2 as the Filer's and the desktop panel's icons are, black at its
/// top and left and white at its bottom and right, over dark gray; here a white gear with its
/// shadow to the bottom right.
const CONFIGURATION_ICON: [&str; 20] = [
    "##############################",
    "#++++++++++++++++++++++++++++o",
    "#++++++++++++++++++++++++++++o",
    "#++++++++++++ooo+++++++++++++o",
    "#++++++++oo++ooo#+oo+++++++++o",
    "#++++++++ooooooooooo#++++++++o",
    "#+++++++++ooooooooo##++++++++o",
    "#+++++++++ooo###ooo#+++++++++o",
    "#+++++++oooo##+++oooo++++++++o",
    "#+++++++oooo#++++oooo#+++++++o",
    "#+++++++oooo#++++oooo#+++++++o",
    "#++++++++#ooo+++ooo###+++++++o",
    "#+++++++++ooooooooo#+++++++++o",
    "#++++++++ooooooooooo+++++++++o",
    "#++++++++oo##ooo##oo#++++++++o",
    "#+++++++++##+ooo#++##++++++++o",
    "#+++++++++++++###++++++++++++o",
    "#++++++++++++++++++++++++++++o",
    "#++++++++++++++++++++++++++++o",
    "#ooooooooooooooooooooooooooooo",
];

/// Encodes a picture as a plot image, which `GMPlotImg` draws with a plot mode of two pages:
/// its rectangle, then planes 0 and 1 and a mask, each of rows of whole words. The two planes
/// pick SX-Window's grays, `o` white (neither), `.` light gray (plane 0), `+` dark gray (plane
/// 1), and `#` black (both); a space is left out of the mask and lets what is under show.
fn plot_image(picture: &[&str]) -> Vec<u8> {
    let width = picture.first().map_or(0, |row| row.len());
    assert!(picture.iter().all(|row| row.len() == width), "rows are all as wide");
    let words = width.div_ceil(16);
    let mut data = Vec::new();
    for word in [0, 0, width as u16, picture.len() as u16] {
        data.extend_from_slice(&word.to_be_bytes());
    }
    let planes: [fn(u8) -> bool; 3] = [
        |dot| matches!(dot, b'.' | b'#'),
        |dot| matches!(dot, b'+' | b'#'),
        |dot| dot != b' ',
    ];
    for set in planes {
        for row in picture {
            let mut bits = vec![0u16; words];
            for (x, dot) in row.bytes().enumerate() {
                if set(dot) {
                    bits[x / 16] |= 0x8000 >> (x % 16);
                }
            }
            for word in bits {
                data.extend_from_slice(&word.to_be_bytes());
            }
        }
    }
    data
}

/// Encodes a plain menu as a `'MENU'` resource, laid out as Sharp's are: ID 0 (`MI_PLN`), no
/// size or definition yet, every item enabled, the item count, then each item's shortcut,
/// check, length, and title, padded to an even size.
fn menu(items: &[&[u8]]) -> Vec<u8> {
    let mut data = Vec::new();
    data.extend_from_slice(&[0; 6]); // ID, width, height
    data.extend_from_slice(&0u32.to_be_bytes()); // def
    data.extend_from_slice(&u32::MAX.to_be_bytes()); // enable
    data.extend_from_slice(&0u32.to_be_bytes()); // defData
    data.extend_from_slice(&(items.len() as u16).to_be_bytes());
    for title in items {
        data.extend_from_slice(&[0, 0, title.len() as u8]);
        data.extend_from_slice(title);
        if data.len() % 2 != 0 {
            data.push(0);
        }
    }
    data
}

/// An `'ICN#'` entry (`IcKind` of `TASK.H`), laid out as Sharp's in `\SHELL\ICON.LB`: its icon
/// `'PAT3'` `id`, the attributes and right-press menus of Sharp's file entries, then the file
/// name `pattern`, the `program` that opens it, and its `parameters` (`%` for the file), as
/// Pascal strings.
fn icon_entry(id: u16, pattern: &[u8], program: &[u8], parameters: &[u8]) -> Vec<u8> {
    let mut data = b"PAT3".to_vec();
    for word in [id, 0x0007, 0x0000, 0x0009, 0x000A] {
        data.extend_from_slice(&word.to_be_bytes());
    }
    for text in [pattern, program, parameters] {
        data.push(text.len() as u8);
        data.extend_from_slice(text);
    }
    data
}

/// Returns `library` (`\SHELL\ICON.LB`) with the entry that opens VM setups (`*.v68`) with
/// VMX68K.X and their file icon, as Sharp's installer registers applications, or `None` if it
/// cannot be parsed. The Filer reads it when SX-Window starts.
pub fn add_setup_icon(library: &[u8]) -> Option<Vec<u8>> {
    let mut library = ResourceLibrary::parse(library)?;
    library.insert(
        b"ICN#",
        Resource::new(SETUP_ICON_ID, icon_entry(SETUP_ICON_ID, b"*.v68", b"vmx68k.x", b"%")),
    );
    library.insert(b"PAT3", Resource::new(SETUP_ICON_ID, plot_image(&SETUP_ICON)));
    Some(library.to_bytes())
}

/// Builds a VM setup (`*.V68`) as VMX68K.X writes it: `'V68S'` 0, its version (1) and the
/// speed (as `VmSpeed`'s), and `'SRAM'` 0, the VM's 16 KB of SRAM once it has one.
pub fn setup_library(speed: u16, sram: Option<&[u8]>) -> Vec<u8> {
    let mut library = ResourceLibrary::new();
    let mut settings = 1u16.to_be_bytes().to_vec();
    settings.extend_from_slice(&speed.to_be_bytes());
    library.insert(b"V68S", Resource::new(0, settings));
    if let Some(sram) = sram {
        library.insert(b"SRAM", Resource::new(0, sram.to_vec()));
    }
    library.to_bytes()
}

/// Builds `VMX68K.LB`.
pub fn vmx68k_library() -> Vec<u8> {
    let mut library = ResourceLibrary::new();
    library.insert(b"MENU", Resource::new(SPEED_MENU_ID, menu(&SPEEDS)));
    library.insert(
        b"PLOT",
        Resource::new(CONFIGURATION_ICON_ID, plot_image(&CONFIGURATION_ICON)),
    );
    library.to_bytes()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn menus_are_laid_out_as_sharps() {
        // Sharp's 'MENU' $F000 starts so, with カット (X) as its first item.
        let data = menu(&[b"10MHz", b"Go", b"Burst"]);
        assert_eq!(&data[..20], &[0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0xFF, 0xFF, 0xFF, 0xFF, 0, 0, 0, 0, 0, 3]);
        assert_eq!(&data[20..23], &[0, 0, 5]);
        assert_eq!(&data[23..28], b"10MHz");
        assert_eq!(&data[28..33], &[0, 0, 2, b'G', b'o']);
        assert_eq!(data[33], 0, "padded to an even size");
        assert_eq!(&data[34..37], &[0, 0, 5]);
    }

    #[test]
    fn the_library_holds_the_menu_and_the_icon() {
        let library = ResourceLibrary::parse(&vmx68k_library()).unwrap();
        let speeds = library.get(b"MENU", SPEED_MENU_ID).unwrap();
        assert_eq!(&speeds.data[18..20], &[0, 4]);
        let icon = library.get(b"PLOT", CONFIGURATION_ICON_ID).unwrap();
        assert_eq!(&icon.data[..8], &[0, 0, 0, 0, 0, 30, 0, 20]);
        assert_eq!(icon.data.len(), 8 + 3 * 20 * 4, "two planes and the mask");
    }

    #[test]
    fn plot_images_have_two_planes_and_a_mask() {
        let data = plot_image(&["o.+# "; 1]);
        assert_eq!(&data[..8], &[0, 0, 0, 0, 0, 5, 0, 1]);
        assert_eq!(&data[8..], &[0b0101_0000, 0, 0b0011_0000, 0, 0b1111_0000, 0]);
    }

    #[test]
    fn setups_open_with_vmx68k() {
        let mut icons = ResourceLibrary::new();
        icons.insert(b"ICN#", Resource::new(0x91, icon_entry(0x91, b"*.lb", b"", b"")));
        let icons = ResourceLibrary::parse(&add_setup_icon(&icons.to_bytes()).unwrap()).unwrap();
        let entry = &icons.get(b"ICN#", SETUP_ICON_ID).unwrap().data;
        assert_eq!(&entry[..6], b"PAT3\x00\xC0");
        assert_eq!(&entry[14..], b"\x05*.v68\x08vmx68k.x\x01%");
        let icon = &icons.get(b"PAT3", SETUP_ICON_ID).unwrap().data;
        assert_eq!(&icon[..8], &[0, 0, 0, 0, 0, 48, 0, 34]);
        assert!(icons.get(b"ICN#", 0x91).is_some(), "Sharp's entries stay");
    }

    #[test]
    fn entries_are_laid_out_as_sharps() {
        // Sharp's `*.bat` entry, $9A.
        let mut sharp = b"PAT3\x00\x9A\x00\x07\x00\x00\x00\x09\x00\x0A".to_vec();
        sharp.extend_from_slice(b"\x05*.bat\x09command.x\x01%");
        assert_eq!(icon_entry(0x9A, b"*.bat", b"command.x", b"%"), sharp);
    }
}
