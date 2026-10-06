# 0006. The notification channel cannot raise SIGPIPE

- Status: Accepted
- Date: 2026-10-06
- Related: [0001](0001-capability-based-platform-gating.md)

## Context

The daemon tells the launcher how the start went by writing to a pipe: the
ready message, the exec marker, or an error. The launcher can be gone by
then: killed, or ended with its terminal. A write to a pipe with no reader
raises SIGPIPE, and the write fails with `EPIPE` only if SIGPIPE is ignored.

Daemonizing keeps the caller's SIGPIPE disposition (R127). The Rust runtime
ignores SIGPIPE, so most callers arrive with `SIG_IGN`. A caller can set
`SIG_DFL` with `-Zon-broken-pipe=kill`, or with a crate that resets SIGPIPE
so `println!` cannot panic on a closed pipe. That caller arrives with the
default, and a write to a gone launcher kills the daemon:

- `exec` never runs the program, and the pidfile stays behind. R157 breaks.
- An error report dies by signal before it prints. R161 breaks.
- `notify_parent` kills a daemon that was about to start serving, though its
  rustdoc promises `NotifyFailed`.

A fix that ignores SIGPIPE before each write works for the first two, which
end in `execvp` or exit. It does not work for `notify_parent`, where the
daemon keeps running with the caller's disposition. There are two other
ways to suppress the signal around one write. Both are wrong for a library
that must run on high-assurance systems:

- Save the disposition, ignore SIGPIPE, write, and restore. The disposition
  is process-wide. Another thread that installs a SIGPIPE handler between
  the save and the restore loses it without any report, and no test can
  catch that race reliably.
- Block SIGPIPE in the calling thread, write, consume the pending signal,
  and unblock. This changes no process-wide state. But macOS has no
  `sigtimedwait`, and consuming the signal with `sigpending` and `sigwait`
  can swallow a SIGPIPE that someone sent to the whole process with
  `kill -PIPE` during the write.

## Decision

The notification channel is a connected socket pair,
`socketpair(AF_UNIX, SOCK_STREAM)`, not a pipe. The daemon writes to it only
with `send(..., MSG_NOSIGNAL)`. A send to a gone launcher then fails with
`EPIPE` and raises no signal, whatever the caller's disposition. Nothing
touches a signal disposition or a signal mask to get there.

- One function sends on the channel, and every message goes through it: the
  ready message, the exec marker, error reports, and the drop-time report.
  Nothing else writes to the daemon's end.
- Both ends are close-on-exec, as the pipe's are today. macOS has
  no `SOCK_CLOEXEC`, so there `RealForker` sets the flag right after
  creation, as it does for the pipe now.
- The channel is one-way. The daemon calls `shutdown(SHUT_RD)` on its
  end, and the launcher calls `shutdown(SHUT_WR)` on its end.
- The launcher reads with `read`, as before. The bytes on the wire (R160),
  EOF after a successful exec, and the effect of a forked process holding a
  copy of the daemon's end do not change.

`MSG_NOSIGNAL` is in POSIX.1-2008. `libc` defines it for every platform in
the capability table and for illumos, the best-effort stand-in. The table
lists Linux, Android, macOS, FreeBSD, NetBSD, and OpenBSD. Of the targets
`nix` supports, only Redox lacks it, and blivet does not build for Redox
today. Redox has no `flock`, `initgroups`, or `faccessat`. So the crate uses
`MSG_NOSIGNAL` directly, with no capability alias and no fallback path. A future target without it
fails to compile, which names the problem at once.

## Consequences

- C1's `ignore_sigpipe` calls before the exec marker and in `signal_error`
  go away. So does the `ignore_sigpipe` after a failed `execvp`: the error
  report that follows sends with `MSG_NOSIGNAL`. R128 stays. `exec` still
  sets SIGPIPE to `SIG_DFL` just before `execvp`, so the program starts with
  the default disposition.
- `notify_parent` returns `NotifyFailed` when the launcher is gone, for every
  caller. R127 then holds as written: the daemon runs with the caller's
  disposition until `exec` hands the process to another program.
- R157 and R161 drop their SIGPIPE clauses. The SPEC keeps the name
  "notification pipe" for the channel, with one sentence saying it is a
  socket pair. Renaming it across every requirement would churn the text
  without changing a rule.
- The `make_pipe` test helper returns a socket pair. The test that makes the
  marker write fail with an error other than `EPIPE` can no longer use the
  read end, because a socket pair's ends are both writable before the
  shutdown. It sends on a descriptor that is not a socket, which fails with
  `ENOTSOCK`.
- A new test sets `SIG_DFL`, closes the launcher's end, and calls
  `notify_parent`. It passes only if the daemon gets `NotifyFailed` back.
- The README's best-effort tier promises that "Other Unix targets that `nix`
  supports" compile. Redox shows that is not true, before and after this
  change. That claim needs its own fix.
