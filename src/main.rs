use std::ffi::CString;
use std::os::fd::BorrowedFd;
use std::path::PathBuf;
use std::process::ExitCode;

use clap::Parser;
use nix::sys::stat::Mode;

use daemonize::{DaemonConfig, DaemonizeError};

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

    /// Redirect stdout to file
    #[arg(short = 'o', long = "stdout")]
    stdout: Option<PathBuf>,

    /// Redirect stderr to file
    #[arg(short = 'e', long = "stderr")]
    stderr: Option<PathBuf>,

    /// Append to stdout/stderr files
    #[arg(short = 'a', long = "append")]
    append: bool,

    /// Lockfile path
    #[arg(short = 'l', long = "lock")]
    lockfile: Option<PathBuf>,

    /// Set environment variable (name=value)
    #[arg(short = 'E', long = "env")]
    env: Vec<String>,

    /// Run daemon as user
    #[arg(short = 'u', long = "user")]
    user: Option<String>,

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
    if let Some(ref p) = args.stderr {
        config.stderr(p);
    }
    config.append(args.append);
    if let Some(ref p) = args.lockfile {
        config.lockfile(p);
    }
    for env_str in &args.env {
        let (key, value) = parse_env_pair(env_str);
        config.env(key, value);
    }
    if let Some(ref u) = args.user {
        config.user(u);
    }

    // Resolve program path before daemonization
    let program_path = resolve_program_path(&args.program[0]);
    let program_path = match program_path {
        Ok(p) => p,
        Err(e) => {
            eprintln!("{e}");
            return ExitCode::from(e.exit_code());
        }
    };

    // Validate config before daemonization
    if let Err(e) = config.validate() {
        eprintln!("{e}");
        return ExitCode::from(e.exit_code());
    }

    // Verbose diagnostics
    if args.verbose {
        eprintln!("daemonize: program={program_path}");
        eprintln!("daemonize: config={config:?}");
    }

    // Daemonize
    #[allow(unsafe_code)]
    let mut ctx = match unsafe { daemonize::daemonize(&config) } {
        Ok(ctx) => ctx,
        Err(e) => {
            eprintln!("{e}");
            return ExitCode::from(e.exit_code());
        }
    };

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
            DaemonizeError::ProgramNotFound(format!(
                "cannot resolve program path {program}: {e}"
            ))
        })?;
        let path_str = canonical.to_str().ok_or_else(|| {
            DaemonizeError::ProgramNotFound("program path is not valid UTF-8".into())
        })?;
        // Check executable
        if nix::unistd::access(&canonical, nix::unistd::AccessFlags::X_OK).is_err() {
            return Err(DaemonizeError::ProgramNotFound(format!(
                "program is not executable: {path_str}"
            )));
        }
        Ok(path_str.to_string())
    } else {
        // Bare name: execvp will search PATH
        Ok(program.to_string())
    }
}

fn clear_cloexec(fd: BorrowedFd<'_>) -> Result<(), nix::Error> {
    use nix::fcntl::{fcntl, FcntlArg, FdFlag};
    fcntl(fd, FcntlArg::F_SETFD(FdFlag::empty()))?;
    Ok(())
}
