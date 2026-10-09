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

run_phase relaunch-write
test -f "$PERSIST_ROOT/relaunch-marker.json"
run_phase relaunch-verify
run_phase isolation
run_phase clear-cancel
run_phase clear-confirm
run_phase clear-failure

echo "browser persistence acceptance harness finished (root=$PERSIST_ROOT)"
