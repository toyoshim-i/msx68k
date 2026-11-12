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

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use msx68k_bus::{CpuModel, CpuSpeedMode, FrameBudget, GdbServer, VmConfig, X68kVm};
use msx68k_vicon::{ViconGpuCompositor, ViconSurfacePresenter};
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

/// Host time left free each frame for rendering while Burst runs flat out.
const BURST_RENDER_RESERVE: Duration = Duration::from_millis(3);

#[derive(Debug, Clone)]
struct AppArgs {
    /// IPL ROM given by `--ipl` or `MSX68K_IPL`; otherwise the one for the CPU is used.
    ipl_path: Option<PathBuf>,
    cgrom_path: PathBuf,
    fd0_path: Option<PathBuf>,
    fd1_path: Option<PathBuf>,
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
    gdb_port: Option<u16>,
}

impl Default for AppArgs {
    fn default() -> Self {
        let ipl_path = std::env::var_os("MSX68K_IPL").map(PathBuf::from);
        let cgrom_path = std::env::var_os("MSX68K_CGROM")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from(DEFAULT_CGROM_PATH));
        let fd0_path = std::env::var_os("MSX68K_FD0").map(PathBuf::from);
        let fd1_path = std::env::var_os("MSX68K_FD1").map(PathBuf::from);
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
            hdd0_path,
            sram_path,
            speed_mode: None,
            cpu_model: CpuModel::Mc68000,
            ram_mb: None,
            scale: 1,
            resolution: None,
            fullscreen: false,
            gdb_port: None,
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
                "--help" | "-h" => {
                    println!(
                        "msx68k — Sharp X68000 / SX-Window Hybrid Execution Environment\n\n\
                         Options:\n  \
                           --ipl <path>         Path to the IPL ROM (default: {DEFAULT_IPL_PATH}, {DEFAULT_IPL30_PATH} for 68030)\n  \
                           --cgrom <path>       Path to CGROM.DAT  (default: {DEFAULT_CGROM_PATH})\n  \
                           --fd0 <path>         Floppy disk image for Drive 0 (.XDF/.2HD/.DIM)\n  \
                           --fd1 <path>         Floppy disk image for Drive 1 (.XDF/.2HD/.DIM)\n  \
                           --hdd0 <path>        SCSI HDD image for Target 0 (.HDS/.IMG)\n  \
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

struct Msx68kApp {
    args: AppArgs,
    vm: X68kVm,
    gdb: Option<GdbServer>,
    gpu: Option<GpuWindowState>,
    last_title_update: Instant,
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
            VmConfig::master_sx()
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
                Ok(data) => {
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
            if let Ok(data) = std::fs::read(fd1) {
                if vm.bus.insert_floppy(1, data, true) {
                    println!("Mounted FD1: {}", fd1.display());
                }
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
        Self {
            args,
            vm,
            gdb,
            gpu: None,
            last_title_update: now,
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

            gpu.compositor.render_bus_to_surface_view(
                &self.vm.bus,
                &gpu.presenter,
                &view,
                gpu.surface_config.width,
                gpu.surface_config.height,
                // VM windows join once VMX68K.SX reports them.
                &[],
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
                gpu.window.set_title(&format!(
                    "msx68k — X68000 [{cpu_label} {speed_label} | {res_w}x{res_h} | {fdd_led}{scsi_led} | PC=${:06X}] (F1:10M F2:16M F3:24M F4:Burst F5:Res F12:Reset)",
                    self.vm.cpu_state.pc
                ));
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
                // If FDD motor is spinning during floppy access, give a higher instruction budget
                // so floppy boot and program loads complete smoothly.
                let fdd_busy = self.vm.bus.fdc.drives.iter().any(|d| d.motor_on);
                let cpu_mhz = mode.timing_config().cpu_mhz.max(10);
                FrameBudget::Cycles(if fdd_busy {
                    (cpu_mhz * 1_000_000) / 15
                } else {
                    (cpu_mhz * 1_000_000) / 60
                })
            }
        }
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
                            KeyCode::F12 => {
                                self.flush_persistent_storage(true);
                                let _ = self.vm.reset();
                                println!("Reset X68000 VM");
                                return;
                            }
                            _ => {}
                        }
                    }
                    if !event.repeat {
                        if let Some(sc) = map_keycode_to_x68k_scancode(code) {
                            let byte = if event.state == ElementState::Pressed {
                                sc
                            } else {
                                sc | 0x80
                            };
                            self.vm.bus.mfp.push_keyboard_byte(byte);
                        }
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
            self.next_frame_deadline = now + frame_interval;
            // Run and present here rather than on redraw requests, which a compositor may withhold
            // indefinitely, for example while the window is covered.
            self.step_vm_one_frame();
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
