# Plan: browser restart persistence diagnostics (approved checkpoint)

Status: **approved investigation step** — macOS fixture-only native cookie readback diagnostics; Windows E0382 fix; **no production teardown / no claimed cookie-loss fix**; no CI push, no merge.

## Evidence driving this step

| Source | Finding |
|--------|---------|
| Linux CI log `zeron-browser-native-linux-failed.log` | `browser::tests::persistent_context_is_scoped_to_locator` failed: `create_dir_all` on hardcoded `/root/.hermes/...` → **Permission denied** (code 13). |
| macOS CI log `zeron-restart-diagnostics-macos-failed.log` (2026-10-09) | `relaunch-write` passed (JS readback **cookie=1** and **localStorage=1**); new process `relaunch-verify` failed 3/3 with **`restart-verify: cookie missing (other storage present)`** — same `origin`, `locator`, `store_uuid` in boundary logs. |
| macOS CI log `zeron-cbf-cookie-disk-ci-failed.log` (2026-10-09) | Writer native diag **`matches=1 session_only=0 has_expiry=1`** on persistent store `263b2c57-bada-4815-811b-0b111750230a`; after writer exit **`Cookies/Cookies.binarycookies` absent**, `Cookies/` empty while **`LocalStorage` / `Origins` present**; WebKit log **`WebsiteDataStore::~WebsiteDataStore`** then **`NetworkProcess::destroySession identifier=263b2c57-…`** at 16:15:59.722 before verify; verify native **`present=0`** 3/3 with other storage present. |
| Prior macOS log `zeron-browser-native-macos-failed.log` | Generic partial-state before split probe errors. |
| Investigation summary | No documented WebKit API to force HTTP cookie disk flush; macOS never passes `webkit-data` dirs to WebKit (uses `dataStoreForIdentifier` only); production teardown remains `Host::drop` with `stopLoading` only. |
| Parent plans | `docs/plan/plan-zeron-browser-persistence.md`, `/root/docs/plan/plan-zeron-ci-dmg.md` — native acceptance pending; VPS must not run heavy `cargo` builds. |

## Hypothesis (not fully proven)

macOS restart verify fails because **HTTP cookies do not reach on-disk persistence before the writer process exits**, while **localStorage** (other website data types) does. JS `document.cookie` readback succeeding in-process does not prove `WKHTTPCookieStore` persistence across relaunch.

**Not a fix:** `-[WKHTTPCookieStore getAllCookies:]` is a **read** completion handler (in-memory enumeration). Public WebKit headers do not document it as flushing cookies to disk. Fixture diagnostics use it only to log **name presence** (`present=0|1`, never value), `isPersistent`, and store `identifier` — not to change teardown or claim persistence.

## Scope (this checkpoint)

1. **Split storage diagnostics** — probe titles `persist-probe;nonce=<token>;cookie=0|1;localStorage=0|1`; actionable errors name missing surface(s) without logging stored values. *(done prior step)*
2. **Write-phase gate** — shutdown only after readback reports both surfaces. *(done prior step)*
3. **macOS fixture native diagnostics (async only)** — after write readback, await `getAllCookies`-based line on the GPUI executor (oneshot + `cx.spawn`, same pattern as `clear_website_data`); bounded timeout in harness via `tokio::time::timeout` while yielding; retain `WKWebsiteDataStore` until callback; **no** nested synchronous `CFRunLoop` pumping inside `Drop`, entity `update`, or sync borrows. On `restart-verify` split failure, **await** native check (`*:native-on-failure`) before `bail`.
4. **Production teardown** — unchanged: no `active_hosts`, no last-webview `getAllCookies` drain on `Host::drop`.
5. **Windows** — fix E0382 in `clear_profile_data` (`Arc<Mutex<..>>` for completion handler slot; Err path still signals oneshot).
6. **objc2 features** — keep pinned `objc2-web-kit` 0.3.2 with `WKHTTPCookieStore` / `NSHTTPCookie` only for fixture readback.

## Out of scope (stop line)

- Treating `getAllCookies` as disk flush or shipping a “cookie loss fix” based on it.
- Passing custom `webkit-data` paths into WebKit on macOS (architecture change).
- Cookie file copying, default-store fallback, test weakening, arbitrary teardown sleeps.
- Workflow changes, commits, pushes, merge.
- Local `cargo` / full workspace `rustc` on VPS (resource guard).
- Downloading crates to inspect `objc2-web-kit` sources when not already in the local registry (stop and report API uncertainty instead).

### Architecture change proposal (if native diag shows cookies in-store at write but absent after restart)

If CI shows `write-storage:after-readback-native present=1` and `restart-verify:native-on-failure present=0` with stable `datastore_uuid`, next step is **not** more JS probing or readback-on-drop: evaluate whether Zeron must align macOS with Linux by **binding WebKit’s on-disk store** or find a **documented** persistence/defer-exit API. Stop and design before large store refactors.

## Implementation map

| Area | Change |
|------|--------|
| `crates/ui/src/browser/macos_fixture_store_retention.rs` | Fixture-only `ZERON_BROWSER_FIXTURE_RETAIN_WEBSITE_DATA_STORE=1` process-lifetime `mem::forget(Retained<WKWebsiteDataStore>)` pin (causal A/B writer arm). |
| `crates/ui/src/browser/macos_persistence_diag.rs` | Sibling module under `browser/`; async fixture readback only. |
| `crates/ui/src/browser/macos.rs` | `fixture_native_cookie_diag` → `Task`; no teardown drain. |
| `crates/ui/examples/browser-fixture/persistence_harness.rs` | Await native diag after write readback and before verify split `bail`. |
| `crates/ui/src/browser/mod.rs` | `mod macos_persistence_diag`; `fixture_native_cookie_diag`. |
| `crates/ui/src/browser/windows_webview.rs` | `Arc<Mutex>` for clear handler slot. |
| `crates/ui/Cargo.toml` | `WKHTTPCookieStore`, `NSHTTPCookie` features on pinned objc2 crates. |

## API note (uncertainty)

`objc2-web-kit` 0.3.2 was not present under `/root/.cargo/registry/src` at verification time; binding semantics were taken from Apple’s WebKit API shape (`getAllCookies:` completion handler). If registry inspection later contradicts this, stop and revise the plan before any production persistence behavior.

## Verification (allowed on VPS)

- [ ] `rustfmt` on touched Rust sources (`/root/.cargo/bin/rustfmt`).
- [ ] `git diff --check`.
- [x] Isolated unit probe (no `cargo`, no workspace deps): `rustc --test crates/ui/src/browser/persistence_probe.rs` — **5/5 tests pass**.
- [ ] Existing lightweight script tests if applicable.

### macOS CI compile evidence (2026-10-09)

Source: `zeron-native-cookie-diag-ci-failed.log` — `browser-fixture` example build.

| Error | Location | Minimal fix |
|-------|----------|-------------|
| E0277 | `persistence_harness.rs:81` | `Entity::update` returns `Task<Result<String,String>>`; drop trailing `?` on task creation (match `load_origin` nested `browser.update`); keep `futures::select` + GPUI timer bound before awaiting task. |

### Windows CI compile evidence (pre-fix)

Source: `/root/.hermes/profiles/girlfriend/cache/scratch/zeron-browser-windows-failed.log` (PR #2 merge `aec9f02`).

| Error | Location | Minimal fix |
|-------|----------|-------------|
| E0631 | `windows_webview.rs:162` | `map_err(\|e\| runtime_error(&e))` |
| E0599 | `windows_webview.rs:313` | Drop non-existent `CreateCoreWebView2Profile` fallback |
| E0505 | `windows_webview.rs:123` | `data_for_clear` clone before callback |
| E0133 | `windows_webview.rs:718` | `unsafe { webview13.Profile() }` |
| E0382 | `windows_webview.rs:176` | `Arc<Mutex<..>>` clone for handler; Err branch uses same slot |

## Verification (requires CI / native host)

- [ ] Linux UI job: `persistent_context_is_scoped_to_locator` passes on runner temp.
- [ ] macOS native fixture: logs include `after-readback-native` and optional `native-on-failure`; pass or fail with split storage + native lines.
- [ ] Windows UI job: compiles after E0382 fix.

## Remaining evidence gaps after this step

- Whether `present=1` in native store at end of write survives into verify process (confirms or refutes exit-timing hypothesis; **does not** prove a flush API).
- If native store shows cookie on verify but JS probe does not — `document.cookie` / timing issue.
- If native store empty on verify — WebKit persistent cookie backing for identifier-based stores (may need architecture proposal).
- WebKit persistent store correctness across process restart — not proven by unit tests alone.

## Opus 5.5 second opinion (2026-10-09) and decisive experiments

Source: `zeron-opus55-medium-cookie-second-opinion.json` (code + public WebKit `main`/Wry v0.56.1 source reading; nothing native ran; runner WebKit may be older than `main`).

### Correction

`-[WKHTTPCookieStore getAllCookies:]` is an **in-memory read**, and so is the JS probe. `present=1` before exit therefore **does not prove the cookie was never written to disk**, nor that it was. The earlier "loss, not flush" reading is withdrawn: "never written" and "written then lost / not loaded" both remain open until on-disk evidence exists.

### Facts from code

- `isPersistent=true` describes the store, not the cookie; the cookie's own session-only flag and expiry were never logged.
- Probe cookie: `path=/; max-age=31536000`, no `Secure`/`HttpOnly`, plain HTTP on `127.0.0.1` (`persistence_probe.rs`).
- Store is ours: `macos.rs:108-111` `dataStoreForIdentifier`, `macos.rs:140-145` `setWebsiteDataStore`; Wry keeps an injected configuration's store.
- `scripts/run-macos-browser-fixture.sh:7` uses a fresh `mktemp` bundle path per phase, but `:13` a fixed `CFBundleIdentifier sh.zeron.browser-fixture`. WebKit keys storage by bundle id: `~/Library/WebKit/<bundle-id>/WebsiteDataStore/<UUID>/`, cookies at `Cookies/Cookies.binarycookies`.
- Per-profile `webkit-data` / `webkit-cache` dirs (`profile.rs`) are unused on macOS.
- Shutdown order (`persistence_harness.rs:682-685`): `drop(state)`, `remove_window()` (drops `Shell` → browser context → store reference), 100 ms, `quit` → `-[NSApp terminate:]`.
- WebKit teardown (source reading): releasing the store sends `DestroySession`; `NetworkProcess::destroySession` closes the storage manager (saves localStorage) but does **not** call `platformFlushCookies`. Only the connection-closed path flushes cookies, and only for sessions that still exist. The only explicit flush is private `_flushCookiesToDiskWithCompletionHandler:`.

### Ranked hypotheses

1. **High** — store released before exit destroys the network session without saving cookies (fits localStorage-survives / cookie-lost). *Falsified if* the cookie name is already in `Cookies.binarycookies` after `relaunch-write`, or no `destroySession` for the store UUID precedes exit.
2. **Medium-low** — cookie reaches disk but is not loaded back for IP-literal host `127.0.0.1`. *Falsified if* the name is on disk and a `localhost` A/B behaves the same.
3. **Low** — cookie classified session-only (e.g. tracking prevention). *Falsified by* `session_only=0 has_expiry=1` in the native diag line.
4. **Very low** — storage location changes between phases (bundle id constant; localStorage survives).
5. **Very low** — wrong store injected.

### Experiments implemented (fixture/CI only; no production change)

| # | Where | What it records |
|---|-------|-----------------|
| 1 | `scripts/ci/run-macos-browser-persistence-fixture.sh` (after the `relaunch-write` process has **exited**, before `relaunch-verify` starts; again on verify failure) | `sw_vers`, WebKit `CFBundleVersion`, Safari version; existence/size/mtime of bundle dir, `WebsiteDataStore/<UUID>/`, `Cookies/Cookies.binarycookies`, `LocalStorage`, `Origins`; entries of the fixed fixture `<UUID>` subtree only (depth 3, names/size/mtime, capped at 60 lines); **cookie name match count only** in the cookie file, never values. UUID read from marker `profileDiag` `store_uuid=`; absent paths print `absent`. Metadata only (no log capture) so writer-exit → verifier timing is unchanged. Never fails the job. |
| 2 | `crates/ui/src/browser/macos_persistence_diag.rs` | Existing async `getAllCookies` line gains `matches=N session_only=0\|1 has_expiry=0\|1` (`na` when absent). No values; same oneshot + bounded GPUI timer; no runloop pumping or `Drop` work. |
| 3 | Same script, **only after `relaunch-verify` finished** (success, or failure via the EXIT trap) | `log show --info --predicate 'subsystem == "com.apple.WebKit"'` from `relaunch-write` start (covers writer teardown), under a 60 s `perl alarm`; full output in `$RUNNER_TEMP/browser-captures-persistence-evidence/` (caught by the existing `browser-captures*` upload glob; no workflow edit), filtered `destroySession` / `~WebsiteDataStore` / closed-connection / flush lines echoed to the job log. The EXIT trap saves and re-exits with the original status, so evidence can never turn a pass into a failure or vice versa. |

Unchanged: split cookie/localStorage assertions, real process restart, identity isolation; no sleeps-as-fix, cookie backup, or default-store fallback.

API note: `objc2-foundation` 0.3.2 source was not in the local registry; `NSHTTPCookie::isSessionOnly` / `expiresDate` are wrapped in `#[allow(unused_unsafe)] unsafe` so the build is correct whichever safety marking the bindings use. `NSDate` feature is already active (`macos.rs` uses `NSDate::distantPast`).

### Reading the next CI run

The disk snapshot is taken **after the writer process exited**, so it shows the post-teardown state, not what was on disk before exit.

- `cookies-file: absent` or `cookie-name-matches: 0` after write, with `session_only=0 has_expiry=1` → **consistent with** H1 but inconclusive: the check only looks at one assumed path and a raw cookie-name byte match, so an alternate store layout/location, a different on-disk encoding, or a cookie written then deleted during teardown all read the same. Do **not** claim "never written". Corroborate with the WebKit log (`destroySession` for the store before exit, no flush) and then the fixture-only A/B (keep the store retained until `terminate:`), **not** a fix.
- `cookie-name-matches: ≥1` after write → cookie was on disk after writer exit; H1 weakened; investigate H2 (`localhost` A/B).
- `session_only=1` or `has_expiry=0` → H3.
- If H1 holds, it is not test-only: `sync_browser_profile` replaces the browser context on identity switch. A fix is a lifecycle decision (process-lifetime stores, or private flush API with distribution risk) — stop and design.
- Passing this fixture does not validate Google SSO (`HttpOnly`/`Secure` `Set-Cookie` over HTTPS, embedded-webview restrictions).

### Causal lifecycle A/B (fixture-only; 2026-10-09)

**Goal:** Test whether **releasing the persistent `WKWebsiteDataStore` during normal fixture teardown** (while the process still runs GPUI quit) prevents cookie disk persistence, without claiming a product fix.

| Arm | Env | Process | Expected if H1 (teardown-before-flush) |
|-----|-----|---------|----------------------------------------|
| `baseline` | unset `ZERON_BROWSER_FIXTURE_RETAIN_WEBSITE_DATA_STORE` | Writer uses production [`macos_store_registry`](../../crates/ui/src/browser/macos_store_registry.rs) (leaked main-thread registry pin per store UUID) plus normal teardown | **Release acceptance** (`relaunch-write` / `relaunch-verify` after contrast) must pass verify without retention env. Causal baseline verify is **no longer expected to fail** post-registry; pre-registry CI showed verify fail + absent cookie file. |
| `retain-datastore` | `ZERON_BROWSER_FIXTURE_RETAIN_WEBSITE_DATA_STORE=1` on **writer only** | Extra `mem::forget` on top of production registry; verifier is a **new process** without the env | Contrasts redundant pin vs registry-only; both arms may pass verify after registry ships. |

**Isolation:** `scripts/ci/run-macos-browser-persistence-fixture.sh` runs contrast first with separate `ZERON_BROWSER_PERSISTENCE_ROOT` and `ZERON_BROWSER_PERSISTENCE_DEVICE_A` (`fixture-causal-baseline` vs `fixture-causal-retain`) so locator-derived `store_uuid` and WebKit subtrees do not cross-contaminate arms. Release harness then runs on default `local` device and `PERSIST_ROOT` with `acceptance-lifecycle boundary` logs.

**Not acceptance:** Contrast logs `causal-lifecycle outcome … (diagnostic only; not release acceptance)` and is wrapped in `set +e`; **only** the subsequent `relaunch-write` / `relaunch-verify` pair (and later phases) set job exit status.

**Rust retention mechanics:** Production `macos_store_registry.rs` keeps one `Retained<WKWebsiteDataStore>` per store UUID in a deliberately leaked main-thread registry (`RegistryCore` with `RefCell` short borrows in `browser_store_registry_core.rs`); TLS holds `&'static` to the leaked box so thread teardown does not drop pins before WebKit exit (see `plan-browser-store-registry.md`). Fixture `macos_fixture_store_retention.rs` adds an optional **second** pin via `mem::forget` when `ZERON_BROWSER_FIXTURE_RETAIN_WEBSITE_DATA_STORE=1` for causal A/B only. **Do not** treat a passing retain-only arm as shipping criteria; release acceptance uses registry without retention env.

**Manual rerun (macOS runner, after `browser-fixture` binary exists):**

```bash
export ZERON_BROWSER_PERSISTENCE_ROOT=/tmp/zeron-causal-test
export ZERON_BROWSER_PERSISTENCE_DEVICE_A=fixture-causal-retain
export ZERON_BROWSER_FIXTURE_RETAIN_WEBSITE_DATA_STORE=1
export ZERON_BROWSER_PERSISTENCE_PHASE=relaunch-write
scripts/run-macos-browser-fixture.sh target/debug/examples/browser-fixture /tmp/cap-write
unset ZERON_BROWSER_FIXTURE_RETAIN_WEBSITE_DATA_STORE
export ZERON_BROWSER_PERSISTENCE_PHASE=relaunch-verify
scripts/run-macos-browser-fixture.sh target/debug/examples/browser-fixture /tmp/cap-verify
```

## Rollback

Revert diagnostic commits only; on-disk persistence roots under `RUNNER_TEMP` / user data dirs are untouched.
