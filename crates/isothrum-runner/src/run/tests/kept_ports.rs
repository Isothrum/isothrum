//! A Vagrant run's kept host ports: a provider's `auto_correct` moved a forward at the first boot
//! (8080 taken: 2200), and re-picks at each later one (8080 free again).

use std::sync::atomic::{AtomicBool, AtomicU16, Ordering};

use super::vagrant::{VERSION, a_vagrant_lab_on};
use super::*;
use crate::process::fake::FakeProcesses;

/// The variable a Vagrantfile reads `web`'s 80/tcp from.
const WEB_80: &str = "ISOTHRUM_HOST_PORT_WEB_80_TCP";

/// Vagrant whose Vagrantfile reads [`WEB_80`] when `honours`, else gets what `auto_correct`
/// picks: 2200 at the first boot, 8080 after.
struct Box_ {
    honours: bool,
    boots: AtomicU16,
    host: AtomicU16,
    running: AtomicBool,
}

impl Box_ {
    fn answer(&self, call: &Call) -> Reply {
        let args: Vec<&str> = call.args.iter().map(String::as_str).collect();
        match args.as_slice() {
            ["--version"] => Reply::ok(VERSION),
            ["up", ..] => {
                let boot = self.boots.fetch_add(1, Ordering::SeqCst);
                let picked = if boot == 0 { 2200 } else { 8080 };
                let given = call.env(WEB_80).filter(|_| self.honours).and_then(|v| v.parse().ok());
                self.host.store(given.unwrap_or(picked), Ordering::SeqCst);
                self.running.store(true, Ordering::SeqCst);
                Reply::ok("")
            }
            ["halt"] => {
                self.running.store(false, Ordering::SeqCst);
                Reply::ok("")
            }
            ["status", "--machine-readable"] => {
                let state = if self.running.load(Ordering::SeqCst) { "running" } else { "poweroff" };
                Reply::ok(&format!("1794305819,web,state,{state}"))
            }
            ["port", "web", "--machine-readable"] => Reply::ok(&format!("1794305819,web,forwarded_port,80,{}", self.host.load(Ordering::SeqCst))),
            _ => panic!("a call the model doesn't know: {}", call.line()),
        }
    }
}

/// Up, a shutdown and a resume of a run on `model`: the resume's events, and the processes.
async fn up_park_resume(honours: bool) -> (Vec<crate::events::Event>, Arc<FakeProcesses>, u16) {
    let model = Arc::new(Box_ {
        honours,
        boots: AtomicU16::new(0),
        host: AtomicU16::new(0),
        running: AtomicBool::new(false),
    });
    let fake = Arc::new(FakeProcesses::model({
        let model = Arc::clone(&model);
        move |call| model.answer(call)
    }));
    let (_root, project, runner) = a_vagrant_lab_on(&fake);
    let run = runner.open(&project, None).expect("opened");
    let opts = RunOptions::new().with_provider(crate::Provider::Virtualbox);
    run.up(&opts, &Recorder::new()).await.expect("up");
    run.park(crate::Park::Shutdown, &opts, &Recorder::new()).await.expect("parked");
    let events = Recorder::new();
    run.resume(&opts, &events).await.expect("resumed");
    let host = model.host.load(Ordering::SeqCst);
    (events.events(), fake, host)
}

/// Whether `events` hold a `ports_moved` warning.
fn moved(events: &[crate::events::Event]) -> bool {
    events.iter().any(|e| {
        matches!(
            e.kind,
            EventKind::Warning {
                code: crate::events::WarningCode::PortsMoved,
                ..
            }
        )
    })
}

#[tokio::test]
async fn a_vagrantfile_reading_its_kept_ports_boots_on_them_again() {
    let (events, fake, host) = up_park_resume(true).await;
    assert_eq!(host, 2200, "the port the first boot got");
    let boots = fake.made("vagrant up");
    assert_eq!(boots.first().and_then(|c| c.env(WEB_80)), None, "nothing kept before the first start");
    assert_eq!(boots.last().and_then(|c| c.env(WEB_80)), Some("2200"), "the resume got the kept port");
    assert!(!moved(&events), "{events:?}");
}

#[tokio::test]
async fn a_port_that_moved_anyway_is_warned() {
    let (events, _fake, host) = up_park_resume(false).await;
    assert_eq!(host, 8080);
    assert!(moved(&events), "{events:?}");
}
