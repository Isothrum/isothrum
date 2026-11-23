//! Why the service couldn't start or stopped on a failure.

use std::path::PathBuf;

use isothrum_client::ServiceInfo;

/// Why the service couldn't run.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum ServeError {
    /// Another service holds the home.
    #[error("a runner service already holds {}{}", home.display(), holder_text(holder.as_deref()))]
    Held {
        /// The home.
        home: PathBuf,
        /// What the holder says of itself, when it said (boxed: the error stays small).
        holder: Option<Box<ServiceInfo>>,
    },
    /// A file of the service couldn't be used.
    #[error("{what} ({}) couldn't be used: {source}", path.display())]
    Io {
        /// What.
        what: &'static str,
        /// Its path.
        path: PathBuf,
        /// The OS's reason.
        #[source]
        source: std::io::Error,
    },
    /// The endpoint couldn't be opened.
    #[error("the service couldn't listen at {endpoint}: {source}")]
    Listen {
        /// Where.
        endpoint: String,
        /// The OS's reason.
        #[source]
        source: std::io::Error,
    },
    /// The runner couldn't be built on the home.
    #[error(transparent)]
    Runner(#[from] isothrum_runner::Error),
}

/// `: pid N, Isothrum V, since T`, when the holder said.
fn holder_text(holder: Option<&ServiceInfo>) -> String {
    holder.map_or_else(String::new, |h| format!(" (pid {}, Isothrum {}, since {})", h.pid, h.version, h.started))
}
