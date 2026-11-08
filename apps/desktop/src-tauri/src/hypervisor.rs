//! Which hypervisor (Vagrant provider) a lab with VMs runs on here. Vagrant left to itself picks
//! by plugin order, which on an Apple Silicon Mac is UTM, whose plugin gives a VM no private
//! network, so every lab failed there before a VM was made. The app chooses instead: the one
//! [`OVERRIDE_ENV`] names, else the first installed and usable local hypervisor that Isoloom
//! doesn't refuse for this lab, in [`PREFERENCE`]'s order. The choice is recorded in the lab's
//! folder (`vms::PROVIDER`), so every later Vagrant command runs against the same hypervisor.

use isothrum_machine::providers::{Provider, ProviderStatus};

/// What the choice reads of a hypervisor's detection: usable here, or why not.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Candidate {
    pub(crate) provider: Provider,
    pub(crate) available: bool,
    pub(crate) reason: Option<String>,
}

impl Candidate {
    /// The detection's verdicts, as the choice reads them.
    pub(crate) fn of(statuses: &[ProviderStatus]) -> Vec<Candidate> {
        statuses
            .iter()
            .map(|s| Candidate {
                provider: s.provider,
                available: s.available,
                reason: s.reason.clone(),
            })
            .collect()
    }
}

/// The environment variable naming the hypervisor to use (its Vagrant id: `virtualbox`,
/// `qemu`, …), overriding the choice.
pub(crate) const OVERRIDE_ENV: &str = "ISOTHRUM_VM_PROVIDER";

/// The local hypervisors, most preferred first: the desktop hypervisors with full private
/// networks, then QEMU (one network per VM, x86 machines emulated) and UTM (no private network).
const PREFERENCE: [Provider; 7] = [
    Provider::Virtualbox,
    Provider::Parallels,
    Provider::VmwareDesktop,
    Provider::Libvirt,
    Provider::Hyperv,
    Provider::Qemu,
    Provider::Utm,
];

/// The hypervisor's name as the user reads it.
fn name(p: Provider) -> &'static str {
    match p {
        Provider::Virtualbox => "VirtualBox",
        Provider::Parallels => "Parallels",
        Provider::VmwareDesktop => "VMware",
        Provider::Libvirt => "libvirt",
        Provider::Hyperv => "Hyper-V",
        Provider::Qemu => "QEMU",
        Provider::Utm => "UTM",
        _ => "this hypervisor",
    }
}

/// The hypervisor the lab runs on: `wanted` (the override) when given, else the first of
/// [`PREFERENCE`] that `statuses` (the detection's verdicts) reports usable here and `refusal`
/// (Isoloom's verdict on the lab and that provider) accepts. The error says why each one is out.
pub(crate) fn choose(
    statuses: &[Candidate],
    wanted: Option<&str>,
    refusal: impl Fn(&str) -> Option<String>,
) -> Result<Provider, String> {
    if let Some(id) = wanted {
        let p = PREFERENCE
            .iter()
            .copied()
            .find(|p| p.id() == id)
            .ok_or_else(|| {
                let ids: Vec<&str> = PREFERENCE.iter().map(|p| p.id()).collect();
                format!(
                    "{OVERRIDE_ENV}={id} isn't a local hypervisor: one of {}",
                    ids.join(", ")
                )
            })?;
        return match refusal(id) {
            None => Ok(p),
            Some(why) => Err(format!(
                "{OVERRIDE_ENV}={id}: this lab can't run on {}: {why}",
                name(p)
            )),
        };
    }
    let mut reasons = Vec::new();
    for p in PREFERENCE {
        // A hypervisor the detection didn't list doesn't exist on this OS.
        let Some(status) = statuses.iter().find(|s| s.provider == p) else {
            continue;
        };
        if !status.available {
            let why = status.reason.as_deref().unwrap_or("not installed");
            reasons.push(format!("- {}: {why}", name(p)));
            continue;
        }
        match refusal(p.id()) {
            None => return Ok(p),
            Some(why) => reasons.push(format!("- {}: this lab can't run on it: {why}", name(p))),
        }
    }
    Err(format!(
        "No hypervisor on this computer can run this lab's VMs:\n{}\nInstall one of them with its \
         Vagrant plugin, or name one with {OVERRIDE_ENV}.",
        reasons.join("\n")
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn status(provider: Provider, available: bool, reason: Option<&str>) -> Candidate {
        Candidate {
            provider,
            available,
            reason: reason.map(str::to_owned),
        }
    }

    /// This Mac: UTM and QEMU installed, VirtualBox not.
    fn mac() -> Vec<Candidate> {
        vec![
            status(
                Provider::Virtualbox,
                false,
                Some("VirtualBox isn't installed"),
            ),
            status(
                Provider::Parallels,
                false,
                Some("Parallels isn't installed"),
            ),
            status(Provider::Qemu, true, None),
            status(Provider::Utm, true, None),
        ]
    }

    #[test]
    fn the_first_usable_hypervisor_isoloom_accepts_is_chosen() {
        let utm_refused =
            |p: &str| (p == "utm").then(|| "vagrant_utm gives a VM no private network".to_owned());
        assert_eq!(choose(&mac(), None, utm_refused), Ok(Provider::Qemu));
        let mut with_vbox = mac();
        with_vbox[0] = status(Provider::Virtualbox, true, None);
        assert_eq!(choose(&with_vbox, None, |_| None), Ok(Provider::Virtualbox));
    }

    #[test]
    fn with_none_usable_every_reason_is_told() {
        let e = choose(&mac(), None, |_| Some("the lab has a TPM".to_owned())).unwrap_err();
        assert!(
            e.contains("- VirtualBox: VirtualBox isn't installed"),
            "{e}"
        );
        assert!(
            e.contains("- QEMU: this lab can't run on it: the lab has a TPM"),
            "{e}"
        );
        assert!(e.contains(OVERRIDE_ENV), "{e}");
    }

    #[test]
    fn the_override_wins_but_is_still_checked() {
        assert_eq!(choose(&mac(), Some("utm"), |_| None), Ok(Provider::Utm));
        let e = choose(&mac(), Some("utm"), |_| {
            Some("no private network".to_owned())
        })
        .unwrap_err();
        assert!(
            e.starts_with("ISOTHRUM_VM_PROVIDER=utm: this lab can't run on UTM"),
            "{e}"
        );
        let not_local = choose(&mac(), Some("aws"), |_| None).unwrap_err();
        assert!(
            not_local.contains("isn't a local hypervisor"),
            "{not_local}"
        );
    }
}
