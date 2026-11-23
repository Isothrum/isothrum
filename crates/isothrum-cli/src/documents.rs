//! The JSON documents the command line prints with `--json` besides the runner's own (its events,
//! its status, a console's command): typed, so their schema is written from them
//! (`docs/reference/json-output.schema.json`) and a field can't change without it.

use isothrum_runner::{ConsoleProtocol, NativeConsole, NativeOpen};
use serde::Serialize;

/// `key --json`: the runner's SSH public key.
#[derive(Debug, Serialize, schemars::JsonSchema)]
pub(crate) struct PublicKey {
    /// The key, as `authorized_keys` takes it (`ssh-ed25519 AAAA… isothrum`).
    pub(crate) public_key: String,
}

/// `screen --json`: where a machine's graphical console answers.
#[derive(Debug, Serialize, schemars::JsonSchema)]
pub(crate) struct Screen {
    /// The machine.
    pub(crate) machine: String,
    /// The protocol it speaks.
    pub(crate) protocol: ConsoleProtocol,
    /// Where a VNC or RDP viewer connects, on this computer's loopback (`127.0.0.1:5901`); null
    /// for a console reached through its websocket.
    pub(crate) address: Option<String>,
    /// Proxmox VE's VNC websocket; its ticket is hidden unless `--show-password`.
    pub(crate) websocket: Option<String>,
    /// Who to sign in as, when the console asks.
    pub(crate) user: Option<String>,
    /// The password, with `--show-password` only.
    pub(crate) password: Option<String>,
    /// What else to know to connect.
    pub(crate) note: Option<String>,
    /// The machine an SSH tunnel to the console goes through, open while the command runs.
    pub(crate) tunnel: Option<String>,
}

/// `screen --app --json`: what opens a machine in its hypervisor's own app.
#[derive(Debug, Serialize, schemars::JsonSchema)]
pub(crate) struct NativeApps {
    /// The machine.
    pub(crate) machine: String,
    /// The app's name.
    pub(crate) app: String,
    /// The ways to open it, best first: the first whose program or app this computer has is the
    /// one to take.
    pub(crate) ways: Vec<Way>,
}

/// One way to open the app, never through a shell.
#[derive(Debug, Serialize, schemars::JsonSchema)]
#[serde(untagged)]
pub(crate) enum Way {
    /// Run `program` with `args`, each passed as it is.
    Program {
        /// The program: a name on `PATH`, or a full path.
        program: String,
        /// Its arguments.
        args: Vec<String>,
    },
    /// Open the macOS app `mac_app` on `args` (`open -a <mac_app> <args>…`).
    MacApp {
        /// The app bundle.
        mac_app: String,
        /// What it opens.
        args: Vec<String>,
    },
    /// Open `url` in the default browser.
    Url {
        /// The URL.
        url: String,
    },
}

impl NativeApps {
    /// `console`'s ways, each as its document says it.
    pub(crate) fn of(console: &NativeConsole) -> NativeApps {
        let ways = console
            .ways
            .iter()
            .filter_map(|way| match way {
                NativeOpen::Program { program, args } => Some(Way::Program {
                    program: program.clone(),
                    args: args.clone(),
                }),
                NativeOpen::MacApp { app, args } => Some(Way::MacApp {
                    mac_app: app.to_string_lossy().into_owned(),
                    args: args.clone(),
                }),
                NativeOpen::Url { url } => Some(Way::Url { url: url.clone() }),
                _ => None,
            })
            .collect();
        NativeApps {
            machine: console.machine.clone(),
            app: console.app.clone(),
            ways,
        }
    }
}
