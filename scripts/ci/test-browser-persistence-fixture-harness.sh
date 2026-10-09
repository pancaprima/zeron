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

HARNESS_LIB="$ROOT/scripts/ci/browser-persistence-fixture-harness-lib.sh"
# shellcheck source=browser-persistence-fixture-harness-lib.sh
source "$HARNESS_LIB"

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
MOCK_DEVICE_LOG="$RUNNER_TEMP/mock-device.log"
MOCK_STAGE_LOG="$RUNNER_TEMP/mock-stage.log"
MOCK_FAIL_PHASE=""
MOCK_FAIL_RC=0

mock_reset_logs() {
  : >"$MOCK_PHASE_LOG"
  : >"$MOCK_DUMP_LOG"
  : >"$MOCK_RETAIN_LOG"
  : >"$MOCK_DEVICE_LOG"
  : >"$MOCK_STAGE_LOG"
}

run_phase() {
  local phase="$1"
  echo "$phase" >>"$MOCK_PHASE_LOG"
  echo "${ZERON_BROWSER_FIXTURE_RETAIN_WEBSITE_DATA_STORE:-unset}" >>"$MOCK_RETAIN_LOG"
  echo "${ZERON_BROWSER_PERSISTENCE_DEVICE_A:-unset}" >>"$MOCK_DEVICE_LOG"
  if [ -n "$MOCK_FAIL_PHASE" ] && [ "$phase" = "$MOCK_FAIL_PHASE" ]; then
    return "$MOCK_FAIL_RC"
  fi
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
  echo "dump $stage" >>"$MOCK_STAGE_LOG"
  echo "mock-dump stage=$stage root=$evidence_root"
}

capture_webkit_log() {
  echo "log $1" >>"$MOCK_STAGE_LOG"
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
MOCK_PHASE_RC_write=0
MOCK_PHASE_RC_verify=0

# --- release acceptance: exact phase order, read-only clear-verify between confirm and failure ---
RELEASE_PHASES='relaunch-write
relaunch-verify
isolation
clear-cancel
clear-confirm
clear-verify
clear-failure'

# Runs run_release_acceptance as the runner does (set -e, not in an if/|| context).
run_release_in_subshell() {
  local root="$1"
  set +e
  (
    set -euo pipefail
    PERSIST_ROOT="$root"
    export PERSIST_ROOT
    export ZERON_BROWSER_FIXTURE_RETAIN_WEBSITE_DATA_STORE=1
    export ZERON_BROWSER_PERSISTENCE_DEVICE_A=fixture-causal-retain
    run_release_acceptance
  )
  release_rc=$?
  set -e
}

mock_reset_logs
MOCK_FAIL_PHASE=""
run_release_in_subshell "$RUNNER_TEMP/release-order"
[ "$release_rc" -eq 0 ] || fail "release acceptance expected rc 0 got $release_rc"
[ "$(cat "$MOCK_PHASE_LOG")" = "$RELEASE_PHASES" ] \
  || fail "release phase order mismatch: $(tr '\n' ' ' <"$MOCK_PHASE_LOG")"
pass 'release order relaunch-write..clear-confirm -> clear-verify -> clear-failure'

[ "$(sort -u "$MOCK_RETAIN_LOG")" = "unset" ] \
  || fail "release phases must run without retention env: $(tr '\n' ' ' <"$MOCK_RETAIN_LOG")"
[ "$(sort -u "$MOCK_DEVICE_LOG")" = "unset" ] \
  || fail "release phases must run on the default device: $(tr '\n' ' ' <"$MOCK_DEVICE_LOG")"
pass 'release acceptance unsets inherited retention and causal device env for every phase'

grep -qx 'dump after-relaunch-write-exit' "$MOCK_STAGE_LOG" || fail 'post-writer evidence dump must run'
grep -qx 'log after-relaunch-verify-success' "$MOCK_STAGE_LOG" || fail 'post-verify log capture must run'
pass 'release evidence hooks run on success'

# Failure in any phase stops the sequence and becomes the release exit status.
expect_release_stops_at() {
  local failing="$1" rc="$2"
  mock_reset_logs
  MOCK_FAIL_PHASE="$failing"
  MOCK_FAIL_RC="$rc"
  run_release_in_subshell "$RUNNER_TEMP/release-fail-$failing"
  MOCK_FAIL_PHASE=""
  [ "$release_rc" -eq "$rc" ] || fail "$failing failure expected rc $rc got $release_rc"
  [ "$(tail -n 1 "$MOCK_PHASE_LOG")" = "$failing" ] \
    || fail "sequence must stop at $failing: $(tr '\n' ' ' <"$MOCK_PHASE_LOG")"
  local expected_prefix
  expected_prefix="$(printf '%s\n' "$RELEASE_PHASES" | sed "/^$failing\$/q")"
  [ "$(cat "$MOCK_PHASE_LOG")" = "$expected_prefix" ] \
    || fail "phases before $failing must run in order: $(tr '\n' ' ' <"$MOCK_PHASE_LOG")"
}
expect_release_stops_at clear-confirm 11
log_contains clear-verify "$MOCK_PHASE_LOG" && fail 'clear-verify must not run after clear-confirm failure'
pass 'clear-confirm failure propagates and skips clear-verify'
expect_release_stops_at clear-verify 13
log_contains clear-failure "$MOCK_PHASE_LOG" && fail 'clear-failure must not run after clear-verify failure'
pass 'clear-verify failure propagates and skips clear-failure'
expect_release_stops_at clear-failure 17
pass 'clear-failure failure propagates'
expect_release_stops_at relaunch-verify 19
grep -qx 'dump after-relaunch-verify-failure' "$MOCK_STAGE_LOG" || fail 'verify failure must dump evidence'
grep -qx 'log after-relaunch-verify-failure' "$MOCK_STAGE_LOG" || fail 'verify failure must capture webkit log'
pass 'relaunch-verify failure keeps rc 19 after evidence trap'

grep -qx 'run_release_acceptance' "$ROOT/scripts/ci/run-macos-browser-persistence-fixture.sh" \
  || fail 'runner must call run_release_acceptance directly (job status, not masked)'
pass 'runner gates on run_release_acceptance'

# --- real run_phase: each phase is a separate wrapper process; clear-verify is read-only ---
stub_root="$RUNNER_TEMP/stub-root"
mkdir -p "$stub_root/scripts/ci"
cat >"$stub_root/scripts/ci/run-macos-fixture.sh" <<'STUB'
#!/bin/bash
set -euo pipefail
echo "$ZERON_BROWSER_PERSISTENCE_PHASE inject=${ZERON_BROWSER_FIXTURE_INJECT_CLEAR_ERROR:-unset} retain=${ZERON_BROWSER_FIXTURE_RETAIN_WEBSITE_DATA_STORE:-unset} port=${ZERON_BROWSER_PERSISTENCE_PORT:-unset}" >>"$STUB_LOG"
if [ "$ZERON_BROWSER_PERSISTENCE_PHASE" = "${STUB_FAIL_PHASE:-}" ]; then
  exit "$STUB_FAIL_RC"
fi
if [ "$ZERON_BROWSER_PERSISTENCE_PHASE" = "${STUB_NO_RESULT_PHASE:-}" ]; then
  exit 0
fi
echo "PASS: stub $ZERON_BROWSER_PERSISTENCE_PHASE" >"$2/persistence-result.txt"
echo "PASS: stub $ZERON_BROWSER_PERSISTENCE_PHASE" >"$2/result.txt"
STUB
chmod +x "$stub_root/scripts/ci/run-macos-fixture.sh"
STUB_LOG="$RUNNER_TEMP/stub.log"
export STUB_LOG
: >"$STUB_LOG"

real_root="$RUNNER_TEMP/real-run-phase-root"
mkdir -p "$real_root"
echo '{"port":4321,"origin":"http://127.0.0.1:4321","profileDiag":"store_uuid=MOCK"}' >"$real_root/relaunch-marker.json"
cp "$real_root/relaunch-marker.json" "$RUNNER_TEMP/marker-before.json"

real_rc=0
set +e
(
  set -euo pipefail
  ROOT="$stub_root"
  PERSIST_ROOT="$real_root"
  unset ZERON_BROWSER_FIXTURE_RETAIN_WEBSITE_DATA_STORE || true
  # shellcheck source=browser-persistence-fixture-harness-lib.sh
  source "$HARNESS_LIB"
  run_phase clear-confirm
  run_phase clear-verify
  run_phase clear-failure
)
real_rc=$?
set -e
[ "$real_rc" -eq 0 ] || fail "real run_phase sequence expected 0 got $real_rc"
[ "$(cut -d' ' -f1 "$STUB_LOG" | tr '\n' ' ')" = "clear-confirm clear-verify clear-failure " ] \
  || fail "each phase must launch its own fixture wrapper once: $(tr '\n' '|' <"$STUB_LOG")"
grep -qx 'clear-verify inject=unset retain=unset port=4321' "$STUB_LOG" \
  || fail "clear-verify must reuse marker port without injected error: $(tr '\n' '|' <"$STUB_LOG")"
grep -qx 'clear-failure inject=1 retain=unset port=4321' "$STUB_LOG" \
  || fail "clear-failure must inject error on marker port: $(tr '\n' '|' <"$STUB_LOG")"
grep -qx 'clear-confirm inject=unset retain=unset port=4321' "$STUB_LOG" \
  || fail "clear-confirm must not inject error: $(tr '\n' '|' <"$STUB_LOG")"
cmp -s "$real_root/relaunch-marker.json" "$RUNNER_TEMP/marker-before.json" \
  || fail 'clear phases must keep the relaunch marker (origin/port) unchanged'
pass 'real run_phase: fresh wrapper per phase, clear-verify reuses marker origin without injection'

expect_real_phase_rc() {
  local want="$1"
  : >"$STUB_LOG"
  set +e
  (
    set -euo pipefail
    ROOT="$stub_root"
    PERSIST_ROOT="$real_root"
    # shellcheck source=browser-persistence-fixture-harness-lib.sh
    source "$HARNESS_LIB"
    run_phase clear-verify
  )
  real_rc=$?
  set -e
  [ "$real_rc" -eq "$want" ] || fail "real clear-verify expected rc $want got $real_rc"
  [ ! -e "$RUNNER_TEMP/browser-persistence-captures/clear-verify/persistence-result.txt" ] \
    || fail 'failed clear-verify must not leave a persistence result'
}
export STUB_FAIL_PHASE=clear-verify STUB_FAIL_RC=23
expect_real_phase_rc 23
unset STUB_FAIL_PHASE STUB_FAIL_RC
pass 'real clear-verify wrapper failure propagates its exit status'
export STUB_NO_RESULT_PHASE=clear-verify
expect_real_phase_rc 1
unset STUB_NO_RESULT_PHASE
pass 'real clear-verify without persistence-result.txt fails'

# --- fixture clear-verify arm: read-only, reuses marker origin, nonce-protected probes ---
fixture_src="$ROOT/crates/ui/examples/browser-fixture/persistence_harness.rs"
clear_verify_arm="$(awk '/^        "clear-verify" => /{on=1} /^        "clear-failure" => /{on=0} on' "$fixture_src")"
[ -n "$clear_verify_arm" ] || fail 'fixture must define a clear-verify phase'
for required in \
  'require_relaunch_marker(root)?' \
  'ensure_loopback_origin(&site, &marker)?' \
  'expect_storage(window, &browser_a, origin, false, cx)' \
  'fixture_set_local_device_id(DEVICE_B, cx)' \
  'expect_storage(window, &browser_b, origin, true, cx)' \
  'ZERON_BROWSER_FIXTURE_INJECT_CLEAR_ERROR").is_none()'; do
  printf '%s\n' "$clear_verify_arm" | grep -qF "$required" || fail "clear-verify arm missing: $required"
done
for forbidden in write_storage persist_write_script fixture_eval fixture_browser_clear_dialog \
  fixture_confirm_browser_clear write_relaunch_marker remove_file; do
  printf '%s\n' "$clear_verify_arm" | grep -qF "$forbidden" && fail "clear-verify arm must be read-only: found $forbidden"
done
# A before B: absent check on the cleared identity precedes the intact-identity check.
a_line="$(printf '%s\n' "$clear_verify_arm" | grep -nF 'expect_storage(window, &browser_a, origin, false, cx)' | cut -d: -f1)"
b_line="$(printf '%s\n' "$clear_verify_arm" | grep -nF 'expect_storage(window, &browser_b, origin, true, cx)' | cut -d: -f1)"
[ "$a_line" -lt "$b_line" ] || fail 'clear-verify must check identity A before identity B'
expect_storage_fn="$(awk '/^async fn expect_storage\(/{on=1} on{print} on&&/^}/{exit}' "$fixture_src")"
printf '%s\n' "$expect_storage_fn" | grep -qF 'let probe_nonce = persistence_probe::new_probe_nonce();' \
  || fail 'expect_storage must mint a fresh probe nonce'
printf '%s\n' "$expect_storage_fn" | grep -qF 'persist_read_probe_script(&probe_nonce)' \
  || fail 'expect_storage must embed the nonce in the read-only probe'
printf '%s\n' "$expect_storage_fn" | grep -qF '&probe_nonce,' \
  || fail 'expect_storage must wait for the same nonce'
printf '%s\n' "$expect_storage_fn" | grep -qF 'persist_write_script' \
  && fail 'expect_storage must not use the write script'
pass 'fixture clear-verify is read-only with nonce-protected probes'

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
