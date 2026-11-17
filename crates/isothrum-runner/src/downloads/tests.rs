//! What the runs' downloads fetched, recorded and removed: an image or box there before isn't
//! recorded, a new one is; a removal takes only what is recorded, forgets a reference that moved
//! without removing it, and goes on past one its tool refuses.

use std::sync::{Arc, Mutex, PoisonError};

use super::*;
use crate::events::Recorder;
use crate::process::fake::{Call, CallView as _, FakeProcesses, Reply};
use crate::testing::{modelled, no_ports, processes, runner_config};
use crate::{RunName, RunOptions};

/// The images a project's services run: `web`'s is pulled, `cache`'s is there already, `app`'s
/// is built by `up`; `kit`, under a profile, is started on its own, its image pulled by that `up`.
const CONFIG: &str = r#"{"services": {"web": {"image": "nginx:1.27"}, "cache": {"image": "redis:7"},
    "app": {"image": "app:dev", "build": {"context": "."}}}}"#;
const KIT: &str = r#"{"services": {"kit": {"image": "toolkit:3"}}}"#;

/// What `call` inspects, when it is `docker image inspect --format <format> <image>`: the
/// format and the image.
fn inspected(call: &Call) -> Option<(&str, &str)> {
    let args: Vec<&str> = call.args.iter().map(String::as_str).collect();
    match args.as_slice() {
        [.., "image", "inspect", "--format", format, image] => Some((format, image)),
        _ => None,
    }
}

/// What a modelled Docker engine holds: whether the pull ran, whether `up` built, whether `kit`
/// was started.
#[derive(Debug, Default)]
struct Engine {
    pulled: bool,
    built: bool,
    kit: bool,
}

/// The engine's answer to `call`: `redis:7` held from the start, `nginx:1.27` once pulled,
/// `app:dev` once `up` built it, `toolkit:3` once `up kit` pulled it.
fn engine(e: &Mutex<Engine>, call: &Call) -> Reply {
    let mut e = e.lock().unwrap_or_else(PoisonError::into_inner);
    let line = call.line();
    let has = |words: &str| line.contains(words);
    if let Some(inspected) = inspected(call) {
        return match inspected {
            ("{{.Id}}", "redis:7") => Reply::ok("sha256:redis"),
            ("{{.Id}}", "nginx:1.27") if e.pulled => Reply::ok("sha256:nginx"),
            ("{{.Id}}", "app:dev") if e.built => Reply::ok("sha256:app"),
            ("{{.Id}}", "toolkit:3") if e.kit => Reply::ok("sha256:kit"),
            ("{{.Size}}", "sha256:nginx") => Reply::ok("187654321"),
            (_, image) => Reply::fail(1, &format!("Error: No such object: {image}")),
        };
    }
    if has("config --format json kit") {
        Reply::ok(KIT)
    } else if has("config --format json") {
        Reply::ok(CONFIG)
    } else if has("compose version") {
        Reply::ok("2.29.7")
    } else if has(" pull ") {
        e.pulled = true;
        Reply::ok("")
    } else if has(" up ") {
        e.kit |= has(" kit");
        e.built = true;
        Reply::ok("")
    } else if has("image ls --quiet --no-trunc") {
        let held = [
            (true, "sha256:redis"),
            (e.pulled, "sha256:nginx"),
            (e.built, "sha256:app"),
            (e.kit, "sha256:kit"),
        ];
        Reply::ok(&held.iter().filter(|(here, _)| *here).map(|(_, id)| [*id, "\n"].concat()).collect::<String>())
    } else if has(" ps ") {
        Reply::ok(r#"{"Service":"web","State":"running","Health":"","ExitCode":0,"Publishers":[],"Labels":""}"#)
    } else {
        Reply::ok("")
    }
}

/// A runner with a temporary home on `fake`, and a project of `file` (`compose.yaml` or
/// `Vagrantfile`) holding `text`.
fn a_runner(fake: &Arc<FakeProcesses>, file: &str, text: &str) -> (tempfile::TempDir, std::path::PathBuf, Runner) {
    let root = tempfile::tempdir().expect("a folder");
    let project = root.path().join("stack");
    std::fs::create_dir(&project).expect("the project");
    std::fs::write(project.join(file), text).expect("its file");
    let mut config = runner_config(&root.path().join("home"), processes(fake), no_ports(), crate::runner::this_host());
    config.host_env = crate::host_env::HostEnv::of([("VAGRANT_HOME", root.path().join("vagrant.d"))]);
    (root, project, Runner::new(config).expect("a runner"))
}

/// The image `id` pulled or built as `reference`, through Docker context `orbstack`.
fn in_orbstack(reference: &str, id: &str) -> DownloadItem {
    DownloadItem::Image {
        reference: reference.into(),
        image_id: id.into(),
        docker_context: Some("orbstack".into()),
    }
}

#[tokio::test]
async fn a_start_records_the_images_it_pulled_and_built_and_an_attach_the_one_it_pulled() {
    let (_engine, fake) = modelled::<Engine>(engine);
    let (_root, project, runner) = a_runner(&fake, "compose.yaml", "services:\n  web:\n    image: nginx:1.27\n");
    let run = runner.open(&project, None).expect("opened");
    let opts = RunOptions::new().with_docker_context("orbstack");
    run.up(&opts, &Recorder::new()).await.expect("up");

    let listed = runner.downloads().await.expect("listed");
    let fetched: Vec<&DownloadItem> = listed.iter().map(|d| &d.item).collect();
    assert_eq!(
        fetched,
        [&in_orbstack("app:dev", "sha256:app"), &in_orbstack("nginx:1.27", "sha256:nginx")],
        "the built one and the pulled one"
    );
    assert_eq!(
        (listed[1].run.as_ref().map(RunName::as_str), listed[1].size_bytes),
        (Some("stack"), Some(187_654_321))
    );
    let sized = fake.last("{{.Size}}").expect("its size read");
    assert_eq!(sized.flag("--context"), Some("orbstack"), "in the context it was pulled through");

    // A second start finds them held: nothing more is recorded.
    run.update(&opts, &Recorder::new()).await.expect("update");
    assert_eq!(runner.downloads().await.expect("listed").len(), 2);

    // A service under a profile, started on its own: its `up` pulls its image.
    run.attach("kit", &opts, &Recorder::new()).await.expect("attached");
    let with_kit = runner.downloads().await.expect("listed");
    assert_eq!(with_kit.last().map(|d| &d.item), Some(&in_orbstack("toolkit:3", "sha256:kit")), "{with_kit:?}");
}

/// What a modelled Vagrant holds: whether `up` ran.
#[derive(Debug, Default)]
struct Boxes {
    up: bool,
}

/// `box list --machine-readable` of each box `boxes` names (name, provider, version, arch).
fn box_list(boxes: &[(&str, &str, &str, &str)]) -> String {
    boxes
        .iter()
        .flat_map(|(name, provider, version, arch)| {
            [
                format!("1794875822,,box-name,{name}"),
                format!("1794875822,,box-provider,{provider}"),
                format!("1794875822,,box-version,{version}"),
                format!("1794875822,,box-architecture,{arch}"),
            ]
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// The box Vagrant held before the `up`, and the one the `up` added.
const HELD: (&str, &str, &str, &str) = ("generic/alpine319", "virtualbox", "4.3.12", "n/a");
const ADDED: (&str, &str, &str, &str) = ("bento/debian-12", "virtualbox", "202510.26.0", "arm64");

/// Vagrant over VirtualBox, whose `up` adds [`ADDED`].
fn vagrant(state: &Mutex<Boxes>, call: &Call) -> Reply {
    let mut state = state.lock().unwrap_or_else(PoisonError::into_inner);
    let args: Vec<&str> = call.args.iter().map(String::as_str).collect();
    match (call.program.as_str(), args.as_slice()) {
        ("vagrant", ["--version"]) => Reply::ok("Vagrant 2.4.9"),
        ("vagrant", ["box", "list", "--machine-readable"]) => Reply::ok(&box_list(if state.up { &[HELD, ADDED] } else { &[HELD] })),
        ("vagrant", ["up", ..]) => {
            state.up = true;
            Reply::ok("")
        }
        ("vagrant", ["status", "--machine-readable"]) => Reply::ok("1,web,provider-name,virtualbox\n1,web,state,running"),
        ("VBoxManage", _) => Reply::ok("VMState=\"running\""),
        _ => Reply::ok(""),
    }
}

#[tokio::test]
async fn an_up_records_the_box_it_added_and_not_one_vagrant_held() {
    let state = Arc::new(Mutex::new(Boxes::default()));
    let fake = Arc::new(FakeProcesses::model({
        let state = Arc::clone(&state);
        move |call| vagrant(&state, call)
    }));
    let (root, project, runner) = a_runner(&fake, "Vagrantfile", "Vagrant.configure('2') do |c|\nend\n");
    let folder = root.path().join("vagrant.d/boxes/bento-VAGRANTSLASH-debian-12/202510.26.0/arm64/virtualbox");
    std::fs::create_dir_all(folder.join("disks")).expect("the box's folder");
    std::fs::write(folder.join("box.ovf"), [0_u8; 10]).expect("a file");
    std::fs::write(folder.join("disks").join("disk.vmdk"), [0_u8; 32]).expect("a file");
    let run = runner.open(&project, None).expect("opened");
    run.up(&RunOptions::new().with_provider(crate::Provider::Virtualbox), &Recorder::new())
        .await
        .expect("up");

    let listed = runner.downloads().await.expect("listed");
    let items: Vec<(&DownloadItem, Option<u64>)> = listed.iter().map(|d| (&d.item, d.size_bytes)).collect();
    let added = DownloadItem::Box {
        name: "bento/debian-12".into(),
        provider: "virtualbox".into(),
        version: "202510.26.0".into(),
        architecture: Some("arm64".into()),
    };
    assert_eq!(
        items,
        [(&added, Some(42))],
        "the box held before isn't the run's; the added one's folder is 42 bytes"
    );
}

/// An engine and a Vagrant for a removal: `nginx:1.27` stands for its recorded id, `redis:7` was
/// pulled again since, `busy:1` a container uses; Vagrant holds the box [`ADDED`] only.
fn removal(call: &Call) -> Reply {
    let line = call.line();
    let has = |words: &str| line.contains(words);
    if let Some((_, image)) = inspected(call) {
        return Reply::ok(match image {
            "nginx:1.27" => "sha256:nginx",
            "redis:7" => "sha256:redis-new",
            _ => "sha256:busy",
        });
    }
    if has("image rm busy:1") {
        Reply::fail(
            1,
            "Error response from daemon: conflict: unable to remove repository reference \"busy:1\" (must force) - container 3f2a is using its referenced image 9c1e",
        )
    } else if has("box list") {
        Reply::ok(&box_list(&[ADDED]))
    } else {
        Reply::ok("")
    }
}

/// An image fetched as `reference`, standing for `id`.
fn image(reference: &str, id: &str) -> DownloadItem {
    DownloadItem::Image {
        reference: reference.into(),
        image_id: id.into(),
        docker_context: None,
    }
}

/// Records on `runner` the three images and two boxes [`removal`] knows: their ids.
async fn recorded(runner: &Runner) -> Vec<DownloadId> {
    let held_box = |(name, provider, version, arch): (&str, &str, &str, &str)| DownloadItem::Box {
        name: name.into(),
        provider: provider.into(),
        version: version.into(),
        architecture: (arch != "n/a").then(|| arch.to_owned()),
    };
    let recorded = [
        image("nginx:1.27", "sha256:nginx"),
        image("redis:7", "sha256:redis"),
        image("busy:1", "sha256:busy"),
        held_box(ADDED),
        held_box(HELD),
    ];
    let run = RunName::new("stack").unwrap();
    runner
        .store
        .db()
        .write(|tx| recorded.iter().try_for_each(|item| tx.record_download(Some(&run), item).map(drop)))
        .expect("recorded");
    runner.downloads().await.unwrap().iter().map(|d| d.id).collect()
}

#[tokio::test]
async fn a_removal_takes_only_what_is_recorded_forgets_a_moved_reference_and_goes_on_past_a_refusal() {
    let fake = Arc::new(FakeProcesses::model(removal));
    let (_root, _project, runner) = a_runner(&fake, "compose.yaml", "services: {}\n");
    let ids = recorded(&runner).await;
    let mut asked = ids.clone();
    asked.push(DownloadId(999));
    let removals = runner.remove_downloads(&DownloadsToRemove::Only(asked)).await.expect("removed");
    let outcomes: Vec<&RemovalOutcome> = removals.iter().map(|r| &r.outcome).collect();
    let [nginx, redis, busy, added, held, unknown] = outcomes.as_slice() else {
        panic!("one outcome each: {outcomes:?}")
    };
    assert_eq!(
        [nginx, redis, added, held, unknown],
        [
            &&RemovalOutcome::Removed,
            &&RemovalOutcome::Moved {
                now: "sha256:redis-new".into()
            },
            &&RemovalOutcome::Removed,
            &&RemovalOutcome::Gone,
            &&RemovalOutcome::NotRecorded,
        ]
    );
    assert!(
        matches!(busy, RemovalOutcome::Failed { reason } if reason.contains("container 3f2a is using its referenced image")),
        "{busy:?}"
    );
    assert!(removals.last().is_some_and(|r| r.download.is_none()));

    let removed: Vec<Vec<String>> = fake.calls().into_iter().filter(|c| c.has("rm") || c.has("remove")).map(|c| c.args).collect();
    assert_eq!(
        removed,
        [
            vec!["image", "rm", "nginx:1.27"],
            vec!["image", "rm", "busy:1"],
            vec![
                "box",
                "remove",
                "bento/debian-12",
                "--provider",
                "virtualbox",
                "--box-version",
                "202510.26.0",
                "--architecture",
                "arm64"
            ],
        ],
        "never forced, never a moved reference, never a box gone already"
    );
    let left: Vec<DownloadItem> = runner.downloads().await.unwrap().into_iter().map(|d| d.item).collect();
    assert_eq!(left, [image("busy:1", "sha256:busy")], "only the refused one stays recorded");

    // All: what is recorded, and nothing else.
    let all = runner.remove_downloads(&DownloadsToRemove::All).await.expect("removed");
    assert_eq!(all.iter().map(|r| r.id).collect::<Vec<_>>(), [ids[2]]);
}

#[test]
fn the_wire_reads_back_as_written() {
    let listed: Download = serde_json::from_value(serde_json::json!({
        "id": 3, "kind": "box", "name": "bento/debian-12", "provider": "virtualbox", "version": "202510.26.0",
        "architecture": "arm64", "run": "web", "fetched_at": "2026-11-17T10:00:00Z", "size_bytes": 42
    }))
    .expect("a download");
    assert_eq!(serde_json::from_str::<Download>(&serde_json::to_string(&listed).unwrap()).unwrap(), listed);
    let image = serde_json::json!({"id": 4, "kind": "image", "reference": "nginx:1.27", "image_id": "sha256:a", "run": "web", "fetched_at": "t"});
    let read: Download = serde_json::from_value(image.clone()).expect("an image");
    assert_eq!(serde_json::to_value(&read).unwrap(), image, "no context, no size: neither written");

    assert_eq!(serde_json::to_value(DownloadsToRemove::All).unwrap(), serde_json::json!("all"));
    assert_eq!(
        serde_json::to_value(DownloadsToRemove::Only(vec![DownloadId(3)])).unwrap(),
        serde_json::json!({"only": [3]})
    );
    let moved = DownloadRemoval {
        id: DownloadId(4),
        outcome: RemovalOutcome::Moved { now: "sha256:b".into() },
        download: Some(read),
    };
    let written = serde_json::to_value(&moved).unwrap();
    assert_eq!(
        (&written["outcome"], &written["now"], &written["download"]["kind"]),
        (&serde_json::json!("moved"), &serde_json::json!("sha256:b"), &serde_json::json!("image"))
    );
    assert_eq!(serde_json::from_value::<DownloadRemoval>(written).unwrap(), moved);
}

/// The engine and Vagrant of an adoption: `nginx:1.27` stands for `sha256:nginx`, `redis:7` for
/// `sha256:new`; Vagrant holds `bento/debian-12` 1 in two architectures.
fn held(_: &Mutex<()>, call: &Call) -> Reply {
    if let Some((_, image)) = inspected(call) {
        return match image {
            "nginx:1.27" => Reply::ok("sha256:nginx"),
            "redis:7" => Reply::ok("sha256:new"),
            other => Reply::fail(1, &format!("Error: No such object: {other}")),
        };
    }
    if call.line().contains("box list") {
        return Reply::ok(&box_list(&[
            ("bento/debian-12", "virtualbox", "1", "amd64"),
            ("bento/debian-12", "virtualbox", "1", "arm64"),
            ("bento/debian-12", "virtualbox", "2", "amd64"),
        ]));
    }
    Reply::ok("")
}

#[tokio::test]
async fn an_adopted_download_is_recorded_with_no_run_only_while_it_is_here_as_it_was_fetched() {
    let (_held, fake) = modelled::<()>(held);
    let (_root, _project, runner) = a_runner(&fake, "compose.yaml", "services: {}\n");
    let image = |reference: &str, id: Option<&str>| DownloadToAdopt::Image {
        reference: reference.into(),
        image_id: id.map(str::to_owned),
    };
    let debian = |version: &str| DownloadToAdopt::Box {
        name: "bento/debian-12".into(),
        provider: "virtualbox".into(),
        version: version.into(),
    };
    let asked = [
        image("nginx:1.27", Some("sha256:nginx")),
        image("redis:7", Some("sha256:old")),
        image("gone:1", None),
        debian("1"),
        debian("3"),
    ];
    let adopted = runner.adopt_downloads(&asked).await.expect("adopted");
    let items: Vec<&DownloadItem> = adopted.iter().map(|d| &d.item).collect();
    let arch = |a: &str| DownloadItem::Box {
        name: "bento/debian-12".into(),
        provider: "virtualbox".into(),
        version: "1".into(),
        architecture: Some(a.into()),
    };
    assert_eq!(
        items,
        [&image_item("nginx:1.27"), &arch("amd64"), &arch("arm64")],
        "moved, gone and unheld ones aren't"
    );
    assert!(adopted.iter().all(|d| d.run.is_none()), "{adopted:?}");
    assert_eq!(fake.made("vagrant box list").len(), 1, "Vagrant's boxes read once");

    let again = runner.adopt_downloads(&asked[..1]).await.expect("adopted again");
    assert_eq!(
        again.iter().map(|d| d.id).collect::<Vec<_>>(),
        [adopted[0].id],
        "one recorded already keeps its row"
    );
    assert_eq!(runner.downloads().await.expect("listed").len(), 3);
}

/// `nginx:1.27` as adopted: on this computer's engine.
fn image_item(reference: &str) -> DownloadItem {
    image(reference, "sha256:nginx")
}
