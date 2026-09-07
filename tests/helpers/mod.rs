#![allow(dead_code)]

use std::path::Path;
use std::process::Command;
use std::time::Duration;

use relentless::retry;
use relentless::stop;
use relentless::wait;

const POLL_INTERVAL: Duration = Duration::from_millis(10);
/// Generous, because it bounds only failure. Polling returns the instant the
/// condition holds, so a large bound costs a passing test nothing; a small one
/// fails tests that fork a daemon and wait for it to write through a redirected
/// descriptor whenever the machine is loaded — a shared CI runner, or a local
/// run with other work alongside it.
const POLL_TIMEOUT: Duration = Duration::from_secs(30);

/// Poll `f` at fixed 10ms intervals until it yields `Some`, or give up.
fn poll_until<T>(mut f: impl FnMut() -> Option<T>) -> Option<T> {
    retry(move |_| f())
        .wait(wait::fixed(POLL_INTERVAL))
        .stop(stop::elapsed(POLL_TIMEOUT))
        .call()
        .ok()
}

/// Process information gathered via platform-specific backends.
pub struct ProcessInfo {
    pub pid: u32,
    pub ppid: u32,
    pub sid: u32,
    pub uid: u32,
    pub gid: u32,
    pub cwd: String,
}

/// Query process information for a given PID.
///
/// Uses `ps -o` on all Unix platforms for portability.
pub fn query_process(pid: u32) -> Option<ProcessInfo> {
    let output = Command::new("ps")
        .args(["-o", "pid=,ppid=,sess=,uid=,gid=", "-p", &pid.to_string()])
        .output()
        .ok()?;

    if !output.status.success() {
        return None;
    }

    let line = String::from_utf8_lossy(&output.stdout);
    let fields: Vec<&str> = line.split_whitespace().collect();
    if fields.len() < 5 {
        return None;
    }

    let pid = fields[0].parse().ok()?;
    let ppid = fields[1].parse().ok()?;
    let sid = fields[2].parse().ok()?;
    let uid = fields[3].parse().ok()?;
    let gid = fields[4].parse().ok()?;

    let cwd = query_cwd(pid).unwrap_or_default();

    Some(ProcessInfo {
        pid,
        ppid,
        sid,
        uid,
        gid,
        cwd,
    })
}

/// Query the current working directory of a process.
///
/// Tries procfs and falls back to `lsof`, deciding at runtime rather than by
/// OS name. Gating the procfs path on `target_os = "linux"` sent Android — a
/// kernel with `/proc` and no `lsof` — down a fallback that cannot answer
/// there, so it now reads the `/proc` it has.
///
/// A platform where neither works still yields `None`, which
/// [`query_process`] turns into an empty cwd rather than an error; that is
/// unchanged, and is why the assertions using it compare against a path they
/// expect rather than merely checking it is non-empty.
fn query_cwd(pid: u32) -> Option<String> {
    if let Ok(path) = std::fs::read_link(format!("/proc/{pid}/cwd")) {
        return Some(path.to_string_lossy().into_owned());
    }

    let output = Command::new("lsof")
        .args(["-a", "-p", &pid.to_string(), "-d", "cwd", "-Fn"])
        .output()
        .ok()?;

    if !output.status.success() {
        return None;
    }

    let text = String::from_utf8_lossy(&output.stdout);
    for line in text.lines() {
        if let Some(path) = line.strip_prefix('n') {
            return Some(path.to_string());
        }
    }
    None
}

/// Path to the built CLI binary.
pub fn daemonize_bin() -> std::path::PathBuf {
    let mut path = std::env::current_exe().unwrap();
    path.pop(); // remove test binary name
    path.pop(); // remove deps/
    path.push("daemonize");
    path
}

/// Wait for a pidfile to appear and return its contents as a PID.
pub fn wait_for_pidfile(path: &Path) -> Option<u32> {
    poll_until(|| {
        std::fs::read_to_string(path)
            .ok()
            .and_then(|c| c.trim().parse::<u32>().ok())
    })
}

/// Wait for a process to die.
pub fn wait_for_exit(pid: u32) -> bool {
    poll_until(|| {
        let dead = unsafe { libc::kill(pid as i32, 0) } != 0;
        dead.then_some(())
    })
    .is_some()
}

/// Kill a process (best-effort).
pub fn kill_process(pid: u32) {
    unsafe { libc::kill(pid as i32, libc::SIGTERM) };
    std::thread::sleep(std::time::Duration::from_millis(100));
    unsafe { libc::kill(pid as i32, libc::SIGKILL) };
}

/// Poll a file until it contains `expected`, returning its full content.
///
/// On timeout, returns whatever content exists for better assertion messages.
pub fn wait_for_file_content(path: &Path, expected: &str) -> String {
    poll_until(|| {
        std::fs::read_to_string(path)
            .ok()
            .filter(|c| c.contains(expected))
    })
    .unwrap_or_else(|| std::fs::read_to_string(path).unwrap_or_default())
}
