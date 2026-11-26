//! The process hosting a Claude session, and the one proof that it died. The heartbeat helper
//! names its own parent, the `claude` process that runs the session, and the service reads
//! that process back through `hosts::process`. The service keeps a host only when it ran on the
//! service's own boot and pid namespace when it was reported (`is_local`), so a later boot
//! proves that host's kernel, and the host with it, has stopped.

use crate::hosts::process;

/// Longest boot ID or pid namespace accepted, in bytes. A boot ID is a 36-byte UUID and a
/// namespace reads `pid:[<inode>]`.
const MAX_LABEL_BYTES: usize = 64;

/// One process, identified across pid reuse, reboots and pid namespaces.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ClaudeHost {
    pub pid: u32,
    /// Field 22 of `/proc/<pid>/stat`: when the process started, in clock ticks after boot.
    pub start_time: u64,
    /// `/proc/sys/kernel/random/boot_id` of the machine it runs on.
    pub boot_id: String,
    /// The `/proc/self/ns/pid` link of the process that read `pid`, which names the pid
    /// namespace that number belongs to.
    pub pid_namespace: String,
}

impl ClaudeHost {
    /// The process that started this one, or `None` when it cannot be read.
    pub fn parent() -> Option<Self> {
        Self::of(std::os::unix::process::parent_id())
    }

    /// Process `pid` as this process sees it, or `None` when it cannot be read.
    pub fn of(pid: u32) -> Option<Self> {
        Some(Self {
            pid,
            start_time: process::start_time(pid)?,
            boot_id: process::boot_id()?,
            pid_namespace: process::pid_namespace()?,
        })
    }

    /// Whether the pid names a process and every label fits and none is empty.
    pub fn is_valid(&self) -> bool {
        self.pid != 0
            && [&self.boot_id, &self.pid_namespace]
                .iter()
                .all(|label| !label.is_empty() && label.len() <= MAX_LABEL_BYTES)
    }

    /// Whether this process names this service's own boot and pid namespace, read now.
    /// Anything unreadable is not.
    pub fn is_local(&self) -> bool {
        process::boot_id().is_some_and(|boot| boot == self.boot_id)
            && process::pid_namespace().is_some_and(|namespace| namespace == self.pid_namespace)
    }

    /// Whether this host, which was `is_local` when it was reported, is proven dead: this
    /// service now runs on another boot, or on the same boot and pid namespace the process
    /// has ended (`process::ended`). Anything unreadable or unclear, and another pid namespace
    /// on the same boot, proves nothing.
    pub fn is_dead(&self) -> bool {
        match process::boot_id() {
            None => false,
            Some(boot) if boot != self.boot_id => true,
            Some(_) => {
                process::pid_namespace().is_some_and(|namespace| namespace == self.pid_namespace)
                    && process::ended(self.pid, Some(self.start_time))
            }
        }
    }
}
