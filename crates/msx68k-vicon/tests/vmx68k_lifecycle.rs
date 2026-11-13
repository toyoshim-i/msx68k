//! Sequential scenarios share one primary, including all prior guest side effects.
#[path = "support/vmx68k_disk.rs"]
mod disk;
#[path = "support/vmx68k_session.rs"]
mod session;
#[path = "support/vmx68k_shared.rs"]
mod shared;
#[path = "support/vmx68k_ui.rs"]
mod ui;

#[test]
fn vmx68k_lifecycle() {
    let cases: &[(&str, fn())] = &[
        (
            "screen-growth",
            ui::a_window_the_screen_cut_off_is_drawn_when_the_screen_grows,
        ),
        ("window-menus", ui::vmx68k_opens_a_vm_in_its_window),
        (
            "setup-association",
            ui::a_setup_opens_vmx68k_with_its_settings,
        ),
        ("two-windows", ui::vmx68k_runs_a_vm_in_each_of_its_windows),
        (
            "configuration",
            ui::the_configuration_dialog_sets_the_cpu_and_memory_and_restarts_the_vm,
        ),
        (
            "new-setup",
            ui::new_setup_keeps_the_running_vm_and_opens_its_own_configuration,
        ),
        ("settings-drop", ui::settings_allow_filer_drops_and_restore_the_parent),
        (
            "raw-drop",
            ui::custom_fd_icons_accept_filer_drops_and_eject_without_reset,
        ),
        (
            "dim-drop",
            ui::dim_drops_write_omitted_tracks_and_keep_the_image_format,
        ),
        (
            "legacy-image",
            disk::a_vm_boots_from_an_image_on_the_primary_vms_drive,
        ),
        (
            "raw-image",
            disk::a_vm_boots_from_a_custom_image_on_the_primary_vms_drive,
        ),
        (
            "hdd-image",
            disk::a_vm_boots_from_a_custom_hdd_on_the_primary_vms_drive,
        ),
        (
            "dim-image",
            disk::a_vm_boots_from_a_dim_image_on_the_primary_vms_drive,
        ),
        ("shared", shared::a_vm_boots_from_the_primary_vms_drive_a),
        (
            "shared-fd",
            shared::custom_shared_drives_follow_the_floppy_drives,
        ),
        (
            "shared-hdd",
            shared::custom_shared_drives_follow_the_hdd_and_floppy_drives,
        ),
        (
            "shared-nonboot-hdd",
            shared::custom_shared_drives_follow_the_floppy_and_nonboot_hdd,
        ),
        (
            "passthrough",
            shared::passthrough_floppy_boots_and_writes_the_primarys_disk,
        ),
    ];
    let selection = std::env::var("MSX68K_CASES").unwrap_or_default();
    if selection == "list" {
        for (i, (name, _)) in cases.iter().enumerate() {
            eprintln!("{:02} {name}", i + 1);
        }
        return;
    }
    let index = |value: &str| -> usize {
        if let Ok(i) = value.parse::<usize>() {
            assert!((1..=cases.len()).contains(&i));
            return i - 1;
        }
        cases
            .iter()
            .position(|(name, _)| *name == value)
            .unwrap_or_else(|| panic!("unknown case {value}; use MSX68K_CASES=list"))
    };
    let mut selected = vec![selection.is_empty(); cases.len()];
    if !selection.is_empty() {
        for term in selection.split(',') {
            if let Some((start, end)) = term.split_once("..") {
                let (start, end) = (index(start), index(end));
                assert!(start <= end);
                selected[start..=end].fill(true);
            } else {
                selected[index(term)] = true;
            }
        }
    }
    if !session::prepare() {
        return;
    }
    for (i, (name, run)) in cases.iter().enumerate().filter(|(i, _)| selected[*i]) {
        session::set_case(i, name);
        let started = std::time::Instant::now();
        eprintln!("[vmx68k] START {:02} {name}", i + 1);
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(run));
        eprintln!(
            "[vmx68k] {} {:02} {name} {:.3}s",
            if result.is_ok() { "PASS" } else { "FAIL" },
            i + 1,
            started.elapsed().as_secs_f64()
        );
        if let Err(panic) = result {
            std::panic::resume_unwind(panic);
        }
    }
}
