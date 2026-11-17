//! Who listens on a host port: `lsof` (macOS, Linux), `ss` where `lsof` isn't (Linux), PowerShell's
//! `Get-NetTCPConnection` / `Get-NetUDPEndpoint` (Windows). Asked once a run failed for a taken
//! port, or by an embedder about a port it found taken ([`port_holder`]), best-effort: a lookup
//! that fails or finds nothing names no one.

use std::time::Duration;

use super::{Call, argv};
use crate::error::PortHolder;
use crate::port::HostPort;
use crate::process::ProcessRunner;
use crate::project::platform::Protocol;

/// The process listening on this computer's host `port` (`protocol`), asked of the system's own
/// tools through `processes`, each given `limit`: when one is found. Best-effort: none when no tool
/// answers or names a process (`ss` run without root names none, nor does a listener of another
/// user's on some systems).
pub async fn port_holder(processes: &dyn ProcessRunner, port: HostPort, protocol: Protocol, limit: Duration) -> Option<PortHolder> {
    let call = Call {
        runner: processes,
        cwd: None,
        env: Vec::new(),
        limit,
        sink: &crate::formats::discard,
        host_env: &crate::host_env::EMPTY,
    };
    holder(&call, port, protocol).await
}

/// The process listening on host `port` (`protocol`), when one is found.
pub(crate) async fn holder(call: &Call<'_>, port: HostPort, protocol: Protocol) -> Option<PortHolder> {
    #[cfg(windows)]
    {
        windows(call, port, protocol).await
    }
    #[cfg(not(windows))]
    {
        unix(call, port, protocol).await
    }
}

/// `lsof`, then `ss`.
#[cfg(not(windows))]
async fn unix(call: &Call<'_>, port: HostPort, protocol: Protocol) -> Option<PortHolder> {
    let (spec, state) = match protocol {
        Protocol::Udp => (format!("-iUDP:{port}"), None),
        Protocol::Tcp => (format!("-iTCP:{port}"), Some("-sTCP:LISTEN")),
    };
    let mut args = vec!["-nP".to_owned(), spec];
    args.extend(state.map(str::to_owned));
    args.push("-FpcL".to_owned());
    if let Ok(out) = call.run::<String>("lsof", args).await
        && let Some(holder) = from_lsof(&out.stdout)
    {
        return Some(holder);
    }
    let kind = if protocol == Protocol::Udp { "-lunp" } else { "-ltnp" };
    let out = call.run::<String>("ss", argv(["-H", kind, &format!("( sport = :{port} )")])).await.ok()?;
    from_ss(&out.stdout)
}

/// PowerShell: the owning process of the listener, then its name.
#[cfg(windows)]
async fn windows(call: &Call<'_>, port: HostPort, protocol: Protocol) -> Option<PortHolder> {
    let listener = match protocol {
        Protocol::Udp => format!("Get-NetUDPEndpoint -LocalPort {port}"),
        Protocol::Tcp => format!("Get-NetTCPConnection -LocalPort {port} -State Listen"),
    };
    let script = format!(
        "$ErrorActionPreference='SilentlyContinue'; $c = {listener} | Select-Object -First 1; \
         if ($c) {{ $p = Get-Process -Id $c.OwningProcess; if ($p) {{ Write-Output \"$($p.Id) $($p.ProcessName)\" }} }}"
    );
    let out = call
        .run::<String>("powershell", argv(["-NoProfile", "-NonInteractive", "-Command", &script]))
        .await
        .ok()?;
    from_powershell(&out.stdout)
}

/// The first process of `lsof -FpcL`: its records are field lines, `p<pid>` then `c<command>`.
#[cfg(any(not(windows), test))]
fn from_lsof(lines: &[String]) -> Option<PortHolder> {
    let mut pid = None;
    for line in lines {
        if let Some(p) = line.strip_prefix('p') {
            pid = p.trim().parse().ok();
        } else if let (Some(c), Some(pid)) = (line.strip_prefix('c'), pid) {
            return Some(PortHolder::new(pid, c.trim()));
        }
    }
    None
}

/// The first listener of `ss -H -l…p`: `… users:(("nginx",pid=1234,fd=6))`. `ss` run without
/// root shows no process: none then.
#[cfg(any(not(windows), test))]
fn from_ss(lines: &[String]) -> Option<PortHolder> {
    let users = lines.first()?.split("users:((").nth(1)?;
    let program = users.split('"').nth(1)?;
    let pid = users.split("pid=").nth(1)?.split(|c: char| !c.is_ascii_digit()).next()?.parse().ok()?;
    Some(PortHolder::new(pid, program))
}

/// The script's line: `<pid> <name>`.
#[cfg(any(windows, test))]
fn from_powershell(lines: &[String]) -> Option<PortHolder> {
    let (pid, program) = lines.iter().map(|l| l.trim()).find(|l| !l.is_empty())?.split_once(' ')?;
    Some(PortHolder::new(pid.parse().ok()?, program.trim()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lines(text: &str) -> Vec<String> {
        text.lines().map(str::to_owned).collect()
    }

    #[test]
    fn the_first_listener_is_read_from_each_tools_answer() {
        let holder = |pid, program: &str| Some(PortHolder::new(pid, program));
        assert_eq!(from_lsof(&lines("p4123\ncnginx\nLroot\nftxt\np5678\ncpython3\n")), holder(4123, "nginx"));
        // A command line before any process line belongs to none.
        assert_eq!(from_lsof(&lines("cnginx\np700\nccom.docke\n")), holder(700, "com.docke"));
        assert_eq!(from_lsof(&[]), None);
        assert_eq!(
            from_ss(&lines(r#"LISTEN 0 4096 0.0.0.0:8080 0.0.0.0:* users:(("nginx",pid=1234,fd=6))"#)),
            holder(1234, "nginx")
        );
        assert_eq!(from_ss(&lines("LISTEN 0 4096 0.0.0.0:8080 0.0.0.0:*")), None);
        assert_eq!(from_powershell(&lines("\n4123 nginx\n")), holder(4123, "nginx"));
        assert_eq!(from_powershell(&lines("")), None);
    }

    #[cfg(not(windows))]
    #[tokio::test]
    async fn lsof_is_asked_first_and_ss_when_it_names_no_one() {
        use crate::process::fake::{FakeProcesses, Reply};
        let fake = FakeProcesses::script()
            .expect("lsof -nP -iTCP:8080", Reply::ok(""))
            .expect("ss -H -ltnp", Reply::ok(r#"LISTEN 0 4096 *:8080 *:* users:(("caddy",pid=77,fd=3))"#))
            .build();
        let call = crate::cli::testing::call(&fake);
        assert_eq!(holder(&call, HostPort::new(8080), Protocol::Tcp).await, Some(PortHolder::new(77, "caddy")));
    }

    #[cfg(not(windows))]
    #[tokio::test]
    async fn an_embedder_asks_who_holds_a_port_and_a_lookup_that_finds_none_names_no_one() {
        use crate::process::fake::{FakeProcesses, Reply};
        let limit = Duration::from_secs(4);
        let fake = FakeProcesses::script().expect("lsof -nP -iUDP:53", Reply::ok("p91\ncdnsmasq\n")).build();
        assert_eq!(
            port_holder(&fake, HostPort::new(53), Protocol::Udp, limit).await,
            Some(PortHolder::new(91, "dnsmasq"))
        );
        let none = FakeProcesses::script()
            .expect("lsof -nP -iTCP:8080", Reply::missing())
            .expect("ss -H -ltnp", Reply::ok("LISTEN 0 4096 *:8080 *:*"))
            .build();
        assert_eq!(port_holder(&none, HostPort::new(8080), Protocol::Tcp, limit).await, None);
    }
}
