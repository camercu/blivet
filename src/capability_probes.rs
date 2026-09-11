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
//! The negative probes run only on a platform the table lists. For any other
//! target the absent capability is a default rather than a measured fact, and
//! asserting it would fail on a Unix that simply has not been added yet —
//! illumos has a working `/proc/self/fd` — telling whoever ran the suite that
//! the crate is broken when it is merely unlisted.
//!
//! Three claims have no negative probe, and each gap is deliberate rather than
//! overlooked:
//!
//! - `blivet_thread_count`: where it is absent there is no `count` function to
//!   call, and no portable way to ask "could a count have been read here?".
//! - `blivet_fd_dir`: every listed platform answering "no" is a BSD, where
//!   both candidate directories are optional mounts — `procfs` on NetBSD,
//!   `fdescfs` on `/dev/fd` — so finding one populated says the machine
//!   running the test mounted it, not that the platform offers a listing the
//!   crate could rely on. The table's "no" already means "not something to
//!   depend on", and no runtime observation separates that from "not mounted
//!   here", so the assertion would fail on a NetBSD with its default
//!   `/etc/fstab` while saying nothing true.
//! - `blivet_rt_signals_reserved`: among the platforms the table lists — the
//!   only ones a negative probe runs on — every one answering "no" is a macOS
//!   or BSD target for which `libc` defines no `SIGRTMIN`, so the absent case
//!   cannot name the thing it would measure. This is a fact about `libc`'s
//!   coverage, not about the platforms: `libc` does define `SIGRTMIN` for the
//!   solarish targets, which have a real-time range and are not listed here.
//!   Adding one of them to the table means writing this probe.

/// The highest descriptor number a directory of *static* device nodes is
/// likely to cover. NetBSD's `MAKEDEV` creates `/dev/fd/0` through
/// `/dev/fd/63` as plain character devices that exist whether or not anything
/// is open on them, so a listing containing a low number proves nothing.
// Only the fd-directory probe uses these, and it exists only where the
// capability does. Anything wider leaves them uncalled on a platform without
// it, which the `-D warnings` this project sets turns into a build failure.
#[cfg(blivet_fd_dir)]
const STATIC_FD_RANGE_TOP: i32 = 63;

/// Hold descriptors open until one lands above [`STATIC_FD_RANGE_TOP`], and
/// return them with that descriptor's number.
///
/// The files stay open in the returned `Vec` — dropping it closes them, so a
/// caller must keep it alive while asking whether the descriptor is listed.
// Only the fd-directory probe uses these, and it exists only where the
// capability does. Anything wider leaves them uncalled on a platform without
// it, which the `-D warnings` this project sets turns into a build failure.
#[cfg(blivet_fd_dir)]
fn open_fd_above_static_range() -> (Vec<std::fs::File>, i32) {
    use std::os::fd::AsRawFd;

    let mut held = Vec::new();
    loop {
        let file = tempfile::tempfile().expect("a temp file");
        let fd = file.as_raw_fd();
        held.push(file);
        if fd > STATIC_FD_RANGE_TOP {
            return (held, fd);
        }
        assert!(
            held.len() < 512,
            "opened {} files without reaching a descriptor above {}",
            held.len(),
            STATIC_FD_RANGE_TOP
        );
    }
}

/// Does the directory at `path` list `fd`, this process's own open descriptor?
///
/// This is the question `blivet_fd_dir` answers. Existence proves nothing: the
/// BSDs' `/dev/fd` shows only 0-2 unless fdescfs is mounted, and NetBSD
/// populates it with static nodes for 0-63. So the descriptor asked about is
/// one from [`open_fd_above_static_range`] — a listing that has it is tracking
/// this process, not enumerating device nodes.
// Only the fd-directory probe uses these, and it exists only where the
// capability does. Anything wider leaves them uncalled on a platform without
// it, which the `-D warnings` this project sets turns into a build failure.
#[cfg(blivet_fd_dir)]
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
#[cfg(blivet_known_platform)]
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
    let (_held, fd) = open_fd_above_static_range();
    let dir = env!("BLIVET_FD_DIR");
    assert!(
        lists_own_fd(dir, fd),
        "{dir} must list fd {fd}, which this process holds open"
    );
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

/// Without `blivet_faccessat_lacks_eaccess` the platform accepts `AT_EACCESS`,
/// so the writability probe answers for the effective UID. That is the table's
/// default, which `validate` acts on every call, so this runs on an unlisted
/// target too: a Unix that rejects the flag has a broken writability probe and
/// a red test is the right answer.
#[cfg(not(blivet_faccessat_lacks_eaccess))]
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

/// `blivet_faccessat_lacks_eaccess` claims the flag is rejected. It has to be
/// asked for by value: `nix` does not define the constant where the platform
/// lacks it, which is the compile error that started all this.
///
/// Set only from a table row, so this is already a measured fact rather than a
/// default and needs no `blivet_known_platform` beside it.
#[cfg(blivet_faccessat_lacks_eaccess)]
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
