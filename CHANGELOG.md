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
