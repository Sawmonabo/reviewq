//! What `/proc` says about a process, for the hosts that must prove one ended: the one reader
//! of that format. Only absence proves anything: a process that exists says nothing about
//! whether the work it hosted is still there.

use std::fs;
use std::io::ErrorKind;

/// `/proc/sys/kernel/random/boot_id`, which names this boot of the machine, or `None` when it
/// cannot be read.
pub fn boot_id() -> Option<String> {
    let boot = fs::read_to_string("/proc/sys/kernel/random/boot_id").ok()?;
    Some(boot.trim().to_owned())
}

/// The `/proc/self/ns/pid` link, which names the pid namespace this process reads pids in, or
/// `None` when it cannot be read.
pub fn pid_namespace() -> Option<String> {
    Some(
        fs::read_link("/proc/self/ns/pid")
            .ok()?
            .to_str()?
            .to_owned(),
    )
}

/// When process `pid` started, in clock ticks after boot, or `None` when it cannot be read.
pub fn start_time(pid: u32) -> Option<u64> {
    stat(pid).ok()?.map(|(_, start_time)| start_time)
}

/// Whether process `pid` of this boot, read in this process's pid namespace, is proven to have
/// ended: its `/proc/<pid>/stat` is missing, names a start time other than `start_time` when one
/// is given, or shows a zombie (`Z`) or dead (`X`) process. Anything unreadable or unclear
/// proves nothing.
pub fn ended(pid: u32, start_time: Option<u64>) -> bool {
    match stat(pid) {
        Err(error) => error.kind() == ErrorKind::NotFound,
        Ok(Some((state, started))) => {
            start_time.is_some_and(|start| start != started) || matches!(state, 'Z' | 'X')
        }
        Ok(None) => false,
    }
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
