//! Adding a file to the Human68k partition of an HDD image in place changes only the FAT, the
//! directory, and the clusters the file takes.

use std::path::Path;

use msx68k_bus::human_fs::{short_name, HumanVolume};
use msx68k_bus::scsi::build_bootable_sxwin_scsi_hdd;

fn image() -> Option<Vec<u8>> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let human = std::fs::read(root.join("disks/HUMAN302.XDF")).ok()?;
    Some(build_bootable_sxwin_scsi_hdd(&human, &[], 32))
}

#[test]
fn an_added_file_reads_back_and_nothing_else_changes() {
    let Some(mut image) = image() else {
        eprintln!("HUMAN302.XDF not found, skipping");
        return;
    };
    let before = image.clone();
    let bin = [short_name("BIN", "")];
    let data: Vec<u8> = (0..5000u32).map(|i| (i * 7) as u8).collect();

    let mut volume = HumanVolume::open(&mut image).unwrap();
    let free = volume.free_clusters();
    let command = volume.read_file(&[], &short_name("COMMAND", "X")).unwrap();
    volume.add_file(&bin, &short_name("TEST", "X"), &data, 0x6000, 0x5821).unwrap();
    assert_eq!(volume.read_file(&bin, &short_name("TEST", "X")).unwrap(), data);
    assert_eq!(volume.free_clusters(), free - 5, "5000 bytes take five 1 KB clusters");
    assert_eq!(volume.read_file(&[], &short_name("COMMAND", "X")).unwrap(), command);
    assert!(volume
        .add_file(&bin, &short_name("TEST", "X"), b"again", 0, 0)
        .unwrap_err()
        .contains("already exists"));
    assert!(volume.add_file(&[short_name("NONE", "")], &short_name("A", ""), b"", 0, 0).is_err());

    // Changed bytes: the two FATs, one directory entry, and the file's five clusters.
    let changed: Vec<usize> = (0..image.len()).filter(|&i| image[i] != before[i]).collect();
    let partition = 32 * 1024;
    let fat_end = partition + 1024 + 2 * 64 * 1024;
    let outside_fat: Vec<usize> = changed.iter().copied().filter(|&i| i >= fat_end).collect();
    let span = outside_fat.last().unwrap() - outside_fat.first().unwrap();
    assert!(changed.iter().all(|&i| i >= partition + 1024), "the boot sector stays");
    assert!(outside_fat.len() <= 32 + 5000, "{} bytes changed", outside_fat.len());
    assert!(span < 8 * 1024 * 1024, "the changes stay together: {span}");
}

#[test]
fn a_full_directory_grows_by_a_cluster() {
    let Some(mut image) = image() else {
        eprintln!("HUMAN302.XDF not found, skipping");
        return;
    };
    let mut volume = HumanVolume::open(&mut image).unwrap();
    let bin = [short_name("BIN", "")];
    // A 1 KB cluster holds 32 entries, so this many files make \BIN grow at least once.
    let names: Vec<String> = (0..40).map(|i| format!("F{i:02}")).collect();
    for (i, name) in names.iter().enumerate() {
        volume
            .add_file(&bin, &short_name(name, "DAT"), &[i as u8; 100], 0, 0)
            .unwrap_or_else(|e| panic!("{name}: {e}"));
    }
    for (i, name) in names.iter().enumerate() {
        assert_eq!(volume.read_file(&bin, &short_name(name, "DAT")).unwrap(), [i as u8; 100]);
    }
}

#[test]
fn directories_are_made_and_files_replaced() {
    let Some(mut image) = image() else {
        eprintln!("HUMAN302.XDF not found, skipping");
        return;
    };
    let mut volume = HumanVolume::open(&mut image).unwrap();
    let free = volume.free_clusters();
    let dir = short_name("VMX68K", "");
    volume.make_dir(&[], &dir, 0, 0).unwrap();
    volume.make_dir(&[], &dir, 0, 0).unwrap();
    assert_eq!(volume.free_clusters(), free - 1, "made once");
    let name = short_name("DRIVE0", "XDF");
    volume.add_file(&[dir], &name, &[1; 3000], 0, 0).unwrap();
    assert_eq!(volume.free_clusters(), free - 4);

    volume.replace_file(&[dir], &name, &[2; 1500], 0, 0).unwrap();
    assert_eq!(volume.read_file(&[dir], &name).unwrap(), [2; 1500]);
    assert_eq!(volume.free_clusters(), free - 3, "the old clusters are free again");
    assert!(volume.replace_file(&[dir], &short_name("NONE", ""), b"", 0, 0).is_err());
    assert!(volume.make_dir(&[dir], &name, 0, 0).is_err(), "a file is in the way");
}
