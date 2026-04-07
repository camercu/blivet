#![allow(dead_code)]

use std::path::Path;
use std::process::Command;
use std::time::Duration;

use relentless::retry;
use relentless::stop;
use relentless::wait;

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
#[cfg(target_os = "linux")]
fn query_cwd(pid: u32) -> Option<String> {
    std::fs::read_link(format!("/proc/{pid}/cwd"))
        .ok()
        .map(|p| p.to_string_lossy().into_owned())
}

#[cfg(not(target_os = "linux"))]
fn query_cwd(pid: u32) -> Option<String> {
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
pub fn wait_for_pidfile(path: &Path, timeout_ms: u64) -> Option<u32> {
    retry(|_| {
        std::fs::read_to_string(path)
            .ok()
            .and_then(|c| c.trim().parse::<u32>().ok())
            .ok_or(())
    })
    .wait(wait::fixed(Duration::from_millis(50)))
    .stop(stop::elapsed(Duration::from_millis(timeout_ms)))
    .call()
    .ok()
}

/// Wait for a process to die.
pub fn wait_for_exit(pid: u32, timeout_ms: u64) -> bool {
    retry(|_| {
        let ret = unsafe { libc::kill(pid as i32, 0) };
        if ret != 0 { Ok(()) } else { Err(()) }
    })
    .wait(wait::fixed(Duration::from_millis(50)))
    .stop(stop::elapsed(Duration::from_millis(timeout_ms)))
    .call()
    .is_ok()
}

/// Kill a process (best-effort).
pub fn kill_process(pid: u32) {
    unsafe { libc::kill(pid as i32, libc::SIGTERM) };
    std::thread::sleep(std::time::Duration::from_millis(100));
    unsafe { libc::kill(pid as i32, libc::SIGKILL) };
}
