//! Detection and repair of historical X68000 boot/IPL viruses (ForceBoot, Hodson, Scheck, Surgeon).

use std::cell::RefCell;
use crate::floppy_image::{FloppyImage, ImageStorage, Probe};

/// Recognized X68000 virus types.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VirusType {
    /// DBF-1: ForceBoot (NX68K IPL V1.02, V1.00, V1.05)
    ForceBoot,
    /// DBH-4: Hodson
    Hodson,
    /// Scheck (stealth SRAM-infecting virus)
    Scheck,
    /// Surgeon (Carrot Party joke virus)
    Surgeon,
    /// Unknown virus detected via heuristic SRAM protection-unlock inspection
    Unknown,
}

impl VirusType {
    /// Human-readable English name.
    pub fn name_en(&self) -> &'static str {
        match self {
            VirusType::ForceBoot => "ForceBoot (NX68K IPL)",
            VirusType::Hodson => "Hodson (DBH-4)",
            VirusType::Scheck => "Scheck",
            VirusType::Surgeon => "Surgeon",
            VirusType::Unknown => "Unknown virus (SRAM unlock code detected)",
        }
    }

    /// Human-readable Japanese name.
    pub fn name_ja(&self) -> &'static str {
        match self {
            VirusType::ForceBoot => "ForceBoot (NX68K IPL)",
            VirusType::Hodson => "Hodson (DBH-4)",
            VirusType::Scheck => "Scheck",
            VirusType::Surgeon => "Surgeon",
            VirusType::Unknown => "未知のウイルス (SRAM保護解除コード検知)",
        }
    }

    /// Shift-JIS bytes of the Japanese virus name for display in SX-Window.
    pub fn name_ja_sjis(&self) -> &'static [u8] {
        match self {
            VirusType::ForceBoot => b"ForceBoot (NX68K IPL)",
            VirusType::Hodson => b"Hodson (DBH-4)",
            VirusType::Scheck => b"Scheck",
            VirusType::Surgeon => b"Surgeon",
            VirusType::Unknown => &[
                0x96, 0xA2, 0x92, 0x6D, 0x82, 0xCC, 0x83, 0x45, 0x83, 0x43, 0x83, 0x8B, 0x83, 0x58,
                0x20, 0x28, 0x53, 0x52, 0x41, 0x4D, 0x95, 0xDB, 0x8C, 0xEC, 0x89, 0xF0, 0x8F, 0x9C,
                0x8C, 0xDF, 0x92, 0x6D, 0x29,
            ],
        }
    }

    /// Whether this virus backs up the original IPL to Sector 4 and can be self-repaired
    /// without requiring an external clean disk image.
    pub fn can_self_repair_from_sector4(&self) -> bool {
        matches!(self, VirusType::ForceBoot)
    }
}

// SHA-256 hashes of known 1024-byte infected IPL sectors.
const FORCEBOOT_SHA256: [u8; 32] = [
    0x01, 0xd1, 0x53, 0x8f, 0x26, 0x2e, 0x73, 0x8b, 0x35, 0xe3, 0x93, 0x27, 0x78, 0xd0, 0x73, 0x24,
    0xa0, 0x10, 0x5e, 0x07, 0x19, 0x13, 0x04, 0x2d, 0xd2, 0x94, 0x26, 0xa9, 0x51, 0x7c, 0x38, 0xd5,
];

const HODSON_SHA256: [u8; 32] = [
    0xf6, 0x29, 0x5f, 0xd8, 0x98, 0xe6, 0x39, 0x98, 0xa6, 0xbf, 0x6d, 0x64, 0x22, 0xb4, 0xda, 0x65,
    0xe1, 0xb9, 0x76, 0xbe, 0xf6, 0xf8, 0x9e, 0x21, 0x38, 0xa2, 0xa2, 0xd8, 0x12, 0xf8, 0xb1, 0x63,
];

const SCHECK_SHA256: [u8; 32] = [
    0x9a, 0xcc, 0xfb, 0x67, 0x09, 0x75, 0xe5, 0x8f, 0xa5, 0x1e, 0xb5, 0xf8, 0x6c, 0x03, 0xb8, 0x6a,
    0x82, 0xde, 0xee, 0x89, 0x1b, 0x48, 0xd3, 0xcd, 0x51, 0x3d, 0xda, 0x1a, 0x2b, 0xc7, 0x3d, 0xf4,
];

/// Standalone FIPS 180-4 SHA-256 implementation.
pub fn sha256(data: &[u8]) -> [u8; 32] {
    const K: [u32; 64] = [
        0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4, 0xab1c5ed5,
        0xd807aa98, 0x12835b01, 0x243185be, 0x550c7dc3, 0x72be5d74, 0x80deb1fe, 0x9bdc06a7, 0xc19bf174,
        0xe49b69c1, 0xefbe4786, 0x0fc19dc6, 0x240ca1cc, 0x2de92c6f, 0x4a7484aa, 0x5cb0a9dc, 0x76f988da,
        0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7, 0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967,
        0x27b70a85, 0x2e1b2138, 0x4d2c6dfc, 0x53380d13, 0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85,
        0xa2bfe8a1, 0xa81a664b, 0xc24b8b70, 0xc76c51a3, 0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070,
        0x19a4c116, 0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a, 0x5b9cca4f, 0x682e6ff3,
        0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208, 0x90befffa, 0xa4506ceb, 0xbef9a3f7, 0xc67178f2,
    ];

    let mut h: [u32; 8] = [
        0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a,
        0x510e527f, 0x9b05688c, 0x1f83d9ab, 0x5be0cd19,
    ];

    let bit_len = (data.len() as u64) * 8;
    let mut padded = Vec::with_capacity(data.len() + 64);
    padded.extend_from_slice(data);
    padded.push(0x80);
    while (padded.len() % 64) != 56 {
        padded.push(0x00);
    }
    padded.extend_from_slice(&bit_len.to_be_bytes());

    for chunk in padded.chunks_exact(64) {
        let mut w = [0u32; 64];
        for i in 0..16 {
            w[i] = u32::from_be_bytes([
                chunk[i * 4],
                chunk[i * 4 + 1],
                chunk[i * 4 + 2],
                chunk[i * 4 + 3],
            ]);
        }
        for i in 16..64 {
            let s0 = w[i - 15].rotate_right(7) ^ w[i - 15].rotate_right(18) ^ (w[i - 15] >> 3);
            let s1 = w[i - 2].rotate_right(17) ^ w[i - 2].rotate_right(19) ^ (w[i - 2] >> 10);
            w[i] = w[i - 16].wrapping_add(s0).wrapping_add(w[i - 7]).wrapping_add(s1);
        }

        let mut a = h[0];
        let mut b = h[1];
        let mut c = h[2];
        let mut d = h[3];
        let mut e = h[4];
        let mut f = h[5];
        let mut g = h[6];
        let mut hh = h[7];

        for i in 0..64 {
            let s1 = e.rotate_right(6) ^ e.rotate_right(11) ^ e.rotate_right(25);
            let ch = (e & f) ^ ((!e) & g);
            let temp1 = hh.wrapping_add(s1).wrapping_add(ch).wrapping_add(K[i]).wrapping_add(w[i]);
            let s0 = a.rotate_right(2) ^ a.rotate_right(13) ^ a.rotate_right(22);
            let maj = (a & b) ^ (a & c) ^ (b & c);
            let temp2 = s0.wrapping_add(maj);

            hh = g;
            g = f;
            f = e;
            e = d.wrapping_add(temp1);
            d = c;
            c = b;
            b = a;
            a = temp1.wrapping_add(temp2);
        }

        h[0] = h[0].wrapping_add(a);
        h[1] = h[1].wrapping_add(b);
        h[2] = h[2].wrapping_add(c);
        h[3] = h[3].wrapping_add(d);
        h[4] = h[4].wrapping_add(e);
        h[5] = h[5].wrapping_add(f);
        h[6] = h[6].wrapping_add(g);
        h[7] = h[7].wrapping_add(hh);
    }

    let mut result = [0u8; 32];
    for (i, val) in h.iter().enumerate() {
        result[i * 4..i * 4 + 4].copy_from_slice(&val.to_be_bytes());
    }
    result
}

fn contains_subslice(haystack: &[u8], needle: &[u8]) -> bool {
    haystack.windows(needle.len()).any(|w| w == needle)
}

/// Detects whether an IPL sector (at least 1024 bytes) is infected by an X68000 virus.
pub fn detect_ipl_virus(ipl: &[u8]) -> Option<VirusType> {
    if ipl.len() < 1024 {
        return None;
    }
    let ipl1024 = &ipl[..1024];

    // 1. Exact SHA-256 hash match against known virus variants
    let hash = sha256(ipl1024);
    if hash == FORCEBOOT_SHA256 {
        return Some(VirusType::ForceBoot);
    }
    if hash == HODSON_SHA256 {
        return Some(VirusType::Hodson);
    }
    if hash == SCHECK_SHA256 {
        return Some(VirusType::Scheck);
    }

    // 2. Specific string signatures
    if contains_subslice(ipl1024, b"NX68K IPL") {
        return Some(VirusType::ForceBoot);
    }
    if contains_subslice(ipl1024, b"Hodson soft")
        || (contains_subslice(ipl1024, b"Hodson") && contains_subslice(ipl1024, b"90/04/20"))
    {
        return Some(VirusType::Hodson);
    }
    if contains_subslice(ipl1024, b"Surgeon") {
        return Some(VirusType::Surgeon);
    }

    // 3. Scheck structural signature:
    // References SRAM target address $ED3C00 and SRAM boot pointer $ED3F60
    if contains_subslice(ipl1024, &[0x00, 0xED, 0x3C, 0x00])
        && contains_subslice(ipl1024, &[0x00, 0xED, 0x3F, 0x60])
    {
        return Some(VirusType::Scheck);
    }

    // 4. Heuristic: Check for SRAM write-protection unlock address ($00E8E00D or $00E8000D).
    // Normal boot sectors/IPLs only load operating system files and never touch SRAM write protect.
    if contains_subslice(ipl1024, &[0x00, 0xE8, 0xE0, 0x0D])
        || contains_subslice(ipl1024, &[0x00, 0xE8, 0x00, 0x0D])
    {
        // If it also references Scheck's SRAM address $ED3C00, classify as Scheck
        if contains_subslice(ipl1024, &[0x00, 0xED, 0x3C, 0x00]) {
            return Some(VirusType::Scheck);
        }
        return Some(VirusType::Unknown);
    }

    None
}

struct MemoryStorage(RefCell<Vec<u8>>);

impl ImageStorage for MemoryStorage {
    fn read_at(&self, offset: usize, len: usize) -> Option<Vec<u8>> {
        Some(
            self.0
                .borrow()
                .get(offset..offset.checked_add(len)?)?
                .to_vec(),
        )
    }

    fn write_at(&self, offset: usize, data: Vec<u8>) -> bool {
        let mut bytes = self.0.borrow_mut();
        if offset + data.len() > bytes.len() {
            bytes.resize(offset + data.len(), 0);
        }
        bytes[offset..offset + data.len()].copy_from_slice(&data);
        true
    }
}

/// Reads the 1024-byte IPL (Track 0, Head 0, Sector 1) from a floppy disk image (RAW or DIM).
pub fn read_floppy_ipl(image_bytes: &[u8]) -> Option<Vec<u8>> {
    let Probe::Image(image) = FloppyImage::probe(image_bytes.len(), image_bytes) else {
        return None;
    };
    let storage = MemoryStorage(RefCell::new(image_bytes.to_vec()));
    image.read(&storage, 0, 1024)
}

/// Scans a floppy disk image file's IPL for viruses.
pub fn scan_floppy_image(image_bytes: &[u8]) -> Option<VirusType> {
    let ipl = read_floppy_ipl(image_bytes)?;
    detect_ipl_virus(&ipl)
}

/// Repairs a floppy image by overwriting its IPL (Sector 1) with clean IPL bytes.
pub fn repair_floppy_from_clean_ipl(image_bytes: &mut Vec<u8>, clean_ipl: &[u8]) -> bool {
    if clean_ipl.len() < 1024 {
        return false;
    }
    let Probe::Image(mut image) = FloppyImage::probe(image_bytes.len(), image_bytes) else {
        return false;
    };
    let storage = MemoryStorage(RefCell::new(std::mem::take(image_bytes)));
    let ok = image.write(&storage, 0, clean_ipl[..1024].to_vec());
    *image_bytes = storage.0.into_inner();
    ok
}

/// Repairs a ForceBoot-infected floppy image by restoring the backed-up original IPL bytes
/// from Sector 4 (first 640 bytes) back into Sector 1.
/// Also restores Sector 4 (FAT #2) by copying Sector 2 (FAT #1) if it is a standard FAT disk.
pub fn repair_floppy_forceboot_from_sector4(image_bytes: &mut Vec<u8>) -> bool {
    let Probe::Image(mut image) = FloppyImage::probe(image_bytes.len(), image_bytes) else {
        return false;
    };
    let storage = MemoryStorage(RefCell::new(std::mem::take(image_bytes)));

    // Read Sector 4 (offset 3072 = 3 * 1024 bytes)
    let Some(sector4) = image.read(&storage, 3072, 1024) else {
        *image_bytes = storage.0.into_inner();
        return false;
    };

    // Restore the backed-up 640 bytes ($280) from Sector 4 to Sector 1
    let original_ipl_part = sector4[..640].to_vec();
    if !image.write(&storage, 0, original_ipl_part) {
        *image_bytes = storage.0.into_inner();
        return false;
    }

    // If Sector 2 has a valid FAT12 media descriptor (e.g. 0xFE, 0xFF, 0xFF),
    // heal Sector 4 (FAT #2) by copying Sector 2 (FAT #1).
    if let Some(sector2) = image.read(&storage, 1024, 1024) {
        if sector2.len() >= 3 && sector2[1] == 0xFF && sector2[2] == 0xFF {
            let _ = image.write(&storage, 3072, sector2);
        }
    }

    *image_bytes = storage.0.into_inner();
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sha256_empty_string() {
        let hash = sha256(b"");
        let hex = hash.iter().map(|b| format!("{:02x}", b)).collect::<String>();
        assert_eq!(hex, "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855");
    }

    #[test]
    fn detect_known_viruses_via_hash_or_signature() {
        // Clean IPL
        let mut clean = [0u8; 1024];
        clean[2..10].copy_from_slice(b"Hudson s");
        assert_eq!(detect_ipl_virus(&clean), None);

        // ForceBoot signature
        let mut forceboot = [0u8; 1024];
        forceboot[100..109].copy_from_slice(b"NX68K IPL");
        assert_eq!(detect_ipl_virus(&forceboot), Some(VirusType::ForceBoot));

        // Hodson signature
        let mut hodson = [0u8; 1024];
        hodson[2..13].copy_from_slice(b"Hodson soft");
        assert_eq!(detect_ipl_virus(&hodson), Some(VirusType::Hodson));

        // Surgeon signature
        let mut surgeon = [0u8; 1024];
        surgeon[200..207].copy_from_slice(b"Surgeon");
        assert_eq!(detect_ipl_virus(&surgeon), Some(VirusType::Surgeon));

        // Scheck signature (SRAM references)
        let mut scheck = [0u8; 1024];
        scheck[50..54].copy_from_slice(&[0x00, 0xED, 0x3C, 0x00]);
        scheck[60..64].copy_from_slice(&[0x00, 0xED, 0x3F, 0x60]);
        assert_eq!(detect_ipl_virus(&scheck), Some(VirusType::Scheck));

        // Unknown heuristic (SRAM unlock only)
        let mut unknown = [0u8; 1024];
        unknown[30..34].copy_from_slice(&[0x00, 0xE8, 0xE0, 0x0D]);
        assert_eq!(detect_ipl_virus(&unknown), Some(VirusType::Unknown));
    }

    #[test]
    fn repair_from_clean_ipl_synthetic() {
        let mut infected_disk = vec![0u8; 1_261_568];
        // Inject Scheck signature in Sector 1
        infected_disk[50..54].copy_from_slice(&[0x00, 0xED, 0x3C, 0x00]);
        infected_disk[60..64].copy_from_slice(&[0x00, 0xED, 0x3F, 0x60]);
        assert_eq!(scan_floppy_image(&infected_disk), Some(VirusType::Scheck));

        let mut clean_disk = vec![0u8; 1_261_568];
        clean_disk[0..15].copy_from_slice(b"CLEAN_BOOT_SECT");

        let clean_ipl = read_floppy_ipl(&clean_disk).unwrap();
        assert!(repair_floppy_from_clean_ipl(&mut infected_disk, &clean_ipl));

        assert_eq!(scan_floppy_image(&infected_disk), None);
        assert_eq!(&infected_disk[0..15], b"CLEAN_BOOT_SECT");
    }

    #[test]
    fn repair_forceboot_from_sector4_synthetic() {
        let mut disk = vec![0u8; 1_261_568];
        // Inject ForceBoot signature in Sector 1
        disk[100..109].copy_from_slice(b"NX68K IPL");
        assert_eq!(scan_floppy_image(&disk), Some(VirusType::ForceBoot));

        // In Sector 4 (offset 3072), place the original 640 bytes
        disk[3072..3072 + 16].copy_from_slice(b"ORIGINAL_SECTOR1");

        // In Sector 2 (offset 1024), put a FAT12 header
        disk[1024] = 0xFE;
        disk[1025] = 0xFF;
        disk[1026] = 0xFF;

        assert!(repair_floppy_forceboot_from_sector4(&mut disk));
        assert_eq!(scan_floppy_image(&disk), None);
        assert_eq!(&disk[0..16], b"ORIGINAL_SECTOR1");
        // Sector 4 should have been restored with FAT #2 from Sector 2
        assert_eq!(&disk[3072..3075], &[0xFE, 0xFF, 0xFF]);
    }
}
