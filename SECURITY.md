# Security policy

## Supported versions

| Version | Security fixes |
|---|---|
| Latest release of the current major | Yes |
| Last release of the previous major | For three months after the next major's first release |
| Anything older | No: upgrade |

Fixes ship as new releases of the crates and the `isothrum` binaries. See
[Policies](docs/policies.md#supported-releases).

## Reporting a vulnerability

Please **don't open a public issue**. Report it privately through GitHub:
[**Report a vulnerability**](https://github.com/Isothrum/isothrum/security/advisories/new)
(the repository's *Security* tab, *Report a vulnerability*).

Include what you can: the affected version (`isothrum --version`), the platform, the steps or a
proof of concept, and the impact you see. Remove real secrets from logs (Isothrum masks the values
it knows as `***`, but a tool's own output may carry others).

## What happens next

| Step | Target |
|---|---|
| Acknowledgement of your report | 3 business days |
| First assessment (confirmed or not, severity) | 10 business days |
| Fix released, for a confirmed critical or high severity issue | 30 days |
| Fix released, other confirmed issues | the next release, or 90 days |

We coordinate the disclosure with you: a GitHub security advisory (with a CVE when it applies)
is published with the fixed release, crediting you unless you prefer otherwise. We'll keep you
informed if a target can't be met.

## Scope

In scope: the code in this repository (the crates, the `isothrum` binary, the release workflow).
Vulnerabilities in the tools Isothrum runs (Docker, Vagrant, Terraform and its providers, OpenSSH)
or in the files Isoloom generates belong to those projects; tell us too when Isothrum makes one
worse.

## Security model, briefly

The full model: [docs/explanation/security-model.md](docs/explanation/security-model.md).

- **Runs tools as the user.** No daemon, no service, no `setuid`: Isothrum starts Docker, Vagrant,
  Terraform and `ssh` as the user who runs it. A run manifest is trusted input: it names the
  commands that run.
- **Elevation only for Hyper-V on Windows.** An operation on a Hyper-V run, by an account that is
  neither elevated nor in *Hyper-V Administrators*, starts one elevated helper behind one UAC
  prompt; nothing else elevates.
- **Secrets are masked and never on a command line.** Values come from the environment
  (`--secret-from-env`), reach tools through their environment, a Terraform variable, an
  owner-only file or SSH standard input, and are shown as `***` in events, logs, errors and
  results.
- **State is private.** The home's folders are `0700` and its files `0600` on Unix; on Windows a
  home outside the user's profile gets an owner-only DACL. No secret value is stored, except in
  five owner-only files in the run's state folder, each kept only while the run needs it: a
  `state_file` delivery's file, a guest delivery's input (`guest-<machine>.in`), an elevated
  Hyper-V session's request variables (`env-<n>.json`), the graphical consoles' passwords the
  runner chose for a VMware run (`consoles.json`) and a Proxmox console request's credentials
  (`proxmox-console.curl`, only while `curl` runs)
  ([Security model](docs/explanation/security-model.md)).
- **Signed releases.** Binaries come with checksums, a Sigstore signature and build provenance
  ([verify a release](docs/how-to/verify-a-release.md)); the crates forbid `unsafe` code.
