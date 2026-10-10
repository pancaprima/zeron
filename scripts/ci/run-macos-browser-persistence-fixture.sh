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

( set +e; record_runner_webkit ) || true
( set +e; run_causal_lifecycle_contrast ) || true
run_release_acceptance

echo "browser persistence acceptance harness finished (root=$PERSIST_ROOT)"
