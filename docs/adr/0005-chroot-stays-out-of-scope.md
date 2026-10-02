# 0005. Chroot stays out of scope; library-owned chroot is deferred

- Status: Accepted (proceed with the narrow fix); library-owned chroot
  deferred
- Date: 2026-10-01
- Related: [0004](0004-privileged-init-runs-inside-daemonize.md)

## Context

ADR 0004 runs the caller's privileged `init` inside the daemonize call,
then chowns the pidfile, lockfile, and logs and switches user. The README
offered `chroot` as something to do in `init`. A review found four ways a
`chroot` there goes wrong, because steps after `init` assume the old root:

1. The chown found the files by path, so after a `chroot` it looked inside
   the jail: it skipped them, or chowned a same-named file there.
2. `initgroups` read the jail's `/etc/group`, so the daemon got the jail's
   supplementary groups, or none.
3. The working directory stayed outside the jail (step 5 had already run
   `chdir`), the classic way out of a chroot.
4. Pidfile cleanup (`cleanup()`, `Drop`, the signal handler) removes the
   pidfile by path, which after a `chroot` names a file inside the jail.

`docs/SPEC.md` lists "Chroot support" under Out of scope.

## Decision

Chroot stays out of scope. Fix what affects every user switch, chroot or
not:

- Chown the descriptors the sequence already holds (`fchown`), not paths.
  This fixes trap 1, and chowns exactly the file that was opened: a path
  swapped between the open and the chown no longer redirects it. A symlink
  already planted when the file is opened still redirects the open itself,
  so the caveat that these paths belong in directories untrusted users
  cannot write stays.
- Call `initgroups` before `init` runs (the identity is resolved there
  already), so the supplementary groups come from the real group database.
  This fixes trap 2 with no new FFI. `init` then runs as root with the
  target user's supplementary groups, which root's permission checks
  ignore.
- Stop suggesting `chroot` in `init` in the README and rustdoc, and say
  that a caller who does it owns traps 3 and 4.

## Deferred: library-owned chroot

A `DaemonConfig::chroot(path)` setter was built as a spike (branch
`spike/chroot-option`, local) to price it. The sequence resolved the
identity and supplementary groups before `init`, ran `init` outside the
jail, chowned held descriptors, then did `chroot`, `chdir` inside the jail,
and the user switch. Pidfile removal kept a descriptor of the pidfile's
directory from before the `chroot` and used `unlinkat`. Run as root in the
Docker tier, it met every check: root and working directory inside the
jail, files owned by the target user with same-named decoys in the jail
untouched, groups from the real database, `init` outside the jail, CLI
`--chroot` running a program that exists only in the jail.

What it costs, largest first:

1. **The kept directory descriptor is a way out of the jail.** As
   `nobody` inside the jail, `fchdir` on it then `..` reached a host-only
   file. Any code-execution bug in the daemon gets the host filesystem.
   Every design that removes the pidfile after the `chroot` needs some
   handle from outside.
2. That removal fails anyway when the pidfile's directory is root-owned
   (`/run`): the daemon, now unprivileged, gets `EACCES`. Removal by path
   has the same limit today.
3. `validate()` cannot see inside the jail: a jail symlink to a host path
   passes validation and fails after the fork (exit 71). `HOME` is set to
   the host passwd entry's directory, which the jail may lack.
4. The CLI cannot check the program before the fork under `--chroot`, and
   the jail needs the program's interpreter and libraries.
5. In foreground mode `chroot` jails the caller's own process, and an
   error after it returns to a caller already inside the jail.
6. The signal handler's state grows (a duplicated descriptor per install),
   and the SPEC's Out of scope entry reverses.

Cheap parts: the `chroot` step is about 8 lines, and running `init` outside
the jail is only ordering.

A cheaper variant drops the directory descriptor: the library does
`chroot` and `chdir` (closing trap 3) and skips pidfile removal after it,
leaving a stale pidfile. The lock is released at exit, so a stale pidfile
does not block the next start, but tools that read it see a dead PID.

Revisit when a user asks for chroot support, or when a way to remove the
pidfile after the `chroot` without an outside handle turns up (for
example, a supervisor that owns the pidfile). Start from the spike branch
and this record; build fresh against `main`.

## Consequences

- `chroot` in `init` remains possible but unsupported: the caller does the
  `chdir` into the jail and accepts a pidfile that cleanup cannot remove.
- The chown follows held descriptors: it changes the file the sequence
  opened, whatever its path names by then.
- `init` runs with the target user's supplementary groups.
