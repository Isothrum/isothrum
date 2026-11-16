# A Compose project: a web server and a cache

[`compose.yaml`](compose.yaml) is an ordinary Compose file with two services: `web`, a BusyBox web
server published on this computer's loopback, and `cache`, a Redis server that publishes nothing.
`docker compose up` runs it as it is; Isothrum runs it too, with nothing added to the folder.

Needs Docker with Compose 2.17.0 or later (`docker compose version`).

## Bring it up

```sh
cd examples/compose-web-cache
isothrum up
```

Isothrum checks Docker and Compose, keeps a free host port for `web`'s published port, starts both
services and waits until their health checks pass. Compose's own progress lines come first, then:

```text
The run is up.
```

The run is named after the folder, `compose-web-cache`, and recorded under `~/.isothrum/runs/`.
Nothing is written into this folder.

## Read its status

```sh
isothrum status
```

```text
compose-web-cache: ready
  cache                    running
  web                      running      57608->80/tcp
```

Your port will differ. `curl http://127.0.0.1:57608/` answers `hello from web`.

## Run a command in a service

```sh
isothrum exec cache -- redis-cli ping
```

```text
PONG
```

`exec` prints what the command prints and exits with its exit status.

## Park it and resume it

```sh
isothrum park
isothrum status
```

```text
compose-web-cache: parked
  cache                    exited(0)
  web                      exited(143)
```

`park` stops the containers and keeps them, their network and their volumes.

```sh
isothrum resume
```

```text
The run is up again.
```

`web` answers at the same port as before: Isothrum keeps the host port it picked at the first `up`
until `down`.

## Clean up

```sh
isothrum down
isothrum status
```

```text
compose-web-cache: absent
  cache                    absent
  web                      absent
```

`down` removes the containers, the network and the volumes the run created.

## Next

- Add `--json` to any command for machine-readable output: [`--json` output](../../docs/reference/events.md).
- Run it twice side by side: `isothrum up --name second`, then `--name second` on every command.
