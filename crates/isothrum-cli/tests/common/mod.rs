//! What the binary's tests share: `isothrum` run in a lab of its own (its runner's home, a user's
//! home and environment, a project folder), its JSON lines read back, and on Unix a stand-in
//! `docker` on its `PATH`.

use std::process::{Command, Output};

use isothrum_test_support::TempHome;
use serde_json::Value;

/// A helper's outcome: a failed step is the test's report.
pub(crate) type Outcome<T> = Result<T, Box<dyn std::error::Error>>;

/// A Docker engine with Compose, standing in for `docker`: see its own comment.
#[cfg(unix)]
const DOCKER: &str = r#"#!/bin/sh
# A Docker engine with Compose for the binary's tests, for a project of one service `web`, whose
# container's state (running, paused, exited; no file: absent) is in "$ISOTHRUM_TEST_DOCKER/web".
# `compose exec` runs its command here. With $ISOTHRUM_TEST_PIDFILE set, `compose up` writes its
# process id there and runs until it is stopped (two minutes at most), running
# $ISOTHRUM_TEST_ON_INT on SIGINT (default: exit; empty: SIGINT is ignored). The Compose command
# $ISOTHRUM_TEST_DOCKER_FAILS names (`up`, `pause`...) fails as a daemon refusing it.
case "$1" in
  info) echo 29.0.0; exit 0 ;;
  compose) shift ;;
  *) exit 0 ;;
esac
while [ $# -gt 0 ]; do
  case "$1" in
    -p | -f | --project-directory | --env-file) shift 2 ;;
    *) break ;;
  esac
done
if [ "$1" = "$ISOTHRUM_TEST_DOCKER_FAILS" ]; then
  echo "Error response from daemon: $1 refused" >&2
  exit 1
fi
web="$ISOTHRUM_TEST_DOCKER/web"
held() { [ -f "$web" ]; }
case "$1" in
  version) echo 2.29.7 ;;
  config) if [ "$2" = --services ]; then echo web; else echo '{"services":{"web":{}}}'; fi ;;
  up)
    if [ -n "$ISOTHRUM_TEST_PIDFILE" ]; then
      trap "${ISOTHRUM_TEST_ON_INT-exit 130}" INT
      echo $$ > "$ISOTHRUM_TEST_PIDFILE"; echo starting
      i=0; while [ $i -lt 1200 ]; do sleep 0.1; i=$((i+1)); done
    fi
    echo running > "$web" ;;
  pause) held && echo paused > "$web" ;;
  unpause | start) held && echo running > "$web" ;;
  stop) held && echo exited > "$web" ;;
  down) rm -f "$web" ;;
  ps) held && printf '{"ID":"4b0f1d7e2c3a","Service":"web","State":"%s","Health":"","ExitCode":0,"Publishers":[],"Labels":""}\n' "$(cat "$web")" ;;
  exec)
    shift
    while [ $# -gt 0 ]; do
      case "$1" in
        --user | -u | --workdir | -w | --env | -e) shift 2 ;;
        -*) shift ;;
        *) break ;;
      esac
    done
    shift
    exec "$@" ;;
esac
exit 0
"#;

/// The folders a command of the binary runs with, removed when dropped.
pub(crate) struct Lab {
    /// The user's home, environment and locale (not the test's).
    pub(crate) env: TempHome,
    /// The runner's home (`ISOTHRUM_HOME`).
    pub(crate) home: tempfile::TempDir,
    /// The project's folder: empty, or a Compose project of one service `web` ([`Lab::compose`]).
    pub(crate) run: tempfile::TempDir,
    /// The stand-in `docker`'s engine: its container's state, and what the tests have it write.
    pub(crate) docker: tempfile::TempDir,
}

impl Lab {
    /// A lab whose project folder holds nothing.
    pub(crate) fn empty() -> Outcome<Lab> {
        Ok(Lab {
            env: TempHome::new(),
            home: tempfile::tempdir()?,
            run: tempfile::tempdir()?,
            docker: tempfile::tempdir()?,
        })
    }

    /// A lab whose project is a Compose project of one service, `web`.
    #[cfg(unix)]
    pub(crate) fn compose() -> Outcome<Lab> {
        let lab = Lab::empty()?;
        std::fs::write(
            lab.run.path().join("compose.yaml"),
            "services:
  web:
    image: nginx
",
        )?;
        Ok(lab)
    }

    /// The project's folder, as an argument.
    pub(crate) fn dir(&self) -> &str {
        self.run.path().to_str().unwrap_or_default()
    }

    /// `isothrum`, on this lab's homes, without the runner service, the stand-in `docker` first on
    /// its `PATH` (Unix).
    pub(crate) fn command(&self) -> Outcome<Command> {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_isothrum"));
        cmd.envs(self.env.envs())
            .env("ISOTHRUM_NO_SERVICE", "1")
            .env("ISOTHRUM_HOME", self.home.path())
            .env("ISOTHRUM_TEST_DOCKER", self.docker.path());
        #[cfg(unix)]
        {
            let docker = isothrum_test_support::installed_executable(std::path::Path::new(env!("CARGO_TARGET_TMPDIR")), "docker", DOCKER)?;
            let dir = docker.parent().ok_or("the stand-in's folder")?;
            let path = std::env::var_os("PATH").unwrap_or_default();
            cmd.env(
                "PATH",
                std::env::join_paths(std::iter::once(dir.to_path_buf()).chain(std::env::split_paths(&path)))?,
            );
        }
        Ok(cmd)
    }

    /// `isothrum <args>`, run to its end.
    pub(crate) fn isothrum(&self, args: &[&str]) -> Outcome<Output> {
        Ok(self.command()?.args(args).output()?)
    }

    /// The stand-in's file `name` (the state of its container `web`, what a test has it write).
    #[cfg(unix)]
    pub(crate) fn docker_file(&self, name: &str) -> std::path::PathBuf {
        self.docker.path().join(name)
    }
}

/// Every line `out` printed on standard output, as JSON: a line that isn't fails.
pub(crate) fn json_lines(out: &Output) -> Outcome<Vec<Value>> {
    let stdout = String::from_utf8(out.stdout.clone())?;
    let lines = stdout.lines().map(serde_json::from_str).collect::<Result<Vec<Value>, _>>();
    Ok(lines.map_err(|e| format!("{e}: {stdout}; standard error: {}", String::from_utf8_lossy(&out.stderr)))?)
}

/// The last line of standard output, as JSON.
pub(crate) fn last_event(out: &Output) -> Outcome<Value> {
    let lines = json_lines(out)?;
    Ok(lines
        .last()
        .cloned()
        .ok_or_else(|| format!("no output; standard error: {}", String::from_utf8_lossy(&out.stderr)))?)
}
