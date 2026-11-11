//! The boxes a start's VMs boot, downloaded before `vagrant up` in a step of their own with a
//! download's rule: each listed one the run's provider and this computer's architecture take,
//! once; one Vagrant has already skipped; a stuck download stopped as `download_stalled`, a
//! refused one `download_failed`, and `vagrant up` never run after either.

use std::time::Duration;

use isoloom_manifest::Arch;

use super::*;
use crate::{Error, EventKind, StepState};

/// The fixture's run on a computer of architecture `arch`.
fn fixture_on(arch: Arch) -> Fixture {
    let folder = RunFolder::new("vagrant", WEB_VM);
    let (engine, fake) = modelled(super::answer);
    let ports: Arc<dyn crate::HostPorts> = Arc::new(FakePorts::new([41000, 41001]));
    let mut config = runner_config(folder.home.path(), processes(&fake), ports, HostOs::Macos);
    config.arch = arch;
    Fixture {
        manifest: folder.manifest.clone(),
        runner: Runner::new(config).unwrap(),
        folder,
        fake,
        engine,
    }
}

/// What the fake Vagrant answers to `vagrant box <args>`.
pub(super) fn answer(e: &mut Engine, args: &[&str]) -> Reply {
    match args {
        ["list", "--machine-readable"] => Reply::ok(
            &e.boxes
                .iter()
                .map(|[n, p, v, a]| format!("1,,box-name,{n}\n1,,box-provider,{p}\n1,,box-version,{v}\n1,,box-architecture,{a}"))
                .collect::<Vec<_>>()
                .join("\n"),
        ),
        ["add", name, "--provider", provider, rest @ ..] => match e.box_add {
            BoxAdd::Adds => {
                let arch = rest.windows(2).find(|w| w[0] == "--architecture").map_or("arm64", |w| w[1]);
                e.boxes.push([(*name).to_owned(), (*provider).to_owned(), "1.0.0".to_owned(), arch.to_owned()]);
                Reply::ok(&format!("==> box: Successfully added box '{name}' (v1.0.0) for '{provider} ({arch})'!"))
            }
            BoxAdd::Stuck => {
                let mut reply = Reply::ok("");
                reply.stdout = std::iter::once("    box: Progress: 41% (Rate: 1201k/s, Estimated time remaining: 0:04:10)".to_owned())
                    .chain(std::iter::repeat_n(
                        "    box: Progress: 42% (Rate: 0/s, Estimated time remaining: --:--:--)".to_owned(),
                        24 * 60,
                    ))
                    .collect();
                reply.paced(Duration::from_secs(60))
            }
            BoxAdd::Fails => Reply::fail(
                1,
                &format!("The box '{name}' could not be found or\\ncould not be accessed in the remote catalog."),
            ),
            BoxAdd::ThereAlready => Reply::fail(
                1,
                "The box you're attempting to add already exists. Remove it before\\nadding it again or add it with the `--force` flag.",
            ),
        },
        _ => Reply::ok(""),
    }
}

/// The `vagrant box add` calls, then whether `vagrant up` came after every one.
fn adds(f: &Fixture) -> (Vec<String>, bool) {
    let lines = f.fake.lines();
    let adds: Vec<String> = lines.iter().filter(|l| l.starts_with("vagrant box add ")).cloned().collect();
    let last_add = lines.iter().rposition(|l| l.starts_with("vagrant box add "));
    let up = lines.iter().position(|l| l.starts_with("vagrant up --provider"));
    (adds, last_add.zip(up).is_some_and(|(a, u)| a < u))
}

/// The `boxes-up` step's states, in order.
fn box_steps(events: &Recorder) -> Vec<StepState> {
    events
        .events()
        .into_iter()
        .filter_map(|e| match e.kind {
            EventKind::Step { step, state, .. } if step == "boxes-up" => Some(state),
            _ => None,
        })
        .collect()
}

#[tokio::test]
async fn a_start_downloads_the_boxes_its_vms_boot_on_its_provider_before_vagrant_up() {
    // An x86-64 computer: web boots Debian, the controller Alpine.
    let f = fixture_on(Arch::Amd64);
    let run = f.runner.open(&f.manifest, None).unwrap();
    let events = Recorder::new();
    run.up(&opts(), &events).await.unwrap();
    let (added, before_up) = adds(&f);
    assert_eq!(
        added,
        [
            "vagrant box add bento/debian-12 --provider virtualbox",
            "vagrant box add generic/alpine319 --provider virtualbox"
        ]
    );
    assert!(before_up, "{:?}", f.fake.lines());
    assert_eq!(box_steps(&events), [StepState::Running, StepState::Done]);
    let said = serde_json::to_string(&events.events()).unwrap();
    assert!(said.contains("Downloading the VMs' boxes"), "{said}");
    assert!(said.contains("Successfully added box 'bento/debian-12'"), "its lines are the step's: {said}");

    // An arm64 computer: the controller boots Debian too, downloaded once.
    let arm = fixture_on(Arch::Arm64);
    let on_arm = arm.runner.open(&arm.manifest, None).unwrap();
    on_arm.up(&opts(), &Recorder::new()).await.unwrap();
    assert_eq!(adds(&arm).0, ["vagrant box add bento/debian-12 --provider virtualbox"]);
}

#[tokio::test]
async fn a_box_vagrant_has_for_this_computer_is_not_downloaded_again() {
    let f = fixture_on(Arch::Arm64);
    let there = |arch: &str| ["bento/debian-12", "virtualbox", "202510.26.0", arch].map(str::to_owned);
    // The x86-64 build only: the arm64 one is downloaded.
    f.engine().boxes.push(there("amd64"));
    let run = f.runner.open(&f.manifest, None).unwrap();
    run.up(&opts(), &Recorder::new()).await.unwrap();
    assert_eq!(adds(&f).0.len(), 1);

    // Every box there: no download, no step.
    let again = fixture_on(Arch::Arm64);
    again.engine().boxes.push(there("arm64"));
    let all_there = again.runner.open(&again.manifest, None).unwrap();
    let events = Recorder::new();
    all_there.up(&opts(), &events).await.unwrap();
    assert_eq!(adds(&again).0, Vec::<String>::new());
    assert_eq!(box_steps(&events), []);
    // A resume boots the VMs that are there: no box is listed.
    let listed = again.fake.lines().iter().filter(|l| l.starts_with("vagrant box list")).count();
    all_there.park(Park::Pause, &RunOptions::new(), &Recorder::new()).await.unwrap();
    all_there.resume(&RunOptions::new(), &Recorder::new()).await.unwrap();
    assert_eq!(again.fake.lines().iter().filter(|l| l.starts_with("vagrant box list")).count(), listed);
}

#[tokio::test(start_paused = true)]
async fn a_stuck_box_download_is_stopped_as_a_stalled_download_and_nothing_starts() {
    let f = fixture_on(Arch::Arm64);
    f.engine().box_add = BoxAdd::Stuck;
    let run = f.runner.open(&f.manifest, None).unwrap();
    let started = tokio::time::Instant::now();
    let err = run.up(&opts(), &Recorder::new()).await.unwrap_err();
    // Its last new line came at two minutes: 15 minutes later it is stopped.
    assert_eq!(started.elapsed(), Duration::from_secs(17 * 60));
    assert!(
        matches!(&err, Error::DownloadStalled { step, after_s: 900, .. } if step == "boxes-up"),
        "{err:?}"
    );
    assert_eq!(err.code(), ErrorCode::DownloadStalled);
    assert!(!f.fake.lines().iter().any(|l| l.starts_with("vagrant up --provider")));
}

#[tokio::test]
async fn a_refused_box_download_says_vagrants_reason_and_one_there_already_is_no_failure() {
    let f = fixture_on(Arch::Arm64);
    f.engine().box_add = BoxAdd::Fails;
    let run = f.runner.open(&f.manifest, None).unwrap();
    let err = run.up(&opts(), &Recorder::new()).await.unwrap_err();
    let Error::DownloadFailed { step, reason } = &err else { panic!("{err:?}") };
    assert_eq!(step, "boxes-up");
    assert!(reason.contains("could not be found"), "{reason}");
    assert!(!f.fake.lines().iter().any(|l| l.starts_with("vagrant up --provider")));

    let there = fixture_on(Arch::Arm64);
    there.engine().box_add = BoxAdd::ThereAlready;
    let added_meanwhile = there.runner.open(&there.manifest, None).unwrap();
    added_meanwhile.up(&opts(), &Recorder::new()).await.unwrap();
}

#[tokio::test]
async fn a_start_on_qemu_downloads_the_libvirt_build_vagrant_qemu_boots() {
    // Regression: the download asked for the box's qemu build, then `vagrant up` found no libvirt
    // build (vagrant-qemu's format) and downloaded the box a second time.
    let f = fixture_on(Arch::Amd64);
    f.engine().plugins.push("vagrant-qemu".into());
    let run = f.runner.open(&f.manifest, None).unwrap();
    let qemu = || opts().with_provider(isoloom_manifest::Provider::Qemu);
    run.up(&qemu(), &Recorder::new()).await.unwrap();
    let (added, before_up) = adds(&f);
    assert!(!added.is_empty() && added.iter().all(|a| a.contains(" --provider libvirt")), "{added:?}");
    assert!(before_up, "{:?}", f.fake.lines());

    // Its libvirt build there: nothing downloaded.
    let again = fixture_on(Arch::Amd64);
    again.engine().plugins.push("vagrant-qemu".into());
    again
        .engine()
        .boxes
        .push(["cloud-image/debian-12", "libvirt", "20260923.2610.0", "amd64"].map(str::to_owned));
    again.runner.open(&again.manifest, None).unwrap().up(&qemu(), &Recorder::new()).await.unwrap();
    assert_eq!(adds(&again).0, Vec::<String>::new());
}
