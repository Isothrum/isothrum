//! The container self-test: a throwaway two-container lab (`BusyBox`), proving the engine answers,
//! an image pulls, Compose starts a lab, the containers reach each other by name, and a published
//! port is reachable from this machine.

use std::path::Path;
use std::time::Duration;

use isothrum_runner::ProcessRunner;

use super::Reporter;
use super::downloads::ensure_image;
use crate::error::{Error, Result};
use crate::exec;

const PROJECT: &str = "cyberctf-selftest";

/// The test image: ~2 MB, so a slow pull is the network, not a stuck engine.
pub(crate) const IMAGE: &str = "busybox:1.36";

/// What the test web server serves, so a reply is known to come from it.
const MARKER: &str = "cyberctf-selftest-ok";

/// Tries (500 ms apart) for the lab's web server to answer: httpd needs a moment.
const ANSWER_TRIES: u32 = 10;
const RETRY: Duration = Duration::from_millis(500);

/// How long the test lab gets to start (its image is local by then), and each probe and the
/// clean-up to answer, before a wedged engine fails the step instead of hanging the test.
const START_LIMIT: Duration = Duration::from_secs(180);
const PROBE_LIMIT: Duration = Duration::from_secs(20);
const DOWN_LIMIT: Duration = Duration::from_secs(60);

/// How long a single connection to the published port may take.
const REACH_TIMEOUT: Duration = Duration::from_secs(3);

fn compose_file() -> String {
    format!(
        r#"services:
  web:
    image: {IMAGE}
    command: ["sh", "-c", "mkdir -p /www && echo {MARKER} > /www/index.html && httpd -f -p 80 -h /www"]
    ports: ["127.0.0.1::80"]
  probe:
    image: {IMAGE}
    command: ["sleep", "300"]
"#
    )
}

async fn compose(runner: &dyn ProcessRunner, dir: &Path, rest: &[&str], limit: Duration) -> Result<String> {
    let mut args = vec!["compose", "-p", PROJECT, "-f", "docker-compose.yml"];
    args.extend_from_slice(rest);
    exec::within("docker compose", limit, exec::run(runner, "docker", &args, Some(dir))).await
}

/// Whether an HTTP GET to `addr` (as `docker compose port` gives it, `host:port`) returns a body
/// carrying `marker`, with every step time-limited so a dead port can't hang the test.
async fn reaches(addr: &str, marker: &str) -> bool {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let Ok(Ok(mut sock)) = tokio::time::timeout(REACH_TIMEOUT, tokio::net::TcpStream::connect(addr)).await else {
        return false;
    };
    let request = format!("GET / HTTP/1.0\r\nHost: {addr}\r\nConnection: close\r\n\r\n");
    if sock.write_all(request.as_bytes()).await.is_err() {
        return false;
    }
    let mut buf = Vec::new();
    match tokio::time::timeout(REACH_TIMEOUT, sock.read_to_end(&mut buf)).await {
        Ok(Ok(_)) => String::from_utf8_lossy(&buf).contains(marker),
        _ => false,
    }
}

pub(super) async fn run_test(runner: &dyn ProcessRunner, dir: &Path, r: &Reporter) -> Result<()> {
    std::fs::write(dir.join("docker-compose.yml"), compose_file())?;
    let res = steps(runner, dir, r).await;
    // Always tear the test lab down, even after a failed step.
    let down = compose(runner, dir, &["down", "-v", "--remove-orphans", "-t", "0"], DOWN_LIMIT).await;
    r.cleanup("Clean up", &res, down, None);
    res
}

async fn steps(runner: &dyn ProcessRunner, dir: &Path, r: &Reporter) -> Result<()> {
    r.step("engine", "Container engine answers", async {
        let v = exec::run_read(runner, "docker", &["info", "--format", "{{.ServerVersion}}"], None).await?;
        Ok(((), Some(format!("Docker {}", v.trim()))))
    })
    .await?;

    r.step("pull", "Download a test image", async {
        let had = ensure_image(runner).await?;
        Ok(((), Some(if had { format!("{IMAGE} (ready)") } else { IMAGE.to_owned() })))
    })
    .await?;

    r.step("start", "Start a two-container test lab", async {
        compose(runner, dir, &["up", "-d", "--quiet-pull"], START_LIMIT).await?;
        Ok(((), None))
    })
    .await?;

    r.step("network", "Containers reach each other", async {
        let mut last = String::new();
        for _ in 0..ANSWER_TRIES {
            match compose(runner, dir, &["exec", "-T", "probe", "wget", "-qO-", "-T", "2", "http://web"], PROBE_LIMIT).await {
                Ok(out) if out.contains(MARKER) => return Ok(((), Some("probe → web on the lab network".to_owned()))),
                Ok(out) => last = out,
                Err(e) => last = e.to_string(),
            }
            tokio::time::sleep(RETRY).await;
        }
        Err(Error::Invalid(format!("probe could not reach web: {last}")))
    })
    .await?;

    r.step("port", "Lab port reachable from this machine", async {
        let addr = compose(runner, dir, &["port", "web", "80"], PROBE_LIMIT).await?.trim().to_owned();
        for _ in 0..ANSWER_TRIES {
            if reaches(&addr, MARKER).await {
                return Ok(((), Some(addr)));
            }
            tokio::time::sleep(RETRY).await;
        }
        Err(Error::Invalid(format!("{addr} did not answer")))
    })
    .await?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_test_lab_serves_its_marker_on_a_loopback_port() {
        let f = compose_file();
        assert!(f.contains("image: busybox:1.36"), "{f}");
        assert!(f.contains("echo cyberctf-selftest-ok > /www/index.html"), "{f}");
        assert!(f.contains("httpd -f -p 80 -h /www") && f.contains("127.0.0.1::80"), "{f}");
        assert!(!f.contains('\t'), "YAML forbids tabs in indentation");
    }

    #[tokio::test]
    async fn a_dead_address_is_not_reachable() {
        // Nothing is listening on this loopback port, so the connection is refused fast.
        assert!(!reaches("127.0.0.1:9", MARKER).await);
    }
}
