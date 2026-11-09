# GNS3 feature coverage

Which of GNS3's features this repository's crates cover: the compiler (`isothrum-compiler`, a
drawn topology to an Isoloom spec, and `isothrum-wasm`, the same in the browser with every
target's files and the download) and the runner (`isothrum-runner` and its CLI, which run what
Isoloom generates). What an application built on them adds around a run that is up (console
windows, packet capture, links set down and up, snapshots) is that application's to list.

Built from GNS3's documented feature set; check a row against docs.gns3.com before quoting it.

Status: **Covered** · **Partial** (part of it, or another mechanism) · **Not covered**.

## Nodes (compiler)

| GNS3 feature | Status | What covers it |
|---|---|---|
| Docker container nodes | Covered | A container node, one image each, kept running idle when its image runs nothing of its own |
| QEMU / full VM nodes | Covered | A VM node: its OS and its provisioning steps; the Isoloom target decides what runs it (Vagrant, Proxmox, a cloud) |
| VirtualBox / VMware nodes | Partial | The Vagrant providers the runner drives, not node types of their own |
| Ethernet switch | Covered | A LAN on a switch, with its VLAN (the switch's VLANs nest under a network named after it) |
| Cisco IOS / IOU / Dynamips images | Covered | A container node with an `appliance` (IOL and the other vrnetlab images, or Dynamips) and its configuration; the user brings the images |
| Router you configure | Covered | Any container or VM (FRR, a Cisco appliance as its LANs' gateway) |
| NAT / cloud node | Partial | A LAN's `internet` flag; no node of its own |
| VPCS, Ethernet hub, Frame Relay and ATM switches | Not covered | |

## Links (compiler)

| GNS3 feature | Status | What covers it |
|---|---|---|
| Point-to-point cabling between ports | Covered | Each cable is its own /29 network, cut from `10.200.0.0/16` |
| Named ports | Covered | A cable's ends name their nodes' ports |
| Link filters: latency, jitter, loss, bandwidth | Covered | `tc` on a LAN or a cable (Isoloom's netem) |
| Routing between links | Covered | A router you configure, or the automatic router: its networks reach each other (`reach` rules both ways) |

## Projects (compiler)

| GNS3 feature | Status | What covers it |
|---|---|---|
| Save and export a project | Covered | The compiler writes `isoloom.yml` and the project's own files (appliance configurations, provisioning starters); the WebAssembly engine zips them with every target's files |
| Validation before running | Covered | The engine's plan lists the problems Isoloom finds in the spec |
| Import a `.gns3` project | Not covered | |

## Running an environment (runner)

| GNS3 feature | Status | What covers it |
|---|---|---|
| Start, stop and suspend a run | Covered | `up`, `down`, `park` (`pause` or `shutdown`) and `resume` |
| Start and stop one node | Partial | `retire` takes a machine out of a run that is up; `attach` and `detach` the run's on-demand access machine |
| Console to a node | Covered | `console <machine>`: the command that opens a shell in any machine of the run |
| Remote server | Partial | Proxmox and cloud targets bring a run up on another host; no server of the runner's own |
