% DAEMONIZE(1) daemonize 0.1.0
%
% April 2026

# NAME

daemonize - run a program as a Unix daemon

# SYNOPSIS

**daemonize** [*OPTIONS*] **--** *program* [*args*...]

# DESCRIPTION

**daemonize** runs *program* as a Unix daemon, detaching it from the controlling
terminal via the standard double-fork method. The parent process waits for
the daemon to start successfully before exiting, so **daemonize** can be used
reliably in init scripts and process supervisors.

The daemonization sequence:

1. Creates a notification pipe, then forks.
2. Calls **setsid**(2) to create a new session.
3. Forks a second time so the daemon cannot reacquire a controlling terminal.
4. Sets the umask, changes the working directory, and redirects
   stdin/stdout/stderr to */dev/null*.
5. Writes the PID file and acquires the lock file (if configured).
6. Resets all signal dispositions to **SIG_DFL** and clears the signal mask.
7. Applies environment variables.
8. Transfers ownership of pidfile, lockfile, and output files to the target
   user/group (if configured) via **chown**(2).
9. Switches user and group (if configured) via **setuid**(2), **setgid**(2),
   and **initgroups**(3).
10. Redirects stdout/stderr to files (if configured).
11. Closes all inherited file descriptors (except the lock file), unless
    **--no-close-fds** is given.
12. Exec's *program*.

In foreground mode (**-f**), steps 1-3 are skipped: no fork or setsid occurs,
and the notification pipe is not created. All other steps still apply.

The parent exits 0 only after the daemon has successfully exec'd *program*.
If any step fails, the parent exits with a non-zero status and prints a
diagnostic to stderr.

When **-u** or **-g** is specified, ownership of the PID file, lock file, and
output files is transferred to the target user/group before privileges are
dropped, so the daemon can continue to write to them after the switch.

# OPTIONS

**-p**, **--pidfile** *path*
:   Write the daemon's PID to *path*. The path must be absolute.
    When **--lock** is not specified, the pidfile is also used as the
    lock file, providing single-instance enforcement by default.

**-c**, **--chdir** *path*
:   Change the daemon's working directory to *path* (default: **/**).
    The path must be absolute and refer to an existing directory.

**-m**, **--umask** *mode*
:   Set the daemon's file creation mask to *mode* (octal, e.g. **022**).
    Default: **000**.

**-o**, **--stdout** *path*
:   Redirect the daemon's stdout to *path*. The path must be absolute.
    When **--stderr** is not specified, stderr is also redirected: if *path*
    ends in **.stdout**, stderr goes to the same name with a **.stderr**
    extension; otherwise both streams share the same file (opened once,
    shared via **dup2**(2)).

**-e**, **--stderr** *path*
:   Redirect the daemon's stderr to *path*. The path must be absolute.
    Defaults to the **--stdout** path (with **.stdout**→**.stderr** extension
    swap) when not specified.

**-a**, **--append**
:   Open stdout/stderr files in append mode instead of truncating them.

**-l**, **--lock** *path*
:   Acquire an exclusive lock on *path* using **flock**(2). Prevents
    multiple instances of the same daemon from running simultaneously.
    The lock file is held for the lifetime of the daemon process and
    survives across **exec**(3). The path must be absolute. Defaults to
    the **--pidfile** path when not specified, so a pidfile alone is
    sufficient for single-instance enforcement. Use **--lock** with a
    different path to separate the lock file from the PID file.

**-E**, **--env** *name*=*value*
:   Set environment variable *name* to *value* in the daemon. May be
    specified multiple times. If *value* is omitted (no **=**), the
    variable is set to an empty string.

**-u**, **--user** *name*|*uid*
:   Run the daemon as *name* (or numeric *uid*). Calls **setuid**(2),
    **setgid**(2), and **initgroups**(3), and sets the **USER**, **HOME**,
    and **LOGNAME** environment variables. If a numeric string is given, it
    is treated as a UID. When **--group** is not specified, the user's
    primary group is used. Requires root privileges.

**-g**, **--group** *name*|*gid*
:   Run the daemon as group *name* (or numeric *gid*). Calls **setgid**(2)
    to set the effective group. If a numeric string is given, it is treated
    as a GID. May be combined with **-u** to set user and group independently.
    Requires root privileges.

**-f**, **--foreground**
:   Stay in the foreground instead of daemonizing. Skips the double-fork
    and **setsid**(2), but still applies all other setup steps (umask, chdir,
    signal reset, etc.). Useful for systemd, containers, and debugging.
    Consider combining with **--no-close-fds** to preserve supervisor-passed
    file descriptors.

**--no-close-fds**
:   Keep inherited file descriptors (3 and above) open. By default, all
    inherited descriptors except the lock file are closed. Useful with
    **-f** to preserve supervisor-passed file descriptors.

**-v**, **--verbose**
:   Print diagnostic information to stderr before daemonizing.

**-h**, **--help**
:   Print a help message and exit.

**-V**, **--version**
:   Print version information and exit.

# EXIT STATUS

Exit codes follow the **sysexits.h** conventions:

**0**
:   Daemon started successfully.

**64** (EX_USAGE)
:   Configuration or validation error.

**66** (EX_NOINPUT)
:   Program not found or not executable.

**67** (EX_NOUSER)
:   User or group not found.

**69** (EX_UNAVAILABLE)
:   Lock file held by another process.

**71** (EX_OSERR)
:   OS error: fork, setsid, chdir, or exec failed.

**73** (EX_CANTCREAT)
:   Cannot create lock file, PID file, or output file; or **chown**(2)
    of those files failed.

**77** (EX_NOPERM)
:   Permission denied for user or group switch.

# EXAMPLES

Run a program as a daemon with a PID file:

    daemonize -p /var/run/myapp.pid -- /usr/bin/myapp --config /etc/myapp.conf

Redirect output and run as a specific user (stderr mirrors stdout):

    daemonize -p /var/run/myapp.pid \
              -o /var/log/myapp/output.log \
              -a \
              -u myapp \
              -- /usr/bin/myapp

Split stdout and stderr using extension convention:

    daemonize -p /var/run/myapp.pid \
              -o /var/log/myapp/app.stdout \
              -- /usr/bin/myapp  # stderr goes to app.stderr

Prevent duplicate instances (pidfile acts as lock file by default):

    daemonize -p /var/run/myapp.pid -- /usr/bin/myapp

Use a separate lock file:

    daemonize -p /var/run/myapp.pid \
              -l /var/run/myapp.lock \
              -- /usr/bin/myapp

Run as a specific user and group:

    daemonize -u www-data -g www-data \
              -p /var/run/myapp.pid \
              -- /usr/bin/myapp

Run in foreground mode (useful for systemd or containers):

    daemonize --foreground --no-close-fds -p /var/run/myapp.pid -- /usr/bin/myapp

Set environment variables and working directory:

    daemonize -c /srv/myapp \
              -E DATABASE_URL=postgres://localhost/myapp \
              -E RUST_LOG=info \
              -- /usr/bin/myapp

# SEE ALSO

**daemon**(3), **fork**(2), **setsid**(2), **flock**(2), **sysexits**(3)
