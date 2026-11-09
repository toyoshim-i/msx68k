//! Tests for Beyond 1K×1K (Approach B): `IOCS _CRTMOD` Mode `$50`,
//! Extended Planar Framebuffer at `$1000_0000` (`EXT_TVRAM_BASE`), and `MSX68K.CFG` disk config parsing.

use msx68k_bus::{Bus, EXT_TVRAM_BASE, X68kVm};

#[test]
fn test_iocs_crtmod_mode_50_queries_and_mode_switches() {
    let mut vm = X68kVm::new_app_vm();

    // Default resolution: 768x512, extended crtmod inactive
    assert_eq!(vm.bus.crtc.active_resolution(), (768, 512));
    assert!(!vm.bus.is_extended_crtmod_active());
    assert!(!vm.bus.is_ext_tvram_active());

    // Place TRAP #15 + RTS at $0001_0000
    let code_addr = 0x0001_0000;
    vm.bus.write_u16(code_addr, 0x4E4F, true).unwrap(); // TRAP #15
    vm.bus.write_u16(code_addr + 2, 0x4E75, true).unwrap(); // RTS

    // 1. Extended Query via D1.W = 0x0150, D2.L = -1
    vm.cpu_state.d[0] = 0x10; // IOCS _CRTMOD
    vm.cpu_state.d[1] = 0x0150;
    vm.cpu_state.d[2] = 0xFFFF_FFFF;
    vm.cpu_state.pc = code_addr;
    vm.step();

    assert_eq!(vm.cpu_state.d[0], 16, "Current mode should be 16 when at 768x512");
    assert_eq!(vm.cpu_state.d[1], (768 << 16) | 512);
    assert_eq!(vm.cpu_state.d[2], 0, "Fullscreen flag should be 0");
    assert_eq!(vm.cpu_state.d[3], (1920 << 16) | 1080, "Native resolution default");

    // 2. Switch to Host Native Fullscreen (D1.W = 0x0150, D2.L = 0)
    vm.cpu_state.d[0] = 0x10;
    vm.cpu_state.d[1] = 0x0150;
    vm.cpu_state.d[2] = 0;
    vm.cpu_state.pc = code_addr;
    vm.step();

    assert_eq!(vm.cpu_state.d[0], 0, "Mode switch returns 0");
    assert_eq!(vm.bus.crtc.active_resolution(), (1920, 1080));
    assert!(vm.bus.fullscreen_mode);
    assert!(vm.bus.is_extended_crtmod_active());
    assert!(vm.bus.is_ext_tvram_active());
    assert_eq!(vm.bus.active_tvram_row_bytes(), 240); // ((1920 + 15) / 16) * 2 = 240
    assert_eq!(vm.bus.active_tvram_plane_bytes(), 240 * 1080); // 259,200 bytes per plane

    // Verify SRAM updated
    assert_eq!(vm.bus.read_u8(0x00ED_007E, true).unwrap(), 0x80 | 0x30);

    // 3. Query again after fullscreen activation
    vm.cpu_state.d[0] = 0x10;
    vm.cpu_state.d[1] = 0x0150;
    vm.cpu_state.d[2] = 0xFFFF_FFFF;
    vm.cpu_state.pc = code_addr;
    vm.step();

    assert_eq!(vm.cpu_state.d[0], 0x50, "Current mode should now be 0x50");
    assert_eq!(vm.cpu_state.d[1], (1920 << 16) | 1080);
    assert_eq!(vm.cpu_state.d[2], 1, "Fullscreen flag should now be 1");

    // 4. Standard IOCS query (D1.W = -1)
    vm.cpu_state.d[0] = 0x10;
    vm.cpu_state.d[1] = 0xFFFF_FFFF;
    vm.cpu_state.pc = code_addr;
    vm.step();
    assert_eq!(vm.cpu_state.d[0], 0x50, "Standard query should report mode 0x50");

    // 5. Switch to Windowed 2560x1440 (D1.W = 0x0150, D2.L = (2560 << 16) | 1440)
    vm.cpu_state.d[0] = 0x10;
    vm.cpu_state.d[1] = 0x0150;
    vm.cpu_state.d[2] = (2560 << 16) | 1440;
    vm.cpu_state.pc = code_addr;
    vm.step();

    assert_eq!(vm.cpu_state.d[0], 0);
    assert_eq!(vm.bus.crtc.active_resolution(), (2560, 1440));
    assert!(!vm.bus.fullscreen_mode);
    assert!(vm.bus.is_extended_crtmod_active());
    assert!(vm.bus.is_ext_tvram_active());
    assert_eq!(vm.bus.active_tvram_row_bytes(), 320); // ((2560 + 15) / 16) * 2 = 320
    assert_eq!(vm.bus.active_tvram_plane_bytes(), 320 * 1440); // 460,800 bytes per plane

    // 6. Test Planar Memory Access at EXT_TVRAM_BASE ($1000_0000)
    // Enable CRTC simultaneous write across all 4 planes (R21 bit 8 + plane mask 0x0F)
    vm.bus.crtc.regs[21] = 0x01F0; // simultaneous write to planes 0, 1, 2, 3
    vm.bus.write_u8(EXT_TVRAM_BASE, 0xAA, true).unwrap();

    let plane_stride = vm.bus.active_tvram_plane_bytes() as u32;
    for plane in 0..4 {
        let val = vm.bus.read_u8(EXT_TVRAM_BASE + plane * plane_stride, true).unwrap();
        assert_eq!(val, 0xAA, "Plane {} should have received 0xAA", plane);
    }

    // Test 16-bit word write with bitmask
    vm.bus.crtc.regs[21] = 0x03F0; // simultaneous write + bitmask enabled
    vm.bus.crtc.regs[23] = 0xF00F; // mask: retain bits 15..12 and 3..0, update bits 11..4
    vm.bus.write_u16(EXT_TVRAM_BASE + 2, 0xFFFF, true).unwrap();

    for plane in 0..4 {
        let word = vm.bus.read_u16(EXT_TVRAM_BASE + plane * plane_stride + 2, true).unwrap();
        assert_eq!(word, 0x0FF0, "Plane {} should have received masked word 0x0FF0", plane);
    }

    // 7. Switch back down to standard 768x512 (D1.W = 0x0150, D2.L = (768 << 16) | 512)
    vm.cpu_state.d[0] = 0x10;
    vm.cpu_state.d[1] = 0x0150;
    vm.cpu_state.d[2] = (768 << 16) | 512;
    vm.cpu_state.pc = code_addr;
    vm.step();

    assert_eq!(vm.cpu_state.d[0], 0);
    assert_eq!(vm.bus.crtc.active_resolution(), (768, 512));
    assert!(!vm.bus.is_ext_tvram_active());
    assert_eq!(vm.bus.active_tvram_row_bytes(), 128);
    assert_eq!(vm.bus.read_u8(0x00ED_007E, true).unwrap(), 0x80 | 16);
}

#[test]
fn test_disk_config_parser_applies_fullscreen_and_custom_resolution() {
    let mut vm = X68kVm::new_app_vm();

    // Synthesize a minimal SCSI HDD image with MSX68K.CFG in root
    let total_bytes = 8 * 1024 * 1024;
    let mut disk = vec![0u8; total_bytes];
    let partition_start_rec = 32u32;
    let total_recs = (total_bytes / 1024) as u32;
    let partition_recs = total_recs - partition_start_rec;

    // SCSI Header and partition table
    disk[0x00..0x08].copy_from_slice(b"X68SCSI1");
    disk[0x800..0x804].copy_from_slice(b"X68K");
    disk[0x810..0x818].copy_from_slice(b"Human68k");
    disk[0x818..0x81C].copy_from_slice(&partition_start_rec.to_be_bytes());
    disk[0x81C..0x820].copy_from_slice(&partition_recs.to_be_bytes());

    // Boot sector at partition start (record 32 = offset 0x8000)
    let boot_off = (partition_start_rec as usize) * 1024;
    let boot = msx68k_bus::synthesize_partition_boot_sector(partition_start_rec, partition_recs, 64, 1024);
    disk[boot_off..boot_off + 1024].copy_from_slice(&boot);

    // Root directory offset: boot_off + 1 sector + 2 * 64 sectors = boot_off + 129 * 1024
    let root_off = boot_off + (1 + 2 * 64) * 1024;
    let data_start_off = root_off + 1024 * 32;

    // Write MSX68K.CFG data to Cluster 2 (data_start_off)
    let cfg_content = b"SCREEN_WIDTH=2560\nSCREEN_HEIGHT=1440\nFULLSCREEN=0\n";
    disk[data_start_off..data_start_off + cfg_content.len()].copy_from_slice(cfg_content);

    // Write root directory entry for MSX68K.CFG
    let entry_off = root_off;
    disk[entry_off..entry_off + 8].copy_from_slice(b"MSX68K  ");
    disk[entry_off + 8..entry_off + 11].copy_from_slice(b"CFG");
    disk[entry_off + 11] = 0x20;
    disk[entry_off + 26..entry_off + 28].copy_from_slice(&2u16.to_le_bytes()); // Cluster 2
    disk[entry_off + 28..entry_off + 32].copy_from_slice(&(cfg_content.len() as u32).to_le_bytes());

    // Attach disk to SCSI target 0
    vm.bus.scsi.drives[0] = Some(disk);

    // Ensure target_resolution is None and SRAM $ED007E is 0x80 | 0x30
    vm.bus.target_resolution = None;
    vm.bus.sram_mut()[0x7E] = 0x80 | 0x30;

    // Load config from disk
    vm.bus.load_disk_config_if_present();

    assert_eq!(vm.bus.target_resolution, Some((2560, 1440)));
    assert!(!vm.bus.fullscreen_mode);
}
