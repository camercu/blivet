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
//! platform list. The crate front page's "Platform support" section is written
//! from it here and included by `src/lib.rs`, so no prose states it; the
//! Supported row of the tier tables in `README.md` and `docs/SPEC.md` is
//! written from `BLIVET_PLATFORMS` by `tests/docgen.rs`.

/// Where a platform's live thread count is read from.
///
/// The payload on `KernelQuery` is how the documentation names the call, so the
/// generated platform table cannot disagree with the table below about which
/// platform reads its count from where. `ProcFs` needs no payload: there is one
/// procfs path and every procfs platform uses it.
#[derive(PartialEq)]
enum ThreadCount {
    /// The `Threads:` line of `/proc/self/status`.
    ProcFs,
    /// The per-OS kernel query in `unsafe_ops` — `proc_pidinfo` on macOS, a
    /// `sysctl` on each BSD.
    KernelQuery(&'static str),
}

impl ThreadCount {
    /// How the documentation names this source.
    fn source(&self) -> &'static str {
        match self {
            Self::ProcFs => "`/proc/self/status`",
            Self::KernelQuery(call) => call,
        }
    }
}

/// What one target OS can do.
struct Platform {
    /// `target_os` value, as rustc reports it.
    target_os: &'static str,
    /// Display name, as the README and the crate front page write it.
    name: &'static str,
    /// Where the live thread count comes from, or `None` where it cannot be
    /// read at all. Being readable gates the safe `daemonize` and
    /// `drop_privileges` entry points, which must prove the process is
    /// single-threaded before forking or calling `setenv`.
    ///
    /// One field rather than a readable flag beside a source flag, because
    /// only three of those four combinations mean anything: a source with no
    /// readable count would compile `count()` with every caller gated out,
    /// which is a `dead_code` error under the `-D warnings` CI sets, reached
    /// from a table row that reads perfectly well.
    thread_count: Option<ThreadCount>,
    /// Directory listing this process's open fds, where a trustworthy one
    /// exists. The BSDs' `/dev/fd` shows only 0-2 unless fdescfs is mounted, so
    /// they have none and fall back to closing `3..rlim_cur`.
    fd_dir: Option<&'static str>,
    /// Does libc reserve real-time signals that a signal sweep must not reset?
    ///
    /// True also selects *how* the sweep is built: the standard signals
    /// chained with `SIGRTMIN..=SIGRTMAX`, skipping the reserved band between
    /// them. False sweeps `1..=64`, which reaches a real-time signal wherever
    /// the platform's range falls below 64 — NetBSD's is 33-63 and illumos's
    /// starts at 41, so both get their real-time dispositions reset, while
    /// FreeBSD's starts at 65 and OpenBSD and macOS have none. Resetting is
    /// harmless where libc reserves none of that band, which is the case on
    /// all three, but it is the seam: a platform that reserves a band would
    /// answer this question false and silently stop skipping it. Splitting the
    /// two questions is the fix when such a platform is added.
    ///
    /// The numbers above come from each platform's `sys/signal.h`, not from
    /// the `libc` crate, which exposes `SIGRTMIN` only for the linux-like and
    /// solarish targets. A platform having no `SIGRTMIN` in `libc` is a fact
    /// about the crate's coverage and says nothing about the platform.
    ///
    /// Considered and declined: splitting the two questions now, and putting
    /// each platform's range in the table so a probe could measure the claim
    /// rather than argue it. Nothing behaves wrongly today — every listed
    /// platform inside the sweep reserves none of its band — and the ranges
    /// would be a fourth hand-maintained copy of ABI facts no test here can
    /// check, on targets with no runner. The trigger to do it: a listed
    /// platform that reserves part of its real-time range and answers this
    /// question false. That platform is the one this field cannot describe.
    rt_signals_reserved: bool,
    /// Does `faccessat` *reject* `AT_EACCESS`, leaving a writability probe to
    /// answer for the real UID rather than the effective one?
    ///
    /// Named after the deviation, not the feature: accepting the flag is
    /// POSIX, so an unlisted target assuming it does what every other
    /// capability's default does — declines to assume anything the platform
    /// has not earned. Android is the only listed platform that rejects it.
    faccessat_lacks_eaccess: bool,
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
        thread_count: Some(ThreadCount::ProcFs),
        fd_dir: Some("/proc/self/fd"),
        rt_signals_reserved: true,
        faccessat_lacks_eaccess: false,
    },
    Platform {
        // Android is Linux-like in libc but a distinct `target_os`, and it
        // does not answer every question the way Linux does: it has `/proc`,
        // and bionic reserves real-time signals (32-35, for POSIX timers,
        // debuggerd, and the profiler) as glibc reserves 32-33 — but its
        // `faccessat` rejects `AT_EACCESS`.
        target_os: "android",
        name: "Android",
        thread_count: Some(ThreadCount::ProcFs),
        fd_dir: Some("/proc/self/fd"),
        rt_signals_reserved: true,
        faccessat_lacks_eaccess: true,
    },
    Platform {
        target_os: "macos",
        name: "macOS",
        thread_count: Some(ThreadCount::KernelQuery("`proc_pidinfo`")),
        fd_dir: Some("/dev/fd"),
        rt_signals_reserved: false,
        faccessat_lacks_eaccess: false,
    },
    Platform {
        target_os: "freebsd",
        name: "FreeBSD",
        thread_count: Some(ThreadCount::KernelQuery("`sysctl`")),
        fd_dir: None,
        rt_signals_reserved: false,
        faccessat_lacks_eaccess: false,
    },
    Platform {
        target_os: "netbsd",
        name: "NetBSD",
        thread_count: Some(ThreadCount::KernelQuery("`sysctl`")),
        fd_dir: None,
        rt_signals_reserved: false,
        faccessat_lacks_eaccess: false,
    },
    Platform {
        target_os: "openbsd",
        name: "OpenBSD",
        thread_count: Some(ThreadCount::KernelQuery("`sysctl`")),
        fd_dir: None,
        rt_signals_reserved: false,
        faccessat_lacks_eaccess: false,
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
        // Not a capability but an alias emitted the same way: is this target in
        // the table at all? The probes that assert a capability is *absent*
        // gate on it, because "absent" is a measured fact about a listed
        // platform and only a default for any other — asserting the default
        // would fail on, say, illumos, which has a working /proc/self/fd and
        // would be told by a red test that the crate is broken.
        //
        // Considered and declined: giving each alias a value instead
        // (`cfg(blivet_fd_dir = "no")` for measured-absent, nothing emitted
        // for an unlisted target) would say the same thing per capability and
        // delete this alias. It also puts `= "yes"` on every positive use site
        // — the common case, and today a bare `#[cfg(blivet_fd_dir)]` — to
        // spare the two negative probes one attribute each. The concept earns
        // the line it costs.
        ("blivet_known_platform", platform.is_some()),
        (
            "blivet_thread_count",
            platform.is_some_and(|p| p.thread_count.is_some()),
        ),
        (
            "blivet_thread_count_procfs",
            platform.is_some_and(|p| p.thread_count == Some(ThreadCount::ProcFs)),
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
            "blivet_faccessat_lacks_eaccess",
            platform.is_some_and(|p| p.faccessat_lacks_eaccess),
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

    // Two views of the same table. Display names go to `tests/docgen.rs`, which
    // owns the Supported row of the tier tables in README.md and docs/SPEC.md;
    // `target_os` values go to `tests/target_lists.rs`, which holds the target
    // lists that cannot be generated to the table.
    let names: Vec<&str> = PLATFORMS.iter().map(|p| p.name).collect();
    println!("cargo::rustc-env=BLIVET_PLATFORMS={}", names.join(","));
    write_platform_support_doc();
    write_entry_point_cfg_doc();
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

/// Write the front page's example of gating the checked entry point by OS.
///
/// A consumer cannot see this crate's capability aliases, so the example has
/// to name operating systems — which makes it a copy of the table unless it is
/// written from the table. Written here, a row gaining or losing a thread count
/// changes the example with it, and rustdoc still compiles it as a doctest.
fn write_entry_point_cfg_doc() {
    let out = std::path::PathBuf::from(std::env::var("OUT_DIR").expect("cargo sets OUT_DIR"))
        .join("entry_point_cfg.md");
    let clauses: String = PLATFORMS
        .iter()
        .filter(|p| p.thread_count.is_some())
        .map(|p| format!("    target_os = \"{}\",\n", p.target_os))
        .collect();
    let doc = format!(
        "To also compile on an exotic target without thread-count support, gate the\n\
         call so the deprecated stub is never built:\n\n\
         ```no_run\n\
         # fn main() -> Result<(), Box<dyn std::error::Error>> {{\n\
         # let config = blivet::DaemonConfig::new();\n\
         #[cfg(any(\n{clauses}))]\n\
         let mut ctx = blivet::daemonize(&config)?;\n\
         #[cfg(not(any(\n{clauses})))]\n\
         // SAFETY: no threads spawned before this point.\n\
         let mut ctx = unsafe {{ blivet::daemonize_unchecked(&config)? }};\n\
         # ctx.notify_parent()?;\n\
         # Ok(())\n\
         # }}\n\
         ```\n"
    );
    std::fs::write(&out, doc).unwrap_or_else(|e| panic!("write {}: {e}", out.display()));
}

/// Write the crate front page's "Platform support" section.
///
/// The front page used to spell the list and the per-platform mechanism out in
/// prose, in two places, and both went stale on Android. Generating the section
/// from the table means there is one copy and nothing to compare it with:
/// `src/lib.rs` includes this file, so a row added above reaches the rendered
/// docs with no prose edit at all.
fn write_platform_support_doc() {
    let out = std::path::PathBuf::from(std::env::var("OUT_DIR").expect("cargo sets OUT_DIR"))
        .join("platform_support.md");
    let mut doc = String::from(
        "The checked entry points need the kernel's own count of this process's\n\
         live threads, so they exist wherever that count can be read:\n\n\
         | Platform | Thread count read from |\n| --- | --- |\n",
    );
    for platform in PLATFORMS {
        if let Some(source) = &platform.thread_count {
            doc.push_str(&format!("| {} | {} |\n", platform.name, source.source()));
        }
    }
    doc.push_str(
        "\nOn any other target they are `#[deprecated]` stubs that panic. Use\n\
         [`daemonize_unchecked`] and\n\
         [`drop_privileges_unchecked`](DaemonContext::drop_privileges_unchecked)\n\
         there, having established single-threadedness yourself.\n",
    );
    std::fs::write(&out, doc).unwrap_or_else(|e| panic!("write {}: {e}", out.display()));
}
