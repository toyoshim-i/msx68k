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

    /// Dynamically applies a screen resolution preset (up to 1024x1024) to the CRTC,
    /// battery-backed SRAM settings ($ED0072..$ED007E), and resident SYS/FSX.X kernel structures.
    pub fn apply_screen_resolution(&mut self, width: u32, height: u32) {
        let width = width.clamp(256, 1024);
        let height = height.clamp(256, 1024);
        self.target_resolution = Some((width, height));

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

        // 2. Update battery-backed SRAM ($ED0072..$ED007E)
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
            // bit 7 = 1 (configured), bit 6 = 0 (scroll disabled), bits 0..4 = 16 (CRT mode 16)
            self.sram[0x7E] = 0x80 | 16;
        }

        // 3. Patch resident SYS/FSX.X kernel data structures if loaded in Main RAM
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
                            u32::from_be_bytes([ram[addr], ram[addr + 1], ram[addr + 2], ram[addr + 3]]) as usize
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

                    let update_bmap = |ram: &mut [u8], bmap_addr: usize, w: u16, h: u16| {
                        if bmap_addr > 0 && bmap_addr + 10 <= ram.len() {
                            ram[bmap_addr + 6..bmap_addr + 8].copy_from_slice(&w.to_be_bytes());
                            ram[bmap_addr + 8..bmap_addr + 10].copy_from_slice(&h.to_be_bytes());
                        }
                    };

                    let update_rgn_handle = |ram: &mut [u8], rgn_h_addr: usize, w: u16, h: u16, check_larger_only: bool| {
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
                                        let cur_r = i16::from_be_bytes([ram[rgn_ptr + 8], ram[rgn_ptr + 9]]);
                                        if cur_r > 0 && cur_r < w as i16 {
                                            ram[rgn_ptr + 8..rgn_ptr + 10].copy_from_slice(&w.to_be_bytes());
                                        }
                                        let cur_b = i16::from_be_bytes([ram[rgn_ptr + 10], ram[rgn_ptr + 11]]);
                                        if cur_b > 0 && cur_b < h as i16 {
                                            ram[rgn_ptr + 10..rgn_ptr + 12].copy_from_slice(&h.to_be_bytes());
                                        }
                                    } else {
                                        ram[rgn_ptr..rgn_ptr + 4].copy_from_slice(&12u32.to_be_bytes());
                                        ram[rgn_ptr + 8..rgn_ptr + 10].copy_from_slice(&w.to_be_bytes());
                                        ram[rgn_ptr + 10..rgn_ptr + 12].copy_from_slice(&h.to_be_bytes());
                                    }
                                }
                            }
                        }
                    };

                    // 1. Global Screen Rects & Templates
                    // Screen Rect at text_base + 0x54372: (0, 0, width, height)
                    self.main_ram[text_base + 0x54372 + 4..text_base + 0x54372 + 6].copy_from_slice(&w_be);
                    self.main_ram[text_base + 0x54372 + 6..text_base + 0x54372 + 8].copy_from_slice(&h_be);

                    // Max window dimensions at text_base + 0x543B6: (width - 2, height - 24)
                    let max_w = w_u16.saturating_sub(2);
                    let max_h = h_u16.saturating_sub(24);
                    self.main_ram[text_base + 0x543B6..text_base + 0x543B8].copy_from_slice(&max_w.to_be_bytes());
                    self.main_ram[text_base + 0x543B8..text_base + 0x543BA].copy_from_slice(&max_h.to_be_bytes());

                    // ActiveScreenModeRecord at text_base + 0x55278
                    let active_rec = text_base + 0x55278;
                    self.main_ram[active_rec + 6..active_rec + 8].copy_from_slice(&w_be);
                    self.main_ram[active_rec + 8..active_rec + 10].copy_from_slice(&h_be);

                    // Bitmap copy at text_base + 0x5528E
                    let bmap_copy = text_base + 0x5528E;
                    self.main_ram[bmap_copy + 6..bmap_copy + 8].copy_from_slice(&w_be);
                    self.main_ram[bmap_copy + 8..bmap_copy + 10].copy_from_slice(&h_be);

                    // Graphic Bitmap template at text_base + 0x552A4
                    let grp_bmap = text_base + 0x552A4;
                    self.main_ram[grp_bmap + 6..grp_bmap + 8].copy_from_slice(&w_be);
                    self.main_ram[grp_bmap + 8..grp_bmap + 10].copy_from_slice(&h_be);

                    // Default GrafPort rect template at text_base + 0x552BA
                    let gp_tmpl = text_base + 0x552BA;
                    self.main_ram[gp_tmpl + 4..gp_tmpl + 6].copy_from_slice(&w_be);
                    self.main_ram[gp_tmpl + 6..gp_tmpl + 8].copy_from_slice(&h_be);

                    // Mode table entries:
                    // Mode 3 (768x512) at text_base + 0x55338
                    let m3 = text_base + 0x55338;
                    self.main_ram[m3 + 6..m3 + 8].copy_from_slice(&w_be);
                    self.main_ram[m3 + 8..m3 + 10].copy_from_slice(&h_be);

                    // Mode 4 (1024x424 / 1024x768) at text_base + 0x5534E
                    let m4 = text_base + 0x5534E;
                    self.main_ram[m4 + 6..m4 + 8].copy_from_slice(&w_be);
                    self.main_ram[m4 + 8..m4 + 10].copy_from_slice(&h_be);

                    // Mode 6 (1024x1024) at text_base + 0x5537A
                    let m6 = text_base + 0x5537A;
                    self.main_ram[m6 + 6..m6 + 8].copy_from_slice(&w_be);
                    self.main_ram[m6 + 8..m6 + 10].copy_from_slice(&h_be);

                    // Physical CRT resolution table entry 16 at text_base + 0x15200
                    let crt16 = text_base + 0x15200;
                    self.main_ram[crt16..crt16 + 2].copy_from_slice(&w_be);
                    self.main_ram[crt16 + 2..crt16 + 4].copy_from_slice(&h_be);

                    // Clear text scroll offsets in FSX.X
                    self.main_ram[text_base + 0x15364..text_base + 0x15368]
                        .copy_from_slice(&[0, 0, 0, 0]);
                    self.main_ram[text_base + 0x5525A..text_base + 0x5525E]
                        .copy_from_slice(&[0, 0, 0, 0]);

                    // 2. Desktop GrafPort (desktopGrafPort = *(text_base + 0x5437A))
                    let desktop_gp = read_ptr(&self.main_ram, text_base + 0x5437A);
                    if desktop_gp > 0 && desktop_gp + 0x20 <= self.main_ram.len() {
                        // desktopGrafPort + 0x04..0x0C (rect): (0, 0, width, height)
                        update_rect_wh(&mut self.main_ram, desktop_gp + 4, w_u16, h_u16);
                        // bmap = *(desktopGrafPort + 0x00)
                        let bmap = read_ptr(&self.main_ram, desktop_gp);
                        update_bmap(&mut self.main_ram, bmap, w_u16, h_u16);
                        // vis_h = *(desktopGrafPort + 0x10)
                        update_rgn_handle(&mut self.main_ram, desktop_gp + 0x10, w_u16, h_u16, false);
                        // clip_h = *(desktopGrafPort + 0x14)
                        update_rgn_handle(&mut self.main_ram, desktop_gp + 0x14, w_u16, h_u16, true);
                    }

                    // 3. Desktop Window (desktopWindow = *(text_base + 0x54384))
                    let desktop_win = read_ptr(&self.main_ram, text_base + 0x54384);
                    if desktop_win > 0 && desktop_win + 0x90 <= self.main_ram.len() {
                        // desktopWindow + 0x04..0x0C (rect): (0, 0, width, height)
                        update_rect_wh(&mut self.main_ram, desktop_win + 4, w_u16, h_u16);
                        // bmap = *(desktopWindow + 0x00)
                        let bmap = read_ptr(&self.main_ram, desktop_win);
                        update_bmap(&mut self.main_ram, bmap, w_u16, h_u16);
                        // Update region handles on desktopWindow:
                        update_rgn_handle(&mut self.main_ram, desktop_win + 0x10, w_u16, h_u16, false); // visRgn
                        update_rgn_handle(&mut self.main_ram, desktop_win + 0x14, w_u16, h_u16, true);  // clipRgn
                        update_rgn_handle(&mut self.main_ram, desktop_win + 0x48, w_u16, h_u16, false); // outside
                        update_rgn_handle(&mut self.main_ram, desktop_win + 0x4C, w_u16, h_u16, false); // inside (CRITICAL!)
                        update_rgn_handle(&mut self.main_ram, desktop_win + 0x88, w_u16, h_u16, false); // rgn88
                        update_rgn_handle(&mut self.main_ram, desktop_win + 0x8C, w_u16, h_u16, false); // rgn8C

                        // 4. Open Window Linked List (curr = *(desktopWindow + 0x80), next = *(curr + 0x66))
                        let mut curr_win = read_ptr(&self.main_ram, desktop_win + 0x80);
                        let mut count = 0;
                        while curr_win > 0 && curr_win + 0x6A <= self.main_ram.len() && count < 64 {
                            let bmap = read_ptr(&self.main_ram, curr_win);
                            update_bmap(&mut self.main_ram, bmap, w_u16, h_u16);
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
                        if menu_bm > 0 && menu_bm + 10 <= self.main_ram.len() {
                            self.main_ram[menu_bm + 6..menu_bm + 8].copy_from_slice(&w_be);
                        }
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

        // 4. Fill extended TVRAM areas (desktop stipple pattern sampled from top-right corner)
        let new_w = width as usize;
        let new_h = height as usize;
        if self.tvram().is_some() && (new_w > 768 || new_h > 512) {
            let tv = self.tvram_mut();
            for plane in 0..4 {
                let plane_base = plane * 0x20000;
                let mut tile = [[0u8; 2]; 32]; // 16 rows * 2 cols
                for ty in 0..16 {
                    for tc in 0..2 {
                        let src_off = plane_base + ty * 128 + (46 + tc) * 2;
                        tile[ty * 2 + tc] = [tv[src_off], tv[src_off + 1]];
                    }
                }
                // Fill horizontal extension (x = 768..new_w, y = 0..512.min(new_h))
                for y in 0..512.min(new_h) {
                    let ty = y % 16;
                    for col in 48..(new_w / 16) {
                        let word = tile[ty * 2 + (col % 2)];
                        let dst_off = plane_base + y * 128 + col * 2;
                        tv[dst_off..dst_off + 2].copy_from_slice(&word);
                    }
                }
                // Fill vertical extension (x = 0..new_w, y = 512..new_h)
                for y in 512..new_h {
                    let ty = y % 16;
                    for col in 0..(new_w / 16) {
                        let word = tile[ty * 2 + (col % 2)];
                        let dst_off = plane_base + y * 128 + col * 2;
                        tv[dst_off..dst_off + 2].copy_from_slice(&word);
                    }
                }
            }
        }
    }
}
