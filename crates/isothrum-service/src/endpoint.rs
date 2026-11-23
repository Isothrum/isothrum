//! The local endpoint: a Unix domain socket in the home, owner-only, on Unix; a named pipe of a
//! random name only the current user may open, remote clients refused, on Windows.
//!
//! A socket's path has a limit (103 bytes on macOS and the BSDs, 107 on Linux). A home on a
//! longer path has its socket in a folder of the user's own on a short one instead: the user's
//! runtime folder (`XDG_RUNTIME_DIR`) or temporary folder, in `isothrum-<uid>/` (made owner-only,
//! refused when another account owns it or may enter it), named after the home. `service.json`
//! names it, so a client finds it there as anywhere.

use interprocess::local_socket::tokio::Listener;
use interprocess::local_socket::{GenericFilePath, ListenerOptions, ToFsName as _};
use isothrum_client::ServicePaths;

use crate::error::ServeError;

/// Listens for the home of `paths`: the listener and its endpoint's name.
///
/// # Errors
///
/// The endpoint can't be created or made owner-only.
#[cfg(unix)]
pub(crate) fn listen(paths: &ServicePaths) -> Result<(Listener, String), ServeError> {
    use std::os::unix::fs::PermissionsExt as _;
    let path = socket_path(paths).map_err(|source| ServeError::Listen {
        endpoint: paths.socket().display().to_string(),
        source,
    })?;
    let endpoint = path.to_str().map(str::to_owned).ok_or_else(|| ServeError::Listen {
        endpoint: path.display().to_string(),
        source: std::io::Error::new(std::io::ErrorKind::InvalidInput, "the home's path isn't UTF-8"),
    })?;
    let failed = |source| ServeError::Listen {
        endpoint: endpoint.clone(),
        source,
    };
    // The lease is held: a socket there is a dead service's.
    match std::fs::remove_file(&path) {
        Err(e) if e.kind() != std::io::ErrorKind::NotFound => return Err(failed(e)),
        _ => {}
    }
    let name = endpoint.as_str().to_fs_name::<GenericFilePath>().map_err(failed)?;
    let listener = ListenerOptions::new().name(name).create_tokio().map_err(failed)?;
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).map_err(failed)?;
    Ok((listener, endpoint))
}

/// The longest socket path the OS takes: `sun_path` less its closing NUL.
#[cfg(unix)]
const SOCKET_MAX: usize = if cfg!(target_os = "linux") { 107 } else { 103 };

/// Where the home of `paths` listens: its own `service.sock`, else, for a home on a path too long
/// for a socket, a socket named after the home in the user's own short folder ([`short_folder`]).
///
/// # Errors
///
/// The short folder can't be made, or isn't the user's alone, or is too long itself.
#[cfg(unix)]
fn socket_path(paths: &ServicePaths) -> std::io::Result<std::path::PathBuf> {
    let base = std::env::var_os("XDG_RUNTIME_DIR")
        .map(std::path::PathBuf::from)
        .filter(|p| p.is_absolute() && p.is_dir())
        .unwrap_or_else(std::env::temp_dir);
    socket_path_in(paths, &base)
}

/// [`socket_path`], a long home's socket in `isothrum-<uid>/` of `base`.
#[cfg(unix)]
fn socket_path_in(paths: &ServicePaths, base: &std::path::Path) -> std::io::Result<std::path::PathBuf> {
    let own = paths.socket();
    if own.as_os_str().len() <= SOCKET_MAX {
        return Ok(own);
    }
    let home = paths.home();
    let path = short_folder(home, base)?.join(format!("{:016x}.sock", fnv1a(home.as_os_str().as_encoded_bytes())));
    if path.as_os_str().len() > SOCKET_MAX {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!("the home's socket path is too long, and so is {}", path.display()),
        ));
    }
    Ok(path)
}

/// `isothrum-<uid>/` in `base` (the user's runtime folder, `XDG_RUNTIME_DIR`, else the temporary
/// folder): made owner-only when missing; refused when another account owns it or it lets another
/// in. `home`'s owner is the user.
#[cfg(unix)]
fn short_folder(home: &std::path::Path, base: &std::path::Path) -> std::io::Result<std::path::PathBuf> {
    use std::os::unix::fs::{DirBuilderExt as _, MetadataExt as _};
    let uid = std::fs::metadata(home)?.uid();
    let folder = base.join(format!("isothrum-{uid}"));
    match std::fs::DirBuilder::new().mode(0o700).create(&folder) {
        Err(e) if e.kind() != std::io::ErrorKind::AlreadyExists => return Err(e),
        _ => {}
    }
    let found = std::fs::symlink_metadata(&folder)?;
    if !found.is_dir() || found.uid() != uid || found.mode() & 0o077 != 0 {
        return Err(std::io::Error::new(
            std::io::ErrorKind::PermissionDenied,
            format!("{} isn't a folder this account alone owns and enters", folder.display()),
        ));
    }
    Ok(folder)
}

/// FNV-1a of `bytes`: a short, stable name for a home.
#[cfg(unix)]
fn fnv1a(bytes: &[u8]) -> u64 {
    bytes
        .iter()
        .fold(0xcbf2_9ce4_8422_2325_u64, |h, b| (h ^ u64::from(*b)).wrapping_mul(0x0000_0100_0000_01b3))
}

/// Listens for the home of `paths`: the listener and its endpoint's name.
///
/// # Errors
///
/// The pipe can't be created, or the account's SID can't be read for its ACL.
#[cfg(windows)]
pub(crate) fn listen(_paths: &ServicePaths) -> Result<(Listener, String), ServeError> {
    use interprocess::os::windows::local_socket::ListenerOptionsExt as _;
    use interprocess::os::windows::security_descriptor::SecurityDescriptor;
    let endpoint = format!(r"\\.\pipe\isothrum-{}", crate::random_hex());
    let failed = |source| ServeError::Listen {
        endpoint: endpoint.clone(),
        source,
    };
    let sid = account_sid().map_err(failed)?;
    // Only the account (generic all), protected from inherited entries.
    let sddl = widestring::U16CString::from_str(format!("D:P(A;;GA;;;{sid})")).map_err(|e| failed(std::io::Error::other(e)))?;
    let descriptor = SecurityDescriptor::deserialize(&sddl).map_err(failed)?;
    let name = endpoint.as_str().to_fs_name::<GenericFilePath>().map_err(failed)?;
    let listener = ListenerOptions::new()
        .name(name)
        .security_descriptor(descriptor)
        .create_tokio()
        .map_err(failed)?;
    Ok((listener, endpoint))
}

/// The account's SID, from `whoami /user /fo csv /nh` (`"host\ada","S-1-5-21-…"`): no FFI.
#[cfg(windows)]
#[expect(
    clippy::disallowed_methods,
    reason = "the service secures its endpoint before any run exists, with no process runner to hand"
)]
fn account_sid() -> std::io::Result<String> {
    let out = std::process::Command::new("whoami").args(["/user", "/fo", "csv", "/nh"]).output()?;
    let text = String::from_utf8_lossy(&out.stdout);
    text.trim()
        .rsplit(',')
        .next()
        .map(|field| field.trim().trim_matches('"').to_owned())
        .filter(|sid| sid.starts_with("S-1-"))
        .ok_or_else(|| {
            std::io::Error::other(format!(
                "`whoami /user` didn't name the account: {}",
                String::from_utf8_lossy(&out.stderr).trim()
            ))
        })
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    #[test]
    fn a_home_on_a_short_path_listens_in_it_and_one_on_a_long_path_in_the_user_s_own_folder() {
        use std::os::unix::fs::PermissionsExt as _;
        let short = tempfile::Builder::new().prefix("h").tempdir_in("/tmp").expect("a home");
        let base = short.path().join("run");
        std::fs::create_dir(&base).expect("a runtime folder");
        let own = ServicePaths::new(short.path().to_path_buf());
        assert_eq!(socket_path_in(&own, &base).expect("a socket"), own.socket());

        let long = short.path().join("a".repeat(60)).join("b".repeat(60));
        std::fs::create_dir_all(&long).expect("a long home");
        let deep = ServicePaths::new(long.clone());
        let at = socket_path_in(&deep, &base).expect("a socket");
        assert!(at.as_os_str().len() <= SOCKET_MAX, "{}", at.display());
        assert_ne!(at, deep.socket());
        assert_eq!(socket_path_in(&deep, &base).expect("the same"), at, "the same home, the same socket");
        let other = ServicePaths::new(long.join("c"));
        std::fs::create_dir_all(other.home()).expect("another long home");
        assert_ne!(socket_path_in(&other, &base).expect("a socket"), at, "another home, another socket");
        let folder = at.parent().expect("its folder");
        assert_eq!(std::fs::metadata(folder).expect("there").permissions().mode() & 0o777, 0o700);
    }
}
