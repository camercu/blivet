use std::ffi::CString;
use std::os::fd::BorrowedFd;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use clap::Parser;
use nix::sys::stat::Mode;

use blivet::{DaemonConfig, DaemonizeError};

/// Daemonize a program.
#[derive(Parser)]
#[command(version, about)]
#[command(trailing_var_arg = true)]
struct Args {
    /// Pidfile path
    #[arg(short = 'p', long = "pidfile")]
    pidfile: Option<PathBuf>,

    /// Working directory
    #[arg(short = 'c', long = "chdir")]
    chdir: Option<PathBuf>,

    /// Process umask (octal, e.g. 022)
    #[arg(short = 'm', long = "umask", value_parser = parse_octal_mode)]
    umask: Option<Mode>,

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

    /// Set environment variable (name=value)
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

fn parse_octal_mode(s: &str) -> Result<Mode, String> {
    let bits = u32::from_str_radix(s, 8).map_err(|e| format!("invalid octal umask: {e}"))?;
    Ok(Mode::from_bits_truncate(bits as libc::mode_t))
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
    // Default lockfile to pidfile path for single-instance enforcement
    let lockfile = args.lockfile.as_ref().or(args.pidfile.as_ref());
    if let Some(p) = lockfile {
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
        eprintln!("daemonize: config={config:?}");
    }

    // Daemonize
    #[allow(unsafe_code)]
    let mut ctx = match unsafe { blivet::daemonize(&config) } {
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
        if let Err(e) = ctx.drop_privileges() {
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

    // exec — if this returns, it failed
    let Err(err) = nix::unistd::execvp(&c_program, &c_args);
    ctx.report_error(&DaemonizeError::ExecFailed(format!(
        "exec {program_path}: {err}"
    )));
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

    // --- parse_octal_mode ---

    #[test]
    fn parse_octal_mode_valid() {
        let mode = parse_octal_mode("022").unwrap();
        assert_eq!(mode.bits(), 0o022);
    }

    #[test]
    fn parse_octal_mode_zero() {
        let mode = parse_octal_mode("000").unwrap();
        assert_eq!(mode.bits(), 0);
    }

    #[test]
    fn parse_octal_mode_full() {
        let mode = parse_octal_mode("777").unwrap();
        assert_eq!(mode.bits(), 0o777);
    }

    #[test]
    fn parse_octal_mode_invalid() {
        assert!(parse_octal_mode("999").is_err());
        assert!(parse_octal_mode("abc").is_err());
        assert!(parse_octal_mode("").is_err());
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
