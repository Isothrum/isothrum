//! An owner-only home on Windows, where file modes don't exist: a protected DACL on the home
//! folder that every folder and file under it inherits (its runs, its SSH key, the plugin cache).
//! Set with `icacls` (in every Windows), since this crate makes no FFI call.
//!
//! A home inside the user's profile (`~/.isothrum`, the default) needs none: the profile's own
//! ACL (the account, SYSTEM, Administrators) is what its new folders inherit. Elsewhere (a
//! `D:\isothrum`, whose ACL lets every local account read) the home gets that same ACL, protected
//! from what its parent would pass down: the account, SYSTEM, and Administrators, whom Windows
//! lets take any file over anyway and whose elevated session (Hyper-V's) writes in a run's
//! state.

use std::path::{Component, Path};

/// SYSTEM and the local Administrators group, granted beside the account.
const SYSTEM_AND_ADMINISTRATORS: [&str; 2] = ["S-1-5-18", "S-1-5-32-544"];

/// Gives `home` an owner-only, protected DACL, unless it is inside the user's `profile`
/// (`USERPROFILE`, as the runner read it).
///
/// # Errors
///
/// The account's SID can't be read (`whoami`), or `icacls` fails: the home is left as it was,
/// and the runner doesn't start in it.
#[cfg(windows)]
pub(super) fn owner_only(home: &Path, profile: Option<&Path>) -> std::io::Result<()> {
    let home = std::fs::canonicalize(home)?;
    let profile = profile.and_then(|p| std::fs::canonicalize(p).ok());
    if profile.is_some_and(|p| inside(&home, &p)) {
        return Ok(());
    }
    protect(&home)
}

/// Gives `folder` the owner-only DACL, protected from what its parent passes down.
#[cfg(windows)]
fn protect(folder: &Path) -> std::io::Result<()> {
    let whoami = std::process::Command::new("whoami").args(["/user", "/fo", "csv", "/nh"]).output()?;
    let sid = account_sid(&String::from_utf8_lossy(&whoami.stdout)).ok_or_else(|| {
        std::io::Error::other(format!(
            "`whoami /user` didn't name the account: {}",
            String::from_utf8_lossy(&whoami.stderr).trim()
        ))
    })?;
    let icacls = std::process::Command::new("icacls").arg(folder).args(grants(&sid)).output()?;
    if icacls.status.success() {
        Ok(())
    } else {
        let output = String::from_utf8_lossy(&icacls.stdout).trim().to_owned();
        Err(std::io::Error::other(format!("icacls couldn't make it owner-only: {output}")))
    }
}

/// Whether `path` is `folder` or inside it, comparing as Windows does (any case, either slash).
fn inside(path: &Path, folder: &Path) -> bool {
    let parts = |p: &Path| -> Vec<String> {
        p.components()
            .filter(|c| !matches!(c, Component::CurDir))
            .map(|c| c.as_os_str().to_string_lossy().to_lowercase())
            .flat_map(|c| c.split(['\\', '/']).filter(|s| !s.is_empty()).map(str::to_owned).collect::<Vec<_>>())
            .collect()
    };
    let (path, folder) = (parts(path), parts(folder));
    !folder.is_empty() && path.starts_with(&folder)
}

/// The account's SID, from `whoami /user /fo csv /nh` (`"host\ada","S-1-5-21-…"`).
fn account_sid(whoami: &str) -> Option<String> {
    let sid = whoami.trim().rsplit(',').next()?.trim().trim_matches('"');
    sid.starts_with("S-1-").then(|| sid.to_owned())
}

/// `icacls`' arguments: inheritance from the parent removed, full control to the account,
/// SYSTEM and Administrators, inherited by every folder and file under it.
fn grants(sid: &str) -> Vec<String> {
    let mut args = vec!["/inheritance:r".to_owned()];
    for who in std::iter::once(sid).chain(SYSTEM_AND_ADMINISTRATORS) {
        args.extend(["/grant:r".to_owned(), format!("*{who}:(OI)(CI)F")]);
    }
    args.push("/Q".to_owned());
    args
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_home_in_the_profile_is_inside_it_whatever_the_case_or_slash() {
        let profile = Path::new("C:\\Users\\Ada");
        assert!(inside(Path::new("c:\\users\\ada\\.isothrum"), profile));
        assert!(inside(Path::new("C:/Users/Ada/AppData/isothrum"), profile));
        assert!(inside(Path::new("/home/ada/.isothrum"), Path::new("/home/ada")));
        assert!(!inside(Path::new("C:\\Users\\Adam\\.isothrum"), profile), "a sibling isn't inside");
        assert!(!inside(Path::new("D:\\isothrum"), profile));
        assert!(!inside(Path::new("D:\\isothrum"), Path::new("")));
    }

    #[test]
    fn the_account_is_read_from_whoami() {
        let sid = "S-1-5-21-1004336348-1177238915-682003330-1001";
        assert_eq!(account_sid(&format!("\"desktop-7\\ada\",\"{sid}\"\r\n")).as_deref(), Some(sid));
        assert_eq!(account_sid("ERROR: access denied"), None);
    }

    #[test]
    fn the_dacl_is_protected_and_inherited_by_everything_under_it() {
        assert_eq!(
            grants("S-1-5-21-1"),
            [
                "/inheritance:r",
                "/grant:r",
                "*S-1-5-21-1:(OI)(CI)F",
                "/grant:r",
                "*S-1-5-18:(OI)(CI)F",
                "/grant:r",
                "*S-1-5-32-544:(OI)(CI)F",
                "/Q"
            ]
        );
    }

    /// On a real Windows (`cargo test -p isothrum-runner protected`): the folder's ACL lists the
    /// three grants, none inherited from its parent.
    #[cfg(windows)]
    #[test]
    fn a_protected_folder_keeps_only_its_own_grants() {
        let dir = tempfile::tempdir().unwrap();
        protect(dir.path()).unwrap();
        let out = std::process::Command::new("icacls").arg(dir.path()).output().unwrap();
        let acl = String::from_utf8_lossy(&out.stdout);
        assert_eq!(acl.matches("(OI)(CI)(F)").count(), 3, "{acl}");
        assert!(!acl.contains("(I)"), "nothing inherited: {acl}");
    }
}
