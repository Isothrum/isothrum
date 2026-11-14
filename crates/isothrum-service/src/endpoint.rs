//! The local endpoint: a Unix domain socket in the home, owner-only, on Unix; a named pipe of a
//! random name only the current user may open, remote clients refused, on Windows.

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
    let path = paths.socket();
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
