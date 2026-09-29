#!/usr/bin/env bash
# Smoke-test the daemonize CLI and run the library tests on a live Android
# device or emulator over adb.
#
# This is the only tier with a real Android kernel and SELinux, so it is where
# the daemonize sequence itself — fork, setsid, pidfile, signal — is proven.
# The bionic container proves libc behaviour but runs on a host Linux kernel.
#
# Expects the target already built for x86_64-linux-android and a single device
# visible to adb. Takes the target triple as $1 so an arm64 device works too.
set -euo pipefail

TRIPLE="${1:-x86_64-linux-android}"
BUILD_DIR="target/${TRIPLE}/debug"
# /data/local/tmp is the one directory adb can both write to and execute from.
DEVICE_DIR=/data/local/tmp
PIDFILE="${DEVICE_DIR}/daemonize-smoke.pid"

fail() {
    echo "FAIL: $*" >&2
    exit 1
}

[ -x "${BUILD_DIR}/daemonize" ] || fail "no daemonize binary in ${BUILD_DIR}"
# cargo names the test binary with a metadata hash and leaves a .d depfile
# beside it. Stale hashes from earlier builds linger, so take the newest and
# say when there was a choice — this runs against a device with whatever was
# built beforehand, so it cannot ask cargo which artifact is current the way
# the NetBSD job does.
# `|| true` keeps the next line reachable: under `set -e` the assignment takes
# the substitution's status, so with no match the script would exit here and
# the diagnostic below would never print.
CANDIDATES=$(ls -t "${BUILD_DIR}"/deps/blivet-* 2>/dev/null | grep -v '\.d$' || true)
TEST_BIN=$(printf '%s\n' "${CANDIDATES}" | head -1)
[ -n "${TEST_BIN}" ] || fail "no library test binary in ${BUILD_DIR}/deps"
[ -x "${TEST_BIN}" ] || fail "library test binary ${TEST_BIN} is not executable"
if [ "$(printf '%s\n' "${CANDIDATES}" | wc -l)" -gt 1 ]; then
    echo "note: several test binaries present, running the newest: ${TEST_BIN}" >&2
fi

adb wait-for-device
adb push "${BUILD_DIR}/daemonize" "${DEVICE_DIR}/daemonize" >/dev/null
adb push "${TEST_BIN}" "${DEVICE_DIR}/blivet-tests" >/dev/null
adb shell chmod 755 "${DEVICE_DIR}/daemonize" "${DEVICE_DIR}/blivet-tests"
adb shell rm -f "${PIDFILE}"

# Runs far longer than the check below, which kills it, so the check cannot
# race its exit; bounded, so a run that dies before the kill does not leave it
# on the device for good.
adb shell "${DEVICE_DIR}/daemonize" -p "${PIDFILE}" -- sleep 600

# The parent exits only once the daemon has exec'd, and the daemon writes the
# pidfile before that, so the file is already there.
adb shell "[ -f ${PIDFILE} ]" || fail "pidfile not created"

PID=$(adb shell cat "${PIDFILE}" | tr -d '\r')
adb shell "kill -0 ${PID}" || fail "daemon ${PID} not running"
adb shell "kill ${PID}"
adb shell rm -f "${PIDFILE}"
echo "PASS: smoke test on android"

# Android has no /tmp; the suite takes its temp directory from TMPDIR.
#
# Routed through the shared guard, which is what every other tier uses to
# demand the run actually executed tests: a harness with every test compiled
# away still exits 0 and prints "ok", which reads as coverage this tier does
# not have.
# No `|| fail` here: the guard already distinguishes a vacuous run from a
# failing one and forwards the child's status. Collapsing both into one message
# would put the vacuous-run verdict on a run whose tests simply failed, which is
# the conflation the guard exists to prevent.
sh scripts/assert-tests-ran.sh \
    adb shell "cd ${DEVICE_DIR} && TMPDIR=${DEVICE_DIR} ./blivet-tests"
echo "PASS: library tests on android"
