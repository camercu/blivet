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
fn poll_until<T>(f: impl FnMut() -> Option<T>) -> Option<T> {
    poll_for(POLL_TIMEOUT, f)
}

/// Poll `f` at fixed 10ms intervals until it yields `Some`, or `bound` passes.
fn poll_for<T>(bound: Duration, mut f: impl FnMut() -> Option<T>) -> Option<T> {
    retry(move |_| f())
        .wait(wait::fixed(POLL_INTERVAL))
        .stop(stop::elapsed(bound))
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
///
/// Cargo exports it, so this neither walks up from the test binary's own path
/// nor needs to know how deep `deps/` is.
pub fn daemonize_bin() -> std::path::PathBuf {
    std::path::PathBuf::from(env!("CARGO_BIN_EXE_daemonize"))
}

/// Wait for a pidfile to appear and return its contents as a PID.
pub fn wait_for_pidfile(path: &Path) -> Option<u32> {
    poll_until(|| {
        std::fs::read_to_string(path)
            .ok()
            .and_then(|c| c.trim().parse::<u32>().ok())
    })
}

/// Wait for a child to exit, polling, so one that never does fails its test at
/// the poll bound instead of hanging the run.
pub fn wait_for_child(child: &mut std::process::Child) -> Option<std::process::ExitStatus> {
    poll_until(|| child.try_wait().ok().flatten())
}

/// How long [`kill_process`] lets a process handle `SIGTERM` before `SIGKILL`.
///
/// It bounds only the unusual case — a target that ignores `SIGTERM`, or an
/// orphan nothing reaps (the containers run with `--init` so something does).
/// Every target the suite starts exits on `SIGTERM`, and polling returns the
/// moment it has.
const TERM_GRACE: Duration = Duration::from_secs(2);

/// A daemon that runs until it is stopped, stopped when this is dropped.
///
/// Dropping happens on a failed assertion too, so a failing test does not
/// leave its daemon running after the run. Only for a program that runs until
/// it is stopped: a PID names a process only while it lives, and once a short
/// program has exited its PID can be handed to an unrelated one, which the drop
/// would then signal. A test whose program exits by itself waits with
/// [`wait_for_pidfile`] instead.
pub struct Daemon {
    pid: u32,
}

impl Daemon {
    pub fn pid(&self) -> u32 {
        self.pid
    }
}

impl Drop for Daemon {
    fn drop(&mut self) {
        kill_process(self.pid);
    }
}

/// Wait for a long-running daemon's pidfile, and hold the daemon it names.
pub fn wait_for_daemon(pidfile: &Path) -> Option<Daemon> {
    wait_for_pidfile(pidfile).map(|pid| Daemon { pid })
}

/// Stop a process: `SIGTERM`, then `SIGKILL` if it is still there after
/// [`TERM_GRACE`]. Reached only through [`Daemon`], which says when that is
/// safe.
///
/// This waits by polling, like every other wait in this module. A fixed sleep
/// here cost each of its call sites the whole sleep whether or not the process
/// had already gone.
fn kill_process(pid: u32) {
    unsafe { libc::kill(pid as i32, libc::SIGTERM) };
    let gone = poll_for(TERM_GRACE, || {
        let gone = unsafe { libc::kill(pid as i32, 0) } != 0;
        gone.then_some(())
    });
    if gone.is_none() {
        unsafe { libc::kill(pid as i32, libc::SIGKILL) };
    }
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
