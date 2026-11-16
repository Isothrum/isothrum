# Isothrum

[![Gate](https://github.com/Isothrum/isothrum/actions/workflows/gate.yml/badge.svg?branch=main)](https://github.com/Isothrum/isothrum/actions/workflows/gate.yml)
[![crates.io](https://img.shields.io/crates/v/isothrum-cli.svg)](https://crates.io/crates/isothrum-cli)
[![Licence: Apache-2.0](https://img.shields.io/badge/licence-Apache--2.0-blue.svg)](LICENSE)

Isothrum brings environments up and down: containers with Docker Compose, virtual machines with
Vagrant, servers and cloud resources with Terraform. It runs any Compose project, Vagrantfile or
Terraform module as it is, whoever wrote it, through that format's own tool. `up`, `check`,
`park`, `resume`, `update` and `down` work the same whatever the format, with one JSON event
stream, stable error codes and secrets that never reach a command line.

It is for teams that want reproducible test, demo or training environments on a laptop, a server
or a cloud account, and for CI pipelines that need one for the length of a job. It is a command
line (`isothrum`) and Rust libraries to embed in your own program.

## Install

**Release binaries** for Linux (x86_64, aarch64; static), macOS (arm64, x86_64) and Windows
(x86_64) are on the [releases page](https://github.com/Isothrum/isothrum/releases), with SHA-256
checksums, a Sigstore signature and build provenance:
[verify a download](docs/how-to/verify-a-release.md).

With [cargo-binstall](https://github.com/cargo-bins/cargo-binstall), which fetches the release
binary for your platform:

```sh
cargo binstall isothrum-cli
```

From source (Rust 1.90 or later):

```sh
cargo install isothrum-cli --locked
```

As a library: `cargo add isothrum-runner` ([crate README](crates/isothrum-runner/README.md)).

## Requirements

The tools of the projects you run ([supported tool versions](docs/reference/tool-versions.md)):

- Docker with Compose 2.17.0 or later, for container targets;
- Vagrant 2.4.0 or later and a hypervisor (VirtualBox, VMware, Hyper-V, Parallels, libvirt, QEMU,
  UTM), for VM targets;
- Terraform 1.6.0 or later, for servers and clouds;
- an OpenSSH client, for targets reached over SSH.

Isothrum checks each one before it starts anything. It needs nothing else: the project's files
can be written by hand or by a generator such as [Isoloom](https://github.com/isoloom/isoloom),
and Isothrum depends on none ([what Isothrum is, and what it is not](docs/explanation/boundaries.md)).

Proven by real runs: `docker` (macOS, Linux, Windows); `vagrant` on VirtualBox (macOS), libvirt
(Linux) and Hyper-V (Windows); `docker-vm` on VirtualBox (macOS); `proxmox` and
`vagrant/vmware_esxi` from macOS. The cloud targets are generated and validated, not yet run for
real.

## Quickstart

```sh
cd my-env                          # a folder with an isoloom.yml
isoloom generate --target docker   # writes .isoloom/docker/run.json and the Compose files
isothrum up                        # start, wait until ready, print the addresses
isothrum check                     # run the checks: exit 0 when all pass
isothrum down                      # remove everything the run created
```

Add `--json` for machine-readable output: an operation's events, one JSON object per line
(`status`, `console`, `screen` and `key` print one JSON document). Secrets come from the environment only:
`--secret-from-env NAME[=VAR]`. The [tutorial](docs/tutorials/first-run.md) walks through a
first run.

## Documentation

[docs/README.md](docs/README.md) is the index: a tutorial, how-to guides (CI, proxies and CAs,
Hyper-V without admin rights, offline use, cost controls, SSH keys), the reference (commands, exit
codes, environment variables, events, state) and the architecture and security model.

## Crates

| Crate | What it is |
|---|---|
| [`isothrum-cli`](crates/isothrum-cli) | The `isothrum` command line. |
| [`isothrum-runner`](crates/isothrum-runner) | The engine: lifecycle, state and locks, drivers, secrets, checks, events. |
| [`isothrum-machine`](crates/isothrum-machine) | Which Vagrant providers (hypervisors) a computer can drive. |

All crates share one version. `xtask` is not published.

## Stability

Isothrum follows Semantic Versioning. Within a major version, the Rust API of the published crates,
the CLI's commands, flags and exit codes, the JSON event and status schemas, the error and warning
codes and the `record.json` format don't break; CI checks the Rust API with `cargo semver-checks`.
The details, the run manifest versions each release reads, the MSRV policy and the supported
releases are in [Policies](docs/policies.md).

## Security, support and contributing

- Report a vulnerability privately: [SECURITY.md](SECURITY.md). The security model:
  [docs/explanation/security-model.md](docs/explanation/security-model.md).
- Questions and bugs: [SUPPORT.md](SUPPORT.md).
- Contributions: [CONTRIBUTING.md](CONTRIBUTING.md) and the [Code of Conduct](CODE_OF_CONDUCT.md).

## Licence

Apache-2.0: see [LICENSE](LICENSE) and [NOTICE](NOTICE).
