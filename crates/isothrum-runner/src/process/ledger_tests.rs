use super::*;

#[test]
fn a_start_reads_back_and_is_its_process_within_the_second_the_os_rounds_to() {
    for start in [
        Start::Boot {
            boot: "6f1e2c3a-9b8d-4e7f-a1b2-c3d4e5f60718".into(),
            ticks: 987_654,
        },
        Start::At(1_794_415_415),
    ] {
        assert_eq!(Start::parse(&start.word()), Some(start.clone()));
        assert!(start.is(&start));
    }
    assert!(Start::At(100).is(&Start::At(102)) && Start::At(102).is(&Start::At(100)));
    assert!(!Start::At(100).is(&Start::At(103)), "another process that got the id later");
    let boot = |ticks| Start::Boot { boot: "b".into(), ticks };
    assert!(!boot(1).is(&boot(2)) && !boot(1).is(&Start::At(1)));
    for bad in ["", "at:", "at:x", "boot:1", "boot:b:x", "time:1"] {
        assert_eq!(Start::parse(bad), None, "{bad:?}");
    }
}

#[test]
fn the_oss_start_reads_from_proc_and_from_ps() {
    let stat = "4242 (sh) deploy (x)) S 1 4242 4242 0 -1 4194560 120 0 0 0 0 0 0 0 20 0 1 0 987654 2240512 210";
    assert_eq!(stat_ticks(stat), Some(987_654));
    assert_eq!(stat_ticks("4242 (sh) S 1"), None);
    // `TZ=UTC LC_ALL=C ps -o lstart=`.
    assert_eq!(lstart("Wed Nov 11 16:43:35 2026\n"), Some(1_794_415_415));
    assert_eq!(lstart("Thu Feb 29 00:00:00 2024"), Some(1_709_164_800));
    for bad in ["", "Wed Foo 11 16:43:35 2026", "Wed Nov 11 16:43 2026", "Wed Nov 0 16:43:35 2026"] {
        assert_eq!(lstart(bad), None, "{bad:?}");
    }
}

/// `sleep` in a process group of its own, as the runner starts every tool.
#[cfg(unix)]
fn tool() -> std::process::Child {
    use std::os::unix::process::CommandExt;
    std::process::Command::new("sleep").arg("60").process_group(0).spawn().unwrap()
}

/// Regression (tests/resilience.md A4): a process killed mid-operation left its tools running to
/// their end. A tool a dead process recorded is ended, its entry gone; a live owner's tool, and a
/// recorded id that now names another process (here, this very test), are left alone.
#[cfg(unix)]
#[tokio::test]
async fn a_dead_owners_tools_are_ended_and_nothing_it_didnt_start() {
    let home = tempfile::tempdir().unwrap();
    let dir = home.path().join(DIR);
    let (mut orphan, mut owned) = (tool(), tool());
    let orphan_id = orphan.id();
    left_by_a_killed_process(&dir, orphan_id).unwrap();
    // Reaped as soon as it ends, as init reaps a tool whose parent was killed: a zombie still
    // counts in its process group, and the group would only be killed after the grace.
    let (reaped, reaping) = std::sync::mpsc::channel();
    std::thread::spawn(move || reaped.send(orphan.wait().is_ok()));
    // This process's own tool: its owner (this process) holds its lock.
    let mine = Recorded::write(&dir, owned.id()).unwrap();
    // An id that names another process now: recorded as started at the epoch.
    let me = std::process::id();
    std::fs::write(dir.join(format!("dead-{me}.tool")), format!("{me} at:1\n")).unwrap();

    let asked = std::time::Instant::now();
    assert_eq!(end_orphans(&dir).await, [orphan_id]);
    assert_eq!(reaping.recv_timeout(Duration::from_secs(5)), Ok(true), "the orphaned tool was ended");
    assert!(asked.elapsed() < GRACE, "it stopped when asked, not killed after the grace");
    assert!(owned.try_wait().unwrap().is_none(), "a live process's tool runs on");
    let left: Vec<String> = std::fs::read_dir(&dir)
        .unwrap()
        .flatten()
        .map(|e| e.file_name().into_string().unwrap())
        .collect();
    assert_eq!(left.len(), 2, "this process's lock and its tool's entry: {left:?}");
    assert!(left.iter().all(|n| !n.starts_with("dead")), "{left:?}");

    drop(mine);
    owned.kill().unwrap();
    owned.wait().unwrap();
    assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 1, "the ended tool's entry went with it");
}

#[tokio::test]
async fn nothing_recorded_is_nothing_to_end() {
    let home = tempfile::tempdir().unwrap();
    let none: [u32; 0] = [];
    assert_eq!(end_orphans(&home.path().join(DIR)).await, none);
}

/// Regression: the token was kept for the life of the process, so once the run's state folder was
/// removed and made again (an embedder that lives on, a home cleared by hand), every later tool
/// went unrecorded, or was recorded beside no owner, which nothing ever ends.
#[test]
fn a_folder_made_again_gets_its_owner_again() {
    let home = tempfile::tempdir().unwrap();
    let dir = home.path().join(DIR);
    let pid = std::process::id();
    let first = Recorded::write(&dir, pid).unwrap();
    drop(first);
    std::fs::remove_dir_all(&dir).unwrap();
    let again = Recorded::write(&dir, pid).expect("recorded in the folder made again");
    let names: Vec<String> = std::fs::read_dir(&dir)
        .unwrap()
        .flatten()
        .map(|e| e.file_name().into_string().unwrap())
        .collect();
    let owner = names.iter().find_map(|n| n.strip_suffix(".owner")).expect("an owner beside the entry");
    assert!(names.contains(&format!("{owner}-{pid}.tool")), "{names:?}");
    drop(again);
}
