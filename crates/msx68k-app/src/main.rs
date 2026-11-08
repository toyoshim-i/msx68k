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

use msx68k_bus::{CpuSpeedMode, VmConfig, X68kVm};
use msx68k_vicon::{ViconGpuCompositor, ViconSurfacePresenter};
use winit::application::ApplicationHandler;
use winit::dpi::LogicalSize;
use winit::event::{ElementState, MouseButton, WindowEvent};
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop};
use winit::keyboard::{KeyCode, PhysicalKey};
use winit::window::{Window, WindowId};

const DEFAULT_IPL_PATH: &str = "roms/IPLROM.DAT";
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

#[derive(Debug, Clone)]
struct AppArgs {
    ipl_path: PathBuf,
    cgrom_path: PathBuf,
    fd0_path: Option<PathBuf>,
    fd1_path: Option<PathBuf>,
    hdd0_path: Option<PathBuf>,
    speed_mode: CpuSpeedMode,
    ram_mb: usize,
    scale: u32,
    resolution: Option<(u32, u32)>,
}

impl Default for AppArgs {
    fn default() -> Self {
        let ipl_path = std::env::var_os("MSX68K_IPL")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from(DEFAULT_IPL_PATH));
        let cgrom_path = std::env::var_os("MSX68K_CGROM")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from(DEFAULT_CGROM_PATH));
        let fd0_path = std::env::var_os("MSX68K_FD0").map(PathBuf::from);
        let fd1_path = std::env::var_os("MSX68K_FD1").map(PathBuf::from);
        let hdd0_path = std::env::var_os("MSX68K_HDD0").map(PathBuf::from);
        Self {
            ipl_path,
            cgrom_path,
            fd0_path,
            fd1_path,
            hdd0_path,
            speed_mode: CpuSpeedMode::Mhz10,
            ram_mb: 4,
            scale: 1,
            resolution: None,
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
                        cfg.ipl_path = PathBuf::from(p);
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
                "--speed" => {
                    if let Some(s) = args.next() {
                        cfg.speed_mode = match s.to_ascii_lowercase().as_str() {
                            "10" | "10mhz" => CpuSpeedMode::Mhz10,
                            "16" | "16mhz" => CpuSpeedMode::Mhz16,
                            "24" | "24mhz" => CpuSpeedMode::Mhz24,
                            "burst" | "0" => CpuSpeedMode::Burst,
                            _ => {
                                eprintln!("Unknown --speed '{}', defaulting to 10MHz", s);
                                CpuSpeedMode::Mhz10
                            }
                        };
                    }
                }
                "--ram-mb" => {
                    if let Some(v) = args.next().and_then(|s| s.parse::<usize>().ok()) {
                        cfg.ram_mb = v.clamp(1, 12);
                    }
                }
                "--scale" => {
                    if let Some(v) = args.next().and_then(|s| s.parse::<u32>().ok()) {
                        cfg.scale = v.clamp(1, 4);
                    }
                }
                "--resolution" => {
                    if let Some(s) = args.next() {
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
                "--help" | "-h" => {
                    println!(
                        "msx68k — Sharp X68000 / SX-Window Hybrid Execution Environment\n\n\
                         Options:\n  \
                           --ipl <path>         Path to IPLROM.DAT (default: {DEFAULT_IPL_PATH})\n  \
                           --cgrom <path>       Path to CGROM.DAT  (default: {DEFAULT_CGROM_PATH})\n  \
                           --fd0 <path>         Floppy disk image for Drive 0 (.XDF/.2HD)\n  \
                           --fd1 <path>         Floppy disk image for Drive 1 (.XDF/.2HD)\n  \
                           --hdd0 <path>        SCSI HDD image for Target 0 (.HDS/.IMG)\n  \
                           --speed <mode>       CPU speed: 10 | 16 | 24 | burst (default: 10)\n  \
                           --ram-mb <1..12>     Main RAM size in MB (default: 4)\n  \
                           --scale <1..4>       Initial window scale (default: 1 = 768x512)\n  \
                           --resolution <WxH>   Initial screen resolution (e.g. 768x512, 1024x768, 1024x1024)\n\n\
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
    window: Arc<Window>,
    surface: wgpu::Surface<'static>,
    surface_config: wgpu::SurfaceConfiguration,
    compositor: ViconGpuCompositor,
    presenter: ViconSurfacePresenter,
}

struct Msx68kApp {
    args: AppArgs,
    vm: X68kVm,
    gpu: Option<GpuWindowState>,
    last_title_update: Instant,
    next_frame_deadline: Instant,
    rendered_frames: u64,
    last_cursor_pos: Option<(f64, f64)>,
    mouse_left: bool,
    mouse_right: bool,
    last_resolution: (u32, u32),
}

impl Msx68kApp {
    fn new(args: AppArgs) -> Self {
        let mut vm = X68kVm::new(
            VmConfig::app_vm()
                .with_speed_mode(args.speed_mode)
                .with_ram_mb(args.ram_mb),
        );

        let cgrom = std::fs::read(&args.cgrom_path).unwrap_or_else(|e| {
            eprintln!(
                "Failed to read CGROM at '{}': {e}\nUse --cgrom <path> to specify CGROM.",
                args.cgrom_path.display()
            );
            std::process::exit(1);
        });
        let ipl = std::fs::read(&args.ipl_path).unwrap_or_else(|e| {
            eprintln!(
                "Failed to read IPLROM at '{}': {e}\nUse --ipl <path> to specify IPLROM.",
                args.ipl_path.display()
            );
            std::process::exit(1);
        });

        vm.bus.load_cgrom(&cgrom);
        vm.bus.load_ipl_rom(&ipl);

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
        }

        vm.reset().expect("Failed to reset X68000 VM");
        if let Some((w, h)) = args.resolution {
            vm.bus.apply_screen_resolution(w, h);
            println!("Configured initial screen resolution: {}x{}", w, h);
        }
        let initial_res = vm.bus.crtc.active_resolution();
        println!(
            "Initialized X68000 VM ({:?}, {} MB RAM, initial PC=${:06X})",
            vm.speed_mode(),
            args.ram_mb,
            vm.cpu_state.pc
        );

        let now = Instant::now();
        Self {
            args,
            vm,
            gpu: None,
            last_title_update: now,
            next_frame_deadline: now,
            rendered_frames: 0,
            last_cursor_pos: None,
            mouse_left: false,
            mouse_right: false,
            last_resolution: initial_res,
        }
    }

    fn step_vm_one_frame(&mut self) {
        match self.vm.speed_mode() {
            CpuSpeedMode::Burst => {
                // In Burst mode, pace peripheral clock to exact 1/60s frame time (166,667 10MHz ticks)
                // across up to 250,000 instructions so guest timers (Timer C, V-Blank, double-click)
                // match real wall-clock time.
                const TARGET_TICKS_60HZ: u32 = 10_000_000 / 60;
                self.vm.run_burst_frame(250_000, TARGET_TICKS_60HZ);
            }
            mode => {
                // If FDD motor is spinning during floppy access, give a higher instruction budget
                // so floppy boot and program loads complete smoothly.
                let fdd_busy = self.vm.bus.fdc.drives.iter().any(|d| d.motor_on);
                let cpu_mhz = mode.timing_config().cpu_mhz.max(10);
                let frame_cycles = if fdd_busy {
                    (cpu_mhz * 1_000_000) / 15
                } else {
                    (cpu_mhz * 1_000_000) / 60
                };
                self.vm.run_cycles(frame_cycles);
            }
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
            .with_inner_size(LogicalSize::new(win_w, win_h));

        let window = Arc::new(
            event_loop
                .create_window(attrs)
                .expect("Failed to create window"),
        );

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

        let surface_config = wgpu::SurfaceConfiguration {
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
            format,
            width: size.width.max(1),
            height: size.height.max(1),
            present_mode: wgpu::PresentMode::Fifo,
            alpha_mode: caps.alpha_modes[0],
            view_formats: vec![],
            desired_maximum_frame_latency: 2,
        };
        surface.configure(&device, &surface_config);

        let compositor = ViconGpuCompositor::from_device(device, queue, adapter_info);
        let presenter = compositor.create_surface_presenter(format);

        self.gpu = Some(GpuWindowState {
            window,
            surface,
            surface_config,
            compositor,
            presenter,
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
                let (scale_x, scale_y) = if let Some(gpu) = &self.gpu {
                    let (res_w, res_h) = self.vm.bus.crtc.active_resolution();
                    (
                        (gpu.surface_config.width as f64 / res_w.max(1) as f64).max(0.25),
                        (gpu.surface_config.height as f64 / res_h.max(1) as f64).max(0.25),
                    )
                } else {
                    (1.0, 1.0)
                };
                let x = position.x / scale_x;
                let y = position.y / scale_y;
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
                                    _ => (768, 512),
                                };
                                self.vm.bus.apply_screen_resolution(next_w, next_h);
                                println!("F5: Switched resolution to {}x{}", next_w, next_h);
                                return;
                            }
                            KeyCode::F12 => {
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
            WindowEvent::RedrawRequested => {
                self.step_vm_one_frame();

                let active_res = self.vm.bus.crtc.active_resolution();
                if active_res != self.last_resolution {
                    self.last_resolution = active_res;
                    if let Some(gpu) = &self.gpu {
                        let new_w = active_res.0 * self.args.scale;
                        let new_h = active_res.1 * self.args.scale;
                        let _ = gpu.window.request_inner_size(LogicalSize::new(new_w, new_h));
                        println!(
                            "Auto-resized window to {}x{} (Guest resolution {}x{})",
                            new_w, new_h, active_res.0, active_res.1
                        );
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
                        gpu.window.set_title(&format!(
                            "msx68k — X68000 [{speed_label} | {res_w}x{res_h} | {fdd_led}{scsi_led} | PC=${:06X}] (F1:10M F2:16M F3:24M F4:Burst F5:Res F12:Reset)",
                            self.vm.cpu_state.pc
                        ));
                    }
                }
            }
            _ => {}
        }
    }

    fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
        let now = Instant::now();
        let frame_interval = Duration::from_micros(16_667); // ~60 Hz
        if now >= self.next_frame_deadline {
            self.next_frame_deadline = now + frame_interval;
            if let Some(gpu) = &self.gpu {
                gpu.window.request_redraw();
            }
        }
        event_loop.set_control_flow(ControlFlow::WaitUntil(self.next_frame_deadline));
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
