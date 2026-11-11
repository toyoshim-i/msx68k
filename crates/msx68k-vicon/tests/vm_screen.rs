//! A VM on a thread of its own composites its screen into its own texture at each vertical
//! blank, uploading only the VRAM pages it wrote (`docs/VMX68K.md` Section 3.3).

use std::path::Path;
use std::sync::mpsc;
use std::time::{Duration, Instant};

use msx68k_bus::vm_thread::{Pace, VmThread};
use msx68k_bus::{CpuSpeedMode, VmConfig, X68kVm};
use msx68k_vicon::{decode_tvram_ank_lines, render_bus_frame_cpu, RenderedFrame, ViconGpuCompositor, VmScreen};

#[test]
fn a_vm_thread_composites_its_screen_into_a_texture() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let read = |name: &str| std::fs::read(root.join(name)).ok();
    let (Some(ipl), Some(cgrom), Some(human)) = (
        read("roms/IPLROM.DAT"),
        read("roms/CGROM.DAT"),
        read("disks/HUMAN302.XDF"),
    ) else {
        eprintln!("ROMs or HUMAN302.XDF not found, skipping");
        return;
    };
    let Some(gpu) = ViconGpuCompositor::new() else {
        eprintln!("No wgpu adapter, skipping");
        return;
    };

    let mut vm = X68kVm::new(
        VmConfig::app_vm()
            .with_speed_mode(CpuSpeedMode::Mhz10)
            .with_ram_mb(2),
    );
    vm.bus.load_cgrom(&cgrom);
    vm.bus.load_ipl_rom(&ipl);
    assert!(vm.bus.insert_floppy(0, human, true));
    vm.reset().unwrap();

    let mut screen = VmScreen::new(gpu.device(), gpu.queue());
    let output = screen.output();
    let (uploads_tx, uploads) = mpsc::channel();
    let (capture_tx, capture) = mpsc::channel::<()>();
    let (captured_tx, captured) = mpsc::channel();
    let (resume_tx, resume) = mpsc::channel::<()>();
    let thread = VmThread::spawn_with_vblank(vm, Pace::Unthrottled, move |vm| {
        let _ = uploads_tx.send(screen.present(&mut vm.bus));
        if capture.try_recv().is_ok() {
            // Hold the VM still while the test reads the frame back.
            let _ = captured_tx.send(render_bus_frame_cpu(&vm.bus));
            let _ = resume.recv();
        }
    });

    let deadline = Instant::now() + Duration::from_secs(120);
    while !thread
        .with_vm(|vm| decode_tvram_ank_lines(&vm.bus, 96, 32))
        .iter()
        .any(|line| line.starts_with("A>"))
    {
        assert!(Instant::now() < deadline, "COMMAND.X must show its prompt");
        std::thread::sleep(Duration::from_millis(50));
    }

    // The first frame uploads every VRAM page the guest has allocated.
    let first = uploads.recv().unwrap();
    assert!(first >= 512 * 1024, "first frame uploaded {first} bytes");

    // At the prompt only the blinking cursor changes: a few pages, plus palette and registers.
    let _ = uploads.try_iter().count();
    let still: Vec<usize> = uploads.iter().take(120).collect();
    let most = *still.iter().max().unwrap();
    assert!(most <= 20 * 1024, "a still frame uploaded {most} bytes: {still:?}");

    capture_tx.send(()).unwrap();
    let cpu: RenderedFrame = captured.recv().unwrap();
    let gpu_frame = output.read_back(gpu.device(), gpu.queue()).unwrap();
    resume_tx.send(()).unwrap();
    drop(thread);

    assert_eq!((gpu_frame.width, gpu_frame.height), (cpu.width, cpu.height));
    assert!(cpu.count_lit_pixels() > 0);
    assert!(gpu_frame.rgba8 == cpu.rgba8, "the texture must match the CPU compositor");
}
