//! One service per home: an OS lock on `<home>/service.lock`, held for the service's life and
//! released by the OS when its process ends, however it ends. Once it listens, the service says
//! who and where it is in `<home>/service.json`, which it removes when it stops. A `service.json`
//! found when the lock is taken is a dead service's: it goes before the service listens.

use std::fs::{File, TryLockError};

use isothrum_client::{ServiceInfo, ServicePaths};

use crate::error::ServeError;

/// The home, held.
#[derive(Debug)]
pub(crate) struct Lease {
    /// Holds the lock while open.
    _file: File,
    paths: ServicePaths,
    published: bool,
}

impl Lease {
    /// Holds the home of `paths`.
    ///
    /// # Errors
    ///
    /// [`ServeError::Held`] when another service holds it (with what it said of itself), `io`
    /// when the lock file can't be opened, or a dead service's `service.json` can't be removed.
    pub(crate) fn acquire(paths: &ServicePaths) -> Result<Lease, ServeError> {
        let path = paths.lock();
        let file = File::options()
            .create(true)
            .truncate(false)
            .write(true)
            .open(&path)
            .map_err(|source| ServeError::Io {
                what: "the service's lock file",
                path: path.clone(),
                source,
            })?;
        match file.try_lock() {
            Ok(()) => {
                forget_the_dead(paths)?;
                Ok(Lease {
                    _file: file,
                    paths: paths.clone(),
                    published: false,
                })
            }
            Err(TryLockError::WouldBlock) => Err(ServeError::Held {
                home: paths.home().to_path_buf(),
                holder: paths.read_info(),
            }),
            Err(TryLockError::Error(source)) => Err(ServeError::Io {
                what: "the service's lock",
                path,
                source,
            }),
        }
    }

    /// Says who and where the service is (`service.json`), replacing what a dead one said.
    ///
    /// # Errors
    ///
    /// The file can't be written.
    pub(crate) fn publish(&mut self, info: &ServiceInfo) -> Result<(), ServeError> {
        let path = self.paths.info();
        let fresh = path.with_extension("json.new");
        let io = |source| ServeError::Io {
            what: "the service's info file",
            path: path.clone(),
            source,
        };
        let text = serde_json::to_vec_pretty(info).map_err(|e| io(std::io::Error::other(e)))?;
        std::fs::write(&fresh, text).map_err(io)?;
        std::fs::rename(&fresh, &path).map_err(io)?;
        self.published = true;
        Ok(())
    }
}

impl Drop for Lease {
    fn drop(&mut self) {
        if self.published
            && let Err(e) = std::fs::remove_file(self.paths.info())
        {
            tracing::warn!(error = %e, "the service's info file couldn't be removed");
        }
    }
}

/// Removes the `service.json` a service that ended without removing it left (the lock is held
/// here, so no service runs). This service listens before it writes its own, on Unix at the same
/// socket: a client that read the dead one's file in between would greet this service and take
/// the dead one's pid and epoch for its.
fn forget_the_dead(paths: &ServicePaths) -> Result<(), ServeError> {
    match std::fs::remove_file(paths.info()) {
        Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(ServeError::Io {
            what: "a dead service's info file",
            path: paths.info(),
            source: e,
        }),
        _ => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_second_holder_is_refused_and_told_who_holds_the_home_until_the_first_lets_go() {
        let home = tempfile::tempdir().unwrap();
        let paths = ServicePaths::new(home.path());
        let mut first = Lease::acquire(&paths).unwrap();
        let info = ServiceInfo {
            pid: 42,
            started: "2026-01-01T00:00:00Z".into(),
            version: "3.0.0".into(),
            protocol: 1,
            endpoint: "here".into(),
            epoch: 7,
        };
        first.publish(&info).unwrap();
        let held = Lease::acquire(&paths).unwrap_err();
        assert!(matches!(&held, ServeError::Held { holder: Some(h), .. } if h.pid == 42), "{held:?}");
        assert!(held.to_string().contains("pid 42"), "{held}");
        drop(first);
        assert!(paths.read_info().is_none(), "its info goes with it");
        Lease::acquire(&paths).unwrap();
    }

    #[test]
    fn the_info_a_dead_service_left_is_gone_before_the_next_one_listens() {
        // A service killed, or ended by a client replacing it, leaves its service.json behind.
        let home = tempfile::tempdir().unwrap();
        let paths = ServicePaths::new(home.path());
        std::fs::write(
            paths.info(),
            r#"{"pid":42,"started":"2026-01-01T00:00:00Z","version":"2.9.0","protocol":1,"endpoint":"here","epoch":7}"#,
        )
        .unwrap();
        assert!(paths.read_info().is_some());
        let _held = Lease::acquire(&paths).unwrap();
        assert!(paths.read_info().is_none(), "a client would take the next service for the dead one");
    }
}
