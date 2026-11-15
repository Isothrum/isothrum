//! The format contract, run against a model of Vagrant on VirtualBox.

use std::path::PathBuf;
use std::sync::Mutex;

use super::{Dotfile, Record, VAGRANTFILE};
use crate::formats::contract::{self, Model, Op, RunFormat as Format, RunProvider as Provider, ToolCall as Call, ToolReply as Reply};

const VERSION: &str = include_str!("../../../tool_output/fixtures/vagrant/2.4.1-version.txt");
const TOO_OLD: &str = include_str!("../../../tool_output/fixtures/vagrant/2.3.7-version.txt");
const RUNNING: &str = include_str!("../../../tool_output/fixtures/vagrant/2.4.1-status-virtualbox-web.txt");
const NOT_CREATED: &str = include_str!("../../../tool_output/fixtures/vagrant/2.4.1-status-virtualbox-not-created.txt");
const PORT: &str = include_str!("../../../tool_output/fixtures/vagrant/2.4.1-port.txt");

/// The id VirtualBox gives the VM of machine `web`.
const WEB_ID: &str = "0d4e7a52-6b1f-4c38-9e2a-71f05c3b8d16";

/// Vagrant over VirtualBox: the VMs VirtualBox holds, and the dotfile Vagrant keeps of them.
pub(super) struct VagrantBox {
    old: bool,
    vms: Mutex<Vec<String>>,
}

/// The dotfile folder `call` is given ([`super::dotfile::VAR`]).
fn dotfile_of(call: &Call) -> Dotfile {
    let path = call.env(super::dotfile::VAR).expect("the dotfile folder in the environment");
    Dotfile { path: PathBuf::from(path) }
}

impl VagrantBox {
    /// `destroy -f [machine]`: the VM the dotfile records for `web` removed, and its record; a
    /// machine not created is destroyed already, as Vagrant says with exit 0.
    fn destroy(&self, dot: &Dotfile) -> Reply {
        let recorded = dot.recorded().expect("the dotfile");
        let mut vms = self.vms.lock().expect("the world");
        for r in recorded {
            vms.retain(|id| *id != r.id);
            dot.forget(&r.machine, &r.provider).expect("the record dropped");
        }
        Reply::ok("==> web: Destroying VM and associated drives...")
    }
}

impl Model for VagrantBox {
    const FORMAT: Format = Format::Vagrant;
    const PROVIDER: Option<Provider> = Some(Provider::Virtualbox);
    const FILE: (&'static str, &'static str) = ("Vagrantfile", VAGRANTFILE);
    const VERSION: &'static str = "vagrant --version";
    const MACHINES: &'static [&'static str] = &["web"];
    const REFUSES: &'static [Op] = &[];

    fn new(old: bool) -> VagrantBox {
        VagrantBox {
            old,
            vms: Mutex::new(Vec::new()),
        }
    }

    fn answer(&self, call: &Call) -> Reply {
        assert_eq!(call.program, "vagrant", "{}", call.line());
        let args: Vec<&str> = call.args.iter().map(String::as_str).collect();
        match args.as_slice() {
            ["--version"] => Reply::ok(if self.old { TOO_OLD } else { VERSION }),
            ["up", "--provider", "virtualbox"] => {
                self.vms.lock().expect("the world").push(WEB_ID.to_owned());
                let made = Record {
                    machine: "web".into(),
                    provider: "virtualbox".into(),
                    id: WEB_ID.into(),
                };
                dotfile_of(call).write_back(&made).expect("Vagrant's record");
                Reply::ok("==> web: Machine booted and ready!")
            }
            ["status", "--machine-readable"] => {
                let made = self.vms.lock().expect("the world").contains(&WEB_ID.to_owned());
                Reply::ok(if made { RUNNING } else { NOT_CREATED })
            }
            ["port", "web", "--machine-readable"] => Reply::ok(PORT),
            ["destroy", "-f"] | ["destroy", "-f", "web"] => self.destroy(&dotfile_of(call)),
            other => panic!("a Vagrant call the model doesn't know: {other:?}"),
        }
    }

    fn holds(&self) -> Vec<String> {
        self.vms.lock().expect("the world").clone()
    }
}

contract::suite!(VagrantBox);
