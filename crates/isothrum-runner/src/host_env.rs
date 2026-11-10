//! The runner's own environment, as far as the tools it starts outside its process tree need it:
//! an elevated Hyper-V session and a containerised tool inherit nothing, so they are handed the
//! proxies, the CA bundles, Terraform's CLI configuration and Vagrant's variables by name; the
//! macOS relays keep the hosts the user sends around a proxy; a home in the user's profile needs
//! no ACL of its own. Read once, when the runner is configured ([`crate::RunnerConfig::new`]), so
//! no operation reads the process's environment itself. A tool the runner starts as its child
//! inherits the environment as it is, never this copy.

use crate::network_env::is_network_var;

/// Terraform's CLI configuration file (a provider mirror, credentials).
pub(crate) const TF_CLI_CONFIG_FILE: &str = "TF_CLI_CONFIG_FILE";

/// The variables of the runner's environment it hands on, as read when it was configured.
#[derive(Clone, Default)]
pub(crate) struct HostEnv(Vec<(String, String)>);

impl HostEnv {
    /// This process's environment: the variables it hands on that hold UTF-8.
    pub(crate) fn capture() -> HostEnv {
        HostEnv::of(std::env::vars_os().filter_map(|(k, v)| Some((k.into_string().ok()?, v.into_string().ok()?))))
    }

    /// The variables of `vars` the runner hands on.
    pub(crate) fn of<K: Into<String>, V: Into<String>>(vars: impl IntoIterator<Item = (K, V)>) -> HostEnv {
        HostEnv(vars.into_iter().map(|(k, v)| (k.into(), v.into())).filter(|(k, _)| handed_on(k)).collect())
    }

    /// The value of `name`, its case ignored on Windows (as Windows reads names).
    pub(crate) fn get(&self, name: &str) -> Option<&str> {
        let same = |k: &str| if cfg!(windows) { k.eq_ignore_ascii_case(name) } else { k == name };
        self.0.iter().find(|(k, _)| same(k)).map(|(_, v)| v.as_str())
    }

    /// Every variable, as it was read.
    pub(crate) fn vars(&self) -> impl Iterator<Item = (String, String)> + '_ {
        self.0.iter().cloned()
    }

    /// What an elevated Hyper-V session's broker, which inherits nothing, is handed: the Vagrant
    /// and network variables (a box download goes out the way this account's does), `path` as
    /// `PATH` (the tools found here are found there), and `VAGRANT_HOME` pinned to this account's
    /// boxes, whoever approved the prompt, when none is set.
    pub(crate) fn for_elevation(&self, path: Option<&str>) -> Vec<(String, String)> {
        let mut out: Vec<(String, String)> = self.vars().filter(|(k, _)| is_vagrant_var(k) || is_network_var(k)).collect();
        out.extend(path.map(|p| ("PATH".to_owned(), p.to_owned())));
        if self.get("VAGRANT_HOME").is_none()
            && let Some(profile) = self.get("USERPROFILE")
        {
            out.push(("VAGRANT_HOME".into(), format!("{}\\.vagrant.d", profile.trim_end_matches(['\\', '/']))));
        }
        out
    }
}

/// Names only: the values may hold a proxy's credentials.
impl std::fmt::Debug for HostEnv {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_list().entries(self.0.iter().map(|(k, _)| k)).finish()
    }
}

/// Whether the runner hands `name` on: a network variable, Terraform's CLI configuration, a
/// Vagrant variable, or the user's profile folder (Windows).
fn handed_on(name: &str) -> bool {
    let upper = name.to_ascii_uppercase();
    is_network_var(name) || upper == TF_CLI_CONFIG_FILE || is_vagrant_var(name) || upper == "USERPROFILE"
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
        assert_eq!(env.get("no_proxy"), Some(".corp"));
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
            env.for_elevation(Some("C:\\HashiCorp\\Vagrant\\bin")),
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
        assert_eq!(env.get("NO_PROXY"), Some(".corp"));
    }
}
