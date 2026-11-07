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
use winit::event::{ElementState, WindowEvent};
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop};
use winit::keyboard::{KeyCode, PhysicalKey};
use winit::window::{Window, WindowId};

const DEFAULT_IPL_PATH: &str = "roms/IPLROM.DAT";
const DEFAULT_CGROM_PATH: &str = "roms/CGROM.DAT";
const DEFAULT_FD0_PATH: &str = "disks/HUMAN302.XDF";

#[derive(Debug, Clone)]
struct AppArgs {
    ipl_path: PathBuf,
    cgrom_path: PathBuf,
    fd0_path: Option<PathBuf>,
    fd1_path: Option<PathBuf>,
    speed_mode: CpuSpeedMode,
    ram_mb: usize,
    scale: u32,
}

impl Default for AppArgs {
    fn default() -> Self {
        let ipl_path = std::env::var_os("MSX68K_IPL")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from(DEFAULT_IPL_PATH));
        let cgrom_path = std::env::var_os("MSX68K_CGROM")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from(DEFAULT_CGROM_PATH));
        let fd0_path = std::env::var_os("MSX68K_FD0")
            .map(PathBuf::from)
            .or_else(|| {
                let fd0 = Path::new(DEFAULT_FD0_PATH);
                fd0.exists().then(|| fd0.to_path_buf())
            });
        let fd1_path = std::env::var_os("MSX68K_FD1").map(PathBuf::from);
        Self {
            ipl_path,
            cgrom_path,
            fd0_path,
            fd1_path,
            speed_mode: CpuSpeedMode::Mhz10,
            ram_mb: 2,
            scale: 1,
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
                "--help" | "-h" => {
                    println!(
                        "msx68k — Sharp X68000 / SX-Window Hybrid Execution Environment\n\n\
                         Options:\n  \
                           --ipl <path>         Path to IPLROM.DAT (default: {DEFAULT_IPL_PATH})\n  \
                           --cgrom <path>       Path to CGROM.DAT  (default: {DEFAULT_CGROM_PATH})\n  \
                           --fd0 <path>         Floppy disk image for Drive 0 (.XDF/.2HD)\n  \
                           --fd1 <path>         Floppy disk image for Drive 1 (.XDF/.2HD)\n  \
                           --speed <mode>       CPU speed: 10 | 16 | 24 | burst (default: 10)\n  \
                           --ram-mb <1..12>     Main RAM size in MB (default: 2)\n  \
                           --scale <1..4>       Initial window scale (default: 1 = 768x512)\n\n\
                         Hotkeys:\n  \
                           F1: 10MHz | F2: 16MHz | F3: 24MHz | F4: Burst | F12: Reset"
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

        vm.reset().expect("Failed to reset X68000 VM");
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
        }
    }

    fn step_vm_one_frame(&mut self) {
        match self.vm.speed_mode() {
            CpuSpeedMode::Burst => {
                self.vm.run_steps(250_000);
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

        let win_w = 768 * self.args.scale;
        let win_h = 512 * self.args.scale;
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
            WindowEvent::KeyboardInput { event, .. } => {
                if event.state == ElementState::Pressed {
                    if let PhysicalKey::Code(code) = event.physical_key {
                        match code {
                            KeyCode::F1 => {
                                self.vm.set_speed_mode(CpuSpeedMode::Mhz10);
                                println!("Switched CPU speed to 10MHz (Bus 10MHz)");
                            }
                            KeyCode::F2 => {
                                self.vm.set_speed_mode(CpuSpeedMode::Mhz16);
                                println!("Switched CPU speed to 16MHz (XVI, Bus 16MHz)");
                            }
                            KeyCode::F3 => {
                                self.vm.set_speed_mode(CpuSpeedMode::Mhz24);
                                println!("Switched CPU speed to 24MHz (RedZone, Bus 16MHz)");
                            }
                            KeyCode::F4 => {
                                self.vm.set_speed_mode(CpuSpeedMode::Burst);
                                println!("Switched CPU speed to Burst (0 wait)");
                            }
                            KeyCode::F12 => {
                                let _ = self.vm.reset();
                                println!("Reset X68000 VM");
                            }
                            _ => {}
                        }
                    }
                }
            }
            WindowEvent::RedrawRequested => {
                self.step_vm_one_frame();

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
                        gpu.window.set_title(&format!(
                            "msx68k — X68000 [{speed_label} | {res_w}x{res_h} | {fdd_led} | PC=${:06X}] (F1:10M F2:16M F3:24M F4:Burst F12:Reset)",
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
