//! A container tier must not report success having run no tests.
//!
//! `cargo test` prints `test result: ok.` and exits 0 when it matches no
//! tests, so a container whose sources look stale to cargo — the warm
//! dependency layer leaving a fingerprint newer than the source `COPY` that
//! follows it — goes green having proven nothing. `Dockerfile.termux` avoids
//! that staleness by hand; these tests cover the backstop that notices when
//! some future container does not.

use std::path::PathBuf;
use std::process::Output;

mod common;
use common::code_before;

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

/// Runs the guard over `inner`, a shell command standing in for a test run.
///
/// Invoked as `sh <script>` rather than executed directly: Termux has no
/// `/bin/sh` for a shebang to name, so the containers call it this way too.
fn guard(inner: &str) -> Output {
    guard_with(inner, None, &[])
}

/// The same, with `tools` ahead of `PATH` so a test can stand in for a program
/// the script depends on, and `env` set for the run.
fn guard_with(inner: &str, tools: Option<&std::path::Path>, env: &[(&str, &str)]) -> Output {
    let script = repo_root().join("scripts/assert-tests-ran.sh");
    assert!(
        script.is_file(),
        "the guard script is missing: {}",
        script.display()
    );
    let mut command = std::process::Command::new("sh");
    command
        .arg(&script)
        .arg("sh")
        .arg("-c")
        .arg(inner)
        .current_dir(repo_root());
    if let Some(tools) = tools {
        let path = std::env::var("PATH").unwrap_or_default();
        command.env("PATH", format!("{}:{path}", tools.display()));
    }
    // Cleared, not merely unset by default: the privileged tier exports this
    // for its own run, so every test process under that tier inherits it and
    // these fixtures would be judged against a demand they never made.
    command.env_remove("ASSERT_NO_SKIPPED");
    for (key, value) in env {
        command.env(key, value);
    }
    command.output().expect("the guard script runs")
}

#[test]
fn a_run_that_reported_no_tests_fails() {
    // GIVEN a test run that matched nothing, which cargo reports as success
    let out = guard("echo 'test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured'");

    // THEN the guard rejects it
    assert!(
        !out.status.success(),
        "a run of zero tests must not pass; stderr:\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn a_run_that_reported_tests_passes() {
    // GIVEN a test run that actually ran tests
    let out = guard("echo 'test result: ok. 197 passed; 0 failed; 14 ignored; 0 measured'");

    // THEN the guard accepts it
    assert!(
        out.status.success(),
        "a run of 197 tests must pass; stderr:\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn a_failing_run_stays_failed() {
    // GIVEN a run that ran tests and failed some
    let out = guard("echo 'test result: FAILED. 3 passed; 2 failed; 0 ignored'; exit 101");

    // THEN the guard forwards the failure rather than masking it behind its
    // own "tests did run" verdict
    assert_eq!(
        out.status.code(),
        Some(101),
        "the test command's exit status must survive the guard; stderr:\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn a_run_where_everything_failed_is_not_reported_as_no_tests() {
    // GIVEN a run that executed tests and every one of them failed
    let out = guard("echo 'test result: FAILED. 0 passed; 5 failed; 0 ignored'; exit 101");

    // THEN the guard forwards that failure rather than claiming nothing ran:
    // the counts prove tests did run, and naming the wrong cause sends the
    // reader looking for a stale build that is not there.
    assert_eq!(
        out.status.code(),
        Some(101),
        "an all-failed run must keep its own status"
    );
    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        !combined.contains("reported no tests at all"),
        "5 tests ran and failed, so the no-tests diagnosis is wrong; got:\n{combined}"
    );
}

#[test]
fn a_command_that_failed_before_running_tests_keeps_its_own_error() {
    // GIVEN a command that failed before any test could run — a compile error
    // is the usual one — so it printed no summary line at all
    let out = guard("echo 'error[E0308]: mismatched types' >&2; exit 101");

    // THEN the guard forwards that failure. A build that never got as far as
    // running tests is not the stale-source case, and saying so sends the
    // reader hunting a problem that is not there.
    assert_eq!(
        out.status.code(),
        Some(101),
        "a command that failed outright must keep its status"
    );
    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        !combined.contains("reported no tests at all"),
        "the command failed before running tests; got:\n{combined}"
    );
}

#[test]
fn a_command_killed_before_it_reported_a_status_says_so() {
    // GIVEN the guard's own subshell dies before it can record the exit status
    let out = guard("kill -9 $PPID");

    // THEN the guard names that, rather than letting a failed `cat` speak for
    // it. A script whose whole job is the right diagnosis must not answer with
    // a missing-file error about its own scratch directory.
    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        !out.status.success(),
        "a run that never reported a status must not pass"
    );
    assert!(
        !combined.contains("cat:"),
        "the guard must diagnose the killed run itself; got:\n{combined}"
    );
    assert!(
        combined.contains("killed"),
        "the guard must say the command was killed; got:\n{combined}"
    );
}

#[test]
fn a_run_whose_counts_could_not_be_read_fails() {
    use std::os::unix::fs::PermissionsExt;

    // GIVEN an `awk` that reports nothing and exits 0, so the count the guard
    // reads back is the empty string rather than a number
    let tools = tempfile::tempdir().unwrap();
    let awk = tools.path().join("awk");
    std::fs::write(&awk, "exit 0\n").unwrap();
    std::fs::set_permissions(&awk, std::fs::Permissions::from_mode(0o755)).unwrap();

    let out = guard_with(
        "echo 'test result: ok. 0 passed; 0 failed'",
        Some(tools.path()),
        &[],
    );

    // THEN the guard fails closed. A run of zero tests reaching `exit 0`
    // because the count was unreadable is the outcome the guard exists to
    // prevent, arrived at from the other side.
    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        !out.status.success(),
        "a count the guard could not read must not pass; got:\n{combined}"
    );
    assert!(
        combined.contains("counts"),
        "the guard must say it could not read the counts; got:\n{combined}"
    );
}

/// A nextest summary line, as nextest prints it. `colour` wraps the parts
/// nextest highlights, which CI turns on through `CARGO_TERM_COLOR`.
fn nextest_summary(count: &str, word: &str, colour: bool) -> String {
    let (on, off) = if colour {
        ("\\033[1m", "\\033[0m")
    } else {
        ("", "")
    };
    format!(
        "printf '   {on}Summary{off} [   0.007s] {count} {word} run: {count} passed, 3 skipped\\n'"
    )
}

#[test]
fn a_nextest_run_that_ran_tests_passes() {
    for colour in [false, true] {
        // GIVEN a healthy nextest run, which reports its counts in nextest's
        // words rather than libtest's
        let out = guard(&nextest_summary("271", "tests", colour));

        // THEN the guard accepts it. Reporting a run of 271 tests as one that
        // proved nothing is the wrong diagnosis from a script whose whole
        // product is the right diagnosis.
        let combined = format!(
            "{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        assert!(
            out.status.success(),
            "a nextest run of 271 tests must pass (colour: {colour}); got:\n{combined}"
        );
    }
}

#[test]
fn a_nextest_run_that_ran_nothing_fails() {
    for colour in [false, true] {
        // GIVEN a nextest run whose filter matched nothing, which nextest also
        // reports as success
        let out = guard(&nextest_summary("0", "tests", colour));

        // THEN the guard rejects it, exactly as it does the libtest form
        assert!(
            !out.status.success(),
            "a nextest run of zero tests must not pass (colour: {colour})"
        );
    }
}

/// The guard script, as every tier names it.
const GUARD: &str = "assert-tests-ran.sh";

#[test]
fn a_run_asked_to_leave_nothing_out_rejects_a_skip() {
    // GIVEN a caller that asked for every test, and a run that skipped some
    let out = guard_with(
        &nextest_summary_with_skips("271", "33"),
        None,
        &[("ASSERT_NO_SKIPPED", "1")],
    );

    // THEN the guard rejects it. "Tests ran" is not the claim here; "these
    // tests ran" is, and a skip is coverage the caller says it has.
    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        !out.status.success(),
        "a run that skipped 33 tests must not pass; got:\n{combined}"
    );
    assert!(
        combined.contains("33 test(s) were skipped"),
        "the guard must name the skip count; got:\n{combined}"
    );
}

#[test]
fn a_run_that_left_nothing_out_passes() {
    // GIVEN the same demand and a run that skipped nothing
    let out = guard_with(
        &nextest_summary_with_skips("271", "0"),
        None,
        &[("ASSERT_NO_SKIPPED", "1")],
    );

    // THEN the guard accepts it
    assert!(
        out.status.success(),
        "a run that skipped nothing must pass; stderr:\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn a_skip_is_ignored_unless_the_caller_asked() {
    // GIVEN a run that skipped tests, from a caller that did not demand none
    let out = guard_with(&nextest_summary_with_skips("271", "33"), None, &[]);

    // THEN the guard accepts it: most tiers legitimately filter, and only the
    // caller knows whether its own claim covers what it left out.
    assert!(
        out.status.success(),
        "a skip must not fail a caller that did not ask; stderr:\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// A nextest summary reporting `count` tests run and `skipped` left out.
fn nextest_summary_with_skips(count: &str, skipped: &str) -> String {
    format!(
        "printf '   Summary [   0.007s] {count} tests run: {count} passed, {skipped} skipped\\n'"
    )
}

/// The ways this repo asks cargo to run tests.
///
/// Keyed on the runner rather than on one literal, because a tier that changes
/// harness must not fall out of the rules below by doing so — `just test`
/// already moved from one to another. `cargo llvm-cov --lcov` is absent
/// deliberately: the coverage job produces an artefact and makes no support
/// claim, and it is advisory in CI.
const TEST_RUNNERS: [&str; 3] = ["cargo test", "cargo nextest run", "llvm-cov nextest"];

/// How many test commands `command` runs.
fn test_commands(command: &str) -> usize {
    TEST_RUNNERS
        .iter()
        .map(|runner| command.matches(runner).count())
        .sum()
}

/// The tier wiring: every place that runs a suite on behalf of a support
/// claim, and so is subject to the rules below.
///
/// Collected from disk, not listed: a hand-written list is a copy of the
/// repository that stops matching it, and a tier file added tomorrow would be
/// unguarded with nothing saying so. Every container image, shell script and
/// workflow is read; one that runs no tests simply has nothing to offend.
///
/// The justfile is not here. The hazard the guard answers is a build cache
/// that makes sources look older than the artifacts built from them, which is
/// a container layer cache; a developer's tree and a fresh CI checkout both
/// give cargo a freshness judgement it can trust.
fn tier_files() -> Vec<PathBuf> {
    let root = repo_root();
    let mut files: Vec<PathBuf> = Vec::new();
    for dir in ["", "scripts", ".github/workflows"] {
        let Ok(entries) = std::fs::read_dir(root.join(dir)) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let name = path.file_name().unwrap_or_default().to_string_lossy();
            let wanted =
                name.starts_with("Dockerfile") || name.ends_with(".sh") || name.ends_with(".yml");
            if path.is_file() && wanted {
                files.push(path);
            }
        }
    }
    files.sort();
    // A glob that matched nothing would make every rule below vacuous, and the
    // tier files it should have found are the ones this crate cannot ship
    // without.
    for required in ["Dockerfile", "Dockerfile.termux", "ci.yml"] {
        assert!(
            files
                .iter()
                .any(|f| f.file_name().is_some_and(|n| n == required)),
            "the tier-file scan did not find {required}; it found {files:?}"
        );
    }
    files
}
/// `text` as whole commands: comments dropped, `\` continuations joined, and
/// the quoting of a JSON-array `CMD` flattened so it reads like a shell line.
fn logical_lines(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut current = String::new();
    for line in text.lines() {
        let code = code_before(line, "#").replace(['"', ',', '[', ']'], " ");
        let continues = code.trim_end().ends_with('\\');
        current.push_str(code.trim_end().trim_end_matches('\\'));
        current.push(' ');
        if !continues {
            // Runs of whitespace collapse, so `["cargo", "test"]` reads the
            // same as `cargo test` once its punctuation is gone.
            out.push(current.split_whitespace().collect::<Vec<_>>().join(" "));
            current.clear();
        }
    }
    let tail = current.split_whitespace().collect::<Vec<_>>().join(" ");
    if !tail.is_empty() {
        out.push(tail);
    }
    out
}

#[test]
fn every_tier_runs_its_tests_through_the_guard() {
    // A tier that runs its suite bare reports success having possibly proven
    // nothing, which is the outcome the guard exists to turn red. The check is
    // per command: a `cargo test` with no guard ahead of it on its own line.
    let mut offenders = Vec::new();
    for path in tier_files() {
        let caller = path.file_name().unwrap_or_default().to_string_lossy();
        let text = std::fs::read_to_string(&path)
            .unwrap_or_else(|e| panic!("{caller} must be readable: {e}"));
        for line in logical_lines(&text) {
            // `--no-run` compiles a suite for another platform and runs
            // nothing, so there is no run here for the guard to judge.
            if line.contains("--no-run") {
                continue;
            }
            let before_any_guard = line.split(GUARD).next().unwrap_or(&line);
            if test_commands(before_any_guard) > 0 {
                offenders.push(format!("  {caller}: {}", line.trim()));
            }
        }
    }

    assert!(
        offenders.is_empty(),
        "a tier runs tests without the guard ahead of them:\n{}\n\
         Prefix the command with `sh scripts/{GUARD}`, so a run that matched \
         nothing cannot report the tier's claim as proven.",
        offenders.join("\n")
    );
}

/// The commands each guard invocation in `text` wraps.
///
/// An invocation ends at the end of its logical line — a trailing `\` joins the
/// next one — or where the next invocation begins, since a tier may chain two
/// guarded commands together.
fn wrapped_commands(text: &str) -> Vec<String> {
    logical_lines(text)
        .iter()
        .flat_map(|line| {
            line.split(GUARD)
                .skip(1)
                .map(str::to_string)
                .collect::<Vec<_>>()
        })
        .collect()
}

#[test]
fn a_guard_invocation_wraps_one_test_command() {
    // The guard sums counts across every suite the wrapped command runs, which
    // its own header states. Chaining a second test command into the same
    // invocation therefore lets that command's counts cover the first's zero —
    // and `cargo test --doc` can never report zero, because rustdoc re-reads
    // the sources every run and has no stale binary to reuse. A tier that
    // chains the two guards its own suite for nothing.
    //
    // Counted on `cargo test`, so a tier running a prebuilt test binary — the
    // NetBSD VM and the Android device — is outside what this proves; each of
    // those runs exactly one binary today.
    let mut offenders = Vec::new();
    for path in tier_files() {
        let caller = path.file_name().unwrap_or_default().to_string_lossy();
        let text = std::fs::read_to_string(&path)
            .unwrap_or_else(|e| panic!("{caller} must be readable: {e}"));
        for command in wrapped_commands(&text) {
            let runs = test_commands(&command);
            if runs > 1 {
                offenders.push(format!(
                    "  {caller}: {runs} test commands in one guard invocation:\n    {}",
                    command.trim()
                ));
            }
        }
    }

    assert!(
        offenders.is_empty(),
        "a guard invocation must wrap one test command:\n{}\n\
         Give each command its own `sh scripts/{GUARD}` prefix, so neither \
         command's counts can stand in for the other's.",
        offenders.join("\n")
    );
}
