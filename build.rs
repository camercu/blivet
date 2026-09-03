//! Resolves the target OS into the capabilities it has, and emits one `cfg`
//! alias per capability.
//!
//! The crate does not need to know which OS it is on. It needs to know whether
//! a thread count is queryable, whether a file-descriptor directory is
//! trustworthy, whether a reserved real-time signal range must be skipped, and
//! whether `faccessat` accepts `AT_EACCESS`. Those questions have different
//! answers under one OS name over time, and asking them by OS name is what let
//! Android — a kernel with `/proc` and reserved real-time signals — take the
//! unknown-Unix branch of every decision at once. See
//! `docs/adr/0001-capability-based-platform-gating.md`.
//!
//! The table below is also the single source of truth for the documented
//! platform list: it is exported as `BLIVET_PLATFORMS`, and the doc-drift guard
//! in `src/doc_sync.rs` reads that rather than keeping a copy of its own.

/// What one target OS can do.
struct Platform {
    /// `target_os` value, as rustc reports it.
    target_os: &'static str,
    /// Display name, as the README and the crate front page write it.
    name: &'static str,
    /// Can the live thread count be read? This gates the safe `daemonize` and
    /// `drop_privileges` entry points, which must prove the process is
    /// single-threaded before forking or calling `setenv`.
    thread_count: bool,
    /// Is that count in `/proc/self/status`? False means it comes from the
    /// per-OS kernel query in `unsafe_ops`.
    thread_count_procfs: bool,
    /// Directory listing this process's open fds, where a trustworthy one
    /// exists. The BSDs' `/dev/fd` shows only 0-2 unless fdescfs is mounted, so
    /// they have none and fall back to closing `3..rlim_cur`.
    fd_dir: Option<&'static str>,
    /// Does libc reserve real-time signals that a signal sweep must not reset?
    rt_signals_reserved: bool,
    /// Does `faccessat` accept `AT_EACCESS`, so a writability probe answers for
    /// the effective UID rather than the real one?
    faccessat_eaccess: bool,
}

/// Every platform the crate claims to support, and what each one can do.
///
/// A target absent from this table still compiles: it takes the conservative
/// fallback of every capability, and the `#[deprecated]` stubs for `daemonize`
/// and `drop_privileges` say so at compile time in its consumers' builds.
const PLATFORMS: &[Platform] = &[
    Platform {
        target_os: "linux",
        name: "Linux",
        thread_count: true,
        thread_count_procfs: true,
        fd_dir: Some("/proc/self/fd"),
        rt_signals_reserved: true,
        faccessat_eaccess: true,
    },
    Platform {
        // Android is Linux-like in libc but a distinct `target_os`, and it
        // does not answer every question the way Linux does: it has `/proc`,
        // and bionic reserves real-time signals (32-35, for POSIX timers,
        // debuggerd, and the profiler) as glibc reserves 32-33 — but its
        // `faccessat` rejects `AT_EACCESS`.
        target_os: "android",
        name: "Android",
        thread_count: true,
        thread_count_procfs: true,
        fd_dir: Some("/proc/self/fd"),
        rt_signals_reserved: true,
        faccessat_eaccess: false,
    },
    Platform {
        target_os: "macos",
        name: "macOS",
        thread_count: true,
        thread_count_procfs: false,
        fd_dir: Some("/dev/fd"),
        rt_signals_reserved: false,
        faccessat_eaccess: true,
    },
    Platform {
        target_os: "freebsd",
        name: "FreeBSD",
        thread_count: true,
        thread_count_procfs: false,
        fd_dir: None,
        rt_signals_reserved: false,
        faccessat_eaccess: true,
    },
    Platform {
        target_os: "netbsd",
        name: "NetBSD",
        thread_count: true,
        thread_count_procfs: false,
        fd_dir: None,
        rt_signals_reserved: false,
        faccessat_eaccess: true,
    },
    Platform {
        target_os: "openbsd",
        name: "OpenBSD",
        thread_count: true,
        thread_count_procfs: false,
        fd_dir: None,
        rt_signals_reserved: false,
        faccessat_eaccess: true,
    },
];

fn main() {
    println!("cargo::rerun-if-changed=build.rs");

    let target_os = std::env::var("CARGO_CFG_TARGET_OS").expect("cargo sets CARGO_CFG_TARGET_OS");
    let platform = PLATFORMS.iter().find(|p| p.target_os == target_os);

    // Every alias the crate may gate on, and whether this target has it. One
    // list serves both emissions below, so the set that is declared and the set
    // that can be set cannot disagree.
    let capabilities = [
        (
            "blivet_thread_count",
            platform.is_some_and(|p| p.thread_count),
        ),
        (
            "blivet_thread_count_procfs",
            platform.is_some_and(|p| p.thread_count_procfs),
        ),
        (
            "blivet_fd_dir",
            platform.is_some_and(|p| p.fd_dir.is_some()),
        ),
        (
            "blivet_rt_signals_reserved",
            platform.is_some_and(|p| p.rt_signals_reserved),
        ),
        (
            "blivet_faccessat_eaccess",
            platform.is_some_and(|p| p.faccessat_eaccess),
        ),
    ];

    // Declared whatever the target, so that a misspelled
    // `#[cfg(blivet_fd_dirr)]` is an `unexpected_cfgs` warning — an error under
    // the `-D warnings` CI sets — rather than a branch that silently never
    // compiles.
    for (alias, _) in capabilities {
        println!("cargo::rustc-check-cfg=cfg({alias})");
    }
    for (alias, enabled) in capabilities {
        if enabled {
            println!("cargo::rustc-cfg={alias}");
        }
    }

    // Defined for every target so `env!` resolves; only read under
    // `blivet_fd_dir`, which is set exactly when this is non-empty.
    let fd_dir = platform.and_then(|p| p.fd_dir).unwrap_or("");
    println!("cargo::rustc-env=BLIVET_FD_DIR={fd_dir}");

    // Two views of the same table, both consumed by the doc-drift guard in
    // `src/doc_sync.rs`: display names for the prose platform lists, and
    // `target_os` values for the `cfg` example the front page shows consumers
    // (who cannot see this crate's capability aliases and must gate by OS).
    let names: Vec<&str> = PLATFORMS.iter().map(|p| p.name).collect();
    println!("cargo::rustc-env=BLIVET_PLATFORMS={}", names.join(","));
    let oses: Vec<&str> = PLATFORMS.iter().map(|p| p.target_os).collect();
    println!(
        "cargo::rustc-env=BLIVET_SUPPORTED_TARGET_OS={}",
        oses.join(",")
    );

    // The target actually being built, so the non-Unix `compile_error!` can
    // name it. The full triple rather than `target_os`, which is "unknown" for
    // targets such as `wasm32-unknown-unknown` and so names nothing. Neither
    // `TARGET` nor `CARGO_CFG_TARGET_OS` reaches anywhere but a build script.
    let target = std::env::var("TARGET").expect("cargo sets TARGET");
    println!("cargo::rustc-env=BLIVET_TARGET={target}");
}
