//! Interactive `winit` + `wgpu` desktop window runner for **msx68k**.
//!
//! Usage:
//! ```bash
//! cargo run --release
//! # Or with explicit options:
//! cargo run --release -- --fd0 /path/to/HUMAN302.XDF --speed 10 --ram-mb 2
//! ```
//!
//! Hotkeys in window:
//! - `F1`: Switch to `10MHz` mode (CPU 10MHz / Bus 10MHz)
//! - `F2`: Switch to `16MHz` mode (XVI: CPU 16MHz / Bus 16MHz)
//! - `F3`: Switch to `24MHz` mode (RedZone: CPU 24MHz / Bus 16MHz)
//! - `F4`: Switch to `Burst` mode (0 wait, high-speed execution)
//! - `F12`: Soft Reset X68000 VM

#[allow(dead_code)]
mod audio;

use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use msx68k_bus::vm_thread::{Pace, VmThread};
use msx68k_bus::{CpuModel, CpuSpeedMode, FrameBudget, GdbServer, VmConfig, VmEvent, X68kVm};
use msx68k_vicon::{
    ScreenRect, ViconGpuCompositor, ViconSurfacePresenter, VmLayer, VmScreen, VmScreenOutput,
};
use winit::application::ApplicationHandler;
use winit::dpi::PhysicalSize;
use winit::event::{ElementState, MouseButton, MouseScrollDelta, WindowEvent};
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop};
use winit::keyboard::{KeyCode, PhysicalKey};
use winit::window::{Window, WindowId};

const DEFAULT_IPL_PATH: &str = "roms/IPLROM.DAT";
/// IPL ROM 1.3 of the X68030, the default for `--cpu 68030`.
const DEFAULT_IPL30_PATH: &str = "roms/IPLROM30.DAT";
const DEFAULT_CGROM_PATH: &str = "roms/CGROM.DAT";

/// Maps a host `winit::keyboard::KeyCode` to a 7-bit X68000 hardware scan code (`0x01`..=`0x7F`).
fn map_keycode_to_x68k_scancode(code: KeyCode) -> Option<u8> {
    let sc = match code {
        KeyCode::Escape => 0x01,
        KeyCode::Digit1 => 0x02,
        KeyCode::Digit2 => 0x03,
        KeyCode::Digit3 => 0x04,
        KeyCode::Digit4 => 0x05,
        KeyCode::Digit5 => 0x06,
        KeyCode::Digit6 => 0x07,
        KeyCode::Digit7 => 0x08,
        KeyCode::Digit8 => 0x09,
        KeyCode::Digit9 => 0x0A,
        KeyCode::Digit0 => 0x0B,
        KeyCode::Minus => 0x0C,
        KeyCode::Equal => 0x0D,
        KeyCode::IntlYen => 0x0E,
        KeyCode::Backspace => 0x0F,
        KeyCode::Tab => 0x10,
        KeyCode::KeyQ => 0x11,
        KeyCode::KeyW => 0x12,
        KeyCode::KeyE => 0x13,
        KeyCode::KeyR => 0x14,
        KeyCode::KeyT => 0x15,
        KeyCode::KeyY => 0x16,
        KeyCode::KeyU => 0x17,
        KeyCode::KeyI => 0x18,
        KeyCode::KeyO => 0x19,
        KeyCode::KeyP => 0x1A,
        KeyCode::BracketLeft => 0x1B,
        KeyCode::BracketRight => 0x1C,
        KeyCode::Enter | KeyCode::NumpadEnter => 0x1D,
        KeyCode::KeyA => 0x1E,
        KeyCode::KeyS => 0x1F,
        KeyCode::KeyD => 0x20,
        KeyCode::KeyF => 0x21,
        KeyCode::KeyG => 0x22,
        KeyCode::KeyH => 0x23,
        KeyCode::KeyJ => 0x24,
        KeyCode::KeyK => 0x25,
        KeyCode::KeyL => 0x26,
        KeyCode::Semicolon => 0x27,
        KeyCode::Quote => 0x28,
        KeyCode::Backslash => 0x29,
        KeyCode::KeyZ => 0x2A,
        KeyCode::KeyX => 0x2B,
        KeyCode::KeyC => 0x2C,
        KeyCode::KeyV => 0x2D,
        KeyCode::KeyB => 0x2E,
        KeyCode::KeyN => 0x2F,
        KeyCode::KeyM => 0x30,
        KeyCode::Comma => 0x31,
        KeyCode::Period => 0x32,
        KeyCode::Slash => 0x33,
        KeyCode::IntlRo => 0x34,
        KeyCode::Space => 0x35,
        KeyCode::Home => 0x36,
        KeyCode::Delete => 0x37,
        KeyCode::PageUp => 0x38,
        KeyCode::PageDown => 0x39,
        KeyCode::End => 0x3A,
        KeyCode::ArrowLeft => 0x3B,
        KeyCode::ArrowUp => 0x3C,
        KeyCode::ArrowRight => 0x3D,
        KeyCode::ArrowDown => 0x3E,
        KeyCode::ShiftLeft | KeyCode::ShiftRight => 0x70,
        KeyCode::ControlLeft | KeyCode::ControlRight => 0x71,
        KeyCode::AltLeft => 0x55,
        KeyCode::AltRight => 0x56,
        _ => return None,
    };
    Some(sc)
}

const JOYPAD_BITS: [u8; 6] = [0x01, 0x02, 0x04, 0x08, 0x20, 0x40];

fn check_and_repair_cli_floppy(drive_name: &str, path: &Path, data: &mut Vec<u8>) {
    let Some(virus) = msx68k_bus::virus::scan_floppy_image(data) else {
        return;
    };

    println!(
        "\n[WARNING] Virus detected in {drive_name} ('{}'): {}",
        path.display(),
        virus.name_en()
    );

    if virus.can_self_repair_from_sector4() {
        println!("Original boot loader backup found in Sector 4.");
        print!("Would you like to repair this disk image? [Y/n]: ");
        let _ = std::io::stdout().flush();
        let mut line = String::new();
        if std::io::stdin().read_line(&mut line).is_err()
            || line.trim().eq_ignore_ascii_case("n")
        {
            eprintln!("Disk repair aborted. Exiting.");
            std::process::exit(1);
        }

        let backup_path = path.with_extension("org");
        if let Err(e) = std::fs::write(&backup_path, &data) {
            eprintln!(
                "Warning: Failed to create backup at '{}': {e}",
                backup_path.display()
            );
        } else {
            println!("Backed up original image to '{}'", backup_path.display());
        }

        if !msx68k_bus::virus::repair_floppy_forceboot_from_sector4(data) {
            eprintln!("Error: Failed to restore IPL from Sector 4. Exiting.");
            std::process::exit(1);
        }

        if let Err(e) = std::fs::write(path, &data) {
            eprintln!(
                "Error: Failed to write repaired image to '{}': {e}",
                path.display()
            );
            std::process::exit(1);
        }
        println!(
            "Successfully repaired {drive_name} ('{}').\n",
            path.display()
        );
        return;
    }

    print!("Would you like to repair this disk image? [Y/n]: ");
    let _ = std::io::stdout().flush();
    let mut line = String::new();
    if std::io::stdin().read_line(&mut line).is_err()
        || line.trim().eq_ignore_ascii_case("n")
    {
        eprintln!("Disk repair aborted. Exiting.");
        std::process::exit(1);
    }

    let default_clean = "disks/HUMAN302.XDF";
    print!("Enter path to clean disk image with original IPL [default: {default_clean}]: ");
    let _ = std::io::stdout().flush();
    let mut clean_input = String::new();
    let clean_path = if std::io::stdin().read_line(&mut clean_input).is_ok()
        && !clean_input.trim().is_empty()
    {
        clean_input.trim().to_string()
    } else {
        default_clean.to_string()
    };

    let clean_bytes = match std::fs::read(&clean_path) {
        Ok(b) => b,
        Err(e) => {
            eprintln!(
                "Error: Failed to read clean disk image from '{clean_path}': {e}. Exiting."
            );
            std::process::exit(1);
        }
    };

    let Some(clean_ipl) = msx68k_bus::virus::read_floppy_ipl(&clean_bytes) else {
        eprintln!(
            "Error: Failed to extract IPL from clean disk image '{clean_path}'. Exiting."
        );
        std::process::exit(1);
    };

    let backup_path = path.with_extension("org");
    if let Err(e) = std::fs::write(&backup_path, &data) {
        eprintln!(
            "Warning: Failed to create backup at '{}': {e}",
            backup_path.display()
        );
    } else {
        println!("Backed up original image to '{}'", backup_path.display());
    }

    if !msx68k_bus::virus::repair_floppy_from_clean_ipl(data, &clean_ipl) {
        eprintln!("Error: Failed to repair IPL. Exiting.");
        std::process::exit(1);
    }

    if let Err(e) = std::fs::write(path, &data) {
        eprintln!(
            "Error: Failed to write repaired image to '{}': {e}",
            path.display()
        );
        std::process::exit(1);
    }
    println!(
        "Successfully repaired {drive_name} ('{}').\n",
        path.display()
    );
}

/// Host time left free each frame for rendering while Burst runs flat out.
const BURST_RENDER_RESERVE: Duration = Duration::from_millis(3);

#[derive(Debug, Clone)]
struct AppArgs {
    /// IPL ROM given by `--ipl` or `MSX68K_IPL`; otherwise the one for the CPU is used.
    ipl_path: Option<PathBuf>,
    cgrom_path: PathBuf,
    fd0_path: Option<PathBuf>,
    fd1_path: Option<PathBuf>,
    /// Floppy image VMs opened by VMX68K.X boot from (`docs/VMX68K.md` Section 3.5).
    vm_fd0_path: Option<PathBuf>,
    hdd0_path: Option<PathBuf>,
    sram_path: PathBuf,
    /// Speed given by `--speed`; otherwise 10 MHz for the MC68000 and 25 MHz for the MC68030.
    speed_mode: Option<CpuSpeedMode>,
    cpu_model: CpuModel,
    /// RAM size given by `--ram-mb`; otherwise the size saved in SRAM, or 4 MB without one.
    ram_mb: Option<usize>,
    scale: u32,
    resolution: Option<(u32, u32)>,
    fullscreen: bool,
    vsync: bool,
    gdb_port: Option<u16>,
    /// Host directories mounted as file-level remote drives via `--remote-drive`.
    remote_drives: Vec<PathBuf>,
}

impl Default for AppArgs {
    fn default() -> Self {
        let ipl_path = std::env::var_os("MSX68K_IPL").map(PathBuf::from);
        let cgrom_path = std::env::var_os("MSX68K_CGROM")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from(DEFAULT_CGROM_PATH));
        let fd0_path = std::env::var_os("MSX68K_FD0").map(PathBuf::from);
        let fd1_path = std::env::var_os("MSX68K_FD1").map(PathBuf::from);
        let vm_fd0_path = std::env::var_os("MSX68K_VM_FD0").map(PathBuf::from);
        let hdd0_path = std::env::var_os("MSX68K_HDD0").map(PathBuf::from);
        let sram_path = std::env::var_os("MSX68K_SRAM")
            .map(PathBuf::from)
            .unwrap_or_else(|| {
                if Path::new("disks").is_dir() {
                    PathBuf::from("disks/SRAM.DAT")
                } else {
                    PathBuf::from("SRAM.DAT")
                }
            });
        Self {
            ipl_path,
            cgrom_path,
            fd0_path,
            fd1_path,
            vm_fd0_path,
            hdd0_path,
            sram_path,
            speed_mode: None,
            cpu_model: CpuModel::Mc68000,
            ram_mb: None,
            scale: 1,
            resolution: None,
            fullscreen: false,
            vsync: false,
            gdb_port: None,
            remote_drives: Vec::new(),
        }
    }
}

impl AppArgs {
    fn parse() -> Self {
        let mut cfg = Self::default();
        let mut args = std::env::args().skip(1);
        while let Some(arg) = args.next() {
            match arg.as_str() {
                "--ipl" => {
                    if let Some(p) = args.next() {
                        cfg.ipl_path = Some(PathBuf::from(p));
                    }
                }
                "--cgrom" => {
                    if let Some(p) = args.next() {
                        cfg.cgrom_path = PathBuf::from(p);
                    }
                }
                "--fd0" => {
                    if let Some(p) = args.next() {
                        cfg.fd0_path = Some(PathBuf::from(p));
                    }
                }
                "--fd1" => {
                    if let Some(p) = args.next() {
                        cfg.fd1_path = Some(PathBuf::from(p));
                    }
                }
                "--vm-fd0" => {
                    if let Some(p) = args.next() {
                        cfg.vm_fd0_path = Some(PathBuf::from(p));
                    }
                }
                "--hdd0" => {
                    if let Some(p) = args.next() {
                        cfg.hdd0_path = Some(PathBuf::from(p));
                    }
                }
                "--sram" => {
                    if let Some(p) = args.next() {
                        cfg.sram_path = PathBuf::from(p);
                    }
                }
                "--speed" => {
                    if let Some(s) = args.next() {
                        cfg.speed_mode = match s.to_ascii_lowercase().as_str() {
                            "10" | "10mhz" => Some(CpuSpeedMode::Mhz10),
                            "16" | "16mhz" => Some(CpuSpeedMode::Mhz16),
                            "24" | "24mhz" => Some(CpuSpeedMode::Mhz24),
                            "25" | "25mhz" => Some(CpuSpeedMode::Mhz25),
                            "burst" | "0" => Some(CpuSpeedMode::Burst),
                            _ => {
                                eprintln!("Unknown --speed '{}', using the CPU's default", s);
                                None
                            }
                        };
                    }
                }
                "--cpu" => match args.next().as_deref() {
                    Some("68000") => cfg.cpu_model = CpuModel::Mc68000,
                    Some("68030") => cfg.cpu_model = CpuModel::Mc68030,
                    other => eprintln!("Unknown --cpu {other:?}, keeping the MC68000"),
                },
                "--ram-mb" => {
                    if let Some(v) = args.next().and_then(|s| s.parse::<usize>().ok()) {
                        cfg.ram_mb = Some(v.clamp(1, 12));
                    }
                }
                "--scale" => {
                    if let Some(v) = args.next().and_then(|s| s.parse::<u32>().ok()) {
                        cfg.scale = v.clamp(1, 4);
                    }
                }
                "--fullscreen" | "-f" => {
                    cfg.fullscreen = true;
                }
                "--vsync" => {
                    cfg.vsync = true;
                }
                "--gdb" => match args.next().and_then(|s| s.parse::<u16>().ok()) {
                    Some(port) => cfg.gdb_port = Some(port),
                    None => {
                        eprintln!("--gdb requires a TCP port number");
                        std::process::exit(1);
                    }
                },
                "--resolution" => {
                    if let Some(s) = args.next() {
                        if s.eq_ignore_ascii_case("native") || s.eq_ignore_ascii_case("fullscreen") {
                            cfg.fullscreen = true;
                        } else {
                            let parts: Vec<&str> = s.split(['x', 'X', '*']).collect();
                            if parts.len() == 2 {
                                if let (Ok(w), Ok(h)) =
                                    (parts[0].parse::<u32>(), parts[1].parse::<u32>())
                                {
                                    cfg.resolution = Some((w, h));
                                }
                            } else if s == "1024" {
                                cfg.resolution = Some((1024, 1024));
                            } else if s == "768" {
                                cfg.resolution = Some((768, 512));
                            }
                        }
                    }
                }
                "--remote-drive" => {
                    if let Some(p) = args.next() {
                        cfg.remote_drives.push(PathBuf::from(p));
                    }
                }
                "--help" | "-h" => {
                    println!(
                        "msx68k — Sharp X68000 / SX-Window Hybrid Execution Environment\n\n\
                         Options:\n  \
                           --ipl <path>         Path to the IPL ROM (default: {DEFAULT_IPL_PATH}, {DEFAULT_IPL30_PATH} for 68030)\n  \
                           --cgrom <path>       Path to CGROM.DAT  (default: {DEFAULT_CGROM_PATH})\n  \
                           --fd0 <path>         Floppy disk image for Drive 0 (.XDF/.2HD/.DIM)\n  \
                           --fd1 <path>         Floppy disk image for Drive 1 (.XDF/.2HD/.DIM)\n  \
                           --hdd0 <path>        SCSI HDD image for Target 0 (.HDS/.IMG)\n  \
                           --remote-drive <dir> Host directory to mount as remote drive (can be repeated)\n  \
                           --vm-fd0 <path>      Boot VMs of VMX68K.X from this floppy image, not the primary's A:\n  \
                           --sram <path>        Path to battery-backed SRAM file (default: disks/SRAM.DAT)\n  \
                           --speed <mode>       CPU speed: 10 | 16 | 24 | 25 | burst (default: 10, 25 for 68030)\n  \
                           --cpu <model>        CPU: 68000 | 68030 (default: 68000)\n  \
                           --ram-mb <1..12>     Main RAM size in MB (default: saved in SRAM, else 4)\n  \
                           --scale <1..4>       Initial window scale (default: 1 = 768x512)\n  \
                           --resolution <WxH>   Initial screen resolution (e.g. 768x512, 1024x768, 1024x1024, native)\n  \
                           --fullscreen, -f     Start in host native fullscreen 1:1 mode\n  \
                           --gdb <port>         Serve the GDB remote protocol on 127.0.0.1:<port>\n\n\
                         Hotkeys:\n  \
                           F1: 10MHz | F2: 16MHz | F3: 24MHz | F4: Burst | F5: Cycle Resolution | F12: Reset"
                    );
                    std::process::exit(0);
                }
                _ => {}
            }
        }
        cfg
    }
}

struct GpuWindowState {
    presenter: ViconSurfacePresenter,
    compositor: ViconGpuCompositor,
    surface_config: wgpu::SurfaceConfiguration,
    surface: wgpu::Surface<'static>,
    window: Arc<Window>,
}

/// A VM opened by VMX68K.X, running on a thread of its own and shown in its window.
struct GuestVm {
    id: u32,
    /// The primary VM's window record it shows in.
    window: u32,
    /// Stops and joins the VM's thread when dropped.
    thread: VmThread,
    output: Arc<VmScreenOutput>,
    /// The VM's drive accesses, counted on its thread, and the count its lamp last saw.
    accesses: Arc<AtomicU64>,
    accesses_seen: u64,
    /// Until when the VM's access lamp stays lit after its last access.
    lamp_until: Option<Instant>,
    /// The VM's SRAM each time it changed, sent from its thread, and the latest not yet handed
    /// to VMX68K.X with when it came.
    sram_changes: std::sync::mpsc::Receiver<(u64, Vec<u8>)>,
    sram_changed: Arc<std::sync::mpsc::Sender<(u64, Vec<u8>)>>,
    /// Ignore snapshots queued before a menu change to the boot device.
    sram_epoch: u64,
    sram_pending: Option<(Vec<u8>, Instant)>,
    /// The VM's SRAM as it last was, which a restart keeps, and the primary's drives it sees.
    sram: Option<Vec<u8>>,
    drives: u32,
    bridge: Option<msx68k_bus::vmx68k::VmBridge>,
    fd_accesses: [Arc<AtomicU64>; 4],
    fd_seen: [u64; 4],
    fd_until: [Option<Instant>; 4],
    media_states: std::sync::mpsc::Receiver<(u32, u32)>,
    media_state: (u32, u32),
    joypad_enabled: bool,
    joypad_map: [u8; 6],
    joypad_pressed: u8,
    joypad_latched: u8,
    joypad_latched_frame: u64,
    joypad_sent: u8,
    joypad_frames: Arc<AtomicU64>,
    vsync: bool,
}

/// The machine a VM of VMX68K.X is set up as.
#[derive(Debug, Clone, Copy)]
struct GuestMachine {
    cpu: CpuModel,
    ram_mb: u32,
    speed: CpuSpeedMode,
}

/// How long a VM's SRAM stays unchanged before VMX68K.X saves it, so that a program writing
/// it byte by byte is saved once.
const SRAM_SETTLE: Duration = Duration::from_millis(500);

/// How long a VM's access lamp stays lit after an access, which is over at once in emulation.
const LAMP_AFTERGLOW: Duration = Duration::from_millis(200);

struct Msx68kApp {
    args: AppArgs,
    vm: X68kVm,
    /// ROMs for VMs opened by VMX68K.X, which are MC68000 machines.
    guest_ipl: Vec<u8>,
    /// The IPL ROM of VMs set up with a 68030, if `roms/` has one.
    guest_ipl30: Option<Vec<u8>>,
    guest_cgrom: Vec<u8>,
    guest_vms: Vec<GuestVm>,
    /// The VM each held key went to, so its release follows it: `None` for the primary VM.
    key_targets: std::collections::HashMap<u8, Option<u32>>,
    joypad_held: std::collections::HashMap<u8, (u32, u8)>,
    gdb: Option<GdbServer>,
    gpu: Option<GpuWindowState>,
    last_title_update: Instant,
    last_title: String,
    next_frame_deadline: Instant,
    rendered_frames: u64,
    last_cursor_pos: Option<(f64, f64)>,
    mouse_left: bool,
    mouse_right: bool,
    shift_held: bool,
    /// Whether the host OS pointer is hidden because msx68k draws SX-Window's cursor.
    os_pointer_hidden: bool,
    /// Wheel motion not yet worth a whole notch, horizontally and vertically.
    wheel_remainder: (f64, f64),
    last_resolution: (u32, u32),
    last_fullscreen: bool,
    vsync: bool,
}

impl Msx68kApp {
    fn new(args: AppArgs) -> Self {
        let saved_sram = std::fs::read(&args.sram_path)
            .ok()
            .filter(|data| data.len() == 16 * 1024);
        // SRAM $ED0008 holds the end of main RAM, which SWITCH.X sets in whole megabytes.
        let ram_mb = args.ram_mb.unwrap_or_else(|| match &saved_sram {
            Some(sram) => {
                let ram_end = u32::from_be_bytes([sram[8], sram[9], sram[10], sram[11]]);
                (ram_end as usize).div_ceil(1024 * 1024).clamp(1, 12)
            }
            None => 4,
        });
        let mut vm = X68kVm::new(
            VmConfig::primary_sx()
                .with_speed_mode(args.speed_mode.unwrap_or(match args.cpu_model {
                    CpuModel::Mc68000 => CpuSpeedMode::Mhz10,
                    CpuModel::Mc68030 => CpuSpeedMode::Mhz25,
                }))
                .with_cpu_model(args.cpu_model)
                .with_ram_mb(ram_mb),
        );

        let cgrom = std::fs::read(&args.cgrom_path).unwrap_or_else(|e| {
            eprintln!(
                "Failed to read CGROM at '{}': {e}\nUse --cgrom <path> to specify CGROM.",
                args.cgrom_path.display()
            );
            std::process::exit(1);
        });
        let ipl_path = args.ipl_path.clone().unwrap_or_else(|| {
            PathBuf::from(match args.cpu_model {
                CpuModel::Mc68000 => DEFAULT_IPL_PATH,
                CpuModel::Mc68030 => DEFAULT_IPL30_PATH,
            })
        });
        let ipl = std::fs::read(&ipl_path).unwrap_or_else(|e| {
            eprintln!(
                "Failed to read IPLROM at '{}': {e}\nUse --ipl <path> to specify IPLROM.",
                ipl_path.display()
            );
            std::process::exit(1);
        });

        let gdb = args.gdb_port.map(|port| {
            let server = GdbServer::bind(port).unwrap_or_else(|e| {
                eprintln!("Failed to listen for GDB on 127.0.0.1:{port}: {e}");
                std::process::exit(1);
            });
            println!("GDB server listening on 127.0.0.1:{port}");
            server
        });

        vm.bus.load_cgrom(&cgrom);
        vm.bus.load_ipl_rom(&ipl);

        if let Some(sram_data) = &saved_sram {
            vm.bus.sram_mut().copy_from_slice(sram_data);
            // Keep the saved size unless --ram-mb overrides it or it exceeds the 12 MB limit.
            let saved_end =
                u32::from_be_bytes([sram_data[8], sram_data[9], sram_data[10], sram_data[11]]);
            let ram_bytes = (ram_mb * 1024 * 1024) as u32;
            if args.ram_mb.is_some() || saved_end > ram_bytes {
                vm.bus.sram_mut()[8..12].copy_from_slice(&ram_bytes.to_be_bytes());
            }
            println!(
                "Loaded battery-backed SRAM from '{}'",
                args.sram_path.display()
            );
        }

        if let Some(fd0) = &args.fd0_path {
            match std::fs::read(fd0) {
                Ok(mut data) => {
                    check_and_repair_cli_floppy("FD0", fd0, &mut data);
                    if vm.bus.insert_floppy(0, data, true) {
                        println!("Mounted FD0: {}", fd0.display());
                    } else {
                        eprintln!("Unsupported floppy image size for FD0: {}", fd0.display());
                    }
                }
                Err(e) => eprintln!("Failed to read FD0 '{}': {e}", fd0.display()),
            }
        }

        if let Some(fd1) = &args.fd1_path {
            if let Ok(mut data) = std::fs::read(fd1) {
                check_and_repair_cli_floppy("FD1", fd1, &mut data);
                if vm.bus.insert_floppy(1, data, true) {
                    println!("Mounted FD1: {}", fd1.display());
                }
            }
        }

        if let Some(vm_fd0) = &args.vm_fd0_path {
            if let Ok(mut data) = std::fs::read(vm_fd0) {
                check_and_repair_cli_floppy("VM FD0", vm_fd0, &mut data);
            }
        }

        if let Some(hdd0) = &args.hdd0_path {
            if !hdd0.exists() {
                let human_path = Path::new("disks/HUMAN302.XDF");
                let sx1_path = Path::new("disks/SXWIN311.XDF");
                if human_path.exists() && sx1_path.exists() {
                    println!(
                        "SCSI HDD image '{}' not found, but HUMAN302.XDF and SX-Window disks found.",
                        hdd0.display()
                    );
                    println!("Building bootable all-in-one SX-Window v3.1 SCSI HDD image (32 MB)...");
                    if let Ok(human_data) = std::fs::read(human_path) {
                        let sx_names = [
                            ("SXWIN311", "disks/SXWIN311.XDF"),
                            ("SXWIN312", "disks/SXWIN312.XDF"),
                            ("SXWIN313", "disks/SXWIN313.XDF"),
                            ("SXWIN314", "disks/SXWIN314.XDF"),
                            ("SXWIN315", "disks/SXWIN315.XDF"),
                            ("SXWIN316", "disks/SXWIN316.XDF"),
                        ];
                        let mut sx_loaded = Vec::new();
                        for (name, path_str) in &sx_names {
                            if let Ok(bytes) = std::fs::read(path_str) {
                                sx_loaded.push((*name, bytes));
                            }
                        }
                        let sx_refs: Vec<(&str, &[u8])> =
                            sx_loaded.iter().map(|(n, b)| (*n, b.as_slice())).collect();
                        let hdd_image =
                            msx68k_bus::scsi::build_bootable_sxwin_scsi_hdd(&human_data, &sx_refs, 32);
                        if let Some(parent) = hdd0.parent() {
                            let _ = std::fs::create_dir_all(parent);
                        }
                        if let Err(e) = std::fs::write(hdd0, &hdd_image) {
                            eprintln!(
                                "Failed to save generated SCSI HDD image to '{}': {e}",
                                hdd0.display()
                            );
                        } else {
                            println!(
                                "Successfully created and saved bootable SCSI HDD: {}",
                                hdd0.display()
                            );
                        }
                    }
                }
            }

            match std::fs::read(hdd0) {
                Ok(data) => {
                    if vm.bus.insert_scsi_hdd(0, data) {
                        println!("Mounted SCSI HDD0: {}", hdd0.display());
                        if args.fd0_path.is_none() {
                            vm.bus.set_boot_from_scsi(true);
                        }
                    } else {
                        eprintln!("Failed to mount SCSI HDD0: {}", hdd0.display());
                    }
                }
                Err(e) => eprintln!("Failed to read SCSI HDD0 '{}': {e}", hdd0.display()),
            }
            // Mounting points the ROM boot vector at the SCSI ROM and selects ROM boot; the boot
            // device chosen with SWITCH.X ($ED0018) must survive that.
            if let Some(sram_data) = &saved_sram {
                vm.bus.sram_mut()[0x18..0x1A].copy_from_slice(&sram_data[0x18..0x1A]);
            }
        }

        if !args.remote_drives.is_empty() {
            vm.bus.install_host_remote_drives(args.remote_drives.clone());
            for (idx, dir) in args.remote_drives.iter().enumerate() {
                println!("Mounted remote drive {}: '{}'", idx, dir.display());
            }
        }

        vm.reset().expect("Failed to reset X68000 VM");
        if args.fullscreen {
            vm.bus.fullscreen_mode = true;
        }
        if let Some((w, h)) = args.resolution {
            vm.bus.apply_screen_resolution(w, h);
            println!("Configured initial screen resolution: {}x{}", w, h);
        }
        let initial_res = vm.bus.crtc.active_resolution();
        let initial_fs = vm.bus.fullscreen_mode;
        println!(
            "Initialized X68000 VM ({:?} {:?}, {} MB RAM, initial PC=${:06X})",
            vm.cpu_model(),
            vm.speed_mode(),
            ram_mb,
            vm.cpu_state.pc
        );

        let now = Instant::now();
        // VMs opened by VMX68K.X have the CPU of their setup whatever the primary VM's is.
        let guest_ipl = std::fs::read(DEFAULT_IPL_PATH).unwrap_or_else(|_| ipl.clone());
        let vsync = args.vsync;
        Self {
            args,
            vm,
            guest_ipl,
            guest_ipl30: std::fs::read(DEFAULT_IPL30_PATH).ok(),
            guest_cgrom: cgrom,
            guest_vms: Vec::new(),
            key_targets: Default::default(),
            joypad_held: Default::default(),
            gdb,
            gpu: None,
            last_title_update: now,
            last_title: String::new(),
            next_frame_deadline: now,
            rendered_frames: 0,
            last_cursor_pos: None,
            mouse_left: false,
            mouse_right: false,
            shift_held: false,
            os_pointer_hidden: false,
            wheel_remainder: (0.0, 0.0),
            last_resolution: initial_res,
            last_fullscreen: initial_fs,
            vsync,
        }
    }

    fn flush_persistent_storage(&mut self, force: bool) {
        if force || self.vm.bus.take_sram_dirty() {
            if let Some(parent) = self.args.sram_path.parent() {
                if !parent.as_os_str().is_empty() {
                    let _ = std::fs::create_dir_all(parent);
                }
            }
            if let Err(e) = std::fs::write(&self.args.sram_path, self.vm.bus.sram()) {
                eprintln!(
                    "Warning: Failed to flush SRAM to '{}': {e}",
                    self.args.sram_path.display()
                );
            }
        }
        if force || self.vm.bus.scsi.take_dirty() {
            if let (Some(hdd0_path), Some(hdd_data)) =
                (&self.args.hdd0_path, self.vm.bus.scsi.hdd_data(0))
            {
                if let Err(e) = std::fs::write(hdd0_path, hdd_data) {
                    eprintln!(
                        "Warning: Failed to flush HDD 0 to '{}': {e}",
                        hdd0_path.display()
                    );
                }
            }
        }
    }

    /// Presents the current guest screen and refreshes the window title.
    fn render_frame(&mut self) {
        // Show one cursor: while msx68k draws SX-Window's, the OS pointer over the window hides.
        let hide_os_pointer = self.vm.bus.host_draws_cursor();
        if hide_os_pointer != self.os_pointer_hidden {
            if let Some(gpu) = &self.gpu {
                gpu.window.set_cursor_visible(!hide_os_pointer);
                self.os_pointer_hidden = hide_os_pointer;
            }
        }
        let active_res = self.vm.bus.crtc.active_resolution();
        let active_fs = self.vm.bus.fullscreen_mode;
        if active_res != self.last_resolution || active_fs != self.last_fullscreen {
            self.last_resolution = active_res;
            self.last_fullscreen = active_fs;
            if let Some(gpu) = &mut self.gpu {
                if active_fs {
                    gpu.window.set_fullscreen(Some(winit::window::Fullscreen::Borderless(None)));
                    gpu.surface_config.width = active_res.0.max(1);
                    gpu.surface_config.height = active_res.1.max(1);
                    gpu.surface.configure(gpu.compositor.device(), &gpu.surface_config);
                    println!(
                        "Configured fullscreen mode for {}x{}",
                        active_res.0, active_res.1
                    );
                } else {
                    gpu.window.set_fullscreen(None);
                    let new_w = active_res.0 * self.args.scale;
                    let new_h = active_res.1 * self.args.scale;
                    let applied = gpu
                        .window
                        .request_inner_size(PhysicalSize::new(new_w, new_h))
                        .unwrap_or(PhysicalSize::new(new_w, new_h));
                    gpu.surface_config.width = applied.width.max(1);
                    gpu.surface_config.height = applied.height.max(1);
                    gpu.surface
                        .configure(gpu.compositor.device(), &gpu.surface_config);
                    println!(
                        "Auto-resized window to {}x{} (Guest resolution {}x{})",
                        applied.width, applied.height, active_res.0, active_res.1
                    );
                }
            }
        }

        if let Some(gpu) = &mut self.gpu {
            let frame = match gpu.surface.get_current_texture() {
                Ok(f) => f,
                Err(wgpu::SurfaceError::Lost | wgpu::SurfaceError::Outdated) => {
                    gpu.surface
                        .configure(gpu.compositor.device(), &gpu.surface_config);
                    return;
                }
                Err(_) => return,
            };

            let view = frame
                .texture
                .create_view(&wgpu::TextureViewDescriptor::default());

            // Each VM's screen goes where its window shows, clipped to the visible parts.
            let placed: Vec<_> = self
                .guest_vms
                .iter()
                .filter_map(|guest| {
                    let layout = self.vm.bus.vm_window_layout(guest.window)?;
                    let visible: Vec<ScreenRect> = layout
                        .visible
                        .iter()
                        .map(|&(left, top, right, bottom)| ScreenRect {
                            left,
                            top,
                            right,
                            bottom,
                        })
                        .collect();
                    Some((guest, layout.left, layout.top, layout.width, layout.height, visible))
                })
                .collect();
            let layers: Vec<VmLayer> = placed
                .iter()
                .map(|(guest, left, top, width, height, visible)| VmLayer {
                    output: &guest.output,
                    left: *left,
                    top: *top,
                    width: *width,
                    height: *height,
                    visible,
                })
                .collect();
            gpu.compositor.render_bus_to_surface_view_dirty(
                &mut self.vm.bus,
                &gpu.presenter,
                &view,
                gpu.surface_config.width,
                gpu.surface_config.height,
                &layers,
            );

            frame.present();
            self.rendered_frames += 1;

            if self.last_title_update.elapsed() >= Duration::from_millis(250) {
                self.last_title_update = Instant::now();
                let (res_w, res_h) = self.vm.bus.crtc.active_resolution();
                let speed_label = match self.vm.speed_mode() {
                    CpuSpeedMode::Mhz10 => "10MHz",
                    CpuSpeedMode::Mhz16 => "16MHz (XVI)",
                    CpuSpeedMode::Mhz24 => "24MHz (RedZone)",
                    CpuSpeedMode::Mhz25 => "25MHz (X68030)",
                    CpuSpeedMode::Burst => "Burst",
                    CpuSpeedMode::Custom(_) => "Custom",
                };
                let fdd_led = if self.vm.bus.fdc.drives[0].motor_on {
                    "FD0:BUSY"
                } else if self.vm.bus.fdc.drives[0].is_inserted() {
                    "FD0:READY"
                } else {
                    "FD0:EJECT"
                };
                let scsi_led = if self.vm.bus.is_scsi_rom_enabled() {
                    " | SCSI:ONLINE"
                } else {
                    ""
                };
                let cpu_label = match self.vm.cpu_model() {
                    CpuModel::Mc68000 => "68000",
                    CpuModel::Mc68030 => "68030",
                };
                let vsync_label = if self.vsync { " | VSync" } else { "" };
                let title = format!(
                    "msx68k — X68000 [{cpu_label} {speed_label} | {res_w}x{res_h} | {fdd_led}{scsi_led}{vsync_label}] (F1:10M F2:16M F3:24M F4:Burst F5:Res F6:VSync F12:Reset)"
                );
                if title != self.last_title {
                    gpu.window.set_title(&title);
                    self.last_title = title;
                }
                self.flush_persistent_storage(false);
            }
        }
    }

    fn frame_budget(&self) -> FrameBudget {
        match self.vm.speed_mode() {
            CpuSpeedMode::Burst => {
                // Only the CPU runs flat out: peripherals follow host time from power-on, so
                // guest timers (title screens, Timer C, V-Blank, double-click speed) keep their
                // real rates.
                FrameBudget::RealTime {
                    deadline: self.next_frame_deadline - BURST_RENDER_RESERVE,
                }
            }
            mode => {
                let cpu_mhz = mode.timing_config().cpu_mhz.max(10);
                let frame_cycles = (cpu_mhz * 1_000_000) / 60;
                if self.vsync {
                    FrameBudget::VSync {
                        max_cycles: frame_cycles * 2,
                    }
                } else {
                    FrameBudget::Cycles(frame_cycles)
                }
            }
        }
    }

    fn sync_joypad_port_a(guest: &mut GuestVm) {
        let port_a = !(guest.joypad_pressed | guest.joypad_latched);
        if port_a != guest.joypad_sent {
            guest.joypad_sent = port_a;
            guest.thread.post(move |vm| vm.bus.ppi.set_port_a(port_a));
        }
    }

    /// Sends a key to the VM whose window SX-Window has active, straight into its MFP, or to
    /// the primary VM. A release goes where its press went.
    fn send_key(&mut self, scancode: u8, pressed: bool, repeat: bool) {
        if pressed {
            let active = self.vm.bus.active_window();
            if let Some(guest) = self
                .guest_vms
                .iter_mut()
                .find(|guest| Some(guest.window) == active)
            {
                if guest.joypad_enabled {
                    let mask = guest
                        .joypad_map
                        .iter()
                        .zip(JOYPAD_BITS)
                        .filter(|(&mapped, _)| mapped == scancode)
                        .fold(0u8, |acc, (_, bit)| acc | bit);
                    if mask != 0 {
                        self.joypad_held.insert(scancode, (guest.id, mask));
                        guest.joypad_pressed |= mask;
                        if !repeat {
                            guest.joypad_latched |= mask;
                            guest.joypad_latched_frame =
                                guest.joypad_frames.load(Ordering::Relaxed);
                        }
                        Self::sync_joypad_port_a(guest);
                        return;
                    }
                }
            }
        } else if !repeat {
            if let Some((id, mask)) = self.joypad_held.remove(&scancode) {
                if let Some(guest) = self.guest_vms.iter_mut().find(|guest| guest.id == id) {
                    guest.joypad_pressed &= !mask;
                    Self::sync_joypad_port_a(guest);
                }
                return;
            }
        } else if self.joypad_held.contains_key(&scancode) {
            return;
        }
        if repeat {
            return;
        }
        let target = if pressed {
            let active = self.vm.bus.active_window();
            let guest = self
                .guest_vms
                .iter()
                .find(|guest| Some(guest.window) == active)
                .map(|guest| guest.id);
            self.key_targets.insert(scancode, guest);
            guest
        } else {
            self.key_targets.remove(&scancode).flatten()
        };
        let byte = if pressed { scancode } else { scancode | 0x80 };
        match target.and_then(|id| self.guest_vms.iter().find(|guest| guest.id == id)) {
            Some(guest) => guest.thread.post(move |vm| vm.bus.mfp.push_keyboard_byte(byte)),
            None => self.vm.bus.mfp.push_keyboard_byte(byte),
        }
    }

    /// Starts and stops the VMs that VMX68K.X opened and closed.
    fn update_guest_vms(&mut self) {
        for event in self.vm.bus.take_vm_events() {
            match event {
                VmEvent::Opened { id, window, drives, speed, sram, cpu, ram_mb, custom, .. } => {
                    let machine = GuestMachine { cpu, ram_mb, speed };
                    if let Some(guest) = self.start_guest_vm(id, window, drives, machine, sram, custom) {
                        self.guest_vms.push(guest);
                    } else {
                        self.vm.bus.detach_vm_bridge(id);
                    }
                }
                VmEvent::Restart { id, cpu, ram_mb, speed, custom } => {
                    let Some(at) = self.guest_vms.iter().position(|guest| guest.id == id) else {
                        continue;
                    };
                    // Nothing may wait on the bridge while the old VM's thread stops.
                    self.vm.bus.detach_vm_bridge(id);
                    let old = self.guest_vms.remove(at);
                    let (window, drives, sram, joypad_enabled, joypad_map, vsync) = (
                        old.window,
                        old.drives,
                        old.sram.clone(),
                        old.joypad_enabled,
                        old.joypad_map,
                        old.vsync,
                    );
                    drop(old);
                    let machine = GuestMachine { cpu, ram_mb, speed };
                    if let Some(mut guest) = self.start_guest_vm(id, window, drives, machine, sram, custom) {
                        guest.joypad_enabled = joypad_enabled;
                        guest.joypad_map = joypad_map;
                        guest.vsync = vsync;
                        guest.thread.set_vsync(vsync || self.vsync);
                        self.guest_vms.push(guest);
                    } else {
                        self.vm.bus.detach_vm_bridge(id);
                    }
                }
                VmEvent::Media {
                    id,
                    unit,
                    handle,
                    len,
                } => {
                    if let Some(guest) = self.guest_vms.iter().find(|guest| guest.id == id) {
                        if let Some(bridge) = guest.bridge.clone() {
                            let image = self.vm.bus.take_vm_image(handle);
                            guest.thread.post(move |vm| {
                                let disk = (handle != u32::MAX).then(|| {
                                    if let Some(image) = image {
                                        msx68k_bus::vmx68k::RemoteDisk::with_image(
                                            bridge.clone(),
                                            handle,
                                            len as usize,
                                            image,
                                        )
                                    } else {
                                        msx68k_bus::vmx68k::RemoteDisk::with_handle(
                                            bridge.clone(),
                                            handle,
                                            len as usize,
                                        )
                                    }
                                });
                                vm.bus.change_vm_floppy(unit as usize, disk, &bridge);
                            });
                        }
                    }
                }
                VmEvent::Boot { id, choice, rom } => {
                    if let Some(guest) = self.guest_vms.iter_mut().find(|guest| guest.id == id) {
                        if let Some(sram) = guest.sram.as_mut() {
                            msx68k_bus::vmx68k::apply_boot_choice(sram, choice, rom);
                        }
                        guest.sram_epoch = guest.sram_epoch.wrapping_add(1);
                        guest.sram_pending = None;
                        let epoch = guest.sram_epoch;
                        let changed = Arc::clone(&guest.sram_changed);
                        guest.thread.post(move |vm| {
                            msx68k_bus::vmx68k::apply_boot_choice(vm.bus.sram_mut(), choice, rom);
                            let _ = changed.send((epoch, vm.bus.sram().to_vec()));
                        });
                    }
                }
                VmEvent::Speed { id, mode } => {
                    if let Some(guest) = self.guest_vms.iter().find(|guest| guest.id == id) {
                        guest.thread.post(move |vm| vm.set_speed_mode(mode));
                    }
                }
                VmEvent::SpriteOptions { id, options } => {
                    if let Some(guest) = self.guest_vms.iter().find(|guest| guest.id == id) {
                        guest.thread.post(move |vm| vm.bus.sprite_options = options);
                    }
                }
                VmEvent::Reset { id } => {
                    if let Some(guest) = self.guest_vms.iter().find(|guest| guest.id == id) {
                        guest.thread.post(move |vm| {
                            if let Err(e) = vm.reset() {
                                eprintln!("VM {id} failed to reset: {e:?}");
                            }
                        });
                    }
                }
                VmEvent::Interrupt { id } => {
                    if let Some(guest) = self.guest_vms.iter().find(|guest| guest.id == id) {
                        guest.thread.post(move |vm| vm.bus.request_nmi());
                    }
                }
                VmEvent::Joypad { id, enabled, map } => {
                    if let Some(guest) = self.guest_vms.iter_mut().find(|guest| guest.id == id) {
                        guest.joypad_enabled = enabled;
                        guest.joypad_map = map;
                        if !enabled {
                            guest.joypad_pressed = 0;
                            guest.joypad_latched = 0;
                            Self::sync_joypad_port_a(guest);
                        }
                    }
                }
                VmEvent::VSync { id, enabled } => {
                    if let Some(guest) = self.guest_vms.iter_mut().find(|guest| guest.id == id) {
                        guest.vsync = enabled;
                        guest.thread.set_vsync(enabled || self.vsync);
                    }
                }
                VmEvent::Closed { id } => self.guest_vms.retain(|guest| guest.id != id),
            }
        }
    }

    /// Lights each VM's access lamp while it reaches its drives, and for a while after, reports
    /// the size of its screen, and hands its SRAM to VMX68K.X to save when it changed.
    fn update_vm_lamps(&mut self) {
        let now = Instant::now();
        for guest in &mut self.guest_vms {
            if guest.joypad_latched != 0
                && guest
                    .joypad_frames
                    .load(Ordering::Relaxed)
                    .wrapping_sub(guest.joypad_latched_frame)
                    >= 2
            {
                guest.joypad_latched = 0;
                Self::sync_joypad_port_a(guest);
            }
            let accesses = guest.accesses.load(Ordering::Relaxed);
            if accesses != guest.accesses_seen {
                guest.accesses_seen = accesses;
                guest.lamp_until = Some(now + LAMP_AFTERGLOW);
            }
            let lit = guest.lamp_until.is_some_and(|until| now < until);
            self.vm.bus.set_vm_lamp(guest.id, lit);
            while let Ok(state) = guest.media_states.try_recv() {
                guest.media_state = state;
            }
            let mut packed = guest.media_state.0;
            for unit in 0..4 {
                let accesses = guest.fd_accesses[unit].load(Ordering::Relaxed);
                if accesses != guest.fd_seen[unit] {
                    guest.fd_seen[unit] = accesses;
                    guest.fd_until[unit] = Some(now + LAMP_AFTERGLOW);
                }
                if guest.fd_until[unit].is_some_and(|until| now < until) {
                    packed |= 1 << (8 + unit);
                }
            }
            self.vm
                .bus
                .set_vm_floppies(guest.id, packed, guest.media_state.1);
            // VMX68K.X sizes its window to the VM's screen as the VM changes its mode.
            if let Some(frame) = guest.output.completed() {
                self.vm.bus.set_vm_screen_size(guest.id, frame.width, frame.height);
            }
            // The VM's SRAM goes to VMX68K.X to save in its setup once it settles.
            let changed = Arc::clone(&guest.sram_changed);
            let epoch = guest.sram_epoch;
            guest.thread.post(move |vm| {
                if vm.bus.take_sram_dirty() {
                    let _ = changed.send((epoch, vm.bus.sram().to_vec()));
                }
            });
            while let Ok((epoch, sram)) = guest.sram_changes.try_recv() {
                if epoch != guest.sram_epoch {
                    continue;
                }
                guest.sram = Some(sram.clone());
                guest.sram_pending = Some((sram, now));
            }
            if guest.sram_pending.as_ref().is_some_and(|(_, at)| now >= *at + SRAM_SETTLE) {
                let (sram, _) = guest.sram_pending.take().unwrap();
                self.vm.bus.set_vm_sram(guest.id, sram);
            }
        }
    }

    /// Stops every VM, detaching its disk first so its thread is not left waiting for it.
    fn stop_guest_vms(&mut self) {
        for guest in &self.guest_vms {
            self.vm.bus.detach_vm_bridge(guest.id);
        }
        self.guest_vms.clear();
    }

    fn start_guest_vm(
        &mut self,
        id: u32,
        window: u32,
        drives: u32,
        machine: GuestMachine,
        sram: Option<Vec<u8>>,
        custom: Option<Vec<msx68k_bus::vmx68k::CustomDrive>>,
    ) -> Option<GuestVm> {
        let gpu = self.gpu.as_ref()?;
        // A 68030 needs the X68030's IPL ROM.
        let (cpu, ipl) = match (machine.cpu, &self.guest_ipl30) {
            (CpuModel::Mc68030, Some(ipl30)) => (CpuModel::Mc68030, ipl30),
            _ => (CpuModel::Mc68000, &self.guest_ipl),
        };
        let speed = if cpu == machine.cpu { machine.speed } else { CpuSpeedMode::Mhz10 };
        let mut vm = X68kVm::new(
            VmConfig::app_vm()
                .with_cpu_model(cpu)
                .with_speed_mode(speed)
                .with_ram_mb(machine.ram_mb as usize),
        );
        vm.bus.sprite_options = self.vm.bus.vm_sprite_options(id);
        // The VM's own SRAM from its setup, with the size of its RAM as the machine has it.
        if let Some(sram) = sram.as_ref().filter(|sram| sram.len() == vm.bus.sram().len()) {
            vm.bus.sram_mut().copy_from_slice(sram);
            vm.bus.sram_mut()[8..12].copy_from_slice(&(machine.ram_mb << 20).to_be_bytes());
        }
        vm.bus.load_cgrom(&self.guest_cgrom);
        vm.bus.load_ipl_rom(ipl);
        let mut storage_bridge = None;
        if let Some(path) = &self.args.vm_fd0_path {
            // The debug override does not use the setup's reserved physical drives.
            self.vm.bus.detach_vm_bridge(id);
            // Write-protected, and only ever a copy: the image on the host stays as it is.
            match std::fs::read(path) {
                Ok(data) => {
                    if !vm.bus.insert_floppy(0, data, true) {
                        eprintln!("Unsupported floppy image size for VMs: {}", path.display());
                    }
                }
                Err(e) => eprintln!("Failed to read '{}' for VMs: {e}", path.display()),
            }
        } else if let Some(custom) = custom {
            let (mut bridge, requests) = msx68k_bus::vmx68k::vm_bridge();
            self.vm.bus.bind_vm_passthrough(id, &mut bridge);
            storage_bridge = Some(bridge.clone());
            for drive in vm.bus.install_custom_drives(bridge, &custom) {
                eprintln!("VM {id}: custom drive not supported yet: {drive:?}");
            }
            self.vm.bus.attach_vm_bridge(id, requests);
        } else {
            // Shared mode: the VM sees the primary's drives under the same letters, bridged
            // at the file level through VMX68K.X, and boots from A:. The floppy image mode for
            // drive 0 comes with the VM's settings.
            let (bridge, requests) = msx68k_bus::vmx68k::vm_bridge();
            let drives = (1..=drives.max(1)).collect();
            vm.bus
                .install_remote_boot(msx68k_bus::remote_fs::RemoteFs::new(bridge, drives));
            self.vm.bus.attach_vm_bridge(id, requests);
        }
        if let Err(e) = vm.reset() {
            eprintln!("VM {id} failed to reset: {e:?}");
            self.vm.bus.detach_vm_bridge(id);
            return None;
        }
        let accesses = vm.bus.drive_access_counter();
        let fd_accesses = std::array::from_fn(|unit| Arc::clone(&vm.bus.fdc.drives[unit].accesses));
        let initial_sram = vm.bus.sram().to_vec();
        self.vm.bus.set_vm_sram(id, initial_sram.clone());
        let custom_media = storage_bridge.is_some();
        let initial_media = (
            if custom_media {
                vm.bus.floppy_state()
            } else {
                0
            },
            msx68k_bus::vmx68k::boot_choice(vm.bus.sram()),
        );
        let (media_state, media_states) = std::sync::mpsc::sync_channel(1);
        let (sram_changed, sram_changes) = std::sync::mpsc::channel();
        let joypad_frames = Arc::new(AtomicU64::new(0));
        let vblank_frames = Arc::clone(&joypad_frames);
        let mut screen = VmScreen::new(gpu.compositor.device(), gpu.compositor.queue());
        let output = screen.output();
        let thread = VmThread::spawn_with_vblank(vm, Pace::RealTime, move |vm| {
            vblank_frames.fetch_add(1, Ordering::Relaxed);
            let _ = media_state.try_send((
                if custom_media {
                    vm.bus.floppy_state()
                } else {
                    0
                },
                msx68k_bus::vmx68k::boot_choice(vm.bus.sram()),
            ));
            screen.present(&mut vm.bus);
        });
        thread.set_vsync(self.vsync);
        println!("Started VM {id} for window ${window:06X}");
        Some(GuestVm {
            id,
            window,
            thread,
            output,
            accesses,
            accesses_seen: 0,
            lamp_until: None,
            sram_changes,
            sram_changed: Arc::new(sram_changed),
            sram_pending: None,
            sram_epoch: 0,
            sram: Some(initial_sram),
            drives,
            bridge: storage_bridge,
            fd_accesses,
            fd_seen: [0; 4],
            fd_until: [None; 4],
            media_states,
            media_state: initial_media,
            joypad_enabled: false,
            joypad_map: [0x3C, 0x3E, 0x3B, 0x3D, 0x2A, 0x2B],
            joypad_pressed: 0,
            joypad_latched: 0,
            joypad_latched_frame: 0,
            joypad_sent: 0xFF,
            joypad_frames,
            vsync: false,
        })
    }

    fn step_vm_one_frame(&mut self) {
        let budget = self.frame_budget();
        match &mut self.gdb {
            Some(gdb) if gdb.is_connected() => gdb.run_frame(&mut self.vm, budget),
            _ => self.vm.run_frame(budget),
        }
    }
}

impl ApplicationHandler for Msx68kApp {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.gpu.is_some() {
            return;
        }

        let (active_w, active_h) = self.vm.bus.crtc.active_resolution();
        let win_w = active_w * self.args.scale;
        let win_h = active_h * self.args.scale;
        let attrs = Window::default_attributes()
            .with_title("msx68k — Sharp X68000")
            .with_inner_size(PhysicalSize::new(win_w, win_h));

        let window = Arc::new(
            event_loop
                .create_window(attrs)
                .expect("Failed to create window"),
        );

        if let Some(monitor) = window.current_monitor().or_else(|| window.primary_monitor()) {
            let msize = monitor.size();
            let nw = msize.width.clamp(768, 3840);
            let nh = msize.height.clamp(512, 2160);
            self.vm.bus.host_native_resolution = (nw, nh);
        }
        if self.vm.bus.fullscreen_mode {
            let (nw, nh) = self.vm.bus.host_native_resolution;
            self.vm.bus.apply_screen_resolution(nw, nh);
            window.set_fullscreen(Some(winit::window::Fullscreen::Borderless(None)));
        }

        let instance = wgpu::Instance::new(&wgpu::InstanceDescriptor {
            backends: wgpu::Backends::all(),
            ..Default::default()
        });

        let surface = instance
            .create_surface(window.clone())
            .expect("Failed to create wgpu surface");

        let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
            power_preference: wgpu::PowerPreference::HighPerformance,
            compatible_surface: Some(&surface),
            force_fallback_adapter: false,
        }))
        .expect("No suitable wgpu GPU adapter found");

        let adapter_info = adapter.get_info();
        println!(
            "Using GPU adapter: {} ({:?})",
            adapter_info.name, adapter_info.backend
        );

        let (device, queue) = pollster::block_on(
            adapter.request_device(&wgpu::DeviceDescriptor::default(), None),
        )
        .expect("Failed to create wgpu device");

        let size = window.inner_size();
        let caps = surface.get_capabilities(&adapter);
        let format = caps
            .formats
            .iter()
            .copied()
            .find(|f| !f.is_srgb())
            .unwrap_or(caps.formats[0]);

        // FIFO can block until the compositor repaints, which would stall this thread and the
        // guest with it.
        let present_mode = [wgpu::PresentMode::Mailbox, wgpu::PresentMode::Immediate]
            .into_iter()
            .find(|mode| caps.present_modes.contains(mode))
            .unwrap_or(wgpu::PresentMode::Fifo);

        let surface_config = wgpu::SurfaceConfiguration {
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
            format,
            width: size.width.max(1),
            height: size.height.max(1),
            present_mode,
            alpha_mode: caps.alpha_modes[0],
            view_formats: vec![],
            desired_maximum_frame_latency: 2,
        };
        surface.configure(&device, &surface_config);
        println!("Using present mode: {present_mode:?}");

        let compositor = ViconGpuCompositor::from_device(device, queue, adapter_info);
        let presenter = compositor.create_surface_presenter(format);

        self.gpu = Some(GpuWindowState {
            presenter,
            compositor,
            surface_config,
            surface,
            window,
        });
        self.next_frame_deadline = Instant::now();
    }

    fn window_event(
        &mut self,
        event_loop: &ActiveEventLoop,
        _window_id: WindowId,
        event: WindowEvent,
    ) {
        match event {
            WindowEvent::CloseRequested => {
                self.flush_persistent_storage(true);
                // VM threads present to the GPU, so they stop before it goes.
                self.stop_guest_vms();
                if let Some(gpu) = &self.gpu {
                    let _ = gpu.compositor.device().poll(wgpu::Maintain::Wait);
                }
                self.gpu = None;
                event_loop.exit();
            }
            WindowEvent::Resized(new_size) => {
                if let Some(gpu) = &mut self.gpu {
                    if new_size.width > 0 && new_size.height > 0 {
                        gpu.surface_config.width = new_size.width;
                        gpu.surface_config.height = new_size.height;
                        gpu.surface
                            .configure(gpu.compositor.device(), &gpu.surface_config);
                    }
                }
            }
            WindowEvent::CursorMoved { position, .. } => {
                let (x, y) = if let Some(gpu) = &self.gpu {
                    let (res_w, res_h) = self.vm.bus.crtc.active_resolution();
                    let sw = gpu.surface_config.width.max(1) as f64;
                    let sh = gpu.surface_config.height.max(1) as f64;
                    let rw = res_w.max(1) as f64;
                    let rh = res_h.max(1) as f64;
                    let target_aspect = rw / rh;
                    let window_aspect = sw / sh;
                    let (vp_x, vp_y, vp_w, vp_h) = if window_aspect > target_aspect {
                        let w = sh * target_aspect;
                        ((sw - w) * 0.5, 0.0, w, sh)
                    } else {
                        let h = sw / target_aspect;
                        (0.0, (sh - h) * 0.5, sw, h)
                    };
                    (
                        ((position.x - vp_x) * (rw / vp_w.max(1.0))).clamp(0.0, (rw - 1.0).max(0.0)),
                        ((position.y - vp_y) * (rh / vp_h.max(1.0))).clamp(0.0, (rh - 1.0).max(0.0)),
                    )
                } else {
                    (position.x, position.y)
                };
                let guest_x = x.round() as i16;
                let guest_y = y.round() as i16;
                self.vm.bus.set_host_mouse_position(Some((guest_x, guest_y)));
                if let Some((lx, ly)) = self.last_cursor_pos {
                    let dx = (x - lx).round() as i32;
                    let dy = (y - ly).round() as i32;
                    if dx != 0 || dy != 0 {
                        self.vm.bus.scc.add_mouse_delta(dx, dy);
                        self.last_cursor_pos = Some((lx + dx as f64, ly + dy as f64));
                    }
                } else {
                    self.last_cursor_pos = Some((x, y));
                }
            }
            WindowEvent::CursorLeft { .. } => {
                self.last_cursor_pos = None;
                self.vm.bus.set_host_mouse_position(None);
            }
            WindowEvent::MouseInput { state, button, .. } => {
                let pressed = state == ElementState::Pressed;
                match button {
                    MouseButton::Left => self.mouse_left = pressed,
                    MouseButton::Right => self.mouse_right = pressed,
                    _ => {}
                }
                self.vm
                    .bus
                    .scc
                    .set_mouse_buttons(self.mouse_left, self.mouse_right);
                self.vm.bus.scc.add_mouse_delta(0, 0);
            }
            WindowEvent::ModifiersChanged(modifiers) => {
                self.shift_held = modifiers.state().shift_key();
            }
            WindowEvent::MouseWheel { delta, .. } => {
                let pixels_per_notch = self.vm.bus.wheel_config.trackpad_pixels as f64;
                let (x, y) = match delta {
                    MouseScrollDelta::LineDelta(x, y) => (x as f64, y as f64),
                    MouseScrollDelta::PixelDelta(pos) => {
                        (pos.x / pixels_per_notch, pos.y / pixels_per_notch)
                    }
                };
                // Shift turns the wheel sideways.
                let (x, y) = if self.shift_held { (x + y, 0.0) } else { (x, y) };
                self.wheel_remainder.0 += x;
                self.wheel_remainder.1 += y;
                let whole = |remainder: &mut f64| {
                    let notches = remainder.trunc();
                    *remainder -= notches;
                    notches as i32
                };
                // Positive deltas move the content right and down, revealing what is left and
                // above, so they scroll toward the start.
                let horizontal = whole(&mut self.wheel_remainder.0);
                let vertical = whole(&mut self.wheel_remainder.1);
                if vertical != 0 {
                    self.vm.bus.post_mouse_wheel(-vertical, false);
                }
                if horizontal != 0 {
                    self.vm.bus.post_mouse_wheel(-horizontal, true);
                }
            }
            WindowEvent::KeyboardInput { event, .. } => {
                if let PhysicalKey::Code(code) = event.physical_key {
                    if event.state == ElementState::Pressed {
                        match code {
                            KeyCode::F1 => {
                                self.vm.set_speed_mode(CpuSpeedMode::Mhz10);
                                println!("Switched CPU speed to 10MHz (Bus 10MHz)");
                                return;
                            }
                            KeyCode::F2 => {
                                self.vm.set_speed_mode(CpuSpeedMode::Mhz16);
                                println!("Switched CPU speed to 16MHz (XVI, Bus 16MHz)");
                                return;
                            }
                            KeyCode::F3 => {
                                self.vm.set_speed_mode(CpuSpeedMode::Mhz24);
                                println!("Switched CPU speed to 24MHz (RedZone, Bus 16MHz)");
                                return;
                            }
                            KeyCode::F4 => {
                                self.vm.set_speed_mode(CpuSpeedMode::Burst);
                                println!("Switched CPU speed to Burst (0 wait)");
                                return;
                            }
                            KeyCode::F5 => {
                                let (cur_w, cur_h) = self.vm.bus.crtc.active_resolution();
                                let (next_w, next_h) = match (cur_w, cur_h) {
                                    (768, 512) => (1024, 768),
                                    (1024, 768) => (1024, 1024),
                                    (1024, 1024) => (1280, 960),
                                    (1280, 960) => (1920, 1080),
                                    _ => (768, 512),
                                };
                                self.vm.bus.fullscreen_mode = false;
                                self.vm.bus.request_screen_resolution(next_w, next_h);
                                println!("F5: Requested resolution {}x{}", next_w, next_h);
                                return;
                            }
                            KeyCode::F6 => {
                                self.vsync = !self.vsync;
                                for guest in &self.guest_vms {
                                    guest.thread.set_vsync(guest.vsync || self.vsync);
                                }
                                println!(
                                    "F6: Host VSync lock {}",
                                    if self.vsync { "ON" } else { "OFF" }
                                );
                                return;
                            }
                            KeyCode::F12 => {
                                self.flush_persistent_storage(true);
                                let _ = self.vm.reset();
                                println!("Reset X68000 VM");
                                return;
                            }
                            _ => {}
                        }
                    }
                    if let Some(sc) = map_keycode_to_x68k_scancode(code) {
                        self.send_key(sc, event.state == ElementState::Pressed, event.repeat);
                    }
                }
            }
            WindowEvent::RedrawRequested => self.render_frame(),
            _ => {}
        }
    }

    fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
        let budget = self.frame_budget();
        let mut gdb_connected = false;
        if let Some(gdb) = &mut self.gdb {
            gdb.poll(&mut self.vm, budget);
            gdb_connected = gdb.is_connected();
        }

        let now = Instant::now();
        let frame_interval = Duration::from_micros(16_667); // ~60 Hz
        if now >= self.next_frame_deadline {
            self.next_frame_deadline += frame_interval;
            if now >= self.next_frame_deadline + frame_interval {
                self.next_frame_deadline = now + frame_interval;
            }
            // Run and present here rather than on redraw requests, which a compositor may withhold
            // indefinitely, for example while the window is covered.
            self.step_vm_one_frame();
            self.update_guest_vms();
            self.update_vm_lamps();
            self.render_frame();
        }
        // A GDB client waits for each reply, so keep round trips well below a frame.
        let wake_at = if gdb_connected {
            self.next_frame_deadline.min(now + Duration::from_millis(1))
        } else {
            self.next_frame_deadline
        };
        event_loop.set_control_flow(ControlFlow::WaitUntil(wake_at));
    }
}

impl Drop for Msx68kApp {
    fn drop(&mut self) {
        self.flush_persistent_storage(true);
        // VM threads present to the GPU at every vertical blank, so they stop before it goes.
        self.stop_guest_vms();
        if let Some(gpu) = &self.gpu {
            let _ = gpu.compositor.device().poll(wgpu::Maintain::Wait);
        }
        self.gpu = None;
    }
}

fn main() {
    let args = AppArgs::parse();
    let event_loop = EventLoop::new().expect("Failed to create winit EventLoop");
    let mut app = Msx68kApp::new(args);
    event_loop
        .run_app(&mut app)
        .expect("EventLoop terminated with error");
}
