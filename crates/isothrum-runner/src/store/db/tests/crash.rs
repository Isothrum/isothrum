//! The store across a crash: a process killed while its write transaction has already spilled
//! pages into the WAL, the WAL a killed process committed into replayed at the next open, a
//! store whose file is gone or whose pages are damaged behind a sound header rebuilt from its
//! mirror, and one whose mirror can't be read refused as `store_corrupt`.

use std::io::{Seek as _, SeekFrom, Write as _};
use std::path::Path;

use isothrum_test_support::PidGuard;

use super::robust::{a_home_with_a_vm, damage};
use super::*;

/// The variable that makes [`a_child_killed_mid_write`] write in the home it names, and hang
/// inside its last transaction until it is killed.
const CHILD_HOME: &str = "ISOTHRUM_TEST_STORE_CRASH_HOME";

/// The runs the child commits, each in a transaction of its own.
const COMMITTED: [&str; 3] = ["c1", "c2", "c3"];

/// How many runs the child's cut transaction writes: more than its page cache holds, so its
/// pages spill into the WAL before any commit.
const CUT: usize = 300;

/// Where the child writes the WAL's length once its commits are in.
const WAL_COMMITTED: &str = "wal-committed";

/// The store's WAL in `home`, and its length.
fn wal_len(home: &Path) -> u64 {
    std::fs::metadata(home.join(format!("{DB_FILE}-wal"))).map_or(0, |m| m.len())
}

/// Not a test of its own: the child of the tests below, run only when they set [`CHILD_HOME`].
/// Its schema checkpointed into the main file, it commits [`COMMITTED`] (into the WAL only), then
/// writes [`CUT`] runs in a transaction that never ends.
#[test]
fn a_child_killed_mid_write() {
    let Some(home) = std::env::var_os(CHILD_HOME) else { return };
    let home = Path::new(&home);
    let store = Store::open(home).unwrap();
    store
        .conn
        .lock()
        .unwrap()
        .execute_batch("PRAGMA wal_checkpoint(TRUNCATE); PRAGMA cache_size = 8;")
        .unwrap();
    for n in COMMITTED {
        store.write(|tx| tx.save_record(&record(n), Desired::Running)).unwrap();
    }
    std::fs::write(home.join(WAL_COMMITTED), wal_len(home).to_string()).unwrap();
    drop(store.write(|tx| -> crate::Result<()> {
        for i in 0..CUT {
            tx.save_record(&record(&format!("cut{i}")), Desired::Running)?;
        }
        println!("inside");
        std::io::stdout().flush().unwrap();
        loop {
            std::thread::park();
        }
    }));
}

/// The child run in `home` and killed (`SIGKILL`, `TerminateProcess`) inside its cut transaction;
/// whether it was inside it.
fn killed_mid_write(home: &Path) -> bool {
    let (mut child, inside) = crate::testing::child_test_saying("store::db::tests::crash::a_child_killed_mid_write", CHILD_HOME, home, "inside");
    drop(PidGuard::new(child.id()));
    child.wait().unwrap();
    inside
}

/// The runs `store` holds, by name, sorted.
fn runs_of(store: &Store) -> Vec<String> {
    let mut names: Vec<String> = store
        .read(|tx| tx.runs().map(|rows| rows.into_iter().map(|r| r.name.to_string()).collect()))
        .unwrap();
    names.sort();
    names
}

#[test]
fn a_process_killed_mid_write_keeps_each_commit_and_none_of_the_cut_transaction() {
    let home = tempfile::tempdir().unwrap();
    assert!(killed_mid_write(home.path()), "the child was inside its transaction");
    let committed: u64 = std::fs::read_to_string(home.path().join(WAL_COMMITTED)).unwrap().parse().unwrap();
    assert!(
        wal_len(home.path()) > committed,
        "the cut transaction's pages spilled into the WAL ({} > {committed})",
        wal_len(home.path())
    );
    let store = Store::open(home.path()).expect("the store opens, checked");
    assert_eq!(store.recovered(), None, "nothing to repair");
    assert_eq!(runs_of(&store), COMMITTED, "every commit, and nothing of the cut transaction");
    store
        .write(|tx| tx.save_record(&record("after"), Desired::Running))
        .expect("and takes writes again");
}

#[test]
fn the_wal_a_killed_process_committed_into_is_replayed_at_open() {
    let home = tempfile::tempdir().unwrap();
    assert!(killed_mid_write(home.path()));
    // The main file alone, without its WAL: the commits aren't in it.
    let bare = tempfile::tempdir().unwrap();
    std::fs::copy(home.path().join(DB_FILE), bare.path().join(DB_FILE)).unwrap();
    let conn = rusqlite::Connection::open(bare.path().join(DB_FILE)).unwrap();
    let in_main: i64 = conn.query_row("SELECT COUNT(*) FROM runs", [], |r| r.get(0)).unwrap();
    assert_eq!(in_main, 0, "the commits live in the WAL only");
    let store = Store::open(home.path()).unwrap();
    assert_eq!(runs_of(&store), COMMITTED, "replayed from the WAL");
}

/// Removes the store's file in `home`, with its `-wal` and `-shm`: its mirror stays.
fn remove_store(home: &Path) {
    for suffix in ["", "-wal", "-shm"] {
        let file = home.join(format!("{DB_FILE}{suffix}"));
        if file.exists() {
            std::fs::remove_file(file).unwrap();
        }
    }
}

#[test]
fn a_store_whose_file_is_gone_is_rebuilt_from_its_mirror() {
    let (home, op) = a_home_with_a_vm();
    remove_store(home.path());
    let rebuilt = Store::open(home.path()).expect("rebuilt from the mirror");
    assert!(rebuilt.recovered().is_some(), "says it repaired");
    let row = rebuilt.read(|tx| tx.run(&name("web"))).unwrap().expect("the run is back");
    assert_eq!((row.state, row.desired), (RunState::Absent, Desired::Running));
    let unfinished: Vec<OperationId> = rebuilt.read(|tx| tx.unfinished(&name("web"))).unwrap().iter().map(|o| o.id).collect();
    assert_eq!(unfinished, [op]);
}

/// The page the `runs` table's rows start at, in the store of `home`, and the store's page size.
fn runs_root_page(home: &Path) -> (u64, u64) {
    let conn = rusqlite::Connection::open(home.join(DB_FILE)).unwrap();
    let root: i64 = conn
        .query_row("SELECT rootpage FROM sqlite_master WHERE type = 'table' AND name = 'runs'", [], |r| r.get(0))
        .unwrap();
    let size: i64 = conn.query_row("PRAGMA page_size", [], |r| r.get(0)).unwrap();
    (u64::try_from(root).unwrap(), u64::try_from(size).unwrap())
}

#[test]
fn a_store_damaged_behind_a_sound_header_is_found_by_its_check_and_read_back_from_its_mirror() {
    let (home, _) = a_home_with_a_vm();
    let (root, size) = runs_root_page(home.path());
    assert!(root > 1, "the runs aren't on the header's page");
    let mut f = std::fs::OpenOptions::new().write(true).open(home.path().join(DB_FILE)).unwrap();
    f.seek(SeekFrom::Start((root - 1) * size)).unwrap();
    f.write_all(&vec![0xA5; usize::try_from(size).unwrap()]).unwrap();
    f.sync_all().unwrap();
    drop(f);
    let rebuilt = Store::open(home.path()).expect("rebuilt from the mirror");
    assert!(rebuilt.recovered().is_some(), "the integrity check found the damage");
    assert_eq!(runs_of(&rebuilt), ["web"]);
}

#[test]
fn a_damaged_store_whose_mirror_cant_be_read_is_store_corrupt_and_left_in_place() {
    let (home, _) = a_home_with_a_vm();
    std::fs::write(home.path().join(MIRROR_FILE), b"{ not a mirror").unwrap();
    damage(&home.path().join(DB_FILE));
    let err = Store::open(home.path()).expect_err("nothing to rebuild it from");
    assert_eq!(err.code(), ErrorCode::StoreCorrupt, "{err}");
    let names: Vec<String> = std::fs::read_dir(home.path())
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .filter(|n| n.starts_with(DB_FILE))
        .collect();
    isothrum_test_support::assert_every!(&names, |n: &&String| !n.contains("corrupt"));
    assert!(home.path().join(DB_FILE).is_file(), "left where it was");
}

/// Every recorded download.
fn recorded(tx: &Tx<'_>) -> Result<Vec<crate::Download>> {
    tx.downloads()
}

#[test]
fn a_download_is_recorded_once_and_a_store_rebuilt_from_its_mirror_keeps_it() {
    let home = tempfile::tempdir().unwrap();
    let image = crate::DownloadItem::Image {
        reference: "nginx:1.27".into(),
        image_id: "sha256:a1".into(),
        docker_context: Some("orbstack".into()),
    };
    let held = crate::DownloadItem::Box {
        name: "bento/debian-12".into(),
        provider: "virtualbox".into(),
        version: "202510.26.0".into(),
        architecture: None,
    };
    {
        let store = Store::open(home.path()).unwrap();
        let run = name("web");
        assert!(store.write(|tx| tx.record_download(Some(&run), &image)).unwrap(), "new");
        assert!(!store.write(|tx| tx.record_download(Some(&run), &image)).unwrap(), "recorded once");
        assert!(store.write(|tx| tx.record_download(Some(&run), &held)).unwrap());
    }
    remove_store(home.path());
    let rebuilt = Store::open(home.path()).expect("rebuilt from the mirror");
    assert!(rebuilt.recovered().is_some(), "says it repaired");
    let kept = rebuilt.read(recorded).unwrap();
    let items: Vec<&crate::DownloadItem> = kept.iter().map(|d| &d.item).collect();
    assert_eq!(items, [&image, &held]);
    assert!(
        kept.iter()
            .all(|d| d.run.as_ref().is_some_and(|r| r.as_str() == "web") && d.size_bytes.is_none()),
        "{kept:?}"
    );
    rebuilt.write(|tx| tx.forget_download(kept[0].id)).unwrap();
    assert_eq!(rebuilt.read(recorded).unwrap().len(), 1, "forgotten");
}
