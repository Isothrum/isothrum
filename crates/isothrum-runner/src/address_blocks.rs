//! The address blocks a container engine's networks hold ([`Runner::address_blocks_in_use`]),
//! so a deployment's own networks can be given blocks nothing else holds.

use crate::Result;
use crate::cli::docker;
use crate::formats::compose::PROJECT_LABEL;
use crate::formats::tool_error;
use crate::name::RunName;
use crate::runner::Runner;

impl Runner {
    /// The IPv4 address blocks (CIDR, `172.17.0.0/16`) the networks of the engine Docker context
    /// `docker_context` reaches (the current one when none is named) hold, but those of run
    /// `except`'s own networks (left from an earlier start): what a deployment's networks must
    /// stay clear of. IPv6 blocks are left out.
    ///
    /// # Errors
    ///
    /// Docker is missing, or the engine can't say (it doesn't answer, a network went while it
    /// was read).
    pub async fn address_blocks_in_use(&self, except: Option<&RunName>, docker_context: Option<&str>) -> Result<Vec<String>> {
        let processes = self.docker(docker_context);
        let call = self.call(&processes);
        docker::network_blocks(&call, PROJECT_LABEL, except.map(RunName::as_str))
            .await
            .map_err(|e| tool_error("address-blocks", e))
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use crate::process::fake::{FakeProcesses, Reply};
    use crate::testing::{no_ports, processes, runner_config};
    use crate::{RunName, Runner};

    /// A runner with a temporary home on `fake`.
    fn a_runner(fake: FakeProcesses) -> (tempfile::TempDir, Runner) {
        let home = tempfile::tempdir().expect("a folder");
        let config = runner_config(home.path(), processes(&Arc::new(fake)), no_ports(), crate::runner::this_host());
        (home, Runner::new(config).expect("a runner"))
    }

    /// An engine whose networks are `listed` (`<name>\t<project>` lines; none: it doesn't
    /// answer), each inspected network's blocks as `inspect` gives them.
    fn engine(listed: Option<&'static str>, inspect: fn(&str) -> &'static str) -> FakeProcesses {
        FakeProcesses::model(move |call| match call.args.iter().map(String::as_str).collect::<Vec<_>>().as_slice() {
            ["network", "ls", "--format", format] if format.contains(".Label \"com.docker.compose.project\"") => match listed {
                Some(listed) => Reply::ok(listed),
                None => Reply::fail(1, "Cannot connect to the Docker daemon at unix:///var/run/docker.sock"),
            },
            ["network", "inspect", "--format", _, names @ ..] => Reply::ok(&names.iter().map(|n| inspect(n)).collect::<Vec<_>>().join("\n")),
            other => panic!("unexpected call {other:?}"),
        })
    }

    #[tokio::test]
    async fn the_blocks_of_every_other_network_are_listed_ipv4_only() {
        // `web`'s own network is told by its exact project label: `web-2`'s, whose name starts
        // the same, is another's.
        let fake = engine(Some("bridge\t\nweb_default\tweb\nweb-2_default\tweb-2\n"), |name| match name {
            "bridge" => "172.17.0.0/16 fd00::/64 ",
            "web-2_default" => "10.9.0.0/24 ",
            other => panic!("{other} is web's own"),
        });
        let (_home, runner) = a_runner(fake);
        let web = RunName::new("web").expect("a name");
        assert_eq!(
            runner.address_blocks_in_use(Some(&web), None).await.expect("listed"),
            ["172.17.0.0/16", "10.9.0.0/24"]
        );
    }

    #[tokio::test]
    async fn an_engine_that_cant_say_is_an_error_and_no_other_network_inspects_nothing() {
        let web = RunName::new("web").expect("a name");
        let (_silent, silent) = a_runner(engine(None, |_| ""));
        let err = silent.address_blocks_in_use(Some(&web), None).await.expect_err("no answer");
        assert_eq!(err.code(), crate::ErrorCode::ToolFailed, "{err}");
        let (_alone, alone) = a_runner(engine(Some("web_default\tweb\n"), |name| panic!("{name} is web's own")));
        assert_eq!(alone.address_blocks_in_use(Some(&web), None).await.expect("listed"), Vec::<String>::new());
    }
}
