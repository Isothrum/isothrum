# isothrum-cli

`isothrum`, the command line of the [Isothrum runner](https://crates.io/crates/isothrum-runner):
bring up, check, park, resume, update and tear down a Compose project, a Vagrantfile or a
Terraform module, whoever wrote it. Add `--json` for one JSON event per line on standard output,
for CI and scripts.

## Install

Release binaries (Linux x86_64/aarch64, macOS, Windows) are on the
[GitHub releases](https://github.com/Isothrum/isothrum/releases) page, with checksums, a Sigstore
signature and build provenance. With [cargo-binstall](https://github.com/cargo-bins/cargo-binstall)
they install in one command:

```sh
cargo binstall isothrum-cli
```

Or build from source:

```sh
cargo install isothrum-cli --locked
```

## Example

```sh
# In a Compose project's folder:
isothrum up
isothrum check
isothrum down
```

`isothrum --help` lists every command.

## Feature flags

None.

## Documentation

- Getting started, how-tos and reference: <https://github.com/Isothrum/isothrum/blob/main/docs/README.md>
- Verifying a download: <https://github.com/Isothrum/isothrum/blob/main/docs/how-to/verify-a-release.md>

Minimum Rust: 1.90. Licence: Apache-2.0.
