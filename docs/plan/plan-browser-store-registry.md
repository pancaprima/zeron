# Plan: macOS persistent `WKWebsiteDataStore` process registry (approved)

Status: **implemented on `feat/browser-persistence`** — production fix for cookie loss across process restart when GPUI/browser teardown drops the last web-view reference before WebKit flushes HTTP cookies. Native macOS acceptance still required in CI after parent review.

## Problem

Evidence (see `plan-browser-restart-diagnostics.md`):

- `relaunch-write` passes in-process; `relaunch-verify` in a **new process** often fails with cookie missing while localStorage survives.
- WebKit logs show `WebsiteDataStore::~WebsiteDataStore` / `NetworkProcess::destroySession` for the profile UUID during writer teardown **before** process exit.
- Fixture-only `mem::forget` retention arm (`ZERON_BROWSER_FIXTURE_RETAIN_WEBSITE_DATA_STORE=1`) lets cookies reach disk and verify passes in a new process — causal support for **store released too early in-process**, not a missing `dataStoreForIdentifier` identity.

Linux/Windows already keep profile storage alive via explicit manager directories and environment lifetime; macOS uses public `dataStoreForIdentifier` only (no custom `webkit-data` paths in production).

## Approved architecture

| Requirement | Approach |
|-------------|----------|
| Pin stores for process lifetime | `RegistryCore` (`browser_store_registry_core.rs`) maps `[u8; 16]` → `Retained<WKWebsiteDataStore>` via `RefCell<HashMap<…>>`; one leaked `Box` on the main thread, TLS holds `&'static` only (no map destructor on thread exit) |
| Reuse same store | `dataStoreForIdentifier` only on first open per UUID in-process; later `BrowserContext` / tab teardown reuses the pinned `Retained` |
| Workspace/profile isolation | Distinct locators → distinct UUIDs → distinct map entries; deferred mode never enters the registry |
| Public API only | `+[WKWebsiteDataStore dataStoreForIdentifier:]`, `removeDataOfTypes:modifiedSince:completionHandler:` for clear |
| No fake persistence | No cookie file copy, no default-store fallback, no weakened fixture assertions |
| Main-thread safety | Registry accessed only on the AppKit thread; `MainThreadMarker` required at the native open boundary; no `Send`/`Sync` on ObjC handles |
| Reentrancy | `RegistryCore::get_or_open` uses short `RefCell` borrows only; no borrow across `open`; nested open picks the already-inserted canonical store for the same key (std-only, no `unsafe`) |

**Out of scope:** cross-process registry (verify still loads disk by UUID in a new process), private `_flushCookiesToDisk`, per-profile custom WebKit directories on macOS.

## Clear and cancel lifecycle

| Action | Behavior with registry |
|--------|-------------------------|
| **Confirm clear** | `clear_website_data` calls `ensure_store` → same pinned store → `removeDataOfTypes` with `allWebsiteDataTypes` and `distantPast`. Registry entry **remains**; only website data is removed. Reloaded tabs must not resurrect cleared cookies/storage from an old in-memory session on the same store instance. |
| **Cancel clear** | UI closes confirmation without calling `clear_website_data`; registry and store unchanged (fixture `clear-cancel` phase). |
| **Clear persisted** | Fixture `clear-verify` phase, a **new process** right after `clear-confirm`, read-only: identity A shows cookie and localStorage absent, identity B shows both present (B was written in `clear-confirm`). Proves the clear and B's isolation reached disk, not only the in-process store. |
| **Clear failure** | Fixture-injected error returns before WebKit; registry unchanged; data preserved (`clear-failure` phase). Runs after `clear-verify` because it writes A again. |
| **Profile switch** | New `BrowserContext` with different locator uses a different UUID key; prior profile's pin stays in the map until process exit (intended — avoids teardown flush races if user switches back). |

## Tradeoffs

- **Memory:** One bounded leak for the registry container (`Box::leak`); plus one WebKit store (and its network session) per persistent profile UUID opened in the process until exit — acceptable for typical single-workspace use; map growth if many distinct locators are opened in one run (same as opening that many stores without release). Pins are **not** freed at profile switch or tab close by design.
- **Privacy:** Pins do not copy data across profiles; they only delay WebKit session destruction for a known UUID.
- **Diagnostics:** Causal A/B (`baseline` vs `retain-datastore`) no longer expects baseline verify to fail once production registry ships; contrast still useful as registry-only vs registry+extra `mem::forget` (see diagnostics plan).

## Implementation map

| File | Role |
|------|------|
| [`crates/ui/src/browser/browser_store_registry_core.rs`](../../crates/ui/src/browser/browser_store_registry_core.rs) | Std-only `RegistryCore`, shared with CI leaf tests |
| [`crates/ui/src/browser/macos_store_registry.rs`](../../crates/ui/src/browser/macos_store_registry.rs) | Leaked main-thread registry, `get_or_open_persistent_store` |
| `crates/ui/src/browser/macos.rs` | Persistent `open_store` / `ensure_store` integration; clear unchanged path via `ensure_store` |
| `crates/ui/src/browser/macos_fixture_store_retention.rs` | Optional extra pin for causal experiment only; env gate in std-only `fixture_store_retention_env.rs` (path include, shared with CI leaf) |
| `crates/ui/examples/browser-fixture/persistence_harness.rs` | Fixture phases, including read-only `clear-verify` |
| `scripts/ci/browser_store_registry_leaf.rs` | `rustc --test` against production `RegistryCore` (path include) |
| `scripts/ci/browser_persistence_fixture_retain_env.rs` | `rustc` leaf against production retention env gate (path include) |
| `scripts/ci/browser-persistence-fixture-harness-lib.sh` | `run_phase`, evidence helpers, causal contrast, `run_release_acceptance` (release phase order) |
| `scripts/ci/run-macos-browser-persistence-fixture.sh` | Runner: evidence + causal contrast under `set +e`, then `run_release_acceptance` sets job status |
| `scripts/ci/test-browser-persistence-fixture-harness.sh` | Harness bash regressions + both rustc leaves |

## Testing

### CI gates

**`macOS tests` / `macos-native`, step "Browser persistence harness, store registry core and retention env gate"** runs `bash scripts/ci/test-browser-persistence-fixture-harness.sh` after the Rust toolchain step and before rust-cache / the workspace build (rustc only, seconds). It triggers on `crates/**`, `.github/workflows/macos.yml`, and each harness script / leaf (`browser-persistence-fixture-harness-lib.sh`, `test-browser-persistence-fixture-harness.sh`, `browser_store_registry_leaf.rs`, `browser_persistence_fixture_retain_env.rs`, `run-macos-browser-persistence-fixture.sh`) in both `pull_request` and `push` path filters. It covers:

- `rustc --test scripts/ci/browser_store_registry_leaf.rs` — production `RegistryCore`: reuse, isolation, caller-drop pin, reentrant open, clear does not evict (core proof only).
- `rustc scripts/ci/browser_persistence_fixture_retain_env.rs` — production retention env gate (`1` only).
- Release order is exactly `relaunch-write → relaunch-verify → isolation → clear-cancel → clear-confirm → clear-verify → clear-failure`. A failure in any phase stops the sequence and becomes the exit status (clear-confirm, clear-verify, clear-failure, relaunch-verify; the verify-failure evidence trap keeps the rc). Inherited `ZERON_BROWSER_FIXTURE_RETAIN_WEBSITE_DATA_STORE` / `ZERON_BROWSER_PERSISTENCE_DEVICE_A` are unset for every release phase.
- Real `run_phase` against a stub wrapper: one wrapper process per phase. `clear-verify` gets the marker port and no injected error, `clear-failure` gets the injected error, and the marker stays unchanged. Wrapper failure and a missing `persistence-result.txt` fail `clear-verify`.
- Structural guard on the fixture `clear-verify` arm: marker/origin reuse, A-absent before B-present, injected-error guard, and no write/clear/marker calls. `expect_storage` mints a fresh nonce and waits for that same nonce.
- Causal-contrast regressions (writer-only retention env, arm roots, rc propagation).

Local: same script, plus `bash -n` on `scripts/ci/*.sh`, `rustfmt --check` on touched Rust when available, `git diff --check`.

**`Native browser persistence acceptance`** (same job, after build) — `scripts/ci/run-macos-browser-persistence-fixture.sh`; release phases set job status.

### Disk layout evidence is diagnostic, not a gate

`dump_store_evidence` reads `~/Library/WebKit/<bundle-id>/WebsiteDataStore/<UUID>/Cookies/Cookies.binarycookies` and counts raw cookie-name bytes. It stays **diagnostic only** (never fails the job, never prints values):

- The path is an undocumented WebKit layout, not public API, and may differ by macOS/WebKit version.
- The snapshot runs right after the UI process exits. Cookie flushing happens in WebKit's network process on connection close and is not synchronized with the harness, so "absent" can be a timing artifact.
- The pre-registry A/B run did produce positive corroboration in the fixture-retained arm: the cookie file appeared, the fixture cookie name matched, and a new process verified both storage types. The baseline lacked the file and lost the cookie. This validates the probe for that runner/layout, not a stable cross-version WebKit contract; keep the behavioral restart checks authoritative.

**Authoritative acceptance** is native behavior in a new process: `relaunch-verify` (cookie **and** localStorage present, read through nonce-protected probes) and `clear-verify` (A absent, B present). Treat disk evidence as optional corroboration when reading logs. Revisit a gate only after several CI runs show `cookie-name-matches: ≥1` after writer exit.

### Native macOS (required gates — not run on VPS)

- [ ] Release harness: `relaunch-write` / `relaunch-verify` pass **without** `ZERON_BROWSER_FIXTURE_RETAIN_WEBSITE_DATA_STORE`.
- [ ] `isolation`, `clear-cancel`, `clear-confirm`, `clear-verify` (fresh process, read-only), `clear-failure` phases pass.
- [ ] Record (optional, non-gating) post-write disk evidence: `cookies-file` / `cookie-name-matches` after writer exit.
- [ ] Causal contrast: document outcomes (both arms may pass verify); retain arm still writer-only env.
- [ ] Real app: profile switch isolation; Google/HttpOnly flows — separate manual matrix.

### Linux / Windows

- No registry module; existing persistent paths unchanged. Linux `persistent_context_is_scoped_to_locator` on runner temp.

## Ownership risks

- **Double pin:** Production registry + fixture `mem::forget` on writer — redundant, safe.
- **Clear vs reload:** Shell must finish clear before reload; `set_clearing` guard unchanged — race if reload races clear completion (pre-existing).
- **Leaked registry:** Intentional process-lifetime tradeoff; not a per-request allocation. Leaf tests exercise `RegistryCore` only; native clear/eviction semantics remain fixture CI.
- **Wrapper retries:** `run-macos-fixture.sh` retries a failed phase up to 3 times. `clear-verify` is read-only, so retries are idempotent. `clear-confirm` retries rewrite both identities before clearing, so a later `clear-verify` still checks the last attempt.

## Rollback

Revert registry module and `macos.rs` integration; on-disk WebKit data under `~/Library/WebKit/<bundle>/WebsiteDataStore/<UUID>/` remains. Behavior returns to teardown-time cookie loss risk.
