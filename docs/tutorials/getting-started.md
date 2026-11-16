# Getting started

In this tutorial you install Isothrum, bring up a small Compose project, read its status, run a
command in it, park it, resume it and remove it. It takes about ten minutes and needs Docker.

Isothrum runs a folder as the tool it was written for: a Compose project (`compose.yaml`) with
`docker compose`, a Vagrant project (`Vagrantfile`) with `vagrant`, a Terraform root module
(`*.tf`) with `terraform`. The folder needs nothing of Isothrum's: the same commands work on any
of the three, and the files stay as they are.

## Before you start

Docker running, with Compose 2.17.0 or later:

```sh
docker compose version
```

## 1. Install Isothrum

Pick one:

- **A release binary** for Linux, macOS or Windows from the
  [releases page](https://github.com/Isothrum/isothrum/releases), with its checksum, signature and
  provenance ([verify a download](../how-to/verify-a-release.md)). Put `isothrum` on your `PATH`.
- **With cargo-binstall**, which fetches that binary for you: `cargo binstall isothrum-cli`.
- **From source**, with Rust 1.90 or later: `cargo install isothrum-cli --locked`.

Check it answers:

```sh
isothrum --version
```

## 2. Write a project

Make a folder with one file, `compose.yaml`: a web server published on this computer's loopback,
and a cache that publishes nothing. It is the repository's
[`examples/compose-web-cache`](../../examples/compose-web-cache); copying that folder does the
same.

```sh
mkdir ~/hello && cd ~/hello
```

```yaml
# compose.yaml
services:
  web:
    image: busybox:1.36
    init: true
    command: ["sh", "-c", "mkdir -p /www && echo 'hello from web' > /www/index.html && exec httpd -f -p 80 -h /www"]
    ports: ["127.0.0.1::80"]
    depends_on:
      cache:
        condition: service_healthy
    healthcheck:
      test: ["CMD", "wget", "-q", "-O", "/dev/null", "http://127.0.0.1/"]
      interval: 2s
      retries: 15

  cache:
    image: redis:7-alpine
    healthcheck:
      test: ["CMD", "redis-cli", "ping"]
      interval: 2s
      retries: 15
```

Nothing in it is Isothrum's: `docker compose up` would run it too.

## 3. Bring it up

```sh
isothrum up
```

Isothrum finds `compose.yaml` in the folder, checks that Docker and Compose answer, picks a free
host port for `web`'s published port 80, starts both services and waits until their health checks
pass. Compose's own lines come first, then:

```text
The run is up.
```

The run is named after the folder, `hello`. Isothrum records it under `~/.isothrum/runs/hello/`,
its home (`--home` or `ISOTHRUM_HOME` to put it elsewhere). Nothing is written into `~/hello`.

## 4. Read its status

```sh
isothrum status
```

```text
hello: ready
  cache                    running
  web                      running      57608->80/tcp
```

The run is `ready`, and `web`'s port 80 is published at port 57608 of `127.0.0.1`. Your port will
differ. Open it:

```sh
curl http://127.0.0.1:57608/
```

```text
hello from web
```

A program reads the same with `--json`, as one JSON document:

```sh
isothrum status --json
```

```json
{"state":"ready","live":true,"desired":"running","provider":null,"machines":[{"name":"cache","state":"running","published":[]},{"name":"web","state":"running","published":[{"port":80,"protocol":"tcp","host_port":57608}]}],"outputs":[]}
```

## 5. Run a command in it

```sh
isothrum exec cache -- redis-cli ping
```

```text
PONG
```

`exec` runs the command in the `cache` service, with no terminal, prints what it prints and exits
with its exit status, so it fits a script. For an interactive shell, `isothrum console web` prints
the command that opens one.

## 6. Park it and resume it

`park` stops the run and keeps what it made: the containers, their network and their volumes.

```sh
isothrum park
isothrum status
```

```text
hello: parked
  cache                    exited(0)
  web                      exited(143)
```

`resume` starts it again:

```sh
isothrum resume
```

```text
The run is up again.
```

`web` answers at the same port as before. Isothrum keeps the host port it picked at the first `up`
until the run is removed, so an address you wrote down still works.

## 7. Clean up

```sh
isothrum down
isothrum status
```

```text
hello: absent
  cache                    absent
  web                      absent
```

`down` removes everything the run created: the containers, the network and the volumes. The
images stay in Docker, as `docker compose down` leaves them.

## What you did

You ran the whole life of a run: `up`, `status`, `exec`, `park`, `resume` and `down`. They are the
same for a Vagrant project and a Terraform module:

- [`examples/vagrant-one-vm`](../../examples/vagrant-one-vm): one VM. `up` takes `--provider`
  (`isothrum up --provider virtualbox`) the first time.
- [`examples/terraform-local`](../../examples/terraform-local): a module that needs no cloud;
  `status` prints its outputs.

Next:

- Give a project values: `--env NAME=VALUE` reaches Compose's `${NAME}`, a Vagrantfile's
  `ENV["NAME"]` and a Terraform variable `NAME`; `--secret-from-env NAME` gives a secret from the
  environment, never on the command line.
- Run the same project twice side by side: `--name` on every command.
- Read the events of any command as JSON lines with `--json`: [the `--json`
  output](../reference/events.md).
- Run it in a pipeline: [Use isothrum in CI](../how-to/ci.md).
- Every command and flag: [the command line](../reference/cli.md).
