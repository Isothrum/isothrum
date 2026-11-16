# Isothrum documentation

Organised by what you need: learning, doing a task, looking something up, or understanding why.

## Tutorial

- [Your first run](tutorials/first-run.md): `up`, `check` and `down` of a Docker Compose example.

## How-to guides

- [Use isothrum in CI](how-to/ci.md) (GitHub Actions, GitLab CI), and keep one home from `up` to
  `down`
- [Run behind a corporate proxy and a custom CA](how-to/corporate-proxy.md)
- [Use Hyper-V without administrator rights](how-to/hyper-v-without-admin.md)
- [Move the home](how-to/move-the-home.md)
- [Run offline or air-gapped](how-to/offline.md)
- [Control costs](how-to/cost-controls.md): lifetimes, expiry and `reap`
- [Manage the SSH key](how-to/ssh-keys.md)
- [Verify a release](how-to/verify-a-release.md): checksums, signature, provenance

## Reference

- [Command line](reference/cli.md): every command and option
- [Exit codes and error codes](reference/exit-codes.md)
- [Environment variables](reference/environment.md)
- [Event stream](reference/events.md) and the `status --json` schema
- [State layout](reference/state.md): the home, `state.db`
- [Runner service](reference/service.md): `isothrum serve`, its files, its protocol, replacing a service of another release
- [Supported tool versions](reference/tool-versions.md): Compose, Vagrant, Terraform, OpenTofu
- [Terraform modules](reference/terraform.md): what any module gets, and the optional outputs and values that add screens and Proxmox reads
- [Vagrant projects](reference/vagrant.md): the optional variables that keep a forwarded port's host port across boots
- [GNS3 feature coverage](reference/gns3-coverage.md) of the runner
- API: [isothrum-runner](https://docs.rs/isothrum-runner),
  [isothrum-machine](https://docs.rs/isothrum-machine)

## Explanation

- [Architecture](explanation/architecture.md): what the runner reads and adds, the crates, the runner's parts
- [What Isothrum is, and what it is not](explanation/boundaries.md): a general-purpose runtime,
  independent of Isoloom and of any application built on it
- [Security model](explanation/security-model.md): privileges, secrets, state on disk, SSH

## Project

- [Policies](policies.md): what is stable, run manifest versions, MSRV, supported releases
- [Upgrading from 2.x](reference/state.md#upgrading-from-2x); [Migrating to 2.0](migrating-to-2.0.md) and [to 1.0](migrating-to-1.0.md)
- [Changelog](../CHANGELOG.md), [Contributing](../CONTRIBUTING.md), [Security](../SECURITY.md),
  [Support](../SUPPORT.md), [Publishing a release](../PUBLISHING.md)
