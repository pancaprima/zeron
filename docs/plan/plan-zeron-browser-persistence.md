# Plan: Zeron persistent browser profiles and clear browser data

Status: user approved scope and implementation. Implementation branch `feat/browser-persistence` in worktree `/root/zeron-browser-persistence` (base `1074bc5`). No push, deploy, native data wipe, or live-engine restart authorized.

## Progress (2026-10-09)

- [x] Isolated worktree and plan copy under `docs/plan/`.
- [x] `browser/profile.rs`: validated 16-hex locator, scoped storage paths, deterministic store UUIDs, private dir permissions (Unix).
- [x] macOS: `WKWebsiteDataStore::dataStoreForIdentifier` via `NSUUID::from_bytes`; process-lifetime store registry (`macos_store_registry.rs`) pins persistent stores by UUID across tab/window teardown; no silent ephemeral fallback for persistent mode; clear on GPUI main executor with async oneshot (no background-thread `MainThreadMarker` / blocking channel).
- [x] Linux: `g_object_new` website data manager + explicit SQLite cookie store; `webkit_website_data_manager_clear` with cancellable slot and broad data types; shared `clear_waiter` `Arc`; helper-death / timeout completion.
- [x] Windows: profile from `ICoreWebView2_13::Profile` when available, else `ICoreWebView2Environment11::CreateCoreWebView2Profile`; async clear with oneshot; persistent profiles do not fall back to per-run PID folders on `ERROR_BUSY`.
- [x] Shell: locator snapshot during clear, `set_clearing` guard, single clear task, `reload_after_data_clear`, profile-change guard.
- [ ] **Native verification pending** on macOS, Windows, and Linux hosts (persistence across relaunch, isolation, clear/cancel). VPS build blocked by toolchain/native deps; no executed native matrix in this worktree.
- [ ] Confirm pinned API symbols against installed SDKs/runtime on each platform (`NSUUID`, `ICoreWebView2Environment11`, WebKitGTK 4.1 headers).

## Goal

Persistent cookies and supported website data survive normal app relaunch under the same Zeron profile. Separate Zeron identities must not share browser data. Provide an explicit, confirmed Clear browser data action. Site-defined session cookies and expired/revoked credentials do not gain persistence guarantees.

## Verified starting points

- macOS browser/macos.rs:43 uses WKWebsiteDataStore::nonPersistentDataStore, and Wry construction also enables incognito.
- Linux browser/linux/helper.c:619 creates an ephemeral WebKit context.
- Windows browser/windows_webview.rs:569 enables InPrivate with a profile name; existing runtime data directory is temporary.
- shell.rs:12752-12769 derives workspace_locator identity and resets BrowserContext on profile changes.
- Tabs share the BrowserContext; no existing cache/cookie-management UI found in audit.

## Isolation and concurrent work

Content-search implementation is still active in /root/zeron and touches shell.rs. Browser implementation must use an independent git worktree at /root/zeron-browser-persistence, branch feat/browser-persistence, based on inspected base HEAD, not copy uncommitted search changes. Do not edit the search worktree. Keep changes separate for later reviewed integration; do not cherry-pick/merge without reviewing overlapping shell changes.

## Architecture

1. Introduce explicit browser-profile identity and storage configuration. Reuse real workspace/auth scope identity; profile must be stable across app restarts and distinct across local/synced/development scopes and authenticated accounts. Do not use a random runtime UUID, tab/chat ID, access token, or directory title.
2. Derive opaque filesystem-safe storage key from identity. Store browser data under the application's established private user-data directory, not a repository or temp directory. Do not embed raw credentials in paths or logs. Inspect actual data-directory conventions before choosing APIs.
3. Create persistent native stores using pinned native/dependency APIs. macOS: verify supported WKWebsiteDataStore persistent identity APIs and deployment target; do not use a single global default store if it breaks isolation. Linux: explicit WebKit website-data/cache manager directories, supplied securely to helper. Windows: stable user-data directory and profile name, InPrivate disabled for this normal persistent profile.
4. Replace browser context before opening tabs for another identity. Unknown identity must never accidentally use another account's persistent default; safe deferred/ephemeral initialization is acceptable until identity is known.
5. Preserve existing navigation, preview proxy, tab sharing, native failure recovery and browser helper behavior. No persistence of page contents into transcript/CRDT or cloud sync.

## Clear data action

- Add a discoverable browser action: Clear browser data for the current browser profile, all sites.
- Confirmation explains it removes cookies, cache and site storage, can sign out websites and affects all tabs of this profile. Cancel is non-destructive default.
- Target supported cookies, HTTP cache and local website storage via native profile-specific APIs, not indiscriminate filesystem deletion while native handles are open.
- Coordinate live tabs/loading and asynchronous completion; avoid tabs immediately recreating data during clear. Reload/recreate native views if required, retaining intended page addresses, and report failures clearly. No automatic clear on normal close/relaunch/profile switch.
- No deletion of other profiles, browser engine installation/runtime assets, auth tokens or Zeron account/session data.
- No per-site inspector, cookie value viewer, private-mode toggle, or detailed cache size UI in this feature.

## Tests and verification

Use existing Rust unit tests, GPUI test fixtures, browser fixture and platform tests. Add failing behavior regressions before corresponding fixes where executable. Do not claim native behavior from mocked policy tests.

- Stable profile identity/path across relaunch; distinct identities and scopes remain separate; paths safe and no raw tokens.
- Startup cannot create wrong-profile persistent data before auth/profile identity is ready.
- Tabs in same profile share persistent cookies; persistent cookie and localStorage survive real native teardown/restart.
- Profile B cannot read profile A test cookie/storage; switching back restores A data.
- Confirmed clear removes current-profile cookies/cache/site storage; cancel preserves data; other profiles untouched; clear errors surface.
- Existing navigation/preview/clipboard/helper tests remain passing.
- Linux native fixture exercises restart by destroying/restarting the relevant helper or app process against the same directories, not merely reopening another tab.
- macOS and Windows must have native verification on their hosts or truthful pending status. Document supported API/OS baseline and any prerequisite blocker; no silent global-profile fallback.

Toolchain exists at /root/.cargo/bin (stable, rustfmt, clippy). Use one Cargo build job and low debug settings on this VPS. Check disk/resources and stop before free disk <4 GiB. No package/toolchain installs authorized by this task. Existing native dependencies may block GUI builds; report exact errors and do not fake results.

## Deliverables

Independent branch and implemented source, repo copy at docs/plan/plan-zeron-browser-persistence.md, tests and exact execution logs, native verification matrix and remaining gaps. Parent verifies diffs and executed checks independently. No completion claim until acceptance is actually verified; cross-platform blockers must remain explicit.

## Rollback and risk

Code can be reverted; persistent profiles introduce on-disk site data, so reverting code alone does not erase it. Preserve data by default. Any migration/removal requires explicit authorization. The old implementation was ephemeral, so there is no historical browser session available to recover after already closing the app. Persistent data improves convenience but changes privacy/security policy; keep profile-local permissions and never log cookie values.

## Native API references (pinned intent)

| Platform | Persistence | Clear |
|----------|-------------|-------|
| macOS 11+ | `+[WKWebsiteDataStore dataStoreForIdentifier:]` with `NSUUID` from deterministic 16-byte id (`objc2-web-kit` 0.3.2) | `-[WKWebsiteDataStore removeDataOfTypes:modifiedSince:completionHandler:]` with `allWebsiteDataTypes` |
| Linux WebKitGTK 4.1 | `WebKitWebsiteDataManager` via `g_object_new(..., "base-data-directory", ..., "base-cache-directory", ...)` + `webkit_cookie_manager_set_persistent_storage` | `webkit_website_data_manager_clear(..., types, 0, NULL, callback, user_data)` |
| Windows WebView2 109+ | `CreateCoreWebView2EnvironmentWithOptions` + `ICoreWebView2ControllerOptions` profile name / user-data folder | `ICoreWebView2Profile2::ClearBrowsingDataAll` on profile from `ICoreWebView2_13::Profile` or `ICoreWebView2Environment11::CreateCoreWebView2Profile` |
