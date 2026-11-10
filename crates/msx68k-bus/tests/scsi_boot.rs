//! Integration tests for the Virtual SCSI Subsystem ($FC0000 ROM, IOCS $F5 _SCSIDRV,
//! and romdb block driver) and the Mixed Mode Hypercall & FSX.X 1:1 Mouse Hook.

use std::path::Path;

use msx68k_bus::scsi::{build_bootable_sxwin_scsi_hdd, parse_scsi_partitions};
use msx68k_bus::{CpuSpeedMode, VmConfig, X68kVm};
use msx68k_cpu::Bus;

fn render_bus_tvram_rgba(bus: &msx68k_bus::X68kBus) -> (u32, u32, Vec<u8>) {
    let (width, height) = bus.crtc.active_resolution();
    let tvram = bus.active_tvram().expect("TVRAM must be allocated");
    let row_bytes = bus.active_tvram_row_bytes();
    let plane_bytes = bus.active_tvram_plane_bytes();
    let tx_pal = bus.vicon.text_palette();
    let contrast = (bus.sysport.contrast & 0x0F).max(1) as u32;

    let grbi_to_rgba = |grbi: u16| -> [u8; 4] {
        let i_bit = (grbi & 1) as u32;
        let g6 = ((((grbi >> 11) & 0x1F) as u32) << 1) | i_bit;
        let r6 = ((((grbi >> 6) & 0x1F) as u32) << 1) | i_bit;
        let b6 = ((((grbi >> 1) & 0x1F) as u32) << 1) | i_bit;
        let r8 = (((r6 << 2) | (r6 >> 4)) * contrast / 15) as u8;
        let g8 = (((g6 << 2) | (g6 >> 4)) * contrast / 15) as u8;
        let b8 = (((b6 << 2) | (b6 >> 4)) * contrast / 15) as u8;
        [r8, g8, b8, 0xFF]
    };

    let mut rgba = vec![0u8; (width * height * 4) as usize];
    for y in 0..height {
        for x in 0..width {
            let tx = (x + bus.crtc.text_scroll_x() as u32) % width;
            let ty = (y + bus.crtc.text_scroll_y() as u32) % height;
            let in_plane = ((ty as usize) * row_bytes) + ((tx >> 3) as usize);
            let bit = 7 - (tx & 7);
            let b0 = ((tvram[in_plane] >> bit) & 1) as usize;
            let b1 = ((tvram[plane_bytes + in_plane] >> bit) & 1) as usize;
            let b2 = ((tvram[plane_bytes * 2 + in_plane] >> bit) & 1) as usize;
            let b3 = ((tvram[plane_bytes * 3 + in_plane] >> bit) & 1) as usize;
            let idx = b0 | (b1 << 1) | (b2 << 2) | (b3 << 3);
            let color = grbi_to_rgba(tx_pal[idx]);
            let off = ((y * width + x) * 4) as usize;
            rgba[off..off + 4].copy_from_slice(&color);
        }
    }
    (width, height, rgba)
}

/// Requests a resolution change and runs the guest until the scheduler has applied it.
fn switch_resolution_at_task_turn(vm: &mut X68kVm, width: u32, height: u32) {
    vm.bus.request_screen_resolution(width, height);
    for _ in 0..5_000_000 {
        if vm.bus.crtc.active_resolution() == (width, height) {
            return;
        }
        vm.step();
    }
    panic!("resolution {width}x{height} was never applied");
}

fn save_rgba_to_png(width: u32, height: u32, rgba: &[u8], out_png: &str) {
    let raw_path = format!("{}.raw", out_png);
    std::fs::write(&raw_path, rgba).expect("write raw");
    let py_cmd = format!(
        "from PIL import Image; img = Image.frombytes('RGBA', ({}, {}), open('{}', 'rb').read()); img.save('{}')",
        width, height, raw_path, out_png
    );
    let status = std::process::Command::new("python3")
        .arg("-c")
        .arg(&py_cmd)
        .status()
        .expect("python convert");
    assert!(status.success());
    let _ = std::fs::remove_file(&raw_path);
}

#[test]
fn test_scsi_hdd_generation_and_partition_table() {
    let workspace_root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let human_path = workspace_root.join("disks/HUMAN302.XDF");
    if !human_path.exists() {
        eprintln!("disks/HUMAN302.XDF not found, skipping disk generation test");
        return;
    }

    let human_data = std::fs::read(&human_path).unwrap();
    let sx_disks = [
        ("SXWIN311", workspace_root.join("disks/SXWIN311.XDF")),
        ("SXWIN312", workspace_root.join("disks/SXWIN312.XDF")),
        ("SXWIN313", workspace_root.join("disks/SXWIN313.XDF")),
        ("SXWIN314", workspace_root.join("disks/SXWIN314.XDF")),
        ("SXWIN315", workspace_root.join("disks/SXWIN315.XDF")),
        ("SXWIN316", workspace_root.join("disks/SXWIN316.XDF")),
    ];

    let mut sx_loaded = Vec::new();
    for (name, path) in &sx_disks {
        if let Ok(bytes) = std::fs::read(path) {
            sx_loaded.push((*name, bytes));
        }
    }
    let sx_refs: Vec<(&str, &[u8])> = sx_loaded.iter().map(|(n, b)| (*n, b.as_slice())).collect();

    let hdd_image = build_bootable_sxwin_scsi_hdd(&human_data, &sx_refs, 32);

    let hds_path = workspace_root.join("disks/SXWIN31.HDS");
    let _ = std::fs::write(&hds_path, &hdd_image);

    // Diagnostic: inspect directory structure and FAT on generated hdd_image
    {
        let part_offset = 32 * 1024;
        let fat1_off = part_offset + 1024;
        let root_off = fat1_off + 2 * 64 * 1024;
        let data_off = root_off + 1024 * 32;

        eprintln!("[HDD Check] part_offset={}, fat1_off={}, root_off={}, data_off={}", part_offset, fat1_off, root_off, data_off);

        // Scan root directory
        let mut shell_cluster = 0u16;
        for i in 0..1024 {
            let entry_off = root_off + i * 32;
            let b0 = hdd_image[entry_off];
            if b0 == 0 { break; }
            if b0 == 0xE5 { continue; }
            let name = &hdd_image[entry_off..entry_off + 11];
            let attr = hdd_image[entry_off + 11];
            let clus = u16::from_le_bytes([hdd_image[entry_off + 26], hdd_image[entry_off + 27]]);
            let sz = u32::from_le_bytes(hdd_image[entry_off + 28..entry_off + 32].try_into().unwrap());
            eprintln!("[HDD Root Entry {:3}] {:?} attr={:#02x} clus={} sz={}", i, String::from_utf8_lossy(name), attr, clus, sz);
            if name.starts_with(b"SHELL") && (attr & 0x10) != 0 {
                shell_cluster = clus;
            }
        }

        // Verify directory organization in generated HDD
        let mut bin_cluster = 0u16;
        let mut x68_cluster = 0u16;
        let mut dic_cluster = 0u16;
        let mut root_file_count = 0;
        let mut root_dir_count = 0;
        for i in 0..1024 {
            let entry_off = root_off + i * 32;
            let b0 = hdd_image[entry_off];
            if b0 == 0 { break; }
            if b0 == 0xE5 { continue; }
            let name = &hdd_image[entry_off..entry_off + 11];
            let attr = hdd_image[entry_off + 11];
            let clus = u16::from_le_bytes([hdd_image[entry_off + 26], hdd_image[entry_off + 27]]);
            if (attr & 0x10) != 0 {
                root_dir_count += 1;
                if name.starts_with(b"BIN") { bin_cluster = clus; }
                if name.starts_with(b"X68") { x68_cluster = clus; }
                if name.starts_with(b"DIC") { dic_cluster = clus; }
            } else {
                root_file_count += 1;
            }
        }

        assert!(root_dir_count >= 8, "Expected subdirectories (BIN, SYS, SHELL, X68, DIC, FONT, etc.) in root");
        assert!(root_file_count <= 10, "Root directory must not be cluttered with application files (count={})", root_file_count);
        assert!(bin_cluster >= 2, "BIN directory must exist in root");
        assert!(x68_cluster >= 2, "X68 directory must exist in root");
        assert!(dic_cluster >= 2, "DIC directory must exist in root");

        // Verify root directory files:
        let mut found_root_sxmode = false;
        let mut found_root_sx_bat = false;
        for i in 0..1024 {
            let entry_off = root_off + i * 32;
            let b0 = hdd_image[entry_off];
            if b0 == 0 { break; }
            if b0 == 0xE5 { continue; }
            let name = &hdd_image[entry_off..entry_off + 11];
            let attr = hdd_image[entry_off + 11];
            if (attr & 0x10) == 0 {
                if name.starts_with(b"SXMODE") {
                    found_root_sxmode = true;
                }
                if name.starts_with(b"SX      BAT") {
                    found_root_sx_bat = true;
                }
            }
        }
        assert!(found_root_sxmode, "SXMODE.X must be present in root A:\\ for 1-click access");
        assert!(found_root_sx_bat, "SX.BAT must be present in root A:\\ to match official installer");

        // Verify SHELL/SXWIN.ENV contains official search path with A:\BIN and A:\X68
        assert!(
            hdd_image.windows(b"path=A:\\;A:\\SHELL;A:\\SYS;A:\\BIN;A:\\X68;".len()).any(|w| w == b"path=A:\\;A:\\SHELL;A:\\SYS;A:\\BIN;A:\\X68;"),
            "SXWIN.ENV must contain official search path with A:\\BIN and A:\\X68"
        );

        // Verify ｺﾝｿｰﾙ.ENV retains original -Ccommand.x without database corruption
        assert!(
            hdd_image.windows(b"-Ccommand.x".len()).any(|w| w == b"-Ccommand.x"),
            "ｺﾝｿｰﾙ.ENV must preserve original -Ccommand.x"
        );

        // Verify files in X68 directory
        let x68_off = data_off + ((x68_cluster as usize) - 2) * 1024;
        let mut found_sxmode = false;
        let mut found_hdformat = false;
        let mut x68_file_count = 0;
        for i in 0..128 {
            let entry_off = x68_off + i * 32;
            let b0 = hdd_image[entry_off];
            if b0 == 0 { break; }
            if b0 == 0xE5 { continue; }
            let name = &hdd_image[entry_off..entry_off + 11];
            let attr = hdd_image[entry_off + 11];
            if (attr & 0x10) == 0 {
                x68_file_count += 1;
                if name.starts_with(b"SXMODE") { found_sxmode = true; }
                if name.starts_with(b"HD") { found_hdformat = true; }
            }
        }
        assert!(x68_file_count >= 20, "X68 directory must contain applications and tools (found={})", x68_file_count);
        assert!(found_sxmode, "SXMODE.X must be in X68 directory");
        assert!(found_hdformat, "HDFORMAT must be in X68 directory");

        // Verify dictionaries in DIC directory
        let dic_off = data_off + ((dic_cluster as usize) - 2) * 1024;
        let mut found_x68k_dic = false;
        for i in 0..32 {
            let entry_off = dic_off + i * 32;
            let b0 = hdd_image[entry_off];
            if b0 == 0 { break; }
            if b0 == 0xE5 { continue; }
            let name = &hdd_image[entry_off..entry_off + 11];
            if name.starts_with(b"X68K") && name.ends_with(b"DIC") {
                found_x68k_dic = true;
            }
        }
        assert!(found_x68k_dic, "X68K.DIC must be located in DIC directory");

        // Scan SHELL directory
        let mut found_sxcon = false;
        if shell_cluster >= 2 {
            let sh_off = data_off + ((shell_cluster as usize) - 2) * 1024;
            eprintln!("[HDD Check] SHELL directory at cluster {}, offset {}:", shell_cluster, sh_off);
            for i in 0..32 {
                let entry_off = sh_off + i * 32;
                let b0 = hdd_image[entry_off];
                if b0 == 0 { break; }
                if b0 == 0xE5 { continue; }
                let name = &hdd_image[entry_off..entry_off + 11];
                let attr = hdd_image[entry_off + 11];
                let clus = u16::from_le_bytes([hdd_image[entry_off + 26], hdd_image[entry_off + 27]]);
                let sz = u32::from_le_bytes(hdd_image[entry_off + 28..entry_off + 32].try_into().unwrap());
                eprintln!("  [SHELL Entry {:2}] {:?} attr={:#02x} clus={} sz={}", i, String::from_utf8_lossy(name), attr, clus, sz);

                if name.starts_with(b"SXCON") && name[8..].starts_with(b"X") {
                    found_sxcon = true;
                    assert_eq!(sz, 39558, "SXCON.X size must be 39,558 bytes");
                    let file_off = data_off + ((clus as usize) - 2) * 1024;
                    assert_eq!(&hdd_image[file_off..file_off + 2], b"HU", "SXCON.X must start with 'HU' magic");
                }

                if name.starts_with(b"SXWIN") && !name.ends_with(b"ENV") {
                    // Check FAT chain for SXWIN.X
                    let mut curr_c = clus;
                    let mut chain = Vec::new();
                    while (2..0xFFF0).contains(&curr_c) {
                        chain.push(curr_c);
                        let fat_entry_off = fat1_off + (curr_c as usize) * 2;
                        curr_c = u16::from_be_bytes([hdd_image[fat_entry_off], hdd_image[fat_entry_off + 1]]);
                    }
                    chain.push(curr_c);
                    eprintln!("    SXWIN.X cluster chain: {:?}", chain);
                }
            }
        }
        assert!(found_sxcon, "SHELL/SXCON.X must be recovered and present in SHELL directory");
    }
    assert!(hdd_image.len() >= 32 * 1024 * 1024);

    // Verify physical SCSI disk header at offset 0
    assert_eq!(&hdd_image[0..8], b"X68SCSI1");
    let sector_size_code = u16::from_be_bytes([hdd_image[8], hdd_image[9]]);
    assert_eq!(sector_size_code, 0x0200); // 1024 bytes per sector code

    // Verify partition table at offset 2048 (0x800)
    let partitions = parse_scsi_partitions(&hdd_image).expect("Must parse partition table");
    assert!(!partitions.is_empty(), "Must produce at least one partition");
    let p0 = &partitions[0];
    assert_eq!(&p0.name, b"Human68k");
    assert!(p0.start_record >= 4);

    // Verify boot sector inside partition
    let p0_byte_offset = (p0.start_record as usize) * 1024;
    assert_eq!(&hdd_image[p0_byte_offset..p0_byte_offset + 2], &[0x60, 0x24]);
    assert_eq!(&hdd_image[p0_byte_offset + 2..p0_byte_offset + 8], b"X68IPL");
}

#[test]
fn test_virtual_scsi_rom_and_iocs_dispatch() {
    let mut vm = X68kVm::new_app_vm();

    // Fabricate a minimal 2 MB SCSI HDD with valid partition table
    let mut mock_disk = vec![0u8; 2 * 1024 * 1024];
    mock_disk[0..8].copy_from_slice(b"X68SCSI1");
    mock_disk[14..16].copy_from_slice(&512u16.to_be_bytes());
    mock_disk[16..20].copy_from_slice(&4096u32.to_be_bytes()); // 4096 sectors = 2 MB

    // Partition table at offset 2048
    mock_disk[0x800..0x804].copy_from_slice(b"X68K");
    mock_disk[0x810..0x820].copy_from_slice(b"Human68k\0\0\0\0\0\0\0\0");
    mock_disk[0x820] = 0x80; // Bootable flag
    mock_disk[0x824..0x828].copy_from_slice(&8u32.to_be_bytes()); // Start LBA = 8
    mock_disk[0x828..0x82C].copy_from_slice(&4000u32.to_be_bytes()); // 4000 sectors

    // Write a recognizable pattern at LBA 8
    mock_disk[8 * 512..8 * 512 + 8].copy_from_slice(b"BOOTSECT");

    // Before mounting SCSI HDD, reading $FC0000 raises a BusError
    assert!(vm.bus.read_u32(0x00FC_0000, true).is_err());
    assert!(!vm.bus.is_scsi_rom_enabled());

    // Insert SCSI HDD into Target 0
    assert!(vm.bus.insert_scsi_hdd(0, mock_disk));
    assert!(vm.bus.is_scsi_rom_enabled());

    // Verify SCSI ROM header at $FC0000
    assert_eq!(vm.bus.read_u32(0x00FC_0000, true).unwrap(), 0x00FC_0050);
    let mut sig = [0u8; 6];
    for (i, b) in sig.iter_mut().enumerate() {
        *b = vm.bus.read_u8(0x00FC_0024 + i as u32, true).unwrap();
    }
    assert_eq!(&sig, b"SCSIIN");
    assert_eq!(vm.bus.read_u32(0x00FC_0048, true).unwrap(), 0x4875_6D61); // "Huma"
    assert_eq!(vm.bus.read_u32(0x00FC_004C, true).unwrap(), 0x6E36_386B); // "n68k"

    // Install IOCS $F5 vector via hypercall 0xFE84 (or direct install)
    vm.cpu_state.pc = 0x00FC_0064;
    vm.step();
    let iocs_vec = vm.bus.read_u32(0x0007D4, true).unwrap();
    assert_eq!(iocs_vec, 0x00FC_0068);

    // Test IOCS $F5 _S_INQUIRY ($20):
    // Buffer at $0001_0000, Target ID = 0 (in D2.B), length = 36 (in D3)
    let inq_buf = 0x0001_0000;
    vm.cpu_state.d[1] = 0x20; // _S_INQUIRY
    vm.cpu_state.d[2] = 0x00; // ID 0
    vm.cpu_state.d[3] = 36;
    vm.cpu_state.a[1] = inq_buf;
    vm.cpu_state.pc = 0x00FC_0068;
    vm.step();
    assert_eq!(vm.cpu_state.d[0], 0); // Success status

    let mut vendor = [0u8; 8];
    for (i, b) in vendor.iter_mut().enumerate() {
        *b = vm.bus.read_u8(inq_buf + 8 + i as u32, true).unwrap();
    }
    assert_eq!(&vendor, b"SHARP   ");

    // Test IOCS $F5 _S_READ ($21):
    // Read 1 sector (512 bytes, D5 = 1) from LBA 8 into $0001_1000
    let read_buf = 0x0001_1000;
    vm.cpu_state.d[1] = 0x21; // _S_READ
    vm.cpu_state.d[2] = 8; // LBA 8
    vm.cpu_state.d[3] = 1; // 1 sector
    vm.cpu_state.d[4] = 0; // ID 0
    vm.cpu_state.d[5] = 1; // 512 bytes per sector
    vm.cpu_state.a[1] = read_buf;
    vm.cpu_state.pc = 0x00FC_0068;
    vm.step();
    assert_eq!(vm.cpu_state.d[0], 0);

    let mut read_sig = [0u8; 8];
    for (i, b) in read_sig.iter_mut().enumerate() {
        *b = vm.bus.read_u8(read_buf + i as u32, true).unwrap();
    }
    assert_eq!(&read_sig, b"BOOTSECT");
}

#[test]
fn test_scsi_romdb_driver_construction_and_fat_bpb() {
    let mut vm = X68kVm::new_app_vm();

    // Create a 4 MB mock disk with partition starting at LBA 16
    let mut mock_disk = vec![0u8; 4 * 1024 * 1024];
    mock_disk[0..8].copy_from_slice(b"X68SCSI1");
    mock_disk[14..16].copy_from_slice(&512u16.to_be_bytes());
    mock_disk[0x800..0x804].copy_from_slice(b"X68K");
    mock_disk[0x810..0x818].copy_from_slice(b"Human68k");
    mock_disk[0x818..0x81C].copy_from_slice(&16u32.to_be_bytes()); // Start LBA = 16
    mock_disk[0x81C..0x820].copy_from_slice(&8000u32.to_be_bytes());

    // Boot sector at LBA 16 with standard BPB
    let bpb_off = 16 * 512;
    mock_disk[bpb_off..bpb_off + 8].copy_from_slice(b"X68SCSI1");
    mock_disk[bpb_off + 0x12..bpb_off + 0x14].copy_from_slice(&512u16.to_be_bytes()); // Bytes per sector (BE)
    mock_disk[bpb_off + 0x14] = 4; // Sectors per cluster
    mock_disk[bpb_off + 11..bpb_off + 13].copy_from_slice(&512u16.to_le_bytes()); // Bytes per sector (LE)
    mock_disk[bpb_off + 13] = 4; // Sectors per cluster
    mock_disk[bpb_off + 14..bpb_off + 16].copy_from_slice(&1u16.to_le_bytes()); // Reserved sectors
    mock_disk[bpb_off + 16] = 2; // FAT count
    mock_disk[bpb_off + 17..bpb_off + 19].copy_from_slice(&512u16.to_le_bytes()); // Root entries
    mock_disk[bpb_off + 19..bpb_off + 21].copy_from_slice(&8000u16.to_le_bytes()); // Total sectors
    mock_disk[bpb_off + 21] = 0xF8; // Media byte
    mock_disk[bpb_off + 22..bpb_off + 24].copy_from_slice(&8u16.to_le_bytes()); // Sectors per FAT

    assert!(vm.bus.insert_scsi_hdd(0, mock_disk));

    // 1. Simulate HUMAN.SYS calling ScsiRomdbBuild with A4 = 0x0001_0000, D2 = 0
    let driver_header = 0x0001_0000;
    vm.cpu_state.a[4] = driver_header;
    vm.cpu_state.d[2] = 0;
    vm.cpu_state.pc = 0x00FC_0060;
    vm.step();

    assert_eq!(vm.cpu_state.d[2], 1); // Unit count / next index
    assert_eq!(vm.bus.read_u32(driver_header, true).unwrap(), 0xFFFF_FFFF);
    assert_eq!(vm.bus.read_u32(driver_header + 6, true).unwrap(), 0x00FC_006C); // Strategy
    assert_eq!(vm.bus.read_u32(driver_header + 10, true).unwrap(), 0x00FC_0070); // Interrupt
    assert_eq!(vm.bus.read_u8(driver_header + 14, true).unwrap(), 1); // 1 unit

    // Check BPB table pointer at driver_header + 22 points to driver_header + 26
    let bpb_ptr = vm.bus.read_u32(driver_header + 22, true).unwrap();
    assert_eq!(bpb_ptr, driver_header + 26);

    // Verify BPB values in guest memory
    let sec_size = vm.bus.read_u16(bpb_ptr, true).unwrap();
    assert_eq!(sec_size, 512);
    let sec_per_clus = vm.bus.read_u8(bpb_ptr + 2, true).unwrap();
    assert_eq!(sec_per_clus, 4);

    // Calling ScsiRomdbBuild again with D2 = 1 should return D2 = -1 (end of drivers)
    vm.cpu_state.d[2] = 1;
    vm.cpu_state.pc = 0x00FC_0060;
    vm.step();
    assert_eq!(vm.cpu_state.d[2], 0xFFFF_FFFF);

    // 2. Test Block Driver Strategy and Interrupt (Command 4 = READ)
    let req_packet = 0x0002_0000;
    vm.bus.write_u8(req_packet, 26, true).unwrap(); // Packet length
    vm.bus.write_u8(req_packet + 1, 0, true).unwrap(); // Unit 0
    vm.bus.write_u8(req_packet + 2, 4, true).unwrap(); // Command 4 (READ)
    vm.bus.write_u16(req_packet + 3, 0x0000, true).unwrap(); // Status
    vm.bus.write_u32(req_packet + 14, 0x0003_0000, true).unwrap(); // Buffer address
    vm.bus.write_u32(req_packet + 18, 1, true).unwrap(); // 1 sector
    vm.bus.write_u32(req_packet + 22, 0, true).unwrap(); // Relative sector 0 (= LBA 16)

    // Strategy sets req_ptr
    vm.cpu_state.a[5] = req_packet;
    vm.cpu_state.pc = 0x00FC_006C;
    vm.step();

    // Interrupt performs sector read
    vm.cpu_state.a[5] = req_packet;
    vm.cpu_state.pc = 0x00FC_0070;
    vm.step();

    // Verify status = 0x0000 (Success)
    let status = vm.bus.read_u16(req_packet + 3, true).unwrap();
    assert_eq!(status, 0x0000);

    // Verify read buffer at $0003_0000 received the boot sector from LBA 16
    let mut boot_sig = [0u8; 8];
    for (i, b) in boot_sig.iter_mut().enumerate() {
        *b = vm.bus.read_u8(0x0003_0000 + i as u32, true).unwrap();
    }
    assert_eq!(&boot_sig, b"X68SCSI1");
}

#[test]
fn test_fsx_mouse_hook_patch_and_sx_mouse_sync() {
    let mut vm = X68kVm::new_app_vm();

    // Place FSX.X mock code in Main RAM
    let text_base: u32 = 0x0002_0000;
    let handler_addr = text_base + 0x04F82A;
    let patch_addr = text_base + 0x04F768;

    // Write original signature MOVEM.L D0-D2/A5, -(SP) at patch_addr
    vm.bus
        .write_u32(patch_addr, 0x48E7_E004, true)
        .unwrap();

    // Writing to IOCS mouse vector $0000_0938 triggers automatic patch detection
    vm.bus
        .write_u32(0x0000_0938, handler_addr, true)
        .unwrap();

    // Verify patch_addr now contains [0xFE, 0x81, 0x4E, 0x75] (Hypercall 0xFE81 + RTS)
    let patched_code = vm.bus.read_u32(patch_addr, true).unwrap();
    assert_eq!(patched_code, 0xFE81_4E75);

    // Setup MouseWorkArea at A4 = 0x0003_0000
    let mwa = 0x0003_0000;
    vm.bus.write_u16(mwa + 0x34, 0, true).unwrap(); // min_x = 0
    vm.bus.write_u16(mwa + 0x36, 0, true).unwrap(); // min_y = 0
    vm.bus.write_u16(mwa + 0x38, 768, true).unwrap(); // max_x = 768
    vm.bus.write_u16(mwa + 0x3A, 512, true).unwrap(); // max_y = 512

    vm.bus.write_u16(mwa + 0x0A, 50, true).unwrap(); // current x
    vm.bus.write_u16(mwa + 0x0C, 50, true).unwrap(); // current y

    // Case 1: Host mouse position is Some((400, 300)) -> 1:1 absolute synchronization
    vm.bus.set_host_mouse_position(Some((400, 300)));
    vm.cpu_state.a[4] = mwa;
    vm.cpu_state.pc = patch_addr;
    vm.step();

    assert_eq!(vm.bus.read_u16(mwa + 0x0A, true).unwrap(), 400);
    assert_eq!(vm.bus.read_u16(mwa + 0x0C, true).unwrap(), 300);
    assert_eq!(vm.bus.read_u16(mwa + 0x1A, true).unwrap(), 0);
    assert_eq!(vm.bus.read_u16(mwa + 0x1C, true).unwrap(), 0);

    // Case 2: Host mouse clamped to boundary
    vm.bus.set_host_mouse_position(Some((9999, -500)));
    vm.cpu_state.pc = patch_addr;
    vm.step();
    assert_eq!(vm.bus.read_u16(mwa + 0x0A, true).unwrap(), 767);
    assert_eq!(vm.bus.read_u16(mwa + 0x0C, true).unwrap(), 0);

    // Case 3: Host mouse position is None -> relative delta addition
    vm.bus.set_host_mouse_position(None);
    vm.bus.write_u16(mwa + 0x16, (-100i16) as u16, true).unwrap(); // dx = -100
    vm.bus.write_u16(mwa + 0x18, 50u16, true).unwrap(); // dy = +50
    vm.cpu_state.pc = patch_addr;
    vm.step();
    assert_eq!(vm.bus.read_u16(mwa + 0x0A, true).unwrap(), 667);
    assert_eq!(vm.bus.read_u16(mwa + 0x0C, true).unwrap(), 50);
}

#[test]
fn test_scsi_iplrom_boot_milestone() {
    let workspace_root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let ipl_path = workspace_root.join("roms/IPLROM.DAT");
    let cgrom_path = workspace_root.join("roms/CGROM.DAT");
    let human_path = workspace_root.join("disks/HUMAN302.XDF");

    if !ipl_path.exists() || !cgrom_path.exists() || !human_path.exists() {
        eprintln!("ROMs or disks not found, skipping real SCSI boot milestone");
        return;
    }

    let ipl_data = std::fs::read(&ipl_path).unwrap();
    let cgrom_data = std::fs::read(&cgrom_path).unwrap();
    let human_data = std::fs::read(&human_path).unwrap();

    let hdd_image = build_bootable_sxwin_scsi_hdd(&human_data, &[], 32);

    let mut vm = X68kVm::new(
        VmConfig::app_vm()
            .with_speed_mode(CpuSpeedMode::Mhz10)
            .with_ram_mb(4),
    );
    vm.bus.load_cgrom(&cgrom_data);
    vm.bus.load_ipl_rom(&ipl_data);

    // Mount SCSI HDD on Target 0 and set SCSI boot mode (no floppy in FD0)
    assert!(vm.bus.insert_scsi_hdd(0, hdd_image));
    vm.bus.set_boot_from_scsi(true);

    vm.reset().unwrap();
    assert_eq!(vm.cpu_state.pc, 0x00FF_0010);

    let mut scsi_boot_sector_read = false;
    let mut human_sys_resident = false;

    // Run up to 5,000,000 instructions in chunks
    for chunk in 0..250 {
        vm.run_steps(20_000);

        // Check if SCSI IPL code or boot sector has been loaded to $002000
        if !scsi_boot_sector_read
            && (vm.bus.main_ram()[0x2000] == 0x60
                || &vm.bus.main_ram()[0x2000..0x2008] == b"X68SCSI1")
        {
            scsi_boot_sector_read = true;
            eprintln!(
                "[SCSI Boot] SCSI IPL code loaded at $002000 (chunk {}, PC=${:06X}, ram[2000..2020]={:02X?})",
                chunk, vm.cpu_state.pc, &vm.bus.main_ram()[0x2000..0x2020]
            );
        }
        if scsi_boot_sector_read && chunk <= 150 {
            eprintln!("[SCSI Boot] chunk {}: PC=${:06X}", chunk, vm.cpu_state.pc);
        }

        // Check if HUMAN.SYS is resident in Main RAM
        if !human_sys_resident
            && vm.bus.main_ram()[0x6800..0x30000]
                .windows(8)
                .any(|w| w == b"HUMAN   " || w == b"COMMAND ")
        {
            human_sys_resident = true;
            eprintln!(
                "[SCSI Boot] HUMAN.SYS / COMMAND.X resident in Main RAM (chunk {}, PC=${:06X})",
                chunk, vm.cpu_state.pc
            );
        }

        if scsi_boot_sector_read && human_sys_resident && chunk > 100 {
            break;
        }
    }

    assert!(
        scsi_boot_sector_read,
        "IPLROM must boot from SCSI HDD and load boot sector to $002000"
    );
    assert!(
        human_sys_resident,
        "SCSI boot loader must load HUMAN.SYS from FAT16 partition into Main RAM"
    );
}

#[test]
fn test_sxwin31_scsi_boot_and_mouse_sync() {
    let workspace_root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let ipl_path = workspace_root.join("roms/IPLROM.DAT");
    let cgrom_path = workspace_root.join("roms/CGROM.DAT");
    let human_path = workspace_root.join("disks/HUMAN302.XDF");
    let sx1_path = workspace_root.join("disks/SXWIN311.XDF");

    if !ipl_path.exists() || !cgrom_path.exists() || !human_path.exists() || !sx1_path.exists() {
        eprintln!("ROMs or SX-Window disks not found, skipping full SX-Window SCSI boot test");
        return;
    }

    let ipl_data = std::fs::read(&ipl_path).unwrap();
    let cgrom_data = std::fs::read(&cgrom_path).unwrap();
    let human_data = std::fs::read(&human_path).unwrap();

    let sx_disks = [
        ("SXWIN311", workspace_root.join("disks/SXWIN311.XDF")),
        ("SXWIN312", workspace_root.join("disks/SXWIN312.XDF")),
        ("SXWIN313", workspace_root.join("disks/SXWIN313.XDF")),
        ("SXWIN314", workspace_root.join("disks/SXWIN314.XDF")),
        ("SXWIN315", workspace_root.join("disks/SXWIN315.XDF")),
        ("SXWIN316", workspace_root.join("disks/SXWIN316.XDF")),
    ];
    let mut sx_loaded = Vec::new();
    for (name, path) in &sx_disks {
        if let Ok(bytes) = std::fs::read(path) {
            sx_loaded.push((*name, bytes));
        }
    }
    let sx_refs: Vec<(&str, &[u8])> = sx_loaded.iter().map(|(n, b)| (*n, b.as_slice())).collect();

    let hdd_image = build_bootable_sxwin_scsi_hdd(&human_data, &sx_refs, 32);

    let hds_path = workspace_root.join("disks/SXWIN31.HDS");
    let _ = std::fs::write(&hds_path, &hdd_image);

    let mut vm = X68kVm::new(
        VmConfig::master_sx()
            .with_speed_mode(CpuSpeedMode::Burst)
            .with_ram_mb(4),
    );
    vm.bus.load_cgrom(&cgrom_data);
    vm.bus.load_ipl_rom(&ipl_data);

    // Mount SCSI HDD on Target 0 and set SCSI boot mode (no floppy in FD0 or FD1)
    assert!(vm.bus.insert_scsi_hdd(0, hdd_image));
    vm.bus.set_boot_from_scsi(true);

    vm.reset().unwrap();

    // Run until SX-Window GUI initializes desktop or up to 90 chunks of 250,000 steps
    for _chunk in 1..=90 {
        vm.run_steps(250_000);
        if vm.bus.vicon.r0_mode() == 0x0004 && vm.bus.vicon.r2_display() == 0x003F {
            let tv_non_zero = vm
                .bus
                .tvram()
                .map_or(0, |tv| tv.iter().filter(|&&b| b != 0).count());
            if tv_non_zero > 140_000 {
                break;
            }
        }
    }

    // Verify VICON is configured for SX-Window 768x512 desktop
    assert_eq!(vm.bus.vicon.r0_mode(), 0x0004);
    assert_eq!(vm.bus.vicon.r2_display(), 0x003F);
    assert_eq!(vm.bus.crtc.active_resolution(), (768, 512));

    // Verify TVRAM holds rendered SX-Window desktop
    let tv_non_zero = vm
        .bus
        .tvram()
        .map_or(0, |tv| tv.iter().filter(|&&b| b != 0).count());
    assert!(
        tv_non_zero > 140_000,
        "Expected SX-Window v3.1 desktop in TVRAM (>140k non-zero bytes), got {tv_non_zero}"
    );

    // Verify FSX.X patch was applied
    let iocs_mouse_vec = vm.bus.read_u32(0x0000_0938, true).unwrap();
    assert!(iocs_mouse_vec > 0x04F82A, "FSX.X mouse vector must be installed");
    let text_base = iocs_mouse_vec - 0x04F82A;
    let patch_addr = text_base + 0x04F768;
    assert_eq!(
        vm.bus.read_u32(patch_addr, true).unwrap(),
        0xFE81_4E75,
        "FSX.X must be patched with Hypercall 0xFE81 (SxMouseSync) + RTS"
    );

    // Test 1:1 host mouse synchronization
    vm.bus.set_host_mouse_position(Some((250, 180)));
    // Trigger Timer C / SCC mouse delta update
    vm.bus.scc.add_mouse_delta(10, 10);
    for _ in 0..10 {
        vm.run_steps(100_000);
    }
    let global_x_addr = text_base + 0x549F6;
    let global_y_addr = text_base + 0x549F8;
    let cur_x = vm.bus.read_u16(global_x_addr, true).unwrap() as i16;
    let cur_y = vm.bus.read_u16(global_y_addr, true).unwrap() as i16;
    assert_eq!(
        (cur_x, cur_y),
        (250, 180),
        "SX-Window cursor coordinates at dynamic global vector (text_base + $549F6) must match host mouse position"
    );

    assert!(
        vm.bus.is_mouse_polling_active(),
        "Mouse polling must be active once SX-Window desktop is running"
    );

    let vec_a44e = vm.bus.read_u32(text_base + 0x1538, true).unwrap();
    let vec_a44f = vm.bus.read_u32(text_base + 0x153C, true).unwrap();
    let unimpl = text_base + 0x0366;
    eprintln!(
        "A44E: {:#010x}, A44F: {:#010x}, unimpl: {:#010x}",
        vec_a44e, vec_a44f, unimpl
    );
    assert_ne!(
        vec_a44e, unimpl,
        "Trap $A44E must be installed by SHELL/SXCON.X (got unimpl handler)"
    );
    assert_ne!(
        vec_a44f, unimpl,
        "Trap $A44F must be installed by SHELL/SXCON.X (got unimpl handler)"
    );

    // Verify drive mapping table at $1C7E..$1C82 and boot drive at $1C15:
    // Drive A: ($1C7E) -> Physical Drive 0 (SCSI HDD)
    // Drive B: ($1C7F) -> Physical Drive 1 (Floppy Drive 0)
    // Drive C: ($1C80) -> Physical Drive 2 (Floppy Drive 1)
    let drive_a = vm.bus.read_u8(0x0000_1C7E, true).unwrap();
    let drive_b = vm.bus.read_u8(0x0000_1C7F, true).unwrap();
    let drive_c = vm.bus.read_u8(0x0000_1C80, true).unwrap();
    let boot_drv = vm.bus.read_u8(0x0000_1C15, true).unwrap();
    eprintln!(
        "Drive map: A:={}, B:={}, C:={}, boot_drive={}",
        drive_a, drive_b, drive_c, boot_drv
    );
    assert_eq!(drive_a, 0, "Drive A: must map to SCSI HDD Unit 0");
    assert_eq!(drive_b, 1, "Drive B: must map to Floppy Drive 0");
    assert_eq!(drive_c, 2, "Drive C: must map to Floppy Drive 1");
    assert_eq!(boot_drv, 0, "Boot drive must be Drive A: (0)");

    let a4_ptr = vm.bus.read_u32(text_base + 0x633CA, true).unwrap();
    let a4_x = vm.bus.read_u16(a4_ptr + 0x0A, true).unwrap() as i16;
    let a4_y = vm.bus.read_u16(a4_ptr + 0x0C, true).unwrap() as i16;
    assert_eq!(
        (a4_x, a4_y),
        (250, 180),
        "FSX.X MouseWorkArea coordinates must match host mouse position"
    );

    eprintln!("[Desktop Idle State] PC: 0x{:08X}, SP: 0x{:08X}", vm.cpu_state.pc, vm.cpu_state.a[7]);
    let stack_dump: Vec<u32> = (0..10).map(|i| vm.bus.read_u32(vm.cpu_state.a[7] + i * 4, true).unwrap_or(0)).collect();
    eprintln!("[Stack Dump at Desktop Idle] {:08X?}", stack_dump);

    // Initial desktop boots in standard 768x512 mode
    assert_eq!(vm.bus.crtc.active_resolution(), (768, 512));

    // Spawning A:\BIN\SXMODE.X via SXCALL $A351 (TSFock) as a real Process Manager task
    let stub_addr = 0x0024_0000;
    let mut stub_bytes = Vec::new();
    stub_bytes.extend_from_slice(&[0x42, 0x67]); // 0x00: clr.w -(sp) (ID = 0)
    stub_bytes.extend_from_slice(&[0x42, 0xa7]); // 0x02: clr.l -(sp) (Type = 0)
    stub_bytes.extend_from_slice(&[0x42, 0xa7]); // 0x04: clr.l -(sp) (environ = 0)
    stub_bytes.extend_from_slice(&[0x48, 0x7a, 0x00, 0x14]); // 0x06: pea cmd_str(pc) (target 0x1c)
    stub_bytes.extend_from_slice(&[0x48, 0x7a, 0x00, 0x12]); // 0x0a: pea file_str(pc) (target 0x1e)
    stub_bytes.extend_from_slice(&[0x42, 0x67]); // 0x0e: clr.w -(sp) (mode1=0, mode2=0)
    stub_bytes.extend_from_slice(&[0xa3, 0x51]); // 0x10: SXCALL TSFock ($A351)
    stub_bytes.extend_from_slice(&[0x4f, 0xef, 0x00, 0x14]); // 0x12: lea 20(sp), sp
    stub_bytes.extend_from_slice(&[0x4e, 0x71, 0x4e, 0x71]); // 0x16: nop; nop

    // 0x1a: padding
    stub_bytes.extend_from_slice(&[0x00, 0x00]);
    // 0x1c: cmd_str = ""
    stub_bytes.extend_from_slice(&[0x00, 0x00]);
    // 0x1e: file_str
    stub_bytes.extend_from_slice(b"A:\\BIN\\SXMODE.X\0");

    for (i, &b) in stub_bytes.iter().enumerate() {
        vm.bus.write_u8(stub_addr + i as u32, b, true).unwrap();
    }

    // Clear TSEventAvail reentrancy guard bit before invoking TSFock stub
    vm.bus.write_u8(text_base + 0x1536c, 0, true).unwrap();
    vm.cpu_state.pc = stub_addr;
    vm.cpu_state.a[7] = 0x0024_0800; // Safe dedicated stack for launcher stub

    for _ in 0..100_000 {
        if vm.cpu_state.pc == stub_addr + 0x16 {
            break;
        }
        vm.step();
    }
    let tsfock_ret = vm.cpu_state.d[0];
    eprintln!("[TSFock finished] ret={}", tsfock_ret);
    assert!(tsfock_ret > 0, "TSFock must successfully spawn SXMODE task ID > 0");

    let real_a5 = vm.bus.read_u32(text_base + 0x15358, true).unwrap_or(0);
    let task_tbl_ptr = vm.bus.read_u32(real_a5 + 0x16e4, true).unwrap_or(0);
    let tcb_base = vm.bus.read_u32(task_tbl_ptr, true).unwrap_or(0);

    // Set up CPU to execute Task 6 directly as active task
    let tcb6 = tcb_base + tsfock_ret * 0x200;
    let task6_pc = vm.bus.read_u32(tcb6 + 0x1bc, true).unwrap();
    let task6_usp = vm.bus.read_u32(tcb6 + 0x1b8, true).unwrap();
    vm.bus.write_u32(tcb6 + 0x198, tcb6 + 0x5a, true).unwrap();
    vm.bus.write_u16(tcb6 + 0x166, 1, true).unwrap();
    vm.bus.write_u16(real_a5 + 0x150, tsfock_ret as u16, true).unwrap();
    vm.bus.write_u8(text_base + 0x1536c, 0, true).unwrap();

    vm.cpu_state.pc = task6_pc;
    vm.cpu_state.a[0] = tcb6;
    vm.cpu_state.a[2] = 0; // No CLI args
    vm.cpu_state.a[5] = real_a5;
    vm.cpu_state.a[7] = task6_usp;
    vm.cpu_state.sr = 0x0000; // User mode, IPL 0
    eprintln!("[Task {} Launch] PC: 0x{:08X}, SP: 0x{:08X}, A0(TCB): 0x{:08X}", tsfock_ret, task6_pc, task6_usp, tcb6);

    let mut tsfock_alines = Vec::new();
    let cfg_15c = vm.bus.read_u16(real_a5 + 0x15c, true).unwrap_or(0);
    eprintln!("[Task Table Debug] real_a5: 0x{:08X}, tbl_ptr: 0x{:08X}, tcb_base: 0x{:08X}, $15c: {}", real_a5, task_tbl_ptr, tcb_base, cfg_15c);
    for tid in 0..8 {
        let tcb = tcb_base + tid * 0x200;
        let state = vm.bus.read_u16(tcb + 0x166, true).unwrap_or(0);
        let usp = vm.bus.read_u32(tcb + 0x1b8, true).unwrap_or(0);
        let pc = vm.bus.read_u32(tcb + 0x1bc, true).unwrap_or(0);
        let ev_ptr = vm.bus.read_u32(tcb + 0x1ac, true).unwrap_or(0);
        eprintln!("  Task {}: state={}, usp=0x{:08X}, pc=0x{:08X}, ev_ptr=0x{:08X}", tid, state, usp, pc, ev_ptr);
    }
    let vec69 = vm.bus.read_u32(0x0114, true).unwrap_or(0);
    let vec70 = vm.bus.read_u32(0x0118, true).unwrap_or(0);
    let vec_vdisp = vm.bus.read_u32(0x0068, true).unwrap_or(0);
    eprintln!("[Timer Vectors] Vector 69 (0x114): 0x{:08X}, Vector 70 (0x118): 0x{:08X}, Vector 26 (V-DISP): 0x{:08X}", vec69, vec70, vec_vdisp);
    let mfp_iera = vm.bus.read_u8(0x00E88007, true).unwrap_or(0);
    let mfp_ierb = vm.bus.read_u8(0x00E88009, true).unwrap_or(0);
    let mfp_imra = vm.bus.read_u8(0x00E88013, true).unwrap_or(0);
    let mfp_imrb = vm.bus.read_u8(0x00E88015, true).unwrap_or(0);
    let mfp_isra = vm.bus.read_u8(0x00E8800F, true).unwrap_or(0);
    let mfp_isrb = vm.bus.read_u8(0x00E88011, true).unwrap_or(0);
    let mfp_tcdr = vm.bus.read_u8(0x00E88023, true).unwrap_or(0);
    eprintln!("[MFP Regs] IER: 0x{:02X}{:02X}, IMR: 0x{:02X}{:02X}, ISR: 0x{:02X}{:02X}, TCDR: {}",
        mfp_iera, mfp_ierb, mfp_imra, mfp_imrb, mfp_isra, mfp_isrb, mfp_tcdr);
    let mut timer_irq_count = 0;
    for step_i in 0..1_000_000 {
        let cur_pc = vm.cpu_state.pc;
        if cur_pc == vec69 {
            timer_irq_count += 1;
        }
        let opcode = vm.bus.read_u16(cur_pc, true).unwrap_or(0);
        if opcode == 0xA360 {
            eprintln!("[SXMODE Launch] A360 (TSGetID) at PC: 0x{:08X}", cur_pc);
        }
        if (opcode & 0xF000) == 0xA000 && !tsfock_alines.contains(&opcode) {
            let task_id = vm.bus.read_u16(real_a5 + 0x150, true).unwrap_or(999);
            eprintln!("[First Occurrence] A-line 0x{:04X} at PC 0x{:08X}, Task {}", opcode, cur_pc, task_id);
            tsfock_alines.push(opcode);
        }
        if step_i % 200_000 == 0 {
            let task_id = vm.bus.read_u16(real_a5 + 0x150, true).unwrap_or(999);
            eprintln!("[Step {}] PC: 0x{:08X}, SR: 0x{:04X}, IPL: {}, cur_task: {}",
                step_i, cur_pc, vm.cpu_state.sr, vm.bus.highest_pending_ipl(), task_id);
        }
        vm.step();
    }

    eprintln!("[TSFock alines] (ret={:?}): {:x?}, Timer IRQs: {}", tsfock_ret, tsfock_alines, timer_irq_count);
    let vec10 = vm.bus.read_u32(0x0028, true).unwrap();
    eprintln!("[Vector 10] A-line handler: 0x{:08X}", vec10);
    eprintln!("[State after TSFock loop] PC: 0x{:08X}, text_base: 0x{:08X}", vm.cpu_state.pc, text_base);
    let a5 = vm.cpu_state.a[5];
    let cur_task = vm.bus.read_u16(a5 + 0x150, true).unwrap_or(999);
    eprintln!("[Active Task ID] a5: 0x{:08X}, current task: {}", a5, cur_task);
    assert!(tsfock_ret > 0, "TSFock must successfully spawn SXMODE task ID > 0");
    for &expected in &[
        0xA360, // TSGetID
        0xA1F9, // WMOpen
        0xA131, // GMSetGraph
        0xA147, // GMForeColor
        0xA148, // GMBackColor
        0xA18D, // GMFontMode
        0xA18B, // GMFontKind
        0xA18E, // GMFontSize
        0xA207, // WMShow
        0xA357, // TSEventAvail
        0xA20D, // WMUpdate
        0xA144, // GMPenMode
        0xA173, // GMFillRect
        0xA172, // GMFrameRect
        0xA16E, // GMMove
        0xA190, // GMDrawStrL
        0xA20E, // WMUpdtOver
    ] {
        assert!(
            tsfock_alines.contains(&expected),
            "Spawned SXMODE.X task must execute A-line trap {expected:#06X} on booted SYS/FSX.X"
        );
    }

    // 1. Save screenshot before resolution change (768x512 desktop with SXMODE.X open):
    let (w0, h0, rgba0) = render_bus_tvram_rgba(&vm.bus);
    assert_eq!((w0, h0), (768, 512), "Before-click screenshot must be standard 768x512 desktop");
    if let Ok(dir) = std::env::var("MSX68K_SCREENSHOT_DIR") {
        let before_path = Path::new(&dir).join("sxmode_before_click.png");
        save_rgba_to_png(w0, h0, &rgba0, before_path.to_str().unwrap());
        eprintln!("[Screenshot 1] Saved before-click PNG ({}x{}) to {:?}", w0, h0, before_path);
    }

    // Verify that all 8 resolution buttons inside the SXMODE.X window are rendered properly:
    // Window content starts at (180, 120). Buttons span x = 200..380, height 22, stride 28.
    for i in 0..8 {
        let btn_y1 = 120 + 40 + i * 28;
        let btn_y2 = btn_y1 + 22;
        let mut dark_count = 0usize;
        let mut light_count = 0usize;
        for y in btn_y1..btn_y2 {
            for x in 200..380 {
                let idx = ((y * w0 + x) * 4) as usize;
                let r = rgba0[idx];
                let g = rgba0[idx + 1];
                let b = rgba0[idx + 2];
                if r < 50 && g < 50 && b < 50 {
                    dark_count += 1;
                } else if r > 150 && g > 150 && b > 150 {
                    light_count += 1;
                }
            }
        }
        assert!(
            dark_count > 20,
            "Button {i} must render dark frame/text pixels, found {dark_count}"
        );
        assert!(
            light_count > 50,
            "Button {i} must render light background pixels, found {light_count}"
        );
        eprintln!("[SXMODE Button {i}] dark_pixels={}, light_pixels={}", dark_count, light_count);
    }

    // 2. Simulate mouse movement and click targeting Button 2 ("1024 x 1024"):
    // In WMOpen, win_bounds (180, 120, 400, 270) specifies the content port rectangle
    // (local (0,0) = global (180, 120)).
    // Button 2 is at local (20..200, 96..118) -> global (200..380, 216..238), center (290, 227).
    vm.bus.set_host_mouse_position(Some((290, 227)));
    vm.bus.scc.add_mouse_delta(1, 1);
    for _ in 0..50_000 {
        vm.step();
    }
    let mouse_x = vm.bus.read_u16(text_base + 0x549F6, true).unwrap();
    let mouse_y = vm.bus.read_u16(text_base + 0x549F8, true).unwrap();
    eprintln!("[Click Prep] FSX mouse pos: ({}, {}), poll_count: {}", mouse_x, mouse_y, vm.bus.scc.poll_count());

    let mut click_alines = Vec::new();
    let mut click_hypers = Vec::new();
    let mut sxmode_pcs = Vec::new();

    vm.bus.scc.set_mouse_buttons(true, false);
    vm.bus.scc.add_mouse_delta(1, 1);
    for _ in 0..200_000 {
        let cur_pc = vm.cpu_state.pc;
        if (0x0D6000..=0x0D8000).contains(&cur_pc) && !sxmode_pcs.contains(&cur_pc) {
            sxmode_pcs.push(cur_pc);
        }
        let opcode = vm.bus.read_u16(cur_pc, true).unwrap_or(0);
        if (opcode & 0xF000) == 0xA000 && !click_alines.contains(&opcode) {
            click_alines.push(opcode);
        }
        if (opcode & 0xFFF0) == 0xFE80 && !click_hypers.contains(&opcode) {
            click_hypers.push(opcode);
        }
        vm.step();
    }
    vm.bus.scc.set_mouse_buttons(false, false);
    vm.bus.scc.add_mouse_delta(1, 1);
    for _ in 0..500_000 {
        let cur_pc = vm.cpu_state.pc;
        if (0x0D6000..=0x0D8000).contains(&cur_pc) && !sxmode_pcs.contains(&cur_pc) {
            sxmode_pcs.push(cur_pc);
        }
        let opcode = vm.bus.read_u16(cur_pc, true).unwrap_or(0);
        if (opcode & 0xF000) == 0xA000 && !click_alines.contains(&opcode) {
            click_alines.push(opcode);
        }
        if (opcode & 0xFFF0) == 0xFE80 && !click_hypers.contains(&opcode) {
            click_hypers.push(opcode);
        }
        vm.step();
    }
    let a5 = vm.cpu_state.a[5];
    let cur_task = vm.bus.read_u16(a5 + 0x150, true).unwrap_or(999);
    let task_tbl = vm.bus.read_u32(a5 + 0x16e4, true).unwrap_or(0);
    let cfg_13c = vm.bus.read_u16(a5 + 0x13c, true).unwrap_or(0);
    let cfg_144 = vm.bus.read_u16(a5 + 0x144, true).unwrap_or(0);
    let cfg_14c = vm.bus.read_u16(a5 + 0x14c, true).unwrap_or(0);
    let cfg_15c = vm.bus.read_u16(a5 + 0x15c, true).unwrap_or(0);
    let cfg_16c = vm.bus.read_u16(a5 + 0x16c, true).unwrap_or(0);
    let cfg_1bc = vm.bus.read_u32(a5 + 0x1bc, true).unwrap_or(0);
    let guard_1536c = vm.bus.read_u8(text_base + 0x1536c, true).unwrap_or(0);
    eprintln!("[Scheduler State] cur_task: {}, $15c(max): {}, $13c: {}, $144: {}, $14c: {}, $16c: {}, $1bc: 0x{:08X}, guard_1536c: 0x{:02X}, task_tbl: 0x{:08X}",
        cur_task, cfg_15c, cfg_13c, cfg_144, cfg_14c, cfg_16c, cfg_1bc, guard_1536c, task_tbl);
    if task_tbl > 0 {
        let task_ptr = vm.bus.read_u32(task_tbl, true).unwrap_or(0);
        eprintln!("[Task Table Base] 0x{:08X}", task_ptr);
        for tid in 0..8 {
            // In scheduler: d0 shifted or indexed into task table:
            // let us inspect memory around task_ptr
            let tcb = task_ptr + tid * 0x200; // approximate TCB stride
            let state = vm.bus.read_u16(tcb + 0x166, true).unwrap_or(0);
            let usp = vm.bus.read_u32(tcb + 0x1b8, true).unwrap_or(0);
            eprintln!("  Task {}: state=0x{:04X}, usp=0x{:08X}", tid, state, usp);
            if tid == 6 {
                let stack_words: Vec<u32> = (0..8).map(|i| vm.bus.read_u32(usp + i * 4, true).unwrap_or(0)).collect();
                eprintln!("    Task 6 Stack: {:08X?}", stack_words);
                let loop_code: Vec<u8> = (0..32).map(|i| vm.bus.read_u8(0x000E0D3A + i, true).unwrap_or(0)).collect();
                eprintln!("    Code at 0x000E0D3A: {:02X?}", loop_code);
            }
        }
    }
    eprintln!("[Click Done] A-lines: {:x?}, Hypers: {:x?}, SXMODE PCs: {:x?}, poll_count: {}", click_alines, click_hypers, sxmode_pcs, vm.bus.scc.poll_count());
    assert_eq!(
        vm.bus.crtc.active_resolution(),
        (1024, 1024),
        "Mouse click on Button 2 in SXMODE GUI must switch resolution to 1024x1024"
    );

    // Verify desktopWindow->inside, desktopGrafPort->visible, and SXMODE.X bmap->rect
    let dt_win = vm.bus.read_u32(text_base + 0x54384, true).unwrap();
    assert!(dt_win > 0);
    let inside_h = vm.bus.read_u32(dt_win + 0x4C, true).unwrap();
    assert!(inside_h > 0);
    let inside_ptr = vm.bus.read_u32(inside_h, true).unwrap();
    assert!(inside_ptr > 0);
    assert_eq!(vm.bus.read_u16(inside_ptr + 8, true).unwrap(), 1024, "desktopWindow->inside bounds.right must be 1024");
    assert_eq!(vm.bus.read_u16(inside_ptr + 10, true).unwrap(), 1024, "desktopWindow->inside bounds.bottom must be 1024");

    let dt_gp = vm.bus.read_u32(text_base + 0x5437A, true).unwrap();
    assert!(dt_gp > 0);
    let vis_h = vm.bus.read_u32(dt_gp + 0x10, true).unwrap();
    assert!(vis_h > 0);
    let vis_ptr = vm.bus.read_u32(vis_h, true).unwrap();
    assert!(vis_ptr > 0);
    assert_eq!(vm.bus.read_u16(vis_ptr + 8, true).unwrap(), 1024, "desktopGrafPort->visible bounds.right must be 1024");
    assert_eq!(vm.bus.read_u16(vis_ptr + 10, true).unwrap(), 1024, "desktopGrafPort->visible bounds.bottom must be 1024");

    let top_win = vm.bus.read_u32(dt_win + 0x80, true).unwrap();
    assert!(top_win > 0);
    let top_bmap = vm.bus.read_u32(top_win, true).unwrap();
    assert!(top_bmap > 0);
    assert_eq!(vm.bus.read_u16(top_bmap + 6, true).unwrap(), 1024, "SXMODE.X window->graph.bmap->rect.right must be 1024");
    assert_eq!(vm.bus.read_u16(top_bmap + 8, true).unwrap(), 1024, "SXMODE.X window->graph.bmap->rect.bottom must be 1024");

    // Inspect regions requested by parent
    let inspect_rgn = |name: &str, h_addr: u32, bus: &mut msx68k_bus::X68kBus| {
        let h = bus.read_u32(h_addr, true).unwrap_or(0);
        if h > 0 {
            let ptr = bus.read_u32(h, true).unwrap_or(0);
            if ptr > 0 {
                let size = bus.read_u32(ptr, true).unwrap_or(0);
                let l = bus.read_u16(ptr + 4, true).unwrap_or(0) as i16;
                let t = bus.read_u16(ptr + 6, true).unwrap_or(0) as i16;
                let r = bus.read_u16(ptr + 8, true).unwrap_or(0) as i16;
                let b = bus.read_u16(ptr + 10, true).unwrap_or(0) as i16;
                eprintln!("  [RGN] {}: h=0x{:08X}, ptr=0x{:08X}, size={}, bounds=({}, {}, {}, {})",
                    name, h, ptr, size, l, t, r, b);
                if size > 12 {
                    let mut data_words = Vec::new();
                    for i in 0..((size - 12) / 2).min(16) {
                        data_words.push(bus.read_u16(ptr + 12 + i * 2, true).unwrap_or(0));
                    }
                    eprintln!("        data: {:04X?}", data_words);
                }
            } else {
                eprintln!("  [RGN] {}: h=0x{:08X}, ptr=NULL", name, h);
            }
        } else {
            eprintln!("  [RGN] {}: h=NULL", name);
        }
    };

    eprintln!("[Desktop & Window Regions after 1024x1024 switch]:");
    inspect_rgn("desktopGrafPort->visible", dt_gp + 0x10, &mut vm.bus);
    inspect_rgn("desktopWindow->visRgn", dt_win + 0x10, &mut vm.bus);
    inspect_rgn("desktopWindow->outside", dt_win + 0x48, &mut vm.bus);
    inspect_rgn("desktopWindow->inside", dt_win + 0x4C, &mut vm.bus);
    inspect_rgn("desktopWindow->rgn88", dt_win + 0x88, &mut vm.bus);
    inspect_rgn("desktopWindow->rgn8C", dt_win + 0x8C, &mut vm.bus);
    inspect_rgn("SXMODE->visRgn", top_win + 0x10, &mut vm.bus);
    inspect_rgn("SXMODE->clipping", top_win + 0x14, &mut vm.bus);
    inspect_rgn("SXMODE->outside", top_win + 0x48, &mut vm.bus);
    inspect_rgn("SXMODE->inside", top_win + 0x4C, &mut vm.bus);

    // Drag SXMODE.X's window to (800, 550) across the 768x512 boundary
    vm.bus.set_host_mouse_position(Some((250, 105)));
    vm.bus.scc.add_mouse_delta(1, 1);
    for _ in 0..50_000 {
        vm.step();
    }
    let m_x = vm.bus.read_u16(text_base + 0x549F6, true).unwrap();
    let m_y = vm.bus.read_u16(text_base + 0x549F8, true).unwrap();
    eprintln!("[Drag Prep] mouse pos: ({}, {})", m_x, m_y);

    vm.bus.scc.set_mouse_buttons(true, false);
    vm.bus.scc.add_mouse_delta(1, 1);
    for _ in 0..100_000 {
        vm.step();
    }

    vm.bus.set_host_mouse_position(Some((800, 550)));
    vm.bus.scc.add_mouse_delta(1, 1);
    for _ in 0..200_000 {
        vm.step();
    }

    vm.bus.scc.set_mouse_buttons(false, false);
    vm.bus.scc.add_mouse_delta(1, 1);
    for _ in 0..500_000 {
        vm.step();
    }

    eprintln!("[Regions after dragging SXMODE across 768x512 boundary]:");
    inspect_rgn("SXMODE->visRgn", top_win + 0x10, &mut vm.bus);
    inspect_rgn("SXMODE->outside", top_win + 0x48, &mut vm.bus);
    inspect_rgn("SXMODE->inside", top_win + 0x4C, &mut vm.bus);

    let sx_inside_h = vm.bus.read_u32(top_win + 0x4C, true).unwrap();
    let sx_inside_ptr = vm.bus.read_u32(sx_inside_h, true).unwrap();
    let sx_inside_l = vm.bus.read_u16(sx_inside_ptr + 4, true).unwrap() as i16;
    let sx_inside_t = vm.bus.read_u16(sx_inside_ptr + 6, true).unwrap() as i16;
    let sx_inside_r = vm.bus.read_u16(sx_inside_ptr + 8, true).unwrap() as i16;
    let sx_inside_b = vm.bus.read_u16(sx_inside_ptr + 10, true).unwrap() as i16;
    assert!(sx_inside_r > 768, "Moved window inside.right must extend beyond 768, got {sx_inside_r}");
    assert!(sx_inside_b > 512, "Moved window inside.bottom must extend beyond 512, got {sx_inside_b}");
    assert_eq!(sx_inside_r - sx_inside_l, 220, "Moved window inside width must be 220");
    assert_eq!(sx_inside_b - sx_inside_t, 276, "Moved window inside height must be 276");

    let sx_vis_h = vm.bus.read_u32(top_win + 0x10, true).unwrap();
    let sx_vis_ptr = vm.bus.read_u32(sx_vis_h, true).unwrap();
    let sx_vis_w = vm.bus.read_u16(sx_vis_ptr + 8, true).unwrap() as i16;
    let sx_vis_h_val = vm.bus.read_u16(sx_vis_ptr + 10, true).unwrap() as i16;
    assert_eq!(sx_vis_w, 220, "SXMODE visible region width must not be clipped, expected 220, got {sx_vis_w}");
    assert_eq!(sx_vis_h_val, 276, "SXMODE visible region height must not be clipped, expected 276, got {sx_vis_h_val}");

    // Save screenshot after moving SXMODE across the 768x512 boundary in 1024x1024
    let (wm, hm, rgbam) = render_bus_tvram_rgba(&vm.bus);
    assert_eq!((wm, hm), (1024, 1024));
    if let Ok(dir) = std::env::var("MSX68K_SCREENSHOT_DIR") {
        let moved_path = Path::new(&dir).join("sxmode_moved_beyond_768x512.png");
        save_rgba_to_png(wm, hm, &rgbam, moved_path.to_str().unwrap());
        eprintln!("[Screenshot Moved] Saved moved SXMODE PNG ({}x{}) to {:?}", wm, hm, moved_path);
    }

    // Verify A:\SYS\MSX68K.CFG was created on the SCSI disk by SXMODE.X
    let hdd_data = vm.bus.scsi.hdd_data(0).expect("SCSI HDD 0 must be present");
    let has_cfg = hdd_data.windows(b"SCREEN_WIDTH=".len()).any(|w| w == b"SCREEN_WIDTH=");
    assert!(has_cfg, "A:\\SYS\\MSX68K.CFG must be persisted on SCSI HDD by SXMODE.X");
    assert_eq!(vm.bus.sram()[0x7E], 0x80 | 0x30, "SRAM crtmode must be $B0 (extended)");

    // Switch to Beyond 1K×1K (1920x1080) via IOCS _CRTMOD Mode $50
    vm.cpu_state.d[0] = 0x10;
    vm.cpu_state.d[1] = 0x0150;
    vm.cpu_state.d[2] = (1920 << 16) | 1080;
    let trap_res = vm.bus.handle_iocs_crtmod(&mut vm.cpu_state);
    assert!(matches!(trap_res, msx68k_cpu::TrapResult::Handled { .. }));

    assert_eq!(vm.bus.crtc.active_resolution(), (1920, 1080), "Resolution must switch to 1920x1080");
    assert!(vm.bus.is_ext_tvram_active(), "ext_tvram must be active at 1920x1080");
    assert_eq!(vm.bus.active_tvram_row_bytes(), 240, "active_tvram_row_bytes must be 240 for 1920");
    assert_eq!(vm.bus.active_tvram_plane_bytes(), 240 * 1080, "active_tvram_plane_bytes must be 240 * 1080");

    // Drag SXMODE.X window out to (1300, 750) well beyond 1024x1024
    vm.bus.set_host_mouse_position(Some((sx_inside_l + 50, sx_inside_t - 8)));
    vm.bus.scc.add_mouse_delta(1, 1);
    for _ in 0..50_000 {
        vm.step();
    }
    vm.bus.scc.set_mouse_buttons(true, false);
    vm.bus.scc.add_mouse_delta(1, 1);
    for _ in 0..100_000 {
        vm.step();
    }
    vm.bus.set_host_mouse_position(Some((1300 + 50, 750 - 8)));
    vm.bus.scc.add_mouse_delta(1, 1);
    for _ in 0..200_000 {
        vm.step();
    }
    vm.bus.scc.set_mouse_buttons(false, false);
    vm.bus.scc.add_mouse_delta(1, 1);
    for _ in 0..500_000 {
        vm.step();
    }

    let sx_inside_h2 = vm.bus.read_u32(top_win + 0x4C, true).unwrap();
    let sx_inside_ptr2 = vm.bus.read_u32(sx_inside_h2, true).unwrap();
    let sx_inside_l2 = vm.bus.read_u16(sx_inside_ptr2 + 4, true).unwrap() as i16;
    let sx_inside_t2 = vm.bus.read_u16(sx_inside_ptr2 + 6, true).unwrap() as i16;
    let sx_inside_r2 = vm.bus.read_u16(sx_inside_ptr2 + 8, true).unwrap() as i16;
    let sx_inside_b2 = vm.bus.read_u16(sx_inside_ptr2 + 10, true).unwrap() as i16;
    assert!(sx_inside_r2 > 1300, "Moved window inside.right must extend beyond 1300, got {sx_inside_r2}");
    assert!(sx_inside_b2 > 750, "Moved window inside.bottom must extend beyond 750, got {sx_inside_b2}");

    let sx_vis_h2 = vm.bus.read_u32(top_win + 0x10, true).unwrap();
    let sx_vis_ptr2 = vm.bus.read_u32(sx_vis_h2, true).unwrap();
    let vis_w2 = vm.bus.read_u16(sx_vis_ptr2 + 8, true).unwrap() as i16;
    let vis_h_val2 = vm.bus.read_u16(sx_vis_ptr2 + 10, true).unwrap() as i16;
    assert_eq!(vis_w2, 220, "SXMODE visible width must not be clipped, expected 220, got {vis_w2}");
    assert_eq!(vis_h_val2, 276, "SXMODE visible height must not be clipped, expected 276, got {vis_h_val2}");
    assert!(sx_inside_l2 + vis_w2 > 1300, "Global visible right must extend beyond 1300");
    assert!(sx_inside_t2 + vis_h_val2 > 750, "Global visible bottom must extend beyond 750");

    // Render 1920x1080 frame and check rendered window pixels at x > 1300, y > 750
    let (w1080, h1080, rgba1080) = render_bus_tvram_rgba(&vm.bus);
    assert_eq!((w1080, h1080), (1920, 1080));
    let sample_idx = ((780 * 1920 + 1350) * 4) as usize;
    let px = &rgba1080[sample_idx..sample_idx + 4];
    eprintln!("[1920x1080 Window Sample Pixel at (1350, 780)]: RGBA={:?}", px);
    assert_eq!(px[3], 0xFF, "Sample pixel alpha must be 0xFF");

    // Verify that the old cursor background save buffer was correctly rescaled and
    // no stale remnant exists at (1440..1472, 293..308) in 1920x1080 mode:
    for y in 293..309 {
        for x in 1440..1472 {
            let idx = ((y * 1920 + x) * 4) as usize;
            let r = rgba1080[idx];
            let g = rgba1080[idx + 1];
            let b = rgba1080[idx + 2];
            let is_desktop =
                (r == 116 && g == 125 && b == 125) || (r == 177 && g == 185 && b == 185);
            assert!(
                is_desktop,
                "Stale cursor background remnant detected at ({x}, {y}): RGBA=[{r}, {g}, {b}]"
            );
        }
    }

    if let Ok(dir) = std::env::var("MSX68K_SCREENSHOT_DIR") {
        let path1080 = Path::new(&dir).join("sxmode_1920x1080_extended.png");
        save_rgba_to_png(w1080, h1080, &rgba1080, path1080.to_str().unwrap());
        eprintln!("[Screenshot 1920x1080] Saved 1920x1080 PNG to {:?}", path1080);
    }

    // Verify Fix 1: Global menuGrafPort (*(text_base + 0x54AE0)) and Menubar Window (*(text_base + 0x543C6))
    let menu_gp_ptr = vm.bus.read_u32(text_base + 0x54AE0, true).unwrap();
    assert!(menu_gp_ptr > 0, "menuGrafPort must be allocated by MNInit");
    let menu_gp_w = vm.bus.read_u16(menu_gp_ptr + 8, true).unwrap();
    let menu_gp_h = vm.bus.read_u16(menu_gp_ptr + 10, true).unwrap();
    assert_eq!((menu_gp_w, menu_gp_h), (1920, 1080), "menuGrafPort rect must be expanded to 1920x1080");
    let menu_gp_bm = vm.bus.read_u32(menu_gp_ptr, true).unwrap();
    assert!(menu_gp_bm > 0);
    assert_eq!(vm.bus.read_u16(menu_gp_bm + 6, true).unwrap(), 1920, "menuGrafPort bmap width must be 1920");
    assert_eq!(vm.bus.read_u16(menu_gp_bm + 8, true).unwrap(), 1080, "menuGrafPort bmap height must be 1080");
    assert_eq!(vm.bus.read_u32(menu_gp_bm + 10, true).unwrap(), msx68k_bus::EXT_TVRAM_BASE, "menuGrafPort baseAddr must be EXT_TVRAM_BASE");
    assert_eq!(vm.bus.read_u16(menu_gp_bm + 14, true).unwrap(), 240, "menuGrafPort rowBytes must be 240");

    let menubar_win = vm.bus.read_u32(text_base + 0x543C6, true).unwrap();
    assert!(menubar_win > 0, "menubar_win must be present at text_base + 0x543C6");
    assert_eq!(vm.bus.read_u16(menubar_win + 8, true).unwrap(), 1920, "menubar_win rect.right must be 1920");
    let menubar_bm = vm.bus.read_u32(menubar_win, true).unwrap();
    assert!(menubar_bm > 0);
    assert_eq!(vm.bus.read_u16(menubar_bm + 6, true).unwrap(), 1920, "menubar_bm width must be 1920");

    // Perform right-click popup menu on desktop at (1500, 200) well into extended TVRAM:
    vm.bus.set_host_mouse_position(Some((1500, 200)));
    vm.bus.scc.add_mouse_delta(1, 1);
    for _ in 0..50_000 {
        vm.step();
    }
    vm.bus.scc.set_mouse_buttons(false, true); // Right-click down
    vm.bus.scc.add_mouse_delta(1, 1);
    for _ in 0..200_000 {
        vm.step();
    }
    // Release right-click
    vm.bus.scc.set_mouse_buttons(false, false);
    vm.bus.scc.add_mouse_delta(1, 1);
    for _ in 0..200_000 {
        vm.step();
    }

    // Verify Fix 2: Resolution switch cycle (1920x1080 -> 768x512 -> 1920x1080)
    // with SXMODE.X remaining at (1300, 750) and preserving window pixels:
    switch_resolution_at_task_turn(&mut vm, 768, 512);
    assert_eq!(vm.bus.crtc.active_resolution(), (768, 512));
    assert!(!vm.bus.is_ext_tvram_active());
    for _ in 0..100_000 {
        vm.step();
    }

    // Switch back to 1920x1080:
    switch_resolution_at_task_turn(&mut vm, 1920, 1080);
    assert_eq!(vm.bus.crtc.active_resolution(), (1920, 1080));
    assert!(vm.bus.is_ext_tvram_active());
    for _ in 0..100_000 {
        vm.step();
    }

    let (w1080_restored, h1080_restored, rgba1080_restored) = render_bus_tvram_rgba(&vm.bus);
    assert_eq!((w1080_restored, h1080_restored), (1920, 1080));
    let restored_px = &rgba1080_restored[sample_idx..sample_idx + 4];
    eprintln!("[1920x1080 Restored Sample Pixel at (1350, 780)]: RGBA={:?}", restored_px);
    assert_eq!(restored_px, px, "Window pixels at (1350, 780) must remain intact across resolution switch cycle");

    // Verify SXMODE.X window regions remain unclipped after resolution switch cycle:
    let sx_vis_h_restored = vm.bus.read_u32(top_win + 0x10, true).unwrap();
    let sx_vis_ptr_restored = vm.bus.read_u32(sx_vis_h_restored, true).unwrap();
    let vis_w_restored = vm.bus.read_u16(sx_vis_ptr_restored + 8, true).unwrap() as i16;
    let vis_h_restored = vm.bus.read_u16(sx_vis_ptr_restored + 10, true).unwrap() as i16;
    assert_eq!(vis_w_restored, 220, "SXMODE visible width must be 220 after resolution switch cycle");
    assert_eq!(vis_h_restored, 276, "SXMODE visible height must be 276 after resolution switch cycle");

    // Verify SXMODE.X execution on the running SX-Window system:
    let sxmode_src = workspace_root.join("guest/sxmode/sxmode.s");
    let inc_dir = workspace_root.join("guest/include");
    let sxmode_bytes = msx68k_bus::guest_build::build_guest_x_executable(&sxmode_src, &[&inc_dir])
        .expect("Must build SXMODE.X");

    // Load SXMODE.X into high RAM (at 0x0020_0000)
    let app_load_addr = 0x0020_0000;
    let text_size = u32::from_be_bytes([sxmode_bytes[12], sxmode_bytes[13], sxmode_bytes[14], sxmode_bytes[15]]);
    let text_payload = &sxmode_bytes[64..64 + text_size as usize];
    for (i, &b) in text_payload.iter().enumerate() {
        vm.bus.write_u8(app_load_addr + i as u32, b, true).unwrap();
    }

    // Test CLI execution: "SXMODE 2" (switch to 1024x1024)
    let cmdline_addr = 0x0022_0000;
    let cmd_str = b"\x012\0";
    for (i, &b) in cmd_str.iter().enumerate() {
        vm.bus.write_u8(cmdline_addr + i as u32, b, true).unwrap();
    }

    vm.cpu_state.a[0] = 0x0021_0000; // PSP
    vm.cpu_state.a[2] = cmdline_addr; // Command line buffer pointer
    vm.cpu_state.a[7] = 0x0023_0000; // Stack pointer
    vm.cpu_state.pc = app_load_addr + 0x0020; // _start (offset 0x20 in text)

    for _ in 0..500 {
        let opcode = vm.bus.read_u16(vm.cpu_state.pc, true).unwrap_or(0);
        if opcode == 0xFF4C {
            break;
        }
        vm.step();
    }
    assert_eq!(vm.bus.crtc.active_resolution(), (1024, 1024), "SXMODE 2 CLI must switch to 1024x1024");
    assert_eq!(vm.bus.read_u16(text_base + 0x55278 + 6, true).unwrap(), 1024);
    assert_eq!(vm.bus.read_u16(text_base + 0x55278 + 8, true).unwrap(), 1024);

    // Verify Desktop GrafPort, BitMap, and Region bounding boxes were updated to 1024x1024
    let dtgs_addr = text_base + 0x54380;
    let gp_addr = vm.bus.read_u32(dtgs_addr + 4, true).unwrap();
    assert!(gp_addr > 0);
    assert_eq!(vm.bus.read_u16(gp_addr + 8, true).unwrap(), 1024);
    assert_eq!(vm.bus.read_u16(gp_addr + 10, true).unwrap(), 1024);
    let bm_addr = vm.bus.read_u32(gp_addr, true).unwrap();
    assert!(bm_addr > 0);
    assert_eq!(vm.bus.read_u16(bm_addr + 6, true).unwrap(), 1024);
    assert_eq!(vm.bus.read_u16(bm_addr + 8, true).unwrap(), 1024);

    let vis_h = vm.bus.read_u32(gp_addr + 0x10, true).unwrap();
    assert!(vis_h > 0);
    let vis_ptr = vm.bus.read_u32(vis_h, true).unwrap();
    assert!(vis_ptr > 0);
    assert_eq!(vm.bus.read_u16(vis_ptr + 8, true).unwrap(), 1024);
    assert_eq!(vm.bus.read_u16(vis_ptr + 10, true).unwrap(), 1024);

    // Verify TVRAM contains rendered desktop pattern in extended region (x >= 768 and y >= 512)
    let tv = vm.bus.tvram().expect("TVRAM must be allocated");
    let ext_x_byte = 50 * 128 + 52 * 2; // y=50, x=832 (>768)
    let ext_x_word = u16::from_be_bytes([tv[ext_x_byte], tv[ext_x_byte + 1]]);
    assert_ne!(ext_x_word, 0, "Extended horizontal area (x > 768) must contain desktop pattern");
    let ext_y_byte = 600 * 128 + 20 * 2; // y=600 (>512)
    let ext_y_word = u16::from_be_bytes([tv[ext_y_byte], tv[ext_y_byte + 1]]);
    assert_ne!(ext_y_word, 0, "Extended vertical area (y > 512) must contain desktop pattern");

    // Save actual 1024x1024 screenshot if requested:
    let (w1024, h1024, rgba1024) = render_bus_tvram_rgba(&vm.bus);
    assert_eq!((w1024, h1024), (1024, 1024));
    if let Ok(dir) = std::env::var("MSX68K_SCREENSHOT_DIR") {
        let after_path = Path::new(&dir).join("sxmode_after_1024x1024.png");
        save_rgba_to_png(w1024, h1024, &rgba1024, after_path.to_str().unwrap());
        eprintln!("[Screenshot 2] Saved true 1024x1024 PNG ({}x{}) to {:?}", w1024, h1024, after_path);
    }

    // Verify VM continues running and processing V-Blank without hanging at 1024x1024:
    let initial_frames = vm.bus.crtc.frame_count();
    for _ in 0..100_000 {
        vm.step();
    }
    let after_frames = vm.bus.crtc.frame_count();
    eprintln!("[1024x1024 V-Blank test] frame count: {} -> {} (advanced by {})", initial_frames, after_frames, after_frames - initial_frames);
    assert!(after_frames > initial_frames, "CRTC frame count must advance on 1024x1024 mode without hanging!");

    // Test GUI entry via gui_start with CLI argument "1024x768"
    let gui_entry = u32::from_be_bytes([sxmode_bytes[72], sxmode_bytes[73], sxmode_bytes[74], sxmode_bytes[75]]);
    let cmd_str1 = b"\x0b1024x768\0";
    for (i, &b) in cmd_str1.iter().enumerate() {
        vm.bus.write_u8(cmdline_addr + i as u32, b, true).unwrap();
    }
    vm.cpu_state.a[0] = 0x0021_0000;
    vm.cpu_state.a[2] = cmdline_addr;
    vm.cpu_state.a[7] = 0x0023_0000;
    vm.cpu_state.pc = app_load_addr + gui_entry;

    for _ in 0..500 {
        let opcode = vm.bus.read_u16(vm.cpu_state.pc, true).unwrap_or(0);
        if opcode == 0xFF4C {
            break;
        }
        vm.step();
    }
    assert_eq!(vm.bus.crtc.active_resolution(), (1024, 768), "SXMODE 1024x768 via gui_start must switch to 1024x768");

    // Verify popup menu engine remains unpatched (original MOVEA.L 24(A6), A5)
    assert_eq!(
        vm.bus.read_u16(text_base + 0x049FE6, true).unwrap(),
        0x2A6E,
        "Popup menu bounds must remain original MOVEA.L 24(A6), A5"
    );

    // Verify TMSetWrap remains unpatched (original BEQ.W +$84 and BNE.S)
    assert_eq!(
        vm.bus.read_u8(text_base + 0x00D838 + 8, true).unwrap(),
        0x67,
        "TMSetWrap branch must remain original BEQ.W"
    );
    assert_eq!(
        vm.bus.read_u16(text_base + 0x00D8AC + 8, true).unwrap(),
        0x6610,
        "TMSetWrap branch must remain original BNE.S"
    );

    // Verify Fix 3: soft_reset resets CRTC, extended modes, and re-mirrors IPLROM vectors
    vm.bus.soft_reset();
    assert_eq!(vm.bus.crtc.active_resolution(), (768, 512));
    assert!(!vm.bus.is_ext_tvram_active());
    assert_eq!(vm.bus.max_desktop_size(), (0, 0));
    assert_eq!(
        &vm.bus.main_ram()[0..8],
        &ipl_data[ipl_data.len() - 64 * 1024..][..8]
    );
}

#[test]
fn test_burst_frame_peripheral_timing_pacing() {
    let mut vm = X68kVm::new(
        VmConfig::master_sx()
            .with_speed_mode(CpuSpeedMode::Burst)
            .with_ram_mb(4),
    );

    // Initial frame count
    let initial_frames = vm.bus.crtc.frame_count();
    assert_eq!(initial_frames, 0);

    // Execute 60 frames paced at 1/60s (166_667 ticks per frame = 10,000,020 ticks = 1.0 wall-clock second)
    const TARGET_TICKS_60HZ: u32 = 10_000_000 / 60;
    for _ in 0..60 {
        vm.run_burst_frame(250_000, TARGET_TICKS_60HZ);
    }

    let elapsed_frames = vm.bus.crtc.frame_count();
    // In standard 31.5 kHz mode (568 scanlines * 317 cycles = 180,056 cycles/V-Blank frame, 55.54 Hz),
    // 10,000,020 cycles produces exactly 55 or 56 V-Blank frames in 1.0 second.
    // (Without burst frame pacing, 60 frames * 250,000 * 8 = 120,000,000 cycles would produce 666+ frames!)
    assert!(
        (54..=57).contains(&elapsed_frames),
        "Expected ~55 V-Blank frames in 1.0s of paced burst execution, got {elapsed_frames}"
    );

    // Also verify that unpaced run_steps still runs at full unpaced speed for fast headless tests
    let unpaced_start = vm.bus.crtc.frame_count();
    vm.run_steps(250_000); // 250,000 * 8 = 2,000,000 ticks = ~11 frames in a single call
    let unpaced_frames = vm.bus.crtc.frame_count() - unpaced_start;
    assert!(
        unpaced_frames >= 10,
        "run_steps should remain unpaced for headless batch tests, advanced {unpaced_frames} frames"
    );
}

#[test]
fn test_sx_set_screen_mode_hypercall_and_resident_patch() {
    let mut vm = X68kVm::new_app_vm();

    // 1. Initial resolution is 768x512
    assert_eq!(vm.bus.crtc.active_resolution(), (768, 512));

    // Place Hypercall 0xFE88 + RTS at $0001_0000
    let code_addr = 0x0001_0000;
    vm.bus.write_u16(code_addr, 0xFE88, true).unwrap();
    vm.bus.write_u16(code_addr + 2, 0x4E75, true).unwrap();

    // Mock resident SYS/FSX.X kernel structures in Main RAM
    let text_base: u32 = 0x0003_0000;
    // Set IOCS mouse vector $0000_0938
    vm.bus
        .write_u32(0x0000_0938, text_base + 0x04F82A, true)
        .unwrap();

    // ActiveScreenModeRecord at text_base + 0x55278 (width at +6, height at +8)
    let active_rec = text_base + 0x55278;
    vm.bus.write_u16(active_rec + 6, 768, true).unwrap();
    vm.bus.write_u16(active_rec + 8, 512, true).unwrap();

    // Physical CRT mode 16 table at text_base + 0x15200
    let crt16 = text_base + 0x15200;
    vm.bus.write_u16(crt16, 768, true).unwrap();
    vm.bus.write_u16(crt16 + 2, 512, true).unwrap();

    // Desktop GrafPort at text_base + 0x54380
    let dtgs = text_base + 0x54380;
    let gp = text_base + 0x57000;
    vm.bus.write_u32(dtgs + 4, gp, true).unwrap();
    vm.bus.write_u16(gp + 4, 0, true).unwrap();
    vm.bus.write_u16(gp + 6, 0, true).unwrap();
    vm.bus.write_u16(gp + 8, 768, true).unwrap();
    vm.bus.write_u16(gp + 10, 512, true).unwrap();

    // MouseWorkArea at text_base + 0x5CA44
    let a4 = text_base + 0x5CA44;
    vm.bus.write_u32(text_base + 0x633CA, a4, true).unwrap();
    // bounds: min=(0,0), max=(768, 512)
    vm.bus.write_u16(a4 + 0x34, 0, true).unwrap();
    vm.bus.write_u16(a4 + 0x36, 0, true).unwrap();
    vm.bus.write_u16(a4 + 0x38, 768, true).unwrap();
    vm.bus.write_u16(a4 + 0x3A, 512, true).unwrap();
    // mouse pos: (500, 400)
    vm.bus.write_u16(a4 + 0x0A, 500, true).unwrap();
    vm.bus.write_u16(a4 + 0x0C, 400, true).unwrap();

    // Test A: Query Mode (D1 = -1)
    vm.cpu_state.d[1] = 0xFFFF_FFFF;
    vm.cpu_state.pc = code_addr;
    vm.step();
    assert_eq!(vm.cpu_state.d[0], 0, "Initial query should return mode 0");
    assert_eq!(vm.cpu_state.d[1], 768);
    assert_eq!(vm.cpu_state.d[2], 512);

    // Test B: Switch to Mode 1 (1024x768)
    vm.cpu_state.d[1] = 1;
    vm.cpu_state.pc = code_addr;
    vm.step();
    assert_eq!(vm.cpu_state.d[0], 1, "Should return mode 1");
    assert_eq!(vm.cpu_state.d[1], 1024);
    assert_eq!(vm.cpu_state.d[2], 768);
    assert_eq!(vm.bus.crtc.active_resolution(), (1024, 768));
    assert_eq!(vm.bus.crtc.regs[2], 0);
    assert_eq!(vm.bus.crtc.regs[3], 128);
    assert_eq!(vm.bus.crtc.regs[6], 0);
    assert_eq!(vm.bus.crtc.regs[7], 768);
    assert_eq!(vm.bus.crtc.regs[10], 0);
    assert_eq!(vm.bus.crtc.regs[11], 0);

    // Check SRAM persistence ($ED0072..$ED007E)
    let sram_sig_s = vm.bus.read_u8(0x00ED_0072, true).unwrap();
    let sram_sig_x = vm.bus.read_u8(0x00ED_0073, true).unwrap();
    assert_eq!(sram_sig_s, b'S');
    assert_eq!(sram_sig_x, b'X');
    let sram_vmode = vm.bus.read_u8(0x00ED_007C, true).unwrap();
    let sram_crtmode = vm.bus.read_u8(0x00ED_007E, true).unwrap();
    assert_eq!(sram_vmode, (4 << 4) | 0x03);
    assert_eq!(sram_crtmode, 0x80 | 0x30);

    // Check patched resident structures
    assert_eq!(vm.bus.read_u16(active_rec + 6, true).unwrap(), 1024);
    assert_eq!(vm.bus.read_u16(active_rec + 8, true).unwrap(), 768);
    assert_eq!(vm.bus.read_u16(crt16, true).unwrap(), 1024);
    assert_eq!(vm.bus.read_u16(crt16 + 2, true).unwrap(), 768);
    assert_eq!(vm.bus.read_u16(gp + 8, true).unwrap(), 1024);
    assert_eq!(vm.bus.read_u16(gp + 10, true).unwrap(), 768);
    assert_eq!(vm.bus.read_u16(a4 + 0x38, true).unwrap(), 1024);
    assert_eq!(vm.bus.read_u16(a4 + 0x3A, true).unwrap(), 768);

    // Test C: Switch to Mode 2 (1024x1024)
    vm.cpu_state.d[1] = 2;
    vm.cpu_state.pc = code_addr;
    vm.step();
    assert_eq!(vm.cpu_state.d[0], 2, "Should return mode 2");
    assert_eq!(vm.cpu_state.d[1], 1024);
    assert_eq!(vm.cpu_state.d[2], 1024);
    assert_eq!(vm.bus.crtc.active_resolution(), (1024, 1024));
    let sram_vmode2 = vm.bus.read_u8(0x00ED_007C, true).unwrap();
    assert_eq!(sram_vmode2, (6 << 4) | 0x03);
    assert_eq!(vm.bus.read_u8(0x00ED_007E, true).unwrap(), 0x80 | 0x30);
    assert_eq!(vm.bus.read_u16(active_rec + 8, true).unwrap(), 1024);
    assert_eq!(vm.bus.read_u16(a4 + 0x3A, true).unwrap(), 1024);

    // Test D: Switch back to Mode 0 (768x512) and check clamping of mouse
    // Place mouse at (900, 800) before switching down
    vm.bus.write_u16(a4 + 0x0A, 900, true).unwrap();
    vm.bus.write_u16(a4 + 0x0C, 800, true).unwrap();
    vm.cpu_state.d[1] = 0;
    vm.cpu_state.pc = code_addr;
    vm.step();
    assert_eq!(vm.cpu_state.d[0], 0, "Should return mode 0");
    assert_eq!(vm.cpu_state.d[1], 768);
    assert_eq!(vm.cpu_state.d[2], 512);
    assert_eq!(vm.bus.crtc.active_resolution(), (768, 512));
    assert_eq!(vm.bus.read_u8(0x00ED_007E, true).unwrap(), 0x80 | 16);
    assert_eq!(vm.bus.read_u16(a4 + 0x38, true).unwrap(), 768);
    assert_eq!(vm.bus.read_u16(a4 + 0x3A, true).unwrap(), 512);
    // Mouse coords must have been clamped to (767, 511)
    let clamped_x = vm.bus.read_u16(a4 + 0x0A, true).unwrap();
    let clamped_y = vm.bus.read_u16(a4 + 0x0C, true).unwrap();
    assert_eq!(clamped_x, 767);
    assert_eq!(clamped_y, 511);
}

#[test]
fn test_sxwin31_scsi_boot_with_1024x768_initial_resolution() {
    let workspace_root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let ipl_path = workspace_root.join("roms/IPLROM.DAT");
    let cgrom_path = workspace_root.join("roms/CGROM.DAT");
    let human_path = workspace_root.join("disks/HUMAN302.XDF");
    if !ipl_path.exists() || !cgrom_path.exists() || !human_path.exists() {
        eprintln!("ROMs or HUMAN302.XDF not found, skipping boot test");
        return;
    }

    let ipl_data = std::fs::read(&ipl_path).unwrap();
    let cgrom_data = std::fs::read(&cgrom_path).unwrap();
    let human_data = std::fs::read(&human_path).unwrap();

    let sx_disks = [
        ("SXWIN311", workspace_root.join("disks/SXWIN311.XDF")),
        ("SXWIN312", workspace_root.join("disks/SXWIN312.XDF")),
        ("SXWIN313", workspace_root.join("disks/SXWIN313.XDF")),
        ("SXWIN314", workspace_root.join("disks/SXWIN314.XDF")),
        ("SXWIN315", workspace_root.join("disks/SXWIN315.XDF")),
        ("SXWIN316", workspace_root.join("disks/SXWIN316.XDF")),
    ];
    let mut sx_loaded = Vec::new();
    for (name, path) in &sx_disks {
        if let Ok(bytes) = std::fs::read(path) {
            sx_loaded.push((*name, bytes));
        }
    }
    let sx_refs: Vec<(&str, &[u8])> = sx_loaded.iter().map(|(n, b)| (*n, b.as_slice())).collect();
    let hdd_image = build_bootable_sxwin_scsi_hdd(&human_data, &sx_refs, 32);

    let hds_path = workspace_root.join("disks/SXWIN31.HDS");
    let _ = std::fs::write(&hds_path, &hdd_image);

    let mut vm = X68kVm::new(
        VmConfig::master_sx()
            .with_speed_mode(CpuSpeedMode::Burst)
            .with_ram_mb(4),
    );

    vm.bus.load_cgrom(&cgrom_data);
    vm.bus.load_ipl_rom(&ipl_data);

    assert!(vm.bus.insert_scsi_hdd(0, hdd_image));
    vm.bus.set_boot_from_scsi(true);

    vm.reset().unwrap();

    // Pre-configure initial resolution to 1024x768 (as if passed via CLI --resolution 1024x768)
    vm.bus.apply_screen_resolution(1024, 768);

    // Boot until SX-Window reaches desktop
    for _chunk in 1..=90 {
        vm.run_steps(250_000);
        if vm.bus.vicon.r0_mode() == 0x0004 && vm.bus.vicon.r2_display() == 0x003F {
            let tv_non_zero = vm
                .bus
                .tvram()
                .map_or(0, |tv| tv.iter().filter(|&&b| b != 0).count());
            if tv_non_zero > 140_000 {
                break;
            }
        }
    }

    // Verify CRTC resolution remained 1024x768 throughout boot
    assert_eq!(vm.bus.crtc.active_resolution(), (1024, 768));

    // Verify FSX.X structures in RAM
    let iocs_mouse_vec = vm.bus.read_u32(0x0000_0938, true).unwrap();
    let text_base = iocs_mouse_vec - 0x04F82A;
    assert_eq!(vm.bus.read_u16(text_base + 0x55278 + 6, true).unwrap(), 1024);
    assert_eq!(vm.bus.read_u16(text_base + 0x55278 + 8, true).unwrap(), 768);

    let dt_win = vm.bus.read_u32(text_base + 0x54384, true).unwrap_or(0);
    assert!(dt_win > 0);
    let inside_h = vm.bus.read_u32(dt_win + 0x4C, true).unwrap_or(0);
    assert!(inside_h > 0);
    let inside_ptr = vm.bus.read_u32(inside_h, true).unwrap_or(0);
    assert!(inside_ptr > 0);
    assert_eq!(vm.bus.read_u16(inside_ptr + 8, true).unwrap(), 1024);
    assert_eq!(vm.bus.read_u16(inside_ptr + 10, true).unwrap(), 768);

    let dt_gp = vm.bus.read_u32(text_base + 0x5437A, true).unwrap_or(0);
    assert!(dt_gp > 0);
    let vis_h = vm.bus.read_u32(dt_gp + 0x10, true).unwrap_or(0);
    assert!(vis_h > 0);
    let vis_ptr = vm.bus.read_u32(vis_h, true).unwrap_or(0);
    assert!(vis_ptr > 0);
    assert_eq!(vm.bus.read_u16(vis_ptr + 8, true).unwrap(), 1024);
    assert_eq!(vm.bus.read_u16(vis_ptr + 10, true).unwrap(), 768);

    // Verify TVRAM contains pixels in extended areas (>768 and >512)
    let tv = vm.bus.tvram().expect("TVRAM must be allocated");
    let ext_x_byte = 50 * 128 + 52 * 2; // y=50, x=832 (>768)
    let ext_x_word = u16::from_be_bytes([tv[ext_x_byte], tv[ext_x_byte + 1]]);
    eprintln!("  ext_x_word: 0x{:04X}", ext_x_word);
    assert_ne!(ext_x_word, 0, "Boot-time 1024x768 desktop must draw into extended horizontal area (x > 768)");
    let ext_y_byte = 600 * 128 + 20 * 2; // y=600 (>512)
    let ext_y_word = u16::from_be_bytes([tv[ext_y_byte], tv[ext_y_byte + 1]]);
    assert_ne!(ext_y_word, 0, "Boot-time 1024x768 desktop must draw into extended vertical area (y > 512)");

    // Verify menuGrafPort was correctly initialized to 1024x768 from mode preset table
    let menu_gp_ptr = vm.bus.read_u32(text_base + 0x54AE0, true).unwrap();
    assert!(menu_gp_ptr > 0, "menuGrafPort must be allocated by MNInit");
    let menu_gp_bm = vm.bus.read_u32(menu_gp_ptr, true).unwrap();
    assert!(menu_gp_bm > 0);
    assert_eq!(vm.bus.read_u16(menu_gp_bm + 6, true).unwrap(), 1024, "menuGrafPort bmap width must be 1024");
    assert_eq!(vm.bus.read_u16(menu_gp_bm + 8, true).unwrap(), 768, "menuGrafPort bmap height must be 768");

    // Verify popup menu engine remains unpatched (original MOVEA.L 24(A6), A5)
    let pop_patch = (text_base + 0x049FE6) as usize;
    assert_eq!(
        &vm.bus.main_ram()[pop_patch..pop_patch + 10],
        &[0x2A, 0x6E, 0x00, 0x18, 0x2C, 0x15, 0x2E, 0x2D, 0x00, 0x04]
    );

    // Verify TextEdit TMSetWrap dynamic line wrapping remains unpatched
    assert_eq!(vm.bus.main_ram()[(text_base + 0x00D840) as usize], 0x67);
    assert_eq!(
        &vm.bus.main_ram()[(text_base + 0x00D8B4) as usize..(text_base + 0x00D8B6) as usize],
        &[0x66, 0x10]
    );

    // Verify all 7 preset mode table entries are synchronized
    for i in 0..7 {
        let preset = (text_base + 0x552F6 + i * 22) as usize;
        let w = u16::from_be_bytes([vm.bus.main_ram()[preset + 6], vm.bus.main_ram()[preset + 7]]);
        let h = u16::from_be_bytes([vm.bus.main_ram()[preset + 8], vm.bus.main_ram()[preset + 9]]);
        assert_eq!((w, h), (1024, 768), "Preset {i} mode table must be synchronized to 1024x768");
    }

    // Verify soft reset restores vectors and resets extended resolution
    vm.bus.soft_reset();
    assert_eq!(vm.bus.crtc.active_resolution(), (768, 512));
    assert!(!vm.bus.is_ext_tvram_active());
    assert_eq!(vm.bus.max_desktop_size(), (0, 0));
    assert!(vm.bus.target_resolution.is_none());
    assert_eq!(&vm.bus.main_ram()[0..8], &ipl_data[ipl_data.len() - 64 * 1024..][..8]);
}

#[test]
fn test_sxwin31_scsi_boot_with_1920x1080_initial_resolution() {
    let workspace_root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let ipl_path = workspace_root.join("roms/IPLROM.DAT");
    let cgrom_path = workspace_root.join("roms/CGROM.DAT");
    let human_path = workspace_root.join("disks/HUMAN302.XDF");

    if !ipl_path.exists() || !cgrom_path.exists() || !human_path.exists() {
        eprintln!("[SKIP] Required ROMs/disks not found for scsi_boot test");
        return;
    }

    let ipl_data = std::fs::read(&ipl_path).expect("Read IPLROM");
    let cgrom_data = std::fs::read(&cgrom_path).expect("Read CGROM");
    let human_data = std::fs::read(&human_path).expect("Read HUMAN302.XDF");

    let sx_disks = [
        ("SXWIN311", workspace_root.join("disks/SXWIN311.XDF")),
        ("SXWIN312", workspace_root.join("disks/SXWIN312.XDF")),
        ("SXWIN313", workspace_root.join("disks/SXWIN313.XDF")),
        ("SXWIN314", workspace_root.join("disks/SXWIN314.XDF")),
        ("SXWIN315", workspace_root.join("disks/SXWIN315.XDF")),
        ("SXWIN316", workspace_root.join("disks/SXWIN316.XDF")),
    ];
    let mut sx_loaded: Vec<(&str, Vec<u8>)> = Vec::new();
    for (name, path) in &sx_disks {
        if let Ok(bytes) = std::fs::read(path) {
            sx_loaded.push((*name, bytes));
        }
    }
    let sx_refs: Vec<(&str, &[u8])> = sx_loaded.iter().map(|(n, b)| (*n, b.as_slice())).collect();
    let hdd_image = build_bootable_sxwin_scsi_hdd(&human_data, &sx_refs, 32);

    let mut vm = X68kVm::new(
        VmConfig::master_sx()
            .with_speed_mode(CpuSpeedMode::Burst)
            .with_ram_mb(4),
    );

    vm.bus.load_cgrom(&cgrom_data);
    vm.bus.load_ipl_rom(&ipl_data);

    assert!(vm.bus.insert_scsi_hdd(0, hdd_image));
    vm.bus.set_boot_from_scsi(true);

    vm.reset().unwrap();

    // Pre-configure initial resolution to 1920x1080 (as if passed via CLI --resolution 1920x1080 or MSX68K.CFG)
    vm.bus.apply_screen_resolution(1920, 1080);

    // Boot until SX-Window reaches desktop
    for _chunk in 1..=90 {
        vm.run_steps(250_000);
        if vm.bus.vicon.r0_mode() == 0x0004 && vm.bus.vicon.r2_display() == 0x003F {
            if let Some(tv) = vm.bus.active_tvram() {
                let tv_non_zero = tv.iter().filter(|&&b| b != 0).count();
                if tv_non_zero > 140_000 {
                    break;
                }
            }
        }
    }

    // Verify CRTC resolution remained 1920x1080 throughout boot
    assert_eq!(vm.bus.crtc.active_resolution(), (1920, 1080));
    assert!(vm.bus.is_ext_tvram_active());
    assert_eq!(vm.bus.active_tvram_row_bytes(), 240);

    // Verify FSX.X structures in RAM
    let iocs_mouse_vec = vm.bus.read_u32(0x0000_0938, true).unwrap();
    let text_base = iocs_mouse_vec - 0x04F82A;
    assert_eq!(vm.bus.read_u16(text_base + 0x55278 + 6, true).unwrap(), 1920);
    assert_eq!(vm.bus.read_u16(text_base + 0x55278 + 8, true).unwrap(), 1080);

    // Verify menuGrafPort was initialized to 1920x1080 with EXT_TVRAM_BASE directly from preset
    let menu_gp_ptr = vm.bus.read_u32(text_base + 0x54AE0, true).unwrap();
    assert!(menu_gp_ptr > 0, "menuGrafPort must be allocated by MNInit");
    let menu_gp_bm = vm.bus.read_u32(menu_gp_ptr, true).unwrap();
    assert!(menu_gp_bm > 0);
    assert_eq!(vm.bus.read_u16(menu_gp_bm + 6, true).unwrap(), 1920, "menuGrafPort bmap width must be 1920");
    assert_eq!(vm.bus.read_u16(menu_gp_bm + 8, true).unwrap(), 1080, "menuGrafPort bmap height must be 1080");
    assert_eq!(vm.bus.read_u32(menu_gp_bm + 10, true).unwrap(), msx68k_bus::EXT_TVRAM_BASE, "menuGrafPort baseAddr must be EXT_TVRAM_BASE");
    assert_eq!(vm.bus.read_u16(menu_gp_bm + 14, true).unwrap(), 240, "menuGrafPort rowBytes must be 240");
}
