# isothrum-machine

Detects which Vagrant providers (hypervisors) this computer can drive, among those Isoloom's run
manifests name (VirtualBox, VMware Desktop, Hyper-V, Parallels, libvirt, QEMU, UTM, VMware ESXi)
that apply to its operating system. For each one it says whether it is usable here and, when it
isn't, why: Vagrant or the hypervisor not installed, a plugin missing or too old, VirtualBox's
service not answering, or Hyper-V needing an administrator (when the caller says the account may
not manage it).

Every probe runs through `isothrum-runner`'s `ProcessRunner`, so tests can script the tools, and the
crate has no `unsafe` code.

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
    // `true`: Vagrant is installed (the caller checks). `None`: no answer of our own to "may this
    // account manage Hyper-V", so no administrator reason is added.
    for status in providers::detect(&tools, true, None).await {
        match &status.reason {
            None => println!("{}: usable", providers::id(status.provider)),
            Some(why) => println!("{}: {why}", providers::id(status.provider)),
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
