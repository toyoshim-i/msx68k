//! SX-Window OS integration, Mixed Mode hypercall dispatcher, and dynamic display/mouse synchronization (`msx68k-bus`).

use msx68k_cpu::{CpuState, TrapResult};
use crate::bus::X68kBus;

/// Reserved F-Line Mixed Mode hypercall IDs (`$FE80`..=`$FE8F`).
#[derive(Debug, Copy, Clone, PartialEq, Eq)]
#[repr(u16)]
pub enum HypercallId {
    /// Synchronize SX-Window mouse coordinates (`A4` = `MouseWorkArea` pointer) with host mouse position.
    SxMouseSync = 0xFE81,
    /// Boot from attached SCSI HDD (`$FC0000`).
    ScsiRomBoot = 0xFE82,
    /// `HUMAN.SYS` `romdb` block driver builder (`JSR (A3)` at `*(P-16)`).
    ScsiRomdbBuild = 0xFE83,
    /// `IOCS $F5` vector installer (`JSR (A0)` at `*(P-12)`).
    ScsiIocsInstall = 0xFE84,
    /// `IOCS $F5` (`_SCSIDRV`) subfunction dispatcher.
    ScsiIocsDispatch = 0xFE85,
    /// `romdb` block driver Strategy routine (`A5` = request packet).
    ScsiDriverStrategy = 0xFE86,
    /// `romdb` block driver Interrupt routine (`A5` = request packet).
    ScsiDriverInterrupt = 0xFE87,
    /// Dynamic resolution & screen mode switch (Step 1: up to 1024x1024 full display).
    SxSetScreenMode = 0xFE88,
}

impl HypercallId {
    /// Decodes an opcode word into a [`HypercallId`] if in range `$FE80`..=`$FE8F`.
    pub fn from_opcode(opcode: u16) -> Option<Self> {
        match opcode {
            0xFE81 => Some(Self::SxMouseSync),
            0xFE82 => Some(Self::ScsiRomBoot),
            0xFE83 => Some(Self::ScsiRomdbBuild),
            0xFE84 => Some(Self::ScsiIocsInstall),
            0xFE85 => Some(Self::ScsiIocsDispatch),
            0xFE86 => Some(Self::ScsiDriverStrategy),
            0xFE87 => Some(Self::ScsiDriverInterrupt),
            0xFE88 => Some(Self::SxSetScreenMode),
            _ => None,
        }
    }
}

impl X68kBus {
    /// Primary Mixed Mode Hypercall Dispatcher for F-Line opcodes `$FE80`..=`$FE8F`.
    pub fn handle_hypercall(
        &mut self,
        state: &mut CpuState,
        opcode: u16,
        _pc: u32,
    ) -> TrapResult {
        let Some(hypercall) = HypercallId::from_opcode(opcode) else {
            return TrapResult::Unhandled;
        };

        match hypercall {
            HypercallId::SxMouseSync => self.hypercall_sx_mouse_sync(state),
            HypercallId::ScsiRomBoot => self.hypercall_scsi_rom_boot(state),
            HypercallId::ScsiRomdbBuild => self.hypercall_scsi_romdb_build(state),
            HypercallId::ScsiIocsInstall => self.hypercall_scsi_iocs_install(state),
            HypercallId::ScsiIocsDispatch => self.hypercall_scsi_iocs_dispatch(state),
            HypercallId::ScsiDriverStrategy => self.hypercall_scsi_driver_strategy(state),
            HypercallId::ScsiDriverInterrupt => self.hypercall_scsi_driver_interrupt(state),
            HypercallId::SxSetScreenMode => self.hypercall_sx_set_screen_mode(state),
        }
    }

    /// Sets the current host OS mouse position in emulator display coordinates.
    pub fn set_host_mouse_position(&mut self, pos: Option<(i16, i16)>) {
        self.host_mouse_pos = pos;
    }

    /// Returns the current host OS mouse position if inside the emulator window.
    pub fn host_mouse_position(&self) -> Option<(i16, i16)> {
        self.host_mouse_pos
    }

    /// Verifies and applies the declarative module patch to `SYS/FSX.X` when the mouse completion
    /// vector at `$0000_0938` is written.
    pub fn check_fsx_mouse_hook_patch(&mut self) {
        if self.main_ram.len() < 0x093C {
            return;
        }
        let handler_addr = u32::from_be_bytes([
            self.main_ram[0x0938],
            self.main_ram[0x0939],
            self.main_ram[0x093A],
            self.main_ram[0x093B],
        ]);
        if handler_addr > 0x04F82A {
            let text_base = handler_addr - 0x04F82A;
            let patch_addr = (text_base + 0x04F768) as usize;
            if patch_addr + 4 <= self.main_ram.len()
                && self.main_ram[patch_addr..patch_addr + 4] == [0x48, 0xE7, 0xE0, 0x04]
            {
                // Patch MOVEM.L D0-D2/A5, -(SP) with Hypercall 0xFE81 (SxMouseSync) + RTS (0x4E75)
                self.main_ram[patch_addr..patch_addr + 4]
                    .copy_from_slice(&[0xFE, 0x81, 0x4E, 0x75]);

                if self.target_resolution.is_none()
                    && (self.sram.get(0x7E).copied().unwrap_or(0) & 0x3F) == 0x30
                {
                    self.load_disk_config_if_present();
                }

                // If a non-default target resolution is configured, apply it to FSX.X now
                if let Some((w, h)) = self.target_resolution {
                    self.apply_screen_resolution(w, h);
                }
            }
        }
    }

    /// Handles `0xFE81` (`SxMouseSync`): updates SX-Window mouse coordinates in `MouseWorkArea` (`A4`).
    pub(crate) fn hypercall_sx_mouse_sync(&mut self, state: &mut CpuState) -> TrapResult {
        let a4 = (state.a[4] & 0x00FF_FFFF) as usize;
        if a4 + 0x3C <= self.main_ram.len() {
            let min_x = i16::from_be_bytes([self.main_ram[a4 + 0x34], self.main_ram[a4 + 0x35]]);
            let min_y = i16::from_be_bytes([self.main_ram[a4 + 0x36], self.main_ram[a4 + 0x37]]);
            let max_x = i16::from_be_bytes([self.main_ram[a4 + 0x38], self.main_ram[a4 + 0x39]]);
            let max_y = i16::from_be_bytes([self.main_ram[a4 + 0x3A], self.main_ram[a4 + 0x3B]]);

            let limit_x = (max_x.saturating_sub(1)).max(min_x);
            let limit_y = (max_y.saturating_sub(1)).max(min_y);

            if let Some((hx, hy)) = self.host_mouse_pos {
                let cx = hx.clamp(min_x, limit_x);
                let cy = hy.clamp(min_y, limit_y);
                self.main_ram[a4 + 0x0A..a4 + 0x0C].copy_from_slice(&(cx as u16).to_be_bytes());
                self.main_ram[a4 + 0x0C..a4 + 0x0E].copy_from_slice(&(cy as u16).to_be_bytes());
                self.main_ram[a4 + 0x1A..a4 + 0x1C].copy_from_slice(&0u16.to_be_bytes());
                self.main_ram[a4 + 0x1C..a4 + 0x1E].copy_from_slice(&0u16.to_be_bytes());
            } else {
                let cur_x = i16::from_be_bytes([self.main_ram[a4 + 0x0A], self.main_ram[a4 + 0x0B]]);
                let cur_y = i16::from_be_bytes([self.main_ram[a4 + 0x0C], self.main_ram[a4 + 0x0D]]);
                let dx = i16::from_be_bytes([self.main_ram[a4 + 0x16], self.main_ram[a4 + 0x17]]);
                let dy = i16::from_be_bytes([self.main_ram[a4 + 0x18], self.main_ram[a4 + 0x19]]);

                let cx = cur_x.saturating_add(dx).clamp(min_x, limit_x);
                let cy = cur_y.saturating_add(dy).clamp(min_y, limit_y);
                self.main_ram[a4 + 0x0A..a4 + 0x0C].copy_from_slice(&(cx as u16).to_be_bytes());
                self.main_ram[a4 + 0x0C..a4 + 0x0E].copy_from_slice(&(cy as u16).to_be_bytes());
            }
        }
        TrapResult::Handled { extra_cycles: 0 }
    }

    /// Handles `0xFE88` (`SxSetScreenMode`): queries or switches SX-Window display resolution
    /// (Mode 0: 768x512, Mode 1: 1024x768, Mode 2: 1024x1024).
    pub(crate) fn hypercall_sx_set_screen_mode(&mut self, state: &mut CpuState) -> TrapResult {
        let (cur_w, cur_h) = self.crtc.active_resolution();
        let cur_mode: u32 = match (cur_w, cur_h) {
            (1024, 1024) => 2,
            (1024, 768) => 1,
            _ => 0,
        };

        let req_mode = state.d[1] as i32;
        if req_mode == -1 {
            // Query mode: return current mode and resolution
            state.d[0] = cur_mode;
            state.d[1] = cur_w;
            state.d[2] = cur_h;
            return TrapResult::Handled { extra_cycles: 0 };
        }

        let (target_w, target_h, target_mode) = match req_mode {
            1 => (1024, 768, 1),
            2 => (1024, 1024, 2),
            0 => (768, 512, 0),
            _ => {
                let w = state.d[1];
                let h = state.d[2];
                if w >= 1024 && h >= 1024 {
                    (1024, 1024, 2)
                } else if w >= 1024 && h >= 768 {
                    (1024, 768, 1)
                } else {
                    (768, 512, 0)
                }
            }
        };

        self.apply_screen_resolution(target_w, target_h);
        state.d[0] = target_mode;
        state.d[1] = target_w;
        state.d[2] = target_h;
        TrapResult::Handled { extra_cycles: 0 }
    }

    /// Intercepts `IOCS _CRTMOD` (`TRAP #15`, `D0.B == $10`) to support Extended Mode `$50`
    /// (1:1 Native Resolution & Beyond 1K×1K modes) and coordinate dynamic screen switches.
    pub fn handle_iocs_crtmod(&mut self, state: &mut CpuState) -> TrapResult {
        let d1_w = (state.d[1] & 0xFFFF) as u16;

        // Case 1: Standard Query (D1.W == -1)
        if d1_w == 0xFFFF {
            if self.is_extended_crtmod_active() {
                if self.main_ram.len() > 0x93C {
                    self.main_ram[0x93C] = 0x50;
                }
                state.d[0] = 0x50;
                return TrapResult::Handled { extra_cycles: 0 };
            }
            return TrapResult::PassToVector;
        }

        // Case 2: Extended msx68k Mode $50 (e.g. D1.W == 0x0050 or 0x0150)
        if (d1_w & 0xFF) == 0x50 {
            if state.d[2] == 0xFFFF_FFFF {
                // Subcase 2a: Extended Query (D2.L == -1)
                let (cur_w, cur_h) = self.crtc.active_resolution();
                let (nat_w, nat_h) = self.host_native_resolution;
                let cur_mode = if self.is_extended_crtmod_active() {
                    0x50
                } else if self.main_ram.len() > 0x93C && self.main_ram[0x93C] != 0 {
                    self.main_ram[0x93C] as u32
                } else {
                    16
                };
                state.d[0] = cur_mode;
                state.d[1] = ((cur_w & 0xFFFF) << 16) | (cur_h & 0xFFFF);
                state.d[2] = if self.fullscreen_mode { 1 } else { 0 };
                state.d[3] = ((nat_w & 0xFFFF) << 16) | (nat_h & 0xFFFF);
                return TrapResult::Handled { extra_cycles: 0 };
            } else if state.d[2] == 0 {
                // Subcase 2b: Host Native Fullscreen 1:1 mode
                let (nat_w, nat_h) = self.host_native_resolution;
                self.fullscreen_mode = true;
                self.apply_screen_resolution(nat_w, nat_h);
                if self.main_ram.len() > 0x93C {
                    self.main_ram[0x93C] = 0x50;
                }
                state.d[0] = 0;
                return TrapResult::Handled { extra_cycles: 0 };
            } else {
                // Subcase 2c: Windowed 1:1 mode (w = D2.L >> 16, h = D2.L & 0xFFFF)
                let req_w = (state.d[2] >> 16) & 0xFFFF;
                let req_h = state.d[2] & 0xFFFF;
                self.fullscreen_mode = false;
                self.apply_screen_resolution(req_w, req_h);
                let (act_w, act_h) = self.crtc.active_resolution();
                if self.main_ram.len() > 0x93C {
                    self.main_ram[0x93C] = if (act_w, act_h) == (768, 512) {
                        16
                    } else {
                        0x50
                    };
                }
                state.d[0] = 0;
                return TrapResult::Handled { extra_cycles: 0 };
            }
        }

        // Case 3: SYS/FSX.X booting with $00ED007E == $B0 (D1.B == $30)
        if (d1_w & 0xFF) == 0x30 {
            // Rewrite D1.B to 0x10 so ROM _CRTMOD initializes Mode 16 (768x512) without rejecting $30 > 19
            state.d[1] = (state.d[1] & 0xFFFF_FF00) | 0x10;
            return TrapResult::PassToVector;
        }

        // Case 4: Standard Mode 16 (D1.B == $10) called at runtime after desktop is initialized
        if (d1_w & 0xFF) == 0x10 && self.main_ram.len() >= 0x0940 {
            let iocs_mouse_vec = u32::from_be_bytes([
                self.main_ram[0x938],
                self.main_ram[0x939],
                self.main_ram[0x93A],
                self.main_ram[0x93B],
            ]);
            if iocs_mouse_vec > 0x04F82A {
                let text_base = (iocs_mouse_vec - 0x04F82A) as usize;
                let dt_win_ptr_addr = text_base + 0x54384;
                let dt_win = if dt_win_ptr_addr + 4 <= self.main_ram.len() {
                    u32::from_be_bytes([
                        self.main_ram[dt_win_ptr_addr],
                        self.main_ram[dt_win_ptr_addr + 1],
                        self.main_ram[dt_win_ptr_addr + 2],
                        self.main_ram[dt_win_ptr_addr + 3],
                    ])
                } else {
                    0
                };
                if dt_win != 0 {
                    self.fullscreen_mode = false;
                    self.apply_screen_resolution(768, 512);
                    if self.main_ram.len() > 0x93C {
                        self.main_ram[0x93C] = 16;
                    }
                }
            }
        }

        TrapResult::PassToVector
    }

    /// Scans SCSI HDD 0 for `/SYS/MSX68K.CFG` or `/MSX68K.CFG` and applies display configuration.
    pub fn load_disk_config_if_present(&mut self) {
        let Some(disk) = self.scsi.hdd_data(0) else {
            return;
        };

        let (start_rec, _rec_count, sector_bytes, sec_per_clus, root_entries, fat_sectors) =
            crate::scsi::detect_partition_params(disk);

        let boot_off = start_rec * sector_bytes;
        let fat_bytes = (fat_sectors as usize) * sector_bytes;
        let fat1_off = boot_off + sector_bytes;
        let fat2_off = fat1_off + fat_bytes;
        let root_dir_off = fat2_off + fat_bytes;
        let root_dir_bytes = (root_entries as usize) * 32;
        let data_start_off = root_dir_off + root_dir_bytes;
        let cluster_bytes = (sec_per_clus as usize) * sector_bytes;

        if root_dir_off + root_dir_bytes > disk.len() {
            return;
        }

        // Helper to find file entry in a directory byte slice
        let find_file = |dir_slice: &[u8], name: &[u8; 8], ext: &[u8; 3]| -> Option<(usize, usize)> {
            for chunk in dir_slice.chunks_exact(32) {
                if chunk[0] == 0 {
                    break;
                }
                if chunk[0] == 0xE5 || chunk[0] == 0x2E {
                    continue;
                }
                if &chunk[0..8] == name && &chunk[8..11] == ext {
                    let mut start_c = u16::from_le_bytes([chunk[26], chunk[27]]) as usize;
                    if start_c < 2 {
                        start_c = u16::from_be_bytes([chunk[26], chunk[27]]) as usize;
                    }
                    let mut size =
                        u32::from_le_bytes([chunk[28], chunk[29], chunk[30], chunk[31]]) as usize;
                    if size == 0 || size > 65536 {
                        size = u32::from_be_bytes([chunk[28], chunk[29], chunk[30], chunk[31]])
                            as usize;
                    }
                    return Some((start_c, size));
                }
            }
            None
        };

        // 1. Try finding MSX68K.CFG in root directory
        let mut target_file = find_file(
            &disk[root_dir_off..root_dir_off + root_dir_bytes],
            b"MSX68K  ",
            b"CFG",
        );

        // 2. If not found in root, look for SYS directory and search inside it
        if target_file.is_none() {
            for chunk in disk[root_dir_off..root_dir_off + root_dir_bytes].chunks_exact(32) {
                if chunk[0] == 0 {
                    break;
                }
                if chunk[0] == 0xE5 || chunk[0] == 0x2E {
                    continue;
                }
                if (chunk[11] & 0x10) != 0 && &chunk[0..8] == b"SYS     " {
                    let mut sys_c = u16::from_le_bytes([chunk[26], chunk[27]]) as usize;
                    if sys_c < 2 {
                        sys_c = u16::from_be_bytes([chunk[26], chunk[27]]) as usize;
                    }
                    if sys_c >= 2 {
                        let sys_off = data_start_off + (sys_c - 2) * cluster_bytes;
                        if sys_off + cluster_bytes <= disk.len() {
                            target_file = find_file(
                                &disk[sys_off..sys_off + cluster_bytes],
                                b"MSX68K  ",
                                b"CFG",
                            );
                        }
                    }
                    break;
                }
            }
        }

        let Some((cluster, size)) = target_file else {
            return;
        };

        if cluster < 2 {
            return;
        }

        let file_off = data_start_off + (cluster - 2) * cluster_bytes;
        let read_len = size.min(cluster_bytes);
        if file_off + read_len > disk.len() {
            return;
        }

        let cfg_data = &disk[file_off..file_off + read_len];
        let mut cfg_w: Option<u32> = None;
        let mut cfg_h: Option<u32> = None;
        let mut cfg_fullscreen: Option<bool> = None;

        if let Ok(text) = std::str::from_utf8(cfg_data) {
            for line in text.lines() {
                let line = line.trim();
                if let Some(rest) = line.strip_prefix("SCREEN_WIDTH=") {
                    if let Ok(val) = rest.trim().parse::<u32>() {
                        cfg_w = Some(val);
                    }
                } else if let Some(rest) = line.strip_prefix("SCREEN_HEIGHT=") {
                    if let Ok(val) = rest.trim().parse::<u32>() {
                        cfg_h = Some(val);
                    }
                } else if let Some(rest) = line.strip_prefix("FULLSCREEN=") {
                    let val = rest.trim();
                    if val == "1" || val.eq_ignore_ascii_case("true") {
                        cfg_fullscreen = Some(true);
                    } else if val == "0" || val.eq_ignore_ascii_case("false") {
                        cfg_fullscreen = Some(false);
                    }
                }
            }
        }

        if cfg_fullscreen == Some(true) {
            self.fullscreen_mode = true;
            self.target_resolution = Some(self.host_native_resolution);
        } else if let (Some(w), Some(h)) = (cfg_w, cfg_h) {
            if w >= 256 && h >= 256 {
                self.fullscreen_mode = false;
                self.target_resolution = Some((w, h));
            }
        }
    }

    /// Dynamically applies a screen resolution preset (up to 3840×2160) to the CRTC,
    /// planar framebuffer (standard TVRAM or EXT_TVRAM at $1000_0000), battery-backed SRAM settings,
    /// and resident SYS/FSX.X kernel structures.
    pub fn apply_screen_resolution(&mut self, width: u32, height: u32) {
        let width = ((width.clamp(256, 3840) + 15) & !15).clamp(256, 3840);
        let height = height.clamp(256, 2160);
        self.target_resolution = Some((width, height));

        let (old_w, old_h) = self.crtc.active_resolution();
        let old_was_ext = self.is_ext_tvram_active();
        let old_row_bytes = self.active_tvram_row_bytes();
        let old_plane_bytes = self.active_tvram_plane_bytes();

        let use_ext = width > 1024 || height > 1024;
        let new_row_bytes = if use_ext {
            (width as usize).div_ceil(16) * 2
        } else {
            128
        };
        let new_plane_bytes = if use_ext {
            new_row_bytes * (height as usize)
        } else {
            crate::crtc::TVRAM_PLANE_BYTES
        };
        let new_base_addr = if use_ext {
            crate::bus::EXT_TVRAM_BASE
        } else {
            0x00E0_0000
        };

        // 1. Program CRTC registers
        let h_cols = (width / 8) as u16;
        self.crtc.regs[2] = 0;
        self.crtc.regs[3] = h_cols;
        self.crtc.regs[4] = (height as u16) + 32;
        self.crtc.regs[5] = 5;
        self.crtc.regs[6] = 0;
        self.crtc.regs[7] = height as u16;
        self.crtc.regs[10] = 0; // Text scroll X = 0
        self.crtc.regs[11] = 0; // Text scroll Y = 0
        if height > 512 {
            self.crtc.regs[20] = 0x001F; // 31kHz high-res mode (>512 lines)
        } else {
            self.crtc.regs[20] = 0x0416; // 31kHz standard mode (768x512)
        }

        // 2. Manage Framebuffer buffers and copy existing scanlines
        let old_buf: Option<Vec<u8>> = if old_was_ext {
            self.ext_tvram.take()
        } else {
            self.tvram().map(|s| s.to_vec())
        };
        let had_drawn_pixels = old_buf.as_ref().is_some_and(|b| b.iter().any(|&x| x != 0));

        if use_ext {
            let mut new_buf = vec![0u8; 4 * new_plane_bytes];
            if let Some(ref src) = old_buf {
                let copy_rows = (old_h as usize).min(height as usize);
                let copy_bytes = old_row_bytes.min(new_row_bytes);
                for plane in 0..4 {
                    let src_base = plane * old_plane_bytes;
                    let dst_base = plane * new_plane_bytes;
                    for r in 0..copy_rows {
                        let s_off = src_base + r * old_row_bytes;
                        let d_off = dst_base + r * new_row_bytes;
                        if s_off + copy_bytes <= src.len() && d_off + copy_bytes <= new_buf.len() {
                            new_buf[d_off..d_off + copy_bytes]
                                .copy_from_slice(&src[s_off..s_off + copy_bytes]);
                        }
                    }
                }
            }
            self.ext_tvram = Some(new_buf);
            self.ext_tvram_row_bytes = new_row_bytes;
            self.ext_tvram_plane_bytes = new_plane_bytes;
        } else {
            if old_was_ext {
                if let Some(ref src) = old_buf {
                    let copy_rows = (old_h as usize).min(height as usize).min(1024);
                    let copy_bytes = old_row_bytes.min(128);
                    let tv = self.tvram_mut();
                    for plane in 0..4 {
                        let src_base = plane * old_plane_bytes;
                        let dst_base = plane * crate::crtc::TVRAM_PLANE_BYTES;
                        for r in 0..copy_rows {
                            let s_off = src_base + r * old_row_bytes;
                            let d_off = dst_base + r * 128;
                            if s_off + copy_bytes <= src.len() && d_off + copy_bytes <= tv.len() {
                                tv[d_off..d_off + copy_bytes]
                                    .copy_from_slice(&src[s_off..s_off + copy_bytes]);
                            }
                        }
                    }
                }
            }
            self.ext_tvram = None;
            self.ext_tvram_row_bytes = 0;
            self.ext_tvram_plane_bytes = 0;
        }

        // 3. Update battery-backed SRAM ($ED0072..$ED007E)
        if self.sram.len() >= 0x80 {
            self.sram[0x72] = b'S';
            self.sram[0x73] = b'X';
            let vmode = if width >= 1024 && height >= 1024 {
                6
            } else if width >= 1024 && height >= 768 {
                4
            } else {
                3
            };
            self.sram[0x7C] = (vmode << 4) | 0x03;
            if (width, height) == (768, 512) && !self.fullscreen_mode {
                self.sram[0x7E] = 0x80 | 16;
            } else {
                self.sram[0x7E] = 0x80 | 0x30;
            }
        }

        // 4. Patch resident SYS/FSX.X kernel data structures if loaded in Main RAM
        if self.main_ram.len() >= 0x0940 {
            let iocs_mouse_vec = u32::from_be_bytes([
                self.main_ram[0x938],
                self.main_ram[0x939],
                self.main_ram[0x93A],
                self.main_ram[0x93B],
            ]);
            if iocs_mouse_vec > 0x04F82A {
                let text_base = (iocs_mouse_vec - 0x04F82A) as usize;
                if text_base + 0x65000 <= self.main_ram.len() {
                    let w_u16 = width as u16;
                    let h_u16 = height as u16;
                    let w_be = w_u16.to_be_bytes();
                    let h_be = h_u16.to_be_bytes();

                    // Helpers for safely reading pointers and updating kernel structures
                    let read_ptr = |ram: &[u8], addr: usize| -> usize {
                        if addr + 4 <= ram.len() {
                            u32::from_be_bytes([
                                ram[addr],
                                ram[addr + 1],
                                ram[addr + 2],
                                ram[addr + 3],
                            ]) as usize
                        } else {
                            0
                        }
                    };

                    let update_rect_wh = |ram: &mut [u8], addr: usize, w: u16, h: u16| {
                        if addr + 8 <= ram.len() {
                            ram[addr..addr + 2].copy_from_slice(&0u16.to_be_bytes());
                            ram[addr + 2..addr + 4].copy_from_slice(&0u16.to_be_bytes());
                            ram[addr + 4..addr + 6].copy_from_slice(&w.to_be_bytes());
                            ram[addr + 6..addr + 8].copy_from_slice(&h.to_be_bytes());
                        }
                    };

                    let update_planar_bmap = |ram: &mut [u8],
                                              bmap_addr: usize,
                                              w: u16,
                                              h: u16,
                                              base_addr: u32,
                                              row_bytes: u16,
                                              plane_bytes: u32| {
                        if bmap_addr > 0 && bmap_addr + 20 <= ram.len() {
                            ram[bmap_addr + 6..bmap_addr + 8].copy_from_slice(&w.to_be_bytes());
                            ram[bmap_addr + 8..bmap_addr + 10].copy_from_slice(&h.to_be_bytes());
                            ram[bmap_addr + 10..bmap_addr + 14]
                                .copy_from_slice(&base_addr.to_be_bytes());
                            ram[bmap_addr + 14..bmap_addr + 16]
                                .copy_from_slice(&row_bytes.to_be_bytes());
                            ram[bmap_addr + 16..bmap_addr + 20]
                                .copy_from_slice(&plane_bytes.to_be_bytes());
                        }
                    };

                    let update_rgn_handle = |ram: &mut [u8],
                                             rgn_h_addr: usize,
                                             w: u16,
                                             h: u16,
                                             check_larger_only: bool| {
                        let rgn_h = if rgn_h_addr + 4 <= ram.len() {
                            u32::from_be_bytes([
                                ram[rgn_h_addr],
                                ram[rgn_h_addr + 1],
                                ram[rgn_h_addr + 2],
                                ram[rgn_h_addr + 3],
                            ]) as usize
                        } else {
                            0
                        };
                        if rgn_h > 0 && rgn_h + 4 <= ram.len() {
                            let rgn_ptr = u32::from_be_bytes([
                                ram[rgn_h],
                                ram[rgn_h + 1],
                                ram[rgn_h + 2],
                                ram[rgn_h + 3],
                            ]) as usize;
                            if rgn_ptr > 0 && rgn_ptr + 12 <= ram.len() {
                                let size = u32::from_be_bytes([
                                    ram[rgn_ptr],
                                    ram[rgn_ptr + 1],
                                    ram[rgn_ptr + 2],
                                    ram[rgn_ptr + 3],
                                ]);
                                if size >= 12 {
                                    if check_larger_only {
                                        let cur_r = i16::from_be_bytes([
                                            ram[rgn_ptr + 8],
                                            ram[rgn_ptr + 9],
                                        ]);
                                        if cur_r > 0 && cur_r < w as i16 {
                                            ram[rgn_ptr + 8..rgn_ptr + 10]
                                                .copy_from_slice(&w.to_be_bytes());
                                        }
                                        let cur_b = i16::from_be_bytes([
                                            ram[rgn_ptr + 10],
                                            ram[rgn_ptr + 11],
                                        ]);
                                        if cur_b > 0 && cur_b < h as i16 {
                                            ram[rgn_ptr + 10..rgn_ptr + 12]
                                                .copy_from_slice(&h.to_be_bytes());
                                        }
                                    } else {
                                        ram[rgn_ptr..rgn_ptr + 4]
                                            .copy_from_slice(&12u32.to_be_bytes());
                                        ram[rgn_ptr + 8..rgn_ptr + 10]
                                            .copy_from_slice(&w.to_be_bytes());
                                        ram[rgn_ptr + 10..rgn_ptr + 12]
                                            .copy_from_slice(&h.to_be_bytes());
                                    }
                                }
                            }
                        }
                    };

                    // Global Screen Rect at text_base + 0x54372: (0, 0, width, height)
                    self.main_ram[text_base + 0x54372 + 4..text_base + 0x54372 + 6]
                        .copy_from_slice(&w_be);
                    self.main_ram[text_base + 0x54372 + 6..text_base + 0x54372 + 8]
                        .copy_from_slice(&h_be);

                    // Max window dimensions at text_base + 0x543B6: (width - 2, height - 24)
                    let max_w = w_u16.saturating_sub(2);
                    let max_h = h_u16.saturating_sub(24);
                    self.main_ram[text_base + 0x543B6..text_base + 0x543B8]
                        .copy_from_slice(&max_w.to_be_bytes());
                    self.main_ram[text_base + 0x543B8..text_base + 0x543BA]
                        .copy_from_slice(&max_h.to_be_bytes());

                    // ActiveScreenModeRecord at text_base + 0x55278
                    update_planar_bmap(
                        &mut self.main_ram,
                        text_base + 0x55278,
                        w_u16,
                        h_u16,
                        new_base_addr,
                        new_row_bytes as u16,
                        new_plane_bytes as u32,
                    );

                    // Bitmap copy at text_base + 0x5528E
                    update_planar_bmap(
                        &mut self.main_ram,
                        text_base + 0x5528E,
                        w_u16,
                        h_u16,
                        new_base_addr,
                        new_row_bytes as u16,
                        new_plane_bytes as u32,
                    );

                    // Graphic Bitmap template at text_base + 0x552A4
                    let grp_bmap = text_base + 0x552A4;
                    self.main_ram[grp_bmap + 6..grp_bmap + 8].copy_from_slice(&w_be);
                    self.main_ram[grp_bmap + 8..grp_bmap + 10].copy_from_slice(&h_be);

                    // Default GrafPort rect template at text_base + 0x552BA
                    let gp_tmpl = text_base + 0x552BA;
                    self.main_ram[gp_tmpl + 4..gp_tmpl + 6].copy_from_slice(&w_be);
                    self.main_ram[gp_tmpl + 6..gp_tmpl + 8].copy_from_slice(&h_be);

                    // Mode table presets:
                    // Mode 3 (768x512) at text_base + 0x55338
                    update_planar_bmap(
                        &mut self.main_ram,
                        text_base + 0x55338,
                        w_u16,
                        h_u16,
                        new_base_addr,
                        new_row_bytes as u16,
                        new_plane_bytes as u32,
                    );
                    // Mode 4 (1024x768) at text_base + 0x5534E
                    update_planar_bmap(
                        &mut self.main_ram,
                        text_base + 0x5534E,
                        w_u16,
                        h_u16,
                        new_base_addr,
                        new_row_bytes as u16,
                        new_plane_bytes as u32,
                    );
                    // Mode 6 (1024x1024) at text_base + 0x5537A
                    update_planar_bmap(
                        &mut self.main_ram,
                        text_base + 0x5537A,
                        w_u16,
                        h_u16,
                        new_base_addr,
                        new_row_bytes as u16,
                        new_plane_bytes as u32,
                    );

                    // Physical CRT resolution table entry 16 at text_base + 0x15200
                    let crt16 = text_base + 0x15200;
                    self.main_ram[crt16..crt16 + 2].copy_from_slice(&w_be);
                    self.main_ram[crt16 + 2..crt16 + 4].copy_from_slice(&h_be);

                    // Clear text scroll offsets in FSX.X
                    self.main_ram[text_base + 0x15364..text_base + 0x15368]
                        .copy_from_slice(&[0, 0, 0, 0]);
                    self.main_ram[text_base + 0x5525A..text_base + 0x5525E]
                        .copy_from_slice(&[0, 0, 0, 0]);

                    // Desktop GrafPort (desktopGrafPort = *(text_base + 0x5437A))
                    let desktop_gp = read_ptr(&self.main_ram, text_base + 0x5437A);
                    if desktop_gp > 0 && desktop_gp + 0x20 <= self.main_ram.len() {
                        update_rect_wh(&mut self.main_ram, desktop_gp + 4, w_u16, h_u16);
                        let bmap = read_ptr(&self.main_ram, desktop_gp);
                        update_planar_bmap(
                            &mut self.main_ram,
                            bmap,
                            w_u16,
                            h_u16,
                            new_base_addr,
                            new_row_bytes as u16,
                            new_plane_bytes as u32,
                        );
                        update_rgn_handle(
                            &mut self.main_ram,
                            desktop_gp + 0x10,
                            w_u16,
                            h_u16,
                            false,
                        );
                        update_rgn_handle(
                            &mut self.main_ram,
                            desktop_gp + 0x14,
                            w_u16,
                            h_u16,
                            true,
                        );
                    }

                    // Desktop Window (desktopWindow = *(text_base + 0x54384))
                    let desktop_win = read_ptr(&self.main_ram, text_base + 0x54384);
                    if desktop_win > 0 && desktop_win + 0x90 <= self.main_ram.len() {
                        update_rect_wh(&mut self.main_ram, desktop_win + 4, w_u16, h_u16);
                        let bmap = read_ptr(&self.main_ram, desktop_win);
                        update_planar_bmap(
                            &mut self.main_ram,
                            bmap,
                            w_u16,
                            h_u16,
                            new_base_addr,
                            new_row_bytes as u16,
                            new_plane_bytes as u32,
                        );
                        update_rgn_handle(
                            &mut self.main_ram,
                            desktop_win + 0x10,
                            w_u16,
                            h_u16,
                            false,
                        );
                        update_rgn_handle(
                            &mut self.main_ram,
                            desktop_win + 0x14,
                            w_u16,
                            h_u16,
                            true,
                        );
                        update_rgn_handle(
                            &mut self.main_ram,
                            desktop_win + 0x48,
                            w_u16,
                            h_u16,
                            false,
                        );
                        update_rgn_handle(
                            &mut self.main_ram,
                            desktop_win + 0x4C,
                            w_u16,
                            h_u16,
                            false,
                        );
                        update_rgn_handle(
                            &mut self.main_ram,
                            desktop_win + 0x88,
                            w_u16,
                            h_u16,
                            false,
                        );
                        update_rgn_handle(
                            &mut self.main_ram,
                            desktop_win + 0x8C,
                            w_u16,
                            h_u16,
                            false,
                        );

                        // Open Window Linked List
                        let mut curr_win = read_ptr(&self.main_ram, desktop_win + 0x80);
                        let mut count = 0;
                        while curr_win > 0 && curr_win + 0x6A <= self.main_ram.len() && count < 64
                        {
                            let bmap = read_ptr(&self.main_ram, curr_win);
                            if bmap > 0 && bmap + 20 <= self.main_ram.len() {
                                let cur_base = u32::from_be_bytes([
                                    self.main_ram[bmap + 10],
                                    self.main_ram[bmap + 11],
                                    self.main_ram[bmap + 12],
                                    self.main_ram[bmap + 13],
                                ]);
                                if cur_base == 0x00E0_0000 || cur_base == crate::bus::EXT_TVRAM_BASE
                                {
                                    update_planar_bmap(
                                        &mut self.main_ram,
                                        bmap,
                                        w_u16,
                                        h_u16,
                                        new_base_addr,
                                        new_row_bytes as u16,
                                        new_plane_bytes as u32,
                                    );
                                }
                            }
                            curr_win = read_ptr(&self.main_ram, curr_win + 0x66);
                            count += 1;
                        }
                    }

                    // Menubar GrafPort pointed by dtgs + 0x46:
                    let dtgs = text_base + 0x54380;
                    let menu_gp = read_ptr(&self.main_ram, dtgs + 0x46);
                    if menu_gp > 0 && menu_gp + 0x10 <= self.main_ram.len() {
                        self.main_ram[menu_gp + 8..menu_gp + 10].copy_from_slice(&w_be);
                        let menu_bm = read_ptr(&self.main_ram, menu_gp);
                        if menu_bm > 0 && menu_bm + 20 <= self.main_ram.len() {
                            let menu_h = u16::from_be_bytes([
                                self.main_ram[menu_bm + 8],
                                self.main_ram[menu_bm + 9],
                            ]);
                            update_planar_bmap(
                                &mut self.main_ram,
                                menu_bm,
                                w_u16,
                                menu_h,
                                new_base_addr,
                                new_row_bytes as u16,
                                new_plane_bytes as u32,
                            );
                        }
                    }

                    // Patch GMNewBits (text_base + 0x02F23A) and GMUnlockBits (text_base + 0x02F32E) immediates
                    if text_base + 0x02F23E <= self.main_ram.len()
                        && self.main_ram[text_base + 0x02F238..text_base + 0x02F23A] == [0x2D, 0x7C]
                    {
                        self.main_ram[text_base + 0x02F23A..text_base + 0x02F23E]
                            .copy_from_slice(&new_base_addr.to_be_bytes());
                    }
                    if text_base + 0x02F332 <= self.main_ram.len()
                        && (self.main_ram[text_base + 0x02F32C..text_base + 0x02F32E]
                            == [0x21, 0x7C]
                            || self.main_ram[text_base + 0x02F32C..text_base + 0x02F32E]
                                == [0x20, 0x3C])
                    {
                        self.main_ram[text_base + 0x02F32E..text_base + 0x02F332]
                            .copy_from_slice(&new_base_addr.to_be_bytes());
                    }

                    // Update MouseWorkArea if pointer at text_base + 0x633CA is valid
                    let a4_ptr_addr = text_base + 0x633CA;
                    let a4 = read_ptr(&self.main_ram, a4_ptr_addr);
                    if a4 + 0x3C <= self.main_ram.len() {
                        self.main_ram[a4 + 0x34..a4 + 0x36].copy_from_slice(&0u16.to_be_bytes());
                        self.main_ram[a4 + 0x36..a4 + 0x38].copy_from_slice(&0u16.to_be_bytes());
                        self.main_ram[a4 + 0x38..a4 + 0x3A].copy_from_slice(&w_be);
                        self.main_ram[a4 + 0x3A..a4 + 0x3C].copy_from_slice(&h_be);

                        // Clamp mouse position if outside new bounds
                        let cur_x = u16::from_be_bytes([
                            self.main_ram[a4 + 0x0A],
                            self.main_ram[a4 + 0x0B],
                        ])
                        .min((width - 1) as u16);
                        let cur_y = u16::from_be_bytes([
                            self.main_ram[a4 + 0x0C],
                            self.main_ram[a4 + 0x0D],
                        ])
                        .min((height - 1) as u16);
                        self.main_ram[a4 + 0x0A..a4 + 0x0C].copy_from_slice(&cur_x.to_be_bytes());
                        self.main_ram[a4 + 0x0C..a4 + 0x0E].copy_from_slice(&cur_y.to_be_bytes());
                        self.main_ram[text_base + 0x549F6..text_base + 0x549F8]
                            .copy_from_slice(&cur_x.to_be_bytes());
                        self.main_ram[text_base + 0x549F8..text_base + 0x549FA]
                            .copy_from_slice(&cur_y.to_be_bytes());
                    }
                }
            }
        }

        // 5. Fill extended TVRAM areas (desktop stipple pattern sampled from top-right corner)
        let new_w = width as usize;
        let new_h = height as usize;
        let fill_w = (old_w as usize).min(new_w);
        let fill_h = (old_h as usize).min(new_h);
        if had_drawn_pixels && (new_w > fill_w || new_h > fill_h) {
            let stride = new_row_bytes;
            let plane_stride = new_plane_bytes;
            let tv: &mut [u8] = if use_ext {
                self.ext_tvram.as_deref_mut().unwrap()
            } else {
                self.tvram_mut()
            };
            for plane in 0..4 {
                let plane_base = plane * plane_stride;
                let mut tile = [[0u8; 2]; 32]; // 16 rows * 2 cols
                for ty in 0..16 {
                    for tc in 0..2 {
                        let src_off = plane_base + ty * stride + (46 + tc) * 2;
                        if src_off + 2 <= tv.len() {
                            tile[ty * 2 + tc] = [tv[src_off], tv[src_off + 1]];
                        }
                    }
                }
                // Fill horizontal extension (x = fill_w..new_w, y = 0..fill_h)
                for y in 0..fill_h {
                    let ty = y % 16;
                    for col in (fill_w / 16)..(new_w / 16) {
                        let word = tile[ty * 2 + (col % 2)];
                        let dst_off = plane_base + y * stride + col * 2;
                        if dst_off + 2 <= tv.len() {
                            tv[dst_off..dst_off + 2].copy_from_slice(&word);
                        }
                    }
                }
                // Fill vertical extension (x = 0..new_w, y = fill_h..new_h)
                for y in fill_h..new_h {
                    let ty = y % 16;
                    for col in 0..(new_w / 16) {
                        let word = tile[ty * 2 + (col % 2)];
                        let dst_off = plane_base + y * stride + col * 2;
                        if dst_off + 2 <= tv.len() {
                            tv[dst_off..dst_off + 2].copy_from_slice(&word);
                        }
                    }
                }
            }
        }
    }
}
