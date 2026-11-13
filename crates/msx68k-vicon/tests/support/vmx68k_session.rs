//! One live primary for named VMX68K scenarios. Fixtures use actual DOS calls;
//! neither RAM nor disk snapshots are restored between scenarios.
use msx68k_bus::scsi::{
    build_bootable_sxwin_scsi_hdd, build_bootable_sxwin_scsi_hdd_with_files, ExtractedFile,
};
use msx68k_bus::vm_thread::VmThread;
use msx68k_bus::vmx68k::{vm_bridge, BridgeOp, BridgeReply, VmBridge};
use msx68k_bus::{CpuSpeedMode, VmConfig, VmEvent, X68kVm};
use std::{
    cell::RefCell,
    collections::HashSet,
    ops::{Deref, DerefMut},
    path::Path,
    sync::{Arc, Weak},
    time::{Duration, Instant},
};

thread_local! {
    static SESSION: RefCell<Option<Session>> = const { RefCell::new(None) };
    static FIRST: RefCell<u32> = const { RefCell::new(0) };
    static CASE: RefCell<(usize, String)> = RefCell::new((0, String::new()));
}
struct Session {
    primary: X68kVm,
    controller: u32,
    bridge: VmBridge,
    persistent: HashSet<Vec<u8>>,
}
pub struct Primary {
    session: Option<Session>,
    extra: Vec<Vec<u8>>,
    free_handles: u32,
    threads: Vec<(u32, Arc<VmThread>)>,
    retired: Vec<Weak<VmThread>>,
    pub case_letter: char,
}
impl Deref for Primary {
    type Target = X68kVm;
    fn deref(&self) -> &X68kVm {
        &self.session.as_ref().unwrap().primary
    }
}
impl DerefMut for Primary {
    fn deref_mut(&mut self) -> &mut X68kVm {
        &mut self.session.as_mut().unwrap().primary
    }
}
pub fn case_id(relative: u32) -> u32 {
    FIRST.with(|id| *id.borrow() + relative - 1)
}
pub fn set_case(index: usize, name: &str) {
    CASE.with(|c| *c.borrow_mut() = (index, name.to_string()));
}
pub fn prepare() -> bool {
    let started = Instant::now();
    let Some(session) = Session::start() else {
        eprintln!("[vmx68k] SKIP: ROM/SX disks unavailable");
        return false;
    };
    eprintln!(
        "[vmx68k] PRIMARY boot {:.3}s",
        started.elapsed().as_secs_f64()
    );
    SESSION.with(|slot| *slot.borrow_mut() = Some(session));
    true
}
pub fn boot() -> Option<Primary> {
    boot_with(vec![])
}
pub fn boot_with(files: Vec<ExtractedFile>) -> Option<Primary> {
    let session = SESSION
        .with(|s| s.borrow_mut().take())
        .expect("run this scenario through vmx68k_lifecycle");
    let first_id = session.primary.bus.next_vm_id();
    FIRST.with(|id| *id.borrow_mut() = first_id);
    let mut primary = Primary {
        session: Some(session),
        extra: vec![],
        free_handles: 0,
        threads: vec![],
        retired: vec![],
        case_letter: CASE.with(|c| (b'a' + c.borrow().0 as u8) as char),
    };
    primary.assert_resources();
    if CASE.with(|c| c.borrow().1 != "screen-growth")
        && primary.bus.crtc.active_resolution() != (1024, 768)
    {
        primary.bus.request_screen_resolution(1024, 768);
        primary.run_steps(5_000_000);
    }
    // These are scenario inputs written through Human68k, not a restored VM image.
    let mut defaults = msx68k_bus::sx_resource::ResourceLibrary::parse(
        &msx68k_bus::vmx68k_library::setup_library(0, 0, 4, None),
    )
    .unwrap();
    defaults.insert(
        b"V68D",
        msx68k_bus::sx_resource::Resource::new(
            0,
            msx68k_bus::vmx68k_library::setup_drives(false, &[(0, b"")]),
        ),
    );
    primary.put(b"A:\\VMX68K\\DEFAULT.V68", defaults.to_bytes());
    for file in files {
        let path = file_path(&file);
        if primary.session.as_ref().unwrap().persistent.contains(&path) {
            continue;
        }
        primary.put(&path, file.data);
        primary.extra.push(path);
    }
    primary.free_handles = primary
        .rpc(BridgeOp::Open {
            mode: 0,
            path: b"@fds".to_vec(),
        })
        .result;
    Some(primary)
}
pub fn click(vm: &mut X68kVm, at: (i16, i16), times: usize, settle: usize) {
    vm.bus.set_host_mouse_position(Some(at));
    vm.bus.scc.add_mouse_delta(1, 1);
    vm.run_steps(100_000);
    for _ in 0..times {
        for down in [true, false] {
            vm.bus.scc.set_mouse_buttons(down, false);
            vm.bus.scc.add_mouse_delta(1, 1);
            vm.run_steps(100_000);
        }
    }
    vm.run_steps(settle);
}
fn file_path(file: &ExtractedFile) -> Vec<u8> {
    let mut path = b"A:".to_vec();
    for (name, ext, _) in file.path_dirs.iter().chain(std::iter::once(&(
        file.raw_name,
        file.raw_ext,
        file.raw_name2,
    ))) {
        path.push(b'\\');
        path.extend(name.iter().copied().take_while(|&c| c != b' '));
        if ext[0] != b' ' {
            path.push(b'.');
            path.extend(ext.iter().copied().take_while(|&c| c != b' '));
        }
    }
    path
}
impl Primary {
    pub fn rpc(&mut self, op: BridgeOp) -> BridgeReply {
        let bridge = self.session.as_ref().unwrap().bridge.clone();
        let worker = std::thread::spawn(move || bridge.ask(op));
        let until = Instant::now() + Duration::from_secs(10);
        while !worker.is_finished() && Instant::now() < until {
            self.run_steps(50_000);
        }
        if !worker.is_finished() {
            let id = self.session.as_ref().unwrap().controller;
            self.bus.detach_vm_bridge(id);
            panic!("controller request timed out");
        }
        worker.join().unwrap().expect("controller disconnected")
    }
    fn put(&mut self, path: &[u8], data: Vec<u8>) {
        let fd = self
            .rpc(BridgeOp::Create {
                path: path.to_vec(),
                attr: 0x20,
                replace: true,
            })
            .result;
        assert!(
            fd as i32 >= 0,
            "create {}: {fd:x}",
            String::from_utf8_lossy(path)
        );
        for (i, bytes) in data.chunks(32768).enumerate() {
            assert_eq!(
                self.rpc(BridgeOp::HandleWrite {
                    handle: fd,
                    offset: (i * 32768) as u32,
                    data: bytes.to_vec()
                })
                .result,
                bytes.len() as u32
            );
        }
        assert_eq!(self.rpc(BridgeOp::Close { handle: fd }).result, 0);
        let found = self.rpc(BridgeOp::Files {
            attr: 0x27,
            path: path.to_vec(),
        });
        assert_eq!(
            found.result,
            0,
            "fixture missing: {}",
            String::from_utf8_lossy(path)
        );
    }
    pub fn launch(&mut self) {
        let mut command = b"@runA:\\X68\\VMX68K.X\0".to_vec();
        command.extend([0]);
        let result = self
            .rpc(BridgeOp::Open {
                mode: 0,
                path: command,
            })
            .result;
        assert!(result as i32 >= 0, "TSFockB: {result:x}");
        self.run_steps(1_000_000);
    }
    pub fn background_guest(&mut self) {
        assert_eq!(
            self.rpc(BridgeOp::Open {
                mode: 0,
                path: b"@shw".to_vec()
            })
            .result,
            0
        );
        self.run_steps(200_000);
    }
    pub fn thread(&mut self, id: u32, thread: VmThread) -> Arc<VmThread> {
        let thread = Arc::new(thread);
        self.threads.push((id, Arc::clone(&thread)));
        thread
    }
    pub fn close_guest(&mut self, id: u32) {
        assert_eq!(
            self.rpc(BridgeOp::Open {
                mode: 0,
                path: b"@hid".to_vec()
            })
            .result,
            0
        );
        let window = self
            .bus
            .vm_resources()
            .0
            .into_iter()
            .find(|v| v.0 == id)
            .unwrap()
            .1;
        let layout = self
            .bus
            .vm_window_layout(window)
            .expect("visible VM window");
        // Bring the title and close box back onto the screen, using normal tracking.
        let start = (layout.left as i16 + 80, layout.top as i16 - 35);
        click(self, start, 1, 200_000);
        self.bus.set_host_mouse_position(Some(start));
        self.bus.scc.set_mouse_buttons(true, false);
        self.run_steps(200_000);
        let target = (120, 29);
        for i in 1..=10 {
            self.bus.set_host_mouse_position(Some((
                start.0 + (target.0 - start.0) * i / 10,
                start.1 + (target.1 - start.1) * i / 10,
            )));
            self.run_steps(60_000);
        }
        self.bus.scc.set_mouse_buttons(false, false);
        self.run_steps(300_000);
        click(self, (797, 29), 1, 500_000);
        if self.bus.vm_resources().0.iter().any(|v| v.0 == id) {
            click(self, (797, 29), 1, 500_000);
        }
        assert!(
            !self.bus.vm_resources().0.iter().any(|v| v.0 == id),
            "VM {id} did not close normally"
        );
        assert!(
            self.bus
                .take_vm_events()
                .iter()
                .any(|event| matches!(event,VmEvent::Closed{id:closed} if *closed==id)),
            "no Closed event for VM {id}"
        );
        for (_, thread) in self.threads.iter().filter(|(open, _)| *open == id) {
            self.retired.push(Arc::downgrade(thread));
        }
        self.threads.retain(|(open, _)| *open != id);
    }
    fn assert_resources(&self) {
        let (windows, bridges, leases, pending) = self.bus.vm_resources();
        assert_eq!(windows.len(), 1, "guest window leaked: {windows:?}");
        assert_eq!(windows[0].0, self.session.as_ref().unwrap().controller);
        assert_eq!(
            (bridges, leases, pending),
            (1, 0, 0),
            "storage lifecycle leak"
        );
        for drive in &self.bus.fdc.drives {
            assert_eq!(
                drive.is_write_protected(),
                drive.write_protected,
                "passthrough protection leaked"
            );
        }
    }
}
impl Drop for Primary {
    fn drop(&mut self) {
        if std::thread::panicking() {
            let ids: Vec<_> = self.bus.vm_resources().0.into_iter().map(|v| v.0).collect();
            for id in ids {
                self.bus.detach_vm_bridge(id);
            }
            self.threads.clear();
            return;
        }
        let controller = self.session.as_ref().unwrap().controller;
        let ids: Vec<_> = self
            .bus
            .vm_resources()
            .0
            .into_iter()
            .filter(|v| v.0 != controller)
            .map(|v| v.0)
            .collect();
        for id in ids.into_iter().rev() {
            self.close_guest(id);
        }
        assert!(self.threads.is_empty(), "VM thread owner leaked");
        assert!(
            self.retired.iter().all(|thread| thread.strong_count() == 0),
            "VM thread retained after normal exit"
        );
        self.assert_resources();
        // Close the Filer too, without restoring the primary's UI or memory.
        click(self, (421, 37), 2, 300_000);
        let free = self
            .rpc(BridgeOp::Open {
                mode: 0,
                path: b"@fds".to_vec(),
            })
            .result;
        assert_eq!(free, self.free_handles, "DOS file handle leak");
        let extra = std::mem::take(&mut self.extra);
        for path in extra {
            let result = self.rpc(BridgeOp::Delete { path }).result;
            assert_eq!(result, 0, "fixture close/delete failed");
        }
        SESSION.with(|slot| *slot.borrow_mut() = self.session.take());
    }
}
impl Session {
    fn start() -> Option<Self> {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        let read = |name: &str| std::fs::read(root.join(name)).ok();
        let (ipl, cgrom, human) = (
            read("roms/IPLROM.DAT")?,
            read("roms/CGROM.DAT")?,
            read("disks/HUMAN302.XDF")?,
        );
        let sx: Vec<_> = (1..=6)
            .filter_map(|i| {
                read(&format!("disks/SXWIN31{i}.XDF")).map(|b| (format!("SXWIN31{i}"), b))
            })
            .collect();
        if sx.is_empty() {
            return None;
        }
        let refs: Vec<_> = sx.iter().map(|(n, b)| (n.as_str(), b.as_slice())).collect();
        let mut dim = vec![1; 256];
        dim[0] = 0;
        dim.extend(&human);
        let image = |name, ext, data| ExtractedFile {
            path_dirs: vec![(*b"VMX68K  ", *b"   ", [0; 10])],
            raw_name: name,
            raw_ext: ext,
            raw_name2: [0; 10],
            attr: 0x20,
            time: 0x6000,
            date: 0x5821,
            data,
        };
        let mut files = vec![
            image(*b"DRIVE0  ", *b"XDF", human.clone()),
            image(*b"RAW     ", *b"XDF", human.clone()),
            image(*b"DIM     ", *b"DIM", dim),
            image(
                *b"HDD     ",
                *b"HDS",
                build_bootable_sxwin_scsi_hdd(&human, &[], 8),
            ),
        ];
        let ui_file = |name, ext, data| ExtractedFile {
            path_dirs: vec![(*b"X68     ", *b"   ", [0; 10])],
            raw_name: name,
            raw_ext: ext,
            raw_name2: [0; 10],
            attr: 0x20,
            time: 0x6000,
            date: 0x5821,
            data,
        };
        let sram: Vec<u8> = (0..msx68k_bus::vmx68k::VM_SRAM_BYTES)
            .map(|i| (i * 7) as u8)
            .collect();
        files.push(ui_file(
            *b"AA      ",
            *b"V68",
            msx68k_bus::vmx68k_library::setup_library(3, 0, 4, Some(&sram)),
        ));
        files.push(ui_file(*b"ZZZ     ", *b"XDF", vec![0x5A; 737_280]));
        let mut sparse = vec![0; 256];
        sparse[1] = 1;
        sparse[3] = 1;
        sparse.extend(vec![0x5A; 8192]);
        sparse.extend(vec![0x6B; 8192]);
        files.push(ui_file(*b"ZZZ     ", *b"DIM", sparse));
        let persistent = files.iter().map(file_path).collect();
        files.push(ExtractedFile {
            path_dirs: vec![(*b"X68     ", *b"   ", [0; 10])],
            raw_name: *b"ABCTL   ",
            raw_ext: *b"X  ",
            raw_name2: [0; 10],
            attr: 0x20,
            time: 0x6000,
            date: 0x5821,
            data: controller(&root),
        });
        let mut primary = X68kVm::new(
            VmConfig::primary_sx()
                .with_speed_mode(CpuSpeedMode::Burst)
                .with_ram_mb(4),
        );
        primary.bus.load_ipl_rom(&ipl);
        primary.bus.load_cgrom(&cgrom);
        assert!(primary.bus.insert_scsi_hdd(
            0,
            build_bootable_sxwin_scsi_hdd_with_files(&human, &refs, 32, files)
        ));
        primary.bus.set_boot_from_scsi(true);
        primary.reset().unwrap();
        primary.run_steps(30_000_000);
        click(&mut primary, (22, 340), 2, 2_000_000);
        click(&mut primary, (184, 92), 2, 3_000_000);
        let events = primary.bus.take_vm_events();
        let [VmEvent::Opened { id, .. }] = events.as_slice() else {
            panic!("controller failed: {events:?}")
        };
        let controller = *id;
        let (bridge, requests) = vm_bridge();
        primary.bus.attach_vm_bridge(controller, requests);
        click(&mut primary, (421, 37), 2, 300_000);
        Some(Self {
            primary,
            controller,
            bridge,
            persistent,
        })
    }
}
fn controller(root: &Path) -> Vec<u8> {
    // Test-only entry and control messages; DOS servicing remains the real VMX task.
    let mut source = std::fs::read_to_string(root.join("guest/vmx68k/vmx68k.s")).unwrap();
    source = source.replace("gui_start - objr_header", "test_start - objr_header");
    source = source.replace(
        "gui_start:\n",
        &(include_str!("vmx68k_control.s").to_string() + "\ngui_start:\n"),
    );
    // Fixture changes go through Task Man so the Filer receives create/close/delete notices.
    source = source.replace("DOS\t_CREATE", "SXCALL\t$A36D");
    let close_start = source.find("serve_close:\n").unwrap();
    let close_end = source[close_start..].find("serve_files:\n").unwrap() + close_start;
    let close = source[close_start..close_end].replace("DOS\t_CLOSE", "SXCALL\t$A368");
    source.replace_range(close_start..close_end, &close);
    let delete_start = source.find("serve_delete:\n").unwrap();
    let delete_end = source[delete_start..].find("serve_rename:\n").unwrap() + delete_start;
    let delete = source[delete_start..delete_end].replace("DOS\t_DELETE", "SXCALL\t$A373");
    source.replace_range(delete_start..delete_end, &delete);
    source=source.replace("serve_open:\n",&("serve_open:\n\tlea disk_buffer(a5),a0\n\tcmpi.l #'@run',(a0)\n\tbeq test_run\n\tcmpi.l #'@fds',(a0)\n\tbeq test_fds\n\tcmpi.l #'@shw',(a0)\n\tbeq test_show\n\tcmpi.l #'@hid',(a0)\n\tbeq test_hide\n"));
    let path = std::env::temp_dir().join(format!("msx68k-controller-{}.s", std::process::id()));
    std::fs::write(&path, source).unwrap();
    let result =
        msx68k_bus::guest_build::build_guest_x_executable(&path, &[&root.join("guest/include")])
            .unwrap();
    let _ = std::fs::remove_file(path);
    result
}
