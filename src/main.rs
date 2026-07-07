use std::ffi::CString;
use std::os::fd::BorrowedFd;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use clap::Parser;

use blivet::{DaemonConfig, DaemonizeError};

/// Run a program as a Unix daemon.
#[derive(Parser)]
#[command(
    name = "daemonize",
    version,
    trailing_var_arg = true,
    after_help = "Installed by the `blivet` crate (`cargo install blivet`).\n\
                  Home: https://github.com/camercu/blivet"
)]
struct Args {
    /// Pidfile path
    #[arg(short = 'p', long = "pidfile")]
    pidfile: Option<PathBuf>,

    /// Working directory
    #[arg(short = 'c', long = "chdir")]
    chdir: Option<PathBuf>,

    /// Process umask (octal, e.g. 022)
    #[arg(short = 'm', long = "umask", value_parser = parse_octal_mode)]
    umask: Option<u32>,

    /// Redirect stdout to file (also sets stderr if --stderr is not given)
    #[arg(short = 'o', long = "stdout")]
    stdout: Option<PathBuf>,

    /// Redirect stderr to file [default: stdout path, .stdout→.stderr / .out→.err]
    #[arg(short = 'e', long = "stderr")]
    stderr: Option<PathBuf>,

    /// Append to stdout/stderr files
    #[arg(short = 'a', long = "append")]
    append: bool,

    /// Lockfile path [default: pidfile path, if set]
    #[arg(short = 'l', long = "lock")]
    lockfile: Option<PathBuf>,

    /// Do not lock the pidfile (allows multiple instances)
    #[arg(long = "no-lock", conflicts_with = "lockfile")]
    no_lock: bool,

    /// Set environment variable (name=value; a bare name sets the empty string)
    #[arg(short = 'E', long = "env")]
    env: Vec<String>,

    /// Run daemon as user (name or numeric UID)
    #[arg(short = 'u', long = "user")]
    user: Option<String>,

    /// Run daemon as group (name or numeric GID)
    #[arg(short = 'g', long = "group")]
    group: Option<String>,

    /// Stay in foreground (no fork)
    #[arg(short = 'f', long = "foreground")]
    foreground: bool,

    /// Diagnostic output before daemonizing
    #[arg(short = 'v', long = "verbose")]
    verbose: bool,

    /// Program to daemonize and its arguments
    #[arg(required = true, allow_hyphen_values = true)]
    program: Vec<String>,
}

fn parse_octal_mode(s: &str) -> Result<u32, String> {
    let bits = u32::from_str_radix(s, 8).map_err(|e| format!("invalid octal umask: {e}"))?;
    if bits & !0o7777 != 0 {
        return Err(format!("umask out of range (max 7777): {s}"));
    }
    Ok(bits)
}

fn parse_env_pair(s: &str) -> (String, String) {
    match s.find('=') {
        Some(pos) => (s[..pos].to_string(), s[pos + 1..].to_string()),
        None => (s.to_string(), String::new()),
    }
}

fn main() -> ExitCode {
    let args = Args::parse();

    // Build config
    let mut config = DaemonConfig::new();
    if let Some(ref p) = args.pidfile {
        config.pidfile(p);
    }
    if let Some(ref p) = args.chdir {
        config.chdir(p);
    }
    if let Some(m) = args.umask {
        config.umask(m);
    }
    if let Some(ref p) = args.stdout {
        config.stdout(p);
    }
    // Default stderr to stdout path when not explicitly set:
    // - If stdout ends in ".stdout", swap extension to ".stderr"
    // - If stdout ends in ".out", swap extension to ".err"
    // - Otherwise, use the same path (shares the fd via dup2)
    let stderr = args.stderr.as_ref().or(args.stdout.as_ref());
    let stderr = stderr.map(|p| {
        if args.stderr.is_some() {
            p.clone()
        } else {
            derive_stderr_path(p)
        }
    });
    if let Some(ref p) = stderr {
        config.stderr(p);
    }
    config.append(args.append);
    // The library derives the lockfile from the pidfile by default; only the
    // explicit path and the opt-out need forwarding.
    if args.no_lock {
        config.no_lockfile();
    } else if let Some(ref p) = args.lockfile {
        config.lockfile(p);
    }
    for env_str in &args.env {
        let (key, value) = parse_env_pair(env_str);
        config.env(key, value);
    }
    if let Some(ref u) = args.user {
        config.user(u);
    }
    if let Some(ref g) = args.group {
        config.group(g);
    }
    config.foreground(args.foreground);

    // Resolve program path before daemonization
    let program_path = resolve_program_path(&args.program[0]);
    let program_path = match program_path {
        Ok(p) => p,
        Err(e) => {
            eprintln!("{e}");
            return ExitCode::from(e.exit_code());
        }
    };

    // Verbose diagnostics
    if args.verbose {
        eprintln!("daemonize: program={program_path}");
        if let Some(ref p) = args.pidfile {
            eprintln!("daemonize: pidfile={}", p.display());
        }
        // Display-only mirror of the library's lockfile derivation
        // (explicit path, else the pidfile, unless --no-lock).
        let lockfile = if args.no_lock {
            None
        } else {
            args.lockfile.as_ref().or(args.pidfile.as_ref())
        };
        if let Some(p) = lockfile {
            eprintln!("daemonize: lockfile={}", p.display());
        }
        if let Some(ref p) = args.chdir {
            eprintln!("daemonize: chdir={}", p.display());
        }
        if let Some(m) = args.umask {
            eprintln!("daemonize: umask={m:03o}");
        }
        if let Some(ref p) = args.stdout {
            eprintln!("daemonize: stdout={}", p.display());
        }
        if let Some(ref p) = stderr {
            eprintln!("daemonize: stderr={}", p.display());
        }
        if args.append {
            eprintln!("daemonize: append=true");
        }
        if let Some(ref u) = args.user {
            eprintln!("daemonize: user={u}");
        }
        if let Some(ref g) = args.group {
            eprintln!("daemonize: group={g}");
        }
        if args.foreground {
            eprintln!("daemonize: foreground=true");
        }
        for env_str in &args.env {
            eprintln!("daemonize: env={env_str}");
        }
    }

    // Daemonize
    // SAFETY: single-threaded here (no threads spawned before this point); use
    // the unchecked form so the CLI stays portable across all Unix targets.
    #[allow(unsafe_code)]
    let mut ctx = match unsafe { blivet::daemonize_unchecked(&config) } {
        Ok(ctx) => ctx,
        Err(e) => {
            eprintln!("{e}");
            return ExitCode::from(e.exit_code());
        }
    };

    // Drop privileges if user or group is configured
    if args.user.is_some() || args.group.is_some() {
        if let Err(e) = ctx.chown_paths() {
            ctx.report_error(&e);
        }
        // SAFETY: single-threaded here (no threads spawned before exec); use
        // the unchecked form so the CLI stays portable across all Unix targets.
        #[allow(unsafe_code)]
        let drop_result = unsafe { ctx.drop_privileges_unchecked() };
        if let Err(e) = drop_result {
            ctx.report_error(&e);
        }
    }

    // Clear CLOEXEC on lockfile fd so the lock survives exec
    if let Some(lockfile_fd) = ctx.lockfile_fd() {
        if let Err(e) = clear_cloexec(lockfile_fd) {
            ctx.report_error(&DaemonizeError::ExecFailed(format!(
                "failed to clear CLOEXEC on lockfile fd: {e}"
            )));
        }
    }

    // Build argv for execvp
    let c_program = CString::new(program_path.as_str()).unwrap_or_else(|_| {
        ctx.report_error(&DaemonizeError::ExecFailed(
            "program path contains null byte".into(),
        ));
    });
    let c_args: Vec<CString> = args
        .program
        .iter()
        .enumerate()
        .map(|(i, a)| {
            if i == 0 {
                c_program.clone()
            } else {
                CString::new(a.as_str()).unwrap_or_else(|_| {
                    ctx.report_error(&DaemonizeError::ExecFailed(format!(
                        "argument contains null byte: {a}"
                    )));
                })
            }
        })
        .collect();

    // Restore the conventional default SIGPIPE disposition for the target
    // program: daemonization preserves the launcher's disposition (Rust
    // ignores SIGPIPE), and an ignored disposition would survive exec(2).
    // SAFETY: SIG_DFL is a valid disposition; the process is single-threaded.
    #[allow(unsafe_code)]
    unsafe {
        libc::signal(libc::SIGPIPE, libc::SIG_DFL)
    };

    // exec — if this returns, it failed; exec_error_variant classifies the errno.
    let Err(err) = nix::unistd::execvp(&c_program, &c_args);
    ctx.report_error(&exec_error_variant(
        err,
        format!("exec {program_path}: {err}"),
    ));
}

/// Maps an `execvp` errno to the reported error. `ENOENT` (program or script
/// interpreter missing) and `EACCES` (program not executable) both become
/// [`DaemonizeError::ProgramNotFound`], matching the pre-fork path check so a
/// missing-or-unusable program is exit 66 in both path and bare form. Every
/// other errno is a genuine OS-level exec failure ([`DaemonizeError::ExecFailed`],
/// exit 71) (R130).
fn exec_error_variant(err: nix::errno::Errno, message: String) -> DaemonizeError {
    use nix::errno::Errno;
    match err {
        Errno::ENOENT | Errno::EACCES => DaemonizeError::ProgramNotFound(message),
        _ => DaemonizeError::ExecFailed(message),
    }
}

fn resolve_program_path(program: &str) -> Result<String, DaemonizeError> {
    if program.contains('/') {
        // Relative or absolute path with /: canonicalize
        let canonical = std::fs::canonicalize(program).map_err(|e| {
            DaemonizeError::ProgramNotFound(format!("cannot resolve {program}: {e}"))
        })?;
        let path_str = canonical
            .to_str()
            .ok_or_else(|| DaemonizeError::ProgramNotFound("path is not valid UTF-8".into()))?;
        // Check executable
        if nix::unistd::access(&canonical, nix::unistd::AccessFlags::X_OK).is_err() {
            return Err(DaemonizeError::ProgramNotFound(format!(
                "not executable: {path_str}"
            )));
        }
        Ok(path_str.to_string())
    } else {
        // Bare name: execvp will search PATH
        Ok(program.to_string())
    }
}

/// Derive a stderr path from a stdout path.
///
/// If the stdout path has a `.stdout` extension, swaps it for `.stderr`.
/// If the stdout path has a `.out` extension, swaps it for `.err`.
/// Otherwise returns the path unchanged (stderr shares the same file).
fn derive_stderr_path(stdout: &Path) -> PathBuf {
    match stdout.extension() {
        Some(ext) if ext == "stdout" => stdout.with_extension("stderr"),
        Some(ext) if ext == "out" => stdout.with_extension("err"),
        _ => stdout.to_path_buf(),
    }
}

fn clear_cloexec(fd: BorrowedFd<'_>) -> Result<(), nix::Error> {
    use nix::fcntl::{fcntl, FcntlArg, FdFlag};
    fcntl(fd, FcntlArg::F_SETFD(FdFlag::empty()))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    // --- exec_error_variant ---

    // Covers: R130
    #[test]
    fn exec_enoent_maps_to_program_not_found() {
        assert!(matches!(
            exec_error_variant(nix::errno::Errno::ENOENT, "m".into()),
            DaemonizeError::ProgramNotFound(_)
        ));
    }

    // Covers: R130
    #[test]
    fn exec_eacces_maps_to_program_not_found() {
        // A missing-or-unusable program is one exit code (66) in both path and
        // bare form; EACCES is the bare-form counterpart to the path-form X_OK
        // check.
        assert!(matches!(
            exec_error_variant(nix::errno::Errno::EACCES, "m".into()),
            DaemonizeError::ProgramNotFound(_)
        ));
    }

    // Covers: R130
    #[test]
    fn exec_other_errno_stays_exec_failed() {
        // Anything that is not "program missing/unusable" is a genuine OS error
        // (exit 71). E2BIG stands in for that class; it is not integration-
        // reachable, hence the unit test.
        assert!(matches!(
            exec_error_variant(nix::errno::Errno::E2BIG, "m".into()),
            DaemonizeError::ExecFailed(_)
        ));
    }

    // --- README flag table parity ---

    /// The README CLI flag table must list exactly the binary's flags. Keeps
    /// clap as the single source of truth so the docs cannot silently drift.
    #[test]
    fn readme_flag_table_matches_clap() {
        use clap::CommandFactory;

        let cmd = Args::command();
        let mut clap_longs: Vec<String> = cmd
            .get_arguments()
            .filter_map(|a| a.get_long())
            // clap auto-adds --help/--version; they are not in the flag table.
            .filter(|l| *l != "help" && *l != "version")
            .map(String::from)
            .collect();
        clap_longs.sort();

        const README: &str = include_str!("../README.md");
        let mut readme_longs: Vec<String> = README
            .lines()
            // CLI flag-table rows carry the long flag in a backticked cell,
            // e.g. "| `--pidfile PATH`". The short-flag cell may be empty
            // (long-only flags like --no-lock).
            .filter(|line| line.contains("| `--"))
            .filter_map(parse_long_flag)
            .collect();
        readme_longs.sort();
        readme_longs.dedup();

        assert_eq!(
            clap_longs, readme_longs,
            "README CLI flag table is out of sync with the binary's flags"
        );
    }

    /// Extract the long-flag name from a README table row like
    /// `| `-p` | `--pidfile PATH` | ... |`.
    fn parse_long_flag(line: &str) -> Option<String> {
        let start = line.find("`--")? + 3;
        let name: String = line[start..]
            .chars()
            .take_while(|c| c.is_ascii_alphanumeric() || *c == '-')
            .collect();
        (!name.is_empty()).then_some(name)
    }

    #[test]
    fn parse_long_flag_extracts_name() {
        assert_eq!(
            parse_long_flag("| `-p` | `--pidfile PATH` | Write PID |").as_deref(),
            Some("pidfile")
        );
        assert_eq!(
            parse_long_flag("| `-f` | `--foreground` | Stay |").as_deref(),
            Some("foreground")
        );
        assert_eq!(
            parse_long_flag("|      | `--no-lock` | Do not lock |").as_deref(),
            Some("no-lock")
        );
    }

    // --- parse_octal_mode ---

    #[test]
    fn parse_octal_mode_valid() {
        assert_eq!(parse_octal_mode("022").unwrap(), 0o022);
    }

    #[test]
    fn parse_octal_mode_zero() {
        assert_eq!(parse_octal_mode("000").unwrap(), 0);
    }

    #[test]
    fn parse_octal_mode_full() {
        assert_eq!(parse_octal_mode("7777").unwrap(), 0o7777);
    }

    #[test]
    fn parse_octal_mode_invalid() {
        assert!(parse_octal_mode("999").is_err());
        assert!(parse_octal_mode("abc").is_err());
        assert!(parse_octal_mode("").is_err());
    }

    #[test]
    fn parse_octal_mode_out_of_range() {
        // Valid octal but wider than the 12 permission bits.
        assert!(parse_octal_mode("10000").is_err());
    }

    // --- parse_env_pair ---

    #[test]
    fn parse_env_pair_key_value() {
        assert_eq!(parse_env_pair("FOO=bar"), ("FOO".into(), "bar".into()));
    }

    #[test]
    fn parse_env_pair_empty_value() {
        assert_eq!(parse_env_pair("FOO="), ("FOO".into(), String::new()));
    }

    #[test]
    fn parse_env_pair_no_equals() {
        assert_eq!(parse_env_pair("FOO"), ("FOO".into(), String::new()));
    }

    #[test]
    fn parse_env_pair_multiple_equals() {
        assert_eq!(
            parse_env_pair("FOO=bar=baz"),
            ("FOO".into(), "bar=baz".into())
        );
    }

    // --- derive_stderr_path ---

    #[test]
    fn derive_stderr_stdout_extension() {
        let result = derive_stderr_path(Path::new("/var/log/app.stdout"));
        assert_eq!(result, PathBuf::from("/var/log/app.stderr"));
    }

    #[test]
    fn derive_stderr_out_extension() {
        let result = derive_stderr_path(Path::new("/var/log/app.out"));
        assert_eq!(result, PathBuf::from("/var/log/app.err"));
    }

    #[test]
    fn derive_stderr_other_extension() {
        let result = derive_stderr_path(Path::new("/var/log/app.log"));
        assert_eq!(result, PathBuf::from("/var/log/app.log"));
    }

    #[test]
    fn derive_stderr_no_extension() {
        let result = derive_stderr_path(Path::new("/var/log/app"));
        assert_eq!(result, PathBuf::from("/var/log/app"));
    }

    // --- clear_cloexec ---

    // Covers: R110
    #[test]
    fn clear_cloexec_removes_flag() {
        use nix::fcntl::{fcntl, open, FcntlArg, FdFlag, OFlag};
        use nix::sys::stat::Mode;
        use std::os::fd::AsFd;

        let fd = open(
            c"/dev/null",
            OFlag::O_RDONLY | OFlag::O_CLOEXEC,
            Mode::empty(),
        )
        .unwrap();
        let flags = fcntl(fd.as_fd(), FcntlArg::F_GETFD).unwrap();
        assert!(FdFlag::from_bits_truncate(flags).contains(FdFlag::FD_CLOEXEC));

        clear_cloexec(fd.as_fd()).unwrap();

        let flags = fcntl(fd.as_fd(), FcntlArg::F_GETFD).unwrap();
        assert!(!FdFlag::from_bits_truncate(flags).contains(FdFlag::FD_CLOEXEC));
    }
}
