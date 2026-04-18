## 1.0.0 (2026-04-18)


### ⚠ BREAKING CHANGES

* downstream exhaustive matches on DaemonizeError now
require a wildcard arm. This prevents future variant additions from
being breaking changes.

### Features

* add #[non_exhaustive] to DaemonizeError ([6522291](https://github.com/camercu/daemonize-rs/commit/652229110765f72da1450b539704064a847f1bdf))
* add split-phase privilege dropping, group switching, and foreground mode ([16a6231](https://github.com/camercu/daemonize-rs/commit/16a62316c3b6b4e2fbf0e60a25c069d3c6c389f3))
* **cargo:** add repository, readme, and exclude fields for publish readiness ([6f940cf](https://github.com/camercu/daemonize-rs/commit/6f940cf1d3da34e12914fa8a8692e05f14362875))
* **cli:** default lockfile to pidfile path for single-instance enforcement ([b331751](https://github.com/camercu/daemonize-rs/commit/b3317511d5c78495ed5cd5296909056fa29a9384))
* **cli:** default stderr to stdout path, swap .stdout extension to .stderr ([92d2ca0](https://github.com/camercu/daemonize-rs/commit/92d2ca0b3483de26aef7fe7de26ca6c39b84fcc9))
* implement daemonize library and CLI ([208bf0a](https://github.com/camercu/daemonize-rs/commit/208bf0a24625b6d6a0497dcbb49be8deda2520a4))


### Bug Fixes

* add #[must_use] to validate() and notify_parent(), fix docs and redundant user resolution ([18fc36a](https://github.com/camercu/daemonize-rs/commit/18fc36a1263c72e5be492d698ac3f3d7312432c3))
* address code review findings and add missing tests ([d8752cb](https://github.com/camercu/daemonize-rs/commit/d8752cb6f5838eb89f48414ccdb8d76012fca98c))
* **ci:** add curl retry for BSD smoke rustup download ([8e58f7c](https://github.com/camercu/daemonize-rs/commit/8e58f7ceacd7709671ae0865f3928f71402605f2))
* **ci:** add curl retry for transient NetBSD CDN failures ([f7a7e74](https://github.com/camercu/daemonize-rs/commit/f7a7e74424b4e77580b7411f940e28c2dd1a9031))
* **ci:** bump cross-platform-actions to v0.32.0 for OmniOS support ([f129930](https://github.com/camercu/daemonize-rs/commit/f1299304a21e74f0acb55f32fa877b309c936171))
* **ci:** bump cross-platform-actions to v1.0.0 ([e061754](https://github.com/camercu/daemonize-rs/commit/e061754f035220245289506cb79e51c72f18bfeb))
* **ci:** cross-compile for NetBSD smoke test ([93e0ade](https://github.com/camercu/daemonize-rs/commit/93e0adee403d7928d865e1caa1bf2c6d66d01427))
* **ci:** disable cargo publish in semantic-release to unblock release without crates.io token ([efc5dec](https://github.com/camercu/daemonize-rs/commit/efc5decb668c80f82c40232582fa38ca3c14a6b0))
* **ci:** drop ./lib from NetBSD sysroot extract, disable OmniOS smoke ([d4ef1df](https://github.com/camercu/daemonize-rs/commit/d4ef1df229b598a5e57070f0e057b33890124171))
* **ci:** extract full NetBSD sysroot for cross-compilation ([245f859](https://github.com/camercu/daemonize-rs/commit/245f859c1ffadcd3f0633b637012b653b24d291b))
* **ci:** use POSIX sh and system packages for OpenBSD/NetBSD smoke tests ([808047b](https://github.com/camercu/daemonize-rs/commit/808047b747e2e4e40c2449b75666fef1cb7c738a))
* **ci:** use testuser/testgroup in Docker root integration tests ([2336a9c](https://github.com/camercu/daemonize-rs/commit/2336a9ce6deeaa0fc2be1c33b6dc5ff2e4ef7675))
* make all unit tests fast and deterministic ([bf9045d](https://github.com/camercu/daemonize-rs/commit/bf9045de158d9dfc3e0bd3d75e766ee0e33d09a9))
* remove unnecessary unsafe blocks around SIGRTMIN/SIGRTMAX ([360086c](https://github.com/camercu/daemonize-rs/commit/360086c05d86098fa8dac7ef0229c5c1731a28fe))
* **test:** disable close_fds in subprocess tests to prevent systemd EBADF abort ([eb50694](https://github.com/camercu/daemonize-rs/commit/eb5069493c56629ba62ae87c84163c72d7530ab5))
* **test:** remove resolve_user_nonexistent_numeric test that hangs in CI ([ce55369](https://github.com/camercu/daemonize-rs/commit/ce553694f20ac0de8451023af318ef9311cb02ce))
* **test:** skip close_inherited_fds test in CI to prevent systemd EBADF abort ([7dc982b](https://github.com/camercu/daemonize-rs/commit/7dc982bb113b103b7851b85b381216dd07838f6a))
* **test:** skip nonexistent user/group NSS lookups in CI to prevent hangs ([bb1fe01](https://github.com/camercu/daemonize-rs/commit/bb1fe01145886a3b1aa5736b4eb0f9ffc1236bf3))
* **test:** try root group before wheel to avoid NSS hang in CI ([c97b2a4](https://github.com/camercu/daemonize-rs/commit/c97b2a4437a15bac4f2691370f312e5e512b5597))
