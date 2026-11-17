# isothrum-machine

The machine-setup domain: what this computer has (`check::check`: its tools, its Docker-compatible
engines and their CLI contexts, its package manager, its cloud CLIs and, on Windows, whether WSL 2
is ready), and this computer (`Host`: its OS, CPU, package manager and the files
detection reads), and which Vagrant providers (hypervisors) it can drive among those Vagrantfiles
name (VirtualBox, VMware Desktop, Hyper-V, Parallels, libvirt, QEMU, UTM, VMware ESXi). For each
one offered on this computer it says whether VMs can run on it here, what is still to install
(`Need`: Vagrant, the plugin, HashiCorp's VMware Utility) and, when it can't, why, as a code
(`Unavailable`) a UI translates, with an English `message()` for logs and the command line:
Vagrant or the hypervisor not installed, a plugin missing or too old, VirtualBox's service not
answering or its kernel driver not loaded, KVM holding the CPU's virtualization, `/dev/kvm`
missing or not this account's, a group joined since logging in, Hyper-V turned off, its
hypervisor not running, or this account not allowed to manage it.

It also asks the cloud accounts this computer's cloud CLIs (`aws`, `az`, `gcloud`, `oci`) are
signed in to (`cloud`): whether a sign-in still holds, and what an account offers (AWS's spend and
launch permission, Azure's subscriptions, VM sizes and quotas, Google Cloud's billing accounts and
organizations), a refusal read as a code (`Refusal::CredentialsExpired` for a sign-in run out).

Every tool it runs (`vagrant plugin list`, `VBoxManage`, `vmrun`, `whoami`, `id`, ...) goes
through `isothrum-runner`'s `ProcessRunner`, each probe within a time limit, so tests script them;
the files it reads instead (`/dev/kvm`, `/proc/modules`, UTM's app bundle, Hyper-V's service...)
are the `Host`'s `Devices`, which a test points into a folder of its own. The crate has no
`unsafe` code and no statics: everything goes through one `Context`.

## Install

```sh
cargo add isothrum-machine isothrum-runner
cargo add tokio --features macros,rt-multi-thread
```

## Example

```rust,no_run
use std::sync::Arc;

use isothrum_machine::{Context, providers};
use isothrum_runner::SystemProcesses;

#[tokio::main]
async fn main() {
    let cx = Context::here(Arc::new(SystemProcesses::default())).await;
    // `true`: Vagrant is installed (the caller checks).
    for status in providers::detect(&cx, true).await {
        let name = isothrum_runner::wire_name(&status.provider);
        match &status.reason {
            None => println!("{name}: usable"),
            Some(why) => println!("{name}: {why}"),
        }
    }
}
```

The whole machine check in one call:

```rust,no_run
use std::sync::Arc;

use isothrum_machine::{Context, check, wsl};
use isothrum_runner::SystemProcesses;

#[tokio::main]
async fn main() {
    let cx = Context::here(Arc::new(SystemProcesses::default())).await;
    let found = check::check(&cx, wsl::Marks::default()).await;
    println!("Docker: {:?}, daemon running: {}", found.docker.version, found.daemon.running());
    for engine in &found.engines {
        println!("{} ({}): running {}", engine.engine.label(), engine.context, engine.running);
    }
}
```

Installing a tool on Linux, through pkexec's password prompt:

```rust,no_run
use std::sync::Arc;

use isothrum_machine::install::{self, Dependency, Elevation, linux};
use isothrum_machine::Context;
use isothrum_runner::SystemProcesses;

#[tokio::main]
async fn main() {
    let cx = Context::here(Arc::new(SystemProcesses::default())).await;
    let root = linux::Root { elevation: Elevation::Prompt, proxy: linux::proxy_assignments(|v| std::env::var(v).ok()) };
    let steps = linux::install(Dependency::Vagrant, cx.host.packages, None, &root).expect("a plan here");
    install::run_steps(&cx, &steps, &mut |line: &str| println!("{line}")).await.expect("installed");
}
```

## Feature flags

| Feature | What it adds |
|---|---|
| `ts-rs` | The TypeScript types (through [ts-rs](https://docs.rs/ts-rs)) of `ProviderStatus`, `Unavailable`, `Need`, `UtilityState`, `AccountLacks`, `Tool`, `CloudClis`, `PackageManager`, `Engine` (as `DockerEngine`), `EngineStatus`, `WslState` and `Dependency`, which follow their serde form, for a UI in TypeScript; with the runner's `VagrantProvider`. |

No feature is on by default.

## Documentation

- API: <https://docs.rs/isothrum-machine>
- Guides and reference: <https://github.com/Isothrum/isothrum/blob/main/docs/README.md>

Minimum Rust: 1.90. Licence: Apache-2.0.
