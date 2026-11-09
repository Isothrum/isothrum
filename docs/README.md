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
- [State layout](reference/state.md): the home, `record.json`
- [Supported tool versions](reference/tool-versions.md): Compose, Vagrant, Terraform, OpenTofu
- [GNS3 feature coverage](reference/gns3-coverage.md) of the compiler and the runner
- API: [isothrum-runner](https://docs.rs/isothrum-runner),
  [isothrum-compiler](https://docs.rs/isothrum-compiler),
  [isothrum-machine](https://docs.rs/isothrum-machine),
  [isothrum-wasm](https://docs.rs/isothrum-wasm)

## Explanation

- [Architecture](explanation/architecture.md): Isoloom, the run manifest, the runner's parts
- [Security model](explanation/security-model.md): privileges, secrets, state on disk, SSH

## Project

- [Policies](policies.md): what is stable, run manifest versions, MSRV, supported releases
- [Migrating to 2.0](migrating-to-2.0.md) and [to 1.0](migrating-to-1.0.md)
- [Changelog](../CHANGELOG.md), [Contributing](../CONTRIBUTING.md), [Security](../SECURITY.md),
  [Support](../SUPPORT.md), [Publishing a release](../PUBLISHING.md)
