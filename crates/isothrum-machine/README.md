# isothrum-machine

The machine-setup domain: this computer (`Host`: its OS, CPU, package manager and the files
detection reads), and which Vagrant providers (hypervisors) it can drive among those Vagrantfiles
name (VirtualBox, VMware Desktop, Hyper-V, Parallels, libvirt, QEMU, UTM, VMware ESXi). For each
one offered on this computer it says whether VMs can run on it here, what is still to install
(`Need`: Vagrant, the plugin, HashiCorp's VMware Utility) and, when it can't, why, as a code
(`Unavailable`) a UI translates, with an English `message()` for logs and the command line:
Vagrant or the hypervisor not installed, a plugin missing or too old, VirtualBox's service not
answering or its kernel driver not loaded, KVM holding the CPU's virtualization, `/dev/kvm`
missing or not this account's, a group joined since logging in, Hyper-V turned off, its
hypervisor not running, or this account not allowed to manage it.

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

## Feature flags

| Feature | What it adds |
|---|---|
| `ts-rs` | The TypeScript types (through [ts-rs](https://docs.rs/ts-rs)) of `ProviderStatus`, `Unavailable`, `Need`, `UtilityState` and `AccountLacks`, which follow their serde form, for a UI in TypeScript; with the runner's `VagrantProvider`. |

No feature is on by default.

## Documentation

- API: <https://docs.rs/isothrum-machine>
- Guides and reference: <https://github.com/Isothrum/isothrum/blob/main/docs/README.md>

Minimum Rust: 1.90. Licence: Apache-2.0.
