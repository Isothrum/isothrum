# Isothrum

[![Gate](https://github.com/Isothrum/isothrum/actions/workflows/gate.yml/badge.svg?branch=main)](https://github.com/Isothrum/isothrum/actions/workflows/gate.yml)
[![crates.io](https://img.shields.io/crates/v/isothrum-cli.svg)](https://crates.io/crates/isothrum-cli)
[![Licence: Apache-2.0](https://img.shields.io/badge/licence-Apache--2.0-blue.svg)](LICENSE)

Isothrum runs Docker Compose projects, Vagrant projects and Terraform modules with one set of
commands: `up`, `status`, `exec`, `park`, `resume`, `update` and `down` work the same whatever the
folder holds. It drives each project through its own tool and adds what the tools don't share: a
record of every run, host ports that stay put across restarts, one JSON event stream, stable error
codes, lifetimes, and secrets that never reach a command line.

A project needs nothing of Isothrum's. A folder with a `compose.yaml`, a `Vagrantfile` or `*.tf`
files runs as it is, and its files are never rewritten: `docker compose up`, `vagrant up` or
`terraform apply` still work on it.

It is for anyone who wants reproducible test, demo or training environments on a laptop, a server
or a cloud account, and for CI pipelines that need one for the length of a job. It is a command
line (`isothrum`), a runner service, and Rust libraries to embed in your own program.

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

The tool of each project you run ([supported tool versions](docs/reference/tool-versions.md)):

- Docker with Compose 2.17.0 or later, for Compose projects;
- Vagrant 2.4.0 or later and a hypervisor (VirtualBox, VMware, Hyper-V, Parallels, libvirt, QEMU,
  UTM), for Vagrant projects;
- Terraform 1.6.0 or later, for Terraform modules;
- an OpenSSH client, for a command or a console in a Terraform module's VM.

Isothrum checks each one before it starts anything.

Proven by real runs: Compose on macOS, Linux and Windows; Vagrant on VirtualBox (macOS), libvirt
(Linux), Hyper-V (Windows) and VMware ESXi (from macOS); a Terraform module on Proxmox VE (from
macOS). Terraform modules for the clouds are validated, not yet run for real.

## Quickstart

```sh
cd examples/compose-web-cache    # any folder with a compose.yaml, a Vagrantfile or *.tf files
isothrum up                      # start it, and wait until it is ready
isothrum status                  # its state, its machines and the ports they publish
isothrum exec cache -- redis-cli ping
isothrum park                    # stop it, keeping what it made
isothrum resume                  # start it again, on the same ports
isothrum down                    # remove everything it made
```

[Getting started](docs/tutorials/getting-started.md) walks through it from the install. The
[`examples/`](examples) folder has a Compose project, a Vagrant project and a Terraform module
that needs no cloud, each with a README.

Add `--json` for machine-readable output: an operation's events, one JSON object per line
(`status`, `console` and `key` print one JSON document) ([the `--json` output](docs/reference/events.md)).
Values reach the project with `--env NAME=VALUE`, secrets from the environment only with
`--secret-from-env NAME[=VAR]`.

Isothrum can also run the projects Isoloom generates, like any other
([what Isothrum is, and what it is not](docs/explanation/boundaries.md)).

## Documentation

[docs/README.md](docs/README.md) is the index: getting started, how-to guides (CI, proxies and
CAs, Hyper-V without admin rights, offline use, cost controls, SSH keys), the reference (commands,
exit codes, environment variables, events, state) and the architecture and security model.

## Crates

| Crate | What it is |
|---|---|
| [`isothrum-cli`](crates/isothrum-cli) | The `isothrum` command line. |
| [`isothrum-runner`](crates/isothrum-runner) | The engine: lifecycle, state and locks, the format drivers, secrets, events. |
| [`isothrum-service`](crates/isothrum-service) | The runner service: one per home, queueing each run's operations and streaming their events to its clients. |
| [`isothrum-client`](crates/isothrum-client) | The service's client: connect to (or start) a home's service and follow its events. |
| [`isothrum-machine`](crates/isothrum-machine) | Which Vagrant providers (hypervisors) a computer can drive. |

All crates share one version. `xtask` is not published.

## Stability

Isothrum follows Semantic Versioning. Within a major version, the Rust API of the published crates,
the CLI's commands, flags and exit codes, the JSON event and status schemas, the error and warning
codes and the `record.json` format don't break; CI checks the Rust API with `cargo semver-checks`.
The details, the MSRV policy and the supported releases are in [Policies](docs/policies.md).

## Security, support and contributing

- Report a vulnerability privately: [SECURITY.md](SECURITY.md). The security model:
  [docs/explanation/security-model.md](docs/explanation/security-model.md).
- Questions and bugs: [SUPPORT.md](SUPPORT.md).
- Contributions: [CONTRIBUTING.md](CONTRIBUTING.md) and the [Code of Conduct](CODE_OF_CONDUCT.md).

## Licence

Apache-2.0: see [LICENSE](LICENSE) and [NOTICE](NOTICE).
