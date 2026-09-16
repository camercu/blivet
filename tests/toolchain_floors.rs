//! Guard: the container tier's rustc and the floor `msrv-check` enforces are
//! the same number.
//!
//! Three rustc floors gate this crate: its own MSRV, the rust OpenBSD packages,
//! and the base image of the privileged container tier, which runs
//! `cargo build --locked --tests` and so compiles every dev-dependency. The
//! third was a hand-maintained copy no guard knew about, so a dev-dependency
//! landing above it passed `msrv-check` and broke a release-gating tier
//! instead. Headroom is zero: the highest dev-dependency floor in the locked
//! graph declares exactly the image's version.

use std::path::PathBuf;

fn read(rel: &str) -> String {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(rel);
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()))
}

/// The value of a justfile variable declared as `name := "value"`.
fn just_variable(justfile: &str, name: &str) -> String {
    let prefix = format!("{name} :=");
    justfile
        .lines()
        .find_map(|line| line.trim().strip_prefix(&prefix))
        .unwrap_or_else(|| panic!("the justfile declares no `{name}`"))
        .trim()
        .trim_matches('"')
        .to_owned()
}

/// The rust version the privileged tier's base image pins.
fn dockerfile_rust(dockerfile: &str) -> String {
    let tag = dockerfile
        .lines()
        .find_map(|line| line.trim().strip_prefix("FROM rust:"))
        .expect("the Dockerfile builds on a `rust:` image");
    tag.split('-')
        .next()
        .expect("the image tag names a version")
        .to_owned()
}

#[test]
fn the_container_tier_runs_the_rustc_that_msrv_check_enforces() {
    let declared = just_variable(&read("justfile"), "docker_rust");
    let pinned = dockerfile_rust(&read("Dockerfile"));

    assert_eq!(
        pinned, declared,
        "the privileged tier builds on rust {pinned} but `msrv-check` holds \
         dev-dependencies to {declared}. Move both together: the tier compiles \
         the whole dev-dependency graph, so its rustc is a real floor, and a \
         dependency between the two numbers passes the guard and fails the \
         tier."
    );
}
