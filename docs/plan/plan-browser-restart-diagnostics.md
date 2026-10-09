# Plan: browser restart persistence diagnostics (approved checkpoint)

Status: **approved investigation step** — macOS fixture-only native cookie readback diagnostics; Windows E0382 fix; **no production teardown / no claimed cookie-loss fix**; no CI push, no merge.

## Evidence driving this step

| Source | Finding |
|--------|---------|
| Linux CI log `zeron-browser-native-linux-failed.log` | `browser::tests::persistent_context_is_scoped_to_locator` failed: `create_dir_all` on hardcoded `/root/.hermes/...` → **Permission denied** (code 13). |
| macOS CI log `zeron-restart-diagnostics-macos-failed.log` (2026-10-09) | `relaunch-write` passed (JS readback **cookie=1** and **localStorage=1**); new process `relaunch-verify` failed 3/3 with **`restart-verify: cookie missing (other storage present)`** — same `origin`, `locator`, `store_uuid` in boundary logs. |
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

## Rollback

Revert diagnostic commits only; on-disk persistence roots under `RUNNER_TEMP` / user data dirs are untouched.
