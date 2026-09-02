//! A complete daemonized TCP echo server.
//!
//! Demonstrates the full blivet lifecycle, including the parts the API docs
//! cannot show in a doctest: a serve loop and **clean shutdown that removes the
//! pidfile on `SIGTERM`/`SIGINT`** (recall that `Drop` does not run on signal
//! termination — see [`DaemonConfig::cleanup_on_drop`]).
//!
//! Run daemonized:
//!
//! ```text
//! cargo run --example echo_server
//! echo hello | nc 127.0.0.1 7878      # -> hello
//! kill "$(cat "${TMPDIR:-/tmp}/echo_server.pid")"  # pidfile removed on exit
//! ```
//!
//! Run in the foreground (handy for development; Ctrl-C to stop):
//!
//! ```text
//! cargo run --example echo_server -- --foreground
//! ```

use std::io::{BufRead, BufReader, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use blivet::{daemonize, DaemonConfig};

const ADDR: &str = "127.0.0.1:7878";

/// Absolute path to `name` in the system temp directory.
///
/// Taken from `TMPDIR` rather than written out, because the daemon chdirs to
/// `/` (so the path must be absolute) and because a supported platform need
/// not have `/tmp` — Termux, where this CLI is a normal thing to run, does not.
fn temp_path(name: &str) -> std::path::PathBuf {
    std::env::temp_dir().join(name)
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let foreground = std::env::args().any(|a| a == "--foreground");

    let pidfile = temp_path("echo_server.pid");
    let logfile = temp_path("echo_server.log");

    let mut config = DaemonConfig::new();
    // Absolute paths: the daemon chdirs to `/`, so relative paths would break.
    // stdout/stderr default to /dev/null; redirect them to a log file so our
    // println!/eprintln! are visible.
    config
        .pidfile(&pidfile)
        .stdout(&logfile)
        .stderr(&logfile)
        .foreground(foreground);

    // Daemonize first, while still single-threaded. `daemonize` is the safe
    // entry point: it verifies the single-threaded requirement for us. On
    // targets where it is unavailable, use the portable `unsafe` escape hatch
    // `unsafe { daemonize_unchecked(&config)? }` instead.
    let mut ctx = daemonize(&config)?;

    // Privileged init phase: bind the listening socket.
    let listener = TcpListener::bind(ADDR)?;
    println!("echo_server listening on {ADDR}");

    // Signal the parent we are up; the foreground/background parent exits 0.
    ctx.notify_parent()?;

    // It is now safe to spawn threads and start accepting connections.
    //
    // Install signal handlers so SIGTERM/SIGINT exit the accept loop cleanly,
    // letting us remove the pidfile (Drop alone would not run on a signal).
    let shutdown = Arc::new(AtomicBool::new(false));
    signal_hook::flag::register(signal_hook::consts::SIGTERM, Arc::clone(&shutdown))?;
    signal_hook::flag::register(signal_hook::consts::SIGINT, Arc::clone(&shutdown))?;

    // Non-blocking accept so the loop can observe the shutdown flag promptly.
    listener.set_nonblocking(true)?;
    while !shutdown.load(Ordering::Relaxed) {
        match listener.accept() {
            Ok((stream, peer)) => {
                println!("connection from {peer}");
                // The listener is non-blocking; on the BSDs and macOS the
                // accepted socket inherits that flag, which would make the
                // blocking reads in handle_client fail with WouldBlock. Reset
                // it so each connection uses ordinary blocking I/O.
                if let Err(e) = stream.set_nonblocking(false) {
                    eprintln!("failed to reset socket to blocking: {e}");
                    continue;
                }
                std::thread::spawn(move || {
                    if let Err(e) = handle_client(stream) {
                        eprintln!("client error: {e}");
                    }
                });
            }
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                std::thread::sleep(Duration::from_millis(100));
            }
            Err(e) => eprintln!("accept error: {e}"),
        }
    }

    println!("shutting down, removing pidfile");
    ctx.cleanup();
    Ok(())
}

fn handle_client(stream: TcpStream) -> std::io::Result<()> {
    // try_clone so we can read and write the same connection independently.
    let mut writer = stream.try_clone()?;
    let reader = BufReader::new(stream);
    for line in reader.lines() {
        let line = line?;
        writeln!(writer, "{line}")?;
        writer.flush()?;
    }
    Ok(())
}
