//! Integration tests for the Virtual SCSI Subsystem ($FC0000 ROM, IOCS $F5 _SCSIDRV,
//! and romdb block driver) and the Mixed Mode Hypercall & FSX.X 1:1 Mouse Hook.

use std::path::Path;

use msx68k_bus::scsi::{build_bootable_sxwin_scsi_hdd, parse_scsi_partitions};
use msx68k_bus::{CpuSpeedMode, VmConfig, X68kVm};
use msx68k_cpu::Bus;

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

        // Scan SHELL directory
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

    let a4_ptr = vm.bus.read_u32(text_base + 0x633CA, true).unwrap();
    let a4_x = vm.bus.read_u16(a4_ptr + 0x0A, true).unwrap() as i16;
    let a4_y = vm.bus.read_u16(a4_ptr + 0x0C, true).unwrap() as i16;
    assert_eq!(
        (a4_x, a4_y),
        (250, 180),
        "FSX.X MouseWorkArea coordinates must match host mouse position"
    );
}
