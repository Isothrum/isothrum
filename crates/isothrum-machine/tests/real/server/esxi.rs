//! An ESXi host's SSH, as a caller checks it before it places machines there.

use std::time::Duration;

use isothrum_machine::place::esxi;

use crate::{setting, settings};

/// How long the host has to answer.
const LIMIT: Duration = Duration::from_secs(15);

/// The host's SSH port answers as SSH.
#[tokio::test]
#[ignore = "real server: cargo xtask e2e --tier server"]
async fn the_hosts_ssh_port_answers_as_ssh() {
    let [host] = match settings(["ISOTHRUM_TEST_ESXI_HOST"]) {
        Ok(found) => found,
        Err(why) => skip!("no ESXi host: {why}"),
    };
    let port = setting("ISOTHRUM_TEST_ESXI_PORT").and_then(|p| p.parse().ok()).unwrap_or(22);
    let ssh = esxi::ssh(&host, port, LIMIT).await.expect("its SSH answers");
    println!("{ssh:?}");
    assert!(matches!(ssh, esxi::Ssh::Answers { .. }), "{ssh:?}");
}
