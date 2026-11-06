//! The machine-setup commands: what this computer can run (the system check and provider
//! detection) and the self-tests that prove it by booting a throwaway container or VM. They are
//! thin adapters over the `isothrum-machine` crate — the same backend the Cyber CTF launcher uses
//! — so the setup UI (the separate isothrum-ui repo) is a close port of the launcher's.
//!
//! The launcher's machine module resolved the process runner and directories inside itself; here
//! the crate takes them as ports, so each command builds a real [`SystemProcesses`] (Docker
//! context aware) and the app's [`Dirs`] from the Tauri app handle, then calls the domain.

use isothrum_machine::providers::{self, ProviderStatus};
use isothrum_machine::selftest::{self, Event, Kind};
use isothrum_machine::system::{self, SystemReport};
use isothrum_machine::{Dirs, exec};
use isothrum_runner::SystemProcesses;
use tauri::ipc::Channel;
use tauri::{AppHandle, Manager};

// Re-exported so the self-test commands can name a provider in their signatures.
pub(crate) use isothrum_machine::providers::Provider;

/// Real system processes, told the Docker CLI's config so it honours the engine context the user
/// picked (Docker Desktop, `OrbStack`, Colima…).
fn runner(app: &AppHandle) -> SystemProcesses {
    let home = app.path().home_dir().ok();
    SystemProcesses::for_host(|key| std::env::var(key).ok(), home.as_deref())
}

/// The folders the machine domain writes to: the app's data folder (the record of what it has
/// downloaded) and cache folder (a self-test's throwaway lab). Each is left unknown when Tauri
/// can't resolve it, which the domain tolerates.
fn dirs(app: &AppHandle) -> Dirs {
    Dirs::new(
        app.path().app_data_dir().ok(),
        app.path().app_cache_dir().ok(),
    )
}

/// What this computer has installed and can run: the tools, the Docker engines, the hypervisors
/// and the package manager (see [`system::system_check`]). The report already carries every
/// provider's readiness in `vm_providers`.
#[tauri::command]
pub(crate) async fn system_check(app: AppHandle) -> SystemReport {
    system::system_check(&runner(&app), &dirs(&app)).await
}

/// Every Vagrant provider (hypervisor) this computer can drive, and why each is or isn't usable.
/// A lighter check than [`system_check`] when only the providers are wanted.
#[tauri::command]
pub(crate) async fn machine_providers(app: AppHandle) -> Vec<ProviderStatus> {
    let runner = runner(&app);
    // Whether Vagrant answers decides what the providers' reasons say; probe it as the system
    // check does.
    let vagrant = exec::run_read(&runner, "vagrant", &["--version"], None)
        .await
        .is_ok();
    providers::detect(&runner, vagrant).await
}

/// Runs the setup self-test of `kind` (on `provider`, for a VM), each step's event sent on
/// `events` as it happens (see [`selftest::machine_selftest`]). The error is the failing step's,
/// as a message for the setup screen.
#[tauri::command]
pub(crate) async fn machine_selftest(
    app: AppHandle,
    kind: Kind,
    provider: Option<Provider>,
    events: Channel<Event>,
) -> Result<(), String> {
    selftest::machine_selftest(&runner(&app), &dirs(&app), kind, provider, move |event| {
        drop(events.send(event));
    })
    .await
    .map_err(|e| e.to_string())
}

/// Downloads what the self-test of `kind` needs ahead of it (its test image, or a provider's test
/// box), so the test itself doesn't wait on the download.
#[tauri::command]
pub(crate) async fn machine_selftest_prefetch(
    app: AppHandle,
    kind: Kind,
    provider: Option<Provider>,
) -> Result<(), String> {
    selftest::machine_selftest_prefetch(&runner(&app), &dirs(&app), kind, provider)
        .await
        .map_err(|e| e.to_string())
}
