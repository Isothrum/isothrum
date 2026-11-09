# isothrum-cli

`isothrum`, the command line of the [Isothrum runner](https://crates.io/crates/isothrum-runner):
bring up, check, park, resume, update and tear down the environments
[Isoloom](https://github.com/isoloom/isoloom) generates, with Docker Compose, Vagrant or
Terraform. Add `--json` for one JSON event per line on standard output, for CI and scripts.

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
# In a folder where Isoloom generated the docker target (.isoloom/docker/run.json):
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
