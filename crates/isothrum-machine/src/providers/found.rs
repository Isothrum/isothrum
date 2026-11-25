//! A provider's hypervisor as this computer finds it: installed or not, by which program, and its version.

use super::{Check, Context, Provider, ProviderRow, exec, hypervisor_probe};

/// What probing a hypervisor found.
pub(super) struct Found {
    /// Whether it is installed, `None` when there is nothing local to probe.
    pub(super) installed: Option<bool>,
    /// What its tool printed, standard output then error, when it has one and it answered.
    pub(super) said: String,
}

/// Whether `provider`'s hypervisor is installed on the context's host: its tool starts (or ran
/// out of the probe's time: there, and slow), its app bundle whole (a copy under way or damaged
/// isn't: [`crate::install::mac_image::complete`]) or its Windows feature there.
pub(super) async fn hypervisor_found(cx: &Context, provider: Provider) -> Found {
    let devices = &cx.host.devices;
    let installed = |installed| Found {
        installed: Some(installed),
        said: String::new(),
    };
    match ProviderRow::of(provider).check {
        Check::UtmApp => installed(crate::install::mac_image::complete(cx, &devices.utm_app).await),
        Check::HypervFeature => installed(devices.hyperv_service.is_file()),
        Check::Remote => Found {
            installed: None,
            said: String::new(),
        },
        Check::Tool(..) | Check::Qemu => {
            let Some((program, args)) = hypervisor_probe(provider, cx.host.arch) else {
                return installed(false);
            };
            let mut answer = cx.probe(program, args).await;
            // Not on PATH: where its app installs it (VMware's `vmrun`, Parallels' `prlctl`).
            if matches!(answer, Err(ref unanswered) if !unanswered.tool_is_there())
                && let Some(at) = devices.app_tool(program).into_iter().find(|t| t.is_file())
            {
                answer = cx.probe(&at.display().to_string(), args).await;
            }
            match answer {
                Ok(captured) => Found {
                    installed: Some(true),
                    said: exec::all_lines(&captured),
                },
                Err(unanswered) => installed(unanswered.tool_is_there()),
            }
        }
    }
}

/// The hypervisor's own version, when it says one and is held to one: UTM's app bundle
/// (`CFBundleShortVersionString`).
pub(super) async fn hypervisor_version(cx: &Context, provider: Provider) -> Option<String> {
    if ProviderRow::of(provider).check != Check::UtmApp {
        return None;
    }
    let plist = cx.host.devices.utm_app.join("Contents").join("Info.plist");
    let out = cx
        .probe("defaults", &["read", &plist.to_string_lossy(), "CFBundleShortVersionString"])
        .await
        .ok()?;
    let version = out.stdout.join("\n").trim().to_owned();
    (!version.is_empty()).then_some(version)
}
