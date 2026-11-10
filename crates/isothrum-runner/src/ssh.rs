//! SSH to a run's machines: the `ssh.run` action (a check runner piped into a guest's `sh -s`)
//! and the `ready_file` condition. Every call uses the runner's key and the run's own
//! `known_hosts`, through a config file written in the run's state folder: `ssh -J` hands `-F` to
//! each jump host's own `ssh`, never `-i` or `-o`, so the key and the pinned host keys reach a
//! Proxmox node and a run's router as they reach the machine behind them. The config also keeps
//! the user's own `~/.ssh/config` out of the run.
//!
//! `ssh` runs in the run's state folder and names the config relative to it (`-F ssh_config`):
//! the command `-J` makes for each jump host's `ssh` is run by a shell with the `-F` path
//! unquoted, so a path with a space (a home under macOS's `~/Library/Application Support`) broke
//! every jump.
//!
//! The jumps are the run's (`RunOptions::ssh_jump`: a node whose run bridge this host can't
//! route to), then the manifest's `via`, comma-separated, empty entries skipped.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use isoloom_manifest::{SnakeName, Step, Template};

use crate::Result;
use crate::error::Error;
use crate::executor::{Cx, plan};
use crate::process::{Exit, ProcessRunner, ProcessSpec};
use crate::store::write_private;

/// The runner's key, under its home's `ssh/` folder.
const KEY_FILE: &str = "id_ed25519";

/// The run's SSH configuration, in its state folder.
const CONFIG_FILE: &str = "ssh_config";

/// The host keys the run's SSH pinned (`StrictHostKeyChecking accept-new`), in its state folder.
pub(crate) const KNOWN_HOSTS: &str = "known_hosts";

/// How long making the key may take.
const KEYGEN_LIMIT: std::time::Duration = std::time::Duration::from_secs(60);

/// The id a failure to make the key reads as.
const KEY_STEP: &str = "ssh-key";

/// The runner's SSH folder under `home`: its key, which a tool run from an image mounts.
pub(crate) fn dir(home: &Path) -> PathBuf {
    home.join("ssh")
}

/// The runner's private key: `<home>/ssh/id_ed25519`.
pub(crate) fn key_path(home: &Path) -> PathBuf {
    dir(home).join(KEY_FILE)
}

/// The public half of the runner's key, one line, when it exists.
///
/// # Errors
///
/// [`Error::Io`] when the file is there but can't be read.
pub(crate) fn public_key(home: &Path) -> Result<Option<String>> {
    let path = key_path(home).with_extension("pub");
    let text = crate::store::if_there(std::fs::read_to_string(&path)).map_err(Error::io(&path, "read"))?;
    Ok(text.map(|t| t.trim().to_owned()).filter(|k| !k.is_empty()))
}

/// Makes the runner's key when it has none (`ssh-keygen`, no passphrase), and returns its public
/// half.
pub(crate) async fn ensure_key(processes: &dyn ProcessRunner, home: &Path) -> Result<String> {
    if let Some(key) = public_key(home)? {
        return Ok(key);
    }
    let dir = dir(home);
    crate::store::private_dir(&dir).map_err(Error::io(&dir, "created"))?;
    let path = key_path(home);
    let spec = ProcessSpec::new(
        "ssh-keygen",
        ["-q", "-t", "ed25519", "-N", "", "-C", "isothrum", "-f"]
            .map(String::from)
            .into_iter()
            .chain([path.display().to_string()]),
    );
    let progress = crate::executor::Progress::new();
    let out = crate::executor::within(KEY_STEP, KEYGEN_LIMIT, &progress, progress.capture(processes, &spec)).await?;
    let failed = |reason: String| Error::ToolFailed { step: KEY_STEP.into(), reason };
    if !out.succeeded() {
        return Err(failed(out.reason()));
    }
    public_key(home)?.ok_or_else(|| failed("ssh-keygen wrote no public key".into()))
}

/// Whether a run of `manifest` needs the runner's key: it reaches a machine over SSH, or names
/// the key.
pub(crate) fn needed(manifest: &isoloom_manifest::Manifest) -> bool {
    use isoloom_manifest::{Action, Condition, Run, RunVar};
    let names_key = crate::manifest::names_run_var(manifest, |v| matches!(v, RunVar::SshKey | RunVar::SshPublicKey));
    let ssh_step = manifest
        .step_lists()
        .iter()
        .flat_map(|(_, steps)| steps.iter())
        .any(|s| matches!(s.run, Run::Action { action: Action::SshRun, .. }));
    let ready_file = manifest.ready.iter().any(|c| matches!(c, Condition::ReadyFile { .. }));
    names_key || ssh_step || ready_file
}

/// The run's SSH configuration: the key and the run's `known_hosts` for every host, a new host's
/// key accepted once and pinned, no prompt, and a host that stops answering given up in under a
/// minute. `None` when a path is one no OpenSSH config can hold ([`config_path`]).
fn config(key: &Path, known_hosts: &Path) -> Option<String> {
    let path = |p: &Path| config_path(&p.to_string_lossy(), cfg!(windows));
    Some(format!(
        "# Written by the Isothrum runner for this run.\nHost *\n  IdentityFile {}\n  IdentitiesOnly yes\n  UserKnownHostsFile {}\n  StrictHostKeyChecking accept-new\n  BatchMode yes\n  ConnectTimeout 10\n  ServerAliveInterval 15\n  ServerAliveCountMax 3\n  LogLevel ERROR\n",
        path(key)?,
        path(known_hosts)?,
    ))
}

/// A path as every OpenSSH reads it in its config, the parser before 8.7 (Windows 10's, RHEL 8's,
/// Ubuntu 20.04's) as the one after: between double quotes, which both take as one value whatever
/// it holds but `"`, and with `%` doubled (a token there). On Windows its `\` are written `/`,
/// which Windows' OpenSSH reads as `\`: the parser from 8.7 reads `\\` inside quotes as one `\`,
/// so a network path (`\\server\share`) would lose its first. `None` for a path no parser reads
/// as written: one holding `"` or a control character, `${` (an environment variable from 8.4),
/// or, but on Windows, `\` (an escape from 8.7).
fn config_path(path: &str, windows: bool) -> Option<String> {
    let path = if windows { path.replace('\\', "/") } else { path.to_owned() };
    if path.contains(['"', '\\']) || path.contains("${") || path.chars().any(char::is_control) {
        return None;
    }
    Some(format!("\"{}\"", path.replace('%', "%%")))
}

/// Whether `login` is a plain `[user@]host` (letters, digits, `.`, `-`, `_`, `:` and one `@`):
/// nothing ssh could read as an option or a command.
fn plain_login(login: &str) -> bool {
    !login.is_empty() && !login.starts_with('-') && login.matches('@').count() <= 1 && login.chars().all(|c| c.is_ascii_alphanumeric() || ".-_:@".contains(c))
}

/// Where an SSH call goes: a user on a host, through jump hosts.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Login {
    /// `user@host`.
    pub(crate) target: String,
    /// The jump hosts, in order.
    pub(crate) jumps: Vec<String>,
}

impl Login {
    /// `user@host`, through `jump` (the run's) then `via` (comma-separated, empty entries
    /// skipped). Refused when a part isn't a plain login.
    pub(crate) fn new(user: &str, host: &str, jump: Option<&str>, via: &str) -> Result<Login> {
        let target = format!("{user}@{host}");
        let jumps: Vec<String> = jump
            .into_iter()
            .flat_map(|j| j.split(','))
            .chain(via.split(','))
            .map(str::trim)
            .filter(|j| !j.is_empty())
            .map(str::to_owned)
            .collect();
        match std::iter::once(&target).chain(&jumps).find(|l| !plain_login(l)) {
            Some(bad) => Err(Error::Malformed {
                at: format!("the SSH login `{bad}`"),
                reason: "a login takes letters, digits, `.`, `-`, `_`, `:` and one `@` only".to_owned(),
            }),
            None => Ok(Login { target, jumps }),
        }
    }

    /// The `ssh` arguments running `command` there, run from the run's state folder (the config
    /// named relative to it), the variables `sent` carried into the session (`SendEnv`: their
    /// values travel in the SSH channel from ssh's own environment, never on a command line; the
    /// guest's `sshd` takes those its `AcceptEnv` names).
    pub(crate) fn args(&self, sent: &[String], command: &str) -> Vec<String> {
        let mut args = vec!["-F".to_owned(), CONFIG_FILE.to_owned()];
        for var in sent {
            args.extend(["-o".to_owned(), format!("SendEnv={var}")]);
        }
        if !self.jumps.is_empty() {
            args.extend(["-J".to_owned(), self.jumps.join(",")]);
        }
        args.extend([self.target.clone(), command.to_owned()]);
        args
    }
}

/// Forgets the run's pinned host keys and SSH configuration, after a successful `down` whatever
/// the driver: a run brought up again meets new machines, maybe at the same addresses, whose new
/// keys a pinned one would refuse.
pub(crate) fn forget(cx: &Cx<'_>) -> Result<()> {
    let state = cx.run.runner.store.run_dir(cx.run.name())?;
    [KNOWN_HOSTS, CONFIG_FILE].iter().try_for_each(|file| {
        let path = state.join(file);
        crate::store::remove_path(&path).map_err(Error::io(&path, "removed"))
    })
}

/// The process running `command` at `login` for `cx`'s run, the variables of `env` carried into
/// the session: its configuration written first, in the run's state folder, where it runs.
pub(crate) fn spec(cx: &Cx<'_>, login: &Login, command: &str, env: Vec<(String, String)>) -> Result<ProcessSpec> {
    let home = &cx.run.runner.config.home;
    let state = cx.run.runner.store.run_dir(cx.run.name())?;
    let path = state.join(CONFIG_FILE);
    let text = config(&key_path(home), &state.join(KNOWN_HOSTS)).ok_or_else(|| {
        let why = "the runner's home is at a path SSH's config can't name (it holds `\"`, `\\` outside Windows, `${` or a control character)";
        Error::io(&path, "written")(std::io::Error::new(std::io::ErrorKind::InvalidInput, why))
    })?;
    write_private(&path, text.as_bytes()).map_err(Error::io(&path, "written"))?;
    let mut sent: Vec<String> = env.iter().map(|(k, _)| k.clone()).collect();
    sent.sort();
    sent.dedup();
    let mut spec = ProcessSpec::new("ssh", login.args(&sent, command));
    spec.cwd = Some(state);
    spec.env = env;
    Ok(spec)
}

/// The process of an `ssh.run` step: its `with` (`host`, `user`, `via`, `command`) filled, its
/// `stdin` piped, and the variables the manifest delivers to it by name carried into the guest
/// (never the Terraform variables every step gets: a server's password stays on this computer).
pub(crate) fn step_spec(cx: &Cx<'_>, list: &str, step: &Step, with: &BTreeMap<SnakeName, Template>) -> Result<ProcessSpec> {
    let at = plan::at(list, step);
    let arg = |name: &str| cx.vars.with_text(with, &at, name);
    let (host, command) = (arg("host")?, arg("command")?);
    if host.is_empty() || command.is_empty() {
        return Err(Error::Malformed {
            at,
            reason: "an `ssh.run` needs its `host` and `command`".to_owned(),
        });
    }
    let login = with_login(cx, &host, arg)?;
    let mut spec = spec(cx, &login, &command, crate::executor::env_deliveries(cx, step.id.as_str()))?;
    spec.stdin = crate::executor::stdin_of(cx, list, step)?;
    Ok(spec)
}

/// The login of an `ssh.run` at `host`, its other `with` arguments read by `arg`: its `user`
/// (root otherwise), through the run's jump then its `via`.
pub(crate) fn with_login(cx: &Cx<'_>, host: &str, arg: impl Fn(&str) -> Result<String>) -> Result<Login> {
    Login::new(&user_or_root(arg("user")?), host, cx.ssh_jump.as_deref(), &arg("via")?)
}

/// The output holding `machine`'s address: `address_<machine>`, `-` written `_`.
pub(crate) fn address_output(machine: &str) -> String {
    format!("address_{}", machine.replace('-', "_"))
}

/// The login to `machine` a run's module gives in its outputs (read by `output`): its address
/// ([`address_output`]) as its user (`ssh_user_<machine>`, `-` written `_`, else the run's
/// `ssh_user`, else `root`), through `jump`. `None` while the address has no value.
pub(crate) fn machine_login(output: impl Fn(&str) -> Option<String>, machine: &str, jump: Option<&str>) -> Result<Option<Login>> {
    let Some(host) = output(&address_output(machine)).filter(|a| !a.is_empty()) else {
        return Ok(None);
    };
    let key = machine.replace('-', "_");
    let user = user_or_root(output(&format!("ssh_user_{key}")).or_else(|| output("ssh_user")).unwrap_or_default());
    Login::new(&user, &host, jump, "").map(Some)
}

/// The user an SSH login takes: `user`, else `root` (every login the runner makes falls back so).
pub(crate) fn user_or_root(user: String) -> String {
    Some(user).filter(|u| !u.is_empty()).unwrap_or_else(|| "root".into())
}

/// ssh's own exit code when it couldn't connect (a command that ran may exit 255 too: it printed
/// something, or its failure is its own).
pub(crate) const UNREACHABLE: i32 = 255;

/// How long between two tries to reach a host that doesn't answer over SSH yet.
pub(crate) const RETRY: std::time::Duration = std::time::Duration::from_secs(5);

/// Whether a run of `ssh` that ended with `exit`, its command having `printed` on standard output
/// or not, is ssh itself not connecting: exit [`UNREACHABLE`] and nothing printed.
pub(crate) fn unreachable(exit: Exit, printed: bool) -> bool {
    exit.code == Some(UNREACHABLE) && !printed
}

/// Tries of a run of `ssh` until its host answers or a deadline passes: a host still booting.
#[derive(Debug)]
pub(crate) struct Retrying {
    /// When the tries stop.
    deadline: tokio::time::Instant,
    /// Whether a retry was made yet.
    retried: bool,
}

impl Retrying {
    /// Tries until `deadline`.
    pub(crate) fn until(deadline: tokio::time::Instant) -> Retrying {
        Retrying { deadline, retried: false }
    }

    /// Whether to try again after a try that ssh couldn't connect for (`unreachable`, as
    /// [`unreachable()`] says), once [`RETRY`] went by: never past the deadline. `waiting` is called
    /// before the first retry only. A try that connected is never tried again.
    pub(crate) async fn again(&mut self, unreachable: bool, waiting: impl FnOnce()) -> bool {
        if !unreachable || tokio::time::Instant::now() >= self.deadline {
            return false;
        }
        if !self.retried {
            waiting();
            self.retried = true;
        }
        tokio::time::sleep(RETRY).await;
        true
    }
}

/// The most an `ssh.run`'s `wait_s` may give: half an hour.
const WAIT_MAX: u64 = 1800;

/// How long an `ssh.run` waits for its host to answer, and the login it tries.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Wait {
    /// `wait_s`.
    pub(crate) limit: std::time::Duration,
    /// `user@host`, for the message when it never answers.
    pub(crate) login: String,
}

/// The wait of `step` when it is an `ssh.run` with a `wait_s` above 0: refused above
/// [`WAIT_MAX`] or when it isn't a number of seconds.
pub(crate) fn wait_of(cx: &Cx<'_>, list: &str, step: &Step) -> Result<Option<Wait>> {
    let isoloom_manifest::Run::Action {
        action: isoloom_manifest::Action::SshRun,
        with,
    } = &step.run
    else {
        return Ok(None);
    };
    let at = plan::at(list, step);
    let arg = |name: &str| cx.vars.with_text(with, &at, name);
    let text = arg("wait_s")?;
    if text.is_empty() {
        return Ok(None);
    }
    let malformed = |reason: String| Error::Malformed {
        at: format!("{at}.with.wait_s"),
        reason,
    };
    let secs: u64 = text.trim().parse().map_err(|_| malformed(format!("`{text}` isn't a number of seconds")))?;
    if secs > WAIT_MAX {
        return Err(malformed(format!("{secs} is over the {WAIT_MAX} seconds a step may wait")));
    }
    let login = with_login(cx, &arg("host")?, arg)?.target;
    Ok((secs > 0).then(|| Wait {
        limit: std::time::Duration::from_secs(secs),
        login,
    }))
}

#[cfg(test)]
mod tests {
    use proptest::prelude::*;

    use super::*;

    #[test]
    fn only_ssh_failing_to_connect_with_nothing_printed_is_unreachable() {
        assert!(unreachable(Exit::new(Some(UNREACHABLE)), false));
        assert!(!unreachable(Exit::new(Some(UNREACHABLE)), true), "the command ran: its own 255");
        assert!(!unreachable(Exit::new(Some(1)), false));
        assert!(!unreachable(Exit::new(None), false));
    }

    #[tokio::test(start_paused = true)]
    async fn an_unreachable_host_is_tried_again_every_retry_until_the_deadline() {
        let start = tokio::time::Instant::now();
        let mut retrying = Retrying::until(start + RETRY * 2);
        let mut said = 0;
        assert!(retrying.again(true, || said += 1).await);
        assert!(retrying.again(true, || said += 1).await);
        assert_eq!(start.elapsed(), RETRY * 2, "each retry after RETRY");
        assert!(!retrying.again(true, || said += 1).await, "not past the deadline");
        assert_eq!(said, 1, "said once, before the first retry");
        let mut connected = Retrying::until(start + RETRY * 10);
        assert!(
            !connected.again(false, || panic!("nothing to say")).await,
            "a try that connected is the outcome"
        );
        assert_eq!(start.elapsed(), RETRY * 2, "and isn't waited for");
    }

    /// Regression: a public key that couldn't be read was no key, and `ssh-keygen` was run over
    /// the existing one, failing on its overwrite prompt.
    #[tokio::test]
    async fn a_public_key_that_cant_be_read_is_an_error_and_no_new_key_is_made() {
        let home = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(key_path(home.path()).with_extension("pub")).unwrap();
        let fake = std::sync::Arc::new(crate::process::fake::FakeProcesses::new(|_| crate::process::fake::Reply::ok("")));
        let err = ensure_key(fake.as_ref(), home.path()).await.unwrap_err();
        assert_eq!(err.code(), crate::ErrorCode::Io, "{err}");
        assert!(err.to_string().contains("id_ed25519.pub"), "{err}");
        assert!(fake.calls().is_empty(), "no ssh-keygen: {:?}", fake.lines());
        assert_eq!(public_key(&home.path().join("elsewhere")).unwrap(), None, "a missing key is none");
    }

    #[test]
    fn a_machines_login_is_its_address_as_its_user_else_the_runs_else_root() {
        let outputs = |pairs: &'static [(&'static str, &'static str)]| move |name: &str| pairs.iter().find(|(k, _)| *k == name).map(|(_, v)| (*v).to_owned());
        let own = outputs(&[("address_web_1", "198.51.100.7"), ("ssh_user_web_1", "admin"), ("ssh_user", "ubuntu")]);
        let login = machine_login(own, "web-1", Some("me@jump")).unwrap().unwrap();
        assert_eq!(login.target, "admin@198.51.100.7");
        assert_eq!(login.jumps, ["me@jump"]);
        let run_wide = outputs(&[("address_web_1", "198.51.100.7"), ("ssh_user", "ubuntu")]);
        assert_eq!(machine_login(run_wide, "web-1", None).unwrap().unwrap().target, "ubuntu@198.51.100.7");
        let none = outputs(&[("address_web_1", "198.51.100.7")]);
        assert_eq!(machine_login(none, "web-1", None).unwrap().unwrap().target, "root@198.51.100.7");
        assert_eq!(machine_login(outputs(&[("address_web_1", "")]), "web-1", None).unwrap(), None, "no address yet");
        assert_eq!(machine_login(outputs(&[]), "web-1", None).unwrap(), None);
    }

    #[test]
    fn a_login_goes_through_the_runs_jump_then_the_manifests() {
        let l = Login::new("isoloom", "10.60.0.10", Some("root@pve.lan"), ",isoloom@10.10.10.5").unwrap();
        assert_eq!(l.jumps, ["root@pve.lan", "isoloom@10.10.10.5"]);
        assert_eq!(
            l.args(&[], "sh -s"),
            ["-F", "ssh_config", "-J", "root@pve.lan,isoloom@10.10.10.5", "isoloom@10.60.0.10", "sh -s"]
        );
        let direct = Login::new("isoloom", "10.0.0.9", None, "").unwrap();
        assert_eq!(direct.args(&[], "true"), ["-F", "ssh_config", "isoloom@10.0.0.9", "true"]);
    }

    #[test]
    fn the_variables_sent_are_named_before_the_host_their_values_never_in_the_arguments() {
        let l = Login::new("isoloom", "10.0.0.9", None, "").unwrap();
        let sent = ["DEMO_SECRET".to_owned(), "TOKEN".to_owned()];
        assert_eq!(
            l.args(&sent, "sudo -E docker compose up"),
            [
                "-F",
                "ssh_config",
                "-o",
                "SendEnv=DEMO_SECRET",
                "-o",
                "SendEnv=TOKEN",
                "isoloom@10.0.0.9",
                "sudo -E docker compose up"
            ]
        );
    }

    #[test]
    fn an_odd_login_is_refused() {
        for (user, host, via) in [
            ("isoloom", "10.0.0.5;rm -rf /", ""),
            ("-oProxyCommand=x", "h", ""),
            ("u", "h", "a@b@c"),
            ("u", "h", "-J x"),
        ] {
            assert!(Login::new(user, host, None, via).is_err(), "{user}@{host} via {via}");
        }
    }

    #[test]
    fn the_config_carries_the_key_and_the_runs_known_hosts_to_every_host() {
        let c = config(Path::new("/home/a b/.isothrum/ssh/id_ed25519"), Path::new("/r/known_hosts")).unwrap();
        assert!(c.contains("Host *\n"), "{c}");
        assert!(c.contains("IdentityFile \"/home/a b/.isothrum/ssh/id_ed25519\"\n"), "{c}");
        assert!(c.contains("UserKnownHostsFile \"/r/known_hosts\"\n"), "{c}");
        assert!(c.contains("StrictHostKeyChecking accept-new\n") && c.contains("BatchMode yes\n"), "{c}");
    }

    /// Quoted, a path reads the same to OpenSSH before 8.7 (no escapes) and after (`\` escapes).
    #[test]
    fn a_path_is_quoted_for_every_openssh_and_its_tokens_doubled() {
        assert_eq!(config_path("/home/a b/x 50%", false).as_deref(), Some("\"/home/a b/x 50%%\""));
        assert_eq!(config_path("it's", false).as_deref(), Some("\"it's\""));
        assert_eq!(
            config_path("C:\\Users\\a b\\.isothrum\\ssh\\id_ed25519", true).as_deref(),
            Some("\"C:/Users/a b/.isothrum/ssh/id_ed25519\"")
        );
        assert_eq!(
            config_path("\\\\server\\share\\h", true).as_deref(),
            Some("\"//server/share/h\""),
            "a network path"
        );
    }

    #[test]
    fn a_path_no_openssh_reads_as_written_is_refused() {
        for path in ["/a\"b", "/a\\b", "/a/${HOME}", "/a\nb"] {
            assert_eq!(config_path(path, false), None, "{path:?}");
        }
        assert_eq!(config_path("C:\\a\"b", true), None);
    }

    #[test]
    fn a_login_refused_says_what_a_login_takes() {
        let refused = Login::new("u", "h;x", None, "").unwrap_err().to_string();
        assert!(
            refused.ends_with("a login takes letters, digits, `.`, `-`, `_`, `:` and one `@` only"),
            "{refused}"
        );
        assert!(Login::new("u", "fd00::10", None, "").is_ok(), "`:` is one of them");
    }

    proptest! {
        #[test]
        fn a_plain_login_never_starts_an_option(user in "[a-z][a-z0-9_]{0,8}", host in "[a-z0-9.-]{1,20}") {
            let l = Login::new(&user, &host, None, "");
            prop_assert!(l.is_ok());
            let args = l.unwrap().args(&[], "true");
            prop_assert!(!args[2].starts_with('-'));
        }
    }
}
