//! The process hosting a Claude session, as `/proc` names it, and the one proof that it died.
//! The heartbeat helper names its own parent, the `claude` process that runs the session, and
//! the service reads that process back. Only the absence of the process proves anything: a
//! process that exists says nothing about whether the session is there.

use std::fs;
use std::io::ErrorKind;

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
        let (_, start_time) = stat(pid).ok()??;
        let (boot_id, pid_namespace) = here()?;
        Some(Self {
            pid,
            start_time,
            boot_id,
            pid_namespace,
        })
    }

    /// Whether the pid names a process and every label fits and none is empty.
    pub fn is_valid(&self) -> bool {
        self.pid != 0
            && [&self.boot_id, &self.pid_namespace]
                .iter()
                .all(|label| !label.is_empty() && label.len() <= MAX_LABEL_BYTES)
    }

    /// Whether this process is proven dead, read from this machine and pid namespace: its
    /// `/proc/<pid>/stat` is missing, names another start time, or shows it a zombie (`Z`) or
    /// dead (`X`). Anything unreadable or unclear, and a host on another boot or in another
    /// pid namespace, proves nothing.
    pub fn is_dead(&self) -> bool {
        if here().as_ref() != Some(&(self.boot_id.clone(), self.pid_namespace.clone())) {
            return false;
        }
        match stat(self.pid) {
            Err(error) => error.kind() == ErrorKind::NotFound,
            Ok(Some((state, start_time))) => {
                start_time != self.start_time || matches!(state, 'Z' | 'X')
            }
            Ok(None) => false,
        }
    }
}

/// This process's boot ID and pid namespace.
fn here() -> Option<(String, String)> {
    let boot = fs::read_to_string("/proc/sys/kernel/random/boot_id").ok()?;
    let namespace = fs::read_link("/proc/self/ns/pid").ok()?;
    Some((boot.trim().to_owned(), namespace.to_str()?.to_owned()))
}

/// The state and start time in `/proc/<pid>/stat`, read after the last `)` because the command
/// name before it may hold spaces and parentheses; `None` when the text does not parse.
fn stat(pid: u32) -> std::io::Result<Option<(char, u64)>> {
    let text = fs::read_to_string(format!("/proc/{pid}/stat"))?;
    let Some((_, rest)) = text.rsplit_once(')') else {
        return Ok(None);
    };
    let mut fields = rest.split_whitespace();
    // Fields 3 to 22: the state, then 18 more up to the start time.
    let state = fields.next().and_then(|field| field.chars().next());
    let start = fields.nth(18).and_then(|field| field.parse().ok());
    Ok(state.zip(start))
}
