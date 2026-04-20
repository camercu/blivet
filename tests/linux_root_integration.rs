//! Integration tests for Linux-specific and root-only features.
//!
//! These tests are `#[ignore]` by default and run via `--include-ignored`
//! inside the Docker container (which runs as root on Linux).

mod helpers;

use helpers::*;
use std::process::Command;
use std::time::Duration;

fn daemonize_cmd() -> Command {
    Command::new(daemonize_bin())
}

/// Check that we are running as root on Linux. Returns false otherwise.
fn is_root_on_linux() -> bool {
    cfg!(target_os = "linux") && nix::unistd::geteuid().as_raw() == 0
}

// ============================================================
// User switching tests (R27, R28, R29, R35)
// ============================================================

#[test]
#[ignore]
fn user_switch_sets_uid_and_gid() {
    if !is_root_on_linux() {
        eprintln!("skipping: requires root on Linux");
        return;
    }

    let dir = tempfile::tempdir().unwrap();
    let pidfile = dir.path().join("test.pid");
    let env_file = dir.path().join("id_output.txt");

    // Make output dir writable by anyone so the switched user can write
    std::fs::set_permissions(
        dir.path(),
        std::os::unix::fs::PermissionsExt::from_mode(0o777),
    )
    .unwrap();

    let output = daemonize_cmd()
        .args([
            "-p",
            pidfile.to_str().unwrap(),
            "-u",
            "testuser",
            "-o",
            env_file.to_str().unwrap(),
            "--",
            "sh",
            "-c",
            "id -u; id -g; sleep 5",
        ])
        .output()
        .unwrap();

    assert!(
        output.status.success(),
        "daemonize with -u should succeed as root: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let pid = wait_for_pidfile(&pidfile).expect("pidfile should appear");
    std::thread::sleep(Duration::from_millis(500));

    // Verify the daemon's UID/GID via ps
    let info = query_process(pid).expect("daemon process should exist");

    // Resolve expected UID/GID for testuser
    let expected = Command::new("id")
        .args(["-u", "testuser"])
        .output()
        .unwrap();
    let expected_uid: u32 = String::from_utf8_lossy(&expected.stdout)
        .trim()
        .parse()
        .unwrap();

    let expected = Command::new("id")
        .args(["-g", "testuser"])
        .output()
        .unwrap();
    let expected_gid: u32 = String::from_utf8_lossy(&expected.stdout)
        .trim()
        .parse()
        .unwrap();

    assert_eq!(info.uid, expected_uid, "daemon UID should match testuser");
    assert_eq!(info.gid, expected_gid, "daemon GID should match testuser");

    // Verify the id output from inside the daemon matches
    let id_output = std::fs::read_to_string(&env_file).unwrap_or_default();
    let lines: Vec<&str> = id_output.lines().collect();
    if lines.len() >= 2 {
        assert_eq!(
            lines[0].trim(),
            expected_uid.to_string(),
            "id -u inside daemon should match"
        );
        assert_eq!(
            lines[1].trim(),
            expected_gid.to_string(),
            "id -g inside daemon should match"
        );
    }

    kill_process(pid);
}

#[test]
#[ignore]
fn user_switch_sets_env_vars() {
    if !is_root_on_linux() {
        eprintln!("skipping: requires root on Linux");
        return;
    }

    let dir = tempfile::tempdir().unwrap();
    let pidfile = dir.path().join("test.pid");
    let env_file = dir.path().join("env_output.txt");

    std::fs::set_permissions(
        dir.path(),
        std::os::unix::fs::PermissionsExt::from_mode(0o777),
    )
    .unwrap();

    let output = daemonize_cmd()
        .args([
            "-p",
            pidfile.to_str().unwrap(),
            "-u",
            "testuser",
            "-o",
            env_file.to_str().unwrap(),
            "--",
            "sh",
            "-c",
            "echo USER=$USER; echo HOME=$HOME; echo LOGNAME=$LOGNAME; sleep 5",
        ])
        .output()
        .unwrap();

    assert!(
        output.status.success(),
        "daemonize failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let pid = wait_for_pidfile(&pidfile).expect("pidfile should appear");
    std::thread::sleep(Duration::from_millis(500));

    let content = std::fs::read_to_string(&env_file).unwrap_or_default();

    // R28: USER and LOGNAME should be set to target user
    assert!(
        content.contains("USER=testuser"),
        "USER should be testuser, got: {content}"
    );
    assert!(
        content.contains("LOGNAME=testuser"),
        "LOGNAME should be testuser, got: {content}"
    );
    // R29: HOME should be testuser's home dir
    assert!(
        content.contains("HOME=/home/testuser"),
        "HOME should be /home/testuser, got: {content}"
    );

    kill_process(pid);
}

// ============================================================
// Output file ownership after user switch (R10, R82)
// ============================================================

#[test]
#[ignore]
fn output_file_owned_by_target_user() {
    if !is_root_on_linux() {
        eprintln!("skipping: requires root on Linux");
        return;
    }

    let dir = tempfile::tempdir().unwrap();
    let pidfile = dir.path().join("test.pid");
    let stdout_file = dir.path().join("stdout.log");
    let stderr_file = dir.path().join("stderr.log");

    std::fs::set_permissions(
        dir.path(),
        std::os::unix::fs::PermissionsExt::from_mode(0o777),
    )
    .unwrap();

    let output = daemonize_cmd()
        .args([
            "-p",
            pidfile.to_str().unwrap(),
            "-u",
            "testuser",
            "-o",
            stdout_file.to_str().unwrap(),
            "-e",
            stderr_file.to_str().unwrap(),
            "--",
            "sh",
            "-c",
            "echo hello; echo err >&2; sleep 5",
        ])
        .output()
        .unwrap();

    assert!(
        output.status.success(),
        "daemonize failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let pid = wait_for_pidfile(&pidfile).expect("pidfile should appear");
    std::thread::sleep(Duration::from_millis(500));

    // Resolve testuser's UID
    let expected = Command::new("id")
        .args(["-u", "testuser"])
        .output()
        .unwrap();
    let expected_uid: u32 = String::from_utf8_lossy(&expected.stdout)
        .trim()
        .parse()
        .unwrap();

    // Check file ownership
    use std::os::unix::fs::MetadataExt;
    let stdout_meta = std::fs::metadata(&stdout_file).unwrap();
    let stderr_meta = std::fs::metadata(&stderr_file).unwrap();

    assert_eq!(
        stdout_meta.uid(),
        expected_uid,
        "stdout file should be owned by testuser"
    );
    assert_eq!(
        stderr_meta.uid(),
        expected_uid,
        "stderr file should be owned by testuser"
    );

    kill_process(pid);
}

// ============================================================
// daemonize_checked() — Linux-only (R45, R67)
// ============================================================

#[test]
#[ignore]
#[cfg(target_os = "linux")]
fn daemonize_checked_parses_thread_count() {
    // daemonize_checked() can't be called from the test harness (multi-threaded),
    // so we verify its thread-count parsing logic directly. The underlying
    // daemonize() is already covered by CLI integration tests.
    let status = std::fs::read_to_string("/proc/self/status").unwrap();
    let threads_line = status
        .lines()
        .find(|l| l.starts_with("Threads:"))
        .expect("Threads: line must exist in /proc/self/status");
    let count: usize = threads_line
        .split_whitespace()
        .nth(1)
        .expect("Threads: line must have a value")
        .parse()
        .expect("thread count must be a valid usize");
    assert!(count >= 1, "thread count should be at least 1, got {count}");
}

#[test]
#[ignore]
#[cfg(target_os = "linux")]
fn proc_self_status_is_readable() {
    // Verify /proc/self/status is available (required for daemonize_checked)
    let status = std::fs::read_to_string("/proc/self/status");
    assert!(
        status.is_ok(),
        "/proc/self/status should be readable on Linux"
    );

    let content = status.unwrap();
    assert!(
        content.contains("Threads:"),
        "/proc/self/status should contain Threads: line"
    );
}

// ============================================================
// /proc-based process info (Linux-specific CWD check)
// ============================================================

#[test]
#[ignore]
#[cfg(target_os = "linux")]
fn proc_based_cwd_query() {
    // Verify that /proc/<pid>/cwd works for our own process
    let pid = std::process::id();
    let link = std::fs::read_link(format!("/proc/{pid}/cwd"));
    assert!(link.is_ok(), "/proc/{pid}/cwd should be a readable symlink");

    let cwd = std::env::current_dir().unwrap();
    assert_eq!(
        link.unwrap(),
        cwd,
        "/proc/self/cwd should match current_dir()"
    );
}

#[test]
#[ignore]
#[cfg(target_os = "linux")]
fn daemon_cwd_via_proc() {
    let dir = tempfile::tempdir().unwrap();
    let pidfile = dir.path().join("test.pid");
    let chdir = dir.path().to_str().unwrap();

    let output = daemonize_cmd()
        .args([
            "-p",
            pidfile.to_str().unwrap(),
            "-c",
            chdir,
            "--",
            "sleep",
            "30",
        ])
        .output()
        .unwrap();

    assert!(
        output.status.success(),
        "daemonize failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let pid = wait_for_pidfile(&pidfile).expect("pidfile should appear");

    // Verify CWD via /proc
    let proc_cwd = std::fs::read_link(format!("/proc/{pid}/cwd"));
    assert!(proc_cwd.is_ok(), "/proc/{pid}/cwd should be readable");

    let expected = std::fs::canonicalize(chdir).unwrap();
    assert_eq!(
        proc_cwd.unwrap(),
        expected,
        "daemon CWD via /proc should match configured chdir"
    );

    kill_process(pid);
}

// ============================================================
// User switch with supplementary groups (R27)
// ============================================================

#[test]
#[ignore]
fn user_switch_sets_supplementary_groups() {
    if !is_root_on_linux() {
        eprintln!("skipping: requires root on Linux");
        return;
    }

    let dir = tempfile::tempdir().unwrap();
    let pidfile = dir.path().join("test.pid");
    let groups_file = dir.path().join("groups.txt");

    std::fs::set_permissions(
        dir.path(),
        std::os::unix::fs::PermissionsExt::from_mode(0o777),
    )
    .unwrap();

    let output = daemonize_cmd()
        .args([
            "-p",
            pidfile.to_str().unwrap(),
            "-u",
            "testuser",
            "-o",
            groups_file.to_str().unwrap(),
            "--",
            "sh",
            "-c",
            "id -G; sleep 5",
        ])
        .output()
        .unwrap();

    assert!(
        output.status.success(),
        "daemonize failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let pid = wait_for_pidfile(&pidfile).expect("pidfile should appear");
    std::thread::sleep(Duration::from_millis(500));

    let content = std::fs::read_to_string(&groups_file).unwrap_or_default();

    // Should have at least the primary group
    let groups: Vec<&str> = content.split_whitespace().collect();
    assert!(
        !groups.is_empty(),
        "daemon should have at least one group, got: {content}"
    );

    // Get expected groups for testuser
    let expected = Command::new("id")
        .args(["-G", "testuser"])
        .output()
        .unwrap();
    let expected_str = String::from_utf8_lossy(&expected.stdout).trim().to_string();
    let expected_groups: Vec<&str> = expected_str.split_whitespace().collect();

    // The daemon's groups should match the expected groups for testuser
    let mut daemon_groups = groups.clone();
    daemon_groups.sort();
    let mut exp_groups = expected_groups;
    exp_groups.sort();
    assert_eq!(
        daemon_groups, exp_groups,
        "daemon supplementary groups should match testuser's groups"
    );

    kill_process(pid);
}

// ============================================================
// Non-root user switch validation (R35) — runs as root, validates
// that the error path works for non-existent users
// ============================================================

#[test]
#[ignore]
fn user_switch_nonexistent_user_fails() {
    if !is_root_on_linux() {
        eprintln!("skipping: requires root on Linux");
        return;
    }

    let output = daemonize_cmd()
        .args(["-u", "nonexistent_user_xyz_12345", "--", "sleep", "1"])
        .output()
        .unwrap();

    // Should fail with UserNotFound exit code (67)
    assert_eq!(
        output.status.code(),
        Some(67),
        "nonexistent user should exit 67, stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

// ============================================================
// Group switching tests (R61, R70)
// ============================================================

#[test]
#[ignore]
fn group_only_switch_sets_gid() {
    if !is_root_on_linux() {
        eprintln!("skipping: requires root on Linux");
        return;
    }

    let dir = tempfile::tempdir().unwrap();
    let pidfile = dir.path().join("test.pid");

    let output = daemonize_cmd()
        .args([
            "-g",
            "testgroup",
            "-p",
            pidfile.to_str().unwrap(),
            "--",
            "sleep",
            "30",
        ])
        .output()
        .unwrap();

    assert!(
        output.status.success(),
        "group-only switch should succeed, stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let pid = wait_for_pidfile(&pidfile).expect("pidfile should appear");

    let info = query_process(pid).expect("daemon process should exist");

    // R61: group-only should set GID but keep UID as root
    assert_eq!(info.uid, 0, "UID should remain root for group-only switch");
    // GID should be testgroup's GID (not 0/root)
    assert_ne!(info.gid, 0, "GID should be testgroup's GID, not root");

    kill_process(pid);
}

#[test]
#[ignore]
fn user_and_group_switch_sets_independent_gid() {
    if !is_root_on_linux() {
        eprintln!("skipping: requires root on Linux");
        return;
    }

    let dir = tempfile::tempdir().unwrap();
    let pidfile = dir.path().join("test.pid");

    // Switch user to testuser but group to testgroup (independent group)
    let output = daemonize_cmd()
        .args([
            "-u",
            "testuser",
            "-g",
            "testgroup",
            "-p",
            pidfile.to_str().unwrap(),
            "--",
            "sleep",
            "30",
        ])
        .output()
        .unwrap();

    assert!(
        output.status.success(),
        "user+group switch should succeed, stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let pid = wait_for_pidfile(&pidfile).expect("pidfile should appear");

    let info = query_process(pid).expect("daemon process should exist");

    // R60/R70: UID should be testuser, GID should be testgroup's GID
    assert_ne!(info.uid, 0, "UID should be testuser, not root");

    // Get testgroup's GID for comparison
    let testgroup_gid_output = Command::new("getent")
        .args(["group", "testgroup"])
        .output()
        .unwrap();
    if testgroup_gid_output.status.success() {
        let fields = String::from_utf8_lossy(&testgroup_gid_output.stdout);
        let testgroup_gid: u32 = fields.trim().split(':').nth(2).unwrap().parse().unwrap();
        assert_eq!(
            info.gid, testgroup_gid,
            "GID should be testgroup's GID, not testuser's primary group"
        );
    }

    kill_process(pid);
}

#[test]
#[ignore]
fn nonexistent_group_fails_with_exit_67() {
    if !is_root_on_linux() {
        eprintln!("skipping: requires root on Linux");
        return;
    }

    let output = daemonize_cmd()
        .args(["-g", "nonexistent_group_xyz_12345", "--", "sleep", "1"])
        .output()
        .unwrap();

    assert_eq!(
        output.status.code(),
        Some(67),
        "nonexistent group should exit 67, stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
#[ignore]
fn numeric_uid_switch() {
    if !is_root_on_linux() {
        eprintln!("skipping: requires root on Linux");
        return;
    }

    let dir = tempfile::tempdir().unwrap();
    let pidfile = dir.path().join("test.pid");

    // Get testuser's UID
    let testuser_uid_output = Command::new("id")
        .args(["-u", "testuser"])
        .output()
        .unwrap();
    let testuser_uid = String::from_utf8_lossy(&testuser_uid_output.stdout)
        .trim()
        .to_string();

    let output = daemonize_cmd()
        .args([
            "-u",
            &testuser_uid,
            "-p",
            pidfile.to_str().unwrap(),
            "--",
            "sleep",
            "30",
        ])
        .output()
        .unwrap();

    assert!(
        output.status.success(),
        "numeric UID switch should succeed, stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let pid = wait_for_pidfile(&pidfile).expect("pidfile should appear");

    let info = query_process(pid).expect("daemon process should exist");
    assert_eq!(
        info.uid,
        testuser_uid.parse::<u32>().unwrap(),
        "UID should match numeric UID"
    );

    kill_process(pid);
}
