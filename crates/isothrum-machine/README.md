# isothrum-machine

Detects which Vagrant providers (hypervisors) this computer can drive, among those Isoloom's run
manifests name (VirtualBox, VMware Desktop, Hyper-V, Parallels, libvirt, QEMU, UTM, VMware ESXi)
that apply to its operating system. For each one it says whether it is usable here and, when it
isn't, why: Vagrant or the hypervisor not installed, a plugin missing or too old, VirtualBox's
service not answering, or Hyper-V needing an administrator (when `whoami /groups` says this
account may not manage it).

Every tool it runs (`vagrant plugin list`, `VBoxManage`, `vmrun`, ...) goes through
`isothrum-runner`'s `ProcessRunner` (`whoami` included), so tests can script them; a few facts are read from the
filesystem instead (UTM's app bundle, Hyper-V's service, `/dev/kvm`). The crate has no `unsafe`
code.

## Install

```sh
cargo add isothrum-machine isothrum-runner
cargo add tokio --features macros,rt-multi-thread
```

## Example

```rust,no_run
use isothrum_machine::providers;
use isothrum_runner::SystemProcesses;

#[tokio::main]
async fn main() {
    let tools = SystemProcesses::default();
    // `true`: Vagrant is installed (the caller checks).
    for status in providers::detect(&tools, true).await {
        match &status.reason {
            None => println!("{}: usable", isothrum_runner::wire_name(&status.provider)),
            Some(why) => println!("{}: {why}", isothrum_runner::wire_name(&status.provider)),
        }
    }
}
```

## Feature flags

None.

## Documentation

- API: <https://docs.rs/isothrum-machine>
- Guides and reference: <https://github.com/Isothrum/isothrum/blob/main/docs/README.md>

Minimum Rust: 1.90. Licence: Apache-2.0.
