//! Probes that check each capability claim against the running system.
//!
//! The table in `build.rs` is a claim about the world, and claims rot. A probe
//! asks the system directly and asserts the answer matches the compile-time
//! alias, so the day a platform gains or loses a capability is the day a test
//! goes red rather than the day a user hits a wrong branch.
//!
//! Both directions matter, and the negative one matters more: claiming a
//! capability the platform lacks fails loudly at the first call, while denying
//! one it has is silent — it is exactly how Android ended up taking the
//! unknown-Unix fallback of every decision despite having `/proc`.
//!
//! Two claims have no negative probe, and the gap is deliberate rather than
//! overlooked:
//!
//! - `blivet_thread_count`: where it is absent there is no `count` function to
//!   call, and no portable way to ask "could a count have been read here?".
//! - `blivet_rt_signals_reserved`: `libc` exposes `SIGRTMIN` only on the
//!   platforms that have a real-time range at all, so the absent case cannot
//!   name the thing it would measure.

/// A file we can hold open, to give the fd-directory probes something to find.
fn open_fd() -> (std::fs::File, i32) {
    use std::os::fd::AsRawFd;
    let file = tempfile::tempfile().expect("a temp file");
    let fd = file.as_raw_fd();
    (file, fd)
}

/// Does the directory at `path` list `fd`, this process's own open descriptor?
///
/// This is the question `blivet_fd_dir` answers. The BSDs' `/dev/fd` exists but
/// shows only 0-2 unless fdescfs is mounted, so mere existence proves nothing —
/// the listing has to contain a descriptor we know we opened.
fn lists_own_fd(path: &str, fd: i32) -> bool {
    let Ok(entries) = std::fs::read_dir(path) else {
        return false;
    };
    entries
        .filter_map(|e| e.ok()?.file_name().to_str()?.parse::<i32>().ok())
        .any(|listed| listed == fd)
}

/// `blivet_thread_count` claims the live thread count is readable. Read it.
#[cfg(blivet_thread_count)]
// Covers: R139, R140
#[test]
fn thread_count_is_readable() {
    let count = crate::thread_count::count().expect("the thread count must be readable");
    assert!(
        count >= 1,
        "a running process has at least the calling thread, got {count}"
    );
}

/// `blivet_thread_count_procfs` claims the count is in `/proc/self/status`.
#[cfg(blivet_thread_count_procfs)]
// Covers: R139, R140
#[test]
fn proc_status_reports_threads() {
    let status = std::fs::read_to_string("/proc/self/status").expect("/proc/self/status");
    assert!(
        status.lines().any(|l| l.starts_with("Threads:")),
        "/proc/self/status must carry the Threads: line the count is parsed from"
    );
}

/// The absence of `blivet_thread_count_procfs` claims there is no such line to
/// read. A platform that grows one — or that had one all along — shows up here.
#[cfg(not(blivet_thread_count_procfs))]
// Covers: R139, R140
#[test]
fn proc_status_reports_no_threads() {
    let Ok(status) = std::fs::read_to_string("/proc/self/status") else {
        return; // No /proc/self/status at all, which is the common case.
    };
    assert!(
        !status.lines().any(|l| l.starts_with("Threads:")),
        "/proc/self/status carries a Threads: line, so this platform can read \
         the thread count from procfs — set thread_count_procfs in build.rs"
    );
}

/// `blivet_fd_dir` claims the named directory lists this process's open fds.
#[cfg(blivet_fd_dir)]
// Covers: R139, R140
#[test]
fn fd_dir_lists_open_fds() {
    let (_file, fd) = open_fd();
    let dir = env!("BLIVET_FD_DIR");
    assert!(
        lists_own_fd(dir, fd),
        "{dir} must list fd {fd}, which this process holds open"
    );
}

/// The absence of `blivet_fd_dir` claims no directory gives a trustworthy
/// listing. A platform where one does is falling back to the `3..rlim_cur`
/// close loop for nothing.
#[cfg(not(blivet_fd_dir))]
// Covers: R139, R140
#[test]
fn no_fd_dir_lists_open_fds() {
    // Every directory that could plausibly be one on a Unix: the claim is that
    // none of them works here, so each has to be asked.
    const CANDIDATES: &[&str] = &["/proc/self/fd", "/dev/fd"];

    let (_file, fd) = open_fd();
    for dir in CANDIDATES {
        assert!(
            !lists_own_fd(dir, fd),
            "{dir} lists fd {fd}, so this platform has a usable fd directory — \
             set fd_dir in build.rs"
        );
    }
}

/// `blivet_rt_signals_reserved` claims libc holds back signals between the
/// standard range and the real-time range, for POSIX timers and the like.
/// Those are the signals a sweep must skip rather than reset.
#[cfg(blivet_rt_signals_reserved)]
// Covers: R139, R140
#[test]
fn rt_signal_range_starts_above_the_standard_signals() {
    let rtmin = libc::SIGRTMIN();
    assert!(
        rtmin > 32,
        "libc reserves nothing above the 31 standard signals (SIGRTMIN is \
         {rtmin}), so there is no range to skip — clear rt_signals_reserved \
         in build.rs"
    );
    assert!(
        rtmin <= libc::SIGRTMAX(),
        "an empty real-time range leaves the signal sweep with nothing to \
         iterate: SIGRTMIN {rtmin} > SIGRTMAX {}",
        libc::SIGRTMAX()
    );
}

/// `blivet_faccessat_eaccess` claims `faccessat` accepts `AT_EACCESS`, so the
/// writability probe answers for the effective UID.
#[cfg(blivet_faccessat_eaccess)]
// Covers: R139, R140
#[test]
fn faccessat_accepts_at_eaccess() {
    let dir = tempfile::tempdir().expect("a temp dir");
    nix::unistd::faccessat(
        crate::unsafe_ops::at_fdcwd(),
        dir.path(),
        nix::unistd::AccessFlags::W_OK,
        nix::fcntl::AtFlags::AT_EACCESS,
    )
    .expect("a temp dir is writable, and AT_EACCESS must not be rejected");
}

/// The absence of `blivet_faccessat_eaccess` claims the flag is rejected. It
/// has to be asked for by value: `nix` does not define the constant where the
/// platform lacks it, which is the compile error that started all this.
#[cfg(not(blivet_faccessat_eaccess))]
// Covers: R139, R140
#[test]
fn faccessat_rejects_at_eaccess() {
    // 0x200 is AT_EACCESS wherever it is defined; bionic's faccessat accepts
    // only AT_SYMLINK_NOFOLLOW and returns EINVAL for anything else.
    const AT_EACCESS: nix::fcntl::AtFlags = nix::fcntl::AtFlags::from_bits_retain(0x200);

    let dir = tempfile::tempdir().expect("a temp dir");
    let err = nix::unistd::faccessat(
        crate::unsafe_ops::at_fdcwd(),
        dir.path(),
        nix::unistd::AccessFlags::W_OK,
        AT_EACCESS,
    )
    .expect_err("AT_EACCESS must be rejected where the table says it is");
    assert_eq!(
        err,
        nix::errno::Errno::EINVAL,
        "an effective-UID check that fails for any other reason is a different \
         bug than the one this claim is about"
    );
}
