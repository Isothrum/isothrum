# Supported tool versions

Isothrum doesn't hard-code tool versions. Each run manifest lists the tools its target uses on
each host, the command that probes each one, and the minimum version it needs (`tools[]` in
`run.json`). Before `up`, `resume` and `update`, Isothrum runs every probe that applies to this host and
provider: a tool that doesn't answer fails with `tool_missing` (or `tool_failed`), one older than
its minimum with `tool_too_old`, before anything is created.

The minimums below are the ones Isoloom's current targets write. The manifest you generate is the
authority: `jq '.tools' .isoloom/<target>/run.json` shows it.

| Tool | Minimum | Used by | Probe |
|---|---|---|---|
| Docker Engine | any that answers | Compose targets; Terraform from its image on macOS | `docker info` |
| Docker Compose | 2.17.0 | `docker` | `docker compose version --short` |
| Vagrant | 2.4.0 | `vagrant`, `docker-vm` | `vagrant --version` |
| VirtualBox, VMware Desktop, Parallels, libvirt, QEMU | any that answers | Vagrant, with that provider | each one's own command (`VBoxManage --version`, `vmrun list`, ...) |
| Terraform | 1.6.0 | `proxmox`, `docker-vm/proxmox` | `terraform version` |
| Terraform | 1.11.0 | `cloud-vm/*`, `cloud-docker/*` (and Isoloom's other cloud targets) | `terraform version` |
| OpenSSH client | any that answers | targets reached over SSH | `ssh -V` |

On macOS, the Proxmox targets run Terraform from the image `hashicorp/terraform:1.9.8`, pinned by
digest, instead of a host binary (macOS's Local Network Privacy keeps third-party binaries off the
local network); only Docker has to answer there.

## Vagrant providers

`isothrum-machine` (provider detection, for an embedder that offers a choice) also holds the UTM
provider to a version: UTM 4.6.5 or later and the `vagrant_utm` plugin 0.2.0 or later.

## Terraform providers

The providers come from each generated module's own `required_providers`, downloaded by
`terraform init` into a plugin cache every run shares: your `TF_PLUGIN_CACHE_DIR` when it is set,
else `<home>/terraform-plugins`. To install them from a mirror, see
[Run offline](../how-to/offline.md).

## OpenTofu

Not supported as a choice. Isoloom's manifests run `terraform`, and Isothrum runs the program the
manifest names. Isothrum treats a `tofu` step as it treats `terraform` (the shell's cloud settings
withheld, a failed `apply` retried where the manifest allows it), but no generated manifest uses
it. Pointing `terraform` at OpenTofu is untested.
