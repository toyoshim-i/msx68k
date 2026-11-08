//! X68000 24-bit system bus, region-aware memory wait-state model, and VM runner (`msx68k-bus`).

pub mod bus;
pub mod crtc;
pub mod dmac;
pub mod fdc;
pub mod guest_build;
pub mod mfp;
pub mod peripherals;
pub mod scsi;
pub mod timing;
pub mod vicon;
pub mod vm;

pub use msx68k_cpu::{Bus, BusFault, NoopTrapHandler, TrapHandler};
pub use bus::{
    HardwareAccessFlags, X68kBus, GVRAM_BYTES, IO_WINDOW_BYTES, MAX_MAIN_RAM_BYTES,
    ROM_WINDOW_BYTES, SPRITE_WINDOW_BYTES, TVRAM_BYTES,
};
pub use crtc::{Crtc, CrtcSyncEvents, RASTER_BLOCK_BYTES, TVRAM_PLANE_BYTES};
pub use dmac::{DmaChannel, Dmac};
pub use fdc::{Fdc, FloppyDrive, FloppyGeometry, IoInterruptController};
pub use mfp::Mfp;
pub use peripherals::{Opm, Ppi, Rtc, Scc, SystemPort};
pub use scsi::{
    build_bootable_sxwin_scsi_hdd, extract_files_from_xdf, parse_scsi_partitions,
    synthesize_partition_boot_sector, synthesize_scsi_ipl_sector, ExtractedFile, HypercallId,
    ScsiController, ScsiPartition,
};
pub use timing::{
    BusTimingConfig, CpuSpeedMode, MemoryRegion, RegionTimingSpec, VmRole, WaitStateTable,
    PAGE_COUNT_4K, PAGE_SHIFT_4K,
};
pub use vicon::{grbi_to_rgba8, Vicon};
pub use vm::{BusTrapHandler, VmConfig, X68kVm};

#[cfg(test)]
mod tests {
    use super::*;
    use msx68k_cpu::Bus;

    #[test]
    fn test_vm_role_defaults_and_app_speed_selection() {
        // Master VM defaults to Burst (0 delay)
        let master = X68kVm::new_master_sx();
        assert_eq!(master.role(), VmRole::MasterSx);
        assert_eq!(master.speed_mode(), CpuSpeedMode::Burst);
        assert!(master.is_running_burst());
        assert_eq!(master.timing_config().cpu_mhz, 0);
        assert_eq!(master.timing_config().system_bus_mhz, 0);
        assert!(!master.timing_config().wait_enabled);

        // App VM defaults to 10MHz (CPU 10MHz, System Bus 10MHz)
        let app_default = X68kVm::new_app_vm();
        assert_eq!(app_default.role(), VmRole::AppVm);
        assert_eq!(app_default.speed_mode(), CpuSpeedMode::Mhz10);
        assert!(!app_default.is_running_burst());
        assert_eq!(app_default.timing_config().cpu_mhz, 10);
        assert_eq!(app_default.timing_config().system_bus_mhz, 10);

        // App VM configured for 16MHz (XVI: CPU 16MHz, System Bus 16MHz)
        let app_16 = X68kVm::new_app_vm_with_speed(CpuSpeedMode::Mhz16);
        assert_eq!(app_16.timing_config().cpu_mhz, 16);
        assert_eq!(app_16.timing_config().system_bus_mhz, 16);

        // App VM configured for 24MHz (RedZone: CPU 24MHz, System Bus 16MHz)
        let app_24 = X68kVm::new_app_vm_with_speed(CpuSpeedMode::Mhz24);
        assert_eq!(app_24.timing_config().cpu_mhz, 24);
        assert_eq!(app_24.timing_config().system_bus_mhz, 16);
    }

    #[test]
    fn test_region_wait_states_across_10_16_24_and_burst() {
        let t10 = CpuSpeedMode::Mhz10.timing_config();
        let t16 = CpuSpeedMode::Mhz16.timing_config();
        let t24 = CpuSpeedMode::Mhz24.timing_config();
        let t_burst = CpuSpeedMode::Burst.timing_config();

        // 1. Main RAM ($000000):
        //    - 10MHz (Bus 10): 0 waits
        //    - 16MHz (Bus 16): 0 waits
        //    - 24MHz (Bus 16): 2 waits (4 cycles @ 16MHz bus = 6 cycles @ 24MHz CPU -> +2 waits)
        //    - Burst: 0 waits
        assert_eq!(t10.compute_region_wait(MemoryRegion::MainRam), 0);
        assert_eq!(t16.compute_region_wait(MemoryRegion::MainRam), 0);
        assert_eq!(t24.compute_region_wait(MemoryRegion::MainRam), 2);
        assert_eq!(t_burst.compute_region_wait(MemoryRegion::MainRam), 0);

        // 2. GVRAM ($C00000) & TVRAM ($E00000):
        //    TVRAM is always slower than GVRAM, and waits scale up with CPU/Bus speed.
        assert_eq!(t10.compute_region_wait(MemoryRegion::Gvram), 1);
        assert_eq!(t10.compute_region_wait(MemoryRegion::Tvram), 2);

        assert_eq!(t16.compute_region_wait(MemoryRegion::Gvram), 2);
        assert_eq!(t16.compute_region_wait(MemoryRegion::Tvram), 4);

        assert_eq!(t24.compute_region_wait(MemoryRegion::Gvram), 5);
        assert_eq!(t24.compute_region_wait(MemoryRegion::Tvram), 7);

        // 3. Peripheral I/O & ROM hierarchy:
        //    MainRam < ROM/SRAM < OPM/FDC < Palette < MFP < SCC < DMAC
        for cfg in [t10, t16, t24] {
            let w_ram = cfg.compute_region_wait(MemoryRegion::MainRam);
            let w_rom = cfg.compute_region_wait(MemoryRegion::Rom);
            let w_opm = cfg.compute_region_wait(MemoryRegion::Opm);
            let w_pal = cfg.compute_region_wait(MemoryRegion::PaletteVicon);
            let w_mfp = cfg.compute_region_wait(MemoryRegion::Mfp);
            let w_scc = cfg.compute_region_wait(MemoryRegion::Scc);
            let w_dmac = cfg.compute_region_wait(MemoryRegion::Dmac);

            assert!(w_ram < w_rom);
            assert!(w_rom <= w_opm);
            assert!(w_opm < w_pal);
            assert!(w_pal < w_mfp);
            assert!(w_mfp < w_scc);
            assert!(w_scc < w_dmac);
        }
    }

    #[test]
    fn test_execution_cycles_reflect_memory_region_and_bus_speed() {
        // Program at $000400 in Main RAM:
        //   MOVE.W #$1234, $001000.L   (write to Main RAM)
        //   MOVE.W #$1234, $C00000.L   (write to GVRAM)
        //   MOVE.W #$1234, $E00000.L   (write to TVRAM)
        let prog: [u16; 12] = [
            0x33FC, 0x1234, 0x0000, 0x1000, // MOVE.W #$1234, $00001000.L
            0x33FC, 0x1234, 0x00C0, 0x0000, // MOVE.W #$1234, $00C00000.L
            0x33FC, 0x1234, 0x00E0, 0x0000, // MOVE.W #$1234, $00E00000.L
        ];

        let run_in_mode = |mode: CpuSpeedMode| -> (u32, u32, u32) {
            let mut vm = X68kVm::new_app_vm_with_speed(mode);
            // Reset vectors: SSP = $0001_0000, PC = $0000_0400
            vm.bus.write_u32(0x0000, 0x0001_0000, true).unwrap();
            vm.bus.write_u32(0x0004, 0x0000_0400, true).unwrap();
            for (i, &w) in prog.iter().enumerate() {
                vm.bus
                    .write_u16(0x0400 + (i as u32) * 2, w, true)
                    .unwrap();
            }
            vm.reset().unwrap();
            let c_ram = vm.step().cycles;
            let c_gvram = vm.step().cycles;
            let c_tvram = vm.step().cycles;
            (c_ram, c_gvram, c_tvram)
        };

        // Base Motorola cycles for `MOVE.W #<imm>, (xxx).L` = 20 cycles (4 fetches + 1 write).
        // 10MHz (CPU 10 / Bus 10):
        //   RAM write:   20 + 0 = 20
        //   GVRAM write: 20 + 1 = 21
        //   TVRAM write: 20 + 2 = 22
        assert_eq!(run_in_mode(CpuSpeedMode::Mhz10), (20, 21, 22));

        // 16MHz XVI (CPU 16 / Bus 16):
        //   RAM write:   20 + 0 = 20
        //   GVRAM write: 20 + 2 = 22
        //   TVRAM write: 20 + 4 = 24
        assert_eq!(run_in_mode(CpuSpeedMode::Mhz16), (20, 22, 24));

        // 24MHz RedZone (CPU 24 / Bus 16):
        //   4 instruction fetches from Main RAM incur 4 * 2 = 8 wait cycles!
        //   Plus destination write wait: RAM (+2), GVRAM (+5), TVRAM (+7)
        assert_eq!(run_in_mode(CpuSpeedMode::Mhz24), (20 + 8 + 2, 20 + 8 + 5, 20 + 8 + 7));

        // Burst: 0 cycles
        assert_eq!(run_in_mode(CpuSpeedMode::Burst), (0, 0, 0));
    }

    #[test]
    fn test_lazy_allocation_and_vm_isolation_detection_flags() {
        let mut vm = X68kVm::new_app_vm();
        assert!(!vm.bus.is_gvram_allocated());
        assert!(!vm.bus.is_tvram_allocated());
        assert!(!vm.bus.access_flags().requires_isolated_display_vm());

        // Touching OPM ($E90003) records sound access but must NOT trigger display VM isolation
        vm.bus.write_u8(0x00E9_0003, 0x00, true).unwrap();
        assert!(vm.bus.access_flags().sound_io_accessed);
        assert!(!vm.bus.access_flags().requires_isolated_display_vm());

        // Writing to GVRAM ($C00000) lazily allocates GVRAM and flags isolated display VM migration
        vm.bus.write_u16(0x00C0_0000, 0xFFFF, true).unwrap();
        assert!(vm.bus.is_gvram_allocated());
        assert!(vm.bus.access_flags().gvram_accessed);
        assert!(vm.bus.access_flags().requires_isolated_display_vm());
    }

    #[test]
    fn test_unmapped_ram_bus_error_and_disk_io_burst_toggle() {
        // 2MB App VM: accessing $200000 (above 2MB) must raise a Bus Error (Vector 2 at $0008)
        let mut vm = X68kVm::new(
            VmConfig::app_vm()
                .with_ram_mb(2)
                .with_burst_on_disk_io(true),
        );
        // Set reset SSP = $0001_0000, PC = $0000_0400, Bus Error handler at Vector 2 ($0008) = $0000_0800
        vm.bus.write_u32(0x0000, 0x0001_0000, true).unwrap();
        vm.bus.write_u32(0x0004, 0x0000_0400, true).unwrap();
        vm.bus.write_u32(0x0008, 0x0000_0800, true).unwrap();

        // At $0400: TST.W $00200000.L (0x4A79, 0x0020, 0x0000) -> triggers Bus Error to $0800
        // At $0800: MOVEQ #42, D0 (0x702A)
        vm.bus.write_u16(0x0400, 0x4A79, true).unwrap();
        vm.bus.write_u16(0x0402, 0x0020, true).unwrap();
        vm.bus.write_u16(0x0404, 0x0000, true).unwrap();
        vm.bus.write_u16(0x0800, 0x702A, true).unwrap();

        vm.reset().unwrap();
        let fault_step = vm.step();
        assert!(fault_step.cycles >= 50);
        assert_eq!(vm.cpu_state.pc, 0x0800);

        // Temporarily activate disk I/O burst: next step executes in BurstPolicy (0 cycles)
        vm.set_disk_io_active(true);
        assert!(vm.is_running_burst());
        let burst_step = vm.step();
        assert_eq!(burst_step.cycles, 0);
        assert_eq!(vm.cpu_state.d[0], 42);

        // Leaving disk I/O restores 10MHz cycle-accurate tracking
        vm.set_disk_io_active(false);
        assert!(!vm.is_running_burst());
    }
}
