# Environment variables

Every environment variable `isothrum` (and the `isothrum-runner` crate an app embeds) reads, and
every one it sets, passes on or holds back for the tools it runs (Docker Compose, Vagrant,
Terraform, `ssh`). A value given to a run (a param, a secret) never goes on a tool's command line:
it reaches the tool through its environment, as the run manifest's deliveries say.

## What `isothrum` reads

| Variable | What it does |
| -------- | ------------ |
| `ISOTHRUM_HOME` | The runner's home, where runs are recorded (`--home` wins; default `~/.isothrum`). Created owner-only: on Unix each folder under it is `0700` and each file `0600`; on Windows a home outside the user's profile gets a protected DACL (the account, SYSTEM, Administrators) that everything under it inherits. |
| `ISOTHRUM_LOG` | Turns on the runner's tracing on standard error: a level (`info`, `debug`, `trace`) or filter directives (`isothrum_runner=debug,warn`, as `tracing-subscriber`'s `EnvFilter` reads them). Unset: no tracing. The traces name programs and arguments, never a value. |
| `ISOTHRUM_LOG_FORMAT` | `text` (the default) or `json`: one JSON object per trace line, for a log collector. Read only with `ISOTHRUM_LOG`. |
| The variable each `--secret-from-env NAME[=VAR]` names | A secret or input slot's value (`VAR`, else the slot's own `NAME`). |
| `HOME` (`USERPROFILE` on Windows) | Where the default home (`~/.isothrum`) and the Docker CLI's config (`~/.docker`) are. |
| `DOCKER_CONFIG` | Where the Docker CLI's `config.json` is (default `~/.docker`): when it names a current context, `docker` follows it (see `DOCKER_HOST` below). |
| `PATH` | Where the tools are found, and the `PATH` the macOS ESXi relay's `ovftool` wrapper is put in front of. |
| `TF_PLUGIN_CACHE_DIR` | Terraform's plugin cache. When it is set, every run uses it as it is (the runner neither creates nor changes it); unset, the runs share `<home>/terraform-plugins`. |
| `HTTP_PROXY`, `HTTPS_PROXY`, `NO_PROXY` (either case), `SSL_CERT_FILE`, `REQUESTS_CA_BUNDLE`, `CURL_CA_BUNDLE` | How this host reaches the network. Tools started directly inherit them; the ones started elsewhere are handed them (see below). |
| `TF_CLI_CONFIG_FILE` | Terraform's CLI configuration (a provider mirror, credentials): inherited, and handed to a containerised Terraform (below). |
| `no_proxy` (else `NO_PROXY`) | On macOS, kept in the environment of a Vagrant run on an ESXi host (below). |
| `USERPROFILE` (Windows) | Whether the home is inside the user's profile (no DACL of its own needed), and the `VAGRANT_HOME` an elevated Hyper-V session gets. |
| `SystemRoot` (Windows, `isothrum-machine`) | Where Windows' own tools are. |

A value that isn't UTF-8 is never taken for an unset variable. Where it names a path
(`ISOTHRUM_HOME`, `DOCKER_CONFIG`, `PATH`, `TF_PLUGIN_CACHE_DIR`) it is read as it is. Where it is
text, it is refused by name: a secret's variable is a usage error (exit 2), and an `ISOTHRUM_LOG`
or `ISOTHRUM_LOG_FORMAT` a warning, with no tracing. A `PATH` is handed on as text: one that isn't
UTF-8 is left out of an elevated session's variables (and traced), and fails the macOS relay's
set-up when its `ovftool` wrapper must go first on it.

## What the tools get

Each tool inherits the environment `isothrum` runs in, with these changes:

| Variable | For | What happens |
| -------- | --- | ------------ |
| A run's delivered values (`env`, `TF_VAR_<var>`) | the steps the manifest names | Set for those steps only; a secret's value is masked (`***`) in every event and the run's log. |
| `TF_PLUGIN_CACHE_DIR` | `terraform` | Set to the cache above. |
| `TF_VAR_<expiry var>` | `terraform`, a run with an expiry | When the run stops itself, in Unix seconds. |
| `AWS_*`, `ARM_*`, `AZURE_*`, `GOOGLE_*`, `CLOUDSDK_*`, `GCLOUD_*`, `DIGITALOCEAN_*`, `LINODE_*`, `OCI_*`, `TF_VAR_*`, `TF_CLI_ARGS*` | `terraform`, `tofu` | Not inherited: a destroy goes where the run was applied, not where the shell points now. The manifest's params and connection slots are the only cloud settings Terraform gets. |
| `DOCKER_HOST`, `DOCKER_CONTEXT` | `docker` | Not inherited when the Docker CLI's config names a current context of the user's own: a stale socket (another engine's) would send the run where the user never looks. |
| `VAGRANT_DOTFILE_PATH` | `vagrant` | The run's Vagrant state, in the run's state folder (also in the command `isothrum console` prints). |
| `ISOLOOM_HOSTPORTS` | `vagrant`, a local VM run | The host ports pinned for its forwarded ports, as JSON. |

### Tools started elsewhere

- **An elevated Hyper-V session (Windows).** The elevated broker inherits nothing. It is handed
  `PATH`, every `VAGRANT_*`, `VAGRANT_HOME` (default `%USERPROFILE%\.vagrant.d`, this account's
  boxes whoever approved the prompt), the proxy and CA bundle variables above, and each call's own
  values, through an owner-only file in the run's state that the broker deletes as soon as it has
  read it.
- **A containerised Terraform (macOS, for a Proxmox server).** The container gets each variable
  by name (`docker run -e NAME`, never a value on the command line): the step's own, the proxy
  and CA bundle variables, `TF_CLI_CONFIG_FILE` and `TF_PLUGIN_CACHE_DIR`; the CA bundles and the
  CLI configuration, when they are files, are mounted read-only at their own paths, and the plugin
  cache at its own.
- **Vagrant on an ESXi host (macOS).** macOS keeps third-party tools off the local network, so
  the run goes through loopback relays: `ESXI_HOSTNAME` and `ESXI_HOSTPORT` point at the SSH
  relay, `ISOLOOM_SSH_PROXY_COMMAND` is `/usr/bin/nc %h %p`, `http_proxy` is the relay's WinRM
  proxy (the one override: WinRM reaches the guests only through it; `https_proxy` is left as it
  is), `no_proxy` is the user's own plus `localhost,127.0.0.1`, and `ovftool` runs through a
  wrapper (`ISOTHRUM_REAL_OVFTOOL`, `ISOTHRUM_OVFTOOL_PORT`). A `no_proxy` entry covering the
  run's guest network sends WinRM around the relay, where macOS refuses it.
