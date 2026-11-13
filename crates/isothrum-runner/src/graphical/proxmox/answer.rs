//! What Proxmox VE's API answers, parsed once, here, into the fields the runner reads: every
//! answer is `{"data": …}`, and each call's `data` has its own shape ([`Ticket`], [`VncProxy`],
//! [`ClusterVm`]). Unknown fields are skipped.

use serde::Deserialize;
use serde::de::DeserializeOwned;

/// The API's envelope: its `data`, none when it is null or missing.
#[derive(Debug, Deserialize)]
struct Answer<T> {
    data: Option<T>,
}

/// The `data` of `body`, an answer of the API; none when it has none.
///
/// # Errors
///
/// `body` isn't JSON, or not the shape this call answers.
pub(super) fn data<T: DeserializeOwned>(body: &str) -> Result<Option<T>, serde_json::Error> {
    serde_json::from_str::<Answer<T>>(body).map(|a| a.data)
}

/// `access/ticket`'s answer to a user's password: the ticket that signs in and the token a
/// write sends beside it.
#[derive(Debug, Deserialize)]
pub(super) struct Ticket {
    pub(super) ticket: Option<String>,
    #[serde(rename = "CSRFPreventionToken")]
    pub(super) csrf: Option<String>,
}

/// `vncproxy`'s answer: the port of the VM's VNC websocket and the ticket that opens it.
#[derive(Debug, Deserialize)]
pub(super) struct VncProxy {
    port: Option<NumberOrText>,
    pub(super) ticket: Option<String>,
}

impl VncProxy {
    /// The websocket's port, when the answer gives one that is a port.
    pub(super) fn port(&self) -> Option<u16> {
        self.port.as_ref().and_then(NumberOrText::as_u16)
    }
}

/// A VM of `cluster/resources?type=vm`: its id and where it stands (`running`, `stopped`).
#[derive(Debug, Deserialize)]
pub(super) struct ClusterVm {
    vmid: Option<NumberOrText>,
    pub(super) status: Option<String>,
}

impl ClusterVm {
    /// Whether it is the VM `id` names.
    pub(super) fn is(&self, id: &str) -> bool {
        self.vmid.as_ref().is_some_and(|v| v.text() == id)
    }
}

/// A number the API writes as a number or as a string, depending on the call and the version.
#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum NumberOrText {
    Number(u64),
    Text(String),
}

impl NumberOrText {
    fn text(&self) -> String {
        match self {
            NumberOrText::Number(n) => n.to_string(),
            NumberOrText::Text(s) => s.clone(),
        }
    }

    fn as_u16(&self) -> Option<u16> {
        match self {
            NumberOrText::Number(n) => u16::try_from(*n).ok(),
            NumberOrText::Text(s) => s.parse().ok(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Answers as Proxmox VE 8 gives them (secrets shortened).
    const TICKET: &str =
        r#"{"data":{"username":"root@pam","ticket":"PVE:root@pam:6553F0A4::c2lnbmVk","CSRFPreventionToken":"6553F0A4:Y3NyZg","cap":{"vms":{"VM.Console":1}}}}"#;
    const VNCPROXY: &str = r#"{"data":{"cert":"-----BEGIN CERTIFICATE-----\nMIIF\n-----END CERTIFICATE-----\n","port":"5900","ticket":"PVEVNC:6553F0A4::dGlja2V0","upid":"UPID:pve:000A1B2C:0123ABCD:6553F0A4:vncproxy:101:root@pam:","user":"root@pam"}}"#;
    const RESOURCES: &str = r#"{"data":[{"id":"qemu/101","type":"qemu","vmid":101,"name":"web","node":"pve","status":"running","template":0,"maxmem":2147483648,"uptime":3600},{"id":"qemu/102","type":"qemu","vmid":102,"name":"db","node":"pve","status":"stopped","template":0}]}"#;

    #[test]
    fn a_ticket_answer_gives_the_ticket_and_its_token() {
        let t: Ticket = data(TICKET).unwrap().unwrap();
        assert_eq!(t.ticket.as_deref(), Some("PVE:root@pam:6553F0A4::c2lnbmVk"));
        assert_eq!(t.csrf.as_deref(), Some("6553F0A4:Y3NyZg"));
    }

    #[test]
    fn a_vncproxy_answer_gives_its_port_as_a_string_or_a_number() {
        let v: VncProxy = data(VNCPROXY).unwrap().unwrap();
        assert_eq!((v.port(), v.ticket.as_deref()), (Some(5900), Some("PVEVNC:6553F0A4::dGlja2V0")));
        let n: VncProxy = data(r#"{"data":{"port":5901,"ticket":"t"}}"#).unwrap().unwrap();
        assert_eq!(n.port(), Some(5901));
        let none: VncProxy = data(r#"{"data":{"port":"99999"}}"#).unwrap().unwrap();
        assert_eq!((none.port(), none.ticket), (None, None), "not a port, no ticket");
    }

    #[test]
    fn cluster_resources_name_each_vm_by_its_id() {
        let vms: Vec<ClusterVm> = data(RESOURCES).unwrap().unwrap();
        let of = |id: &str| vms.iter().find(|v| v.is(id)).and_then(|v| v.status.as_deref());
        assert_eq!((of("101"), of("102"), of("103")), (Some("running"), Some("stopped"), None));
        let texts: Vec<ClusterVm> = data(r#"{"data":[{"vmid":"104","status":"running"}]}"#).unwrap().unwrap();
        assert!(texts[0].is("104"));
    }

    #[test]
    fn an_answer_without_data_or_not_the_api_s_is_told_apart() {
        assert!(data::<Ticket>(r#"{"data":null}"#).unwrap().is_none());
        assert!(data::<Ticket>("{}").unwrap().is_none());
        assert!(data::<Vec<ClusterVm>>(r#"{"data":{"vmid":1}}"#).is_err(), "not a list");
        assert!(data::<Ticket>("<html>").is_err());
    }
}
