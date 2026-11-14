//! The libvirt adapter: where `virsh domdisplay` says a domain's display is, on the connection
//! the run's Vagrant uses.

use std::net::SocketAddr;

use super::{Call, CliError};
use crate::process::Captured;

/// The tool.
const TOOL: &str = "virsh";

/// The display a domain has, as `domdisplay` says it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Display {
    /// A VNC display, at this address.
    Vnc(SocketAddr),
    /// Another (SPICE) or none: what `virsh` said.
    Other(String),
}

/// `domdisplay <domain>`, on connection `uri` when given (else the one `virsh` picks itself, from
/// `LIBVIRT_DEFAULT_URI`).
pub(crate) async fn domdisplay(call: &Call<'_>, uri: Option<&str>, domain: &str) -> Result<Display, CliError<String>> {
    let mut args = Vec::new();
    if let Some(uri) = uri {
        args.extend(["-c".to_owned(), uri.to_owned()]);
    }
    args.extend(["domdisplay".to_owned(), domain.to_owned()]);
    let out = call.ok(TOOL, args, Captured::reason).await?;
    let said = out.stdout.join("\n");
    Ok(match crate::tool_output::virsh::vnc_display(&said) {
        Some(at) => Display::Vnc(at),
        None => Display::Other(said.trim().to_owned()),
    })
}

/// What `virsh` says of a domain it doesn't know.
const NO_DOMAIN: [&str; 2] = ["failed to get domain", "domain not found"];

/// Removes domain `uuid` on connection `uri` (when given) by its id, as vagrant-libvirt would:
/// `destroy` (powered off; one already off is fine), then `undefine --remove-all-storage
/// --nvram` (its definition, its volumes and its firmware variables). A domain the connection
/// doesn't know is gone already.
pub(crate) async fn remove(call: &Call<'_>, uri: Option<&str>, uuid: &str) -> Result<(), CliError<String>> {
    let on = |verb: &[&str]| {
        let mut args: Vec<String> = uri.map(|u| vec!["-c".to_owned(), u.to_owned()]).unwrap_or_default();
        args.extend(verb.iter().map(|w| (*w).to_owned()));
        args
    };
    drop(call.run::<String>(TOOL, on(&["destroy", uuid])).await?);
    let out = call.run::<String>(TOOL, on(&["undefine", uuid, "--remove-all-storage", "--nvram"])).await?;
    super::done_or_gone(TOOL, &out, &NO_DOMAIN)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cli::testing;
    use crate::process::fake::{FakeProcesses, Reply};

    #[tokio::test]
    async fn a_domains_display_is_read_on_the_connection_asked() {
        let fake = FakeProcesses::script()
            .expect(
                "virsh -c qemu:///system domdisplay d1",
                Reply::ok(include_str!("../tool_output/fixtures/virsh/10.0.0-domdisplay-vnc.txt")),
            )
            .expect(
                "virsh domdisplay d2",
                Reply::ok(include_str!("../tool_output/fixtures/virsh/10.0.0-domdisplay-spice.txt")),
            )
            .build();
        let call = testing::call(&fake);
        assert_eq!(
            domdisplay(&call, Some("qemu:///system"), "d1").await.unwrap(),
            Display::Vnc("127.0.0.1:5900".parse().unwrap())
        );
        assert!(matches!(domdisplay(&call, None, "d2").await.unwrap(), Display::Other(_)));
    }
}
