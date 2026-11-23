//! VMware on Windows: the host adapter of a private network vagrant-vmware-desktop made keeps an
//! automatic address (169.254.x). The utility writes the network's host address into VMware's
//! configuration, but Windows gives the adapter that address only once VMware's
//! `vnetlib -- update adapter vmnet<n>` runs, as an administrator; until then the computer can't
//! reach a machine at its private address (a machine's site, the self-test's ping).
//!
//! After an `up` on VMware on Windows, the `VMnet` adapters left with such an address are updated in
//! one elevated step: one UAC prompt, and only when an adapter is left without its address. A
//! prompt declined, or an update that didn't take, is a warning
//! ([`WarningCode::HostAdapterUnaddressed`]): the run's machines run, reached on their forwarded
//! ports, not at their private addresses.

use std::time::Duration;

use super::ToolCx;
use crate::events::{Stream, WarningCode};
use crate::process::{DECLINED, ProcessSpec, capture, elevated_once};
use crate::project::platform::{HostOs, Provider};

/// The name Windows gives VMware's adapter of network `vmnet<n>`, before `VMnet<n>`.
const ADAPTER: &str = "VMware Network Adapter ";

/// Lists each VMware adapter's IPv4 addresses, one `<adapter>\t<address>` a line (no double
/// quote: Windows PowerShell 5.1 mangles those in a native program's argument).
const LIST: &str = "Get-NetIPAddress -AddressFamily IPv4 -ErrorAction SilentlyContinue | \
     Where-Object { $_.InterfaceAlias -like 'VMware Network Adapter VMnet*' } | \
     ForEach-Object { $_.InterfaceAlias + [char]9 + $_.IPAddress }";

/// How long Windows may take to show an updated adapter's address.
const SETTLE: Duration = Duration::from_secs(10);

/// How often the addresses are read again meanwhile.
const POLL: Duration = Duration::from_secs(1);

/// The networks (`vmnet<n>`) whose adapter, in `listing` ([`LIST`]), has only an automatic
/// address: none listed for an adapter that also has one of its own.
pub(super) fn unaddressed(listing: &[String]) -> Vec<String> {
    let mut automatic = Vec::new();
    let mut own = Vec::new();
    for line in listing {
        let Some((adapter, address)) = line.trim().split_once('\t') else { continue };
        let Some(net) = adapter.strip_prefix(ADAPTER).map(str::to_ascii_lowercase) else {
            continue;
        };
        if !is_vmnet(&net) {
            continue;
        }
        let into = if address.trim().starts_with("169.254.") { &mut automatic } else { &mut own };
        if !into.contains(&net) {
            into.push(net);
        }
    }
    automatic.retain(|n| !own.contains(n));
    automatic
}

/// Whether `net` is a VMware network's name, `vmnet` and its number: all the elevated script is
/// ever given.
fn is_vmnet(net: &str) -> bool {
    net.strip_prefix("vmnet")
        .is_some_and(|n| !n.is_empty() && n.len() <= 3 && n.bytes().all(|b| b.is_ascii_digit()))
}

/// The script run elevated: VMware's `vnetlib` found where VMware installed it, each of `nets`
/// updated. It exits 2 when there is no `vnetlib`, 3 when an update failed.
pub(super) fn update_script(nets: &[String]) -> String {
    let nets = nets.iter().filter(|n| is_vmnet(n)).map(|n| format!("'{n}'")).collect::<Vec<_>>().join(",");
    format!(
        "$dirs = @()\n\
         foreach ($k in 'HKLM:\\SOFTWARE\\VMware, Inc.\\VMware Workstation', 'HKLM:\\SOFTWARE\\WOW6432Node\\VMware, Inc.\\VMware Workstation') {{\n\
         \x20   $d = (Get-ItemProperty -LiteralPath $k -ErrorAction SilentlyContinue).InstallPath\n\
         \x20   if ($d) {{ $dirs += $d }}\n\
         }}\n\
         $dirs += (Join-Path $env:ProgramFiles 'VMware\\VMware Workstation')\n\
         $v = $dirs | ForEach-Object {{ Join-Path $_ 'vnetlib.exe' }} | Where-Object {{ Test-Path -LiteralPath $_ }} | Select-Object -First 1\n\
         if (-not $v) {{ exit 2 }}\n\
         $failed = 0\n\
         foreach ($n in @({nets})) {{ & $v -- update adapter $n; if ($LASTEXITCODE -ne 0) {{ $failed = 3 }} }}\n\
         exit $failed\n"
    )
}

/// `script` as Windows PowerShell runs it.
fn powershell(script: &str) -> ProcessSpec {
    ProcessSpec::new("powershell", ["-NoProfile", "-NonInteractive", "-Command", script])
}

/// The networks whose adapter has only an automatic address now; none when they can't be read.
async fn read_unaddressed(cx: &ToolCx<'_>) -> Vec<String> {
    match capture(cx.processes, &powershell(LIST)).await {
        Ok(out) if out.success() => unaddressed(&out.stdout),
        Ok(out) => {
            tracing::debug!(reason = %out.reason(), "VMware's host adapters couldn't be read");
            Vec::new()
        }
        Err(e) => {
            tracing::debug!(error = %e, "VMware's host adapters couldn't be read");
            Vec::new()
        }
    }
}

/// After an `up` on `provider` on `host`: on VMware on Windows, the private networks' host
/// adapters left without their address given it, as an administrator (one prompt). Best effort:
/// what it couldn't do is a warning, and the start stands.
pub(super) async fn address_host_adapters(cx: &ToolCx<'_>, host: HostOs) {
    if host != HostOs::Windows || cx.provider != Some(Provider::VmwareDesktop) {
        return;
    }
    let nets = read_unaddressed(cx).await;
    if nets.is_empty() {
        return;
    }
    (cx.sink)(
        Stream::Stdout,
        &format!(
            "Giving VMware's host adapter of {} its address: Windows asks for an administrator's permission",
            nets.join(", ")
        ),
    );
    let why = match capture(cx.processes, &powershell(&elevated_once(&update_script(&nets)))).await {
        Ok(out) if out.success() => match settled(cx).await {
            left if left.is_empty() => return,
            left => format!("{} still has no address after VMware's update", left.join(", ")),
        },
        Ok(out) if out.exit.code == Some(DECLINED) => "the Windows permission prompt was declined".to_owned(),
        Ok(out) if out.exit.code == Some(2) => "VMware's `vnetlib.exe` wasn't found".to_owned(),
        Ok(out) => format!("VMware's update failed: {}", out.reason()),
        Err(e) => format!("VMware's update couldn't start: {e}"),
    };
    (cx.warn)(
        WarningCode::HostAdapterUnaddressed,
        &format!(
            "VMware's host adapter of {} has no address ({why}): this computer reaches the machines on their forwarded ports, not at their private addresses",
            nets.join(", ")
        ),
    );
}

/// The networks still without their address once Windows had [`SETTLE`] to show it.
async fn settled(cx: &ToolCx<'_>) -> Vec<String> {
    let deadline = tokio::time::Instant::now() + SETTLE;
    loop {
        let left = read_unaddressed(cx).await;
        if left.is_empty() || tokio::time::Instant::now() >= deadline {
            return left;
        }
        tokio::time::sleep(POLL).await;
    }
}
