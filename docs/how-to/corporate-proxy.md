# Run behind a corporate proxy and a custom CA

Isothrum itself makes no HTTP request: the tools it runs do (Docker pulling images, Vagrant
downloading boxes, Terraform downloading providers, the cloud command lines). Each tool reads its
own proxy and certificate settings, so configure the tools, and run `isothrum` from an environment
that carries those settings. Every variable Isothrum reads or passes on is listed in
[Environment variables](../reference/environment.md).

## The environment

The tools inherit the environment `isothrum` runs in. Set the usual variables in the shell, the
service or the CI job that runs it:

```sh
export HTTPS_PROXY=http://proxy.example.com:3128
export HTTP_PROXY=http://proxy.example.com:3128
export NO_PROXY=localhost,127.0.0.1,.example.com
```

Some tools read only the lowercase names (`https_proxy`, `http_proxy`, `no_proxy`); setting both
is common. Keep `127.0.0.1` and `localhost` in `NO_PROXY`: a run's published ports are on the
loopback.

Terraform doesn't inherit everything: it never gets the shell's cloud settings
(`AWS_*`, `ARM_*`, `GOOGLE_*`, `TF_VAR_*`, ...; see the
[security model](../explanation/security-model.md#secrets)). Proxy and CA variables are not among
them and reach it.

## Each tool

- **Docker.** Images are pulled by the Docker engine, not the `docker` command, so the engine needs
  the proxy: Docker Desktop's proxy settings, or the engine's own configuration on Linux. The
  engine trusts registries' certificates from the system store, or from
  `/etc/docker/certs.d/<registry>/ca.crt` on Linux. For builds and containers that need the proxy
  themselves, the Docker command line's `proxies` setting in `~/.docker/config.json` passes it to
  them.
- **Vagrant.** Box downloads honour the proxy variables. Install the providers' plugins
  (`vagrant plugin install`) through the same proxy before the first run.
- **Terraform** (the host binary). Registry and provider downloads honour `HTTPS_PROXY` and
  `NO_PROXY`. For a private CA: on Linux, the system store, or `SSL_CERT_FILE` / `SSL_CERT_DIR`;
  on macOS and Windows, the system's trust store.
- **Terraform from its container image** (Proxmox targets on macOS). The container doesn't get
  the host's environment as a whole: Isothrum hands it the proxy variables, the CA bundle variables
  (`SSL_CERT_FILE`, `REQUESTS_CA_BUNDLE`, `CURL_CA_BUNDLE`) and `TF_CLI_CONFIG_FILE` by name, and
  mounts the files they name read-only. Point `SSL_CERT_FILE` at a bundle that includes your CA.
- **Hyper-V on Windows.** The elevated helper doesn't inherit the environment either; it is handed
  the same proxy and CA bundle variables.
- **Cloud command lines** (`aws`, `az`, `gcloud`, ...), which `isothrum reap` runs, honour the proxy
  variables and their own CA settings (`AWS_CA_BUNDLE`, `REQUESTS_CA_BUNDLE`, ...).

## SSH

SSH to the machines doesn't go through an HTTP proxy. The runner's SSH ignores your
`~/.ssh/config`, so a `ProxyCommand` there doesn't apply; when the machines are behind a bastion,
give it with `--ssh-jump user@host` at `up` (it is recorded for the run's later operations). See
[Manage the SSH key](ssh-keys.md).

## Check it

```sh
isothrum up --json | jq -c 'select(.event == "error")'
```

A proxy or certificate problem shows as the failing step's `log` lines and its `error` event
(`tool_failed`), with the tool's own message.
