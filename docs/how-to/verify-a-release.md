# Verify a release

Each [GitHub release](https://github.com/Isothrum/isothrum/releases) carries, next to the
archives:

| File | What it is |
|---|---|
| `isothrum-<version>-<target>.tar.gz` / `.zip` | The binary, `LICENSE`, `NOTICE`, the README and `THIRD-PARTY-LICENSES.md`. |
| `isothrum-<version>.cdx.json` | The CycloneDX SBOM of the `isothrum` binary's dependencies. |
| `SHA256SUMS` | The SHA-256 of every file above. |
| `SHA256SUMS.sigstore.json` | A keyless Sigstore signature of `SHA256SUMS` (a cosign bundle). |

Every file listed in `SHA256SUMS` also has a build provenance attestation on GitHub. The release
workflow is [`.github/workflows/release.yml`](../../.github/workflows/release.yml); it runs only
on a version tag, after the test gate.

Targets: `x86_64-unknown-linux-musl`, `aarch64-unknown-linux-musl` (static Linux binaries),
`x86_64-apple-darwin`, `aarch64-apple-darwin` and `x86_64-pc-windows-msvc`.

## Download

```sh
VERSION=2.1.4                      # the release you want
TARGET=x86_64-unknown-linux-musl   # your platform
BASE="https://github.com/Isothrum/isothrum/releases/download/v${VERSION}"
curl -fsSLO "${BASE}/isothrum-${VERSION}-${TARGET}.tar.gz"
curl -fsSLO "${BASE}/SHA256SUMS"
curl -fsSLO "${BASE}/SHA256SUMS.sigstore.json"
```

## 1. Check the signature of SHA256SUMS

With a recent [cosign](https://docs.sigstore.dev/cosign/system_config/installation/) (the
release workflow signs with cosign 3):

```sh
cosign verify-blob SHA256SUMS \
  --bundle SHA256SUMS.sigstore.json \
  --certificate-identity "https://github.com/Isothrum/isothrum/.github/workflows/release.yml@refs/tags/v${VERSION}" \
  --certificate-oidc-issuer https://token.actions.githubusercontent.com
```

`Verified OK` means `SHA256SUMS` was signed by the release workflow of `Isothrum/isothrum`, running
for that tag, and the signature is in Sigstore's public transparency log.

## 2. Check the archive against SHA256SUMS

```sh
sha256sum --check --ignore-missing SHA256SUMS            # Linux
shasum -a 256 --check --ignore-missing SHA256SUMS        # macOS
```

On Windows (PowerShell), compare the hash by hand:

```powershell
(Get-FileHash .\isothrum-$env:VERSION-x86_64-pc-windows-msvc.zip -Algorithm SHA256).Hash.ToLower()
Select-String "x86_64-pc-windows-msvc.zip" .\SHA256SUMS
```

## 3. Check the build provenance

With the [GitHub CLI](https://cli.github.com/):

```sh
gh attestation verify "isothrum-${VERSION}-${TARGET}.tar.gz" \
  --repo Isothrum/isothrum \
  --signer-workflow Isothrum/isothrum/.github/workflows/release.yml
```

This shows the file was built by that workflow from the tagged commit of the repository.

## 4. Install

```sh
tar -xzf "isothrum-${VERSION}-${TARGET}.tar.gz"
install "isothrum-${VERSION}-${TARGET}/isothrum" ~/.local/bin/
isothrum --version
```

`cargo binstall isothrum-cli` downloads the same archives but checks neither the signature nor
the provenance: verify as above when you need to.
