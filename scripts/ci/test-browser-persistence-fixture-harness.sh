#!/bin/bash
# Lightweight shell regression for persistence harness bash (mock run_phase; no native fixture).
set -euo pipefail
ROOT="${ZERON_ROOT:-$(cd "$(dirname "$0")/../.." && pwd)}"
RUNNER_TEMP="$(mktemp -d)"
trap 'rm -rf "$RUNNER_TEMP"' EXIT
export RUNNER_TEMP
BINARY="/mock/browser-fixture"
PERSIST_ROOT="$RUNNER_TEMP/release-root"
export PERSIST_ROOT

# shellcheck source=browser-persistence-fixture-harness-lib.sh
source "$ROOT/scripts/ci/browser-persistence-fixture-harness-lib.sh"

fail() {
  echo "ASSERT FAIL: $*" >&2
  exit 1
}

pass() {
  echo "ASSERT OK: $*"
}

MOCK_PHASE_RC_write=0
MOCK_PHASE_RC_verify=0
MOCK_PHASE_LOG="$RUNNER_TEMP/mock-phase.log"
MOCK_DUMP_LOG="$RUNNER_TEMP/mock-dump.log"
MOCK_RETAIN_LOG="$RUNNER_TEMP/mock-retain.log"

mock_reset_logs() {
  : >"$MOCK_PHASE_LOG"
  : >"$MOCK_DUMP_LOG"
  : >"$MOCK_RETAIN_LOG"
}

run_phase() {
  local phase="$1"
  echo "$phase" >>"$MOCK_PHASE_LOG"
  echo "${ZERON_BROWSER_FIXTURE_RETAIN_WEBSITE_DATA_STORE:-unset}" >>"$MOCK_RETAIN_LOG"
  case "$phase" in
    relaunch-write)
      if [ "$MOCK_PHASE_RC_write" -ne 0 ]; then
        return "$MOCK_PHASE_RC_write"
      fi
      mkdir -p "$PERSIST_ROOT"
      echo '{"port":1,"origin":"http://127.0.0.1:1","profileDiag":"store_uuid=MOCK"}' >"$PERSIST_ROOT/relaunch-marker.json"
      return 0
      ;;
    relaunch-verify)
      return "$MOCK_PHASE_RC_verify"
      ;;
    *)
      return 0
      ;;
  esac
}

dump_store_evidence() {
  local stage="$1"
  local evidence_root="${2:-$PERSIST_ROOT}"
  echo "$evidence_root" >>"$MOCK_DUMP_LOG"
  echo "mock-dump stage=$stage root=$evidence_root"
}

log_contains() {
  local needle="$1"
  local file="$2"
  grep -qx "$needle" "$file"
}

# --- causal arm: writer failure reports real rc and skips verify ---
mock_reset_logs
MOCK_PHASE_RC_write=42
MOCK_PHASE_RC_verify=0
arm_root="$RUNNER_TEMP/causal-fail-write"
out="$(run_causal_lifecycle_arm test-arm 1 fixture-causal-test "$arm_root")"
echo "$out"
echo "$out" | grep -q 'relaunch-write_exit=42' || fail 'writer failure must log exit 42'
echo "$out" | grep -q 'relaunch-verify_exit=skipped' || fail 'verify must be skipped after writer failure'
log_contains relaunch-write "$MOCK_PHASE_LOG" || fail 'write phase must run'
log_contains relaunch-verify "$MOCK_PHASE_LOG" && fail 'verify must not run after write failure'
pass 'writer failure nonzero rc and skip verify'

# --- verify failure stays nonzero ---
mock_reset_logs
MOCK_PHASE_RC_write=0
MOCK_PHASE_RC_verify=7
arm_root="$RUNNER_TEMP/causal-fail-verify"
out="$(run_causal_lifecycle_arm test-arm 1 fixture-causal-test "$arm_root")"
echo "$out"
echo "$out" | grep -q 'relaunch-write_exit=0' || fail 'write should succeed'
echo "$out" | grep -q 'relaunch-verify_exit=7' || fail 'verify failure must stay nonzero'
pass 'verify failure nonzero'

# --- success path ---
mock_reset_logs
MOCK_PHASE_RC_write=0
MOCK_PHASE_RC_verify=0
arm_root="$RUNNER_TEMP/causal-ok"
out="$(run_causal_lifecycle_arm test-arm 1 fixture-causal-test "$arm_root")"
echo "$out"
echo "$out" | grep -q 'relaunch-verify_exit=0' || fail 'success verify rc 0'
[ "$(wc -l <"$MOCK_PHASE_LOG" | tr -d ' ')" -eq 2 ] || fail 'expected write+verify calls'
pass 'success exits 0'

# --- marker/evidence uses variant root not release PERSIST_ROOT ---
mock_reset_logs
MOCK_PHASE_RC_write=0
MOCK_PHASE_RC_verify=0
variant_root="$RUNNER_TEMP/variant-arm-root"
run_causal_lifecycle_arm variant 0 fixture-variant "$variant_root" >/dev/null
[ "$(wc -l <"$MOCK_DUMP_LOG" | tr -d ' ')" -ge 1 ] || fail 'dump_store_evidence must run'
dump_root="$(head -n 1 "$MOCK_DUMP_LOG")"
[ "$dump_root" = "$variant_root" ] || fail "dump root must be variant ($variant_root) got $dump_root"
pass 'dump_store_evidence uses arm root'

# --- retention set only for writer subprocess ---
mock_reset_logs
MOCK_PHASE_RC_write=0
MOCK_PHASE_RC_verify=0
run_causal_lifecycle_arm retain-check 1 fixture-retain "$RUNNER_TEMP/retain-arm" >/dev/null
[ "$(wc -l <"$MOCK_RETAIN_LOG" | tr -d ' ')" -eq 2 ] || fail 'expected two phase snapshots'
writer_retain="$(sed -n '1p' "$MOCK_RETAIN_LOG")"
verify_retain="$(sed -n '2p' "$MOCK_RETAIN_LOG")"
[ "$writer_retain" = "1" ] || fail "writer must see retain=1 got $writer_retain"
[ "$verify_retain" = "unset" ] || fail "verifier must not see retain env got $verify_retain"
pass 'retention writer-only'

# --- release baseline: run_phase failure must propagate (not masked by if !) ---
release_fail_rc=0
set +e
(
  set -euo pipefail
  PERSIST_ROOT="$RUNNER_TEMP/release-fail"
  export PERSIST_ROOT
  MOCK_PHASE_RC_write=99
  run_phase relaunch-write
)
release_fail_rc=$?
set -e
[ "$release_fail_rc" -eq 99 ] || fail "release run_phase failure expected 99 got $release_fail_rc"
pass 'release baseline failure nonzero'

# --- harness must use || for phase rc (not if ! run_phase; then rc=$?) ---
if grep -q 'if ! run_phase' "$ROOT/scripts/ci/browser-persistence-fixture-harness-lib.sh" \
  || grep -q 'if ! run_phase' "$ROOT/scripts/ci/run-macos-browser-persistence-fixture.sh"; then
  fail 'harness must not use if ! run_phase for exit capture'
fi
MOCK_PHASE_RC_write=5
set +e
fixed_rc=0
run_phase relaunch-write || fixed_rc=$?
set -e
[ "$fixed_rc" -eq 5 ] || fail "|| pattern must capture 5 got $fixed_rc"
pass 'exit capture uses || not if !'

# --- production registry core (rustc leaf, no workspace build) ---
registry_bin_dir="$(mktemp -d "$RUNNER_TEMP/registry-bin.XXXXXX")"
registry_test_bin="$registry_bin_dir/browser_store_registry_leaf"
PATH="${PATH:-/usr/local/bin:/usr/bin:/bin}"
rustc --test "$ROOT/scripts/ci/browser_store_registry_leaf.rs" -o "$registry_test_bin"
"$registry_test_bin"
pass 'browser_store_registry_leaf'

retain_env_bin="$registry_bin_dir/browser_persistence_fixture_retain_env"
rustc "$ROOT/scripts/ci/browser_persistence_fixture_retain_env.rs" -o "$retain_env_bin"
"$retain_env_bin"
pass 'browser_persistence_fixture_retain_env'

echo "all browser persistence harness shell regressions passed"
