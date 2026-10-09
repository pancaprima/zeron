# Plan: browser restart persistence diagnostics (approved checkpoint)

Status: **approved investigation step** — implement diagnostics and portable test roots; no WebKit/store behavior changes, no CI push, no merge.

## Evidence driving this step

| Source | Finding |
|--------|---------|
| Linux CI log `zeron-browser-native-linux-failed.log` | `browser::tests::persistent_context_is_scoped_to_locator` failed: `create_dir_all` on hardcoded `/root/.hermes/...` → **Permission denied** (code 13). |
| macOS CI log `zeron-browser-native-macos-failed.log` | `relaunch-write` passed; `relaunch-verify` failed 3/3 with generic **partial state** (cookie vs localStorage not distinguished). |
| Parent plans | `docs/plan/plan-zeron-browser-persistence.md`, `/root/docs/plan/plan-zeron-ci-dmg.md` — native acceptance pending; VPS must not run heavy `cargo` builds. |

## Hypothesis (not a fix)

macOS restart verify may be failing because only one storage surface persisted, or because write phase reported success before readback proved both cookie **and** localStorage. Linux failure is **test harness portability**, not persistence logic.

## Scope (this checkpoint)

1. **Split storage diagnostics** — probe titles `persist-probe;nonce=<token>;cookie=0|1;localStorage=0|1` (`nonce` non-empty, ≤64 chars, ASCII alnum plus `-`/`_`); waiter matches title nonce to the per-phase token from `new_probe_nonce()` via `probe_satisfied` / parse; actionable errors name missing surface(s) without logging stored values.
2. **Write-phase gate** — writer runs the same readback probe after set; shutdown only after title reports `cookie=1` **and** `localStorage=1` for that nonce (not merely that the write script ran).
3. **Synthetic profile diagnostics** — `eprintln` at phase boundaries: phase, loopback origin, `mode` / `locator` / `store_uuid`; marker JSON field `profileDiag` (no tokens, no cookie values).
4. **Portable unit-test dirs** — replace hardcoded `/root/...` in `browser/mod.rs` and `browser/profile.rs` with per-test temp dirs + `Drop` cleanup.

## Out of scope (stop line)

- Guessed WKWebsiteDataStore / WebKitGTK timing fixes or longer sleeps.
- Weakening assertions or default-store / ephemeral fallback.
- Workflow changes, commits, pushes, merge.
- Local `cargo` / `rustc` / `nextest` on VPS (resource guard).

## Implementation map

| Area | Change |
|------|--------|
| `crates/ui/src/browser/persistence_probe.rs` | Probe title parse/build + unit tests (runs in `--lib` CI). |
| `crates/ui/examples/browser-fixture/persistence_harness.rs` | Readback-gated write; split verify errors; boundary logging. |
| `crates/ui/src/shell.rs` | `fixture_browser_persistence_diag` (browser-fixture only). |
| `crates/ui/src/browser/profile.rs`, `mod.rs` | `UniqueTestDir` for tests. |

## Verification (allowed on VPS)

- [x] `rustfmt` on `crates/ui/src/browser/persistence_probe.rs` only (parent `rustfmt --check` flagged line 219 long `assert!`; reformatted to wrapped form).
- [x] `git diff --check` (clean after rustfmt + Windows signature fixes).
- [x] Parent isolated unit probe (no `cargo`, no workspace deps): `rustc --test crates/ui/src/browser/persistence_probe.rs` — **5/5 tests pass** (`parse_probe_title_reads_boolean_flags`, `parse_probe_title_rejects_wrong_prefix_and_malformed`, `probe_satisfied_requires_matching_nonce`, `storage_probe_error_is_actionable_without_values`, `persist_scripts_embed_nonce`).
- [ ] Existing lightweight script tests if applicable.

### Windows CI compile evidence (pre-fix)

Source: `/root/.hermes/profiles/girlfriend/cache/scratch/zeron-browser-windows-failed.log` (PR #2 merge `aec9f02`, jobs `ui-tests` / `app` / `harness-app-tests`).

| Error | Location | Cause (from compiler) | Minimal browser-only fix |
|-------|----------|----------------------|---------------------------|
| E0631 | `windows_webview.rs:162` | `map_err(runtime_error)` but `runtime_error` takes `&Error` | `map_err(\|error\| runtime_error(&error))` |
| E0599 | `windows_webview.rs:313` | `CreateCoreWebView2Profile` **not** on `ICoreWebView2Environment11` in pinned `webview2-com-sys` 0.38.2 (CI help lists only `CreateCoreWebView2Controller`) | Drop non-existent fallback; clear uses `ICoreWebView2_13::Profile` cached in `attach`, else actionable error |
| E0505 | `windows_webview.rs:123` | `data` borrowed for `with_environment` then moved in closure | `let data_for_clear = data.clone()` before callback |
| E0133 | `windows_webview.rs:718` | `Profile()` is `unsafe` | `unsafe { webview13.Profile() }` (same pattern as neighboring WebView2 calls) |

## Verification (requires CI / native host)

- [ ] Linux UI job: `persistent_context_is_scoped_to_locator` passes on runner temp.
- [ ] macOS native fixture: `relaunch-write` + `relaunch-verify` with new diagnostics in log (pass or fail with **which** storage missing).
- [ ] Isolation / clear-* phases unchanged in intent.

## Remaining evidence gaps after this step

- Whether macOS failure is cookie-only, localStorage-only, or race — needs macOS CI log with new probe errors.
- WebKit persistent store correctness across process restart — not proven by unit tests alone.
- Windows native matrix: compile blockers above addressed in-tree; runtime relaunch/clear still needs Windows CI/host.

## Rollback

Revert diagnostic/probe commits only; on-disk persistence roots under `RUNNER_TEMP` / user data dirs are untouched.
