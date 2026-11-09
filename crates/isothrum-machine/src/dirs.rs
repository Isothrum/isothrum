//! Where the machine domain keeps its files: a data folder for the record of what it has
//! downloaded, a cache folder for a self-test's throwaway lab. The app shell resolves the OS paths and hands them in, so this crate stays free of any
//! one app's identity and of a directory-resolving dependency.

use std::path::{Path, PathBuf};

use crate::{Error, Result};

/// The folders the machine domain writes to, each known once the app shell has resolved it. What
/// can do without one skips its file when it is missing.
#[derive(Clone, Debug, Default)]
pub struct Dirs {
    data: Option<PathBuf>,
    cache: Option<PathBuf>,
}

impl Dirs {
    /// The folders, each when the shell knows it.
    #[must_use]
    pub fn new(data: Option<PathBuf>, cache: Option<PathBuf>) -> Dirs {
        Dirs { data, cache }
    }

    /// The data folder.
    ///
    /// # Errors
    ///
    /// [`Error::Invalid`] when the shell never resolved it.
    pub fn data(&self) -> Result<&Path> {
        self.data
            .as_deref()
            .ok_or_else(|| Error::Invalid("the app's data folder is unknown".into()))
    }

    /// The file `name` in the data folder, when that is known.
    #[must_use]
    pub fn data_file(&self, name: &str) -> Option<PathBuf> {
        self.data.as_ref().map(|d| d.join(name))
    }

    /// The cache folder.
    ///
    /// # Errors
    ///
    /// [`Error::Invalid`] when the shell never resolved it.
    pub fn cache(&self) -> Result<&Path> {
        self.cache
            .as_deref()
            .ok_or_else(|| Error::Invalid("the app's cache folder is unknown".into()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_known_folder_is_returned_and_a_file_joined_onto_it() {
        let dirs = Dirs::new(Some(PathBuf::from("/data")), None);
        assert_eq!(dirs.data().unwrap(), Path::new("/data"));
        assert_eq!(
            dirs.data_file("owned.json"),
            Some(PathBuf::from("/data/owned.json"))
        );
    }

    #[test]
    fn an_unknown_folder_is_an_error_and_has_no_file() {
        let dirs = Dirs::default();
        assert!(matches!(dirs.data(), Err(Error::Invalid(_))));
        assert!(matches!(dirs.cache(), Err(Error::Invalid(_))));
        assert_eq!(dirs.data_file("x"), None);
    }
}
