//! Integration tests for the `wgpu` VICON compositor (`msx68k-vicon`), verifying GPU vs. CPU
//! bit-exact parity and rendering the booted Human68k v3.02 TVRAM display.

use std::path::{Path, PathBuf};

use msx68k_bus::{Bus, CpuSpeedMode, VmConfig, X68kVm};
use msx68k_vicon::{decode_tvram_ank_lines, render_bus_frame_cpu, ViconGpuCompositor};

#[test]
fn test_vicon_gpu_and_cpu_compositor_parity() {
    let mut vm = X68kVm::new_app_vm();

    // Configure Text Palette entries #1, #2, #3 and Graphic Palette entry #5
    vm.bus.write_u16(0x00E8_2200, 0x0000, true).unwrap(); // Text #0: Black/Transparent
    vm.bus.write_u16(0x00E8_2202, 0xFFFE, true).unwrap(); // Text #1: Bright White
    vm.bus.write_u16(0x00E8_2204, 0x07C0, true).unwrap(); // Text #2: Red
    vm.bus.write_u16(0x00E8_2206, 0xF800, true).unwrap(); // Text #3: Green
    vm.bus.write_u16(0x00E8_200A, 0x003E, true).unwrap(); // Graphic #5: Blue

    // Enable both Text (bit 5) and Graphics Page 0 (bit 0) -> R2 = 0x0021
    vm.bus.write_u16(0x00E8_2600, 0x0021, true).unwrap();
    // Set VICON R0 = 0x0000 (16-color mode)
    vm.bus.write_u16(0x00E8_2400, 0x0000, true).unwrap();

    // Write a pattern on TVRAM Plane 0 ($E00000) and Plane 1 ($E20000) -> color index 3 at (x=0..7, y=0)
    vm.bus.write_u8(0x00E0_0000, 0b1010_1010, true).unwrap();
    vm.bus.write_u8(0x00E2_0000, 0b1100_1100, true).unwrap();

    // Write Graphic Page 0 pixel at (x=1, y=0) -> color index 5 (visible where TVRAM is 0)
    vm.bus.write_u16(0x00C0_0002, 0x0005, true).unwrap();

    let cpu_frame = render_bus_frame_cpu(&vm.bus);
    assert_eq!(cpu_frame.width, 768);
    assert_eq!(cpu_frame.height, 512);
    assert!(cpu_frame.count_lit_pixels() >= 6);

    if let Some(mut gpu) = ViconGpuCompositor::new() {
        eprintln!(
            "Testing ViconGpuCompositor on adapter: {:?} ({:?})",
            gpu.adapter_info().name,
            gpu.adapter_info().backend
        );
        let gpu_frame = gpu.render_bus_frame(&vm.bus);
        assert_eq!(gpu_frame.width, cpu_frame.width);
        assert_eq!(gpu_frame.height, cpu_frame.height);
        assert_eq!(gpu_frame.rgba8, cpu_frame.rgba8);
    } else {
        eprintln!("No wgpu adapter available in this environment; verified CPU reference.");
    }
}

#[test]
fn test_human68k_boot_tvram_decode_and_vicon_gpu_render() {
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
        return;
    }

    let ipl_data = std::fs::read(&ipl_path).unwrap();
    let cgrom_data = std::fs::read(&cgrom_path).unwrap();
    let human_xdf = std::fs::read(&human_xdf_path).unwrap();

    let mut vm = X68kVm::new(
        VmConfig::app_vm()
            .with_speed_mode(CpuSpeedMode::Burst)
            .with_ram_mb(2),
    );
    vm.bus.load_cgrom(&cgrom_data);
    vm.bus.load_ipl_rom(&ipl_data);
    assert!(vm.bus.insert_floppy(0, human_xdf, true));

    vm.reset().unwrap();
    for chunk in 1..=48 {
        vm.run_steps(250_000);
        let lines = decode_tvram_ank_lines(&vm.bus, 96, 32);
        if lines.iter().any(|l| l.starts_with("A>")) {
            eprintln!(
                "Reached Human68k 'A>' prompt after {} steps ({} chunks)",
                chunk * 250_000,
                chunk
            );
            break;
        }
    }

    let lines = decode_tvram_ank_lines(&vm.bus, 96, 32);
    eprintln!("=== Decoded TVRAM Screen after Human68k v3.02 FD Boot ===");
    for (row, line) in lines.iter().enumerate() {
        if !line.is_empty() {
            eprintln!("Row {:02}: {}", row, line);
        }
    }

    let joined = lines.join("\n");
    assert!(
        joined.contains("Human68k") || joined.contains("SHARP") || joined.contains("A>"),
        "Expected Human68k banner or A> prompt on TVRAM, got:\n{}",
        joined
    );

    let cpu_frame = render_bus_frame_cpu(&vm.bus);
    assert!(cpu_frame.count_lit_pixels() > 1000);

    if let Some(mut gpu) = ViconGpuCompositor::new() {
        let gpu_frame = gpu.render_bus_frame(&vm.bus);
        assert_eq!(gpu_frame.rgba8, cpu_frame.rgba8);
        eprintln!(
            "wgpu VICON rendered {}x{} frame with {} lit pixels (100% match with CPU reference)",
            gpu_frame.width,
            gpu_frame.height,
            gpu_frame.count_lit_pixels()
        );
    }
}

#[test]
fn test_sxwin31_boot_and_vicon_render() {
    let workspace_root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let ipl_path = std::env::var_os("MSX68K_IPL")
        .map(PathBuf::from)
        .unwrap_or_else(|| workspace_root.join("roms/IPLROM.DAT"));
    let cgrom_path = std::env::var_os("MSX68K_CGROM")
        .map(PathBuf::from)
        .unwrap_or_else(|| workspace_root.join("roms/CGROM.DAT"));
    let sxwin_fd0_path = workspace_root.join("disks/SXWIN311.XDF");
    let sxwin_fd1_path = workspace_root.join("disks/SXWIN315.XDF");

    if !ipl_path.exists() || !cgrom_path.exists() || !sxwin_fd0_path.exists() {
        return;
    }

    let ipl_data = std::fs::read(&ipl_path).unwrap();
    let cgrom_data = std::fs::read(&cgrom_path).unwrap();
    let fd0_data = std::fs::read(&sxwin_fd0_path).unwrap();

    let mut vm = X68kVm::new(
        VmConfig::app_vm()
            .with_speed_mode(CpuSpeedMode::Burst)
            .with_ram_mb(4),
    );
    vm.bus.load_cgrom(&cgrom_data);
    vm.bus.load_ipl_rom(&ipl_data);
    assert!(vm.bus.insert_floppy(0, fd0_data, true));
    if let Ok(fd1_data) = std::fs::read(&sxwin_fd1_path) {
        assert!(vm.bus.insert_floppy(1, fd1_data, true));
    }

    vm.reset().unwrap();
    for _ in 1..=90 {
        vm.run_steps(250_000);
    }

    // Verify SRAM preserved the 4 MB RAM size ($0040_0000 at $ED0008, or index 8..12 of SRAM)
    assert_eq!(vm.bus.read_u32(0x00ED_0008, true).unwrap(), 0x0040_0000);

    // Verify VICON is configured for the SX-Window 768x512 desktop (R0=$0004, R2=$003F)
    assert_eq!(vm.bus.vicon.r0_mode(), 0x0004);
    assert_eq!(vm.bus.vicon.r2_display(), 0x003F);
    assert_eq!(vm.bus.crtc.active_resolution(), (768, 512));

    // Verify TVRAM holds the rendered SX-Window v3.1 desktop and Symbol Tray
    let tv_non_zero = vm
        .bus
        .tvram()
        .map_or(0, |tv| tv.iter().filter(|&&b| b != 0).count());
    assert!(
        tv_non_zero > 140_000,
        "Expected SX-Window v3.1 desktop in TVRAM (>140k non-zero bytes), got {tv_non_zero}"
    );

    // Move mouse via Z8530 SCC Channel B and verify SX-Window cursor coordinates update
    for _ in 0..8 {
        vm.bus.scc.add_mouse_delta(-100, -100);
        vm.run_steps(100_000);
    }
    let x0 = vm.bus.read_u16(0x000A_CB72, true).unwrap();
    let y0 = vm.bus.read_u16(0x000A_CB74, true).unwrap();
    assert_eq!((x0, y0), (0, 0), "Cursor should clamp to top-left (0, 0)");

    vm.bus.scc.add_mouse_delta(40, 60);
    vm.run_steps(100_000);
    let x1 = vm.bus.read_u16(0x000A_CB72, true).unwrap();
    let y1 = vm.bus.read_u16(0x000A_CB74, true).unwrap();
    assert!(
        x1 > 0 && y1 > 0,
        "Expected SX-Window cursor coordinates to advance from (0,0), got ({x1}, {y1})"
    );

    // Verify 100% bit-exact parity between CPU reference compositor and wgpu VICON compositor
    // on the live SX-Window v3.1 GUI desktop frame.
    let cpu_frame = render_bus_frame_cpu(&vm.bus);
    assert_eq!(cpu_frame.width, 768);
    assert_eq!(cpu_frame.height, 512);
    if let Some(mut gpu) = ViconGpuCompositor::new() {
        let gpu_frame = gpu.render_bus_frame(&vm.bus);
        assert_eq!(cpu_frame.rgba8, gpu_frame.rgba8);
    }
}

#[test]
fn test_vicon_high_res_ext_tvram_rendering() {
    let mut vm = X68kVm::new_app_vm();

    // Switch to 1920x1080 native resolution
    vm.bus.apply_screen_resolution(1920, 1080);
    assert!(vm.bus.is_ext_tvram_active());
    assert_eq!(vm.bus.crtc.active_resolution(), (1920, 1080));

    // Configure Text Palette entries
    vm.bus.write_u16(0x00E8_2200, 0x0000, true).unwrap(); // Text #0: Transparent
    vm.bus.write_u16(0x00E8_2202, 0xFFFE, true).unwrap(); // Text #1: White
    vm.bus.write_u16(0x00E8_2204, 0x07C0, true).unwrap(); // Text #2: Red
    vm.bus.write_u16(0x00E8_2206, 0xF800, true).unwrap(); // Text #3: Green

    // Enable Text display: R2 bit 5 -> 0x0020
    vm.bus.write_u16(0x00E8_2600, 0x0020, true).unwrap();

    // Write a pixel pattern at (x=100, y=100) using EXT_TVRAM_BASE, mapped as for verification
    vm.bus.guest_screen_mapped = true;
    // row_bytes = ((1920 + 15) / 16) * 2 = 240
    // in_plane = 100 * 240 + (100 / 8) = 24012
    let row_bytes = vm.bus.active_tvram_row_bytes();
    let plane_bytes = vm.bus.active_tvram_plane_bytes();
    let byte_off = 100 * row_bytes + (100 / 8);
    let bit = 7 - (100 % 8);

    // Plane 0 bit set (color bit 0)
    let addr_p0 = msx68k_bus::EXT_TVRAM_BASE + (byte_off as u32);
    // Plane 1 bit set (color bit 1)
    let addr_p1 = msx68k_bus::EXT_TVRAM_BASE + (plane_bytes as u32) + (byte_off as u32);

    vm.bus.write_u8(addr_p0, 1 << bit, true).unwrap();
    vm.bus.write_u8(addr_p1, 1 << bit, true).unwrap();

    let cpu_frame = render_bus_frame_cpu(&vm.bus);
    assert_eq!(cpu_frame.width, 1920);
    assert_eq!(cpu_frame.height, 1080);
    assert!(cpu_frame.count_lit_pixels() > 0);

    // Verify pixel at (100, 100) is green (Color #3 = 0xF800 -> G=31, R=0, B=0)
    let px_idx = (100 * 1920 + 100) * 4;
    let r = cpu_frame.rgba8[px_idx];
    let g = cpu_frame.rgba8[px_idx + 1];
    let b = cpu_frame.rgba8[px_idx + 2];
    assert!(g > 200, "Green channel should be bright, got {}", g);
    assert_eq!(r, 0);
    assert_eq!(b, 0);

    // If GPU compositor is available, verify GPU vs CPU parity at 1920x1080
    if let Some(mut gpu) = ViconGpuCompositor::new() {
        let gpu_frame = gpu.render_bus_frame(&vm.bus);
        assert_eq!(gpu_frame.width, 1920);
        assert_eq!(gpu_frame.height, 1080);
        assert_eq!(gpu_frame.rgba8[px_idx..px_idx + 4], [0, g, 0, 0xFF]);
    }
}
