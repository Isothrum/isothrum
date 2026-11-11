//! The runner's own environment, as far as the tools it starts outside its process tree need it:
//! an elevated Hyper-V session and a containerised tool inherit nothing, so they are handed the
//! proxies, the CA bundles, Terraform's CLI configuration and Vagrant's variables by name; the
//! macOS relays keep the hosts the user sends around a proxy; a home in the user's profile needs
//! no ACL of its own; and a graphical console finds libvirt's connection (`LIBVIRT_DEFAULT_URI`)
//! and Vagrant's own folder (`VAGRANT_HOME`, else `HOME`'s `.vagrant.d`). Read once, when the
//! runner is configured ([`crate::RunnerConfig::new`]), so no operation reads the process's
//! environment itself. A tool the runner starts as its child inherits the environment as it is,
//! never this copy.

use std::ffi::{OsStr, OsString};

use crate::network_env::is_network_var;

/// Terraform's CLI configuration file (a provider mirror, credentials).
pub(crate) const TF_CLI_CONFIG_FILE: &str = "TF_CLI_CONFIG_FILE";

/// The libvirt connection `virsh` and vagrant-libvirt use when nothing names another.
pub(crate) const LIBVIRT_DEFAULT_URI: &str = "LIBVIRT_DEFAULT_URI";

/// The variables of the runner's environment it hands on or reads, as read when it was configured. A
/// value is kept as it is, UTF-8 or not: one that isn't is still set (a `VAGRANT_HOME` that isn't
/// UTF-8 is never replaced by a default), and only where it must be handed on as text is it left
/// out, and traced ([`HostEnv::text`]).
#[derive(Clone, Default)]
pub(crate) struct HostEnv(Vec<(String, OsString)>);

impl HostEnv {
    /// This process's environment: the variables it hands on.
    pub(crate) fn capture() -> HostEnv {
        HostEnv::read(std::env::vars_os())
    }

    /// The variables of `vars` the runner hands on. A name that isn't UTF-8 can't be handed on by
    /// name: one that would be is left out, and traced.
    fn read(vars: impl IntoIterator<Item = (OsString, OsString)>) -> HostEnv {
        HostEnv::of(vars.into_iter().filter_map(|(k, v)| match k.into_string() {
            Ok(k) => Some((k, v)),
            Err(k) => {
                let name = k.to_string_lossy();
                if handed_on(&name) {
                    tracing::warn!(name = %name, "a variable's name isn't UTF-8: it isn't handed on");
                }
                None
            }
        }))
    }

    /// The variables of `vars` the runner hands on.
    pub(crate) fn of<K: Into<String>, V: Into<OsString>>(vars: impl IntoIterator<Item = (K, V)>) -> HostEnv {
        HostEnv(vars.into_iter().map(|(k, v)| (k.into(), v.into())).filter(|(k, _)| handed_on(k)).collect())
    }

    /// The value of `name` as it was read, UTF-8 or not, its case ignored on Windows (as Windows
    /// reads names).
    pub(crate) fn get(&self, name: &str) -> Option<&OsStr> {
        self.0.iter().find(|(k, _)| same_var(k, name)).map(|(_, v)| v.as_os_str())
    }

    /// The value of `name` as text, to hand on where only text goes (a variable's value, an
    /// argument). One that isn't UTF-8 is left out, and traced.
    pub(crate) fn text(&self, name: &str) -> Option<&str> {
        self.0.iter().find(|(k, _)| same_var(k, name)).and_then(|(k, v)| as_text(k, v))
    }

    /// Every variable whose value is text, as it was read; one that isn't UTF-8 is left out, and
    /// traced.
    pub(crate) fn vars(&self) -> impl Iterator<Item = (String, String)> + '_ {
        self.0.iter().filter_map(|(k, v)| Some((k.clone(), as_text(k, v)?.to_owned())))
    }

    /// What an elevated Hyper-V session's broker, which inherits nothing, is handed: the Vagrant
    /// and network variables (a box download goes out the way this account's does), `path` as
    /// `PATH` (the tools found here are found there), and `VAGRANT_HOME` pinned to this account's
    /// boxes, whoever approved the prompt, when none is set. A `path` that isn't UTF-8 can't be
    /// handed on as a variable: it is left out, and traced.
    pub(crate) fn for_elevation(&self, path: Option<&OsStr>) -> Vec<(String, String)> {
        let mut out: Vec<(String, String)> = self.vars().filter(|(k, _)| is_vagrant_var(k) || is_network_var(k)).collect();
        let path = path.and_then(|p| {
            let text = p.to_str();
            if text.is_none() {
                tracing::warn!("the PATH isn't UTF-8: an elevated session is handed none");
            }
            text
        });
        out.extend(path.map(|p| ("PATH".to_owned(), p.to_owned())));
        if self.get("VAGRANT_HOME").is_none()
            && let Some(profile) = self.text("USERPROFILE")
        {
            out.push(("VAGRANT_HOME".into(), format!("{}\\.vagrant.d", profile.trim_end_matches(['\\', '/']))));
        }
        out
    }
}

/// `value`, the value of `name`, as text: none, and traced, when it isn't UTF-8.
fn as_text<'a>(name: &str, value: &'a OsStr) -> Option<&'a str> {
    let text = value.to_str();
    if text.is_none() {
        tracing::warn!(name, "a variable's value isn't UTF-8: it isn't handed on");
    }
    text
}

/// Whether `a` and `b` name the same variable on this host: their case ignored on Windows (as
/// Windows reads names), not elsewhere.
pub(crate) fn same_var(a: &str, b: &str) -> bool {
    if cfg!(windows) { a.eq_ignore_ascii_case(b) } else { a == b }
}

/// Names only: the values may hold a proxy's credentials.
impl std::fmt::Debug for HostEnv {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_list().entries(self.0.iter().map(|(k, _)| k)).finish()
    }
}

/// Whether the runner keeps `name`: one it hands on (a network variable, Terraform's CLI
/// configuration, a Vagrant variable, the user's profile folder on Windows), or one it reads
/// itself (`HOME`, libvirt's default connection).
fn handed_on(name: &str) -> bool {
    let upper = name.to_ascii_uppercase();
    is_network_var(name) || upper == TF_CLI_CONFIG_FILE || is_vagrant_var(name) || ["USERPROFILE", "HOME", LIBVIRT_DEFAULT_URI].contains(&upper.as_str())
}

/// Whether `name` is one of Vagrant's own variables (`VAGRANT_HOME`, `VAGRANT_LOG`…), whatever
/// its case.
fn is_vagrant_var(name: &str) -> bool {
    name.to_ascii_uppercase().starts_with("VAGRANT_")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_what_the_runner_hands_on_is_kept_and_no_value_is_shown() {
        let env = HostEnv::of([
            ("HTTPS_PROXY", "http://user:pw@proxy.corp:3128"),
            ("no_proxy", ".corp"),
            ("TF_CLI_CONFIG_FILE", "/etc/terraformrc"),
            ("VAGRANT_HOME", "/v"),
            ("USERPROFILE", "C:\\Users\\ada"),
            ("AWS_SECRET_ACCESS_KEY", "never"),
            ("PATH", "/bin"),
        ]);
        let names: Vec<String> = env.vars().map(|(k, _)| k).collect();
        assert_eq!(names, ["HTTPS_PROXY", "no_proxy", "TF_CLI_CONFIG_FILE", "VAGRANT_HOME", "USERPROFILE"]);
        assert_eq!(env.text("no_proxy"), Some(".corp"));
        assert_eq!(env.get("AWS_SECRET_ACCESS_KEY"), None);
        let shown = format!("{env:?}");
        assert!(shown.contains("HTTPS_PROXY") && !shown.contains("pw@"), "{shown}");
    }

    #[test]
    fn an_elevated_session_gets_the_path_vagrants_and_the_networks_variables() {
        let env = HostEnv::of([
            ("HTTPS_PROXY", "http://proxy.corp:3128"),
            ("TF_CLI_CONFIG_FILE", "C:\\terraformrc"),
            ("USERPROFILE", "C:\\Users\\ada\\"),
            ("VAGRANT_LOG", "info"),
        ]);
        assert_eq!(
            env.for_elevation(Some(OsStr::new("C:\\HashiCorp\\Vagrant\\bin"))),
            [
                ("HTTPS_PROXY".to_owned(), "http://proxy.corp:3128".to_owned()),
                ("VAGRANT_LOG".to_owned(), "info".to_owned()),
                ("PATH".to_owned(), "C:\\HashiCorp\\Vagrant\\bin".to_owned()),
                ("VAGRANT_HOME".to_owned(), "C:\\Users\\ada\\.vagrant.d".to_owned()),
            ],
            "Terraform's and the profile's own aren't Vagrant's"
        );
        let own_home = HostEnv::of([("VAGRANT_HOME", "D:\\boxes"), ("USERPROFILE", "C:\\Users\\ada")]);
        assert_eq!(own_home.for_elevation(None), [("VAGRANT_HOME".to_owned(), "D:\\boxes".to_owned())]);
    }

    #[test]
    fn a_name_is_read_as_this_host_reads_it() {
        let env = HostEnv::of([("NO_PROXY", ".corp")]);
        assert_eq!(env.get("no_proxy").is_some(), cfg!(windows));
        assert_eq!(env.text("NO_PROXY"), Some(".corp"));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn a_value_that_isnt_utf8_is_still_set_and_is_traced_where_it_cant_be_handed_on() {
        use std::os::unix::ffi::OsStringExt;
        let not_utf8 = || OsString::from_vec(b"/boxes/\xff".to_vec());
        let env = HostEnv::read([
            (OsString::from("VAGRANT_HOME"), not_utf8()),
            (OsString::from("USERPROFILE"), OsString::from("C:\\Users\\ada")),
            (OsString::from("SSL_CERT_FILE"), not_utf8()),
        ]);
        assert_eq!(env.get("VAGRANT_HOME"), Some(not_utf8().as_os_str()), "kept as it is");
        let (elevated, elevation_traces) = crate::testing::traced(async { env.for_elevation(None) }).await;
        assert_eq!(elevated, [], "neither left out in silence nor replaced by the profile's boxes");
        assert!(
            elevation_traces.contains("WARN") && elevation_traces.contains("VAGRANT_HOME") && elevation_traces.contains("isn't UTF-8"),
            "{elevation_traces}"
        );
        let (cert, cert_traces) = crate::testing::traced(async { env.text("SSL_CERT_FILE") }).await;
        assert_eq!(cert, None);
        assert!(cert_traces.contains("SSL_CERT_FILE"), "{cert_traces}");
        let bad_name = [(OsString::from_vec(b"VAGRANT_\xff".to_vec()), OsString::new())];
        let (read, name_traces) = crate::testing::traced(async { HostEnv::read(bad_name) }).await;
        assert_eq!(read.vars().count(), 0);
        assert!(name_traces.contains("name isn't UTF-8"), "{name_traces}");
    }
}
