//! NEC uPD72065 Floppy Disk Controller (`$E94001`–`$E94003`), Floppy Disk Drive Unit Control
//! (`$E94005`–`$E94007`), and X68000 I/O Interrupt Controller (`$E9C001`–`$E9C003`, Level 1).

use msx68k_cpu::BusFault;

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use crate::dmac::Dmac;
use crate::vmx68k::RemoteDisk;

// Main Status Register ($E94001 read) bit masks
pub const MSR_RQM: u8 = 0x80; // Request for Master
pub const MSR_DIO: u8 = 0x40; // Data Input/Output (1 = FDC -> CPU, 0 = CPU -> FDC)
pub const MSR_NDM: u8 = 0x20; // Non-DMA Mode execution
pub const MSR_CB: u8 = 0x10;  // Controller Busy

// Status Register 0 (ST0) bits
pub const ST0_NT: u8 = 0x00;  // Normal Termination
pub const ST0_AT: u8 = 0x40;  // Abnormal Termination
pub const ST0_IC: u8 = 0x80;  // Invalid Command
pub const ST0_AI: u8 = 0xC0;  // Attention Interrupt (Ready state changed)
pub const ST0_SE: u8 = 0x20;  // Seek End
pub const ST0_EC: u8 = 0x10;  // Equipment Check
pub const ST0_NR: u8 = 0x08;  // Not Ready

// Status Register 1 (ST1) bits
pub const ST1_EN: u8 = 0x80;  // End of Cylinder
pub const ST1_DE: u8 = 0x20;  // Data Error (CRC)
pub const ST1_ND: u8 = 0x04;  // No Data (Sector not found)
pub const ST1_NW: u8 = 0x02;  // Not Writable
pub const ST1_MA: u8 = 0x01;  // Missing Address Mark

// Status Register 3 (ST3) bits
pub const ST3_WP: u8 = 0x40;  // Write Protected
pub const ST3_RY: u8 = 0x20;  // Ready
pub const ST3_T0: u8 = 0x10;  // Track 0
pub const ST3_TS: u8 = 0x08;  // Two Side

/// X68000 I/O Interrupt Controller (`$E9C000`–`$E9DFFF`, Interrupt Level 1).
///
/// Controls Level 1 interrupts from FDC, FDD, HDC (SASI), and Printer.
#[derive(Debug, Clone)]
pub struct IoInterruptController {
    /// Signal bits in upper nibble: bit 7 = FDC, bit 6 = FDD, bit 5 = PRN Ready, bit 4 = HDC.
    signal: u8,
    /// Enable bits in lower nibble: bit 3 = HDC, bit 2 = FDC, bit 1 = FDD, bit 0 = PRN.
    enable: u8,
    /// Interrupt vector base (`$E9C003`, masked with `0xFC`).
    vector_base: u8,
}

impl Default for IoInterruptController {
    fn default() -> Self {
        Self::new()
    }
}

impl IoInterruptController {
    pub const SIG_FDC: u8 = 0x80;
    pub const SIG_FDD: u8 = 0x40;
    pub const SIG_PRN: u8 = 0x20;
    pub const SIG_HDC: u8 = 0x10;

    pub const EN_HDC: u8 = 0x08;
    pub const EN_FDC: u8 = 0x04;
    pub const EN_FDD: u8 = 0x02;
    pub const EN_PRN: u8 = 0x01;

    pub const fn new() -> Self {
        Self {
            signal: 0,
            enable: 0,
            vector_base: 0x60,
        }
    }

    pub fn reset(&mut self) {
        *self = Self::new();
    }

    #[inline]
    pub fn set_fdc_signal(&mut self, active: bool) {
        if active {
            self.signal |= Self::SIG_FDC;
        } else {
            self.signal &= !Self::SIG_FDC;
        }
    }

    #[inline]
    pub fn set_fdd_signal(&mut self, active: bool) {
        if active {
            self.signal |= Self::SIG_FDD;
        } else {
            self.signal &= !Self::SIG_FDD;
        }
    }

    #[inline]
    pub fn set_hdc_signal(&mut self, active: bool) {
        if active {
            self.signal |= Self::SIG_HDC;
        } else {
            self.signal &= !Self::SIG_HDC;
        }
    }

    /// Returns `true` if any enabled Level 1 I/O interrupt source is active.
    pub const fn has_pending_interrupt(&self) -> bool {
        let fdc = (self.signal & Self::SIG_FDC) != 0 && (self.enable & Self::EN_FDC) != 0;
        let fdd = (self.signal & Self::SIG_FDD) != 0 && (self.enable & Self::EN_FDD) != 0;
        let hdc = (self.signal & Self::SIG_HDC) != 0 && (self.enable & Self::EN_HDC) != 0;
        let prn = (self.signal & Self::SIG_PRN) != 0 && (self.enable & Self::EN_PRN) != 0;
        fdc || fdd || hdc || prn
    }

    /// Acknowledges the highest-priority pending Level 1 interrupt and returns its vector number:
    /// - `vector_base | 0`: FDC
    /// - `vector_base | 1`: FDD
    /// - `vector_base | 2`: HDC
    /// - `vector_base | 3`: PRN
    pub fn acknowledge_interrupt(&mut self) -> Option<u8> {
        if (self.signal & Self::SIG_FDC) != 0 && (self.enable & Self::EN_FDC) != 0 {
            Some(self.vector_base)
        } else if (self.signal & Self::SIG_FDD) != 0 && (self.enable & Self::EN_FDD) != 0 {
            Some(self.vector_base | 1)
        } else if (self.signal & Self::SIG_HDC) != 0 && (self.enable & Self::EN_HDC) != 0 {
            Some(self.vector_base | 2)
        } else if (self.signal & Self::SIG_PRN) != 0 && (self.enable & Self::EN_PRN) != 0 {
            self.enable &= !Self::EN_PRN;
            Some(self.vector_base | 3)
        } else {
            None
        }
    }

    /// Reads an 8-bit byte from the I/O Interrupt Controller (`$E9C000`–`$E9DFFF`).
    pub fn read_u8(&self, addr: u32) -> Result<u8, BusFault> {
        let a = addr & 0x00FF_FFFF;
        if (a & 1) == 0 {
            return Err(BusFault::BusError {
                addr: a,
                is_write: false,
            });
        }
        match a & 0x0F {
            0x01 => Ok((self.signal & 0xF0) | (self.enable & 0x0F)),
            0x03 => Ok(0xFF),
            _ => Err(BusFault::BusError {
                addr: a,
                is_write: false,
            }),
        }
    }

    /// Writes an 8-bit byte to the I/O Interrupt Controller (`$E9C000`–`$E9DFFF`).
    pub fn write_u8(&mut self, addr: u32, val: u8) -> Result<(), BusFault> {
        let a = addr & 0x00FF_FFFF;
        if (a & 1) == 0 {
            return Err(BusFault::BusError {
                addr: a,
                is_write: true,
            });
        }
        match a & 0x0F {
            0x01 => {
                self.enable = val & 0x0F;
                Ok(())
            }
            0x03 => {
                self.vector_base = val & 0xFC;
                Ok(())
            }
            _ => Err(BusFault::BusError {
                addr: a,
                is_write: true,
            }),
        }
    }
}

/// Geometry descriptor for a mounted floppy disk image.
#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub struct FloppyGeometry {
    pub cylinders: u8,
    pub heads: u8,
    pub sectors_per_track: u8,
    pub sector_size_code: u8, // N: sector_bytes = 128 << N
    pub first_sector_id: u8,  // Typically 1
}

impl FloppyGeometry {
    /// Standard X68000 2HD `.XDF` format (`1,261,568` bytes: 77 cyl × 2 heads × 8 sectors × 1024 bytes).
    pub const XDF_2HD: Self = Self {
        cylinders: 77,
        heads: 2,
        sectors_per_track: 8,
        sector_size_code: 3,
        first_sector_id: 1,
    };

    /// Infers floppy geometry from raw image byte length.
    pub fn from_image_len(len: usize) -> Option<Self> {
        match len {
            1_261_568 => Some(Self::XDF_2HD),
            1_419_264 => Some(Self {
                cylinders: 77,
                heads: 2,
                sectors_per_track: 9,
                sector_size_code: 3,
                first_sector_id: 1,
            }),
            1_474_560 => Some(Self {
                cylinders: 80,
                heads: 2,
                sectors_per_track: 18,
                sector_size_code: 2,
                first_sector_id: 1,
            }),
            737_280 => Some(Self {
                cylinders: 80,
                heads: 2,
                sectors_per_track: 9,
                sector_size_code: 2,
                first_sector_id: 1,
            }),
            655_360 => Some(Self {
                cylinders: 80,
                heads: 2,
                sectors_per_track: 8,
                sector_size_code: 2,
                first_sector_id: 1,
            }),
            _ => None,
        }
    }

    /// Expands a `.DIM` image (2HD, 2HC, or 2HQ) into the raw sector image of its geometry.
    ///
    /// A DIM image is a 256-byte header followed by track data. Header byte 0 is the media type,
    /// and bytes 1.. flag the tracks present; absent tracks may be left out of the data, and they
    /// expand to unformatted-looking `$E5` filler.
    pub fn expand_dim(data: &[u8]) -> Option<(Self, Vec<u8>)> {
        const HEADER_BYTES: usize = 256;
        let geometry = match *data.first()? {
            0 => Self::XDF_2HD,
            2 => Self {
                cylinders: 80,
                heads: 2,
                sectors_per_track: 15,
                sector_size_code: 2,
                first_sector_id: 1,
            },
            9 => Self {
                cylinders: 80,
                heads: 2,
                sectors_per_track: 18,
                sector_size_code: 2,
                first_sector_id: 1,
            },
            _ => return None,
        };
        let tracks = geometry.cylinders as usize * geometry.heads as usize;
        let track_bytes = geometry.sectors_per_track as usize * geometry.sector_bytes();
        let present = data.get(1..1 + tracks)?;
        let present_count = present.iter().filter(|&&flag| flag != 0).count();
        let body = data.get(HEADER_BYTES..)?;
        let absent_stored = if body.len() == tracks * track_bytes {
            true
        } else if body.len() == present_count * track_bytes {
            false
        } else {
            return None;
        };
        let mut image = vec![0xE5; tracks * track_bytes];
        let mut src = 0;
        for (track, &flag) in present.iter().enumerate() {
            if flag != 0 {
                image[track * track_bytes..][..track_bytes]
                    .copy_from_slice(&body[src..src + track_bytes]);
            }
            if flag != 0 || absent_stored {
                src += track_bytes;
            }
        }
        Some((geometry, image))
    }

    #[inline]
    pub const fn sector_bytes(&self) -> usize {
        128usize << self.sector_size_code
    }

    /// Computes the byte offset of sector `(c, h, r, n)` within the raw disk image.
    pub fn sector_offset(&self, c: u8, h: u8, r: u8, n: u8) -> Option<usize> {
        if c >= self.cylinders || h >= self.heads || n != self.sector_size_code {
            return None;
        }
        if r < self.first_sector_id || r >= self.first_sector_id + self.sectors_per_track {
            return None;
        }
        let track_idx = (c as usize) * (self.heads as usize) + (h as usize);
        let sec_idx = track_idx * (self.sectors_per_track as usize)
            + ((r - self.first_sector_id) as usize);
        Some(sec_idx * self.sector_bytes())
    }
}

/// State of a single Floppy Disk Drive unit (`0..=3`).
#[derive(Debug, Clone)]
pub struct FloppyDrive {
    /// Whether this physical drive unit is connected (`true` for drives 0 and 1 on X68000).
    pub connected: bool,
    /// Present Cylinder Number (`0..79`).
    pub pcn: u8,
    /// Last rotated sector index for `READ ID`.
    pub id_cursor: u8,
    /// Drive motor active (`$E94007` bit 7).
    pub motor_on: bool,
    /// Write-protect tab state.
    pub write_protected: bool,
    /// Raw disk image data (`None` if no floppy is inserted).
    pub image: Option<Vec<u8>>,
    /// An image kept by the master VM instead (`docs/VMX68K.md` Section 3.1).
    pub remote: Option<RemoteDisk>,
    /// Geometry of the mounted disk image.
    pub geometry: FloppyGeometry,
    /// Pending Seek/Recalibrate or Ready-transition interrupt result `(st0, pcn)`.
    pub pending_irq: Option<(u8, u8)>,
}

impl FloppyDrive {
    pub fn new(connected: bool) -> Self {
        Self {
            connected,
            pcn: 0,
            id_cursor: 0,
            motor_on: false,
            write_protected: false,
            image: None,
            remote: None,
            geometry: FloppyGeometry::XDF_2HD,
            pending_irq: None,
        }
    }

    /// Returns `true` if a disk image is currently inserted.
    #[inline]
    pub const fn is_inserted(&self) -> bool {
        self.image.is_some() || self.remote.is_some()
    }

    /// Returns `true` if the drive is ready (connected, disk inserted, and motor spinning).
    #[inline]
    pub const fn is_ready(&self) -> bool {
        self.connected && self.is_inserted() && self.motor_on
    }
}

/// Bytes a read or write starting at sector `r` of side `h` can transfer: to `eot`, then the
/// other side too for a multi-track command on side 0. They lie together in the image.
fn command_span(geom: FloppyGeometry, mt: bool, h: u8, r: u8, eot: u8) -> usize {
    let last = eot.min(geom.first_sector_id + geom.sectors_per_track - 1);
    let mut sectors = (last as usize + 1).saturating_sub(r as usize);
    if mt && h & 1 == 0 {
        sectors += (last as usize + 1).saturating_sub(geom.first_sector_id as usize);
    }
    sectors * geom.sector_bytes()
}

/// NEC uPD72065 Floppy Disk Controller and 4-Drive FDD subsystem (`$E94000`–`$E95FFF`).
#[derive(Debug, Clone)]
pub struct Fdc {
    /// Four drive units (`0` and `1` connected by default, `2` and `3` disconnected).
    pub drives: [FloppyDrive; 4],
    /// Enforced Ready signal from OPM `CT2` (YM2151 register `$1B` bit 6).
    pub enforced_ready: bool,
    /// Drive status selector written to `$E94005` (`bits 3..0`).
    drive_ctrl_select: u8,
    /// Command buffer during C-Phase.
    cmd_buf: [u8; 10],
    cmd_len: usize,
    cmd_expected: usize,
    /// Result buffer during R-Phase.
    res_buf: [u8; 8],
    res_len: usize,
    res_pos: usize,
    /// Standby status flag (`RESET STANDBY` / `SET STANDBY`).
    standby: bool,
    /// The machine's drive accesses, counted for its access lamp (see
    /// [`crate::bus::X68kBus::drive_access_counter`]).
    pub(crate) drive_accesses: Arc<AtomicU64>,
}

impl Default for Fdc {
    fn default() -> Self {
        Self::new()
    }
}

impl Fdc {
    pub fn new() -> Self {
        Self {
            drives: [
                FloppyDrive::new(true),
                FloppyDrive::new(true),
                FloppyDrive::new(false),
                FloppyDrive::new(false),
            ],
            enforced_ready: false,
            drive_ctrl_select: 0,
            cmd_buf: [0; 10],
            cmd_len: 0,
            cmd_expected: 0,
            res_buf: [0; 8],
            res_len: 0,
            res_pos: 0,
            standby: false,
            drive_accesses: Arc::default(),
        }
    }

    pub fn reset(&mut self, ioi: &mut IoInterruptController) {
        self.cmd_len = 0;
        self.cmd_expected = 0;
        self.res_len = 0;
        self.res_pos = 0;
        for drv in &mut self.drives {
            drv.pending_irq = None;
        }
        ioi.set_fdc_signal(false);
    }

    /// Inserts a floppy disk image (`.XDF` / `.2HD` / `.2DD` / `.DIM`) into drive `unit` (`0..=3`).
    pub fn insert_disk(&mut self, unit: usize, data: Vec<u8>, write_protected: bool) -> bool {
        if unit >= 4 {
            return false;
        }
        let (geometry, data) = match FloppyGeometry::from_image_len(data.len()) {
            Some(geometry) => (geometry, data),
            None => match FloppyGeometry::expand_dim(&data) {
                Some(expanded) => expanded,
                None => return false,
            },
        };
        let drv = &mut self.drives[unit];
        drv.connected = true;
        drv.geometry = geometry;
        drv.write_protected = write_protected;
        drv.image = Some(data);
        true
    }

    /// Inserts a disk image the master VM keeps into drive `unit`, which reads and writes it
    /// through requests. Its size must be one of a raw floppy image.
    pub fn insert_remote_disk(&mut self, unit: usize, disk: RemoteDisk, write_protected: bool) -> bool {
        let Some(geometry) = FloppyGeometry::from_image_len(disk.len()) else {
            return false;
        };
        let Some(drv) = self.drives.get_mut(unit) else {
            return false;
        };
        drv.connected = true;
        drv.geometry = geometry;
        drv.write_protected = write_protected;
        drv.image = None;
        drv.remote = Some(disk);
        true
    }

    /// Ejects the floppy disk from drive `unit` (`0..=3`).
    pub fn eject_disk(&mut self, unit: usize) {
        if unit < 4 {
            self.drives[unit].image = None;
            self.drives[unit].remote = None;
            self.drives[unit].motor_on = false;
        }
    }

    fn update_fdc_irq(&self, ioi: &mut IoInterruptController) {
        let any_unit_irq = self.drives.iter().any(|d| d.pending_irq.is_some());
        let r_phase_irq = self.res_pos < self.res_len && self.res_pos == 0;
        ioi.set_fdc_signal(any_unit_irq || r_phase_irq);
    }

    fn expected_command_bytes(first_byte: u8) -> usize {
        match first_byte & 0x1F {
            0x03 => 3, // SPECIFY
            0x04 => 2, // SENSE DEVICE STATUS
            0x05 => 9, // WRITE DATA
            0x06 => 9, // READ DATA
            0x07 => 2, // RECALIBRATE
            0x08 => 1, // SENSE INTERRUPT STATUS
            0x09 => 9, // WRITE DELETED DATA
            0x0A => 2, // READ ID
            0x0C => 9, // READ DELETED DATA
            0x0D => 6, // WRITE ID (FORMAT)
            0x0F => 3, // SEEK
            0x11 | 0x19 | 0x1D => 9, // SCAN EQUAL / LOW OR EQUAL / HIGH OR EQUAL
            0x14..=0x16 => 1,        // RESET STANDBY / SET STANDBY / SOFTWARE RESET
            _ => 1,
        }
    }

    fn enter_result_phase(&mut self, results: &[u8], assert_irq: bool, ioi: &mut IoInterruptController) {
        let n = results.len().min(self.res_buf.len());
        self.res_buf[..n].copy_from_slice(&results[..n]);
        self.res_len = n;
        self.res_pos = 0;
        self.cmd_len = 0;
        self.cmd_expected = 0;
        if assert_irq {
            ioi.set_fdc_signal(true);
        }
    }

    fn execute_command(&mut self, dmac: &mut Dmac, main_ram: &mut [u8], ioi: &mut IoInterruptController) {
        let cmd = self.cmd_buf[0];
        let op = cmd & 0x1F;
        self.cmd_len = 0;
        self.cmd_expected = 0;
        // Commands that reach a disk light the access lamp: reads, writes, READ ID, FORMAT,
        // and the scans.
        if matches!(op, 0x02 | 0x05 | 0x06 | 0x09 | 0x0A | 0x0C | 0x0D | 0x11 | 0x19 | 0x1D) {
            self.drive_accesses.fetch_add(1, Ordering::Relaxed);
        }

        match op {
            0x03 => {
                // SPECIFY (SRT/HUT, HLT/ND) — no result phase
            }
            0x04 => {
                // SENSE DEVICE STATUS
                let us = (self.cmd_buf[1] & 0x03) as usize;
                let hd = (self.cmd_buf[1] >> 2) & 0x01;
                let drv = &self.drives[us];
                let mut st3 = (hd << 2) | (us as u8) | ST3_TS;
                if drv.pcn == 0 {
                    st3 |= ST3_T0;
                }
                if drv.is_ready() || (drv.connected && self.enforced_ready && drv.is_inserted()) {
                    st3 |= ST3_RY;
                }
                if drv.write_protected {
                    st3 |= ST3_WP;
                }
                self.enter_result_phase(&[st3], false, ioi);
            }
            0x07 => {
                // RECALIBRATE
                let us = (self.cmd_buf[1] & 0x03) as usize;
                let drv = &mut self.drives[us];
                if !drv.connected {
                    drv.pending_irq = Some((ST0_AT | ST0_SE | ST0_EC | (us as u8), 0));
                } else if drv.is_ready() || self.enforced_ready || drv.is_inserted() {
                    drv.pcn = 0;
                    drv.pending_irq = Some((ST0_SE | (us as u8), 0));
                } else {
                    drv.pending_irq = Some((ST0_AT | ST0_SE | ST0_NR | (us as u8), drv.pcn));
                }
                self.update_fdc_irq(ioi);
            }
            0x0F => {
                // SEEK
                let us = (self.cmd_buf[1] & 0x03) as usize;
                let hd = (self.cmd_buf[1] >> 2) & 0x01;
                let ncn = self.cmd_buf[2];
                let drv = &mut self.drives[us];
                if !drv.connected {
                    drv.pending_irq =
                        Some((ST0_AT | ST0_SE | ST0_EC | (hd << 2) | (us as u8), 0));
                } else if drv.is_ready() || self.enforced_ready || drv.is_inserted() {
                    drv.pcn = ncn;
                    drv.pending_irq = Some((ST0_SE | (hd << 2) | (us as u8), drv.pcn));
                } else {
                    drv.pending_irq =
                        Some((ST0_AT | ST0_SE | ST0_NR | (hd << 2) | (us as u8), drv.pcn));
                }
                self.update_fdc_irq(ioi);
            }
            0x08 => {
                // SENSE INTERRUPT STATUS
                let mut found: Option<(u8, u8)> = None;
                for drv in &mut self.drives {
                    if let Some(pair) = drv.pending_irq.take() {
                        found = Some(pair);
                        break;
                    }
                }
                if let Some((st0, pcn)) = found {
                    self.enter_result_phase(&[st0, pcn], false, ioi);
                } else {
                    self.enter_result_phase(&[ST0_IC], false, ioi);
                }
                self.update_fdc_irq(ioi);
            }
            0x0A => {
                // READ ID
                let us = (self.cmd_buf[1] & 0x03) as usize;
                let hd = (self.cmd_buf[1] >> 2) & 0x01;
                let drv = &mut self.drives[us];
                if !drv.is_inserted() {
                    let pcn = drv.pcn;
                    let st0 = ST0_AT | ST0_NR | (hd << 2) | (us as u8);
                    self.enter_result_phase(&[st0, 0, 0, pcn, hd, 1, 3], true, ioi);
                } else {
                    let geom = drv.geometry;
                    let r = geom.first_sector_id + (drv.id_cursor % geom.sectors_per_track);
                    drv.id_cursor = drv.id_cursor.wrapping_add(1);
                    let c = drv.pcn;
                    let n = geom.sector_size_code;
                    let st0 = (hd << 2) | (us as u8);
                    self.enter_result_phase(&[st0, 0, 0, c, hd, r, n], true, ioi);
                }
            }
            0x06 | 0x0C => {
                // READ DATA / READ DELETED DATA
                let mt = (cmd & 0x80) != 0;
                let us = (self.cmd_buf[1] & 0x03) as usize;
                let mut c = self.cmd_buf[2];
                let mut h = self.cmd_buf[3];
                let mut r = self.cmd_buf[4];
                let n = self.cmd_buf[5];
                let eot = self.cmd_buf[6];

                let drv = &mut self.drives[us];
                drv.pcn = c;
                let geom = drv.geometry;
                let sec_len = geom.sector_bytes();
                // A remote image is asked for every sector the command can transfer at once,
                // and the VM waits for it here, where the sectors go to the DMAC.
                let (image, base): (std::borrow::Cow<[u8]>, usize) = match (&drv.image, &drv.remote) {
                    (Some(image), _) => (std::borrow::Cow::Borrowed(&image[..]), 0),
                    (None, Some(remote)) => {
                        let fetched = geom.sector_offset(c, h, r, n).and_then(|first| {
                            let len = command_span(geom, mt, h, r, eot).min(remote.len() - first);
                            remote.read(first, len).map(|data| (data, first))
                        });
                        let Some((data, first)) = fetched else {
                            let st0 = ST0_AT | ((h & 1) << 2) | (us as u8);
                            self.enter_result_phase(&[st0, ST1_ND, 0, c, h, r, n], true, ioi);
                            return;
                        };
                        (std::borrow::Cow::Owned(data), first)
                    }
                    (None, None) => {
                        let st0 = ST0_AT | ST0_NR | ((h & 1) << 2) | (us as u8);
                        self.enter_result_phase(&[st0, 0, 0, c, h, r, n], true, ioi);
                        return;
                    }
                };

                loop {
                    let Some(offset) = geom.sector_offset(c, h, r, n).and_then(|o| o.checked_sub(base))
                    else {
                        let st0 = ST0_AT | ((h & 1) << 2) | (us as u8);
                        self.enter_result_phase(&[st0, ST1_ND, 0, c, h, r, n], true, ioi);
                        return;
                    };
                    if offset + sec_len > image.len() {
                        let st0 = ST0_AT | ((h & 1) << 2) | (us as u8);
                        self.enter_result_phase(&[st0, ST1_ND, 0, c, h, r, n], true, ioi);
                        return;
                    }

                    // Transfer sector bytes via DMAC Channel 0
                    for &byte in &image[offset..offset + sec_len] {
                        if !dmac.device_to_memory_u8(0, byte, main_ram) {
                            break;
                        }
                    }

                    let dmac_active = dmac.channels[0].is_active();
                    if r < eot {
                        r = r.wrapping_add(1);
                    } else if mt && (h & 1) == 0 {
                        h ^= 1;
                        r = 1;
                    } else {
                        if mt {
                            h ^= 1;
                        }
                        c = c.wrapping_add(1);
                        r = 1;
                        break;
                    }

                    if !dmac_active {
                        break;
                    }
                }

                let st0 = ((h & 1) << 2) | (us as u8);
                self.enter_result_phase(&[st0, 0, 0, c, h, r, n], true, ioi);
            }
            0x05 | 0x09 => {
                // WRITE DATA / WRITE DELETED DATA
                let mt = (cmd & 0x80) != 0;
                let us = (self.cmd_buf[1] & 0x03) as usize;
                let mut c = self.cmd_buf[2];
                let mut h = self.cmd_buf[3];
                let mut r = self.cmd_buf[4];
                let n = self.cmd_buf[5];
                let eot = self.cmd_buf[6];

                let drv = &mut self.drives[us];
                drv.pcn = c;
                if drv.write_protected {
                    let st0 = ST0_AT | ((h & 1) << 2) | (us as u8);
                    self.enter_result_phase(&[st0, ST1_NW, 0, c, h, r, n], true, ioi);
                    return;
                }
                let geom = drv.geometry;
                let sec_len = geom.sector_bytes();
                // A remote image is written in one request once the DMAC has given every
                // sector, and the result phase waits for it.
                let remote = drv.remote.clone();
                let mut written = Vec::new();
                let first = geom.sector_offset(c, h, r, n);
                let (image, base): (&mut [u8], usize) = match (drv.image.as_mut(), &remote, first) {
                    (Some(image), _, _) => (&mut image[..], 0),
                    (None, Some(remote), Some(first)) => {
                        let len = command_span(geom, mt, h, r, eot).min(remote.len() - first);
                        written.resize(len, 0);
                        (&mut written[..], first)
                    }
                    (None, Some(_), None) => {
                        let st0 = ST0_AT | ((h & 1) << 2) | (us as u8);
                        self.enter_result_phase(&[st0, ST1_ND, 0, c, h, r, n], true, ioi);
                        return;
                    }
                    (None, None, _) => {
                        let st0 = ST0_AT | ST0_NR | ((h & 1) << 2) | (us as u8);
                        self.enter_result_phase(&[st0, 0, 0, c, h, r, n], true, ioi);
                        return;
                    }
                };
                let mut written_len = 0;

                loop {
                    let Some(offset) = geom.sector_offset(c, h, r, n).and_then(|o| o.checked_sub(base)) else {
                        let st0 = ST0_AT | ((h & 1) << 2) | (us as u8);
                        self.enter_result_phase(&[st0, ST1_ND, 0, c, h, r, n], true, ioi);
                        return;
                    };
                    if offset + sec_len > image.len() {
                        let st0 = ST0_AT | ((h & 1) << 2) | (us as u8);
                        self.enter_result_phase(&[st0, ST1_ND, 0, c, h, r, n], true, ioi);
                        return;
                    }

                    for (i, slot) in image[offset..offset + sec_len].iter_mut().enumerate() {
                        if let Some(byte) = dmac.memory_to_device_u8(0, main_ram) {
                            *slot = byte;
                            written_len = written_len.max(offset + i + 1);
                        } else {
                            break;
                        }
                    }

                    let dmac_active = dmac.channels[0].is_active();
                    if r < eot {
                        r = r.wrapping_add(1);
                    } else if mt && (h & 1) == 0 {
                        h ^= 1;
                        r = 1;
                    } else {
                        if mt {
                            h ^= 1;
                        }
                        c = c.wrapping_add(1);
                        r = 1;
                        break;
                    }

                    if !dmac_active {
                        break;
                    }
                }

                if let (Some(remote), Some(first)) = (&remote, first) {
                    written.truncate(written_len);
                    if !written.is_empty() && !remote.write(first, written) {
                        let st0 = ST0_AT | ((h & 1) << 2) | (us as u8);
                        self.enter_result_phase(&[st0, ST1_NW, 0, c, h, r, n], true, ioi);
                        return;
                    }
                }
                let st0 = ((h & 1) << 2) | (us as u8);
                self.enter_result_phase(&[st0, 0, 0, c, h, r, n], true, ioi);
            }
            0x14 => {
                // RESET STANDBY
                self.standby = false;
            }
            0x15 => {
                // SET STANDBY
                self.standby = true;
            }
            0x16 => {
                // SOFTWARE RESET
                self.reset(ioi);
            }
            _ => {
                // Invalid command -> 1 result byte (ST0_IC = 0x80)
                self.enter_result_phase(&[ST0_IC], false, ioi);
            }
        }
    }

    /// Reads an 8-bit byte from the FDC / FDD I/O range (`$E94000`–`$E95FFF`).
    pub fn read_u8(
        &mut self,
        addr: u32,
        ioi: &mut IoInterruptController,
    ) -> Result<u8, BusFault> {
        let a = addr & 0x00FF_FFFF;
        if (a & 1) == 0 {
            return Err(BusFault::BusError {
                addr: a,
                is_write: false,
            });
        }
        match a & 0x07 {
            0x01 => {
                // Main Status Register (MSR)
                let in_r_phase = self.res_pos < self.res_len;
                let busy = in_r_phase || self.cmd_len > 0;
                let mut msr = MSR_RQM;
                if in_r_phase {
                    msr |= MSR_DIO;
                }
                if busy {
                    msr |= MSR_CB;
                }
                Ok(msr)
            }
            0x03 => {
                // Data Register (R-Phase read)
                if self.res_pos < self.res_len {
                    if self.res_pos == 0 {
                        ioi.set_fdc_signal(false);
                    }
                    let val = self.res_buf[self.res_pos];
                    self.res_pos += 1;
                    if self.res_pos >= self.res_len {
                        self.res_len = 0;
                        self.res_pos = 0;
                        self.update_fdc_irq(ioi);
                    }
                    Ok(val)
                } else {
                    Ok(0x80)
                }
            }
            0x05 => {
                // Drive Option Status: bit 7 = 1 if selected unit has a disk inserted
                let mut status = 0u8;
                for (u, drv) in self.drives.iter().enumerate() {
                    if (self.drive_ctrl_select & (1 << u)) != 0 && drv.is_inserted() {
                        status |= 0x80;
                    }
                }
                Ok(status)
            }
            0x07 => Ok(0xFF),
            _ => Ok(0xFF),
        }
    }

    /// Writes an 8-bit byte to the FDC / FDD I/O range (`$E94000`–`$E95FFF`).
    pub fn write_u8(
        &mut self,
        addr: u32,
        val: u8,
        dmac: &mut Dmac,
        main_ram: &mut [u8],
        ioi: &mut IoInterruptController,
    ) -> Result<(), BusFault> {
        let a = addr & 0x00FF_FFFF;
        if (a & 1) == 0 {
            return Err(BusFault::BusError {
                addr: a,
                is_write: true,
            });
        }
        match a & 0x07 {
            0x01 | 0x03 => {
                // Command / Parameter byte write
                if self.cmd_len == 0 {
                    self.cmd_expected = Self::expected_command_bytes(val);
                }
                if self.cmd_len < self.cmd_buf.len() {
                    self.cmd_buf[self.cmd_len] = val;
                    self.cmd_len += 1;
                }
                if self.cmd_len >= self.cmd_expected {
                    self.execute_command(dmac, main_ram, ioi);
                }
            }
            0x05 => {
                // Drive Option Control
                ioi.set_fdd_signal(false);
                self.drive_ctrl_select = val & 0x0F;
            }
            0x07 => {
                // Drive Select & Motor Control
                ioi.set_fdd_signal(false);
                let motor_on = (val & 0x80) != 0;
                let us = (val & 0x03) as usize;
                let drv = &mut self.drives[us];
                let was_ready = drv.is_ready();
                drv.motor_on = motor_on;
                let now_ready = drv.is_ready();
                if !was_ready && now_ready {
                    // Ready rising transition triggers an Attention Interrupt (ST0_AI = 0xC0 | us)
                    drv.pending_irq = Some((ST0_AI | (us as u8), drv.pcn));
                    self.update_fdc_irq(ioi);
                }
            }
            _ => {}
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn expand_dim_fills_absent_tracks() {
        let track_bytes = 8 * 1024;
        let mut dim = vec![0u8; 256];
        dim[1..1 + 154].fill(1);
        dim[1 + 3] = 0;
        for track in (0..154).filter(|&t| t != 3) {
            dim.extend(std::iter::repeat_n(track as u8, track_bytes));
        }

        let (geometry, image) = FloppyGeometry::expand_dim(&dim).expect("2HD DIM");
        assert_eq!(geometry, FloppyGeometry::XDF_2HD);
        assert_eq!(image.len(), 1_261_568);
        assert!(image[2 * track_bytes..][..track_bytes].iter().all(|&b| b == 2));
        assert!(image[3 * track_bytes..][..track_bytes].iter().all(|&b| b == 0xE5));
        assert!(image[153 * track_bytes..].iter().all(|&b| b == 153));
    }

    #[test]
    fn expand_dim_rejects_truncated_data() {
        let mut dim = vec![0u8; 256];
        dim[1..1 + 154].fill(1);
        dim.extend(std::iter::repeat_n(0, 1_261_568 - 1));
        assert!(FloppyGeometry::expand_dim(&dim).is_none());
    }
}
