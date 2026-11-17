//! The self-tests on this computer's engine and hypervisors, as a caller runs them: each passes
//! and takes down what it started. What the test itself fetched (the engine test's image, the
//! hypervisor test's box) is removed again; what was here before stays.

use std::sync::{Mutex, PoisonError};

use isothrum_machine::providers::{self, Provider};
use isothrum_machine::selftest::hypervisor::{self, Choices, TestVm, add_box, box_for, box_provider};
use isothrum_machine::selftest::{Progress, engine};
use isothrum_machine::{Context, docker};
use isothrum_runner::BoxFuture;

use super::{quit_utm, utm_running};
use crate::{answer, here};

/// The test VM: its own name and host name, which also prefix its libvirt domain.
const VM_NAME: &str = "Isothrum e2e VM";
const VM_HOST: &str = "isothrum-e2e";

/// The engine test passes on the engine the Docker CLI points at, and leaves no deployment; the
/// test image is removed again when the test pulled it.
#[tokio::test]
#[ignore = "real machine: cargo xtask e2e --tier machine"]
async fn the_engine_self_test_passes_on_this_computers_engine() {
    let cx = here().await;
    if let Err(why) = docker::daemon_os(&cx).await {
        skip!("no container engine answers here: {why}")
    }
    let dir = tempfile::tempdir().expect("a folder");
    let said = Mutex::new(Vec::new());
    let sink = |p: engine::StepProgress| said.lock().unwrap_or_else(PoisonError::into_inner).push(format!("{p:?}"));
    let ran = engine::run(&cx, dir.path(), None, &sink).await;
    if ran.pulled.is_some() {
        answer(&cx, "docker", &["image", "rm", engine::IMAGE]).await;
    }
    let said = said.into_inner().unwrap_or_else(PoisonError::into_inner);
    println!("{said:#?}");
    if let Err(failed) = &ran.passed {
        panic!("{failed}")
    }
    assert_eq!(ran.cleaned_up, Ok(()), "taken down");
}

/// The test's choices: `provider`, its box added when Vagrant doesn't hold it (and said so).
struct Here<'a> {
    cx: &'a Context,
    provider: Provider,
    added: Mutex<Option<&'static str>>,
}

impl Choices for Here<'_> {
    fn provider(&self) -> BoxFuture<'_, Result<Provider, String>> {
        let p = self.provider;
        Box::pin(async move { Ok(p) })
    }

    fn fetch_box<'a>(&'a self, p: Provider, name: &'static str, line: &'a (dyn Fn(String) + Send + Sync)) -> BoxFuture<'a, Result<bool, String>> {
        Box::pin(async move {
            if box_for(self.cx, p).await == (name, true) {
                return Ok(true);
            }
            *self.added.lock().unwrap_or_else(PoisonError::into_inner) = Some(name);
            add_box(self.cx, p, name, &mut |l| line(l.to_owned())).await.map(|_| false)
        })
    }
}

/// The hypervisor test boots the smallest box on `provider` and takes its VM down; the box goes
/// again when the test added it. Skips where the provider can't run VMs here, saying why.
async fn boots_on(provider: Provider) {
    let cx = here().await;
    let Some(status) = providers::status(&cx, provider).await else {
        skip!("{} isn't offered here", provider.id())
    };
    if let Some(why) = &status.reason {
        skip!("{} can't run VMs here: {}", provider.id(), why.message())
    }
    let dir = tempfile::tempdir().expect("a folder");
    let said = Mutex::new(Vec::new());
    let sink = |p: hypervisor::StepProgress| {
        if !matches!(p, Progress::Passed(_, hypervisor::Found::Line(_))) {
            said.lock().unwrap_or_else(PoisonError::into_inner).push(format!("{p:?}"));
        }
    };
    let here = Here {
        cx: &cx,
        provider,
        added: Mutex::new(None),
    };
    let utm_was_running = provider == Provider::Utm && utm_running(&cx).await;
    let ran = hypervisor::run(&cx, dir.path(), &TestVm::new(VM_NAME, VM_HOST), &here, &sink).await;
    if provider == Provider::Utm && !utm_was_running {
        quit_utm(&cx).await;
    }
    if let Some(added) = here.added.into_inner().unwrap_or_else(PoisonError::into_inner) {
        answer(
            &cx,
            "vagrant",
            &["box", "remove", added, "--provider", box_provider(provider), "--all", "--force"],
        )
        .await;
    }
    let said = said.into_inner().unwrap_or_else(PoisonError::into_inner);
    println!("{said:#?}");
    if let Err(failed) = &ran.passed {
        panic!("{failed}")
    }
    assert_eq!(ran.cleaned_up, Some(Ok(())), "taken down");
    assert_eq!(leftover(&cx, provider).await, None, "the test's VM is gone");
}

/// The test's VM, when the hypervisor still lists it (UTM isn't asked: asking opens it).
async fn leftover(cx: &Context, provider: Provider) -> Option<String> {
    let listed = match provider {
        Provider::Virtualbox => answer(cx, "VBoxManage", &["list", "vms"]).await,
        Provider::Libvirt => answer(cx, "virsh", &["-c", "qemu:///system", "list", "--all", "--name"]).await,
        _ => None,
    };
    listed?.lines().find(|l| l.contains(VM_NAME) || l.contains(VM_HOST)).map(str::to_owned)
}

#[tokio::test]
#[ignore = "real machine: cargo xtask e2e --tier machine"]
async fn the_hypervisor_self_test_boots_a_vm_on_virtualbox() {
    boots_on(Provider::Virtualbox).await;
}

#[tokio::test]
#[ignore = "real machine: cargo xtask e2e --tier machine"]
async fn the_hypervisor_self_test_boots_a_vm_on_libvirt() {
    boots_on(Provider::Libvirt).await;
}

#[tokio::test]
#[ignore = "real machine: cargo xtask e2e --tier machine"]
async fn the_hypervisor_self_test_boots_a_vm_on_qemu() {
    boots_on(Provider::Qemu).await;
}

#[tokio::test]
#[ignore = "real machine: cargo xtask e2e --tier machine"]
async fn the_hypervisor_self_test_boots_a_vm_on_utm() {
    boots_on(Provider::Utm).await;
}

#[tokio::test]
#[ignore = "real machine: cargo xtask e2e --tier machine"]
async fn the_hypervisor_self_test_boots_a_vm_on_hyperv() {
    boots_on(Provider::Hyperv).await;
}

#[tokio::test]
#[ignore = "real machine: cargo xtask e2e --tier machine"]
async fn the_hypervisor_self_test_boots_a_vm_on_vmware_desktop() {
    boots_on(Provider::VmwareDesktop).await;
}
