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
7. Applies environment variables and switches user (if configured).
8. Redirects stdout/stderr to files (if configured), after the user switch
   so files have correct ownership.
9. Closes all inherited file descriptors (except the lock file).
10. Exec's *program*.

The parent exits 0 only after the daemon has successfully exec'd *program*.
If any step fails, the parent exits with a non-zero status and prints a
diagnostic to stderr.

# OPTIONS

**-p**, **--pidfile** *path*
:   Write the daemon's PID to *path*. The path must be absolute.

**-c**, **--chdir** *path*
:   Change the daemon's working directory to *path* (default: **/**).
    The path must be absolute and refer to an existing directory.

**-m**, **--umask** *mode*
:   Set the daemon's file creation mask to *mode* (octal, e.g. **022**).
    Default: **000**.

**-o**, **--stdout** *path*
:   Redirect the daemon's stdout to *path*. The path must be absolute.
    If both **-o** and **-e** refer to the same file, the file is opened once
    and shared between both streams.

**-e**, **--stderr** *path*
:   Redirect the daemon's stderr to *path*. The path must be absolute.

**-a**, **--append**
:   Open stdout/stderr files in append mode instead of truncating them.

**-l**, **--lock** *path*
:   Acquire an exclusive lock on *path* using **flock**(2). Prevents
    multiple instances of the same daemon from running simultaneously.
    The lock file is held for the lifetime of the daemon process and
    survives across **exec**(3). The path must be absolute.

**-E**, **--env** *name*=*value*
:   Set environment variable *name* to *value* in the daemon. May be
    specified multiple times. If *value* is omitted (no **=**), the
    variable is set to an empty string.

**-u**, **--user** *username*
:   Run the daemon as *username*. Calls **setuid**(2), **setgid**(2), and
    **initgroups**(3), and sets the **USER**, **HOME**, and **LOGNAME**
    environment variables. Requires root privileges.

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
:   User not found.

**69** (EX_UNAVAILABLE)
:   Lock file held by another process.

**71** (EX_OSERR)
:   OS error: fork, setsid, chdir, or exec failed.

**73** (EX_CANTCREAT)
:   Cannot create lock file, PID file, or output file.

**77** (EX_NOPERM)
:   Permission denied for user switch.

# EXAMPLES

Run a program as a daemon with a PID file:

    daemonize -p /var/run/myapp.pid -- /usr/bin/myapp --config /etc/myapp.conf

Redirect output and run as a specific user:

    daemonize -p /var/run/myapp.pid \
              -o /var/log/myapp/stdout.log \
              -e /var/log/myapp/stderr.log \
              -a \
              -u myapp \
              -- /usr/bin/myapp

Prevent duplicate instances with a lock file:

    daemonize -p /var/run/myapp.pid \
              -l /var/run/myapp.lock \
              -- /usr/bin/myapp

Set environment variables and working directory:

    daemonize -c /srv/myapp \
              -E DATABASE_URL=postgres://localhost/myapp \
              -E RUST_LOG=info \
              -- /usr/bin/myapp

# SEE ALSO

**daemon**(3), **fork**(2), **setsid**(2), **flock**(2), **sysexits**(3)
