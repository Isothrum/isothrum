//! Build script: Tauri's codegen, told the app's own commands so the capabilities files can grant
//! them (to the editor's origin only). A failure is returned so Cargo reports it and fails the
//! build, rather than panicking or printing.

fn main() -> Result<(), Box<dyn std::error::Error>> {
    // The app's own commands, so capabilities can grant them (to the editor's origin only).
    let commands = &[
        "desktop_info",
        "lab_up",
        "lab_down",
        "lab_status",
        "lab_live",
        "capture_start",
        "capture_stop",
        "machine_console",
        "machine_display",
        "machine_action",
        "link_list",
        "link_action",
        "snapshot_list",
        "snapshot_take",
        "snapshot_use",
        "snapshot_delete",
        "update_check",
        "update_install",
        "system_check",
        "machine_providers",
        "machine_selftest",
        "machine_selftest_prefetch",
    ];
    tauri_build::try_build(
        tauri_build::Attributes::new()
            .app_manifest(tauri_build::AppManifest::new().commands(commands)),
    )?;
    Ok(())
}
