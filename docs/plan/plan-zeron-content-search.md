# Plan: Zeron explorer content search

Status: **IMPLEMENTATION IN PROGRESS** on branch `feat/content-search` at `/root/zeron` (fork `pancaprima/zeron`, base commit `1074bc5`).

## Progress

| Area | Status |
|------|--------|
| Protocol (`SearchWorkspaceContent`, match/response types, RPC constant) | Done |
| Engine bounded scanner + literal/fuzzy matcher + unit tests | Done — 16/16 unit tests pass (2026-10-09, see logs below) |
| RPC dispatch + `targetDeviceId` forwardable list | Done |
| UI client + explorer Files/Contents + Literal/Fuzzy selectors | Done |
| Content result rows + open/reveal with line selection + stale/dirty guards | Reworked (column units, visible editor errors) |
| Integration test in `workspace_files` RPC suite | 8/8 pass (`workspace_files` test binary; no dedicated content RPC case in that file) |
| Device routing integration test for content search | Exercised via `SEARCH_WORKSPACE_CONTENT` in `workspace_entry_mutations_are_forwarded_to_the_owning_plain_folder` — 11/11 `device_routing` pass |
| Manual UI verification | Not done |
| Performance/memory measurement on real monorepo | Not done |

## Goal and scope

Explorer-only content search, literal + fzf-style fuzzy (nucleo-matcher subsequence via `Pattern::new` + `AtomKind::Fuzzy`, not `Pattern::parse` query operators), unary RPC capped at 200 results, `include_ignored` eye toggle, owning-device forwarding via existing `targetDeviceId` transport, bounded reads and scan budgets, completion metadata, filename search unchanged.

## Scanner fixes (2026-10-09 review pass)

- **Fuzzy API:** `Pattern::indices(haystack, matcher, &mut Vec<u32>) -> Option<u32>`; char indices converted to UTF-8 highlight bytes; `Utf32String::from(line)`.
- **Literal:** Per-source-character fold using the first `char` from `to_lowercase()` (case-mapping tail codepoints stay in the source byte span); `match_text` from scalar offsets.
- **Per-line / global caps:** `CONTENT_SEARCH_MAX_MATCHES_PER_LINE`, match text and highlight caps; global trim sets `ResultLimitReached`.
- **Lines:** Resumable oversized-line skip state machine; CRLF; invalid UTF-8 lines/files tracked via `skipped_unsupported` without dropping earlier matches; binary tail processes bytes before the first NUL then keeps matches + `skipped_binary`.
- **Preview:** Crop forward from first match on long lines (`CONTENT_SEARCH_PREVIEW_MAX_CHARS`).
- **I/O:** `metadata_for_workspace_file` containment; per-file byte cap; `.git` directory pruned in walk (not fully traversed).
- **UI:** `OpenFileColumnUnit` — transcript links 1-based, content search UTF-32 scalar offsets; `navigate_to_line` preserves 1-based columns; stale check compares exact scalar slice; navigation errors on `FilesSurface::error`.

## Budgets (initial)

- Result cap: 200 matches (not a total-match count).
- Per-file read cap: `MAX_PREVIEW_FILE_BYTES` (8 MiB); larger files counted as `skippedTooLarge`.
- Total bytes scanned per request: 64 MiB (`CONTENT_SEARCH_MAX_TOTAL_BYTES_SCANNED`).
- Max line bytes while scanning: 64 KiB (oversized lines skipped without unbounded allocation).
- Preview: up to 160 Unicode scalars cropped around first highlight.
- Scan deadline: 4.5s worker budget (`CONTENT_SEARCH_SCAN_DEADLINE`), RPC timeout remains 6s (`WORKSPACE_FILE_RPC_TIMEOUT`).

## Verification (2026-10-09, `/root/.cargo/bin` on PATH)

Environment: `CARGO_BUILD_JOBS=1 CARGO_PROFILE_DEV_DEBUG=0 CARGO_PROFILE_TEST_DEBUG=0`.

| Command | Result | Log |
|---------|--------|-----|
| `cargo test --locked -p zeron-engine --lib workspace_content_search::` | **ok. 16 passed; 0 failed** | `/root/.hermes/profiles/girlfriend/cache/scratch/zeron-content-lib.log` (`EXIT:0`) |
| `cargo test --locked -p zeron-engine --test workspace_files` | **ok. 8 passed; 0 failed** | `/root/.hermes/profiles/girlfriend/cache/scratch/zeron-content-workspace_files.log` (`EXIT:0`) |
| `cargo test --locked -p zeron-engine --test device_routing` | **ok. 11 passed; 0 failed** (includes remote `SEARCH_WORKSPACE_CONTENT`) | `/root/.hermes/profiles/girlfriend/cache/scratch/zeron-content-device_routing.log` (`EXIT:0`) |
| `cargo check --locked -p zeron-ui` | **Failed** — `openssl-sys` build: no system OpenSSL/pkg-config (`EXIT:101`) | `/root/.hermes/profiles/girlfriend/cache/scratch/zeron-content-ui-check.log` |

Proto lib tests were already **67 passed** on parent branch (not re-run here). First engine build on this host required a full dependency compile (~25m, `CARGO_BUILD_JOBS=1`); incremental re-runs ~15–45s.

```bash
# Reference commands
export PATH="/root/.cargo/bin:$PATH"
CARGO_BUILD_JOBS=1 CARGO_PROFILE_DEV_DEBUG=0 CARGO_PROFILE_TEST_DEBUG=0 \
  cargo test --locked -p zeron-engine --lib workspace_content_search::
CARGO_BUILD_JOBS=1 CARGO_PROFILE_DEV_DEBUG=0 CARGO_PROFILE_TEST_DEBUG=0 \
  cargo test --locked -p zeron-engine --test workspace_files
CARGO_BUILD_JOBS=1 CARGO_PROFILE_DEV_DEBUG=0 CARGO_PROFILE_TEST_DEBUG=0 \
  cargo test --locked -p zeron-engine --test device_routing
```

## Remaining acceptance gaps

- `zeron-ui` compile not verified on this host (OpenSSL dev packages missing; no installs per scope).
- No measured scan duration/memory on representative monorepos.
- Native manual UI pass not performed.
- Streaming read uses initial `metadata_for_workspace_file` + per-file byte cap; not the full `read_file_blocking` TOCTOU double-read protocol (growing files are bounded, not re-canonicalized mid-scan).
- Scan deadline / cancel atomics are implemented; no dedicated integration test proving RPC timeout stops an in-flight scan (unit test covers cancel flag only).

## API references (pinned)

- `nucleo-matcher` 0.3: [Pattern::indices](https://docs.rs/nucleo-matcher/0.3.0/nucleo_matcher/pattern/struct.Pattern.html#method.indices), [Pattern::new + AtomKind::Fuzzy](https://docs.rs/nucleo-matcher/0.3.0/nucleo_matcher/pattern/enum.AtomKind.html).
- Editor navigation: `gpui_base::input::Position` row/column (scalar); `InputState::set_selected_range`, `FileDocument::is_dirty` used as in existing `crates/ui/src/files/document.rs` / `preview.rs` tests.
