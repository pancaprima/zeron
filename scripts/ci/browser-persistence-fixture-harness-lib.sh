# Shared helpers for macOS browser persistence acceptance (sourced by CI harness + shell tests).
# Expects: ROOT, BINARY, RUNNER_TEMP; sets PERSIST_ROOT when sourced from main harness.

browser_persistence_fixture_retain_enabled() {
  [ "${ZERON_BROWSER_FIXTURE_RETAIN_WEBSITE_DATA_STORE:-}" = "1" ]
}

run_phase() {
  local phase="$1"
  local captures="$RUNNER_TEMP/browser-persistence-captures/$phase"
  export ZERON_BROWSER_PERSISTENCE_PHASE="$phase"
  unset ZERON_BROWSER_FIXTURE_INJECT_CLEAR_ERROR || true
  if [ "$phase" = "clear-failure" ]; then
    export ZERON_BROWSER_FIXTURE_INJECT_CLEAR_ERROR=1
  fi
  if [ -f "$PERSIST_ROOT/relaunch-marker.json" ]; then
    export ZERON_BROWSER_PERSISTENCE_PORT="$(
      python3 -c "import json; print(json.load(open('$PERSIST_ROOT/relaunch-marker.json'))['port'])"
    )"
  else
    unset ZERON_BROWSER_PERSISTENCE_PORT || true
  fi
  if [ "$phase" = "relaunch-write" ]; then
    rm -f "$PERSIST_ROOT/relaunch-marker.json"
  fi
  rm -rf "$captures"
  mkdir -p "$captures"
  set +e
  local wrapper_rc=0
  "$ROOT/scripts/ci/run-macos-fixture.sh" "$BINARY" "$captures"
  wrapper_rc=$?
  set -e
  if [ "$wrapper_rc" -ne 0 ]; then
    rm -f "$captures/persistence-result.txt" "$captures/result.txt"
    if [ "$phase" = "relaunch-write" ]; then
      rm -f "$PERSIST_ROOT/relaunch-marker.json"
    fi
    return "$wrapper_rc"
  fi
  if [ ! -f "$captures/persistence-result.txt" ]; then
    rm -f "$captures/persistence-result.txt" "$captures/result.txt"
    if [ "$phase" = "relaunch-write" ]; then
      rm -f "$PERSIST_ROOT/relaunch-marker.json"
    fi
    echo "phase $phase missing persistence-result.txt after wrapper success" >&2
    return 1
  fi
  echo "phase $phase passed"
}

# Fixture-only evidence. Never prints cookie or storage values, never fails the job.
FIXTURE_BUNDLE_ID="sh.zeron.browser-fixture"
FIXTURE_COOKIE_NAME="zeron_persist_fixture"
EVIDENCE_DIR="$RUNNER_TEMP/browser-captures-persistence-evidence"

record_runner_webkit() {
  echo "evidence runner: $(sw_vers 2>/dev/null | tr '\n' ' ')"
  local plist
  for plist in \
    /System/Library/Frameworks/WebKit.framework/Resources/Info.plist \
    /System/Library/Frameworks/WebKit.framework/Versions/A/Resources/Info.plist; do
    if [ -f "$plist" ]; then
      echo "evidence webkit: $(/usr/libexec/PlistBuddy -c 'Print :CFBundleVersion' "$plist" 2>/dev/null || echo unknown) ($plist)"
      break
    fi
  done
  echo "evidence safari: $(/usr/libexec/PlistBuddy -c 'Print :CFBundleShortVersionString' /Applications/Safari.app/Contents/Info.plist 2>/dev/null || echo absent)"
}

describe_path() {
  local label="$1" path="$2"
  if [ -e "$path" ]; then
    echo "evidence $label: exists size=$(stat -f '%z' "$path") mtime=$(stat -f '%Sm' -t '%Y-%m-%dT%H:%M:%S%z' "$path") path=$path"
  else
    echo "evidence $label: absent path=$path"
  fi
}

dump_store_evidence() {
  local stage="$1"
  local evidence_root="${2:-$PERSIST_ROOT}"
  echo "evidence stage=$stage evidence_root=$evidence_root now=$(date '+%Y-%m-%dT%H:%M:%S%z')"
  local marker="$evidence_root/relaunch-marker.json" uuid=""
  if [ -f "$marker" ]; then
    uuid="$(python3 -c "import json,re,sys; m=re.search(r'store_uuid=(\S+)', json.load(open(sys.argv[1])).get('profileDiag') or ''); print(m.group(1).upper() if m else '')" "$marker" 2>/dev/null || true)"
  fi
  local bundle_dir="$HOME/Library/WebKit/$FIXTURE_BUNDLE_ID"
  describe_path bundle-dir "$bundle_dir"
  if [ -z "$uuid" ]; then
    echo "evidence store: store_uuid unavailable (marker missing or profileDiag has no store_uuid)"
    return 0
  fi
  local store="$bundle_dir/WebsiteDataStore/$uuid"
  describe_path store-dir "$store"
  local cookies="$store/Cookies/Cookies.binarycookies"
  describe_path cookies-file "$cookies"
  if [ -f "$cookies" ]; then
    echo "evidence cookie-name-matches: $(LC_ALL=C grep -a -o -F "$FIXTURE_COOKIE_NAME" "$cookies" | wc -l | tr -d ' ') name=$FIXTURE_COOKIE_NAME"
  fi
  describe_path localstorage-dir "$store/LocalStorage"
  describe_path origins-dir "$store/Origins"
  if [ -d "$store" ]; then
    find "$store" -maxdepth 3 -exec stat -f 'evidence store-entry: size=%z mtime=%Sm %N' -t '%H:%M:%S' {} + 2>/dev/null \
      | head -n 60 || true
  fi
}

capture_webkit_log() {
  local stage="$1" since="$2"
  mkdir -p "$EVIDENCE_DIR"
  local out="$EVIDENCE_DIR/webkit-log-$stage.txt"
  if ! perl -e 'alarm shift; exec @ARGV' 60 \
    log show --style compact --info --start "$since" \
    --predicate 'subsystem == "com.apple.WebKit"' >"$out" 2>&1; then
    echo "evidence webkit-log $stage: log show failed or timed out (partial output kept)"
  fi
  echo "evidence webkit-log $stage: $(wc -l <"$out" | tr -d ' ') lines saved to $out"
  grep -E 'destroySession|~WebsiteDataStore|WebsiteDataStore::~|unacknowledged closed-connection|flushCookies|Cookies\.binarycookies' "$out" \
    | head -n 80 | sed 's/^/evidence webkit-log: /' || true
}

# Run one persistence phase in an isolated bash subprocess with explicit root/device env.
# Retention env is set only when retain_writer=1 and phase=relaunch-write.
run_causal_lifecycle_phase_subprocess() {
  local phase="$1"
  local arm_root="$2"
  local arm_device="$3"
  local retain_writer="$4"
  (
    set -euo pipefail
    export ZERON_BROWSER_PERSISTENCE_ROOT="$arm_root"
    export ZERON_BROWSER_PERSISTENCE_DEVICE_A="$arm_device"
    PERSIST_ROOT="$arm_root"
    export PERSIST_ROOT
    if [ "$retain_writer" = 1 ] && [ "$phase" = "relaunch-write" ]; then
      export ZERON_BROWSER_FIXTURE_RETAIN_WEBSITE_DATA_STORE=1
    else
      unset ZERON_BROWSER_FIXTURE_RETAIN_WEBSITE_DATA_STORE || true
    fi
    if [ "${ZERON_BROWSER_PERSISTENCE_CAUSAL_DIAG:-}" = "1" ]; then
      echo "causal-lifecycle subprocess phase=$phase retain_writer=$retain_writer root=$arm_root device=$arm_device retain_env=${ZERON_BROWSER_FIXTURE_RETAIN_WEBSITE_DATA_STORE:-unset}"
    fi
    run_phase "$phase"
  )
}

run_causal_lifecycle_arm() {
  local arm="$1"
  local retain="$2"
  local device="$3"
  local root="$4"
  mkdir -p "$root"
  echo "causal-lifecycle boundary arm=$arm retain_datastore=$retain device=$device root=$root"
  local write_rc=0 verify_rc=0 verify_label="skipped"
  run_causal_lifecycle_phase_subprocess relaunch-write "$root" "$device" "$retain" || write_rc=$?
  ( set +e; dump_store_evidence "causal-${arm}-after-relaunch-write-exit" "$root" ) || true
  if [ "$write_rc" -eq 0 ]; then
    run_causal_lifecycle_phase_subprocess relaunch-verify "$root" "$device" 0 || verify_rc=$?
    verify_label="$verify_rc"
  fi
  echo "causal-lifecycle outcome arm=$arm relaunch-write_exit=$write_rc relaunch-verify_exit=$verify_label (diagnostic only; not release acceptance)"
  unset ZERON_BROWSER_FIXTURE_RETAIN_WEBSITE_DATA_STORE || true
  unset ZERON_BROWSER_PERSISTENCE_DEVICE_A || true
}

RELEASE_VERIFY_PENDING=0
release_acceptance_on_exit() {
  local rc=$?
  trap - EXIT
  if [ "$RELEASE_VERIFY_PENDING" = 1 ]; then
    ( set +e; dump_store_evidence after-relaunch-verify-failure "$PERSIST_ROOT" ) || true
    ( set +e; capture_webkit_log after-relaunch-verify-failure "$RELEASE_WRITE_SINCE" ) || true
  fi
  exit "$rc"
}

# Release acceptance; job status. Each run_phase is a fresh fixture process. Call under `set -e`
# (not in an `if`/`||` context) so the first failing phase aborts the run with its exit status.
# clear-verify is read-only and sits between clear-confirm and clear-failure (which writes again).
run_release_acceptance() {
  unset ZERON_BROWSER_FIXTURE_RETAIN_WEBSITE_DATA_STORE || true
  unset ZERON_BROWSER_PERSISTENCE_DEVICE_A || true
  export ZERON_BROWSER_PERSISTENCE_ROOT="$PERSIST_ROOT"
  RELEASE_WRITE_SINCE="$(date '+%Y-%m-%d %H:%M:%S')"
  echo "acceptance-lifecycle boundary phase=release-harness fixture_lifecycle=production-store-registry retain_env=unset root=$PERSIST_ROOT"
  run_phase relaunch-write
  test -f "$PERSIST_ROOT/relaunch-marker.json"
  ( set +e; dump_store_evidence after-relaunch-write-exit "$PERSIST_ROOT" ) || true
  RELEASE_VERIFY_PENDING=1
  trap release_acceptance_on_exit EXIT
  run_phase relaunch-verify
  RELEASE_VERIFY_PENDING=0
  ( set +e; capture_webkit_log after-relaunch-verify-success "$RELEASE_WRITE_SINCE" ) || true
  run_phase isolation
  run_phase clear-cancel
  run_phase clear-confirm
  run_phase clear-verify
  run_phase clear-failure
}

run_causal_lifecycle_contrast() {
  echo "causal-lifecycle boundary phase=contrast-start"
  local ts="$$"
  run_causal_lifecycle_arm baseline 0 fixture-causal-baseline "$RUNNER_TEMP/zeron-causal-lifecycle-baseline-$ts"
  run_causal_lifecycle_arm retain-datastore 1 fixture-causal-retain "$RUNNER_TEMP/zeron-causal-lifecycle-retain-$ts"
  echo "causal-lifecycle boundary phase=contrast-end interpretation=production uses process store registry; baseline arm no longer expects verify failure. If retain arm still passes with extra fixture forget, redundant pin is harmless. Historical: pre-registry baseline verify failed while retain passed (store-teardown hypothesis)."
}
