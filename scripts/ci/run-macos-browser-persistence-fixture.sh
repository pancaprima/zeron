#!/bin/bash
# Native macOS browser persistence acceptance: real process relaunch + WebKit storage.
# usage: run-macos-browser-persistence-fixture.sh <browser-fixture-binary>
set -euo pipefail
ROOT="${ZERON_ROOT:-$(cd "$(dirname "$0")/../.." && pwd)}"
BINARY="${1:?browser-fixture binary required}"
RUNNER_TEMP="${RUNNER_TEMP:-${TMPDIR:-/tmp}}"
PERSIST_ROOT="${ZERON_BROWSER_PERSISTENCE_ROOT:-$RUNNER_TEMP/zeron-browser-persistence-$(date +%s)-$$}"
export ZERON_BROWSER_PERSISTENCE_ROOT="$PERSIST_ROOT"
mkdir -p "$PERSIST_ROOT"

# shellcheck source=browser-persistence-fixture-harness-lib.sh
source "$ROOT/scripts/ci/browser-persistence-fixture-harness-lib.sh"

VERIFY_PENDING=0
on_exit() {
  local rc=$?
  trap - EXIT
  if [ "$VERIFY_PENDING" = 1 ]; then
    ( set +e; dump_store_evidence after-relaunch-verify-failure "$PERSIST_ROOT" ) || true
    ( set +e; capture_webkit_log after-relaunch-verify-failure "$WRITE_SINCE" ) || true
  fi
  exit "$rc"
}

( set +e; record_runner_webkit ) || true
( set +e; run_causal_lifecycle_contrast ) || true
unset ZERON_BROWSER_FIXTURE_RETAIN_WEBSITE_DATA_STORE || true
unset ZERON_BROWSER_PERSISTENCE_DEVICE_A || true
export ZERON_BROWSER_PERSISTENCE_ROOT="$PERSIST_ROOT"
WRITE_SINCE="$(date '+%Y-%m-%d %H:%M:%S')"
echo "acceptance-lifecycle boundary phase=release-harness fixture_lifecycle=production-store-registry retain_env=unset root=$PERSIST_ROOT"
run_phase relaunch-write
test -f "$PERSIST_ROOT/relaunch-marker.json"
( set +e; dump_store_evidence after-relaunch-write-exit "$PERSIST_ROOT" ) || true
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
