use super::group::{ps_group, stat_group};
use super::start::{lstart, stat_ticks};
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
        assert_eq!(Start::from_entry_text(&start.entry_text()), Some(start.clone()));
        assert!(start.same_process_as(&start));
    }
    assert!(Start::At(100).same_process_as(&Start::At(102)) && Start::At(102).same_process_as(&Start::At(100)));
    assert!(!Start::At(100).same_process_as(&Start::At(103)), "another process that got the id later");
    let boot = |ticks| Start::Boot { boot: "b".into(), ticks };
    assert!(!boot(1).same_process_as(&boot(2)) && !boot(1).same_process_as(&Start::At(1)));
    for bad in ["", "at:", "at:x", "boot:1", "boot:b:x", "time:1"] {
        assert_eq!(Start::from_entry_text(bad), None, "{bad:?}");
    }
}

#[test]
fn an_entry_names_the_container_its_tool_runs_in() {
    let start = Start::At(1_794_415_415);
    let named = entry_text(4242, &start, Some("isothrum-tool-1"));
    let entry = |container: Option<&str>, members| Entry {
        pid: 4242,
        start: start.clone(),
        container: container.map(str::to_owned),
        members,
    };
    assert_eq!(parse_entry(&named), Some(entry(Some("isothrum-tool-1"), vec![])));
    assert_eq!(parse_entry(&entry_text(4242, &start, None)), Some(entry(None, vec![])));
    assert_eq!(parse_entry("4242"), None);
}

#[test]
fn an_entry_reads_back_the_members_of_its_tools_group_and_skips_a_line_cut_short() {
    let start = Start::At(1_794_415_415);
    let text = format!("{}4243 at:1794415416\n4244 boot:b:77\n4245 at:", entry_text(4242, &start, None));
    let read = parse_entry(&text).unwrap();
    assert_eq!(read.container, None, "a member's line is no container");
    assert_eq!(
        read.members,
        [(4243, Start::At(1_794_415_416)), (4244, Start::Boot { boot: "b".into(), ticks: 77 })]
    );
}

#[test]
fn a_group_reads_from_proc_and_from_ps() {
    let stat = "4243 (terraform-provider (x)) S 4242 4242 4200 0 -1 4194560 120 0 0 0 0 0 0 0 20 0 1 0 987654";
    assert_eq!(stat_group(stat), Some(4242));
    assert_eq!(stat_group("4243 (sh) S 1"), None);
    // `TZ=UTC LC_ALL=C ps -A -o pid=,pgid=,lstart=`.
    let ps = "    1     1 Wed Nov 11 16:00:00 2026\n 4242  4242 Wed Nov 11 16:43:35 2026\n 4243  4242 Wed Nov 11 16:43:36 2026\n 4244  4300 Wed Nov 11 16:43:37 2026\n 4245  4242 garbled\n";
    assert_eq!(
        ps_group(ps, 4242),
        [(4243, Start::At(1_794_415_416))],
        "its members only, the tool itself aside"
    );
}

#[test]
fn the_oss_start_reads_from_proc_and_from_ps() {
    let stat = "4242 (sh) deploy (x)) S 1 4242 4242 0 -1 4194560 120 0 0 0 0 0 0 0 20 0 1 0 987654 2240512 210";
    assert_eq!(stat_ticks(stat), Some(987_654));
    assert_eq!(stat_ticks("4242 (sh) S 1"), None);
    // `TZ=UTC LC_ALL=C ps -o lstart=`.
    assert_eq!(lstart("Wed Nov 11 16:43:35 2026\n"), Some(1_794_415_415));
    assert_eq!(lstart("Thu Feb 29 00:00:00 2024"), Some(1_709_164_800));
    for bad in [
        "",
        "Wed Foo 11 16:43:35 2026",
        "Wed Nov 11 16:43 2026",
        "Wed Nov 0 16:43:35 2026",
        "Wed Nov 11 24:00:00 2026",
        "Wed Nov 11 16:60:00 2026",
        "Wed Nov 11 16:43:35:01 2026",
        "Thu Feb 29 00:00:00 2026",
        "Mon Nov 31 00:00:00 2026",
    ] {
        assert_eq!(lstart(bad), None, "{bad:?}");
    }
}

/// `sleep` in a process group of its own, as the runner starts every tool, guarded.
#[cfg(unix)]
fn tool() -> isothrum_test_support::ChildGuard {
    isothrum_test_support::ChildGuard::spawn(std::process::Command::new("sleep").arg("60")).unwrap()
}

/// Regression (tests/resilience.md A4): a process killed mid-operation left its tools running to
/// their end. A tool a dead process recorded is ended, its entry gone; a live owner's tool, and a
/// recorded id that now names another process (here, this very test), are left alone.
#[cfg(unix)]
#[tokio::test]
async fn a_dead_owners_tools_are_ended_and_nothing_it_didnt_start() {
    let home = tempfile::tempdir().unwrap();
    let dir = home.path().join(DIR);
    let (orphan, owned) = (tool(), tool());
    let orphan_id = orphan.id();
    // Should the test fail, the orphan goes with this guard, and its reaper with it.
    let _orphan = isothrum_test_support::PidGuard::new(orphan_id);
    left_by_a_killed_process(&dir, orphan_id).unwrap();
    // Reaped as soon as it ends, as init reaps a tool whose parent was killed: a zombie still
    // counts in its process group, and the group would only be killed after the grace.
    let (reaped, reaping) = std::sync::mpsc::channel();
    std::thread::spawn(move || reaped.send(orphan.wait_with_output().is_ok()));
    // This process's own tool: its owner (this process) holds its lock.
    let mine = Recorded::write(&dir, owned.id(), None).unwrap();
    // An id that names another process now: recorded as started at the epoch.
    let me = std::process::id();
    std::fs::write(dir.join(format!("dead-{me}.tool")), format!("{me} at:1\n")).unwrap();

    let asked = std::time::Instant::now();
    assert_eq!(end_orphans(&dir).await, [orphan_id]);
    assert!(asked.elapsed() < GRACE, "it stopped when asked, not killed after the grace");
    assert_eq!(reaping.recv_timeout(Duration::from_secs(30)), Ok(true), "the orphaned tool was ended");
    assert!(isothrum_test_support::alive(owned.id()), "a live process's tool runs on");
    let left: Vec<String> = std::fs::read_dir(&dir)
        .unwrap()
        .flatten()
        .map(|e| e.file_name().into_string().unwrap())
        .collect();
    assert_eq!(left.len(), 2, "this process's lock and its tool's entry: {left:?}");
    isothrum_test_support::assert_every!(&left, |n: &&String| !n.starts_with("dead"));

    drop(mine);
    drop(owned);
    assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 1, "the ended tool's entry went with it");
}

/// A tool (`sh`) in a process group of its own that started a process of its group (`sleep`), as
/// Terraform starts its provider plugin, guarded; with the id of what it started. A shell's
/// background process ignores SIGINT: what it started takes it back, as a plugin does, and only
/// then writes its id, so a SIGINT never finds it still ignoring one (on a loaded machine `perl`
/// starts slowly, and an id written by the shell came back before it had).
#[cfg(unix)]
fn tool_with_a_member(home: &Path, name: &str) -> (isothrum_test_support::ChildGuard, isothrum_test_support::PidGuard) {
    let file = home.join(name);
    let script = format!(
        "perl -e '$SIG{{INT}} = \"DEFAULT\"; my $f = shift; open(my $o, \">\", \"$f.new\") or die; print $o $$; close($o) or die; rename(\"$f.new\", $f) or die; exec @ARGV' '{}' sleep 600 & wait",
        file.display()
    );
    let tool = isothrum_test_support::ChildGuard::spawn(std::process::Command::new("sh").args(["-c", &script])).unwrap();
    assert!(isothrum_test_support::wait_until(Duration::from_secs(10), || {
        isothrum_test_support::read_pid(&file).is_some()
    }));
    let member = isothrum_test_support::PidGuard::from_file(&file).unwrap();
    (tool, member)
}

/// Writes the entry `dead-<leader>.tool` of `dir`, recorded by a process that has died: the
/// tool `leader` with its `start`, and the `members` of its group, each with its start.
#[cfg(unix)]
fn dead_entry(dir: &Path, leader: u32, start: &str, members: &[(u32, String)]) {
    crate::store::private_dir(dir).unwrap();
    std::fs::write(dir.join("dead.owner"), "").unwrap();
    let lines = members.iter().map(|(pid, started)| format!("{pid} {started}\n")).collect::<Vec<_>>().concat();
    std::fs::write(dir.join(format!("dead-{leader}.tool")), format!("{leader} {start}\n{lines}")).unwrap();
}

/// Regression: a process killed while Terraform applied left Terraform's provider plugin
/// running: Terraform went with it, and a group whose leader was gone was left alone, so the
/// next `terraform init` failed with "text file busy". What the tool started, recorded while it
/// ran, is ended once the tool is gone.
#[cfg(unix)]
#[tokio::test]
async fn what_a_dead_owners_tool_started_is_ended_once_the_tool_is_gone() {
    use nix::sys::signal::{Signal, kill};
    let home = tempfile::tempdir().unwrap();
    let dir = home.path().join(DIR);
    let (mut tool, member) = tool_with_a_member(home.path(), "member.pid");
    let leader = tool.id();
    left_by_a_killed_process(&dir, leader).unwrap();
    let entry = dir.join(format!("dead-{leader}.tool"));
    let mut noted = Vec::new();
    note_members(&entry, leader, &mut noted).await;
    assert_eq!(noted.iter().map(|m| m.0).collect::<Vec<_>>(), [member.id()], "what the tool started, recorded");
    note_members(&entry, leader, &mut noted).await;
    assert_eq!(parse_entry(&std::fs::read_to_string(&entry).unwrap()).unwrap().members, noted, "recorded once");

    // The tool killed outright, as the process that ran it was, and reaped.
    kill(nix::unistd::Pid::from_raw(i32::try_from(leader).unwrap()), Signal::SIGKILL).unwrap();
    assert!(isothrum_test_support::wait_until(Duration::from_secs(10), || tool
        .try_wait()
        .unwrap()
        .is_some()));
    assert!(isothrum_test_support::alive(member.id()), "what it started runs on");

    let asked = std::time::Instant::now();
    assert_eq!(end_orphans(&dir).await, [leader]);
    assert!(asked.elapsed() < GRACE, "it stopped when asked, not killed after the grace");
    assert!(
        isothrum_test_support::wait_until(Duration::from_secs(10), || !isothrum_test_support::alive(member.id())),
        "what the tool started was ended"
    );
    assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 0, "its entry and its owner's gone");
}

/// A group is ended only on a member's proof: a member recorded whose id now names another
/// process (a reused id), and one that is the very process recorded but no longer in the group
/// (a foreign group), leave the group and themselves alone.
#[cfg(unix)]
#[tokio::test]
async fn a_group_no_recorded_member_proves_is_left_alone() {
    let home = tempfile::tempdir().unwrap();
    let dir = home.path().join(DIR);
    // A group with a member, each recorded as started at the epoch: their ids reused since.
    let (reused, reused_member) = tool_with_a_member(home.path(), "reused.pid");
    dead_entry(&dir, reused.id(), "at:1", &[(reused_member.id(), "at:1".to_owned())]);
    // A process recorded as it is, but as a member of a group it isn't in (this test's id, once
    // another process's).
    let foreign = tool();
    let foreign_start = os_start(foreign.id()).await.unwrap().entry_text();
    let me = std::process::id();
    dead_entry(&dir, me, "at:1", &[(foreign.id(), foreign_start)]);

    let none: [u32; 0] = [];
    assert_eq!(end_orphans(&dir).await, none);
    for pid in [reused.id(), reused_member.id(), foreign.id()] {
        assert!(isothrum_test_support::alive(pid), "{pid} runs on");
    }
    assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 0, "the entries are gone all the same");
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
    let first = Recorded::write(&dir, pid, None).unwrap();
    drop(first);
    std::fs::remove_dir_all(&dir).unwrap();
    let again = Recorded::write(&dir, pid, None).expect("recorded in the folder made again");
    let names: Vec<String> = std::fs::read_dir(&dir)
        .unwrap()
        .flatten()
        .map(|e| e.file_name().into_string().unwrap())
        .collect();
    let owner = names.iter().find_map(|n| n.strip_suffix(".owner")).expect("an owner beside the entry");
    assert!(names.contains(&format!("{owner}-{pid}.tool")), "{names:?}");
    drop(again);
}
