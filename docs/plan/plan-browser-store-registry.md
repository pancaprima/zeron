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
| **Clear failure** | Fixture-injected error returns before WebKit; registry unchanged; data preserved (`clear-failure` phase). |
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
| `crates/ui/src/browser/macos_fixture_store_retention.rs` | Optional extra pin for causal experiment only |
| `scripts/ci/browser_store_registry_leaf.rs` | `rustc --test` against production `RegistryCore` (path include) |
| `scripts/ci/run-macos-browser-persistence-fixture.sh` | Release acceptance without retention env; causal contrast `set +e` |

## Testing

### VPS / CI-light (no workspace `cargo` build)

- `rustc --test scripts/ci/browser_store_registry_leaf.rs` — production `RegistryCore`: reuse, isolation, caller-drop pin, reentrant open, clear does not evict (core proof only).
- `rustc scripts/ci/browser_persistence_fixture_retain_env.rs` — retention env gate.
- `bash scripts/ci/test-browser-persistence-fixture-harness.sh` — harness bash regressions **and** registry leaf + retain env (when wired).
- `rustfmt` on touched Rust sources; `git diff --check`.

### Native macOS (required gates — not run on VPS)

- [ ] Release harness: `relaunch-write` / `relaunch-verify` pass **without** `ZERON_BROWSER_FIXTURE_RETAIN_WEBSITE_DATA_STORE`.
- [ ] Post-write disk evidence: `Cookies.binarycookies` present with fixture cookie name match after writer exit.
- [ ] `isolation`, `clear-cancel`, `clear-confirm`, `clear-failure` phases pass.
- [ ] Causal contrast: document outcomes (both arms may pass verify); retain arm still writer-only env.
- [ ] Real app: profile switch isolation; Google/HttpOnly flows — separate manual matrix.

### Linux / Windows

- No registry module; existing persistent paths unchanged. Linux `persistent_context_is_scoped_to_locator` on runner temp.

## Ownership risks

- **Double pin:** Production registry + fixture `mem::forget` on writer — redundant, safe.
- **Clear vs reload:** Shell must finish clear before reload; `set_clearing` guard unchanged — race if reload races clear completion (pre-existing).
- **Leaked registry:** Intentional process-lifetime tradeoff; not a per-request allocation. Leaf tests exercise `RegistryCore` only; native clear/eviction semantics remain fixture CI.

## Rollback

Revert registry module and `macos.rs` integration; on-disk WebKit data under `~/Library/WebKit/<bundle>/WebsiteDataStore/<UUID>/` remains. Behavior returns to teardown-time cookie loss risk.
