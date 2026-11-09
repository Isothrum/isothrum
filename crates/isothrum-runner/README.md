# isothrum-runner

Executes the run manifest [Isoloom](https://github.com/isoloom/isoloom) writes next to a target's
generated files (`.isoloom/<target>/run.json`, `manifest_version` 1): bring an environment up,
check it, park, resume, update and tear it down with Docker Compose, Vagrant or Terraform. The
runner keeps each run's state and lock under a home folder, delivers params and secrets without
ever putting a value on a command line, and reports every operation as a stream of JSON events.

It is the library behind the [`isothrum`](https://crates.io/crates/isothrum-cli) command line;
embed it to drive runs from your own program.

## Install

```sh
cargo add isothrum-runner
cargo add tokio --features macros,rt-multi-thread
cargo add serde_json  # the example prints events as JSON
```

The runner is async, on Tokio.

## Example

```rust,no_run
use std::path::Path;

use isothrum_runner::{Event, RunOptions, Runner, RunnerConfig};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Runs are recorded under this folder (the CLI uses ~/.isothrum).
    let runner = Runner::new(RunnerConfig::new("/var/lib/my-app/isothrum"))?;
    // Opening reads and checks the manifest; nothing runs yet.
    let run = runner.open(Path::new("env/.isoloom/docker/run.json"), None)?;

    // An event sink is any `Fn(&Event)`.
    let events = |event: &Event| println!("{}", serde_json::to_string(event).unwrap_or_default());
    let opts = RunOptions::new();

    let started = run.up(&opts, &events).await?;
    println!("{}", started.message.unwrap_or_default());
    let report = run.check(&opts, &events).await?;
    println!("checks passed: {}", report.ok);
    run.down(&opts, &events).await?;
    Ok(())
}
```

## Feature flags

| Feature | What it adds |
|---|---|
| `fake` | Test doubles for embedders' tests: `isothrum_runner::fake::{FakeProcesses, Reply, Call}` (scripted tools, every call recorded) and `FakePorts` (scripted host ports). Set them on `RunnerConfig::processes` and `RunnerConfig::ports`. |
| `fuzzing` | Internal: the pure cores the repository's fuzz targets drive. Hidden from the docs and not part of the public API. |

No feature is on by default.

## Documentation

- API: <https://docs.rs/isothrum-runner>
- The event stream's JSON Schema:
  [`schema/events-v0.schema.json`](https://github.com/Isothrum/isothrum/blob/main/crates/isothrum-runner/schema/events-v0.schema.json)
- Guides and reference: <https://github.com/Isothrum/isothrum/blob/main/docs/README.md>

Minimum Rust: 1.90. Licence: Apache-2.0.
