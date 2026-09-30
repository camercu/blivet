## [0.14.1](https://github.com/camercu/blivet/compare/v0.14.0...v0.14.1) (2026-09-30)

### Bug Fixes

* **release:** build the man page from a small pinned shell, fetched first ([215be51](https://github.com/camercu/blivet/commit/215be517c575791cd1173b54940d4e975dafad6a))
* **release:** bump only blivet's own entry in Cargo.lock ([133a3c9](https://github.com/camercu/blivet/commit/133a3c9b9b5aeb4b69208a9e66fc83660dedaeea))

## [0.14.0](https://github.com/camercu/blivet/compare/v0.13.0...v0.14.0) (2026-09-30)

### ⚠ BREAKING CHANGES

* **cargo:** the daemonize binary now requires the cli feature. A
consumer building with --no-default-features gets the library alone
and no binary; the default build is unchanged.

### Features

* **cargo:** put the CLI's argument parser behind a default-on feature ([edb2413](https://github.com/camercu/blivet/commit/edb24132917fabf773a1f82fdc7ebb9ee33a3f1f))
* **platform:** declare Android's capabilities, measured on bionic ([1943a9c](https://github.com/camercu/blivet/commit/1943a9c257201b09c86904d933d522591ae218d6))

### Bug Fixes

* **build:** name the target triple when the target is not Unix ([1358fef](https://github.com/camercu/blivet/commit/1358fef07db142f53657e04b2cf9f8628cd00efe))
* **ci:** count non-Unix diagnostics with colour forced off ([810ee19](https://github.com/camercu/blivet/commit/810ee197818bf25193468fd2dba055b82ca7ce21))
* **ci:** fail closed when the guard cannot read the test counts ([cf1fdcb](https://github.com/camercu/blivet/commit/cf1fdcb03b264afc7f7266d3b438775c1d0d4b5d))
* **ci:** guard each test command in the Linux tier on its own ([37f8c0f](https://github.com/camercu/blivet/commit/37f8c0fb9798e95923713103be1a126768fa9d8b))
* **ci:** keep the run transcript out of the working tree ([cd2ed7e](https://github.com/camercu/blivet/commit/cd2ed7ecac2c6f00facaf6312c5030d23410a48e))
* **ci:** key the vacuous-run guard on the runner, not on one literal ([c3f55c9](https://github.com/camercu/blivet/commit/c3f55c9304d30bcba49855d32d022ee00167b84d))
* **ci:** let a command that failed before running tests report its own error ([cee0aa4](https://github.com/camercu/blivet/commit/cee0aa457bfeac939d36a787ec14b9b83e907829))
* **ci:** let the Android script report a missing test binary ([4feff0c](https://github.com/camercu/blivet/commit/4feff0c068e257ddd8a30385dcb85dc07f719409))
* **ci:** let the guard diagnose a killed run itself ([0644c0a](https://github.com/camercu/blivet/commit/0644c0a38c5900f91dcb2738c4b43bdae3f32783))
* **ci:** make the privileged tier assert its own preconditions ([857b308](https://github.com/camercu/blivet/commit/857b308cb8e32ee4242746153c81ed437f46d1df))
* **ci:** run coverage on the gate profile, through the guard ([961adc1](https://github.com/camercu/blivet/commit/961adc117bc16fba3c58c0bc8793265fac05561c))
* **ci:** tell an all-failed run apart from a run that did nothing ([2ce0b6a](https://github.com/camercu/blivet/commit/2ce0b6ad631c2d77b7034722c66d86d0006efb0a))
* **config:** compare paths that do not exist yet by their resolved parent ([4bb3ff8](https://github.com/camercu/blivet/commit/4bb3ff8c3c70037828bb41c18c188b4ea17aa6c7))
* **config:** drop AT_EACCESS on Android, where bionic rejects it ([9a16361](https://github.com/camercu/blivet/commit/9a1636139ab9042be55c29cddbe5c9937fa31714))
* **config:** name the errno when a writability probe cannot run ([4b629e5](https://github.com/camercu/blivet/commit/4b629e58d43679d9fb5a79b1c6198d747f3e0f54))
* **config:** reject a parent that exists but is not a directory ([173935d](https://github.com/camercu/blivet/commit/173935d56ada1f3c3fad24d1ed01b719361f7279))
* **config:** say the filesystem is read-only instead of that the check failed ([c67781d](https://github.com/camercu/blivet/commit/c67781d0969017c863990a210e9772e5c7a39a5b))
* **context:** remove the pidfile before waking the parent on drop ([31aeb8d](https://github.com/camercu/blivet/commit/31aeb8d7eb38fce2282b6255f781db2bad25c82c))
* **daemonize:** arm the pidfile abort guard before the write, not after ([2aa85d5](https://github.com/camercu/blivet/commit/2aa85d5359aecb44bbe4435675320a9f4e3b4bed))
* **daemonize:** leave a pidfile alone when this process failed to open it ([2e938f6](https://github.com/camercu/blivet/commit/2e938f6874e4850cad5f7867432818b0e16449de))
* **daemonize:** remove the pidfile when the sequence aborts after writing it ([ab97878](https://github.com/camercu/blivet/commit/ab978789cc4063d815c73cebd0e4df237de36a6b))
* **docs:** bring drop_privileges' platform list under the guard ([7300f69](https://github.com/camercu/blivet/commit/7300f695d9c1f3ab4aa3fa5d711e6086ea66a430))
* **examples:** gate the daemonize call the way a consumer has to ([d8fd911](https://github.com/camercu/blivet/commit/d8fd911214ca7f81b5d3f079921432dc74392317))
* **forker:** return an error when the notification pipe cannot be made ([4317d68](https://github.com/camercu/blivet/commit/4317d680182ee7949ca1fc7577461ceadb24e879))
* **just:** compare rustc floors by version, not by sort order ([55dceb0](https://github.com/camercu/blivet/commit/55dceb0f27f402483b32f56e4ed728fa989639f1))
* **just:** hold dev-dependencies to the container tier's rustc too ([c49c446](https://github.com/camercu/blivet/commit/c49c446a843569fff762f6129071b9e64c6aefbf))
* **mutants:** count a timed-out mutant as caught ([c5969c2](https://github.com/camercu/blivet/commit/c5969c21b430b7e30baf3b1155ea45f8e5e1db18))
* **platform:** keep the effective-UID writability check on unlisted targets ([4db06ce](https://github.com/camercu/blivet/commit/4db06ced8816ed415ee0b0d20b368f13abdc14a9))
* **platform:** stop a non-Unix build after the error that names it ([44638e2](https://github.com/camercu/blivet/commit/44638e214546993ccc5f007f8d724ce14c625832))
* **redirect:** compare stream files by identity before truncating them ([3e30a0a](https://github.com/camercu/blivet/commit/3e30a0a5442abc5645fc3add88264c23cceacbd4))
* **redirect:** leave the previous logs alone when stderr fails ([c90f808](https://github.com/camercu/blivet/commit/c90f8085885675ec5092db892c9105dd420b1b94))
* **redirect:** move stdout onto fd 1 before opening stderr ([e0d0fce](https://github.com/camercu/blivet/commit/e0d0fce84fd7441107527eb0997575fc99d0ab27))
* **signals:** skip bionic's reserved real-time signals on Android ([d8e9108](https://github.com/camercu/blivet/commit/d8e9108c6117b4accbbc42a52d647cc2e0e82539))
* **steps:** let a daemon with no free fd get past step 12 ([eba2217](https://github.com/camercu/blivet/commit/eba2217c6591be28089da3de7980c7d63076b829))
* **steps:** make a failed step 12 put every stdio slot back ([0d6d884](https://github.com/camercu/blivet/commit/0d6d884bee1e965ce210c0cd0afffd70fe444367))
* **test:** bless the docs only when asked to ([3c6a763](https://github.com/camercu/blivet/commit/3c6a7633b8c07400afaff7db8b7714bda9b92299))
* **test:** fail a subprocess re-invocation that matched no test ([910ff88](https://github.com/camercu/blivet/commit/910ff882eaa318f41c34b68b433fcb9053d988eb))
* **test:** hold numeric user resolution to what the passwd database says ([b4eeb0b](https://github.com/camercu/blivet/commit/b4eeb0bb9531da513072b071b1baa2e7052140cf))
* **test:** keep the gate off cargo-mutants' 5s kill timeout ([a39ee15](https://github.com/camercu/blivet/commit/a39ee15de8f07b0052e5da5566ce345d363f02d6))
* **test:** keep the guards that ship to crates.io off unpublished files ([12bb5ad](https://github.com/camercu/blivet/commit/12bb5ad7b93b2d1b35dc4656bfb5ef407eccea7a))

## [0.13.0](https://github.com/camercu/blivet/compare/v0.12.0...v0.13.0) (2026-07-15)

### Features

* **config:** reject configured paths containing a NUL byte ([112de2b](https://github.com/camercu/blivet/commit/112de2b29d34ffbd8440ffec921d4240369e7ecd))
* **steps:** report post-fork syscall failures instead of panicking ([ad44bd9](https://github.com/camercu/blivet/commit/ad44bd9a7a897c05ebc80565ae8b0bb8c9c28aa1))

### Bug Fixes

* **notify:** report failure when the pipe is dropped unsignaled ([93ce4de](https://github.com/camercu/blivet/commit/93ce4decf29d9fbdaba7140f2ac6b7fd3a61e251))
* report remaining post-fork panics as errors ([e20013c](https://github.com/camercu/blivet/commit/e20013c741ebe2b5656630a270dd52c46ffd9393))
* **steps:** create standalone pidfile with mode 0644 ([dae1c83](https://github.com/camercu/blivet/commit/dae1c8328c3e88f094ed900db6e02a97eff17e51))

## [0.12.0](https://github.com/camercu/blivet/compare/v0.11.0...v0.12.0) (2026-07-11)

### ⚠ BREAKING CHANGES

* **context:** DaemonContext::chown_paths is removed. drop_privileges
now chowns configured paths itself; use DaemonConfig::chown_paths(false)
to opt out. Callers that called chown_paths() then drop_privileges() can
delete the chown_paths() call.

### Features

* **config:** add chown_paths knob (default true) ([7a81321](https://github.com/camercu/blivet/commit/7a813217c2eec54ce4c64b94b6c08f7a5e417e2c))
* **config:** derive Hash for DaemonConfig ([7c8bfec](https://github.com/camercu/blivet/commit/7c8bfece3652e94e5600bc98ed0288f57bca6acf))
* **context:** drop_privileges chowns configured paths first ([1a52e43](https://github.com/camercu/blivet/commit/1a52e431ca302476b6f9bb5c253d6669dcd07b47))
* **context:** honor chown_paths(false) in drop_privileges ([d57a154](https://github.com/camercu/blivet/commit/d57a1545783e4bcb70cb430796ea3253224538bb))

### Code Refactoring

* **context:** make chown_paths private, fold into drop_privileges ([f293f4e](https://github.com/camercu/blivet/commit/f293f4ea0b0b3e7530cf3fb834d998cfd6aa371f))

## [0.11.0](https://github.com/camercu/blivet/compare/v0.10.0...v0.11.0) (2026-07-10)

### ⚠ BREAKING CHANGES

* **error:** DaemonizeError::LockConflict is now a struct variant
{ path: PathBuf } instead of LockConflict(String). Display output is
unchanged.
* **lib:** in foreground mode daemonize()/daemonize_unchecked()
no longer terminate the process on setup errors; callers receive Err
and choose how to exit.
* **config:** a configured pidfile is now exclusively flock'd unless
a separate lockfile() path is set or no_lockfile() is called. Deployments
that intentionally run multiple instances sharing a pidfile path must
call no_lockfile().

### Features

* **cli:** add --no-lock and delegate lockfile derivation to the library ([aff854a](https://github.com/camercu/blivet/commit/aff854a5ef6c9fd7faa83734a6df0ba75ef6cbb9))
* **config:** derive the lockfile from the pidfile by default ([dd2c478](https://github.com/camercu/blivet/commit/dd2c4784bc928110b1d55eafe1403b56ff7acc35))
* **error:** carry the conflicting path in LockConflict ([f257d4e](https://github.com/camercu/blivet/commit/f257d4e6b6456179fa4619e8bbb03368368ef51a))

### Bug Fixes

* **config:** name the offending path in validation error messages ([46fd868](https://github.com/camercu/blivet/commit/46fd8686d7ab4c51f164e0783f903157b206dfff))
* **config:** name the pidfile in derived-lockfile overlap errors ([cffbee4](https://github.com/camercu/blivet/commit/cffbee451e314230a7267952ff9bef9ce0f55829))
* **coverage:** run under nextest to avoid harness pipe corruption ([8dda538](https://github.com/camercu/blivet/commit/8dda538bbfe70b230afc20303f12a0c63dac7cea))
* **docker:** run doctests without --include-ignored ([6948303](https://github.com/camercu/blivet/commit/6948303bb76b9fc736e2e760b59f3c62254b0856))
* **lib:** return foreground setup errors instead of exiting silently ([b285a59](https://github.com/camercu/blivet/commit/b285a598c4d0674c255903be063c206194e20332))
* **steps:** enumerate open fds instead of brute-force close loop ([4a6ca29](https://github.com/camercu/blivet/commit/4a6ca2903c364cd0a5efb9f647682e6fe188ad57))

## [0.10.0](https://github.com/camercu/blivet/compare/v0.9.0...v0.10.0) (2026-07-04)

### ⚠ BREAKING CHANGES

* **cli:** the daemonize CLI exits 66 instead of 71 when a
bare-name (PATH-resolved) target program exists but is not executable.
* **cli:** the daemonize CLI exits 66 instead of 71 when the
target program or its script interpreter does not exist at exec time.

### Bug Fixes

* **cli:** map exec-time EACCES to ProgramNotFound (exit 66) ([c2a6577](https://github.com/camercu/blivet/commit/c2a65775cecf62dac1a9ae0526127d35870fd3eb))
* **cli:** map exec-time ENOENT to ProgramNotFound (exit 66) ([ebc012d](https://github.com/camercu/blivet/commit/ebc012d7fc392257e30b725bcfc1604d3c716806))
* **signals:** make cleanup_on_signals install all-or-nothing ([a2d77ad](https://github.com/camercu/blivet/commit/a2d77ad8f64abee0f79878aef30cea2906f59701))
* **signals:** restore cleanup pointer before dispositions on rollback ([54facb9](https://github.com/camercu/blivet/commit/54facb97e870c57bcc9e273c7a0c42dc040fc8b9))

## [0.9.0](https://github.com/camercu/blivet/compare/v0.8.0...v0.9.0) (2026-07-02)

### ⚠ BREAKING CHANGES

* **signals:** after daemonize()/daemonize_unchecked(), SIGPIPE keeps
the disposition it had at entry (for Rust programs: ignored) instead of
being reset to SIG_DFL. Callers that relied on daemonize() installing
default SIGPIPE must set it themselves.

### Bug Fixes

* **signals:** preserve caller's SIGPIPE disposition across daemonize ([e35e04b](https://github.com/camercu/blivet/commit/e35e04bb54e360960356e7c2f239c074ca16e2f6))
* **steps:** make clamp_max_fd portable to signed rlim_t ([fdfbf11](https://github.com/camercu/blivet/commit/fdfbf11570549b6a7596d2557e5b1f2fcb7c3ff0))
* **steps:** saturate fd-close bound instead of wrapping to i32 ([6c0aa86](https://github.com/camercu/blivet/commit/6c0aa86419558b4884387a3d8e52777412e0998d))

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
