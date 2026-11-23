# Supported tool versions

Before `up`, `resume` and `update`, Isothrum probes the tools the project's format uses on this
host: a tool that doesn't answer fails with `tool_missing` (or `tool_failed`), one older than its
minimum with `tool_too_old`, before anything is created. A tool that prints no version the runner
reads is used, its minimum unchecked, with a warning.

| Tool | Minimum | Used by | Probe |
|---|---|---|---|
| Docker Engine | any that answers | Compose projects; Terraform from its image on macOS | `docker info` |
| Docker Compose | 2.17.0 | Compose projects | `docker compose version --short` |
| Vagrant | 2.4.0 | Vagrantfiles | `vagrant --version` |
| VirtualBox, VMware Desktop, Parallels, libvirt, QEMU | any that answers | Vagrant, with that provider | each one's own command (`VBoxManage --version`, `vmrun list`, ...) |
| Terraform | 1.6.0 | Terraform modules | `terraform version` |
| VMware OVF Tool | any that answers | Vagrantfiles on the `vmware_esxi` provider | `ovftool --version` |
| OpenSSH client | any that answers; 7.6 or later works (below) | machines reached over SSH | `ssh -V` |

The runner probes `ssh` for an answer only, but the configuration the runner writes for each
run's SSH needs OpenSSH 7.6 or later (`StrictHostKeyChecking accept-new`; `-J` jump hosts came
in 7.3). Its paths (the runner's key and the run's `known_hosts`, under the home) are written
between double quotes, which every release reads, Windows 10's 8.1 and RHEL 8's 8.0 included;
on Windows they are written with `/`. A home whose path holds `"`, `${`, a control character
or, outside Windows, `\` can't be written there for every release: an SSH call fails with `io`,
naming the file. A home whose path holds `%` needs OpenSSH 8.4 or later, which reads `%%` in
`UserKnownHostsFile` as `%`.

On macOS, a Terraform module for Proxmox runs Terraform from the image `hashicorp/terraform:1.16.7`, pinned by
digest, instead of a host binary (macOS's Local Network Privacy keeps third-party binaries off the
local network); only Docker has to answer there.

## Vagrant providers

`isothrum-machine` (provider detection, for an embedder that offers a choice) also holds the UTM
provider to a version: UTM 4.6.5 or later and the `vagrant_utm` plugin 0.2.0 or later.

## Terraform providers

The providers come from each module's own `required_providers`, downloaded by
`terraform init` into a plugin cache every run shares: your `TF_PLUGIN_CACHE_DIR` when it is set,
else `<home>/terraform-plugins`. To install them from a mirror, see
[Run offline](../how-to/offline.md).

## OpenTofu

Not supported as a choice: a Terraform module runs with `terraform`. Pointing `terraform` at
OpenTofu is untested.
