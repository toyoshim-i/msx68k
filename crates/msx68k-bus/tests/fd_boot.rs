//! Integration tests for TVRAM/CRTC hardware acceleration, FDC+DMAC floppy sector transfer,
//! and X68000 EXPERT IPLROM + Human68k v3.02 floppy boot.

use std::path::{Path, PathBuf};

use msx68k_bus::{
    CpuSpeedMode, FloppyGeometry, VmConfig, X68kVm, RASTER_BLOCK_BYTES, TVRAM_PLANE_BYTES,
};
use msx68k_cpu::Bus;

#[test]
fn test_tvram_simultaneous_multiplane_write_bitmask_and_raster_copy() {
    let mut vm = X68kVm::new_app_vm();

    // 1. Enable simultaneous plane write on Planes 0 and 2 ($E8002A = R21 = 0x0150)
    vm.bus.write_u16(0x00E8_002A, 0x0150, true).unwrap();
    vm.bus.write_u16(0x00E0_0100, 0xCAFE, true).unwrap();

    // Plane 0 ($E00100) and Plane 2 ($E40100) should have $CAFE; Planes 1 and 3 remain $0000
    assert_eq!(vm.bus.read_u16(0x00E0_0100, true).unwrap(), 0xCAFE);
    assert_eq!(vm.bus.read_u16(0x00E2_0100, true).unwrap(), 0x0000);
    assert_eq!(vm.bus.read_u16(0x00E4_0100, true).unwrap(), 0xCAFE);
    assert_eq!(vm.bus.read_u16(0x00E6_0100, true).unwrap(), 0x0000);

    // 2. Enable bitmask ($E8002E = R23 = 0xFF00 -> preserve upper byte, overwrite lower byte)
    //    R21 = 0x0350 (Bitmask ON + Simultaneous write on Planes 0 and 2)
    vm.bus.write_u16(0x00E8_002E, 0xFF00, true).unwrap();
    vm.bus.write_u16(0x00E8_002A, 0x0350, true).unwrap();
    vm.bus.write_u16(0x00E0_0100, 0x1234, true).unwrap();

    assert_eq!(vm.bus.read_u16(0x00E0_0100, true).unwrap(), 0xCA34);
    assert_eq!(vm.bus.read_u16(0x00E4_0100, true).unwrap(), 0xCA34);

    // 3. Hardware 4-scanline Raster Copy:
    //    Block 0 covers offset 0x0000..0x01FF (including 0x0100).
    //    Copy Block 0 -> Block 2 (offset 0x0400..0x05FF) on Planes 0 and 2 (R21 = 0x0005).
    vm.bus.write_u16(0x00E8_002A, 0x0005, true).unwrap();
    vm.bus.write_u16(0x00E8_002C, 0x0002, true).unwrap(); // Src = 0, Dst = 2
    vm.bus.write_u8(0x00E8_0481, 0x08, true).unwrap(); // Trigger Raster Copy

    let dst_off = 2 * RASTER_BLOCK_BYTES + 0x0100;
    assert_eq!(
        vm.bus.read_u16(0x00E0_0000 + dst_off as u32, true).unwrap(),
        0xCA34
    );
    assert_eq!(
        vm.bus.read_u16(0x00E4_0000 + dst_off as u32, true).unwrap(),
        0xCA34
    );
    assert_eq!(
        vm.bus
            .read_u16(0x00E0_0000 + (TVRAM_PLANE_BYTES + dst_off) as u32, true)
            .unwrap(),
        0x0000
    );
}

#[test]
fn test_fdc_and_dmac_read_sector_into_main_ram() {
    let mut vm = X68kVm::new_app_vm();

    // Create a 1,261,568-byte 2HD .XDF disk image with recognizable boot sector at (C=0, H=0, R=1, N=3)
    let mut disk = vec![0u8; 1_261_568];
    disk[0..8].copy_from_slice(&[0x60, 0x3C, b'X', b'6', b'8', b'I', b'P', b'L']);
    for (i, slot) in disk.iter_mut().enumerate().take(1024).skip(8) {
        *slot = (i & 0xFF) as u8;
    }
    assert!(vm.bus.insert_floppy(0, disk, true));

    // Configure DMAC Channel 0 ($E84000) to transfer 1024 bytes from FDC ($E94003) to Main RAM $002000:
    // OCR ($E84005) = $82 (Device-to-Memory, 8-bit, No Chaining, External Request)
    // SCR ($E84006) = $04 (MAR increments, DAR fixed)
    // MTC ($E8400A) = 1024 ($0400)
    // MAR ($E8400C) = $0000_2000
    // DAR ($E84014) = $00E9_4003
    // CCR ($E84007) = $88 (STR | ITE)
    vm.bus.write_u8(0x00E8_4000, 0xFF, true).unwrap(); // Clear CSR
    vm.bus.write_u8(0x00E8_4005, 0x82, true).unwrap();
    vm.bus.write_u8(0x00E8_4006, 0x04, true).unwrap();
    vm.bus.write_u16(0x00E8_400A, 1024, true).unwrap();
    vm.bus.write_u32(0x00E8_400C, 0x0000_2000, true).unwrap();
    vm.bus.write_u32(0x00E8_4014, 0x00E9_4003, true).unwrap();
    vm.bus.write_u8(0x00E8_4025, 0x64, true).unwrap(); // NIV = $64
    vm.bus.write_u8(0x00E8_4007, 0x88, true).unwrap(); // Start DMA Ch0

    // Issue FDC READ DATA ($46 = MFM | READ DATA) for Unit 0, C=0, H=0, R=1, N=3, EOT=1, GPL=$1B, DTL=$FF
    let cmd = [0x46, 0x00, 0x00, 0x00, 0x01, 0x03, 0x01, 0x1B, 0xFF];
    for &b in &cmd {
        vm.bus.write_u8(0x00E9_4003, b, true).unwrap();
    }

    // Verify 7 result bytes in R-Phase (ST0=0, ST1=0, ST2=0, C=1, H=0, R=1, N=3)
    let mut results = [0u8; 7];
    for slot in &mut results {
        *slot = vm.bus.read_u8(0x00E9_4003, true).unwrap();
    }
    assert_eq!(results, [0x00, 0x00, 0x00, 0x01, 0x00, 0x01, 0x03]);

    // Verify DMAC Channel 0 completed (CSR_COC = 0x80) and Main RAM at $2000 holds the boot sector
    assert_eq!(vm.bus.read_u8(0x00E8_4000, true).unwrap() & 0x80, 0x80);
    assert_eq!(vm.bus.highest_pending_ipl(), 3);
    assert_eq!(&vm.bus.main_ram()[0x2000..0x2008], b"\x60\x3CX68IPL");
    assert_eq!(vm.bus.main_ram()[0x23FF], 0xFF);
    assert_eq!(FloppyGeometry::XDF_2HD.sector_bytes(), 1024);
}

#[test]
fn test_expert_iplrom_and_human68k_fd_boot() {
    let workspace_root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let ipl_path = std::env::var_os("MSX68K_IPL")
        .map(PathBuf::from)
        .unwrap_or_else(|| workspace_root.join("roms/IPLROM.DAT"));
    let cgrom_path = std::env::var_os("MSX68K_CGROM")
        .map(PathBuf::from)
        .unwrap_or_else(|| workspace_root.join("roms/CGROM.DAT"));
    let human_xdf_path = std::env::var_os("MSX68K_FD0")
        .map(PathBuf::from)
        .unwrap_or_else(|| workspace_root.join("disks/HUMAN302.XDF"));

    if !ipl_path.exists() || !cgrom_path.exists() || !human_xdf_path.exists() {
        eprintln!(
            "Skipping real ROM/XDF boot test (place ROMs in roms/ and disk in disks/, or set MSX68K_IPL/MSX68K_CGROM/MSX68K_FD0)"
        );
        return;
    }

    let ipl_data = std::fs::read(&ipl_path).unwrap();
    let cgrom_data = std::fs::read(&cgrom_path).unwrap();
    let human_xdf = std::fs::read(&human_xdf_path).unwrap();

    let mut vm = X68kVm::new(
        VmConfig::app_vm()
            .with_speed_mode(CpuSpeedMode::Mhz10)
            .with_ram_mb(2),
    );
    vm.bus.load_cgrom(&cgrom_data);
    vm.bus.load_ipl_rom(&ipl_data);
    assert!(vm.bus.insert_floppy(0, human_xdf, true));

    vm.reset().unwrap();
    assert_eq!(vm.cpu_state.pc, 0x00FF_0010);

    // Run up to 6,000,000 instructions in chunks of 20,000 and trace boot milestones
    let mut boot_sector_loaded = false;
    let mut human_sys_loaded = false;
    let mut tvram_written = false;

    for chunk in 0..300 {
        vm.run_steps(20_000);

        // Check if FD boot sector (loaded at $0000_2000 by IPLROM) has been read into RAM
        if !boot_sector_loaded
            && (&vm.bus.main_ram()[0x2003..0x200B] == b"X68IPL30"
                || vm.bus.fdc.drives[0].motor_on)
        {
            boot_sector_loaded = true;
            eprintln!(
                "[Boot milestone] FD boot sector loaded at $002000 (chunk {}, PC=${:06X})",
                chunk, vm.cpu_state.pc
            );
        }

        // Check if HUMAN.SYS / COMMAND.X is resident in Main RAM
        if !human_sys_loaded
            && vm.bus.main_ram()[0x6800..0x20000]
                .windows(8)
                .any(|w| w == b"HUMAN   " || w == b"COMMAND ")
        {
            human_sys_loaded = true;
            eprintln!(
                "[Boot milestone] HUMAN.SYS / COMMAND.X resident in Main RAM (chunk {}, PC=${:06X})",
                chunk, vm.cpu_state.pc
            );
        }

        if !tvram_written {
            if let Some(tv) = vm.bus.tvram() {
                let non_zero = tv.iter().filter(|&&b| b != 0).count();
                if non_zero > 4096 {
                    tvram_written = true;
                    eprintln!(
                        "[Boot milestone] TVRAM active with {} non-zero bytes (chunk {}, PC=${:06X})",
                        non_zero, chunk, vm.cpu_state.pc
                    );
                }
            }
        }

        if boot_sector_loaded && human_sys_loaded && tvram_written && chunk > 160 {
            break;
        }
    }

    let tv_nonzero = vm
        .bus
        .tvram()
        .map_or(0, |tv| tv.iter().filter(|&&b| b != 0).count());
    eprintln!(
        "Final state: PC=${:06X}, SR=${:04X}, boot_sector={}, human_sys={}, tv_nonzero={}",
        vm.cpu_state.pc,
        vm.cpu_state.sr,
        boot_sector_loaded,
        human_sys_loaded,
        tv_nonzero
    );

    assert!(boot_sector_loaded, "IPLROM must load the FD boot sector into $002000");
    assert!(human_sys_loaded, "FD boot sector must load HUMAN.SYS into Main RAM");
    assert!(tv_nonzero > 256, "IPLROM / Human68k must render text onto TVRAM");
}
