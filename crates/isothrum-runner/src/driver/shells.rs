//! The shell scripts a console runs once it is in: a login shell on a VM, a shell in a container,
//! as the session's own account or as one the embedder names. Nothing of a shell is redirected: an
//! interactive shell writes its prompt and what is typed to its error output.

/// A login shell on a VM: bash where it has one.
pub(crate) const LOGIN_SHELL: &str = "if command -v bash >/dev/null 2>&1; then exec bash -l; else exec sh -l; fi";

/// A shell in a container: bash where its image has one.
pub(crate) const CONTAINER_SHELL: &str = "if command -v bash >/dev/null 2>&1; then exec bash; else exec sh; fi";

/// A login shell on a VM as `user`, from the session the way in opens (a VM's own login: Vagrant's
/// `vagrant`, a module's user): that session's shell when it is `user`'s already, else `sudo -i`
/// as `user` (its login shell, in its home). `user` is a checked account name
/// ([`crate::console::check_user`]): nothing in it needs quoting, and the script has no double
/// quote, which a Windows host's command line would have to escape on its way to `vagrant` or
/// `ssh` (`id -un` prints one word).
pub(crate) fn login_shell_as(user: &str) -> String {
    format!("if [ $(id -un) = {user} ]; then {LOGIN_SHELL}; else exec sudo -iu {user}; fi")
}

/// A shell in a container entered as its user already (`exec --user`): a login shell, in its home
/// (`cd` with no folder: `$HOME`, which the engine sets from the account), as a login on a VM is.
pub(crate) fn container_shell_as() -> String {
    format!("cd 2>/dev/null; {LOGIN_SHELL}")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The scripts are sh: a console that runs them doesn't fail on a quote or a brace.
    #[cfg(unix)]
    #[test]
    fn the_scripts_as_an_account_are_sh() {
        for script in [login_shell_as("user"), container_shell_as()] {
            let parsed = std::process::Command::new("sh").args(["-n", "-c", &script]).status();
            assert!(parsed.is_ok_and(|s| s.success()), "{script}");
        }
    }

    /// From another account's session (Vagrant's), the script hands over to `sudo -iu` the account.
    #[cfg(unix)]
    #[test]
    fn from_another_account_the_login_is_sudo_as_the_account() {
        use std::os::unix::fs::PermissionsExt;
        let bin = tempfile::tempdir().unwrap();
        for (tool, says) in [("id", "echo vagrant"), ("sudo", "echo \"sudo $*\"")] {
            let path = bin.path().join(tool);
            std::fs::write(&path, format!("#!/bin/sh\n{says}\n")).unwrap();
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        let path = format!("{}:{}", bin.path().display(), std::env::var("PATH").unwrap_or_default());
        let out = std::process::Command::new("sh")
            .args(["-c", &login_shell_as("user")])
            .env("PATH", path)
            .output()
            .unwrap();
        assert_eq!(String::from_utf8_lossy(&out.stdout), "sudo -iu user\n");
    }

    /// Run as the account already, the login shell is the session's own; as another, `sudo -i`.
    #[test]
    fn a_login_as_an_account_is_a_sudo_login_unless_it_is_the_sessions() {
        let script = login_shell_as("user");
        assert_eq!(
            script,
            "if [ $(id -un) = user ]; then if command -v bash >/dev/null 2>&1; then exec bash -l; else exec sh -l; fi; else exec sudo -iu user; fi"
        );
    }
}
