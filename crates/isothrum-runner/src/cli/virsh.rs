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
