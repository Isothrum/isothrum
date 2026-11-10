# Your first run

In this tutorial you bring up a small environment with Docker Compose, check it, and tear it
down. It takes about five minutes and needs nothing but Docker.

You will use Isoloom's `hello-stack` example: a web server and a cache on a private network.

## Before you start

- Docker running, with Compose 2.17.0 or later: `docker compose version`.
- `isothrum` installed (see [Install](../../README.md#install)).
- `isoloom`, to generate the files, and its `examples/` folder: install it from
  [Isoloom](https://github.com/isoloom/isoloom) and clone its repository.

## 1. Generate the files

Copy the example to a folder of your own and generate its Docker target:

```sh
cp -r isoloom/examples/hello-stack ~/hello-stack
cd ~/hello-stack
isoloom generate --target docker
```

Isoloom writes `.isoloom/docker/`: a Compose file, the checks, and `run.json`, the run manifest
Isothrum reads.

## 2. Bring it up

```sh
isothrum up
```

Isothrum checks that Docker and Compose answer, pins a free port on `127.0.0.1` for each published
port, starts the containers and waits until they are ready. It ends with the environment's
message, its addresses filled in:

```text
ready: step_ok up
web__web: http://127.0.0.1:64336/
The page answers at http://127.0.0.1:64336/ (and at
http://web/ from inside). The cache at 10.60.0.20:6379 holds the
greeting seeded at start; nothing here reaches the internet.
```

Your port will differ. Open the URL in a browser: the page says hello.

## 3. Look at it

```sh
isothrum status
```

```text
hello-stack: ready
  cache                    target  running
  web                      target  running      64336->80
  ...
```

The run is recorded under `~/.isothrum/runs/hello-stack/`. Nothing was written into
`~/hello-stack`.

## 4. Check it

```sh
isothrum check
```

Isothrum runs the checks the example declares (the page answers, the cache holds its greeting,
nothing reaches the internet) and prints each result, then the count:

```text
12 passed, 0 failed
```

`isothrum check` exits 0 when every check passes and 1 when one fails, so it fits a CI job.

## 5. Tear it down

```sh
isothrum down
isothrum status
```

`down` removes the containers, networks and volumes the run created. `status` now says
`hello-stack: absent`.

## What you did

You ran the whole life of a run: `up`, `status`, `check`, `down`. The same commands work for every
target Isoloom generates; a VM target adds `--provider` (`isothrum up --provider virtualbox`).

Next:

- Add `--json` to any command to get the events as JSON lines: [Event stream](../reference/events.md).
- Run it in a pipeline: [Use isothrum in CI](../how-to/ci.md).
- Every command and flag: [CLI reference](../reference/cli.md).
