## 0.9.0 (2026-07-02)

* build(just): cross-check FreeBSD/NetBSD/Linux targets in `just check` ([2d186f1](https://github.com/camercu/blivet/commit/2d186f1))
* fix(steps): make clamp_max_fd portable to signed rlim_t ([fdfbf11](https://github.com/camercu/blivet/commit/fdfbf11))
* fix(steps): saturate fd-close bound instead of wrapping to i32 ([6c0aa86](https://github.com/camercu/blivet/commit/6c0aa86))
* docs: fix three stale comments ([7e20c1f](https://github.com/camercu/blivet/commit/7e20c1f))
* docs: point pidfile-cleanup docs at cleanup_on_term_signals ([83a1098](https://github.com/camercu/blivet/commit/83a1098))
* docs(config): correct validate() error listing ([191d49b](https://github.com/camercu/blivet/commit/191d49b))
* docs(lib): reattach daemonize rustdoc lost to a private helper ([26da3e8](https://github.com/camercu/blivet/commit/26da3e8))
* docs(unsafe): add the CLI's unsafe blocks to the containment inventory ([2d0af53](https://github.com/camercu/blivet/commit/2d0af53))
* test(traceability): raise coverage ratchet to 103 ([3c6125d](https://github.com/camercu/blivet/commit/3c6125d))
* fix(signals)!: preserve caller's SIGPIPE disposition across daemonize ([e35e04b](https://github.com/camercu/blivet/commit/e35e04b))
* chore(deps): bump semantic-release to 25 for security fixes ([7767b37](https://github.com/camercu/blivet/commit/7767b37))

### BREAKING CHANGE

* after daemonize()/daemonize_unchecked(), SIGPIPE keeps
the disposition it had at entry (for Rust programs: ignored) instead of
being reset to SIG_DFL. Callers that relied on daemonize() installing
default SIGPIPE must set it themselves.

## [0.8.0](https://github.com/camercu/blivet/compare/v0.7.0...v0.8.0) (2026-06-23)


### ⚠ BREAKING CHANGES

* **api:** `drop_privileges` now panics if a user switch is requested
while more than one thread is running. Callers on non-mainstream targets, or
that manage single-threadedness themselves, must use
`unsafe { drop_privileges_unchecked() }`.
Migrate: ctx.drop_privileges()  ->  unsafe { ctx.drop_privileges_unchecked() }

Co-Authored-By: Claude Opus 4.8 <noreply@anthropic.com>

### Features

* **api:** guard drop_privileges against multithreaded setenv ([cda5645](https://github.com/camercu/blivet/commit/cda5645e842920e356d2422babf0cbcfa0b80699))

## [0.7.0](https://github.com/camercu/blivet/compare/v0.6.0...v0.7.0) (2026-06-22)


### ⚠ BREAKING CHANGES

* **api:** `daemonize` is now the safe, thread-count-checked entry
point (formerly `daemonize_checked`); the unchecked `unsafe fn` is now
`daemonize_unchecked` (formerly `daemonize`).
Migrate: `daemonize_checked(&c)`    -> `daemonize(&c)`
         `unsafe { daemonize(&c) }` -> `unsafe { daemonize_unchecked(&c) }`

Co-Authored-By: Claude Opus 4.8 <noreply@anthropic.com>

### Features

* **api:** make safe `daemonize` the default entry point ([57d3b97](https://github.com/camercu/blivet/commit/57d3b97bb29fd303f50b0dd85aea441c393b5bd5))

## [0.6.0](https://github.com/camercu/blivet/compare/v0.5.0...v0.6.0) (2026-06-22)


### ⚠ BREAKING CHANGES

* **context:** notify_parent and notify_parent_or_report now fail with
PrivilegesNotDropped when a user/group is configured but drop_privileges() was
not called first. Call drop_privileges() before notify_parent() (the already
documented order). There is no opt-out to stay privileged past readiness yet.
* **lib:** on macOS/*BSD, daemonize_checked is now a working function
rather than a #[deprecated] stub. Code that relied on the deprecation
warning, or gated solely on `#[cfg(target_os = "linux")]`, should widen the
gate to the supported set (see crate docs).
* **context:** DaemonContext::notify_parent returns
Result<(), DaemonizeError> instead of Result<(), std::io::Error>.
* **config:** DaemonConfig::umask takes u32 instead of
nix::sys::stat::Mode. Replace `.umask(Mode::from_bits_truncate(0o022))`
with `.umask(0o022)`.

### Features

* **config:** take umask as octal u32 instead of nix Mode ([1131066](https://github.com/camercu/blivet/commit/1131066a2a2f7264e28d4286fb0c30adf5c79a44))
* **context:** add opt-in pidfile cleanup on signals ([eca1739](https://github.com/camercu/blivet/commit/eca1739ce60ffb1a91fa61a48e2d2ed303d1fba8))
* **context:** refuse to notify readiness while privileges undropped ([7e0268b](https://github.com/camercu/blivet/commit/7e0268bc9673ebb76d37ba09e43c858830312700))
* **context:** return DaemonizeError from notify_parent ([ec984d2](https://github.com/camercu/blivet/commit/ec984d283adf01366e2a761284208e37a09bccd7))
* **error:** add Application variant for caller-reported failures ([8b58a0e](https://github.com/camercu/blivet/commit/8b58a0e0f1d2bf227c1e62fe12e80c064e8bcd83))
* **lib:** provide deprecated daemonize_checked stub on non-Linux ([af98234](https://github.com/camercu/blivet/commit/af9823412c8d658f1099ab042989b8032f7b9a86))
* **lib:** support daemonize_checked on macOS and the BSDs ([24117f7](https://github.com/camercu/blivet/commit/24117f731b3c0ed61355d36987212ac0c9614b30))


### Bug Fixes

* **context:** remove pidfile before signaling parent in report_error ([285f997](https://github.com/camercu/blivet/commit/285f997a4b0831e775a411436eb05477ea56d90e))
* **context:** remove pidfile when report_error aborts startup ([cef6425](https://github.com/camercu/blivet/commit/cef642532dbb573ca3a4a5e8665ec5ee9465ccce))
* **error:** never return exit code 0 from exit_code() ([3f62378](https://github.com/camercu/blivet/commit/3f62378a95dd6bfe713dfb04bffc5f4ce87c8967))
* **examples:** reset accepted socket to blocking in echo server ([bde43ec](https://github.com/camercu/blivet/commit/bde43ec30efbfb09b5955f414209fd54b89f1503))
* **lib:** fail closed when daemonize_checked thread count isn't exactly 1 ([6d89d43](https://github.com/camercu/blivet/commit/6d89d433ffb0aa62f2aeb450b262600ccc6ae70c))
* **unsafe_ops:** count OpenBSD threads exactly via a fetch call ([8327501](https://github.com/camercu/blivet/commit/8327501582c58f70177b4218558f69fece8cab2e))
* **unsafe_ops:** error on zero-size OpenBSD thread-count sysctl ([2b62720](https://github.com/camercu/blivet/commit/2b62720653771633c4b1c24142073200109b16de))

## [0.5.0](https://github.com/camercu/blivet/compare/v0.4.0...v0.5.0) (2026-04-25)


### ⚠ BREAKING CHANGES

* the --no-close-fds CLI flag is removed

### Bug Fixes

* **ci:** use Nix for manpage check to pin Pandoc version ([e7af539](https://github.com/camercu/blivet/commit/e7af539d6697e7689f4d5c49800bc0d5a845293e))
* **cli:** correct binary name in --version and --help output ([9a88f0f](https://github.com/camercu/blivet/commit/9a88f0f6e3a5f917426111404271902ed6a8d749))
* remove --no-close-fds CLI flag ([3c1f4e0](https://github.com/camercu/blivet/commit/3c1f4e0c2c7b6fc4e0bd3f0db3f19947527840d7))

## [0.4.0](https://github.com/camercu/blivet/compare/v0.3.3...v0.4.0) (2026-04-25)


### ⚠ BREAKING CHANGES

* In foreground mode, stdout and stderr are no longer
redirected to /dev/null when not explicitly configured. They are left
inherited from the parent process so output reaches the terminal or
supervisor. Stdin is still redirected to /dev/null in all modes.
* DaemonContext now removes the pidfile on drop by
default. Set cleanup_on_drop(false) to preserve the previous behavior.

Add cleanup() for best-effort pidfile removal, callable from signal
handlers or explicitly before exit. Runs automatically on drop when
cleanup_on_drop is true (the default). Standalone lockfiles are left
on disk by convention; the flock is released when DaemonContext drops.

Also mention chroot and setrlimit in split-phase docs/examples.

### Features

* add pidfile cleanup method and cleanup-on-drop to DaemonContext ([ba243b0](https://github.com/camercu/blivet/commit/ba243b030aad1ec124f336f3f3cf14d5ef0f3b70))


### Bug Fixes

* preserve stdout/stderr in foreground mode ([744a93a](https://github.com/camercu/blivet/commit/744a93a9b82dcb405f3f1ef092dfe612d81e8794))

## [0.3.3](https://github.com/camercu/blivet/compare/v0.3.2...v0.3.3) (2026-04-20)


### Bug Fixes

* **ci:** regenerate Cargo.lock during release prepare phase ([c05b2ec](https://github.com/camercu/blivet/commit/c05b2ec423c05f94e2f21686dacffdef341e862b))

## [0.3.2](https://github.com/camercu/blivet/compare/v0.3.1...v0.3.2) (2026-04-20)


### Bug Fixes

* **ci:** add rust toolchain to release workflow for cargo publish ([a39dbbd](https://github.com/camercu/blivet/commit/a39dbbd0383a9ada07adb86e9a4f8203a2dbfeed))
* **ci:** enable crates.io publishing and track Cargo.lock in releases ([b2da1af](https://github.com/camercu/blivet/commit/b2da1af0326998635fc58545e10b3e0088f42661))
* **ci:** scope push trigger to main branch only ([44022cd](https://github.com/camercu/blivet/commit/44022cd1ded191830375b6871bd3b9db546a1ac9))
* **ci:** sync Cargo.lock with v0.3.1 release ([457bd89](https://github.com/camercu/blivet/commit/457bd89a4d3bae3f0b5c03df9ed545e210c42bc1))

## [0.3.1](https://github.com/camercu/blivet/compare/v0.3.0...v0.3.1) (2026-04-20)


### Bug Fixes

* **readme:** use static license badge instead of crates.io lookup ([4b57de5](https://github.com/camercu/blivet/commit/4b57de5fcd17df7fcd3dbec1d37f14f5d1da094d))
* **test:** replace daemonize_checked subprocess test with thread-count parse test ([483c318](https://github.com/camercu/blivet/commit/483c31837b550f9104d0801c4c862b3e8df4e120))
* update changelog links to renamed repository ([a49e9a2](https://github.com/camercu/blivet/commit/a49e9a2d4a003b093bb983206d4767c799d8e900))


### Reverts

* Revert "fix(readme): use static license badge instead of crates.io lookup" ([cfc3261](https://github.com/camercu/blivet/commit/cfc3261bbef8e3479880e03e606b2a3b3568a847))

<<<<<<< HEAD
## [0.3.0](https://github.com/camercu/blivet/compare/v0.2.1...v0.3.0) (2026-04-19)


### ⚠ BREAKING CHANGES

* crate name changed from `daemonize` to `blivet`
* DaemonizeError Display output now includes a variant
prefix (e.g. "fork failed: {msg}" instead of just "{msg}"). Code
matching on error message strings will need updating.

Make Forker::fork() an unsafe trait method since it wraps fork(2),
which is UB in multithreaded processes. Callers now explicitly
acknowledge the safety contract.

Move error message prefixes from call sites into the #[error(...)]
attribute on each DaemonizeError variant, eliminating duplicated
prefix strings across the codebase.

* add prefixes to DaemonizeError Display and make Forker::fork unsafe ([4f26e5c](https://github.com/camercu/blivet/commit/4f26e5c3f4ce8f0b893de54844d379e4a5f94d13))
* rename crate from daemonize to blivet ([65bec20](https://github.com/camercu/blivet/commit/65bec206342dbeb7309ae922a3e76970c9d0e710))


### Features

* **cli:** add .out→.err stderr extension derivation ([0a02f99](https://github.com/camercu/blivet/commit/0a02f99ab63b6b4240ec82b7d25448824d0de50f))


### Bug Fixes

* add stdin branch to dup2_stdio helper ([0646b20](https://github.com/camercu/blivet/commit/0646b204db0c32838b989813ba2d84d80c328bef))
* normalize DaemonContext Debug output to unwrap Option fields ([b23e071](https://github.com/camercu/blivet/commit/b23e0714631d304e19d4bda4e34f25978509a5ea))

## [0.2.1](https://github.com/camercu/blivet/compare/v0.2.0...v0.2.1) (2026-04-18)


### Bug Fixes

* **ci:** add curl retry for BSD smoke rustup download ([8e58f7c](https://github.com/camercu/blivet/commit/8e58f7ceacd7709671ae0865f3928f71402605f2))
* **ci:** add curl retry for transient NetBSD CDN failures ([f7a7e74](https://github.com/camercu/blivet/commit/f7a7e74424b4e77580b7411f940e28c2dd1a9031))
* **ci:** add issues and pull-requests write permissions for semantic-release ([4f362e0](https://github.com/camercu/blivet/commit/4f362e06e413465cb9aa82451f40b633ce301bcd))
* **ci:** disable cargo publish in semantic-release to unblock release without crates.io token ([efc5dec](https://github.com/camercu/blivet/commit/efc5decb668c80f82c40232582fa38ca3c14a6b0))
* **test:** disable close_fds in subprocess tests to prevent systemd EBADF abort ([eb50694](https://github.com/camercu/blivet/commit/eb5069493c56629ba62ae87c84163c72d7530ab5))
* **test:** skip close_inherited_fds test in CI to prevent systemd EBADF abort ([7dc982b](https://github.com/camercu/blivet/commit/7dc982bb113b103b7851b85b381216dd07838f6a))
* **test:** skip nonexistent user/group NSS lookups in CI to prevent hangs ([bb1fe01](https://github.com/camercu/blivet/commit/bb1fe01145886a3b1aa5736b4eb0f9ffc1236bf3))
* **test:** try root group before wheel to avoid NSS hang in CI ([c97b2a4](https://github.com/camercu/blivet/commit/c97b2a4437a15bac4f2691370f312e5e512b5597))
