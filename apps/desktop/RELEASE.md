# Releasing the Isothrum desktop app

The desktop app ships through `.github/workflows/release.yml`: push a version tag and the workflow
builds the installers for Linux, macOS and Windows on a **draft** GitHub release, with a
`latest.json` the app's auto-updater reads. You review the draft and publish it.

## Trigger

- **Tag:** `git tag v0.2.0 && git push origin v0.2.0`. The matrix builds every platform and
  attaches the installers (and `latest.json`) to a draft release named after the tag.
- **Manual:** run the *Release* workflow from the Actions tab (`workflow_dispatch`). It tags the
  draft from the version in `src-tauri/tauri.conf.json` (`v<version>`); no git tag is pushed.

Then open the draft release, check the assets, and publish it. Publishing is a human step — the
workflow never publishes on its own, and no release is cut by building.

## What a build produces

| Platform | Installers | Runner |
| --- | --- | --- |
| Linux | `.deb`, `.AppImage` | `ubuntu-22.04` |
| macOS | `.dmg` (universal: aarch64 + x86_64) | `macos-latest` |
| Windows | `.msi` | `windows-latest` |

`latest.json` plus the per-platform update bundles and their `.sig` signatures are attached when
the updater key is set (see below). The updater endpoint is baked into `tauri.conf.json`
(`https://github.com/Isothrum/isothrum/releases/latest/download/latest.json`) and resolves once the
repository is public and the release is published (not a draft).

## Secrets

Set these under **Settings → Secrets and variables → Actions**. None is printed or committed.

### Required for a working release

| Secret | Purpose |
| --- | --- |
| `TAURI_SIGNING_PRIVATE_KEY` | The updater signing key (minisign). Signs each update bundle so the installed app accepts the update. Its **public** half is in `tauri.conf.json` under `plugins.updater.pubkey` and the two must match. |
| `TAURI_SIGNING_PRIVATE_KEY_PASSWORD` | The password set when the key was generated (empty string if the key has none — still set the secret). |
| `WEBSITE_REPO_TOKEN` | A read-only token for `isoloom/isothrum-website`, from which the bundled editor is built. Needed only while that repository is private; once public, the workflow's default `github.token` is enough. |

If `TAURI_SIGNING_PRIVATE_KEY` is absent the build still succeeds, but it produces **no** update
signatures or `latest.json`, so the auto-updater cannot verify an update. A build like that is fine
for manual testing of the installers, not for a release users auto-update to.

### Optional — macOS signing and notarization

Without these, macOS builds are **unsigned**: they install and run for testing, but Gatekeeper
warns on first open and the updater is not notarized.

| Secret | Purpose |
| --- | --- |
| `APPLE_CERTIFICATE` | Developer ID Application certificate, exported as a `.p12` and base64-encoded. |
| `APPLE_CERTIFICATE_PASSWORD` | The `.p12` export password. |
| `APPLE_SIGNING_IDENTITY` | The certificate's name, e.g. `Developer ID Application: Your Name (TEAMID)`. |
| `APPLE_ID` | The Apple ID used for notarization. |
| `APPLE_PASSWORD` | An app-specific password for that Apple ID. |
| `APPLE_TEAM_ID` | The Apple Developer team id. |

### Optional — Windows signing

Without these, Windows `.msi` builds are **unsigned** (SmartScreen warns on install).

| Secret | Purpose |
| --- | --- |
| `WINDOWS_CERTIFICATE` | Code-signing certificate as a `.pfx`, base64-encoded. |
| `WINDOWS_CERTIFICATE_PASSWORD` | The `.pfx` password. |

When both are set the workflow imports the certificate and passes its thumbprint to Tauri with a
DigiCert timestamp; when either is absent, signing is skipped.

## Generating the updater keypair

Once, on a trusted machine (the private key never enters the repository):

```
cargo tauri signer generate -w ~/.isothrum/updater.key
# or, without a cargo-tauri install, from apps/desktop:
npm run tauri -- signer generate -w ~/.isothrum/updater.key
```

This prints a **public key** and writes the **private key** to `~/.isothrum/updater.key`
(protected by the password you choose). Then:

1. Put the public key into `src-tauri/tauri.conf.json` at `plugins.updater.pubkey` (the value there
   must be this public key, or the installed app rejects every update).
2. Set `TAURI_SIGNING_PRIVATE_KEY` to the contents of `~/.isothrum/updater.key` and
   `TAURI_SIGNING_PRIVATE_KEY_PASSWORD` to its password.
3. Back up the private key and password offline. If the key is lost, installed apps can no longer
   auto-update and users must reinstall from a release signed with a new key.

## Building locally (no release)

```
npm install
npm run build        # runs `npm run editor`, then bundles .deb/.AppImage/.dmg/.msi for this OS
```

The editor is built from the website checkout. By default `npm run editor` looks for
`../../../isothrum-website`; point it elsewhere with `ISOTHRUM_WEBSITE=/path/to/isothrum-website`.
A local bundle is unsigned unless you export the signing variables above into your shell. To skip
bundling (binary + editor only, when you lack signing secrets), run `cargo tauri build --no-bundle`
from `src-tauri`.
