//! The final pass draws a VM's completed screen into its window's content rectangle on the
//! primary VM's screen, clipped to the window's visible region (`docs/VMX68K.md` Section 3.3),
//! and under the primary's text plane, so that what SX-Window draws there, such as menus, stays
//! in front.

use msx68k_bus::{Bus, X68kVm};
use msx68k_vicon::{
    read_back_texture, render_bus_frame_cpu, RenderedFrame, ScreenRect, ViconGpuCompositor, VmLayer, VmScreen,
};

const FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8Unorm;

fn rect(left: i32, top: i32, right: i32, bottom: i32) -> ScreenRect {
    ScreenRect {
        left,
        top,
        right,
        bottom,
    }
}

fn pixel(frame: &RenderedFrame, x: u32, y: u32) -> [u8; 4] {
    let i = ((y * frame.width + x) * 4) as usize;
    frame.rgba8[i..i + 4].try_into().unwrap()
}

#[test]
fn a_vm_layer_shows_only_in_its_visible_region() {
    let Some(mut gpu) = ViconGpuCompositor::new() else {
        eprintln!("No wgpu adapter, skipping");
        return;
    };

    // The primary VM's screen is its red backdrop, with green text dots, as a menu would be,
    // over part of the VM's window.
    let primary = X68kVm::new_app_vm();
    let mut primary_bus = primary.bus;
    primary_bus.write_u16(0x00E8_2200, 0x07C0, true).unwrap();
    primary_bus.write_u16(0x00E8_2202, 0xF800, true).unwrap();
    primary_bus.write_u16(0x00E8_2600, 0x0020, true).unwrap();
    let text_dot = |x: i32, y: i32| (120..200).contains(&x) && (60..90).contains(&y) && x % 2 == 0;
    for y in 60..90u32 {
        for byte in 15..25u32 {
            primary_bus.write_u8(0x00E0_0000 + y * 128 + byte, 0b1010_1010, true).unwrap();
        }
    }

    // The VM shows white text dots on a blue backdrop.
    let mut vm = X68kVm::new_app_vm();
    vm.bus.write_u16(0x00E8_2200, 0x003E, true).unwrap();
    vm.bus.write_u16(0x00E8_2202, 0xFFFE, true).unwrap();
    vm.bus.write_u16(0x00E8_2600, 0x0020, true).unwrap();
    for row in 0..64u32 {
        vm.bus.write_u8(0x00E0_0000 + row * 128 * 3 + row, 0b1010_0101, true).unwrap();
    }
    let mut screen = VmScreen::new(gpu.device(), gpu.queue());
    screen.present(&mut vm.bus);
    let output = screen.output();
    let vm_frame = output.read_back(gpu.device(), gpu.queue()).unwrap();

    let presenter = gpu.create_surface_presenter(FORMAT);
    let (left, top) = (100, 50);
    // Two parts of the window, and one running off the bottom right of the screen.
    let visible = [rect(100, 50, 300, 200), rect(350, 50, 700, 400), rect(700, 450, 900, 600)];
    let shown = |x: i32, y: i32| {
        visible
            .iter()
            .any(|r| (r.left..r.right).contains(&x) && (r.top..r.bottom).contains(&y))
            && x - left < vm_frame.width as i32
            && y - top < vm_frame.height as i32
    };
    let primary_frame = render_bus_frame_cpu(&primary_bus);
    let red = pixel(&primary_frame, 0, 0);
    assert_eq!(red[1..3], [0, 0]);
    let green = pixel(&primary_frame, 120, 60);
    assert_ne!(green, red);

    for scale in [1u32, 2] {
        let (width, height) = (768 * scale, 512 * scale);
        let target = gpu.device().create_texture(&wgpu::TextureDescriptor {
            label: Some("vm_layer_target"),
            size: wgpu::Extent3d {
                width,
                height,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: FORMAT,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        });
        let view = target.create_view(&Default::default());
        gpu.render_bus_to_surface_view(
            &primary_bus,
            &presenter,
            &view,
            width,
            height,
            &[VmLayer {
                output: &output,
                left,
                top,
                visible: &visible,
            }],
        );
        let drawn = read_back_texture(gpu.device(), gpu.queue(), &target, width, height).unwrap();

        for y in 0..512 {
            for x in 0..768 {
                let expected = if text_dot(x, y) {
                    green
                } else if shown(x, y) {
                    pixel(&vm_frame, (x - left) as u32, (y - top) as u32)
                } else {
                    red
                };
                // Every target pixel of the screen pixel.
                for (dx, dy) in [(0, 0), (scale - 1, scale - 1)] {
                    let got = pixel(&drawn, x as u32 * scale + dx, y as u32 * scale + dy);
                    assert_eq!(got, expected, "({x}, {y}) at scale {scale}");
                }
            }
        }
    }
    assert!(vm_frame.count_lit_pixels() > 0);
}
