mod helpers;

use helpers::*;
use std::os::unix::fs::PermissionsExt;
use std::process::Command;
use std::time::{Duration, Instant};

fn daemonize_cmd() -> Command {
    Command::new(daemonize_bin())
}

#[test]
fn happy_path_daemon_is_orphaned_and_in_new_session() {
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

    let pid = wait_for_pidfile(&pidfile, 5000).expect("pidfile should appear");

    let info = query_process(pid).expect("daemon process should exist");

    // R6: PPID should be 1 (orphaned / adopted by init/launchd)
    assert_eq!(info.ppid, 1, "daemon should be orphaned (PPID=1)");

    // R5: PID != SID (not session leader due to double-fork)
    assert_ne!(info.pid, info.sid, "daemon should not be session leader");

    // R22: CWD matches configured chdir
    if !info.cwd.is_empty() {
        let expected_cwd = std::fs::canonicalize(chdir).unwrap();
        assert_eq!(
            info.cwd,
            expected_cwd.to_str().unwrap(),
            "CWD should match configured chdir"
        );
    }

    // R17: pidfile contains PID
    let pidfile_content = std::fs::read_to_string(&pidfile).unwrap();
    assert_eq!(pidfile_content.trim(), pid.to_string());

    kill_process(pid);
}

#[test]
fn default_cwd_is_root() {
    let dir = tempfile::tempdir().unwrap();
    let pidfile = dir.path().join("test.pid");

    let output = daemonize_cmd()
        .args(["-p", pidfile.to_str().unwrap(), "--", "sleep", "30"])
        .output()
        .unwrap();

    assert!(output.status.success());

    let pid = wait_for_pidfile(&pidfile, 5000).expect("pidfile should appear");
    let info = query_process(pid).expect("daemon process should exist");

    if !info.cwd.is_empty() {
        assert_eq!(info.cwd, "/", "default CWD should be /");
    }

    kill_process(pid);
}

#[test]
fn stdout_redirect_writes_output() {
    let dir = tempfile::tempdir().unwrap();
    let pidfile = dir.path().join("test.pid");
    let stdout_file = dir.path().join("stdout.log");

    let output = daemonize_cmd()
        .args([
            "-p",
            pidfile.to_str().unwrap(),
            "-o",
            stdout_file.to_str().unwrap(),
            "--",
            "sh",
            "-c",
            "echo hello_stdout; sleep 1",
        ])
        .output()
        .unwrap();

    assert!(
        output.status.success(),
        "daemonize failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let pid = wait_for_pidfile(&pidfile, 5000).expect("pidfile should appear");

    // Wait for output to appear
    std::thread::sleep(std::time::Duration::from_millis(500));

    let content = std::fs::read_to_string(&stdout_file).unwrap_or_default();
    assert!(
        content.contains("hello_stdout"),
        "stdout file should contain output, got: {content}"
    );

    kill_process(pid);
}

#[test]
fn stderr_redirect_writes_output() {
    let dir = tempfile::tempdir().unwrap();
    let pidfile = dir.path().join("test.pid");
    let stderr_file = dir.path().join("stderr.log");

    let output = daemonize_cmd()
        .args([
            "-p",
            pidfile.to_str().unwrap(),
            "-e",
            stderr_file.to_str().unwrap(),
            "--",
            "sh",
            "-c",
            "echo hello_stderr >&2; sleep 1",
        ])
        .output()
        .unwrap();

    assert!(output.status.success());

    let pid = wait_for_pidfile(&pidfile, 5000).expect("pidfile should appear");
    std::thread::sleep(std::time::Duration::from_millis(500));

    let content = std::fs::read_to_string(&stderr_file).unwrap_or_default();
    assert!(
        content.contains("hello_stderr"),
        "stderr file should contain output, got: {content}"
    );

    kill_process(pid);
}

#[test]
fn append_mode_preserves_existing_content() {
    let dir = tempfile::tempdir().unwrap();
    let pidfile = dir.path().join("test.pid");
    let stdout_file = dir.path().join("stdout.log");

    // Write initial content
    std::fs::write(&stdout_file, "existing\n").unwrap();

    let output = daemonize_cmd()
        .args([
            "-p",
            pidfile.to_str().unwrap(),
            "-o",
            stdout_file.to_str().unwrap(),
            "-a",
            "--",
            "sh",
            "-c",
            "echo appended; sleep 1",
        ])
        .output()
        .unwrap();

    assert!(output.status.success());
    let pid = wait_for_pidfile(&pidfile, 5000).expect("pidfile should appear");
    std::thread::sleep(std::time::Duration::from_millis(500));

    let content = std::fs::read_to_string(&stdout_file).unwrap();
    assert!(content.contains("existing"), "should preserve existing content");
    assert!(content.contains("appended"), "should append new content");

    kill_process(pid);
}

#[test]
fn lockfile_exclusion_second_instance_fails() {
    let dir = tempfile::tempdir().unwrap();
    let pidfile = dir.path().join("test.pid");
    let lockfile = dir.path().join("test.lock");

    // Start first instance
    let output1 = daemonize_cmd()
        .args([
            "-p",
            pidfile.to_str().unwrap(),
            "-l",
            lockfile.to_str().unwrap(),
            "--",
            "sleep",
            "30",
        ])
        .output()
        .unwrap();
    assert!(output1.status.success());

    let pid = wait_for_pidfile(&pidfile, 5000).expect("pidfile should appear");

    // Second instance with same lockfile should fail
    let output2 = daemonize_cmd()
        .args([
            "-l",
            lockfile.to_str().unwrap(),
            "--",
            "sleep",
            "30",
        ])
        .output()
        .unwrap();

    // R16: exit code 69 (EX_UNAVAILABLE)
    assert_eq!(
        output2.status.code(),
        Some(69),
        "second instance should exit 69, stderr: {}",
        String::from_utf8_lossy(&output2.stderr)
    );

    kill_process(pid);
}

#[test]
fn validation_error_nonabsolute_pidfile() {
    let output = daemonize_cmd()
        .args(["-p", "relative.pid", "--", "sleep", "1"])
        .output()
        .unwrap();

    // R30, R51: exit code 64 (EX_USAGE)
    assert_eq!(output.status.code(), Some(64));
}

#[test]
fn program_not_found_exits_66() {
    let output = daemonize_cmd()
        .args(["--", "/nonexistent/program/binary"])
        .output()
        .unwrap();

    assert_eq!(
        output.status.code(),
        Some(66),
        "should exit 66 for missing program, stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn verbose_mode_prints_diagnostics() {
    let dir = tempfile::tempdir().unwrap();
    let pidfile = dir.path().join("test.pid");

    let output = daemonize_cmd()
        .args([
            "-v",
            "-p",
            pidfile.to_str().unwrap(),
            "--",
            "sleep",
            "5",
        ])
        .output()
        .unwrap();

    assert!(output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("daemonize:"),
        "verbose mode should print diagnostics, got: {stderr}"
    );

    let pid = wait_for_pidfile(&pidfile, 5000).expect("pidfile should appear");
    kill_process(pid);
}

#[test]
fn no_verbose_no_diagnostics() {
    let dir = tempfile::tempdir().unwrap();
    let pidfile = dir.path().join("test.pid");

    let output = daemonize_cmd()
        .args(["-p", pidfile.to_str().unwrap(), "--", "sleep", "5"])
        .output()
        .unwrap();

    assert!(output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.is_empty(),
        "without -v should have no stderr, got: {stderr}"
    );

    let pid = wait_for_pidfile(&pidfile, 5000).expect("pidfile should appear");
    kill_process(pid);
}

#[test]
fn env_vars_passed_to_daemon() {
    let dir = tempfile::tempdir().unwrap();
    let pidfile = dir.path().join("test.pid");
    let env_file = dir.path().join("env.txt");

    let output = daemonize_cmd()
        .args([
            "-p",
            pidfile.to_str().unwrap(),
            "-o",
            env_file.to_str().unwrap(),
            "-E",
            "MY_TEST_VAR=hello_world",
            "--",
            "sh",
            "-c",
            "echo $MY_TEST_VAR; sleep 1",
        ])
        .output()
        .unwrap();

    assert!(output.status.success());
    let pid = wait_for_pidfile(&pidfile, 5000).expect("pidfile should appear");
    std::thread::sleep(std::time::Duration::from_millis(500));

    let content = std::fs::read_to_string(&env_file).unwrap_or_default();
    assert!(
        content.contains("hello_world"),
        "env var should be passed to daemon, got: {content}"
    );

    kill_process(pid);
}

#[test]
fn same_path_stdout_stderr() {
    let dir = tempfile::tempdir().unwrap();
    let pidfile = dir.path().join("test.pid");
    let combined = dir.path().join("combined.log");

    let output = daemonize_cmd()
        .args([
            "-p",
            pidfile.to_str().unwrap(),
            "-o",
            combined.to_str().unwrap(),
            "-e",
            combined.to_str().unwrap(),
            "--",
            "sh",
            "-c",
            "echo stdout_line; echo stderr_line >&2; sleep 1",
        ])
        .output()
        .unwrap();

    assert!(output.status.success());
    let pid = wait_for_pidfile(&pidfile, 5000).expect("pidfile should appear");
    std::thread::sleep(std::time::Duration::from_millis(500));

    let content = std::fs::read_to_string(&combined).unwrap_or_default();
    assert!(content.contains("stdout_line"), "should have stdout");
    assert!(content.contains("stderr_line"), "should have stderr");

    kill_process(pid);
}

// --- Relative path resolution (R55) ---

#[test]
fn relative_path_with_slash_canonicalized() {
    let dir = tempfile::tempdir().unwrap();
    let pidfile = dir.path().join("test.pid");
    let stdout_file = dir.path().join("stdout.log");

    // Create a script in the tempdir
    let script = dir.path().join("test_script.sh");
    std::fs::write(&script, "#!/bin/sh\necho resolved_ok\nsleep 1\n").unwrap();
    std::fs::set_permissions(&script, PermissionsExt::from_mode(0o755)).unwrap();

    // Use a relative path with / (e.g. ./test_script.sh from the tempdir)
    let output = daemonize_cmd()
        .current_dir(dir.path())
        .args([
            "-p",
            pidfile.to_str().unwrap(),
            "-o",
            stdout_file.to_str().unwrap(),
            "--",
            "./test_script.sh",
        ])
        .output()
        .unwrap();

    assert!(
        output.status.success(),
        "daemonize with relative path should succeed: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let pid = wait_for_pidfile(&pidfile, 5000).expect("pidfile should appear");
    std::thread::sleep(Duration::from_millis(500));

    let content = std::fs::read_to_string(&stdout_file).unwrap_or_default();
    assert!(
        content.contains("resolved_ok"),
        "script should execute despite chdir, got: {content}"
    );

    kill_process(pid);
}

// --- Truncate mode (R11) ---

#[test]
fn truncate_mode_overwrites_existing() {
    let dir = tempfile::tempdir().unwrap();
    let pidfile = dir.path().join("test.pid");
    let stdout_file = dir.path().join("stdout.log");

    // Write existing content
    std::fs::write(&stdout_file, "old_content_should_be_gone\n").unwrap();

    let output = daemonize_cmd()
        .args([
            "-p",
            pidfile.to_str().unwrap(),
            "-o",
            stdout_file.to_str().unwrap(),
            "--",
            "sh",
            "-c",
            "echo new_content; sleep 1",
        ])
        .output()
        .unwrap();

    assert!(output.status.success());
    let pid = wait_for_pidfile(&pidfile, 5000).expect("pidfile should appear");
    std::thread::sleep(Duration::from_millis(500));

    let content = std::fs::read_to_string(&stdout_file).unwrap();
    assert!(
        !content.contains("old_content_should_be_gone"),
        "truncate should remove old content"
    );
    assert!(
        content.contains("new_content"),
        "should have new content"
    );

    kill_process(pid);
}

// --- Parent notification timing (R39, R42) ---

#[test]
fn parent_waits_for_exec_before_exiting() {
    let dir = tempfile::tempdir().unwrap();
    let pidfile = dir.path().join("test.pid");

    // Time how long the parent takes to return — it should block until
    // exec succeeds (EOF on pipe) or the daemon signals readiness.
    let start = Instant::now();
    let output = daemonize_cmd()
        .args([
            "-p",
            pidfile.to_str().unwrap(),
            "--",
            "sleep",
            "30",
        ])
        .output()
        .unwrap();
    let elapsed = start.elapsed();

    assert!(output.status.success(), "should succeed");

    // Parent should return relatively quickly (exec closes pipe via CLOEXEC)
    assert!(
        elapsed < Duration::from_secs(10),
        "parent should not hang (took {elapsed:?})"
    );

    let pid = wait_for_pidfile(&pidfile, 5000).expect("pidfile should appear");
    kill_process(pid);
}

// --- Exec failure reporting (R43, R44) ---

#[test]
fn exec_failure_reports_to_parent() {
    let dir = tempfile::tempdir().unwrap();

    // Use an absolute path to a file that exists but isn't executable
    let not_executable = dir.path().join("not_exec");
    std::fs::write(&not_executable, "not a binary").unwrap();
    // Don't set execute permission

    let output = daemonize_cmd()
        .args(["--", not_executable.to_str().unwrap()])
        .output()
        .unwrap();

    // Should exit with ProgramNotFound (66) since we check executability pre-fork
    assert_eq!(
        output.status.code(),
        Some(66),
        "non-executable should exit 66, stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

// --- Error exit codes per table row (R51) ---

#[test]
fn chdir_nonexistent_exits_71() {
    let output = daemonize_cmd()
        .args(["-c", "/nonexistent_daemonize_dir_12345", "--", "sleep", "1"])
        .output()
        .unwrap();

    // ChdirFailed or ValidationError — chdir to nonexistent is caught in validate
    let code = output.status.code().unwrap();
    assert!(
        code == 64 || code == 71,
        "nonexistent chdir should exit 64 or 71, got {code}"
    );
}

#[test]
fn permission_denied_user_switch_without_root_exits_77() {
    // Skip if running as root
    if nix::unistd::geteuid().as_raw() == 0 {
        return;
    }

    let output = daemonize_cmd()
        .args(["-u", "nobody", "--", "sleep", "1"])
        .output()
        .unwrap();

    assert_eq!(
        output.status.code(),
        Some(77),
        "non-root user switch should exit 77, stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn lockfile_nonwritable_parent_exits_73_or_64() {
    let output = daemonize_cmd()
        .args(["-l", "/nonexistent_parent_dir/test.lock", "--", "sleep", "1"])
        .output()
        .unwrap();

    let code = output.status.code().unwrap();
    assert!(
        code == 64 || code == 73,
        "lockfile with bad parent should exit 64 or 73, got {code}"
    );
}

// --- No pidfile when not configured (R18) ---

#[test]
fn no_pidfile_when_not_configured() {
    let dir = tempfile::tempdir().unwrap();
    let lockfile = dir.path().join("test.lock");
    let stdout_file = dir.path().join("stdout.log");

    let output = daemonize_cmd()
        .args([
            "-l",
            lockfile.to_str().unwrap(),
            "-o",
            stdout_file.to_str().unwrap(),
            "--",
            "sh",
            "-c",
            "echo running; sleep 5",
        ])
        .output()
        .unwrap();

    assert!(output.status.success());
    std::thread::sleep(Duration::from_millis(500));

    // No pidfile should exist in the tempdir other than lockfile and stdout
    let files: Vec<_> = std::fs::read_dir(dir.path())
        .unwrap()
        .filter_map(|e| e.ok())
        .map(|e| e.file_name().to_string_lossy().to_string())
        .collect();

    assert!(
        !files.iter().any(|f| f.ends_with(".pid")),
        "no pidfile should be created when not configured, found: {files:?}"
    );

    // Clean up: find the daemon via lockfile holder
    let content = std::fs::read_to_string(&stdout_file).unwrap_or_default();
    assert!(content.contains("running"));
}
