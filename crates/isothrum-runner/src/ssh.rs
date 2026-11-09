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
use crate::executor::Cx;
use crate::process::{ProcessRunner, ProcessSpec};
use crate::store::write_private;

/// The runner's key, under its home's `ssh/` folder.
const KEY_FILE: &str = "id_ed25519";

/// The run's SSH configuration, in its state folder.
const CONFIG_FILE: &str = "ssh_config";

/// How long making the key may take.
const KEYGEN_LIMIT: std::time::Duration = std::time::Duration::from_secs(60);

/// The id a failure to make the key reads as.
const KEY_STEP: &str = "ssh-key";

/// The runner's private key: `<home>/ssh/id_ed25519`.
pub(crate) fn key_path(home: &Path) -> PathBuf {
    home.join("ssh").join(KEY_FILE)
}

/// The public half of the runner's key, one line, when it exists.
pub(crate) fn public_key(home: &Path) -> Option<String> {
    let text = std::fs::read_to_string(key_path(home).with_extension("pub")).ok()?;
    Some(text.trim().to_owned()).filter(|k| !k.is_empty())
}

/// Makes the runner's key when it has none (`ssh-keygen`, no passphrase), and returns its public
/// half.
pub(crate) async fn ensure_key(processes: &dyn ProcessRunner, home: &Path) -> Result<String> {
    if let Some(key) = public_key(home) {
        return Ok(key);
    }
    let dir = home.join("ssh");
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
        return Err(failed(crate::executor::why_failed(&out)));
    }
    public_key(home).ok_or_else(|| failed("ssh-keygen wrote no public key".into()))
}

/// Whether a run of `manifest` needs the runner's key: it reaches a machine over SSH, or names
/// the key.
pub(crate) fn needed(manifest: &isoloom_manifest::Manifest) -> bool {
    use isoloom_manifest::{Action, Condition, Part, Placeholder, Run, RunVar};
    let names_key = manifest.templates().iter().any(|(_, t)| {
        t.parts()
            .iter()
            .any(|p| matches!(p, Part::Placeholder(Placeholder::Run(RunVar::SshKey | RunVar::SshPublicKey))))
    });
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
/// minute.
fn config(key: &Path, known_hosts: &Path) -> String {
    format!(
        "# Written by the Isothrum runner for this run.\nHost *\n  IdentityFile {}\n  IdentitiesOnly yes\n  UserKnownHostsFile {}\n  StrictHostKeyChecking accept-new\n  BatchMode yes\n  ConnectTimeout 10\n  ServerAliveInterval 15\n  ServerAliveCountMax 3\n  LogLevel ERROR\n",
        config_path(&key.to_string_lossy()),
        config_path(&known_hosts.to_string_lossy()),
    )
}

/// A path as an ssh config file reads it: its spaces, quotes and backslashes escaped, and `%`
/// (a token there) doubled.
fn config_path(path: &str) -> String {
    let mut out = String::with_capacity(path.len());
    for c in path.chars() {
        match c {
            '\\' | '"' | '\'' | ' ' => {
                out.push('\\');
                out.push(c);
            }
            '%' => out.push_str("%%"),
            _ => out.push(c),
        }
    }
    out
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
                reason: "a login takes letters, digits, `.`, `-`, `_` and one `@` only".to_owned(),
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

/// The process running `command` at `login` for `cx`'s run, the variables of `env` carried into
/// the session: its configuration written first, in the run's state folder, where it runs.
pub(crate) fn spec(cx: &Cx<'_>, login: &Login, command: &str, env: Vec<(String, String)>) -> Result<ProcessSpec> {
    let home = &cx.run.runner.config.home;
    let state = cx.run.runner.store.run_dir(cx.run.name())?;
    let path = state.join(CONFIG_FILE);
    let text = config(&key_path(home), &state.join("known_hosts"));
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
    let at = format!("{list}.{}", step.id);
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
    let at = format!("{list}.{}", step.id);
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
    let login = format!("{}@{}", user_or_root(arg("user")?), arg("host")?);
    Ok((secs > 0).then(|| Wait {
        limit: std::time::Duration::from_secs(secs),
        login,
    }))
}

/// `s` single-quoted for the remote shell (`'` written `'\''`).
pub(crate) fn sh_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', r"'\''"))
}

#[cfg(test)]
mod tests {
    use proptest::prelude::*;

    use super::*;

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
        let c = config(Path::new("/home/a b/.isothrum/ssh/id_ed25519"), Path::new("/r/known_hosts"));
        assert!(c.contains("Host *\n"), "{c}");
        assert!(c.contains("IdentityFile /home/a\\ b/.isothrum/ssh/id_ed25519\n"), "{c}");
        assert!(c.contains("UserKnownHostsFile /r/known_hosts\n"), "{c}");
        assert!(c.contains("StrictHostKeyChecking accept-new\n") && c.contains("BatchMode yes\n"), "{c}");
        assert_eq!(config_path("C:\\Users\\x %1"), "C:\\\\Users\\\\x\\ %%1");
    }

    proptest! {
        #[test]
        fn a_quoted_word_reads_back_as_itself(s in ".{0,40}") {
            // `'…'` with each `'` as `'\''`: undo it the way sh reads it.
            let q = sh_quote(&s);
            let inner = q.strip_prefix('\'').and_then(|q| q.strip_suffix('\'')).unwrap();
            prop_assert_eq!(inner.replace(r"'\''", "'"), s);
        }

        #[test]
        fn a_plain_login_never_starts_an_option(user in "[a-z][a-z0-9_]{0,8}", host in "[a-z0-9.-]{1,20}") {
            let l = Login::new(&user, &host, None, "");
            prop_assert!(l.is_ok());
            let args = l.unwrap().args(&[], "true");
            prop_assert!(!args[2].starts_with('-'));
        }
    }
}
