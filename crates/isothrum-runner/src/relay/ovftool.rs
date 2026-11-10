//! The `ovftool` wrapper: vagrant-vmware-esxi uploads VMs with ovftool to a `vi://` URL it builds
//! from the host name alone, so the wrapper adds the HTTPS relay's port before running the real
//! one.

use std::ffi::{OsStr, OsString};
use std::io;
use std::path::{Path, PathBuf};

/// `ovftool` that adds the relay's port to a `vi://…@127.0.0.1` target (the plugin builds the URL
/// from the host name alone, so it can't carry a port) and runs the real one.
const WRAPPER: &str = r#"#!/bin/sh
# Written by the Isothrum runner: sends ovftool's ESXi target through its loopback relay.
n=$#; i=0
while [ "$i" -lt "$n" ]; do
  a=$1; shift
  case "$a" in
    vi://*@127.0.0.1*) a=$(printf '%s' "$a" | /usr/bin/sed -E "s#@127\.0\.0\.1(/|\$)#@127.0.0.1:${ISOTHRUM_OVFTOOL_PORT}\1#") ;;
  esac
  set -- "$@" "$a"; i=$((i + 1))
done
exec "$ISOTHRUM_REAL_OVFTOOL" "$@"
"#;

/// VMware's own ovftool, where Fusion installs it.
const FUSION_OVFTOOL: &str = "/Applications/VMware Fusion.app/Contents/Library/VMware OVF Tool/ovftool";

/// The ovftool the wrapper hands over to: the one on `path` (the run's `PATH`), else VMware
/// Fusion's own.
pub(super) fn real(path: Option<&OsStr>) -> Option<PathBuf> {
    let on_path = path.and_then(|p| std::env::split_paths(p).map(|d| d.join("ovftool")).find(|f| f.is_file()));
    on_path.or_else(|| Some(PathBuf::from(FUSION_OVFTOOL)).filter(|f| f.is_file()))
}

/// `PATH` with `dir` first, then the run's `path` (none: `dir` alone).
pub(super) fn first_on(dir: &Path, path: Option<&OsStr>) -> io::Result<String> {
    let folders = std::iter::once(dir.to_path_buf()).chain(path.into_iter().flat_map(std::env::split_paths));
    let joined = std::env::join_paths(folders).map_err(|e| io::Error::new(io::ErrorKind::InvalidInput, e))?;
    text(joined, "the PATH with the ovftool wrapper first")
}

/// `value` as the text every variable a tool is handed is: one that isn't UTF-8, `what`, is an
/// error.
pub(super) fn text(value: OsString, what: &str) -> io::Result<String> {
    value
        .into_string()
        .map_err(|v| io::Error::new(io::ErrorKind::InvalidData, format!("{what} isn't UTF-8: {}", v.display())))
}

/// A folder under `home` holding the wrapper as `ovftool` (written when missing or changed), to
/// put first on `PATH`.
pub(super) fn wrapper_dir(home: &Path) -> std::io::Result<PathBuf> {
    let dir = home.join("relay");
    crate::store::private_dir(&dir)?;
    let f = dir.join("ovftool");
    if std::fs::read_to_string(&f).ok().as_deref() != Some(WRAPPER) {
        crate::store::write_private(&f, WRAPPER.as_bytes())?;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&f, std::fs::Permissions::from_mode(0o700))?;
    }
    Ok(dir)
}

#[cfg(all(test, unix))]
mod tests {
    use std::os::unix::ffi::OsStringExt;

    use super::*;

    /// Regression: a `PATH` that isn't UTF-8 was read as none, so the ovftool on it was never
    /// found.
    #[test]
    fn ovftool_is_found_on_a_path_that_isnt_utf8() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("ovftool"), "").unwrap();
        let mut path = b"/nowhere/bad\xff:".to_vec();
        path.extend_from_slice(dir.path().as_os_str().as_encoded_bytes());
        assert_eq!(real(Some(&OsString::from_vec(path))), Some(dir.path().join("ovftool")));
    }

    /// Regression: with no `PATH`, the wrapper's folder was followed by an empty entry (`dir:`),
    /// which a shell reads as the current folder.
    #[test]
    fn the_wrapper_goes_first_on_the_path_and_alone_on_none() {
        let dir = Path::new("/home/ada/.isothrum/relay");
        assert_eq!(
            first_on(dir, Some(OsStr::new("/usr/bin:/bin"))).unwrap(),
            "/home/ada/.isothrum/relay:/usr/bin:/bin"
        );
        assert_eq!(first_on(dir, None).unwrap(), "/home/ada/.isothrum/relay");
        let refused = first_on(dir, Some(&OsString::from_vec(b"/bad\xff".to_vec()))).unwrap_err();
        assert!(refused.to_string().contains("isn't UTF-8"), "{refused}");
    }

    #[test]
    fn the_wrapper_adds_the_relay_port_to_the_vi_target() {
        let home = tempfile::tempdir().unwrap();
        let dir = wrapper_dir(home.path()).unwrap();
        let out = std::process::Command::new(dir.join("ovftool"))
            .args(["--noSSLVerify", "box.vmx", "vi://root:p%40ss@127.0.0.1/pool"])
            .env("ISOTHRUM_OVFTOOL_PORT", "40443")
            .env("ISOTHRUM_REAL_OVFTOOL", "/bin/echo")
            .output()
            .unwrap();
        assert_eq!(
            String::from_utf8_lossy(&out.stdout).trim(),
            "--noSSLVerify box.vmx vi://root:p%40ss@127.0.0.1:40443/pool"
        );
    }
}
