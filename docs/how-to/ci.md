# Use isothrum in CI

Bring an environment up in a pipeline, test against it, and always tear it down.

## The rules

1. **`up` and `down` must use the same home.** A run's record, its lock and, for Terraform
   targets, its Terraform state live in the runner's home (`ISOTHRUM_HOME`), not in a remote
   backend and not in the repository. A `down` with another home finds no run and can't destroy
   what `up` created. Set `ISOTHRUM_HOME` explicitly in the pipeline.
2. **Tear down even when a step fails or the job is cancelled.** Put `down` in a step that always
   runs.
3. **Keep the whole life in one job when you can.** On hosted runners each job gets a fresh
   machine: a Docker or Vagrant environment dies with its job, so `up`, the tests and `down` must
   share it. Splitting across jobs only makes sense for Terraform targets (servers, clouds), whose
   machines outlive the job, and then the home has to travel between jobs (below).
4. **Pass secrets from the environment.** `--secret-from-env NAME[=VAR]` reads a slot's value from
   a CI secret; values never go on the command line and are masked as `***` in the output.
   Terraform doesn't inherit the shell's cloud settings (`AWS_*`, `ARM_*`, `GOOGLE_*`, `TF_VAR_*`,
   ...): give the cloud credentials as secrets (`--secret-from-env`) and the module's settings as
   values (`--env`).
5. **Read the outcome from the exit code and the events.** `--json` prints one event per line on
   standard output, a usage error included; the exit codes are in
   [Exit codes](../reference/exit-codes.md). A cancelled job's SIGINT or SIGTERM asks the tools
   the operation started to stop (SIGINT to each, so Terraform finishes what it has in flight
   and saves its state), waits for them, ends any still running 30 seconds later (or at a second
   signal), and exits 130 or 143, so the teardown step that follows finds no tool still running.
   Give the job's cancellation that long before it kills `isothrum`. On Windows each tool gets
   Ctrl-C on its own console instead of SIGINT. For more detail, `ISOTHRUM_LOG=debug` traces the runner on standard error
   (`ISOTHRUM_LOG_FORMAT=json` for a log collector).

If two pipelines can run at once against the same place (a server, a cloud account), give each run
its own name: `--name ci-<pipeline id>` on every command.

## GitHub Actions, one job

```yaml
jobs:
  environment:
    runs-on: ubuntu-24.04
    steps:
      - uses: actions/checkout@<commit sha> # pin the version you use
      - name: Install isothrum (checksum verified)
        env:
          VERSION: 2.1.3
          TARGET: x86_64-unknown-linux-musl
        run: |
          set -euo pipefail
          base="https://github.com/Isothrum/isothrum/releases/download/v${VERSION}"
          curl -fsSLO "${base}/isothrum-${VERSION}-${TARGET}.tar.gz"
          curl -fsSLO "${base}/SHA256SUMS"
          sha256sum --check --ignore-missing SHA256SUMS
          tar -xzf "isothrum-${VERSION}-${TARGET}.tar.gz"
          echo "${PWD}/isothrum-${VERSION}-${TARGET}" >> "$GITHUB_PATH"
          echo "ISOTHRUM_HOME=${RUNNER_TEMP}/isothrum-home" >> "$GITHUB_ENV"
      # The project's files (a compose.yaml here): committed, or generated in a step before.
      - run: isothrum up --json > up.jsonl
      - run: ./run-your-tests.sh
      - run: isothrum check
      - name: Tear down
        if: always()
        run: isothrum down
      - name: Keep the events
        if: always()
        uses: actions/upload-artifact@<commit sha> # pin the version you use
        with:
          name: isothrum-events
          path: up.jsonl
```

`if: always()` runs the teardown after a failure and after a cancellation.

## GitHub Actions, across jobs (Terraform targets)

The example is a Terraform module for AWS in `infra/`. Keep the home in the workspace, pack it
with `tar` (which keeps the files' modes: `ssh` refuses a private key others can read) together
with the module's folder, and carry it to the job that tears down.

```yaml
jobs:
  up:
    runs-on: ubuntu-24.04
    steps:
      - uses: actions/checkout@<commit sha>
      # ... install isothrum as above, with ISOTHRUM_HOME=${GITHUB_WORKSPACE}/.isothrum-home ...
      - run: |
          isothrum up infra --name "ci-${GITHUB_RUN_ID}" \
            --secret-from-env AWS_ACCESS_KEY_ID --secret-from-env AWS_SECRET_ACCESS_KEY \
            --env region=eu-west-3 \
            --env allowed_cidr="$(curl -fsS https://checkip.amazonaws.com)/32" \
            --lifetime 120
        env:
          AWS_ACCESS_KEY_ID: ${{ secrets.AWS_ACCESS_KEY_ID }}
          AWS_SECRET_ACCESS_KEY: ${{ secrets.AWS_SECRET_ACCESS_KEY }}
      - name: Pack the home and the module
        if: always()
        run: tar -czf isothrum-state.tgz .isothrum-home infra
      - uses: actions/upload-artifact@<commit sha>
        if: always()
        with:
          name: isothrum-state
          path: isothrum-state.tgz
          retention-days: 1

  # test: your jobs against the environment, with `needs: up`.

  down:
    needs: [up, test]
    if: always() && needs.up.result != 'skipped'
    runs-on: ubuntu-24.04
    steps:
      - uses: actions/checkout@<commit sha>
      # ... install isothrum as above, with ISOTHRUM_HOME=${GITHUB_WORKSPACE}/.isothrum-home ...
      - uses: actions/download-artifact@<commit sha>
        with:
          name: isothrum-state
      - run: tar -xzf isothrum-state.tgz
      - run: |
          isothrum down infra --name "ci-${GITHUB_RUN_ID}" \
            --secret-from-env AWS_ACCESS_KEY_ID --secret-from-env AWS_SECRET_ACCESS_KEY
        env:
          AWS_ACCESS_KEY_ID: ${{ secrets.AWS_ACCESS_KEY_ID }}
          AWS_SECRET_ACCESS_KEY: ${{ secrets.AWS_SECRET_ACCESS_KEY }}
```

Secrets are never recorded, so `down` takes the credentials again. The variable names
(`region`, `allowed_cidr`) are the module's own `variable` blocks; `isothrum` gives each `--env`
value to Terraform as that variable.

The archive holds the Terraform state and the runner's SSH private key: anyone who can download
the artifact can reach the machines. Keep the retention short, and prefer a single job when the
repository's artifacts are visible to people who shouldn't have that access. A cache is not a
substitute: a cache can be evicted, and a `down` without the state can't destroy anything.

## GitLab CI

```yaml
environment:
  variables:
    ISOTHRUM_HOME: "$CI_PROJECT_DIR/.isothrum-home"
  script:
    - isothrum up --json > up.jsonl
    - ./run-your-tests.sh
    - isothrum check
  after_script:
    - isothrum down
  artifacts:
    when: always
    paths: [up.jsonl]
```

`after_script` runs after `script` whether it passed or failed, in the same working folder. GitLab
gives it its own time limit (5 minutes by default, `RUNNER_AFTER_SCRIPT_TIMEOUT`): raise it for a
cloud teardown. Across jobs, carry the home as an artifact as in the GitHub example, with
`artifacts: expire_in` set short. Mark the variables that hold credentials as masked and pass
them with `--secret-from-env`.

## Leftovers

A cancelled runner machine, a lost artifact or a teardown that failed can leave a cloud run
behind. Give cloud runs a `--lifetime` and sweep with `isothrum reap`; see
[Control costs](cost-controls.md).
