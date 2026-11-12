//! Adds VMX68K.X, built from guest/vmx68k/vmx68k.s, to \X68 of an existing SX-Window HDD
//! image in place, leaving everything else on it as it is.
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
    let (x68, name) = ([short_name("X68", "")], short_name("VMX68K", "X"));
    if let Err(e) = volume.add_file(&x68, &name, &program, 0x6000, 0x5821) {
        eprintln!("{path}: {e}");
        std::process::exit(1);
    }
    assert_eq!(volume.read_file(&x68, &name).as_deref(), Some(&program[..]));
    std::fs::write(&path, &image).expect("the image must be writable");
    println!("Added \\X68\\VMX68K.X ({} bytes) to {path}", program.len());
}
