//! Closing inherited file descriptors (step 13).

use crate::error::DaemonizeError;
use crate::sys;

/// Convert an `rlim_cur` value to the exclusive upper bound of the fd-close
/// range, saturating at `i32::MAX`.
///
/// A plain `as i32` cast wraps huge limits — `RLIM_INFINITY` becomes `-1`,
/// emptying the close range so *no* fds are closed. Saturating is lossless in
/// practice: fds are C ints, so no open fd can exceed `i32::MAX`.
///
/// `rlim_t` is unsigned on Linux/macOS/NetBSD but signed (`i64`) on FreeBSD,
/// so the conversion goes through `try_from`: any value outside `i32` range —
/// including a negative one, which no kernel should report — maps to
/// `i32::MAX`, erring toward closing everything rather than nothing.
pub(crate) fn clamp_max_fd(rlim_cur: libc::rlim_t) -> i32 {
    i32::try_from(rlim_cur).unwrap_or(i32::MAX)
}

/// Query the process fd limit via getrlimit.
///
/// Returns [`SystemError`](DaemonizeError::SystemError) if `getrlimit` fails
/// (e.g. blocked by a seccomp filter) so the caller can report it.
pub(crate) fn get_max_fd() -> Result<i32, DaemonizeError> {
    #[cfg(test)]
    if crate::steps::failpoints::GETRLIMIT_FAILS.injected() {
        return Err(DaemonizeError::SystemError(
            "getrlimit(RLIMIT_NOFILE): injected failure".into(),
        ));
    }
    #[cfg(test)]
    if let Some(max_fd) = crate::steps::failpoints::injected_max_fd() {
        return Ok(max_fd);
    }
    let limit = nix::sys::resource::getrlimit(nix::sys::resource::Resource::RLIMIT_NOFILE)
        .map_err(|e| DaemonizeError::SystemError(format!("getrlimit(RLIMIT_NOFILE): {e}")))?;
    Ok(clamp_max_fd(limit.0))
}

/// Return an iterator of fd numbers to close, filtering out skip_fds.
///
/// Pure logic — no side effects.
pub(crate) fn fds_to_close(max_fd: i32, skip_fds: &[i32]) -> impl Iterator<Item = i32> + '_ {
    (3..max_fd).filter(move |fd| !skip_fds.contains(fd))
}

/// List this process's open fds from the fd directory, or `None` where no
/// reliable listing exists: the BSDs' `/dev/fd` exposes only 0-2 unless
/// fdescfs is mounted, and a minimal container may lack `/proc` even where the
/// platform normally has it (read failure also returns `None`).
///
/// The listing includes the fd `read_dir` itself uses; it is already closed
/// when the caller acts on the list, and re-closing is a harmless `EBADF`.
///
/// Safe post-fork: `daemonize` requires a single-threaded caller, so the
/// child's allocator lock cannot be held mid-operation by another thread.
#[cfg(blivet_fd_dir)]
pub(crate) fn list_open_fds() -> Option<Vec<i32>> {
    const FD_LIST_DIR: &str = env!("BLIVET_FD_DIR");

    #[cfg(test)]
    if crate::steps::failpoints::FD_LISTING_UNAVAILABLE.injected() {
        return None;
    }
    let entries = std::fs::read_dir(FD_LIST_DIR).ok()?;
    Some(
        entries
            .filter_map(|entry| entry.ok()?.file_name().to_str()?.parse().ok())
            .collect(),
    )
}

#[cfg(not(blivet_fd_dir))]
#[cfg_attr(test, mutants::skip)]
pub(crate) fn list_open_fds() -> Option<Vec<i32>> {
    None
}

/// Step 13: Close inherited file descriptors.
///
/// Closes the fds named by [`list_open_fds`] (minus 0-2 and the skip list),
/// falling back to iterating 3..rlim_cur where no listing is available.
/// The fallback matters for speed, not just portability: `RLIMIT_NOFILE` is
/// commonly raised to 1M+ (systemd `LimitNOFILE`) and `RLIM_INFINITY` clamps
/// to `i32::MAX`, turning the brute-force loop into billions of `close`
/// calls that stall daemon startup.
///
/// Returns [`SystemError`](DaemonizeError::SystemError) if the fallback path's
/// `getrlimit` fails; the fd-listing path is infallible.
pub(crate) fn close_inherited_fds(skip_fds: &[i32]) -> Result<(), DaemonizeError> {
    #[cfg(test)]
    crate::test_support::assert_isolated("close_inherited_fds");
    if let Some(open_fds) = list_open_fds() {
        for fd in open_fds {
            if fd >= 3 && !skip_fds.contains(&fd) {
                sys::raw_close(fd);
            }
        }
        return Ok(());
    }
    let max_fd = get_max_fd()?;
    for fd in fds_to_close(max_fd, skip_fds) {
        sys::raw_close(fd);
    }
    Ok(())
}

#[cfg(test)]
mod tests {

    use std::os::fd::AsRawFd;

    use super::*;

    // Covers: R103
    #[test]
    fn clamp_max_fd_saturates_instead_of_wrapping() {
        // RLIM_INFINITY formerly wrapped to -1 via `as i32`, emptying the
        // close range so no fds were closed at all. The conversion must
        // saturate: closing up to i32::MAX covers every possible fd (fds are
        // C ints). rlim_t::MAX covers both the unsigned (u64::MAX, which IS
        // RLIM_INFINITY on Linux) and signed-FreeBSD (i64::MAX) cases.
        assert_eq!(clamp_max_fd(libc::rlim_t::MAX), i32::MAX);
        assert_eq!(clamp_max_fd(i32::MAX as libc::rlim_t + 1), i32::MAX);
        // Ordinary limits pass through unchanged.
        assert_eq!(clamp_max_fd(1024), 1024);
        assert_eq!(clamp_max_fd(0), 0);
    }

    // Covers: R104
    #[test]
    fn fds_to_close_skips_correctly() {
        let result: Vec<i32> = fds_to_close(10, &[4, 7]).collect();
        assert_eq!(result, vec![3, 5, 6, 8, 9]);
    }

    #[test]
    fn fds_to_close_empty_skip() {
        let result: Vec<i32> = fds_to_close(6, &[]).collect();
        assert_eq!(result, vec![3, 4, 5]);
    }

    #[test]
    fn fds_to_close_all_skipped() {
        let result: Vec<i32> = fds_to_close(6, &[3, 4, 5]).collect();
        assert!(result.is_empty());
    }

    #[test]
    fn fds_to_close_max_below_3() {
        let result: Vec<i32> = fds_to_close(2, &[]).collect();
        assert!(result.is_empty());
    }

    // Covers: R103
    #[test]
    fn get_max_fd_reflects_a_real_limit() {
        // 0-2 always exist, so any true fd limit is at least 3. Guards the
        // fallback close range against collapsing (0/1) or inverting (-1).
        assert!(get_max_fd().unwrap() >= 3);
    }

    // Covers: R135
    #[test]
    #[cfg(blivet_fd_dir)]
    fn list_open_fds_sees_an_open_fd() {
        use std::os::fd::AsRawFd;
        let file = tempfile::tempfile().unwrap();
        let listed = list_open_fds().expect("fd listing is available on this platform");
        assert!(
            listed.contains(&file.as_raw_fd()),
            "an open fd must appear in the listing"
        );
        // No assertion that a *closed* fd disappears: the listing itself
        // opens the fd directory, which can reuse the just-freed number.
    }

    /// Runs everywhere, the hosted Ubuntu runner included, so mutation testing
    /// reaches step 13 in CI. It was once ignored there after an abort blamed
    /// on systemd's `safe_close()` (7dc982b, eb50694); a hosted run with the
    /// ignore removed passed, so the ignore went. If that abort comes back,
    /// the ignore is not the fix: find what in the test process owns the fd.
    /// `close_inherited_fds_without_a_listing_preserves_skipped` forces the
    /// brute-force branch wherever it runs, so no tier is needed for that.
    // Covers: R103, R104
    #[test]
    fn close_inherited_fds_preserves_skipped() {
        crate::test_support::run_in_subprocess(
            "steps::fds::tests::close_inherited_fds_preserves_skipped_subprocess",
        );
    }

    #[test]
    #[ignore = "closes fds process-wide; only safe in an isolated subprocess"]
    fn close_inherited_fds_preserves_skipped_subprocess() {
        // Guard so this never runs as a stray `--include-ignored` in the shared
        // process; it executes only when spawned by run_in_subprocess.
        if !crate::test_support::is_subprocess() {
            return;
        }
        assert_closes_all_but_skipped();
    }

    /// The same property, proven on the brute-force `3..rlim_cur` fallback.
    ///
    /// That branch is what every platform without a trustworthy fd directory
    /// takes at runtime — the BSDs, and every best-effort Unix — and the only
    /// other test that reaches it fails `getrlimit` as well, so it returns
    /// before closing anything. Forcing the listing to be unavailable runs the
    /// fallback on whichever tier runs this test, rather than waiting for a
    /// tier that lacks the directory.
    // Covers: R103, R104, R135
    #[test]
    fn close_inherited_fds_without_a_listing_preserves_skipped() {
        crate::test_support::run_in_subprocess(
            "steps::fds::tests::close_inherited_fds_without_a_listing_preserves_skipped_subprocess",
        );
    }

    #[test]
    #[ignore = "closes fds process-wide; only safe in an isolated subprocess"]
    fn close_inherited_fds_without_a_listing_preserves_skipped_subprocess() {
        if !crate::test_support::is_subprocess() {
            return;
        }
        // Process-global, which is why this runs isolated. Where the platform
        // has no fd directory the flag changes nothing and the fallback was
        // already the only branch.
        crate::steps::failpoints::FD_LISTING_UNAVAILABLE.set();
        // Bounded, so the loop is the branch under test rather than the host's
        // fd limit. Comfortably above the handful of descriptors a test process
        // holds, and far below what a raised RLIMIT_NOFILE would make it walk.
        crate::steps::failpoints::set_max_fd(4096);
        assert_closes_all_but_skipped();
    }

    /// Closes every fd it was not told to keep, and keeps the ones it was.
    ///
    /// Closes fds process-wide, so only an isolated subprocess may call it.
    fn assert_closes_all_but_skipped() {
        let (rd, wr) = crate::test_support::make_pipe();
        // A second pipe deliberately left out of the skip list: it must be
        // closed, or the step silently no-oped (a mutation sweep caught the
        // original test asserting only preservation, never closure).
        let (victim_rd, victim_wr) = crate::test_support::make_pipe();
        drop(victim_rd);
        close_inherited_fds(&[rd.as_raw_fd(), wr.as_raw_fd()]).unwrap();
        // Our pipe fds should still be open
        assert!(nix::unistd::write(&wr, b"ok").is_ok());
        // The non-skipped fd must be gone.
        assert_eq!(
            nix::unistd::write(&victim_wr, b"x"),
            Err(nix::errno::Errno::EBADF),
            "a non-skipped fd must be closed"
        );
        // close_inherited_fds already closed it; don't double-close on drop.
        std::mem::forget(victim_wr);
    }
}
