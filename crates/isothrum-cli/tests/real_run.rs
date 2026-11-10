//! A real run on this host's Docker (ignored; `cargo run -p xtask -- real-run` sets it up): the
//! run Isoloom generated in `$ISOTHRUM_REAL_RUN_DIR` goes up, answers on its entry, passes its
//! checks, parks, resumes on the same address, and goes, leaving nothing. Every event is kept in
//! `$ISOTHRUM_REAL_EVENTS` when it is set.

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
    /// `isothrum <args> --json` on the run; its output lines, and whether it exited 0.
    fn run(&mut self, args: &[&str]) -> Outcome<(Vec<Value>, bool)> {
        let out = Command::new(env!("CARGO_BIN_EXE_isothrum"))
            .args(args)
            .arg(&self.run)
            .args(["--name", NAME, "--json", "--home"])
            .arg(self.home.path())
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

    /// The run's state, as `isothrum status --json` says it.
    fn state(&mut self) -> Outcome<String> {
        let (lines, ok) = self.run(&["status"])?;
        let status = lines.into_iter().next().filter(|_| ok).ok_or("`isothrum status` answered nothing")?;
        Ok(field(&status, "state").as_str().unwrap_or_default().to_owned())
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

/// The first line an HTTP server answers at `url` (`http://127.0.0.1:<port>/`).
fn http_status(url: &str) -> Outcome<String> {
    let address = url.trim_start_matches("http://").trim_end_matches('/');
    let mut stream = std::net::TcpStream::connect(address)?;
    stream.write_all(b"GET / HTTP/1.0\r\nHost: localhost\r\n\r\n")?;
    let mut answer = String::new();
    stream.read_to_string(&mut answer)?;
    Ok(answer.lines().next().unwrap_or_default().to_owned())
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
    let mut real = Real {
        run,
        home: tempfile::tempdir()?,
        events: Vec::new(),
        record: Vec::new(),
    };
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| lifecycle(&mut real)));
    // Whatever happened, nothing is left behind.
    let (down, _) = real.run(&["down"])?;
    for said in &real.record {
        eprintln!("{said}");
    }
    if let Ok(path) = std::env::var("ISOTHRUM_REAL_EVENTS") {
        let text: String = real.events.iter().fold(String::new(), |text, e| text + &e.to_string() + "\n");
        std::fs::write(Path::new(&path), text)?;
    }
    match result {
        Ok(outcome) => outcome?,
        Err(panic) => std::panic::resume_unwind(panic),
    }
    assert_eq!(field(last(&down)?, "event"), "result");
    assert_eq!(real.state()?, "absent");
    let left = Command::new("docker")
        .args(["ps", "-aq", "--filter", &format!("label=com.docker.compose.project={NAME}")])
        .output()?;
    assert_eq!(String::from_utf8_lossy(&left.stdout).trim(), "", "containers left");
    Ok(())
}
