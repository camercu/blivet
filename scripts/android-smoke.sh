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
# beside it. Stale hashes from earlier builds linger, so take the newest.
TEST_BIN=$(ls -t "${BUILD_DIR}"/deps/blivet-* 2>/dev/null | grep -v '\.d$' | head -1)
[ -n "${TEST_BIN}" ] || fail "no library test binary in ${BUILD_DIR}/deps"

adb wait-for-device
adb push "${BUILD_DIR}/daemonize" "${DEVICE_DIR}/daemonize" >/dev/null
adb push "${TEST_BIN}" "${DEVICE_DIR}/blivet-tests" >/dev/null
adb shell chmod 755 "${DEVICE_DIR}/daemonize" "${DEVICE_DIR}/blivet-tests"
adb shell rm -f "${PIDFILE}"

adb shell "${DEVICE_DIR}/daemonize" -p "${PIDFILE}" -- sleep 30

# The daemon writes the pidfile after the second fork, so the parent's exit
# does not mean the file is there yet.
for _ in $(seq 1 10); do
    adb shell "[ -f ${PIDFILE} ]" && break
    sleep 1
done
adb shell "[ -f ${PIDFILE} ]" || fail "pidfile not created"

PID=$(adb shell cat "${PIDFILE}" | tr -d '\r')
adb shell "kill -0 ${PID}" || fail "daemon ${PID} not running"
adb shell "kill ${PID}"
adb shell rm -f "${PIDFILE}"
echo "PASS: smoke test on android"

# Android has no /tmp; the suite takes its temp directory from TMPDIR.
RESULTS=$(mktemp)
trap 'rm -f "${RESULTS}"' EXIT
adb shell "cd ${DEVICE_DIR} && TMPDIR=${DEVICE_DIR} ./blivet-tests" 2>&1 | tee "${RESULTS}"

# A harness with every test compiled away still exits 0 and prints "ok", which
# reads as coverage this tier does not have. Demand a nonzero pass count.
grep -qE '^test result: ok\. [1-9][0-9]* passed' "${RESULTS}" \
    || fail "library tests did not report a passing run"
echo "PASS: library tests on android"
