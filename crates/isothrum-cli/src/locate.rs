//! Where the runner's home is.

use std::path::PathBuf;

use crate::error::{Error, Result};

/// The runner's home: `--home`, else `$ISOTHRUM_HOME`, else `~/.isothrum`, made [`absolute`].
pub(crate) fn home(flag: Option<PathBuf>, env: Option<std::ffi::OsString>, user_home: Option<PathBuf>) -> Result<PathBuf> {
    let home = flag
        .or_else(|| env.filter(|h| !h.is_empty()).map(PathBuf::from))
        .or_else(|| user_home.map(|h| h.join(".isothrum")))
        .ok_or_else(|| Error::Usage("no home folder: give --home or set ISOTHRUM_HOME".into()))?;
    absolute(home)
}

/// `home` made absolute against the directory the command runs in, so the run's state folder is
/// the same for every tool, whatever folder it runs in (a step in the manifest's folder, `ssh` in
/// the run's state folder). An absolute one is as it is.
///
/// # Errors
///
/// The directory the command runs in can't be read (it was removed).
pub(crate) fn absolute(home: PathBuf) -> Result<PathBuf> {
    if home.is_absolute() {
        return Ok(home);
    }
    std::path::absolute(&home).map_err(|source| Error::Read { path: home, source })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_relative_home_is_made_absolute_where_the_command_runs() {
        // Regression (#131): a relative home stayed relative, and each tool resolved it from its
        // own folder: a step's state under the lab's folder, `ssh` finding no key.
        let here = std::env::current_dir().unwrap();
        for made in [
            home(Some("lab-home".into()), None, None).unwrap(),
            home(None, Some("lab-home".into()), None).unwrap(),
        ] {
            assert!(made.is_absolute(), "{}", made.display());
            assert_eq!(made, here.join("lab-home"));
        }
        let already = here.join("abs-home");
        assert_eq!(home(Some(already.clone()), None, None).unwrap(), already);
    }

    #[test]
    fn the_home_is_the_flag_else_the_variable_else_the_users() {
        let abs = |p: &str| std::path::absolute(p).unwrap();
        assert_eq!(home(Some("/a".into()), Some("/b".into()), Some("/u".into())).unwrap(), abs("/a"));
        assert_eq!(home(None, Some("/b".into()), Some("/u".into())).unwrap(), abs("/b"));
        assert_eq!(home(None, Some(std::ffi::OsString::new()), Some("/u".into())).unwrap(), abs("/u/.isothrum"));
        assert_eq!(
            home(None, None, None).unwrap_err().to_string(),
            "no home folder: give --home or set ISOTHRUM_HOME"
        );
    }

    /// Regression: an `ISOTHRUM_HOME` that isn't UTF-8 was read as unset, and the runs went to
    /// `~/.isothrum`; it is a path like `--home`'s.
    #[cfg(unix)]
    #[test]
    fn a_home_variable_that_isnt_utf8_is_still_the_home() {
        use std::os::unix::ffi::OsStringExt;
        let raw = std::ffi::OsString::from_vec(b"/runs/bad\xff".to_vec());
        assert_eq!(home(None, Some(raw.clone()), Some("/u".into())).unwrap(), PathBuf::from(raw));
    }
}
