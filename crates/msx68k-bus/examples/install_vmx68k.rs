//! Installs or updates VMX68K.X, built from guest/vmx68k/vmx68k.s, in \X68 of an existing
//! SX-Window HDD image in place, and gives it a drive 0 image at \VMX68K\DRIVE0.XDF, a copy of
//! disks/HUMAN302.XDF, unless one is there already. Everything else on it stays as it is.
//!
//! cargo run -p msx68k-bus --example install_vmx68k -- <image.HDS>

use std::path::Path;

use msx68k_bus::guest_build::build_guest_x_executable;
use msx68k_bus::human_fs::{short_name, HumanVolume};

fn main() {
    let Some(path) = std::env::args().nth(1) else {
        eprintln!("usage: install_vmx68k <image.HDS>");
        std::process::exit(2);
    };
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let program = build_guest_x_executable(
        &root.join("guest/vmx68k/vmx68k.s"),
        &[&root.join("guest/include")],
    )
    .expect("VMX68K.X must build");
    let mut image = std::fs::read(&path).expect("the image must be readable");
    let mut volume = HumanVolume::open(&mut image).expect("the image must hold a Human68k partition");
    let fail = |e: String| -> ! {
        eprintln!("{path}: {e}");
        std::process::exit(1);
    };
    let (x68, name) = ([short_name("X68", "")], short_name("VMX68K", "X"));
    if volume.read_file(&x68, &name).is_some() {
        volume.replace_file(&x68, &name, &program, 0x6000, 0x5821).unwrap_or_else(|e| fail(e));
        println!("Updated \\X68\\VMX68K.X ({} bytes)", program.len());
    } else {
        volume.add_file(&x68, &name, &program, 0x6000, 0x5821).unwrap_or_else(|e| fail(e));
        println!("Added \\X68\\VMX68K.X ({} bytes)", program.len());
    }
    assert_eq!(volume.read_file(&x68, &name).as_deref(), Some(&program[..]));

    let (dir, drive0) = (short_name("VMX68K", ""), short_name("DRIVE0", "XDF"));
    volume.make_dir(&[], &dir, 0x6000, 0x5821).unwrap_or_else(|e| fail(e));
    if volume.read_file(&[dir], &drive0).is_none() {
        let human = std::fs::read(root.join("disks/HUMAN302.XDF")).expect("disks/HUMAN302.XDF");
        volume.add_file(&[dir], &drive0, &human, 0x6000, 0x5821).unwrap_or_else(|e| fail(e));
        assert_eq!(volume.read_file(&[dir], &drive0).as_deref(), Some(&human[..]));
        println!("Added \\VMX68K\\DRIVE0.XDF ({} bytes)", human.len());
    } else {
        println!("Kept the existing \\VMX68K\\DRIVE0.XDF");
    }
    std::fs::write(&path, &image).expect("the image must be writable");
}
