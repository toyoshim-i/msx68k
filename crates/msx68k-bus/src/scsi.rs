//! Virtual SCSI Subsystem (`$FC0000` ROM + `IOCS $F5` `_SCSIDRV` + `romdb` block driver)
//! and Mixed Mode Hypercall Engine for X68000.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use msx68k_cpu::{CpuState, TrapResult};

use crate::bus::X68kBus;

/// Synthesizes the 1024-byte SCSI IPL sector (Record 1, offset 0x400).
pub fn synthesize_scsi_ipl_sector(partition_start_record: u32) -> [u8; 1024] {
    let mut sector = [0u8; 1024];
    // 0x00..0x02: BRA.S +0x06
    sector[0x00..0x02].copy_from_slice(&[0x60, 0x06]);
    // 0x02..0x08: b"X68IPL"
    sector[0x02..0x08].copy_from_slice(b"X68IPL");

    // Stub at 0x08: calls IOCS $F5 _S_READ ($21) to load partition boot sector at partition_start_record
    // into $002000 and jumps to $002000.
    let mut code = Vec::new();
    code.extend_from_slice(&[0x70, 0xF5]); // moveq #-11, %d0
    code.extend_from_slice(&[0x72, 0x21]); // moveq #0x21, %d1
    code.extend_from_slice(&[0x24, 0x3C]); // movel #imm, %d2
    code.extend_from_slice(&partition_start_record.to_be_bytes());
    code.extend_from_slice(&[0x76, 0x01]); // moveq #1, %d3
    code.extend_from_slice(&[0x78, 0x00]); // moveq #0, %d4
    code.extend_from_slice(&[0x7A, 0x02]); // moveq #2, %d5
    code.extend_from_slice(&[0x22, 0x7C, 0x00, 0x00, 0x20, 0x00]); // movel #0x2000, %a1
    code.extend_from_slice(&[0x4E, 0x4F]); // trap #15
    code.extend_from_slice(&[0x4E, 0xF9, 0x00, 0x00, 0x20, 0x00]); // jmp 0x2000

    sector[0x08..0x08 + code.len()].copy_from_slice(&code);
    sector
}

/// Synthesizes the 1024-byte Human68k partition boot sector.
pub fn synthesize_partition_boot_sector(
    partition_start_record: u32,
    partition_records: u32,
    fat_recs: u16,
    root_entries: u16,
) -> [u8; 1024] {
    let mut sector = [0u8; 1024];
    // 0x00..0x02: BRA.S +0x26
    sector[0x00..0x02].copy_from_slice(&[0x60, 0x24]);
    // 0x02..0x08: b"X68IPL"
    sector[0x02..0x08].copy_from_slice(b"X68IPL");

    // Big-Endian BPB at 0x12..0x26
    sector[0x12..0x14].copy_from_slice(&1024u16.to_be_bytes()); // Bytes per sector
    sector[0x14] = 1; // Sectors per cluster
    sector[0x15] = 2; // FAT count
    sector[0x16..0x18].copy_from_slice(&1u16.to_be_bytes()); // Reserved sectors
    sector[0x18..0x1A].copy_from_slice(&root_entries.to_be_bytes()); // Root entries
    sector[0x1A..0x1C].copy_from_slice(&0u16.to_be_bytes());
    sector[0x1C] = 0xF7; // Media descriptor
    sector[0x1D] = fat_recs as u8; // Sectors per FAT
    sector[0x1E..0x22].copy_from_slice(&partition_records.to_be_bytes()); // Total records
    sector[0x22..0x26].copy_from_slice(&partition_start_record.to_be_bytes()); // Start record

    // Little-Endian BPB at 0x0B..0x14 for DOS/FAT compatibility
    sector[0x0B..0x0D].copy_from_slice(&1024u16.to_le_bytes());
    sector[0x0D] = 1;
    sector[0x0E..0x10].copy_from_slice(&1u16.to_le_bytes());
    sector[0x10] = 2;
    sector[0x11..0x13].copy_from_slice(&root_entries.to_le_bytes());

    // Routine at 0x26:
    // Cluster 2 start LBA = partition_start_record + 1 (boot) + 2 * fat_recs + (root_entries * 32 / 1024)
    let root_dir_recs = (root_entries as u32 * 32) / 1024;
    let human_start_lba = partition_start_record + 1 + 2 * (fat_recs as u32) + root_dir_recs;

    let mut code = Vec::new();
    code.extend_from_slice(&[0x70, 0xF5]); // moveq #-11, %d0
    code.extend_from_slice(&[0x72, 0x21]); // moveq #0x21, %d1
    code.extend_from_slice(&[0x24, 0x3C]); // movel #imm, %d2
    code.extend_from_slice(&human_start_lba.to_be_bytes());
    code.extend_from_slice(&[0x76, 0x40]); // moveq #64, %d3 (64 sectors = 64KB)
    code.extend_from_slice(&[0x78, 0x00]); // moveq #0, %d4
    code.extend_from_slice(&[0x7A, 0x02]); // moveq #2, %d5
    code.extend_from_slice(&[0x22, 0x7C, 0x00, 0x00, 0x67, 0xC0]); // movel #0x67C0, %a1
    code.extend_from_slice(&[0x4E, 0x4F]); // trap #15
    code.extend_from_slice(&[0x4E, 0xF9, 0x00, 0x00, 0x68, 0x00]); // jmp 0x6800

    sector[0x26..0x26 + code.len()].copy_from_slice(&code);
    sector
}

pub use crate::sxwin::HypercallId;

/// Parsed X68000 SCSI partition table entry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScsiPartition {
    pub name: [u8; 8],
    pub start_record: u32,
    pub record_count: u32,
}

/// Parses the `"X68K"` partition table from a SCSI disk image at byte offset 2048 (`0x800`).
pub fn parse_scsi_partitions(disk_image: &[u8]) -> Option<Vec<ScsiPartition>> {
    if disk_image.len() < 0x800 + 0x20 {
        return None;
    }
    if &disk_image[0x800..0x804] != b"X68K" {
        return None;
    }
    let mut partitions = Vec::new();
    for i in 0..8 {
        let entry_off = 0x810 + i * 16;
        if entry_off + 16 > disk_image.len() {
            break;
        }
        let name_bytes: [u8; 8] = disk_image[entry_off..entry_off + 8].try_into().ok()?;
        if name_bytes[0] == 0 {
            continue;
        }
        let start = u32::from_be_bytes(disk_image[entry_off + 8..entry_off + 12].try_into().ok()?);
        let count = u32::from_be_bytes(disk_image[entry_off + 12..entry_off + 16].try_into().ok()?);
        partitions.push(ScsiPartition {
            name: name_bytes,
            start_record: start,
            record_count: count,
        });
    }
    Some(partitions)
}

/// Helper to detect partition start record, record count, and FAT BPB parameters from a SCSI disk image.
pub(crate) fn detect_partition_params(disk: &[u8]) -> (usize, u32, usize, u8, u16, u8) {
    if let Some(parts) = parse_scsi_partitions(disk) {
        if let Some(p) = parts.iter().find(|p| &p.name == b"Human68k") {
            let start_rec = p.start_record as usize;
            let rec_count = p.record_count;
            let mut sector_bytes = 1024usize;
            let mut sec_per_clus = 1u8;
            let mut root_entries = 1024u16;
            let mut fat_sectors = 64u8;

            let off_1024 = start_rec * 1024;
            let off_512 = start_rec * 512;
            let boot_off = if disk.len() >= off_1024 + 512
                && (&disk[off_1024..off_1024 + 8] == b"X68SCSI1" || disk[off_1024] == 0x60)
            {
                Some((off_1024, 1024usize))
            } else if disk.len() >= off_512 + 512
                && (&disk[off_512..off_512 + 8] == b"X68SCSI1" || disk[off_512] == 0x60)
            {
                Some((off_512, 512usize))
            } else {
                None
            };

            if let Some((bo, default_sb)) = boot_off {
                sector_bytes = default_sb;
                let be_sec = u16::from_be_bytes([disk[bo + 0x12], disk[bo + 0x13]]) as usize;
                let le_sec = u16::from_le_bytes([disk[bo + 0x0B], disk[bo + 0x0C]]) as usize;
                if be_sec == 256 || be_sec == 512 || be_sec == 1024 {
                    sector_bytes = be_sec;
                    sec_per_clus = disk[bo + 0x14].max(1);
                    let be_re = u16::from_be_bytes([disk[bo + 0x18], disk[bo + 0x19]]);
                    if be_re != 0 {
                        root_entries = be_re;
                    }
                    if disk[bo + 0x1D] != 0 {
                        fat_sectors = disk[bo + 0x1D];
                    }
                } else if le_sec == 256 || le_sec == 512 || le_sec == 1024 {
                    sector_bytes = le_sec;
                    sec_per_clus = disk[bo + 0x0D].max(1);
                    let le_re = u16::from_le_bytes([disk[bo + 0x11], disk[bo + 0x12]]);
                    if le_re != 0 {
                        root_entries = le_re;
                    }
                    let le_fs = u16::from_le_bytes([disk[bo + 0x16], disk[bo + 0x17]]);
                    if le_fs != 0 {
                        fat_sectors = le_fs as u8;
                    }
                }
            }
            return (
                start_rec,
                rec_count,
                sector_bytes,
                sec_per_clus,
                root_entries,
                fat_sectors,
            );
        }
    }
    (
        32,
        (disk.len() / 1024).saturating_sub(32) as u32,
        1024,
        1,
        1024,
        64,
    )
}

/// Virtual SCSI Controller state attached to `X68kBus`.
pub struct ScsiController {
    /// Up to 8 SCSI target disk images (512 or 1024 bytes per sector).
    pub drives: [Option<Vec<u8>>; 8],
    /// Whether the Virtual SCSI ROM at `$FC0000` is active.
    pub scsi_rom_enabled: bool,
    /// Latched request packet pointer from Strategy routine (`A5`).
    pub req_ptr: u32,
    /// Address of registered block driver header in guest memory (`A4` during ScsiRomdbBuild).
    pub driver_addr: u32,
    /// Whether system boots primarily from SCSI HDD instead of floppy.
    pub boot_from_scsi: bool,
}

impl Default for ScsiController {
    fn default() -> Self {
        Self::new()
    }
}

impl ScsiController {
    /// Creates a new unpopulated SCSI controller.
    pub fn new() -> Self {
        Self {
            drives: [None, None, None, None, None, None, None, None],
            scsi_rom_enabled: false,
            req_ptr: 0,
            driver_addr: 0,
            boot_from_scsi: false,
        }
    }

    /// Mounts a SCSI HDD disk image into `scsi_id` (`0..=7`).
    pub fn insert_hdd(&mut self, scsi_id: usize, image: Vec<u8>) -> bool {
        if scsi_id < self.drives.len() {
            self.drives[scsi_id] = Some(image);
            self.scsi_rom_enabled = true;
            true
        } else {
            false
        }
    }

    /// Direct slice access to SCSI target `scsi_id`.
    pub fn hdd_data(&self, scsi_id: usize) -> Option<&[u8]> {
        self.drives.get(scsi_id).and_then(|d| d.as_deref())
    }

    /// Direct mutable slice access to SCSI target `scsi_id`.
    pub fn hdd_data_mut(&mut self, scsi_id: usize) -> Option<&mut [u8]> {
        self.drives.get_mut(scsi_id).and_then(|d| d.as_deref_mut())
    }
}

impl X68kBus {
    /// Mounts a SCSI hard disk image onto `scsi_id` (`0..=7`), enabling the Virtual SCSI ROM.
    pub fn insert_scsi_hdd(&mut self, scsi_id: usize, image: Vec<u8>) -> bool {
        let ok = self.scsi.insert_hdd(scsi_id, image);
        if ok {
            self.enable_scsi_rom();
            // If mounting Drive 0 and no floppy is inserted, select SCSI ROM boot in SRAM
            if scsi_id == 0 && (!self.fdc.drives[0].is_inserted() || self.scsi.boot_from_scsi) {
                self.set_boot_from_scsi(true);
            }
        }
        ok
    }

    /// Read-only slice access to SCSI target `scsi_id`.
    pub fn scsi_hdd_data(&self, scsi_id: usize) -> Option<&[u8]> {
        self.scsi.hdd_data(scsi_id)
    }

    /// Read-write slice access to SCSI target `scsi_id`.
    pub fn scsi_hdd_data_mut(&mut self, scsi_id: usize) -> Option<&mut [u8]> {
        self.scsi.hdd_data_mut(scsi_id)
    }

    /// Returns `true` if Virtual SCSI ROM at `$FC0000` is enabled.
    pub fn is_scsi_rom_enabled(&self) -> bool {
        self.scsi.scsi_rom_enabled
    }

    /// Configures SRAM boot vectors to boot straight from SCSI ROM (`$FC0000`).
    pub fn set_boot_from_scsi(&mut self, boot: bool) {
        self.scsi.boot_from_scsi = boot;
        if boot {
            // Set SRAM $ED000C (ROM boot entry vector) to $00FC_0000
            self.sram[0x0C..0x10].copy_from_slice(&0x00FC_0000u32.to_be_bytes());
            // Select ROM boot device mode in SRAM $ED0018 ($0001 = ROM boot)
            self.sram[0x18..0x1A].copy_from_slice(&0x0001u16.to_be_bytes());
        } else {
            self.sram[0x0C..0x10].copy_from_slice(&0x00BF_FFFCu32.to_be_bytes());
            self.sram[0x18..0x1A].copy_from_slice(&0x0000u16.to_be_bytes());
        }
    }

    /// Returns `true` if configured to boot from SCSI.
    pub fn boot_from_scsi(&self) -> bool {
        self.scsi.boot_from_scsi
    }

    /// Enables and populates the Virtual SCSI ROM header and entry points at `$FC0000`.
    pub fn enable_scsi_rom(&mut self) {
        self.scsi.scsi_rom_enabled = true;

        // Offset inside self.rom (ROM window $F00000..$FFFFFF)
        const SCSI_ROM_OFFSET: usize = 0x0C0000; // $FC0000 - $F00000

        // $FC0000: Pointer P = $00FC_0050
        self.rom[SCSI_ROM_OFFSET..SCSI_ROM_OFFSET + 4]
            .copy_from_slice(&0x00FC_0050u32.to_be_bytes());

        // $FC0024..$FC002A: "SCSIIN"
        self.rom[SCSI_ROM_OFFSET + 0x24..SCSI_ROM_OFFSET + 0x2A].copy_from_slice(b"SCSIIN");

        // $FC0040 (P - 16): Pointer to romdb builder entry point ($00FC_0060)
        self.rom[SCSI_ROM_OFFSET + 0x40..SCSI_ROM_OFFSET + 0x44]
            .copy_from_slice(&0x00FC_0060u32.to_be_bytes());

        // $FC0044 (P - 12): Pointer to IOCS $F5 installer entry point ($00FC_0064)
        self.rom[SCSI_ROM_OFFSET + 0x44..SCSI_ROM_OFFSET + 0x48]
            .copy_from_slice(&0x00FC_0064u32.to_be_bytes());

        // $FC0048 (P - 8): "Huma"
        self.rom[SCSI_ROM_OFFSET + 0x48..SCSI_ROM_OFFSET + 0x4C].copy_from_slice(b"Huma");

        // $FC004C (P - 4): "n68k"
        self.rom[SCSI_ROM_OFFSET + 0x4C..SCSI_ROM_OFFSET + 0x50].copy_from_slice(b"n68k");

        // $FC0050 (P, SCSI Cold Boot entry): [0xFE, 0x82, 0x4E, 0x75] (ScsiRomBoot + RTS)
        self.rom[SCSI_ROM_OFFSET + 0x50..SCSI_ROM_OFFSET + 0x54]
            .copy_from_slice(&[0xFE, 0x82, 0x4E, 0x75]);

        // $FC0060 (romdb builder): [0xFE, 0x83, 0x4E, 0x75] (ScsiRomdbBuild + RTS)
        self.rom[SCSI_ROM_OFFSET + 0x60..SCSI_ROM_OFFSET + 0x64]
            .copy_from_slice(&[0xFE, 0x83, 0x4E, 0x75]);

        // $FC0064 (IOCS $F5 installer): [0xFE, 0x84, 0x4E, 0x75] (ScsiIocsInstall + RTS)
        self.rom[SCSI_ROM_OFFSET + 0x64..SCSI_ROM_OFFSET + 0x68]
            .copy_from_slice(&[0xFE, 0x84, 0x4E, 0x75]);

        // $FC0068 (IOCS $F5 handler): [0xFE, 0x85, 0x4E, 0x75] (ScsiIocsDispatch + RTS)
        self.rom[SCSI_ROM_OFFSET + 0x68..SCSI_ROM_OFFSET + 0x6C]
            .copy_from_slice(&[0xFE, 0x85, 0x4E, 0x75]);

        // $FC006C (romdb Strategy): [0xFE, 0x86, 0x4E, 0x75] (ScsiDriverStrategy + RTS)
        self.rom[SCSI_ROM_OFFSET + 0x6C..SCSI_ROM_OFFSET + 0x70]
            .copy_from_slice(&[0xFE, 0x86, 0x4E, 0x75]);

        // $FC0070 (romdb Interrupt): [0xFE, 0x87, 0x4E, 0x75] (ScsiDriverInterrupt + RTS)
        self.rom[SCSI_ROM_OFFSET + 0x70..SCSI_ROM_OFFSET + 0x74]
            .copy_from_slice(&[0xFE, 0x87, 0x4E, 0x75]);

        // Pre-install vector $F5 into RAM vector table at $0007D4 ($400 + $F5 * 4)
        if self.main_ram.len() >= 0x07D8 {
            self.main_ram[0x07D4..0x07D8].copy_from_slice(&0x00FC_0068u32.to_be_bytes());
        }
    }

    /// Handles `0xFE82` (`ScsiRomBoot`): boots attached SCSI HDD into Main RAM `$002000`.
    pub(crate) fn hypercall_scsi_rom_boot(&mut self, state: &mut CpuState) -> TrapResult {
        // Ensure vector $F5 is installed
        if self.main_ram.len() >= 0x07D8 {
            self.main_ram[0x07D4..0x07D8].copy_from_slice(&0x00FC_0068u32.to_be_bytes());
        }

        if let Some(disk) = self.scsi.hdd_data(0) {
            // Check if IPL bootloader is present at offset 0x400
            let has_ipl = disk.len() >= 0x800 && (disk[0x400] == 0x60 || disk[0x400] == 0x4E);
            if has_ipl && self.main_ram.len() >= 0x2400 {
                self.main_ram[0x2000..0x2400].copy_from_slice(&disk[0x400..0x800]);
                state.pc = 0x0000_2000;
                return TrapResult::Handled { extra_cycles: 0 };
            }

            // Fallback: search for first Human68k partition boot sector
            if let Some(parts) = parse_scsi_partitions(disk) {
                if let Some(part) = parts.iter().find(|p| &p.name == b"Human68k") {
                    let off = if disk.len() >= (part.start_record as usize) * 1024 + 1024 {
                        (part.start_record as usize) * 1024
                    } else {
                        (part.start_record as usize) * 512
                    };
                    if disk.len() >= off + 1024 && self.main_ram.len() >= 0x2400 {
                        self.main_ram[0x2000..0x2400].copy_from_slice(&disk[off..off + 1024]);
                        state.pc = 0x0000_2000;
                        return TrapResult::Handled { extra_cycles: 0 };
                    }
                }
            }
        }

        TrapResult::Handled { extra_cycles: 0 }
    }

    /// Handles `0xFE83` (`ScsiRomdbBuild`): constructs the Human68k block driver header at `A4`.
    pub(crate) fn hypercall_scsi_romdb_build(&mut self, state: &mut CpuState) -> TrapResult {
        // Ensure vector $F5 is installed
        if self.main_ram.len() >= 0x07D8 {
            self.main_ram[0x07D4..0x07D8].copy_from_slice(&0x00FC_0068u32.to_be_bytes());
        }

        if state.d[2] == 0 && self.scsi.hdd_data(0).is_some() {
            let a4 = (state.a[4] & 0x00FF_FFFF) as usize;
            self.scsi.driver_addr = a4 as u32;
            if a4 + 48 <= self.main_ram.len() {
                // 22-byte Human68k block device driver header
                self.main_ram[a4..a4 + 4].copy_from_slice(&0xFFFF_FFFFu32.to_be_bytes()); // Next driver
                self.main_ram[a4 + 4..a4 + 6].copy_from_slice(&0x0000u16.to_be_bytes()); // Attribute
                self.main_ram[a4 + 6..a4 + 10].copy_from_slice(&0x00FC_006Cu32.to_be_bytes()); // Strategy
                self.main_ram[a4 + 10..a4 + 14].copy_from_slice(&0x00FC_0070u32.to_be_bytes()); // Interrupt
                self.main_ram[a4 + 14] = 1; // 1 unit
                self.main_ram[a4 + 15..a4 + 22].fill(0); // Reserved name bytes

                // Extract partition information and BPB
                let disk = self.scsi.hdd_data(0).unwrap();
                let (start_rec, rec_count, sector_bytes, sec_per_clus, root_entries, fat_sectors) =
                    detect_partition_params(disk);

                // BPB pointer table at A4 + 22 (points to A4 + 26)
                let bpb_ptr = (a4 as u32) + 26;
                self.main_ram[a4 + 22..a4 + 26].copy_from_slice(&bpb_ptr.to_be_bytes());

                // 20-byte Human68k BPB structure at A4 + 26
                let sb_u16 = sector_bytes as u16;
                self.main_ram[a4 + 26..a4 + 28].copy_from_slice(&sb_u16.to_be_bytes()); // Bytes per sector
                self.main_ram[a4 + 28] = sec_per_clus; // Sectors per cluster
                self.main_ram[a4 + 29] = 2; // Number of FATs
                self.main_ram[a4 + 30..a4 + 32].copy_from_slice(&1u16.to_be_bytes()); // Reserved sectors
                self.main_ram[a4 + 32..a4 + 34].copy_from_slice(&root_entries.to_be_bytes()); // Root dir entries
                self.main_ram[a4 + 34..a4 + 36].copy_from_slice(&0u16.to_be_bytes()); // Total sectors 16-bit
                self.main_ram[a4 + 36] = 0xF7; // Media descriptor
                self.main_ram[a4 + 37] = fat_sectors; // FAT sectors per copy
                self.main_ram[a4 + 38..a4 + 42].copy_from_slice(&rec_count.to_be_bytes()); // Total sectors 32-bit
                let sr_u32 = start_rec as u32;
                self.main_ram[a4 + 42..a4 + 46].copy_from_slice(&sr_u32.to_be_bytes()); // Partition start sector
                self.main_ram[a4 + 46..a4 + 48].fill(0);

                state.d[2] = 1;
                return TrapResult::Handled { extra_cycles: 0 };
            }
        }

        state.d[2] = 0xFFFF_FFFF;
        TrapResult::Handled { extra_cycles: 0 }
    }

    /// Handles `0xFE84` (`ScsiIocsInstall`): writes `$00FC_0068` into `$0007D4`.
    pub(crate) fn hypercall_scsi_iocs_install(&mut self, state: &mut CpuState) -> TrapResult {
        if self.main_ram.len() >= 0x07D8 {
            self.main_ram[0x07D4..0x07D8].copy_from_slice(&0x00FC_0068u32.to_be_bytes());
        }
        state.d[0] = 0;
        TrapResult::Handled { extra_cycles: 0 }
    }

    /// Handles `0xFE85` (`ScsiIocsDispatch`): high-level SCSI IOCS subfunction dispatcher.
    pub(crate) fn hypercall_scsi_iocs_dispatch(&mut self, state: &mut CpuState) -> TrapResult {
        // Ensure vector $F5 is intact
        if self.main_ram.len() >= 0x07D8 {
            self.main_ram[0x07D4..0x07D8].copy_from_slice(&0x00FC_0068u32.to_be_bytes());
        }

        let cmd = (state.d[1] & 0xFF) as u8;
        match cmd {
            0x00 => {
                // _S_RESET
                state.d[0] = 0;
            }
            0x01..=0x0A => {
                // _S_SELECT and low-level bus phase stubs
                state.d[0] = 0;
            }
            0x20 => {
                // _S_INQUIRY: write 36-byte device descriptor to buffer A1
                let target = ((state.d[4] >> 4) & 7) as usize;
                if self.scsi.hdd_data(target).is_some() {
                    let buf = (state.a[1] & 0x00FF_FFFF) as usize;
                    if buf + 36 <= self.main_ram.len() {
                        self.main_ram[buf..buf + 36].fill(0);
                        self.main_ram[buf] = 0x00; // Direct-Access Block Device (HDD)
                        self.main_ram[buf + 1] = 0x00; // Fixed media
                        self.main_ram[buf + 2] = 0x02; // SCSI-2
                        self.main_ram[buf + 3] = 0x02;
                        self.main_ram[buf + 4] = 31; // Additional length
                        self.main_ram[buf + 8..buf + 16].copy_from_slice(b"SHARP   ");
                        self.main_ram[buf + 16..buf + 32].copy_from_slice(b"msx68k SCSI HDD ");
                        self.main_ram[buf + 32..buf + 36].copy_from_slice(b"1.00");
                        state.d[0] = 0;
                    } else {
                        state.d[0] = 0xFFFF_FFFF;
                    }
                } else {
                    state.d[0] = 0xFFFF_FFFF;
                }
            }
            0x21 | 0x26 => {
                // _S_READ ($21) / _S_READEXT ($26)
                let target = ((state.d[4] >> 4) & 7) as usize;
                if let Some(disk) = self.scsi.hdd_data(target) {
                    let d5 = (state.d[5] & 3) as usize;
                    let sector_bytes = if d5 <= 2 { 256usize << d5 } else { 512 };
                    let lba = state.d[2] as usize;
                    let count = (state.d[3] & 0xFFFF) as usize;
                    let byte_offset = lba * sector_bytes;
                    let transfer_bytes = count * sector_bytes;
                    let dest = (state.a[1] & 0x00FF_FFFF) as usize;

                    if dest + transfer_bytes <= self.main_ram.len()
                        && byte_offset + transfer_bytes <= disk.len()
                    {
                        self.main_ram[dest..dest + transfer_bytes]
                            .copy_from_slice(&disk[byte_offset..byte_offset + transfer_bytes]);
                        state.d[0] = 0;
                    } else {
                        state.d[0] = 0xFFFF_FFFF;
                    }
                } else {
                    state.d[0] = 0xFFFF_FFFF;
                }
            }
            0x22 | 0x27 => {
                // _S_WRITE ($22) / _S_WRITEEXT ($27)
                let target = ((state.d[4] >> 4) & 7) as usize;
                let d5 = (state.d[5] & 3) as usize;
                let sector_bytes = if d5 <= 2 { 256usize << d5 } else { 512 };
                let lba = state.d[2] as usize;
                let count = (state.d[3] & 0xFFFF) as usize;
                let byte_offset = lba * sector_bytes;
                let transfer_bytes = count * sector_bytes;
                let src = (state.a[1] & 0x00FF_FFFF) as usize;

                if let Some(disk) = self.scsi.hdd_data_mut(target) {
                    if src + transfer_bytes <= self.main_ram.len()
                        && byte_offset + transfer_bytes <= disk.len()
                    {
                        disk[byte_offset..byte_offset + transfer_bytes]
                            .copy_from_slice(&self.main_ram[src..src + transfer_bytes]);
                        state.d[0] = 0;
                    } else {
                        state.d[0] = 0xFFFF_FFFF;
                    }
                } else {
                    state.d[0] = 0xFFFF_FFFF;
                }
            }
            0x23 => {
                // _S_FORMAT
                state.d[0] = 0;
            }
            0x24 => {
                // _S_TESTUNIT
                let target = ((state.d[4] >> 4) & 7) as usize;
                state.d[0] = if self.scsi.hdd_data(target).is_some() {
                    0
                } else {
                    0xFFFF_FFFF
                };
            }
            0x25 => {
                // _S_READCAP: returns (last_lba, block_size)
                let target = ((state.d[4] >> 4) & 7) as usize;
                if let Some(disk) = self.scsi.hdd_data(target) {
                    let buf = (state.a[1] & 0x00FF_FFFF) as usize;
                    if buf + 8 <= self.main_ram.len() {
                        let block_size = 1024u32;
                        let last_lba = ((disk.len() / (block_size as usize)).saturating_sub(1)) as u32;
                        self.main_ram[buf..buf + 4].copy_from_slice(&last_lba.to_be_bytes());
                        self.main_ram[buf + 4..buf + 8].copy_from_slice(&block_size.to_be_bytes());
                        state.d[0] = 0;
                    } else {
                        state.d[0] = 0xFFFF_FFFF;
                    }
                } else {
                    state.d[0] = 0xFFFF_FFFF;
                }
            }
            0x28 => {
                // _S_VERIFY
                state.d[0] = 0;
            }
            0x29 | 0x2C => {
                // _S_MODESENSE
                let buf = (state.a[1] & 0x00FF_FFFF) as usize;
                if buf + 12 <= self.main_ram.len() {
                    self.main_ram[buf..buf + 12].fill(0);
                    self.main_ram[buf] = 11;
                    self.main_ram[buf + 3] = 8;
                    self.main_ram[buf + 8..buf + 12].copy_from_slice(&1024u32.to_be_bytes());
                    state.d[0] = 0;
                } else {
                    state.d[0] = 0xFFFF_FFFF;
                }
            }
            0x2A | 0x32 => {
                // _S_MODESELECT
                state.d[0] = 0;
            }
            0x2B | 0x2F => {
                // _S_REZERO
                state.d[0] = 0;
            }
            0x30 => {
                // _S_REQUEST / _S_REQUESTSENSE
                let buf = (state.a[1] & 0x00FF_FFFF) as usize;
                if buf + 18 <= self.main_ram.len() {
                    self.main_ram[buf..buf + 18].fill(0);
                    self.main_ram[buf] = 0x70;
                    self.main_ram[buf + 7] = 10;
                    state.d[0] = 0;
                }
                state.d[0] = 0;
            }
            _ => {
                state.d[0] = 0;
            }
        }
        TrapResult::Handled { extra_cycles: 0 }
    }

    /// Handles `0xFE86` (`ScsiDriverStrategy`): latches `req_ptr = state.a[5]`.
    pub(crate) fn hypercall_scsi_driver_strategy(&mut self, state: &mut CpuState) -> TrapResult {
        self.scsi.req_ptr = state.a[5] & 0x00FF_FFFF;
        TrapResult::Handled { extra_cycles: 0 }
    }

    /// Handles `0xFE87` (`ScsiDriverInterrupt`): processes the Human68k driver request packet.
    pub(crate) fn hypercall_scsi_driver_interrupt(&mut self, _state: &mut CpuState) -> TrapResult {
        let req_ptr = self.scsi.req_ptr as usize;
        if req_ptr + 26 > self.main_ram.len() {
            return TrapResult::Handled { extra_cycles: 0 };
        }

        let cmd = self.main_ram[req_ptr + 2];
        match cmd {
            0 => {
                // INIT (Command 0)
                self.main_ram[req_ptr + 13] = 1; // 1 unit
                let break_addr = self.scsi.driver_addr + 48;
                self.main_ram[req_ptr + 14..req_ptr + 18]
                    .copy_from_slice(&break_addr.to_be_bytes());
                let bpb_table_ptr = self.scsi.driver_addr + 22;
                self.main_ram[req_ptr + 18..req_ptr + 22]
                    .copy_from_slice(&bpb_table_ptr.to_be_bytes());
                self.main_ram[req_ptr + 3..req_ptr + 5].copy_from_slice(&0x0000u16.to_be_bytes());

                if self.scsi.boot_from_scsi {
                    self.main_ram[req_ptr + 22] = 1; // Starting letter = 1 (A:)
                }
            }
            1 => {
                // MEDIA CHECK (Command 1)
                self.main_ram[req_ptr + 14] = 1; // Media not changed
                self.main_ram[req_ptr + 3..req_ptr + 5].copy_from_slice(&0x0000u16.to_be_bytes());
            }
            2 => {
                // BUILD BPB (Command 2)
                let bpb_ptr = self.scsi.driver_addr + 26;
                self.main_ram[req_ptr + 18..req_ptr + 22]
                    .copy_from_slice(&bpb_ptr.to_be_bytes());
                self.main_ram[req_ptr + 3..req_ptr + 5].copy_from_slice(&0x0000u16.to_be_bytes());
            }
            5 => {
                // MEDIA SENSE / STATUS (Command 5): Bit 1 (0x02) indicates drive ready / media present
                self.main_ram[req_ptr + 13] = 0x02;
                self.main_ram[req_ptr + 3..req_ptr + 5].copy_from_slice(&0x0000u16.to_be_bytes());
            }
            4 => {
                // READ (Command 4)
                let dest = u32::from_be_bytes(
                    self.main_ram[req_ptr + 14..req_ptr + 18].try_into().unwrap(),
                ) as usize;
                let count = u32::from_be_bytes(
                    self.main_ram[req_ptr + 18..req_ptr + 22].try_into().unwrap(),
                ) as usize;
                let lba = u32::from_be_bytes(
                    self.main_ram[req_ptr + 22..req_ptr + 26].try_into().unwrap(),
                ) as usize;

                if let Some(disk) = self.scsi.hdd_data(0) {
                    let (start_rec, _, sector_bytes, _, _, _) = detect_partition_params(disk);
                    let byte_offset = (start_rec + lba) * sector_bytes;
                    let transfer_bytes = count * sector_bytes;

                    if dest + transfer_bytes <= self.main_ram.len()
                        && byte_offset + transfer_bytes <= disk.len()
                    {
                        self.main_ram[dest..dest + transfer_bytes]
                            .copy_from_slice(&disk[byte_offset..byte_offset + transfer_bytes]);
                        self.main_ram[req_ptr + 3..req_ptr + 5]
                            .copy_from_slice(&0x0000u16.to_be_bytes());
                    } else {
                        self.main_ram[req_ptr + 3..req_ptr + 5]
                            .copy_from_slice(&0x810Bu16.to_be_bytes()); // Read fault
                    }
                } else {
                    self.main_ram[req_ptr + 3..req_ptr + 5]
                        .copy_from_slice(&0x8102u16.to_be_bytes()); // Drive not ready
                }
            }
            8 | 9 => {
                // WRITE / WRITE VERIFY (Command 8 / 9)
                let src = u32::from_be_bytes(
                    self.main_ram[req_ptr + 14..req_ptr + 18].try_into().unwrap(),
                ) as usize;
                let count = u32::from_be_bytes(
                    self.main_ram[req_ptr + 18..req_ptr + 22].try_into().unwrap(),
                ) as usize;
                let lba = u32::from_be_bytes(
                    self.main_ram[req_ptr + 22..req_ptr + 26].try_into().unwrap(),
                ) as usize;

                if let Some(disk) = self.scsi.hdd_data_mut(0) {
                    let (start_rec, _, sector_bytes, _, _, _) = detect_partition_params(disk);
                    let byte_offset = (start_rec + lba) * sector_bytes;
                    let transfer_bytes = count * sector_bytes;

                    if src + transfer_bytes <= self.main_ram.len()
                        && byte_offset + transfer_bytes <= disk.len()
                    {
                        disk[byte_offset..byte_offset + transfer_bytes]
                            .copy_from_slice(&self.main_ram[src..src + transfer_bytes]);
                        self.main_ram[req_ptr + 3..req_ptr + 5]
                            .copy_from_slice(&0x0000u16.to_be_bytes());
                    } else {
                        self.main_ram[req_ptr + 3..req_ptr + 5]
                            .copy_from_slice(&0x810Au16.to_be_bytes()); // Write fault
                    }
                } else {
                    self.main_ram[req_ptr + 3..req_ptr + 5]
                        .copy_from_slice(&0x8102u16.to_be_bytes());
                }
            }
            _ => {
                self.main_ram[req_ptr + 3..req_ptr + 5].copy_from_slice(&0x0000u16.to_be_bytes());
            }
        }
        TrapResult::Handled { extra_cycles: 0 }
    }
}

/// Directory path segment as raw byte tuples: `(name8, ext3, name10)`.
pub type DirSegment = ([u8; 8], [u8; 3], [u8; 10]);
/// Hierarchical directory path.
pub type DirPath = Vec<DirSegment>;

/// Represents an extracted file from an XDF floppy image, preserving Human68k 18+3 filenames
/// and full directory hierarchies without Shift-JIS charset corruption.
#[derive(Debug, Clone)]
pub struct ExtractedFile {
    /// Directory path components as raw byte tuples: `(name8, ext3, name10)`.
    pub path_dirs: DirPath,
    /// Filename characters 1..=8 (`chunk[0..8]`).
    pub raw_name: [u8; 8],
    /// File extension (`chunk[8..11]`).
    pub raw_ext: [u8; 3],
    /// Human68k filename characters 9..=18 (`chunk[12..22]`).
    pub raw_name2: [u8; 10],
    /// Raw uncompressed file content.
    pub data: Vec<u8>,
    /// FAT directory attribute byte (`chunk[11]`).
    pub attr: u8,
    /// DOS timestamp (`chunk[22..24]`).
    pub time: u16,
    /// DOS datestamp (`chunk[24..26]`).
    pub date: u16,
}

/// Extracts all files recursively from a standard 1,261,568-byte 2HD XDF disk image.
pub fn extract_files_from_xdf(xdf_data: &[u8]) -> Vec<ExtractedFile> {
    let mut files = Vec::new();
    if xdf_data.len() < 0x2C00 {
        return files;
    }

    let fat = &xdf_data[0x400..0xC00];
    let get_next_cluster = |c: usize| -> usize {
        let off = (c * 3) / 2;
        if off + 1 >= fat.len() {
            return 0xFFF;
        }
        let raw = (fat[off] as usize) | ((fat[off + 1] as usize) << 8);
        if c.is_multiple_of(2) {
            raw & 0x0FFF
        } else {
            raw >> 4
        }
    };

    let read_cluster_chain = |start_cluster: usize| -> Vec<u8> {
        let mut buf = Vec::new();
        let mut c = start_cluster;
        while (2..0x0FF8).contains(&c) {
            let off = 0x2C00 + (c - 2) * 1024;
            if off + 1024 > xdf_data.len() {
                break;
            }
            buf.extend_from_slice(&xdf_data[off..off + 1024]);
            c = get_next_cluster(c);
        }
        buf
    };

    fn parse_dir_entries(
        dir_data: &[u8],
        parent_dirs: &[DirSegment],
        read_chain: &impl Fn(usize) -> Vec<u8>,
        out: &mut Vec<ExtractedFile>,
    ) {
        for chunk in dir_data.chunks_exact(32) {
            let first = chunk[0];
            if first == 0 {
                break;
            }
            if first == 0xE5 || first == 0x2E {
                continue;
            }
            let attr = chunk[11];
            if attr == 0x08 {
                // Skip volume labels
                continue;
            }
            let mut raw_name = [0u8; 8];
            raw_name.copy_from_slice(&chunk[0..8]);
            if first == 0x05 {
                raw_name[0] = 0xE5;
            }
            let mut raw_ext = [0u8; 3];
            raw_ext.copy_from_slice(&chunk[8..11]);
            let mut raw_name2 = [0u8; 10];
            raw_name2.copy_from_slice(&chunk[12..22]);

            let cluster = u16::from_le_bytes([chunk[26], chunk[27]]) as usize;
            let size = u32::from_le_bytes([chunk[28], chunk[29], chunk[30], chunk[31]]) as usize;
            let time = u16::from_le_bytes([chunk[22], chunk[23]]);
            let date = u16::from_le_bytes([chunk[24], chunk[25]]);

            if (attr & 0x10) != 0 {
                // Subdirectory
                let mut child_dirs = parent_dirs.to_vec();
                child_dirs.push((raw_name, raw_ext, raw_name2));
                let sub_data = read_chain(cluster);
                parse_dir_entries(&sub_data, &child_dirs, read_chain, out);
            } else {
                let file_bytes = read_chain(cluster);
                let actual_len = size.min(file_bytes.len());
                out.push(ExtractedFile {
                    path_dirs: parent_dirs.to_vec(),
                    raw_name,
                    raw_ext,
                    raw_name2,
                    data: file_bytes[..actual_len].to_vec(),
                    attr,
                    time,
                    date,
                });
            }
        }
    }

    let root_bytes = &xdf_data[0x1400..0x2C00];
    parse_dir_entries(root_bytes, &Vec::new(), &read_cluster_chain, &mut files);
    files
}

fn normalize_sjis<const N: usize>(bytes: &[u8; N]) -> [u8; N] {
    let mut out = *bytes;
    let mut i = 0;
    while i < N {
        let b = out[i];
        if (0x81..=0x9F).contains(&b) || (0xE0..=0xEF).contains(&b) {
            i += 2;
        } else {
            if b.is_ascii_lowercase() {
                out[i] = b.to_ascii_uppercase();
            }
            i += 1;
        }
    }
    out
}

type FileKey = (
    Vec<([u8; 8], [u8; 3], [u8; 10])>,
    [u8; 8],
    [u8; 3],
    [u8; 10],
);

fn make_file_key(f: &ExtractedFile) -> FileKey {
    let mut norm_dirs = Vec::with_capacity(f.path_dirs.len());
    for (n1, ext, n2) in &f.path_dirs {
        norm_dirs.push((
            normalize_sjis(n1),
            normalize_sjis(ext),
            normalize_sjis(n2),
        ));
    }
    (
        norm_dirs,
        normalize_sjis(&f.raw_name),
        normalize_sjis(&f.raw_ext),
        normalize_sjis(&f.raw_name2),
    )
}

fn replace_ascii_case_insensitive(data: &[u8], target: &[u8], replacement: &[u8]) -> Vec<u8> {
    let mut result = Vec::new();
    let mut i = 0;
    while i < data.len() {
        if i + target.len() <= data.len()
            && data[i..i + target.len()].eq_ignore_ascii_case(target)
        {
            result.extend_from_slice(replacement);
            i += target.len();
        } else {
            result.push(data[i]);
            i += 1;
        }
    }
    result
}

/// Constructs a bootable all-in-one SX-Window v3.1 SCSI Hard Disk image (`.HDF` / `.HDS`).
///
/// Merges all files from `human_xdf` (`HUMAN302.XDF`) and `sxwin_xdfs` (`SXWIN311.XDF`..`SXWIN316.XDF`)
/// into a bootable FAT16 Human68k partition with `X68SCSI1` header and `X68K` partition table.
pub fn build_bootable_sxwin_scsi_hdd(
    human_xdf: &[u8],
    sxwin_xdfs: &[(&str, &[u8])],
    size_mb: usize,
) -> Vec<u8> {
    const RECORD_SIZE: usize = 1024;
    let actual_size_mb = size_mb.max(8);
    let total_bytes = actual_size_mb * 1024 * 1024;
    let total_records = (total_bytes / RECORD_SIZE) as u32;
    let last_record = total_records.saturating_sub(1);

    let partition_start_record = 32u32;
    let partition_records = total_records.saturating_sub(partition_start_record);
    let partition_byte_offset = (partition_start_record as usize) * RECORD_SIZE;

    let mut image = vec![0u8; total_bytes];

    // 1. Sector 0: SCSI Header ("X68SCSI1")
    image[0x00..0x08].copy_from_slice(b"X68SCSI1");
    image[0x08..0x0A].copy_from_slice(&0x0200u16.to_be_bytes()); // 1024 bytes per sector code
    image[0x0A..0x0E].copy_from_slice(&last_record.to_be_bytes());
    image[0x0E..0x10].copy_from_slice(&0x0100u16.to_be_bytes()); // Flags
    let desc = b"This SCSI-UNIT format is 'SxSI'  for scsiform.   by Hero Soft.       (C-lab CO)";
    image[0x10..0x10 + desc.len()].copy_from_slice(desc);

    // 2. Sector 1 (Record 1, offset 0x400): SCSI IPL boot code
    let ipl_sector = synthesize_scsi_ipl_sector(partition_start_record);
    image[0x400..0x800].copy_from_slice(&ipl_sector);

    // 3. Sector 2 (Record 2, offset 0x800): "X68K" partition table
    image[0x800..0x804].copy_from_slice(b"X68K");
    image[0x804..0x808].copy_from_slice(&total_records.to_be_bytes());
    image[0x808..0x80C].copy_from_slice(&last_record.to_be_bytes());
    image[0x80C..0x810].copy_from_slice(&last_record.to_be_bytes());
    image[0x810..0x818].copy_from_slice(b"Human68k");
    image[0x818..0x81C].copy_from_slice(&partition_start_record.to_be_bytes());
    image[0x81C..0x820].copy_from_slice(&partition_records.to_be_bytes());

    // 4. Merge all files
    let mut merged_map: BTreeMap<FileKey, ExtractedFile> = BTreeMap::new();
    for f in extract_files_from_xdf(human_xdf) {
        merged_map.insert(make_file_key(&f), f);
    }
    for (label, xdf_bytes) in sxwin_xdfs {
        for mut f in extract_files_from_xdf(xdf_bytes) {
            // Organize files according to official Sharp SX31_HD.INS layout:
            // 1) SXWIN312 contains desktop applications and tools (all in root on floppy).
            //    Move them into \X68 on HDD.
            if *label == "SXWIN312" && f.path_dirs.is_empty() {
                let mut x68_dir = [b' '; 8];
                x68_dir[..3].copy_from_slice(b"X68");
                f.path_dirs = vec![(x68_dir, [b' '; 3], [0u8; 10])];
            }
            // 2) SXWIN315 contains Japanese dictionaries X68K.DIC and POST.DIC.
            //    Move them into \DIC on HDD. Any dictionary maintenance tools go to \X68.
            if *label == "SXWIN315" && f.path_dirs.is_empty() {
                let ext = normalize_sjis(&f.raw_ext);
                if ext.starts_with(b"DIC") {
                    let mut dic_dir = [b' '; 8];
                    dic_dir[..3].copy_from_slice(b"DIC");
                    f.path_dirs = vec![(dic_dir, [b' '; 3], [0u8; 10])];
                } else {
                    let mut x68_dir = [b' '; 8];
                    x68_dir[..3].copy_from_slice(b"X68");
                    f.path_dirs = vec![(x68_dir, [b' '; 3], [0u8; 10])];
                }
            }
            // 3) SXWIN311 has HDフォーマット.X in root. Move it into \X68.
            if *label == "SXWIN311"
                && f.path_dirs.is_empty()
                && normalize_sjis(&f.raw_name).starts_with(b"HD")
            {
                let mut x68_dir = [b' '; 8];
                x68_dir[..3].copy_from_slice(b"X68");
                f.path_dirs = vec![(x68_dir, [b' '; 3], [0u8; 10])];
            }
            // 4) SXWIN313 has outline font files. If in root, move to \FONT (unless IFM/IVM tools in \SHELL).
            if *label == "SXWIN313" && f.path_dirs.is_empty() {
                let name = normalize_sjis(&f.raw_name);
                if name.starts_with(b"IFM") || name.starts_with(b"IVM") {
                    let mut shell_dir = [b' '; 8];
                    shell_dir[..5].copy_from_slice(b"SHELL");
                    f.path_dirs = vec![(shell_dir, [b' '; 3], [0u8; 10])];
                } else {
                    let mut font_dir = [b' '; 8];
                    font_dir[..4].copy_from_slice(b"FONT");
                    f.path_dirs = vec![(font_dir, [b' '; 3], [0u8; 10])];
                }
            }
            // 5) SXWIN314 has sample media files in root. Move to \SAMPLE.
            if *label == "SXWIN314" && f.path_dirs.is_empty() {
                let mut sample_dir = [b' '; 8];
                sample_dir[..6].copy_from_slice(b"SAMPLE");
                f.path_dirs = vec![(sample_dir, [b' '; 3], [0u8; 10])];
            }
            // 6) SXWIN316 has HDフォーマット.X or installer files in root. Move HD format to \X68, installer tools to \INST.
            if *label == "SXWIN316" && f.path_dirs.is_empty() {
                let name = normalize_sjis(&f.raw_name);
                if name.starts_with(b"HD") {
                    let mut x68_dir = [b' '; 8];
                    x68_dir[..3].copy_from_slice(b"X68");
                    f.path_dirs = vec![(x68_dir, [b' '; 3], [0u8; 10])];
                } else {
                    let mut inst_dir = [b' '; 8];
                    inst_dir[..4].copy_from_slice(b"INST");
                    f.path_dirs = vec![(inst_dir, [b' '; 3], [0u8; 10])];
                }
            }
            merged_map.insert(make_file_key(&f), f);
        }

        // Recover deleted SHELL/SXCON.X from SXWIN311.XDF
        if *label == "SXWIN311" {
            const SXCON_OFFSET: usize = 0x25800;
            const SXCON_SIZE: usize = 39558;
            if xdf_bytes.len() >= SXCON_OFFSET + SXCON_SIZE
                && &xdf_bytes[SXCON_OFFSET..SXCON_OFFSET + 2] == b"HU"
            {
                let mut shell_dir = [b' '; 8];
                shell_dir[..5].copy_from_slice(b"SHELL");
                let mut raw_name = [b' '; 8];
                raw_name[..5].copy_from_slice(b"SXCON");
                let mut raw_ext = [b' '; 3];
                raw_ext[..1].copy_from_slice(b"X");
                let f_sxcon = ExtractedFile {
                    path_dirs: vec![(shell_dir, [b' '; 3], [0u8; 10])],
                    raw_name,
                    raw_ext,
                    raw_name2: [0u8; 10],
                    attr: 0x20,
                    time: 0x6000,
                    date: 0x1CF4,
                    data: xdf_bytes[SXCON_OFFSET..SXCON_OFFSET + SXCON_SIZE].to_vec(),
                };
                merged_map.insert(make_file_key(&f_sxcon), f_sxcon);
            }
        }
    }

    // Apply SX-Window on SCSI patches:
    for f in merged_map.values_mut() {
        // In CONFIG.SYS, redirect dictionary path to \DIC\X68K.DIC
        if f.path_dirs.is_empty()
            && normalize_sjis(&f.raw_name).starts_with(b"CONFIG")
            && normalize_sjis(&f.raw_ext).starts_with(b"SYS")
        {
            f.data = replace_ascii_case_insensitive(&f.data, b"/DB:\\X68K.DIC", b"/DA:\\DIC\\X68K.DIC");
            f.data = replace_ascii_case_insensitive(&f.data, b"/DA:\\X68K.DIC", b"/DA:\\DIC\\X68K.DIC");
            f.data = replace_ascii_case_insensitive(&f.data, b"/DA:\\ASK\\X68K.DIC", b"/DA:\\DIC\\X68K.DIC");
        }
        // In SHELL/SXWIN.ENV, ensure path includes all official application directories:
        if f.path_dirs.len() == 1
            && normalize_sjis(&f.path_dirs[0].0).starts_with(b"SHELL")
            && normalize_sjis(&f.raw_name).starts_with(b"SXWIN")
            && normalize_sjis(&f.raw_ext).starts_with(b"ENV")
        {
            const SJIS_SHARP: &[u8] = &[0xBC, 0xAC, 0xB0, 0xCD, 0xDF, 0xDD]; // ｼｬｰﾍﾟﾝ
            let mut new_path = Vec::new();
            new_path.extend_from_slice(b"path=A:\\;A:\\SHELL;A:\\SYS;A:\\BIN;A:\\X68;A:\\");
            new_path.extend_from_slice(SJIS_SHARP);
            new_path.extend_from_slice(b";A:\\");
            new_path.extend_from_slice(SJIS_SHARP);
            new_path.extend_from_slice(b"\\EXCOM;A:\\FONT;A:\\SAMPLE;\r\n");
            f.data = new_path;
        }
    }

    // 4b. Inject SXMODE.X built from guest/sxmode/sxmode.s into \BIN\SXMODE.X, \X68\SXMODE.X, and root A:\SXMODE.X
    let workspace_root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let sxmode_src = workspace_root.join("guest/sxmode/sxmode.s");
    let inc_dir = workspace_root.join("guest/include");
    if sxmode_src.exists() && inc_dir.exists() {
        if let Ok(sxmode_bytes) =
            crate::guest_build::build_guest_x_executable(&sxmode_src, &[&inc_dir])
        {
            let mut raw_name = [b' '; 8];
            raw_name[..6].copy_from_slice(b"SXMODE");
            let mut raw_ext = [b' '; 3];
            raw_ext[..1].copy_from_slice(b"X");

            // Place in A:\X68\SXMODE.X
            let mut x68_dir = [b' '; 8];
            x68_dir[..3].copy_from_slice(b"X68");
            let f_x68 = ExtractedFile {
                path_dirs: vec![(x68_dir, [b' '; 3], [0u8; 10])],
                raw_name,
                raw_ext,
                raw_name2: [0u8; 10],
                attr: 0x20,
                time: 0x6000,
                date: 0x5821,
                data: sxmode_bytes.clone(),
            };
            merged_map.insert(make_file_key(&f_x68), f_x68);

            // Place in A:\BIN\SXMODE.X
            let mut bin_dir = [b' '; 8];
            bin_dir[..3].copy_from_slice(b"BIN");
            let f_bin = ExtractedFile {
                path_dirs: vec![(bin_dir, [b' '; 3], [0u8; 10])],
                raw_name,
                raw_ext,
                raw_name2: [0u8; 10],
                attr: 0x20,
                time: 0x6000,
                date: 0x5821,
                data: sxmode_bytes.clone(),
            };
            merged_map.insert(make_file_key(&f_bin), f_bin);

            // Also place in root A:\SXMODE.X for convenient 1-click access
            let f_root = ExtractedFile {
                path_dirs: Vec::new(),
                raw_name,
                raw_ext,
                raw_name2: [0u8; 10],
                attr: 0x20,
                time: 0x6000,
                date: 0x5821,
                data: sxmode_bytes,
            };
            merged_map.insert(make_file_key(&f_root), f_root);
        }
    }

    // 5. Partition Boot Sector
    let fat_recs = 64u16;
    let root_entries = 1024u16;
    let boot_off = partition_byte_offset;
    let boot_sector = synthesize_partition_boot_sector(
        partition_start_record,
        partition_records,
        fat_recs,
        root_entries,
    );
    image[boot_off..boot_off + RECORD_SIZE].copy_from_slice(&boot_sector);

    // 6. Setup FAT tables and directory tree
    let fat_bytes = (fat_recs as usize) * RECORD_SIZE;
    let fat1_off = boot_off + RECORD_SIZE;
    let fat2_off = fat1_off + fat_bytes;
    let root_dir_off = fat2_off + fat_bytes;
    let root_dir_bytes = (root_entries as usize) * 32;
    let data_start_off = root_dir_off + root_dir_bytes;

    let mut fat = vec![0u8; fat_bytes];
    // FAT Entry 0 ($FFF7 / $F7FF) & Entry 1 ($FFFF)
    fat[0] = 0xF7;
    fat[1] = 0xFF;
    fat[2] = 0xFF;
    fat[3] = 0xFF;

    let mut next_cluster = 2usize;

    // Separate files into prioritized list: HUMAN.SYS at Cluster 2, then COMMAND.X, then others
    let mut all_files: Vec<ExtractedFile> = merged_map.into_values().collect();
    let human_idx = all_files.iter().position(|f| {
        f.path_dirs.is_empty()
            && normalize_sjis(&f.raw_name).starts_with(b"HUMAN")
            && normalize_sjis(&f.raw_ext).starts_with(b"SYS")
    });
    if let Some(idx) = human_idx {
        let f = all_files.remove(idx);
        all_files.insert(0, f);
    }
    let command_idx = all_files.iter().position(|f| {
        f.path_dirs.is_empty()
            && normalize_sjis(&f.raw_name).starts_with(b"COMMAND")
            && normalize_sjis(&f.raw_ext).starts_with(b"X")
    });
    if let Some(idx) = command_idx {
        let f = all_files.remove(idx);
        let insert_idx = if human_idx.is_some() { 1 } else { 0 };
        all_files.insert(insert_idx, f);
    }

    // Allocate clusters and write file data
    let mut file_start_clusters = Vec::with_capacity(all_files.len());
    for file in &all_files {
        let file_len = file.data.len();
        let num_clusters = if file_len == 0 {
            0
        } else {
            file_len.div_ceil(RECORD_SIZE)
        };
        let start_cluster = if num_clusters == 0 {
            0
        } else {
            let start = next_cluster;
            for i in 0..num_clusters {
                let curr = start + i;
                let is_last = i == num_clusters - 1;
                let next_val = if is_last { 0xFFFFu16 } else { (curr + 1) as u16 };
                let f_idx = curr * 2;
                if f_idx + 2 <= fat.len() {
                    fat[f_idx..f_idx + 2].copy_from_slice(&next_val.to_be_bytes());
                }
                let d_off = data_start_off + (curr - 2) * RECORD_SIZE;
                let src_start = i * RECORD_SIZE;
                let src_end = (src_start + RECORD_SIZE).min(file_len);
                if src_start < file_len && d_off + (src_end - src_start) <= image.len() {
                    image[d_off..d_off + (src_end - src_start)]
                        .copy_from_slice(&file.data[src_start..src_end]);
                }
            }
            next_cluster += num_clusters;
            start
        };
        file_start_clusters.push(start_cluster);
    }

    // Collect all unique directories (including nested directories)
    let mut all_dirs: BTreeSet<DirPath> = BTreeSet::new();
    for f in &all_files {
        for len in 1..=f.path_dirs.len() {
            all_dirs.insert(f.path_dirs[..len].to_vec());
        }
    }

    // Sort directories by depth (shallowest first)
    let mut sorted_dirs: Vec<DirPath> = all_dirs.into_iter().collect();
    sorted_dirs.sort_by_key(|d| d.len());

    // Allocate clusters for all subdirectories
    let mut dir_clusters: BTreeMap<DirPath, usize> = BTreeMap::new();
    for dir_path in &sorted_dirs {
        // Count entries in this directory: 2 (. and ..) + child dirs + files
        let child_dir_count = sorted_dirs
            .iter()
            .filter(|d| d.len() == dir_path.len() + 1 && d.starts_with(dir_path))
            .count();
        let file_count = all_files.iter().filter(|f| &f.path_dirs == dir_path).count();
        let total_entries = 2 + child_dir_count + file_count;
        let num_clusters = (total_entries * 32).div_ceil(RECORD_SIZE);

        let dir_cluster = next_cluster;
        for i in 0..num_clusters {
            let curr = dir_cluster + i;
            let is_last = i == num_clusters - 1;
            let next_val = if is_last { 0xFFFFu16 } else { (curr + 1) as u16 };
            let f_idx = curr * 2;
            if f_idx + 2 <= fat.len() {
                fat[f_idx..f_idx + 2].copy_from_slice(&next_val.to_be_bytes());
            }
        }
        next_cluster += num_clusters;
        dir_clusters.insert(dir_path.clone(), dir_cluster);
    }

    // Write directory contents for each subdirectory
    for dir_path in &sorted_dirs {
        let dir_cluster = *dir_clusters.get(dir_path).unwrap();
        let child_dirs: Vec<&DirPath> = sorted_dirs
            .iter()
            .filter(|d| d.len() == dir_path.len() + 1 && d.starts_with(dir_path))
            .collect();
        let file_entries: Vec<(usize, &ExtractedFile)> = all_files
            .iter()
            .enumerate()
            .filter(|(_, f)| &f.path_dirs == dir_path)
            .collect();

        let total_entries = 2 + child_dirs.len() + file_entries.len();
        let num_clusters = (total_entries * 32).div_ceil(RECORD_SIZE);
        let mut d_bytes = vec![0u8; num_clusters * RECORD_SIZE];

        // '.' entry (index 0)
        d_bytes[0..8].copy_from_slice(b".       ");
        d_bytes[8..11].copy_from_slice(b"   ");
        d_bytes[11] = 0x10;
        d_bytes[26..28].copy_from_slice(&(dir_cluster as u16).to_le_bytes());

        // '..' entry (index 1)
        let parent_path = &dir_path[..dir_path.len() - 1];
        let parent_cluster = if parent_path.is_empty() {
            0u16 // Root directory
        } else {
            *dir_clusters.get(parent_path).unwrap() as u16
        };
        d_bytes[32..40].copy_from_slice(b"..      ");
        d_bytes[40..43].copy_from_slice(b"   ");
        d_bytes[43] = 0x10;
        d_bytes[58..60].copy_from_slice(&parent_cluster.to_le_bytes());

        let mut slot = 2;
        // Child subdirectories
        for child_path in child_dirs {
            let child_cluster = *dir_clusters.get(child_path).unwrap();
            let child_item = child_path.last().unwrap();
            let off = slot * 32;
            if off + 32 <= d_bytes.len() {
                d_bytes[off..off + 8].copy_from_slice(&child_item.0);
                d_bytes[off + 8..off + 11].copy_from_slice(&child_item.1);
                d_bytes[off + 11] = 0x10;
                d_bytes[off + 12..off + 22].copy_from_slice(&child_item.2); // raw_name2
                d_bytes[off + 26..off + 28].copy_from_slice(&(child_cluster as u16).to_le_bytes());
                slot += 1;
            }
        }

        // Direct files
        for (f_idx, file) in file_entries {
            let start_c = file_start_clusters[f_idx];
            let off = slot * 32;
            if off + 32 <= d_bytes.len() {
                d_bytes[off..off + 8].copy_from_slice(&file.raw_name);
                d_bytes[off + 8..off + 11].copy_from_slice(&file.raw_ext);
                d_bytes[off + 11] = file.attr;
                d_bytes[off + 12..off + 22].copy_from_slice(&file.raw_name2); // raw_name2
                d_bytes[off + 22..off + 24].copy_from_slice(&file.time.to_le_bytes());
                d_bytes[off + 24..off + 26].copy_from_slice(&file.date.to_le_bytes());
                d_bytes[off + 26..off + 28].copy_from_slice(&(start_c as u16).to_le_bytes());
                d_bytes[off + 28..off + 32].copy_from_slice(&(file.data.len() as u32).to_le_bytes());
                slot += 1;
            }
        }

        let dir_off = data_start_off + (dir_cluster - 2) * RECORD_SIZE;
        if dir_off + d_bytes.len() <= image.len() {
            image[dir_off..dir_off + d_bytes.len()].copy_from_slice(&d_bytes);
        }
    }

    // Write root directory entries
    let mut root_bytes = vec![0u8; root_dir_bytes];
    let mut root_slot = 0;

    // Top-level subdirectories (depth 1)
    for dir_path in sorted_dirs.iter().filter(|d| d.len() == 1) {
        let dir_cluster = *dir_clusters.get(dir_path).unwrap();
        let dir_item = &dir_path[0];
        let off = root_slot * 32;
        if off + 32 <= root_bytes.len() {
            root_bytes[off..off + 8].copy_from_slice(&dir_item.0);
            root_bytes[off + 8..off + 11].copy_from_slice(&dir_item.1);
            root_bytes[off + 11] = 0x10;
            root_bytes[off + 12..off + 22].copy_from_slice(&dir_item.2); // raw_name2
            root_bytes[off + 26..off + 28].copy_from_slice(&(dir_cluster as u16).to_le_bytes());
            root_slot += 1;
        }
    }

    // Root files
    for (f_idx, file) in all_files.iter().enumerate() {
        if !file.path_dirs.is_empty() {
            continue;
        }
        let start_c = file_start_clusters[f_idx];
        let off = root_slot * 32;
        if off + 32 <= root_bytes.len() {
            root_bytes[off..off + 8].copy_from_slice(&file.raw_name);
            root_bytes[off + 8..off + 11].copy_from_slice(&file.raw_ext);
            root_bytes[off + 11] = file.attr;
            root_bytes[off + 12..off + 22].copy_from_slice(&file.raw_name2); // raw_name2
            root_bytes[off + 22..off + 24].copy_from_slice(&file.time.to_le_bytes());
            root_bytes[off + 24..off + 26].copy_from_slice(&file.date.to_le_bytes());
            root_bytes[off + 26..off + 28].copy_from_slice(&(start_c as u16).to_le_bytes());
            root_bytes[off + 28..off + 32].copy_from_slice(&(file.data.len() as u32).to_le_bytes());
            root_slot += 1;
        }
    }

    image[root_dir_off..root_dir_off + root_dir_bytes].copy_from_slice(&root_bytes);
    image[fat1_off..fat1_off + fat_bytes].copy_from_slice(&fat);
    image[fat2_off..fat2_off + fat_bytes].copy_from_slice(&fat);

    image
}
