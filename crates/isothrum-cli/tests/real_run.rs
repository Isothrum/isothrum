//! Real runs on this host's Docker (ignored), each leaving nothing:
//!
//! - the run Isoloom generated in `$ISOTHRUM_REAL_RUN_DIR` (`cargo run -p xtask -- real-run` sets
//!   it up) goes up, answers on its entry, passes its checks, parks, resumes on the same address,
//!   and goes. Every event is kept in `$ISOTHRUM_REAL_EVENTS` when it is set;
//! - the Compose example, `examples/compose-web-cache`, does what its README shows: up, its page
//!   answers on the port `web` publishes, `exec` reaches the cache, park, resume on the same port,
//!   down. The same commands on a model of Docker are the binary's unit tests
//!   (`commands/tests/examples.rs`).
#![expect(
    clippy::disallowed_methods,
    reason = "a test starts the binary and the programs it checks against: it is no adapter of the product"
)]

use std::error::Error;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::Command;

use serde_json::Value;

/// A test's outcome: a failed step is its report.
type Outcome<T = ()> = Result<T, Box<dyn Error>>;

/// The run's name: its own, so it never meets a run started by hand.
const NAME: &str = "isothrum-real-run";

/// The run being run, the runner's home, and what the run said so far.
struct Real {
    run: PathBuf,
    home: tempfile::TempDir,
    /// Every event of every command, in order.
    events: Vec<Value>,
    /// Each command and what it printed on standard error: the run's record.
    record: Vec<String>,
}

impl Real {
    /// A run of the project in `run`, on a home of its own.
    fn of(run: PathBuf) -> Outcome<Real> {
        Ok(Real {
            run,
            home: tempfile::tempdir()?,
            events: Vec::new(),
            record: Vec::new(),
        })
    }

    /// `isothrum <args> --json` on the run (a command's own arguments after a `--` in `args`); its
    /// output lines, and whether it exited 0.
    fn run(&mut self, args: &[&str]) -> Outcome<(Vec<Value>, bool)> {
        let (own, after) = args.split_at(args.iter().position(|a| *a == "--").unwrap_or(args.len()));
        let out = Command::new(env!("CARGO_BIN_EXE_isothrum"))
            .env("ISOTHRUM_NO_SERVICE", "1")
            .args(own)
            .arg(&self.run)
            .args(["--name", NAME, "--json", "--home"])
            .arg(self.home.path())
            .args(after)
            .output()?;
        let lines: Vec<Value> = String::from_utf8_lossy(&out.stdout)
            .lines()
            .filter_map(|l| serde_json::from_str(l).ok())
            .collect();
        self.record.push(format!(
            "isothrum {}: {} lines, exit {}\n{}",
            args.join(" "),
            lines.len(),
            out.status,
            String::from_utf8_lossy(&out.stderr)
        ));
        self.events.extend(lines.iter().filter(|l| l.get("event").is_some()).cloned());
        Ok((lines, out.status.success()))
    }

    /// `isothrum status --json`.
    fn status(&mut self) -> Outcome<Value> {
        let (lines, ok) = self.run(&["status"])?;
        Ok(lines.into_iter().next().filter(|_| ok).ok_or("`isothrum status` answered nothing")?)
    }

    /// The run's state, as `isothrum status --json` says it.
    fn state(&mut self) -> Outcome<String> {
        Ok(field(&self.status()?, "state").as_str().unwrap_or_default().to_owned())
    }

    /// Runs `lifecycle`, then, whatever happened, `down`: the run is absent after it and Docker
    /// holds no container of it. A failure of `lifecycle` carries the record of every command; the
    /// events are kept in `$ISOTHRUM_REAL_EVENTS` when it is set.
    fn removed_after(mut self, lifecycle: fn(&mut Real) -> Outcome) -> Outcome {
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| lifecycle(&mut self)));
        let (down, _) = self.run(&["down"])?;
        if let Ok(path) = std::env::var("ISOTHRUM_REAL_EVENTS") {
            let text: String = self.events.iter().fold(String::new(), |text, e| text + &e.to_string() + "\n");
            std::fs::write(Path::new(&path), text)?;
        }
        let failed = match result {
            Ok(Ok(())) => None,
            Ok(Err(e)) => Some(e.to_string()),
            Err(panic) => Some(
                panic
                    .downcast_ref::<String>()
                    .cloned()
                    .or_else(|| panic.downcast_ref::<&str>().map(|s| (*s).to_owned()))
                    .unwrap_or_default(),
            ),
        };
        if let Some(why) = failed {
            return Err(format!("{why}\n{}", self.record.join("\n")).into());
        }
        assert_eq!(field(last(&down)?, "event"), "result");
        assert_eq!(self.state()?, "absent");
        let left = Command::new("docker")
            .args(["ps", "-aq", "--filter", &format!("label=com.docker.compose.project={NAME}")])
            .output()?;
        assert_eq!(String::from_utf8_lossy(&left.stdout).trim(), "", "containers left");
        Ok(())
    }
}

/// `value[key]`, or null.
fn field<'a>(value: &'a Value, key: &str) -> &'a Value {
    value.get(key).unwrap_or(&Value::Null)
}

/// The last event of an operation: its result (or error).
fn last(events: &[Value]) -> Outcome<&Value> {
    Ok(events.last().ok_or("no event")?)
}

/// What an HTTP server answers at `url` (`http://127.0.0.1:<port>/`): its status line and its body.
fn http_get(url: &str) -> Outcome<(String, String)> {
    let address = url.trim_start_matches("http://").trim_end_matches('/');
    let mut stream = std::net::TcpStream::connect(address)?;
    stream.write_all(b"GET / HTTP/1.0\r\nHost: localhost\r\n\r\n")?;
    let mut answer = String::new();
    stream.read_to_string(&mut answer)?;
    let status = answer.lines().next().unwrap_or_default().to_owned();
    let body = answer.split("\r\n\r\n").nth(1).unwrap_or_default().trim().to_owned();
    Ok((status, body))
}

/// The first line an HTTP server answers at `url`.
fn http_status(url: &str) -> Outcome<String> {
    Ok(http_get(url)?.0)
}

/// The run's entry, from its operation's `output` events.
fn entry(events: &[Value]) -> Outcome<String> {
    let output = events
        .iter()
        .find(|e| field(e, "event") == "output" && field(e, "role") == "entry")
        .ok_or("no entry output")?;
    Ok(field(output, "value").as_str().ok_or("an entry without a value")?.to_owned())
}

/// Up, checks, park and resume, each checked as it goes.
fn lifecycle(real: &mut Real) -> Outcome {
    let (up, ok) = real.run(&["up"])?;
    if !ok || field(last(&up)?, "event") != "result" {
        return Err(format!("up: {:?}", last(&up)?).into());
    }
    let url = entry(&up)?;
    assert!(http_status(&url)?.contains(" 200 "), "{url}");
    assert_eq!(real.state()?, "ready");

    let (check, checked) = real.run(&["check"])?;
    let failed: Vec<&Value> = check.iter().filter(|e| field(e, "event") == "check" && field(e, "ok") == false).collect();
    assert!(checked, "checks: {failed:?}");

    let (_, parked) = real.run(&["park"])?;
    assert!(parked);
    assert_eq!(real.state()?, "parked");

    let (resume, resumed) = real.run(&["resume"])?;
    assert!(resumed, "{:?}", last(&resume)?);
    assert_eq!(entry(&resume)?, url, "resumed on the same address");
    assert!(http_status(&url)?.contains(" 200 "));
    Ok(())
}

#[test]
#[ignore = "needs Docker and a generated run: cargo run -p xtask -- real-run"]
fn a_generated_run_runs_for_real() -> Outcome {
    let run = PathBuf::from(std::env::var("ISOTHRUM_REAL_RUN_DIR").map_err(|_| "ISOTHRUM_REAL_RUN_DIR: a run folder Isoloom generated for docker")?);
    Real::of(run)?.removed_after(lifecycle)
}

/// Where `web` answers on this computer: the host port its port 80 is published at.
fn web_url(real: &mut Real) -> Outcome<String> {
    let status = real.status()?;
    let web = field(&status, "machines").as_array().into_iter().flatten().find(|m| field(m, "name") == "web");
    let published = web.and_then(|m| field(m, "published").as_array()?.iter().find(|p| field(p, "port") == 80));
    let port = published.and_then(|p| field(p, "host_port").as_u64()).ok_or("`web` publishes no port 80")?;
    Ok(format!("http://127.0.0.1:{port}/"))
}

/// The Compose example's README, each command checked as it goes.
fn example_lifecycle(real: &mut Real) -> Outcome {
    let (up, ok) = real.run(&["up"])?;
    assert!(ok, "up: {:?}", last(&up)?);
    assert_eq!(real.state()?, "ready");
    let url = web_url(real)?;
    assert_eq!(http_get(&url)?.1, "hello from web");

    let (exec, ran) = real.run(&["exec", "cache", "--", "redis-cli", "ping"])?;
    assert!(ran, "exec: {exec:?}");
    assert!(exec.iter().any(|e| field(e, "event") == "log" && field(e, "line") == "PONG"), "{exec:?}");

    assert!(real.run(&["park"])?.1);
    assert_eq!(real.state()?, "parked");
    assert!(real.run(&["resume"])?.1);
    assert_eq!(web_url(real)?, url, "resumed on the same port");
    assert_eq!(http_get(&url)?.1, "hello from web");
    Ok(())
}

#[test]
#[ignore = "needs Docker: cargo test -p isothrum-cli --test real_run -- --ignored the_compose_example"]
fn the_compose_example_runs_for_real() -> Outcome {
    Real::of(Path::new(env!("CARGO_MANIFEST_DIR")).join("../../examples/compose-web-cache"))?.removed_after(example_lifecycle)
}
