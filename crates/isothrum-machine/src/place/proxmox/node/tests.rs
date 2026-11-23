use std::path::PathBuf;
use std::sync::Arc;

use isothrum_runner::fake::{FakeProcesses, Reply};

use super::*;
use crate::testing::shared;

fn node() -> Target {
    Target::new("10.0.0.50", "root", PathBuf::from("/k/id"), PathBuf::from("/kh"))
}

#[tokio::test]
async fn a_node_that_sources_its_sdn_config_reads_so_and_one_that_doesnt_too() {
    let yes = shared(Arc::new(FakeProcesses::script().expect("ssh", Reply::ok("")).build()));
    assert_eq!(sdn_sourced(&yes, &node()).await, Ok(true));
    let no = shared(Arc::new(FakeProcesses::script().expect("ssh", Reply::fail(1, "")).build()));
    assert_eq!(sdn_sourced(&no, &node()).await, Ok(false));
    let refused = shared(Arc::new(
        FakeProcesses::script()
            .expect("ssh", Reply::fail(255, "root@10.0.0.50: Permission denied (publickey)."))
            .build(),
    ));
    assert!(matches!(sdn_sourced(&refused, &node()).await, Err(SshError::Failed(why)) if why.contains("Permission denied")));
    assert!(SDN_SOURCE_CHECK.contains("/etc/network/interfaces"));
}

#[tokio::test]
async fn a_key_the_node_lets_in_signs_in() {
    let ok = shared(Arc::new(FakeProcesses::script().expect("ssh -i /k/id", Reply::ok("")).build()));
    assert_eq!(signs_in(&ok, &node()).await, Ok(()));
}
