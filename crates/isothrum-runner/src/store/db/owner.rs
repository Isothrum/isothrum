//! Who holds something: a process, the boot it runs in, and a token per holder.

use serde::{Deserialize, Serialize};

/// A process holding a lease or running an operation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[non_exhaustive]
pub struct Owner {
    /// Its process id.
    pub pid: u32,
    /// The boot it runs in (Linux: the kernel's boot id; elsewhere empty, and the pid and the
    /// lease's expiry decide).
    pub boot: String,
    /// A random token telling two holders of one process apart.
    pub token: String,
}

impl Owner {
    /// This process, with a new token.
    #[must_use]
    pub fn this_process() -> Owner {
        let mut bytes = [0u8; 8];
        // A token that couldn't be drawn is the time's: unique enough within one process.
        if getrandom::fill(&mut bytes).is_err() {
            let nanos = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |d| d.subsec_nanos());
            bytes = u64::from(nanos).to_le_bytes();
        }
        Owner {
            pid: std::process::id(),
            boot: this_boot(),
            token: bytes
                .iter()
                .flat_map(|b| [b >> 4, b & 0x0f])
                .map(|nibble| char::from_digit(u32::from(nibble), 16).unwrap_or('0'))
                .collect(),
        }
    }

    /// Whether its process may still run: not when it ran in another boot, nor when no process
    /// has its pid (Unix: signal 0; Windows: `OpenProcess`). A pid reused since is taken as alive:
    /// the lease's expiry decides then.
    #[must_use]
    pub fn is_alive(&self) -> bool {
        if self.boot != this_boot() {
            return false;
        }
        if self.pid == std::process::id() {
            return true;
        }
        pid_alive(self.pid)
    }
}

/// This boot's id: the kernel's on Linux, else none.
fn this_boot() -> String {
    #[cfg(target_os = "linux")]
    {
        if let Ok(id) = std::fs::read_to_string("/proc/sys/kernel/random/boot_id") {
            return id.trim().to_owned();
        }
    }
    String::new()
}

#[cfg(unix)]
fn pid_alive(pid: u32) -> bool {
    use nix::errno::Errno;
    let Ok(raw) = i32::try_from(pid) else {
        return false;
    };
    // Signal 0 checks the process exists; EPERM means it does, another user's.
    match nix::sys::signal::kill(nix::unistd::Pid::from_raw(raw), None) {
        Ok(()) | Err(Errno::EPERM) => true,
        Err(_) => false,
    }
}

/// Windows: whether a process has the pid and hasn't exited, as the system says.
#[cfg(windows)]
fn pid_alive(pid: u32) -> bool {
    crate::process::process_runs(pid)
}

#[cfg(not(any(unix, windows)))]
fn pid_alive(_pid: u32) -> bool {
    true
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    #[test]
    fn this_process_is_alive_and_a_holder_of_another_boot_or_a_gone_pid_isnt() {
        let me = Owner::this_process();
        assert!(me.is_alive());
        assert_ne!(me.token, Owner::this_process().token, "a token per holder");
        let mut other_boot = me.clone();
        other_boot.boot = "another-boot".into();
        other_boot.pid = 1;
        assert!(!other_boot.is_alive());
        #[cfg(unix)]
        {
            let mut gone = me.clone();
            gone.pid = dead_pid();
            assert!(!gone.is_alive());
        }
    }

    /// The pid of a process that ran and exited.
    #[cfg(unix)]
    pub(crate) fn dead_pid() -> u32 {
        let mut child = std::process::Command::new("true").spawn().unwrap();
        let pid = child.id();
        child.wait().unwrap();
        pid
    }
}
