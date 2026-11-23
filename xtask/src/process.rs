//! Running the tools the tasks drive: what to say when one fails, and the program to start on
//! Windows, where npm's tools are `.cmd` shims.

use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode, Stdio};
use std::sync::OnceLock;

use crate::Res;

/// The command as one line, for messages.
pub(crate) fn describe(cmd: &Command) -> String {
    let mut parts = vec![cmd.get_program().to_string_lossy().into_owned()];
    parts.extend(cmd.get_args().map(|a| a.to_string_lossy().into_owned()));
    parts.join(" ")
}

/// The variables `cargo run` sets for the xtask itself (`CARGO_PKG_NAME`, `CARGO_MANIFEST_DIR`
/// and the like) taken out of `cmd`'s environment: they describe the xtask, not what it runs,
/// and some tools read them (cargo-machete takes `CARGO_PKG_NAME` as a sign it runs inside a
/// build script, and reads its arguments wrong).
pub(crate) fn without_run_env(cmd: &mut Command) -> &mut Command {
    const SET_BY_RUN: [&str; 4] = ["CARGO_CRATE_NAME", "CARGO_BIN_NAME", "CARGO_PRIMARY_PACKAGE", "CARGO_RUSTC_CURRENT_DIR"];
    for (key, _) in std::env::vars_os() {
        let name = key.to_string_lossy();
        if name.starts_with("CARGO_PKG_") || name.starts_with("CARGO_MANIFEST_") || SET_BY_RUN.contains(&name.as_ref()) {
            cmd.env_remove(&key);
        }
    }
    cmd
}

/// `cargo`, the one running this xtask (`$CARGO`, so the same toolchain), in `dir`, without the
/// variables `cargo run` set for the xtask, compiling through sccache when it is installed
/// ([`compiler_cache`]).
pub(crate) fn cargo(dir: &Path) -> Command {
    let mut cmd = Command::new(std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into()));
    without_run_env(&mut cmd);
    compiler_cache(&mut cmd);
    cmd.current_dir(dir);
    cmd
}

/// `cargo` through rustup's proxy, in `dir`: the toolchain is the one a `+<toolchain>` argument
/// names, else the folder's `rust-toolchain.toml`, never the one running this xtask.
pub(crate) fn rustup_cargo(dir: &Path) -> Command {
    let mut cmd = Command::new("cargo");
    without_run_env(&mut cmd);
    compiler_cache(&mut cmd);
    cmd.env_remove("RUSTUP_TOOLCHAIN").current_dir(dir);
    cmd
}

/// sccache as `cmd`'s compiler wrapper (`RUSTC_WRAPPER`) when it is installed and no wrapper is
/// set (an empty `RUSTC_WRAPPER` says none): a crate another worktree or target folder already
/// compiled comes from its cache. Never required: without it, `cmd` is left as it is.
fn compiler_cache(cmd: &mut Command) -> &mut Command {
    static INSTALLED: OnceLock<bool> = OnceLock::new();
    let installed = *INSTALLED.get_or_init(|| {
        Command::new("sccache")
            .arg("--version")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .is_ok_and(|s| s.success())
    });
    if installed && std::env::var_os("RUSTC_WRAPPER").is_none() {
        cmd.env("RUSTC_WRAPPER", "sccache");
    }
    cmd
}

/// Runs a command attached to the terminal; whether it succeeded. An error only when it can't
/// start.
pub(crate) fn passed(cmd: &mut Command) -> Res<bool> {
    let status = cmd.status().map_err(|e| format!("`{}` couldn't start: {e}", describe(cmd)))?;
    Ok(status.success())
}

/// Runs a command attached to the terminal; an error names it when it fails.
pub(crate) fn run(cmd: &mut Command) -> Res<()> {
    if passed(cmd)? {
        Ok(())
    } else {
        Err(format!("`{}` failed", describe(cmd)).into())
    }
}

/// The exit code of a task whose steps all had to pass.
pub(crate) fn exit_code(ok: bool) -> ExitCode {
    if ok { ExitCode::SUCCESS } else { ExitCode::FAILURE }
}

/// A command's standard output, when it succeeds.
pub(crate) fn output(cmd: &mut Command) -> Res<String> {
    let out = cmd
        .stderr(Stdio::inherit())
        .output()
        .map_err(|e| format!("`{}` couldn't start: {e}", describe(cmd)))?;
    if out.status.success() {
        Ok(String::from_utf8_lossy(&out.stdout).into_owned())
    } else {
        Err(format!("`{}` failed ({})", describe(cmd), out.status).into())
    }
}

/// Runs a command with its output captured: nothing on the terminal, and the end of what it
/// said in the error when it fails.
pub(crate) fn quiet(cmd: &mut Command) -> Res<()> {
    let out = cmd.output().map_err(|e| format!("`{}` couldn't start: {e}", describe(cmd)))?;
    if out.status.success() {
        return Ok(());
    }
    let said = String::from_utf8_lossy(&out.stderr);
    let tail: Vec<&str> = said.lines().rev().take(5).collect::<Vec<_>>().into_iter().rev().collect();
    Err(format!("`{}` failed ({}): {}", describe(cmd), out.status, tail.join(" / ")).into())
}

/// The first line a command prints on success, or `None` (not installed, or it failed).
pub(crate) fn first_line(cmd: &mut Command) -> Option<String> {
    let out = cmd.stdin(Stdio::null()).stderr(Stdio::null()).output().ok()?;
    let text = String::from_utf8_lossy(&out.stdout);
    out.status.success().then(|| text.lines().next().unwrap_or("").trim().to_owned())
}

/// Fails with what to install when `cargo <tool> --version` doesn't answer in `dir`.
pub(crate) fn require_cargo_tool(dir: &Path, tool: &str) -> Res<()> {
    match first_line(cargo(dir).args([tool, "--version"])) {
        Some(_) => Ok(()),
        None => Err(format!("cargo-{tool} is missing: cargo install --locked cargo-{tool}").into()),
    }
}

/// The program to start for `name` with `path` as the search path. On Windows a tool npm
/// installed is a `.cmd` shim, which `Command` doesn't look for: its full path is returned.
/// Elsewhere, and for a name not found, `name` itself.
pub(crate) fn program(name: &str, path: &OsStr) -> OsString {
    if !cfg!(windows) || Path::new(name).extension().is_some() {
        return name.into();
    }
    let found = std::env::split_paths(path).find_map(|dir| ["exe", "cmd", "bat"].iter().map(|ext| dir.join(format!("{name}.{ext}"))).find(|p| p.is_file()));
    found.map_or_else(|| name.into(), PathBuf::into_os_string)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_command_is_described_as_one_line() {
        let mut cmd = Command::new("cargo");
        cmd.args(["hack", "check", "--workspace"]);
        assert_eq!(describe(&cmd), "cargo hack check --workspace");
    }

    #[test]
    fn a_program_with_an_extension_is_started_as_named() {
        assert_eq!(program("npx.cmd", OsStr::new("")), OsString::from("npx.cmd"));
    }
}
