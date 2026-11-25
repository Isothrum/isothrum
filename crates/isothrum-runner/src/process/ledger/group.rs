//! A tool's process group as the host reads it: its members and their starts, to tell it is still the group recorded.

use super::Start;
#[cfg(any(all(unix, not(target_os = "linux")), test))]
use super::start::lstart;
#[cfg(all(unix, not(target_os = "linux")))]
use super::start::ps;
#[cfg(target_os = "linux")]
use super::start::stat_ticks;

/// Whether a process of `members`, recorded in `leader`'s group, is still the very process
/// recorded and still in that group: then the group is the one recorded, since a group's id isn't
/// another group's while a process of it lives.
#[cfg(unix)]
pub(super) async fn members_still_in_group(leader: u32, members: &[(u32, Start)]) -> bool {
    if members.is_empty() {
        return false;
    }
    let now = group_members(leader).await;
    members
        .iter()
        .any(|(pid, start)| now.iter().any(|(id, now)| id == pid && start.same_process_as(now)))
}

/// On Windows the tool's job object ended its whole tree with the process that held it: nothing
/// of the group is left to prove.
#[cfg(windows)]
pub(super) fn members_still_in_group(_leader: u32, _members: &[(u32, Start)]) -> std::future::Ready<bool> {
    std::future::ready(false)
}

/// The processes of process group `leader`, other than `leader` itself, each with its start.
#[cfg(target_os = "linux")]
pub(super) async fn group_members(leader: u32) -> Vec<(u32, Start)> {
    tokio::task::spawn_blocking(move || {
        let Ok(boot) = std::fs::read_to_string("/proc/sys/kernel/random/boot_id") else {
            return Vec::new();
        };
        let Ok(read) = std::fs::read_dir("/proc") else { return Vec::new() };
        read.flatten()
            .filter_map(|e| e.file_name().to_str()?.parse::<u32>().ok())
            .filter(|pid| *pid != leader)
            .filter_map(|pid| {
                let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
                if stat_group(&stat)? != leader {
                    return None;
                }
                let ticks = stat_ticks(&stat)?;
                Some((
                    pid,
                    Start::Boot {
                        boot: boot.trim().to_owned(),
                        ticks,
                    },
                ))
            })
            .collect()
    })
    .await
    .unwrap_or_default()
}

/// The processes of process group `leader`, other than `leader` itself, each with its start, as
/// `ps` reads them (`lstart`, in UTC and the C locale's words).
#[cfg(all(unix, not(target_os = "linux")))]
pub(super) async fn group_members(leader: u32) -> Vec<(u32, Start)> {
    let out = ps(&["-A", "-o", "pid=,pgid=,lstart="]).await;
    match out {
        Ok(out) => ps_group(&String::from_utf8_lossy(&out.stdout), leader),
        Err(e) => {
            tracing::debug!(error = %e, "ps couldn't run to read a tool's process group");
            Vec::new()
        }
    }
}

/// The members of `leader`'s group, other than itself, in `ps -A -o pid=,pgid=,lstart=`'s lines.
#[cfg(any(all(unix, not(target_os = "linux")), test))]
pub(super) fn ps_group(text: &str, leader: u32) -> Vec<(u32, Start)> {
    text.lines()
        .filter_map(|line| {
            let (pid, rest) = line.trim_start().split_once(char::is_whitespace)?;
            let (group, started) = rest.trim_start().split_once(char::is_whitespace)?;
            let (pid, group): (u32, u32) = (pid.parse().ok()?, group.parse().ok()?);
            if group != leader || pid == leader {
                return None;
            }
            Some((pid, Start::At(lstart(started)?)))
        })
        .collect()
}

/// The process group in a `/proc/<pid>/stat` line (field 5).
#[cfg(any(target_os = "linux", test))]
pub(super) fn stat_group(stat: &str) -> Option<u32> {
    /// `pgrp` is field 5 (from 1); the first field after the name is field 3.
    const PGRP_AFTER_NAME: usize = 5 - 3;
    stat.rsplit_once(')')?.1.split_whitespace().nth(PGRP_AFTER_NAME)?.parse().ok()
}
