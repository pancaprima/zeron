#!/bin/bash
# Native macOS browser persistence acceptance: real process relaunch + WebKit storage.
# usage: run-macos-browser-persistence-fixture.sh <browser-fixture-binary>
set -euo pipefail
ROOT="${ZERON_ROOT:-$(cd "$(dirname "$0")/../.." && pwd)}"
BINARY="${1:?browser-fixture binary required}"
PERSIST_ROOT="${ZERON_BROWSER_PERSISTENCE_ROOT:-$RUNNER_TEMP/zeron-browser-persistence-$(date +%s)-$$}"
export ZERON_BROWSER_PERSISTENCE_ROOT="$PERSIST_ROOT"
mkdir -p "$PERSIST_ROOT"

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
  if ! "$ROOT/scripts/ci/run-macos-fixture.sh" "$BINARY" "$captures"; then
    rm -f "$captures/persistence-result.txt" "$captures/result.txt"
    if [ "$phase" = "relaunch-write" ]; then
      rm -f "$PERSIST_ROOT/relaunch-marker.json"
    fi
    return 1
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
  echo "evidence stage=$stage now=$(date '+%Y-%m-%dT%H:%M:%S%z')"
  local marker="$PERSIST_ROOT/relaunch-marker.json" uuid=""
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
  # Hard 60 s alarm: `log show` must never hang the job.
  if ! perl -e 'alarm shift; exec @ARGV' 60 \
    log show --style compact --info --start "$since" \
    --predicate 'subsystem == "com.apple.WebKit"' >"$out" 2>&1; then
    echo "evidence webkit-log $stage: log show failed or timed out (partial output kept)"
  fi
  echo "evidence webkit-log $stage: $(wc -l <"$out" | tr -d ' ') lines saved to $out"
  grep -E 'destroySession|~WebsiteDataStore|WebsiteDataStore::~|unacknowledged closed-connection|flushCookies|Cookies\.binarycookies' "$out" \
    | head -n 80 | sed 's/^/evidence webkit-log: /' || true
}

VERIFY_PENDING=0
on_exit() {
  local rc=$?
  trap - EXIT
  if [ "$VERIFY_PENDING" = 1 ]; then
    ( set +e; dump_store_evidence after-relaunch-verify-failure; capture_webkit_log after-relaunch-verify-failure "$WRITE_SINCE" ) || true
  fi
  exit "$rc"
}

( set +e; record_runner_webkit ) || true
WRITE_SINCE="$(date '+%Y-%m-%d %H:%M:%S')"
run_phase relaunch-write
test -f "$PERSIST_ROOT/relaunch-marker.json"
# Metadata only: log capture here would delay relaunch-verify by up to 60 s.
( set +e; dump_store_evidence after-relaunch-write-exit ) || true
VERIFY_PENDING=1
trap on_exit EXIT
run_phase relaunch-verify
VERIFY_PENDING=0
( set +e; capture_webkit_log after-relaunch-verify-success "$WRITE_SINCE" ) || true
run_phase isolation
run_phase clear-cancel
run_phase clear-confirm
run_phase clear-failure

echo "browser persistence acceptance harness finished (root=$PERSIST_ROOT)"
