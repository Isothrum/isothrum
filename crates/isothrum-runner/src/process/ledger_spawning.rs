//! A tool being created, recorded from just before it is created until it is in its job (Windows).
//!
//! A tool is created suspended, put in its job object, then resumed (`process_wrap`'s
//! `JobObject`). A process killed between its creation and its assignment (Task Manager's End
//! task, a crash, `TerminateProcess`) left it in no job and suspended, with nothing left to
//! resume or end it: it never ran, and never ended. Its pid isn't known before it is created, so
//! [`Spawning`]'s entry `<owner>-spawn<n>.spawn` names its creator (this process's id), the
//! program and when it was being created; the next operation, finding the owner dead, ends the
//! process of that program the owner started since, if it is still there.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

/// The entries' extension.
pub(super) const EXT: &str = "spawn";

/// A tool being created: its entry, removed when this drops.
#[derive(Debug)]
pub(crate) struct Spawning(PathBuf);

impl Spawning {
    /// Records in `dir` that this process is creating the tool `program` (as started: `cmd` for a
    /// shimmed tool). A record that can't be written is traced: the tool is created unrecorded.
    pub(crate) fn write(dir: &Path, program: &str) -> Option<Spawning> {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let since = super::now_secs()?;
        let written = super::owner_token(dir).and_then(|token| {
            let path = dir.join(format!("{token}-spawn{}.{EXT}", NEXT.fetch_add(1, Ordering::Relaxed)));
            std::fs::write(&path, format!("{} {since} {program}\n", std::process::id()))?;
            Ok(path)
        });
        match written {
            Ok(path) => Some(Spawning(path)),
            Err(e) => {
                tracing::warn!(error = %e, dir = %dir.display(), "a tool being created couldn't be recorded: a process killed meanwhile would leave it suspended");
                None
            }
        }
    }
}

impl Drop for Spawning {
    fn drop(&mut self) {
        super::remove_entry(&self.0);
    }
}

/// What an entry says: the creator's id, since when (seconds since the epoch), the program.
#[cfg(any(windows, test))]
#[derive(Debug, PartialEq, Eq)]
pub(super) struct Entry {
    owner: u32,
    since: u64,
    program: String,
}

#[cfg(any(windows, test))]
impl Entry {
    pub(super) fn parse(text: &str) -> Option<Entry> {
        let mut words = text.trim_end().splitn(3, ' ');
        let owner = words.next()?.parse().ok()?;
        let since = words.next()?.parse().ok()?;
        let program = words.next().filter(|p| !p.is_empty())?.to_owned();
        Some(Entry { owner, since, program })
    }
}

/// A process another started, as Windows lists it.
#[cfg(any(windows, test))]
#[derive(Debug, PartialEq, Eq)]
struct Child {
    pid: u32,
    /// When it was created, in seconds since the epoch.
    created: u64,
    /// Its image's file name (`docker.exe`).
    image: String,
}

/// The processes listed one per line as `<pid> <created> <image>` (what [`children_of`] has
/// PowerShell print); a line that doesn't read is skipped.
#[cfg(any(windows, test))]
fn parse_children(text: &str) -> Vec<Child> {
    text.lines()
        .filter_map(|line| {
            let mut words = line.trim().splitn(3, ' ');
            Some(Child {
                pid: words.next()?.parse().ok()?,
                created: words.next()?.parse().ok()?,
                image: words.next().filter(|i| !i.is_empty())?.to_owned(),
            })
        })
        .collect()
}

/// Which of `children` (the processes the dead owner's id started) is the tool `entry` names:
/// of its program, created since (within the second the OS rounds to), and before `reused` when
/// a later process has the owner's id now (what that one started is its own; one created in the
/// same second can't be told apart, and is left alone).
#[cfg(any(windows, test))]
fn orphans(entry: &Entry, reused: Option<u64>, children: &[Child]) -> Vec<u32> {
    children
        .iter()
        .filter(|c| c.created.saturating_add(super::SLACK) >= entry.since)
        .filter(|c| reused.is_none_or(|r| c.created.saturating_add(super::SLACK) < r))
        .filter(|c| program_name(&c.image).eq_ignore_ascii_case(program_name(&entry.program)))
        .map(|c| c.pid)
        .collect()
}

/// A program's name as an image is named: its file name, without `.exe`.
#[cfg(any(windows, test))]
fn program_name(program: &str) -> &str {
    let name = program.rsplit(['/', '\\']).next().unwrap_or(program);
    match name.len().checked_sub(4).and_then(|at| name.get(at..).map(|ext| (at, ext))) {
        Some((at, ext)) if ext.eq_ignore_ascii_case(".exe") => name.get(..at).unwrap_or(name),
        _ => name,
    }
}

/// Ends the tool a dead owner was creating as it died, if it is still there: the ids ended.
#[cfg(windows)]
pub(super) async fn end(entry: &Entry) -> Vec<u32> {
    // The owner is gone: a process with its id now is a later one.
    let reused = super::os_started_at(entry.owner).await;
    let ended = orphans(entry, reused, &children_of(entry.owner).await);
    for pid in &ended {
        tracing::warn!(pid, program = %entry.program, "ending a tool a killed process left suspended outside its job");
        super::end_group(*pid).await;
    }
    ended
}

/// The processes `parent`'s id started that still run (Windows keeps a process's parent id after
/// the parent is gone).
#[cfg(windows)]
async fn children_of(parent: u32) -> Vec<Child> {
    let script = format!(
        "Get-CimInstance Win32_Process -Filter 'ParentProcessId={parent}' | ForEach-Object {{ '{{0}} {{1}} {{2}}' -f $_.ProcessId, ([DateTimeOffset]$_.CreationDate).ToUnixTimeSeconds(), $_.Name }}"
    );
    let mut cmd = tokio::process::Command::new("powershell");
    cmd.args(["-NoProfile", "-NonInteractive", "-Command", &script])
        .stdin(std::process::Stdio::null());
    super::headless(&mut cmd);
    match cmd.output().await {
        Ok(out) if out.status.success() => parse_children(&String::from_utf8_lossy(&out.stdout)),
        Ok(out) => {
            tracing::warn!(parent, code = ?out.status.code(), "the processes a killed process started couldn't be listed");
            Vec::new()
        }
        Err(e) => {
            tracing::warn!(parent, error = %e, "PowerShell couldn't run to list the processes a killed process started");
            Vec::new()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_entry_reads_back_and_a_damaged_one_reads_as_none() {
        assert_eq!(
            Entry::parse("4100 1794415415 C:\\Program Files\\Docker\\docker.exe\n"),
            Some(Entry {
                owner: 4100,
                since: 1_794_415_415,
                program: r"C:\Program Files\Docker\docker.exe".into(),
            })
        );
        for bad in ["", "4100", "4100 1794415415", "4100 1794415415 \n", "x 1 docker", "1 x docker"] {
            assert_eq!(Entry::parse(bad), None, "{bad:?}");
        }
    }

    #[test]
    fn the_listed_processes_read_and_a_line_that_doesnt_is_skipped() {
        let listed = "120 1794415415 docker.exe\r\n\r\nnot a process\r\n121 1794415416 Docker Desktop.exe\r\n";
        assert_eq!(
            parse_children(listed),
            [
                Child {
                    pid: 120,
                    created: 1_794_415_415,
                    image: "docker.exe".into()
                },
                Child {
                    pid: 121,
                    created: 1_794_415_416,
                    image: "Docker Desktop.exe".into()
                },
            ]
        );
    }

    #[test]
    fn a_program_is_named_as_its_image_is() {
        for (program, name) in [
            ("docker", "docker"),
            ("DOCKER.EXE", "DOCKER"),
            (r"C:\Tools\vagrant.exe", "vagrant"),
            ("/usr/bin/terraform", "terraform"),
            (".exe", ""),
            ("exe", "exe"),
        ] {
            assert_eq!(program_name(program), name, "{program}");
        }
    }

    /// Regression (#73): a tool created as the runner's process was killed stayed suspended, in no
    /// job, for good. It is the dead owner's child of the program recorded, created since; not
    /// what a later process with the owner's id started, nor anything else the owner started.
    #[test]
    fn the_tool_a_dead_process_was_creating_is_its_child_of_that_program_created_since() {
        let since = 1_000;
        let entry = Entry {
            owner: 4100,
            since,
            program: "vagrant".into(),
        };
        let child = |pid, created, image: &str| Child {
            pid,
            created,
            image: image.into(),
        };
        let children = [
            child(1, since, "vagrant.exe"),
            // The OS rounds to the second.
            child(2, since - 1, "VAGRANT.EXE"),
            // Started long before: another of the owner's tools, done with its job.
            child(3, since - 60, "vagrant.exe"),
            child(4, since, "ruby.exe"),
        ];
        assert_eq!(orphans(&entry, None, &children), [1, 2]);
        // A later process has the owner's id: only what was created clearly before it.
        assert_eq!(orphans(&entry, Some(since + 2), &children), [2]);
        assert_eq!(orphans(&entry, Some(since + 60), &children), [1, 2]);
    }
}
