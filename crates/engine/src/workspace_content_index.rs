//! Background SQLite line cache for workspace content search (Option B).
//!
//! Indexed reads are **on by default** ([`content_index_reads_enabled`]); opt out via
//! `ZERONA_CONTENT_INDEX`. The scanner in `workspace_content_search` remains the fallback
//! when the index is not ready or the revision is stale.

use std::collections::{HashMap, HashSet, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex, Weak};
use std::thread::JoinHandle;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use rusqlite::{Connection, OpenFlags, OptionalExtension, params};
use tokio_util::sync::CancellationToken;
use zeron_proto::{
    SearchWorkspaceContentResponse, WorkspaceContentMatchMode, WorkspaceFileChange,
    WorkspaceFileChangeKind, WorkspaceFileChanges,
};

use crate::workspace_content_search::{
    CONTENT_SEARCH_MAX_BYTES_PER_FILE, ContentLineMatcher, ScanBudgetState, build_search_response,
    collect_file_lines_for_index, content_search_walk_builder,
};
use crate::workspace_files::validate_workspace_search_query;
use crate::workspace_files::{
    MAX_SEARCH_RESULTS, WorkspaceFilesError, WorkspaceRelativePath, contains_git_component,
    is_internal_temp_wire_path, metadata_for_workspace_file, path_to_wire,
};

pub const CONTENT_INDEX_SCHEMA_VERSION: i64 = 1;
pub const CONTENT_INDEX_GLOBAL_DISK_BYTES: u64 = 256 * 1024 * 1024;
const INDEX_SQLITE_CACHE_KIB: i32 = -8192; // ~8 MiB page cache
const INDEX_FILES_PER_YIELD: u32 = 8;
const INDEX_YIELD_SLEEP: Duration = Duration::from_millis(10);
const INDEX_WORK_QUEUE_CAPACITY: usize = 128;
const META_BUILD_COMPLETE: &str = "build_complete";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum IndexIgnoreProfile {
    RespectGitignore,
    IncludeAll,
}

impl IndexIgnoreProfile {
    fn db_suffix(self) -> &'static str {
        match self {
            Self::RespectGitignore => "gitignore",
            Self::IncludeAll => "all",
        }
    }

    fn include_ignored(self) -> bool {
        matches!(self, Self::IncludeAll)
    }
}

/// Parse `ZERONA_CONTENT_INDEX` / `ZERONA_CONTENT_INDEX_WRITE` without touching the process environment.
pub fn parse_content_index_env_flag(value: Option<&str>) -> bool {
    matches!(
        value,
        Some(v) if v == "1" || v.eq_ignore_ascii_case("true")
    )
}

/// Parse `ZERONA_CONTENT_INDEX` read enablement (default **on**, opt-out).
pub fn parse_content_index_reads_flag(value: Option<&str>) -> bool {
    match value {
        Some(v)
            if v == "0"
                || v.eq_ignore_ascii_case("false")
                || v.eq_ignore_ascii_case("off")
                || v.eq_ignore_ascii_case("no") =>
        {
            false
        }
        _ => true,
    }
}

/// Indexed reads default **on**; set `ZERONA_CONTENT_INDEX` to `0`/`false`/`off`/`no` to disable.
/// The scanner remains the fallback when the index is not ready or the revision is stale.
pub fn content_index_reads_enabled() -> bool {
    parse_content_index_reads_flag(std::env::var("ZERONA_CONTENT_INDEX").ok().as_deref())
}

/// Background writer (disk/CPU). Off in production unless reads are on or explicit bench opt-in.
pub fn content_index_writer_enabled() -> bool {
    ContentIndexEnvConfig::from_process_env().writer_enabled
}

/// Immutable per-manager feature gates (production reads env once at construction).
#[derive(Debug, Clone, Copy)]
pub struct ContentIndexEnvConfig {
    pub reads_enabled: bool,
    pub writer_enabled: bool,
}

impl ContentIndexEnvConfig {
    pub fn from_process_env() -> Self {
        let reads_enabled = content_index_reads_enabled();
        let writer_enabled = reads_enabled
            || parse_content_index_env_flag(
                std::env::var("ZERONA_CONTENT_INDEX_WRITE").ok().as_deref(),
            );
        Self {
            reads_enabled,
            writer_enabled,
        }
    }

    pub const fn for_tests(reads_enabled: bool, writer_enabled: bool) -> Self {
        Self {
            reads_enabled,
            writer_enabled,
        }
    }
}

pub struct ContentIndexWatchBridge {
    manager: Weak<ContentIndexManager>,
    checkout_id: String,
}

impl ContentIndexWatchBridge {
    pub fn on_raw_fs_activity(&self) {
        if let Some(manager) = self.manager.upgrade() {
            manager.mark_dirty_raw(&self.checkout_id);
        }
    }
}

enum IndexWork {
    FullBuild {
        checkout_id: String,
        root: PathBuf,
        profile: IndexIgnoreProfile,
        job_revision: u64,
    },
    Reconcile {
        checkout_id: String,
        root: PathBuf,
        profile: IndexIgnoreProfile,
        job_revision: u64,
    },
    ApplyChanges {
        checkout_id: String,
        root: PathBuf,
        profile: IndexIgnoreProfile,
        resync_required: bool,
        changes: Vec<WorkspaceFileChange>,
        job_revision: u64,
    },
    EvictCheckout {
        checkout_id: String,
    },
    Shutdown,
}

struct IndexWorkQueueInner {
    items: VecDeque<IndexWork>,
    shutdown: bool,
}

struct IndexWorkQueue {
    inner: Mutex<IndexWorkQueueInner>,
    notify: Condvar,
}

struct IndexWorkSender {
    queue: Arc<IndexWorkQueue>,
}

struct IndexWorkReceiver {
    queue: Arc<IndexWorkQueue>,
}

fn index_work_queue() -> (IndexWorkSender, IndexWorkReceiver) {
    let queue = Arc::new(IndexWorkQueue {
        inner: Mutex::new(IndexWorkQueueInner {
            items: VecDeque::new(),
            shutdown: false,
        }),
        notify: Condvar::new(),
    });
    (
        IndexWorkSender {
            queue: queue.clone(),
        },
        IndexWorkReceiver { queue },
    )
}

impl IndexWorkSender {
    fn send(&self, work: IndexWork) {
        if matches!(work, IndexWork::Shutdown) {
            let mut inner = self.queue.inner.lock().expect("index work queue");
            inner.shutdown = true;
            inner.items.push_back(work);
            self.queue.notify.notify_one();
            return;
        }
        let mut inner = self.queue.inner.lock().expect("index work queue");
        if inner.shutdown {
            return;
        }
        coalesce_index_work(&mut inner.items, work);
        while inner.items.len() > INDEX_WORK_QUEUE_CAPACITY {
            promote_index_work_overflow(&mut inner.items);
        }
        self.queue.notify.notify_one();
    }
}

impl IndexWorkReceiver {
    fn recv(&self) -> Option<IndexWork> {
        let mut inner = self.queue.inner.lock().expect("index work queue");
        loop {
            if let Some(work) = inner.items.pop_front() {
                return Some(work);
            }
            if inner.shutdown {
                return None;
            }
            inner = self
                .queue
                .notify
                .wait(inner)
                .expect("index work queue wait");
        }
    }
}

fn coalesce_index_work(items: &mut VecDeque<IndexWork>, work: IndexWork) {
    match work {
        IndexWork::ApplyChanges {
            checkout_id,
            root,
            profile,
            resync_required: false,
            changes,
            job_revision,
        } if !changes.is_empty() => {
            for existing in items.iter_mut().rev() {
                if let IndexWork::ApplyChanges {
                    checkout_id: existing_id,
                    profile: existing_profile,
                    resync_required: false,
                    changes: existing_changes,
                    ..
                } = existing
                {
                    if *existing_id == checkout_id && *existing_profile == profile {
                        existing_changes.extend(changes);
                        return;
                    }
                }
            }
            items.push_back(IndexWork::ApplyChanges {
                checkout_id,
                root,
                profile,
                resync_required: false,
                changes,
                job_revision,
            });
        }
        other => items.push_back(other),
    }
}

fn promote_index_work_overflow(items: &mut VecDeque<IndexWork>) {
    if let Some(work) = items.pop_front() {
        match work {
            IndexWork::ApplyChanges {
                checkout_id,
                root,
                profile,
                ..
            } => {
                items.push_back(IndexWork::ApplyChanges {
                    checkout_id,
                    root,
                    profile,
                    resync_required: true,
                    changes: Vec::new(),
                    job_revision: 0,
                });
            }
            other => items.push_front(other),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ProfilePhase {
    Missing,
    Building,
    Reconciling,
    Dirty,
    Resyncing,
    Ready,
}

struct ProfileState {
    phase: ProfilePhase,
    /// Monotonic in-memory revision; bumps on every invalidation even while Building/Dirty.
    revision: u64,
    root: PathBuf,
    db_path: PathBuf,
    last_used: Instant,
}

struct CheckoutIndexState {
    profiles: HashMap<IndexIgnoreProfile, ProfileState>,
    _bridge: Arc<ContentIndexWatchBridge>,
}

pub struct ContentIndexManager {
    config: ContentIndexEnvConfig,
    index_dir: PathBuf,
    cancel: CancellationToken,
    foreground_searches: AtomicUsize,
    checkouts: Mutex<HashMap<String, CheckoutIndexState>>,
    work_tx: IndexWorkSender,
    worker: Mutex<Option<JoinHandle<()>>>,
}

/// RAII guard: concurrent foreground searches share a refcount (bool is insufficient).
pub struct ForegroundSearchGuard {
    manager: Arc<ContentIndexManager>,
}

impl ForegroundSearchGuard {
    pub fn enter(manager: &Arc<ContentIndexManager>) -> Self {
        manager
            .foreground_searches
            .fetch_add(1, Ordering::AcqRel);
        Self {
            manager: manager.clone(),
        }
    }
}

impl Drop for ForegroundSearchGuard {
    fn drop(&mut self) {
        self.manager
            .foreground_searches
            .fetch_sub(1, Ordering::AcqRel);
    }
}

impl ContentIndexManager {
    pub fn new(index_dir: PathBuf) -> Arc<Self> {
        Self::new_with_config(index_dir, ContentIndexEnvConfig::from_process_env())
    }

    pub fn new_for_tests(index_dir: PathBuf, config: ContentIndexEnvConfig) -> Arc<Self> {
        Self::new_with_config(index_dir, config)
    }

    fn new_with_config(index_dir: PathBuf, config: ContentIndexEnvConfig) -> Arc<Self> {
        std::fs::create_dir_all(&index_dir).ok();
        let (work_tx, work_rx) = index_work_queue();
        let cancel = CancellationToken::new();
        let manager = Arc::new(Self {
            config,
            index_dir,
            cancel,
            foreground_searches: AtomicUsize::new(0),
            checkouts: Mutex::new(HashMap::new()),
            work_tx,
            worker: Mutex::new(None),
        });
        let weak = Arc::downgrade(&manager);
        let handle = std::thread::spawn(move || index_worker_blocking(weak, work_rx));
        *manager.worker.lock().expect("index worker") = Some(handle);
        manager
    }

    fn writer_enabled(&self) -> bool {
        self.config.writer_enabled
    }

    fn reads_enabled(&self) -> bool {
        self.config.reads_enabled
    }

    pub fn watch_bridge(self: &Arc<Self>, checkout_id: &str) -> Arc<ContentIndexWatchBridge> {
        Arc::new(ContentIndexWatchBridge {
            manager: Arc::downgrade(self),
            checkout_id: checkout_id.to_string(),
        })
    }

    pub fn on_watch_activated(
        self: &Arc<Self>,
        checkout_id: String,
        root: PathBuf,
        bridge: Arc<ContentIndexWatchBridge>,
    ) {
        if !self.writer_enabled() {
            return;
        }
        let primary = IndexIgnoreProfile::RespectGitignore;
        let mut profile_map = HashMap::new();
        for profile in [primary, IndexIgnoreProfile::IncludeAll] {
            let db_path = self.db_path(&checkout_id, profile);
            let phase = if profile == primary && db_path.exists() {
                ProfilePhase::Reconciling
            } else if profile == primary {
                ProfilePhase::Missing
            } else {
                ProfilePhase::Missing
            };
            profile_map.insert(
                profile,
                ProfileState {
                    phase,
                    revision: 1,
                    root: root.clone(),
                    db_path,
                    last_used: Instant::now(),
                },
            );
        }
        {
            let mut checkouts = self.checkouts.lock().expect("index checkouts");
            checkouts.insert(
                checkout_id.clone(),
                CheckoutIndexState {
                    profiles: profile_map,
                    _bridge: bridge,
                },
            );
        }
        let primary_db = self.db_path(&checkout_id, primary);
        let job_revision = self.bump_profile_revision(&checkout_id, primary);
        if primary_db.exists() {
            self.work_tx.send(IndexWork::Reconcile {
                checkout_id: checkout_id.clone(),
                root: root.clone(),
                profile: primary,
                job_revision,
            });
        } else {
            self.work_tx.send(IndexWork::FullBuild {
                checkout_id: checkout_id.clone(),
                root: root.clone(),
                profile: primary,
                job_revision,
            });
        }
    }

    pub fn ensure_profile_writer(
        self: &Arc<Self>,
        checkout_id: &str,
        root: &Path,
        include_ignored: bool,
    ) {
        if !self.writer_enabled() {
            return;
        }
        let profile = profile_for_include_ignored(include_ignored);
        if profile == IndexIgnoreProfile::RespectGitignore {
            return;
        }
        let needs_build = {
            let checkouts = self.checkouts.lock().expect("index checkouts");
            match checkouts.get(checkout_id) {
                Some(state) => match state.profiles.get(&profile) {
                    Some(profile_state) => matches!(
                        profile_state.phase,
                        ProfilePhase::Missing | ProfilePhase::Dirty | ProfilePhase::Reconciling
                    ),
                    None => false,
                },
                None => false,
            }
        };
        if !needs_build {
            return;
        }
        self.set_profile_phase(checkout_id, profile, ProfilePhase::Building);
        let job_revision = self.bump_profile_revision(checkout_id, profile);
        let _ = self.work_tx.send(IndexWork::FullBuild {
            checkout_id: checkout_id.to_string(),
            root: root.to_path_buf(),
            profile,
            job_revision,
        });
    }

    pub fn on_watch_changes(
        &self,
        checkout_id: &str,
        root: &Path,
        changes: &WorkspaceFileChanges,
    ) {
        if !self.writer_enabled() {
            return;
        }
        if changes.resync_required {
            self.bump_resync(checkout_id);
            for profile in [IndexIgnoreProfile::RespectGitignore, IndexIgnoreProfile::IncludeAll] {
                let job_revision = self.profile_revision(checkout_id, profile).unwrap_or(1);
                self.work_tx.send(IndexWork::ApplyChanges {
                    checkout_id: checkout_id.to_string(),
                    root: root.to_path_buf(),
                    profile,
                    resync_required: true,
                    changes: Vec::new(),
                    job_revision,
                });
            }
            return;
        }
        if changes.changes.is_empty() {
            return;
        }
        if changes_require_full_rebuild(&changes.changes) {
            for profile in [IndexIgnoreProfile::RespectGitignore, IndexIgnoreProfile::IncludeAll] {
                self.bump_profile_revision(checkout_id, profile);
                self.set_profile_phase(checkout_id, profile, ProfilePhase::Dirty);
                let job_revision = self.profile_revision(checkout_id, profile).unwrap_or(1);
                let _ = self.work_tx.send(IndexWork::FullBuild {
                    checkout_id: checkout_id.to_string(),
                    root: root.to_path_buf(),
                    profile,
                    job_revision,
                });
            }
            return;
        }
        self.mark_dirty_raw(checkout_id);
        for profile in [IndexIgnoreProfile::RespectGitignore, IndexIgnoreProfile::IncludeAll] {
            let job_revision = self.profile_revision(checkout_id, profile).unwrap_or(1);
            self.work_tx.send(IndexWork::ApplyChanges {
                checkout_id: checkout_id.to_string(),
                root: root.to_path_buf(),
                profile,
                resync_required: false,
                changes: changes.changes.clone(),
                job_revision,
            });
        }
    }

    pub fn mark_dirty_raw(&self, checkout_id: &str) {
        let mut checkouts = self.checkouts.lock().expect("index checkouts");
        if let Some(state) = checkouts.get_mut(checkout_id) {
            for profile in state.profiles.values_mut() {
                profile.revision += 1;
                if profile.phase == ProfilePhase::Ready {
                    profile.phase = ProfilePhase::Dirty;
                }
            }
        }
    }

    fn bump_resync(&self, checkout_id: &str) {
        let mut checkouts = self.checkouts.lock().expect("index checkouts");
        if let Some(state) = checkouts.get_mut(checkout_id) {
            for profile in state.profiles.values_mut() {
                profile.revision += 1;
                profile.phase = ProfilePhase::Resyncing;
            }
        }
    }

    fn bump_profile_revision(&self, checkout_id: &str, profile: IndexIgnoreProfile) -> u64 {
        let mut checkouts = self.checkouts.lock().expect("index checkouts");
        if let Some(state) = checkouts.get_mut(checkout_id) {
            if let Some(profile_state) = state.profiles.get_mut(&profile) {
                profile_state.revision += 1;
                return profile_state.revision;
            }
        }
        1
    }

    fn profile_revision(&self, checkout_id: &str, profile: IndexIgnoreProfile) -> Option<u64> {
        let checkouts = self.checkouts.lock().expect("index checkouts");
        checkouts
            .get(checkout_id)?
            .profiles
            .get(&profile)
            .map(|state| state.revision)
    }

    pub fn profile_revision_if_ready(
        &self,
        checkout_id: &str,
        include_ignored: bool,
    ) -> Option<u64> {
        let profile = profile_for_include_ignored(include_ignored);
        let checkouts = self.checkouts.lock().expect("index checkouts");
        let state = checkouts.get(checkout_id)?;
        let profile_state = state.profiles.get(&profile)?;
        if profile_state.phase == ProfilePhase::Ready {
            Some(profile_state.revision)
        } else {
            None
        }
    }

    pub fn can_serve_indexed_read(
        &self,
        checkout_id: &str,
        include_ignored: bool,
    ) -> Option<u64> {
        if !self.reads_enabled() {
            return None;
        }
        self.profile_revision_if_ready(checkout_id, include_ignored)
    }

    pub fn search_indexed(
        &self,
        checkout_id: &str,
        root: &Path,
        query: &str,
        mode: WorkspaceContentMatchMode,
        include_ignored: bool,
        limit: usize,
        cancel: &AtomicBool,
        expected_revision: u64,
    ) -> Result<SearchWorkspaceContentResponse, WorkspaceFilesError> {
        validate_workspace_search_query(query)?;
        let limit = limit.min(MAX_SEARCH_RESULTS);
        let profile = profile_for_include_ignored(include_ignored);
        let db_path = {
            let checkouts = self.checkouts.lock().expect("index checkouts");
            let state = checkouts
                .get(checkout_id)
                .ok_or_else(|| WorkspaceFilesError::Io("content index checkout missing".into()))?;
            let profile_state = state
                .profiles
                .get(&profile)
                .ok_or_else(|| WorkspaceFilesError::Io("content index profile missing".into()))?;
            if profile_state.phase != ProfilePhase::Ready
                || profile_state.revision != expected_revision
            {
                return Err(WorkspaceFilesError::Io("content index stale revision".into()));
            }
            profile_state.db_path.clone()
        };

        if !db_path.is_file() {
            return Err(WorkspaceFilesError::Io("content index missing".into()));
        }
        let conn = open_index_db_readonly(&db_path)?;
        let mut matcher = ContentLineMatcher::new(query, mode)?;
        let mut budget = ScanBudgetState::new();
        let mut matches = Vec::new();
        let mut files_scanned = 0u32;
        let mut skipped_binary = 0u32;
        let mut skipped_too_large = 0u32;
        let mut skipped_unsupported = 0u32;
        let skipped_errors = 0u32;
        let mut hit_result_cap = false;
        let mut scan_incomplete = false;
        let mut incomplete_reason = None;

        let mut stmt = conn
            .prepare(
                "SELECT f.wire_path, f.mtime_nsec, f.size_bytes, f.content_hash, f.skipped_binary, \
                 f.skipped_too_large, f.skipped_unsupported, l.line_no, l.text \
                 FROM file f LEFT JOIN line l ON l.file_id = f.id \
                 ORDER BY f.wire_path ASC, l.line_no ASC",
            )
            .map_err(sqlite_err)?;

        let rows = stmt
            .query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, i64>(1)?,
                    row.get::<_, i64>(2)?,
                    row.get::<_, Vec<u8>>(3)?,
                    row.get::<_, i32>(4)?,
                    row.get::<_, i32>(5)?,
                    row.get::<_, i32>(6)?,
                    row.get::<_, Option<u32>>(7)?,
                    row.get::<_, Option<String>>(8)?,
                ))
            })
            .map_err(sqlite_err)?;

        let mut current_file: Option<String> = None;
        for row in rows {
            if cancel.load(Ordering::Relaxed) {
                return Err(WorkspaceFilesError::Io(
                    "workspace content search cancelled".into(),
                ));
            }
            if !budget.record(0) {
                scan_incomplete = true;
                incomplete_reason =
                    Some(zeron_proto::WorkspaceContentSearchIncompleteReason::ScanBudgetExceeded);
                break;
            }
            let (
                wire_path,
                mtime_nsec,
                size_bytes,
                _content_hash,
                skipped_binary_flag,
                skipped_too_large_flag,
                skipped_unsupported_flag,
                line_no,
                text,
            ) = row.map_err(sqlite_err)?;

            if current_file.as_deref() != Some(wire_path.as_str()) {
                current_file = Some(wire_path.clone());
                if !budget.record(size_bytes as u64) {
                    scan_incomplete = true;
                    incomplete_reason =
                        Some(zeron_proto::WorkspaceContentSearchIncompleteReason::ScanBudgetExceeded);
                    break;
                }
                if skipped_too_large_flag != 0 {
                    skipped_too_large += 1;
                    continue;
                }
                if skipped_unsupported_flag != 0 {
                    skipped_unsupported += 1;
                }
                if skipped_binary_flag != 0 {
                    skipped_binary += 1;
                }
                if !live_metadata_matches(root, &wire_path, mtime_nsec, size_bytes) {
                    return Err(WorkspaceFilesError::Io(
                        "content index metadata mismatch".into(),
                    ));
                }
                files_scanned += 1;
            }

            if let (Some(line_no), Some(text)) = (line_no, text) {
                if !budget.record(text.len() as u64) {
                    scan_incomplete = true;
                    incomplete_reason =
                        Some(zeron_proto::WorkspaceContentSearchIncompleteReason::ScanBudgetExceeded);
                    break;
                }
                matcher.match_line(
                    &wire_path,
                    line_no,
                    text.as_bytes(),
                    &mut matches,
                    limit,
                    &mut hit_result_cap,
                );
                if hit_result_cap && matches.len() >= limit {
                    break;
                }
            }
        }

        {
            let checkouts = self.checkouts.lock().expect("index checkouts");
            let state = checkouts
                .get(checkout_id)
                .ok_or_else(|| WorkspaceFilesError::Io("content index checkout missing".into()))?;
            let profile_state = state
                .profiles
                .get(&profile)
                .ok_or_else(|| WorkspaceFilesError::Io("content index profile missing".into()))?;
            if profile_state.revision != expected_revision {
                return Err(WorkspaceFilesError::Io("content index stale revision".into()));
            }
        }

        Ok(build_search_response(
            matches,
            mode,
            limit,
            files_scanned,
            skipped_binary,
            skipped_too_large,
            skipped_unsupported,
            skipped_errors,
            scan_incomplete,
            incomplete_reason,
            hit_result_cap,
        ))
    }

    /// Signal the background worker to stop without joining (safe from async shutdown).
    pub fn shutdown(&self) {
        self.cancel.cancel();
        self.work_tx.send(IndexWork::Shutdown);
    }

    /// Block until the worker thread exits (tests and synchronous teardown only).
    pub fn shutdown_and_join(&self) {
        self.shutdown();
        if let Some(handle) = self.worker.lock().expect("index worker").take() {
            let _ = handle.join();
        }
    }

    fn db_path(&self, checkout_id: &str, profile: IndexIgnoreProfile) -> PathBuf {
        self.index_dir.join(format!("{}_{}.sqlite", checkout_id, profile.db_suffix()))
    }

    fn set_profile_phase(&self, checkout_id: &str, profile: IndexIgnoreProfile, phase: ProfilePhase) {
        let mut checkouts = self.checkouts.lock().expect("index checkouts");
        if let Some(state) = checkouts.get_mut(checkout_id) {
            if let Some(profile_state) = state.profiles.get_mut(&profile) {
                profile_state.phase = phase;
            }
        }
    }

    fn try_finish_ready(
        &self,
        checkout_id: &str,
        profile: IndexIgnoreProfile,
        job_revision: u64,
    ) {
        let mut checkouts = self.checkouts.lock().expect("index checkouts");
        if let Some(state) = checkouts.get_mut(checkout_id) {
            if let Some(profile_state) = state.profiles.get_mut(&profile) {
                if profile_state.revision != job_revision {
                    return;
                }
                profile_state.phase = ProfilePhase::Ready;
                profile_state.last_used = Instant::now();
            }
        }
        enforce_global_disk_cap(self, &self.index_dir);
    }

    fn mark_profile_dirty(&self, checkout_id: &str, profile: IndexIgnoreProfile) {
        self.set_profile_phase(checkout_id, profile, ProfilePhase::Dirty);
    }

    fn profile_phase(&self, checkout_id: &str, profile: IndexIgnoreProfile) -> Option<ProfilePhase> {
        let checkouts = self.checkouts.lock().expect("index checkouts");
        checkouts
            .get(checkout_id)?
            .profiles
            .get(&profile)
            .map(|state| state.phase)
    }

    fn schedule_full_build(&self, checkout_id: &str, root: &Path, profile: IndexIgnoreProfile) {
        self.set_profile_phase(checkout_id, profile, ProfilePhase::Building);
        let job_revision = self.bump_profile_revision(checkout_id, profile);
        self.work_tx.send(IndexWork::FullBuild {
            checkout_id: checkout_id.to_string(),
            root: root.to_path_buf(),
            profile,
            job_revision,
        });
    }

    fn is_db_in_active_use(&self, db_path: &Path) -> bool {
        let checkouts = self.checkouts.lock().expect("index checkouts");
        for state in checkouts.values() {
            for profile_state in state.profiles.values() {
                if profile_state.db_path == db_path
                    && matches!(
                        profile_state.phase,
                        ProfilePhase::Ready
                            | ProfilePhase::Building
                            | ProfilePhase::Reconciling
                            | ProfilePhase::Resyncing
                    )
                {
                    return true;
                }
            }
        }
        false
    }
}

impl Drop for ContentIndexManager {
    fn drop(&mut self) {
        self.cancel.cancel();
        self.work_tx.send(IndexWork::Shutdown);
    }
}

fn profile_for_include_ignored(include_ignored: bool) -> IndexIgnoreProfile {
    if include_ignored {
        IndexIgnoreProfile::IncludeAll
    } else {
        IndexIgnoreProfile::RespectGitignore
    }
}

fn index_worker_blocking(manager: Weak<ContentIndexManager>, work_rx: IndexWorkReceiver) {
    while let Some(work) = work_rx.recv() {
        if matches!(work, IndexWork::Shutdown) {
            break;
        }
        let Some(manager) = manager.upgrade() else {
            break;
        };
        if manager.cancel.is_cancelled() {
            break;
        }
        while manager.foreground_searches.load(Ordering::Acquire) > 0 {
            if manager.cancel.is_cancelled() {
                return;
            }
            std::thread::sleep(INDEX_YIELD_SLEEP);
        }
        match work {
            IndexWork::Shutdown => break,
            IndexWork::FullBuild {
                checkout_id,
                root,
                profile,
                job_revision,
            } => {
                manager.set_profile_phase(&checkout_id, profile, ProfilePhase::Building);
                let db_path = manager.db_path(&checkout_id, profile);
                let built = run_full_build(
                    &db_path,
                    &root,
                    profile,
                    &manager.cancel,
                    &manager.index_dir,
                    &manager.foreground_searches,
                );
                if built {
                    manager.try_finish_ready(&checkout_id, profile, job_revision);
                } else {
                    manager.mark_profile_dirty(&checkout_id, profile);
                }
            }
            IndexWork::Reconcile {
                checkout_id,
                root,
                profile,
                job_revision,
            } => {
                manager.set_profile_phase(&checkout_id, profile, ProfilePhase::Reconciling);
                let db_path = manager.db_path(&checkout_id, profile);
                if reconcile_index(&db_path, &root, profile, &manager.cancel).is_ok() {
                    manager.set_profile_phase(&checkout_id, profile, ProfilePhase::Dirty);
                    let job_revision = manager
                        .profile_revision(&checkout_id, profile)
                        .unwrap_or(job_revision);
                    manager.work_tx.send(IndexWork::FullBuild {
                        checkout_id: checkout_id.clone(),
                        root: root.clone(),
                        profile,
                        job_revision,
                    });
                } else {
                    manager.mark_profile_dirty(&checkout_id, profile);
                }
            }
            IndexWork::ApplyChanges {
                checkout_id,
                root,
                profile,
                resync_required,
                changes,
                job_revision,
            } => {
                let job_revision = if job_revision == 0 {
                    manager.profile_revision(&checkout_id, profile).unwrap_or(1)
                } else {
                    job_revision
                };
                if resync_required {
                    manager.set_profile_phase(&checkout_id, profile, ProfilePhase::Resyncing);
                    let db_path = manager.db_path(&checkout_id, profile);
                    let built = run_full_build(
                        &db_path,
                        &root,
                        profile,
                        &manager.cancel,
                        &manager.index_dir,
                        &manager.foreground_searches,
                    );
                    if built {
                        manager.try_finish_ready(&checkout_id, profile, job_revision);
                    } else {
                        manager.mark_profile_dirty(&checkout_id, profile);
                    }
                    continue;
                }
                let phase = manager
                    .profile_phase(&checkout_id, profile)
                    .unwrap_or(ProfilePhase::Missing);
                let db_path = manager.db_path(&checkout_id, profile);
                if phase == ProfilePhase::Missing
                    || phase == ProfilePhase::Building
                    || phase == ProfilePhase::Reconciling
                    || !index_profile_trusted(&db_path)
                {
                    manager.schedule_full_build(&checkout_id, &root, profile);
                    continue;
                }
                let ok = apply_incremental_changes(
                    &db_path,
                    &root,
                    profile,
                    &changes,
                    &manager.cancel,
                )
                .is_ok();
                if ok {
                    manager.try_finish_ready(&checkout_id, profile, job_revision);
                } else {
                    manager.mark_profile_dirty(&checkout_id, profile);
                }
            }
            IndexWork::EvictCheckout { checkout_id } => {
                let paths = [
                    manager.db_path(&checkout_id, IndexIgnoreProfile::RespectGitignore),
                    manager.db_path(&checkout_id, IndexIgnoreProfile::IncludeAll),
                ];
                for path in paths {
                    remove_index_files(&path);
                }
                manager.checkouts.lock().expect("index checkouts").remove(&checkout_id);
            }
        }
    }
}

fn changes_require_full_rebuild(changes: &[WorkspaceFileChange]) -> bool {
    changes.iter().any(|change| {
        change.path == ".gitignore"
            || change.path.ends_with("/.gitignore")
            || change.old_path.as_deref().is_some_and(|old| {
                old == ".gitignore" || old.ends_with("/.gitignore")
            })
    })
}

fn sqlite_err(error: rusqlite::Error) -> WorkspaceFilesError {
    WorkspaceFilesError::Io(format!("content index sqlite: {error}"))
}

fn open_index_db(path: &Path) -> Result<Connection, WorkspaceFilesError> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|error| WorkspaceFilesError::Io(error.to_string()))?;
    }
    let conn = Connection::open(path).map_err(sqlite_err)?;
    configure_index_connection(&conn)?;
    Ok(conn)
}

fn open_index_db_readonly(path: &Path) -> Result<Connection, WorkspaceFilesError> {
    let flags = OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX;
    let conn = Connection::open_with_flags(path, flags).map_err(|_| {
        WorkspaceFilesError::Io("content index sqlite open failed".into())
    })?;
    configure_index_connection(&conn)?;
    Ok(conn)
}

fn configure_index_connection(conn: &Connection) -> Result<(), WorkspaceFilesError> {
    conn.busy_timeout(Duration::from_secs(5)).map_err(sqlite_err)?;
    conn.pragma_update(None, "journal_mode", "WAL")
        .map_err(sqlite_err)?;
    conn.pragma_update(None, "cache_size", INDEX_SQLITE_CACHE_KIB)
        .map_err(sqlite_err)?;
    Ok(())
}

fn init_schema(conn: &Connection) -> Result<(), WorkspaceFilesError> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS meta (
            key TEXT PRIMARY KEY,
            value TEXT NOT NULL
         ) STRICT;
         CREATE TABLE IF NOT EXISTS file (
            id INTEGER PRIMARY KEY,
            wire_path TEXT NOT NULL UNIQUE,
            mtime_nsec INTEGER NOT NULL,
            size_bytes INTEGER NOT NULL,
            content_hash BLOB NOT NULL,
            skipped_binary INTEGER NOT NULL DEFAULT 0,
            skipped_too_large INTEGER NOT NULL DEFAULT 0,
            skipped_unsupported INTEGER NOT NULL DEFAULT 0
         ) STRICT;
         CREATE TABLE IF NOT EXISTS line (
            file_id INTEGER NOT NULL,
            line_no INTEGER NOT NULL,
            text TEXT NOT NULL,
            PRIMARY KEY (file_id, line_no),
            FOREIGN KEY (file_id) REFERENCES file(id) ON DELETE CASCADE
         ) STRICT;",
    )
    .map_err(sqlite_err)?;
    let version = conn
        .query_row(
            "SELECT value FROM meta WHERE key = 'schema_version'",
            [],
            |row| row.get::<_, String>(0),
        )
        .optional()
        .map_err(sqlite_err)?;
    if version.as_deref() != Some(CONTENT_INDEX_SCHEMA_VERSION.to_string().as_str()) {
        conn.execute("DELETE FROM line", []).map_err(sqlite_err)?;
        conn.execute("DELETE FROM file", []).map_err(sqlite_err)?;
        conn.execute("DELETE FROM meta", []).map_err(sqlite_err)?;
        conn.execute(
            "INSERT INTO meta(key, value) VALUES ('schema_version', ?1)",
            params![CONTENT_INDEX_SCHEMA_VERSION.to_string()],
        )
        .map_err(sqlite_err)?;
    }
    Ok(())
}

fn read_epoch(db_path: &Path) -> Option<u64> {
    let conn = Connection::open(db_path).ok()?;
    conn.query_row(
        "SELECT value FROM meta WHERE key = 'epoch'",
        [],
        |row| row.get::<_, String>(0),
    )
    .optional()
    .ok()
    .flatten()
    .and_then(|value| value.parse().ok())
}

fn index_profile_trusted(db_path: &Path) -> bool {
    if !db_path.is_file() {
        return false;
    }
    let conn = match Connection::open(db_path) {
        Ok(conn) => conn,
        Err(_) => return false,
    };
    conn.query_row(
        "SELECT value FROM meta WHERE key = ?1",
        params![META_BUILD_COMPLETE],
        |row| row.get::<_, String>(0),
    )
    .optional()
    .ok()
    .flatten()
    .is_some_and(|value| value == "1")
}

fn set_build_complete(conn: &Connection) -> Result<(), WorkspaceFilesError> {
    conn.execute(
        "INSERT INTO meta(key, value) VALUES (?1, '1')
         ON CONFLICT(key) DO UPDATE SET value = excluded.value",
        params![META_BUILD_COMPLETE],
    )
    .map_err(sqlite_err)?;
    Ok(())
}

fn write_epoch(conn: &Connection, epoch: u64) -> Result<(), WorkspaceFilesError> {
    conn.execute(
        "INSERT INTO meta(key, value) VALUES ('epoch', ?1)
         ON CONFLICT(key) DO UPDATE SET value = excluded.value",
        params![epoch.to_string()],
    )
    .map_err(sqlite_err)?;
    Ok(())
}

fn run_full_build(
    db_path: &Path,
    root: &Path,
    profile: IndexIgnoreProfile,
    cancel: &CancellationToken,
    index_dir: &Path,
    foreground_searches: &AtomicUsize,
) -> bool {
    if cancel.is_cancelled() {
        return false;
    }
    if !content_index_disk_headroom(index_dir, db_path) {
        return false;
    }
    let conn = match open_index_db(db_path) {
        Ok(conn) => conn,
        Err(_) => return false,
    };
    if init_schema(&conn).is_err() {
        remove_index_files(db_path);
        return false;
    }
    let epoch = read_epoch(db_path).unwrap_or(1) + 1;
    if write_epoch(&conn, epoch).is_err() {
        return false;
    }
    let tx = match conn.unchecked_transaction() {
        Ok(tx) => tx,
        Err(_) => return false,
    };
    if tx.execute("DELETE FROM line", []).is_err() || tx.execute("DELETE FROM file", []).is_err() {
        return false;
    }
    let root = match std::fs::canonicalize(root) {
        Ok(root) => root,
        Err(_) => return false,
    };
    let builder = content_search_walk_builder(&root, profile.include_ignored());
    let mut indexed_files = 0u32;
    let mut build_budget = ScanBudgetState::new();
    for entry in builder.build() {
        if cancel.is_cancelled() {
            return false;
        }
        while foreground_searches.load(Ordering::Acquire) > 0 {
            if cancel.is_cancelled() {
                return false;
            }
            std::thread::sleep(INDEX_YIELD_SLEEP);
        }
        if indexed_files % INDEX_FILES_PER_YIELD == 0 {
            std::thread::sleep(INDEX_YIELD_SLEEP);
            if !content_index_disk_headroom(index_dir, db_path) {
                return false;
            }
        }
        let entry = match entry {
            Ok(entry) => entry,
            Err(_) => continue,
        };
        if entry.depth() == 0 {
            continue;
        }
        let relative = match entry.path().strip_prefix(&root) {
            Ok(relative) if !contains_git_component(relative) => relative,
            _ => continue,
        };
        if !entry.file_type().is_some_and(|ft| ft.is_file()) {
            continue;
        }
        let wire_path = match path_to_wire(relative) {
            Ok(path) => path,
            Err(_) => continue,
        };
        if is_internal_temp_wire_path(&wire_path) {
            continue;
        }
        indexed_files += 1;
        let file_bytes = metadata_for_workspace_file(&root, &wire_path)
            .map(|meta| meta.len())
            .unwrap_or(0);
        if !build_budget.record(file_bytes) {
            return false;
        }
        if index_one_file(&tx, &root, &wire_path).is_err() {
            return false;
        }
    }
    if tx.commit().is_err() {
        return false;
    }
    set_build_complete(&conn).is_ok()
}

fn reconcile_index(
    db_path: &Path,
    root: &Path,
    profile: IndexIgnoreProfile,
    cancel: &CancellationToken,
) -> Result<(), WorkspaceFilesError> {
    if cancel.is_cancelled() {
        return Err(WorkspaceFilesError::Io("content index cancelled".into()));
    }
    let conn = open_index_db(db_path)?;
    init_schema(&conn)?;
    let root = std::fs::canonicalize(root)
        .map_err(|error| WorkspaceFilesError::Io(error.to_string()))?;
    let mut stmt = conn
        .prepare("SELECT wire_path, mtime_nsec, size_bytes, content_hash FROM file")
        .map_err(sqlite_err)?;
    let rows = stmt
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, i64>(1)?,
                row.get::<_, i64>(2)?,
                row.get::<_, Vec<u8>>(3)?,
            ))
        })
        .map_err(sqlite_err)?;
    let mut stale_paths = Vec::new();
    for row in rows {
        if cancel.is_cancelled() {
            return Err(WorkspaceFilesError::Io("content index cancelled".into()));
        }
        let (wire_path, mtime_nsec, size_bytes, _content_hash) = row.map_err(sqlite_err)?;
        if !live_metadata_matches(&root, &wire_path, mtime_nsec, size_bytes) {
            stale_paths.push(wire_path);
        }
    }
    if !stale_paths.is_empty() {
        let tx = conn.unchecked_transaction().map_err(sqlite_err)?;
        for path in stale_paths {
            delete_file_rows(&tx, &path)?;
        }
        tx.commit().map_err(sqlite_err)?;
    }
    // Ensure walk-visible files exist (startup gap fill).
    let builder = content_search_walk_builder(&root, profile.include_ignored());
    let mut seen = HashSet::new();
    for entry in builder.build() {
        if cancel.is_cancelled() {
            break;
        }
        let entry = match entry {
            Ok(entry) => entry,
            Err(_) => continue,
        };
        if entry.depth() == 0 {
            continue;
        }
        let relative = match entry.path().strip_prefix(&root) {
            Ok(relative) if !contains_git_component(relative) => relative,
            _ => continue,
        };
        if !entry.file_type().is_some_and(|ft| ft.is_file()) {
            continue;
        };
        let wire_path = match path_to_wire(relative) {
            Ok(path) => path,
            Err(_) => continue,
        };
        if is_internal_temp_wire_path(&wire_path) {
            continue;
        }
        seen.insert(wire_path);
    }
    let mut existing = HashSet::new();
    let mut stmt = conn
        .prepare("SELECT wire_path FROM file")
        .map_err(sqlite_err)?;
    let rows = stmt
        .query_map([], |row| row.get::<_, String>(0))
        .map_err(sqlite_err)?;
    for row in rows {
        existing.insert(row.map_err(sqlite_err)?);
    }
    let missing = seen.difference(&existing).cloned().collect::<Vec<_>>();
    if !missing.is_empty() {
        let tx = conn.unchecked_transaction().map_err(sqlite_err)?;
        for wire_path in missing {
            index_one_file(&tx, &root, &wire_path)?;
        }
        tx.commit().map_err(sqlite_err)?;
    }
    Ok(())
}

fn apply_incremental_changes(
    db_path: &Path,
    root: &Path,
    profile: IndexIgnoreProfile,
    changes: &[WorkspaceFileChange],
    cancel: &CancellationToken,
) -> Result<(), WorkspaceFilesError> {
    if cancel.is_cancelled() {
        return Err(WorkspaceFilesError::Io("content index cancelled".into()));
    }
    if !index_profile_trusted(db_path) {
        return Err(WorkspaceFilesError::Io("content index not initialized".into()));
    }
    let conn = open_index_db(db_path)?;
    init_schema(&conn)?;
    let mut build_budget = ScanBudgetState::new();
    let epoch = read_epoch(db_path).unwrap_or(1) + 1;
    write_epoch(&conn, epoch)?;
    let tx = conn.unchecked_transaction().map_err(sqlite_err)?;
    for change in changes {
        if cancel.is_cancelled() {
            return Err(WorkspaceFilesError::Io("content index cancelled".into()));
        }
        match change.kind {
            WorkspaceFileChangeKind::Removed => {
                delete_wire_path_prefix(&tx, &change.path)?;
            }
            WorkspaceFileChangeKind::Renamed => {
                if let Some(old_path) = &change.old_path {
                    delete_wire_path_prefix(&tx, old_path)?;
                }
                apply_path_created_or_modified(
                    &tx,
                    root,
                    profile,
                    &change.path,
                    &mut build_budget,
                    cancel,
                )?;
            }
            WorkspaceFileChangeKind::Created | WorkspaceFileChangeKind::Modified => {
                apply_path_created_or_modified(
                    &tx,
                    root,
                    profile,
                    &change.path,
                    &mut build_budget,
                    cancel,
                )?;
            }
        }
    }
    tx.commit().map_err(sqlite_err)?;
    Ok(())
}

fn apply_path_created_or_modified(
    conn: &Connection,
    root: &Path,
    profile: IndexIgnoreProfile,
    wire_path: &str,
    budget: &mut ScanBudgetState,
    cancel: &CancellationToken,
) -> Result<(), WorkspaceFilesError> {
    if cancel.is_cancelled() {
        return Err(WorkspaceFilesError::Io("content index cancelled".into()));
    }
    if is_directory_wire_path(root, wire_path) {
        for file in walk_profile_files_under_prefix(root, profile, wire_path) {
            if cancel.is_cancelled() {
                return Err(WorkspaceFilesError::Io("content index cancelled".into()));
            }
            if should_index_path(root, profile, &file) {
                record_file_budget(root, &file, budget)?;
                index_one_file(conn, root, &file)?;
            } else {
                delete_file_rows(conn, &file)?;
            }
        }
        return Ok(());
    }
    if should_index_path(root, profile, wire_path) {
        record_file_budget(root, wire_path, budget)?;
        index_one_file(conn, root, wire_path)?;
    } else {
        delete_file_rows(conn, wire_path)?;
    }
    Ok(())
}

fn record_file_budget(
    root: &Path,
    wire_path: &str,
    budget: &mut ScanBudgetState,
) -> Result<(), WorkspaceFilesError> {
    let bytes = metadata_for_workspace_file(root, wire_path)
        .map(|meta| meta.len())
        .unwrap_or(0);
    if !budget.record(bytes) {
        return Err(WorkspaceFilesError::Io("content index build budget exceeded".into()));
    }
    Ok(())
}

fn is_directory_wire_path(root: &Path, wire_path: &str) -> bool {
    WorkspaceRelativePath::file(wire_path)
        .ok()
        .map(|relative| root.join(relative.as_path()).is_dir())
        .unwrap_or(false)
}

fn walk_profile_files_under_prefix(
    root: &Path,
    profile: IndexIgnoreProfile,
    prefix: &str,
) -> Vec<String> {
    let prefix = prefix.trim_end_matches('/');
    let root = std::fs::canonicalize(root).unwrap_or_else(|_| root.to_path_buf());
    let mut paths = Vec::new();
    for entry in content_search_walk_builder(&root, profile.include_ignored()).build().flatten() {
        if entry.depth() == 0 {
            continue;
        }
        let relative = match entry.path().strip_prefix(&root) {
            Ok(relative) if !contains_git_component(relative) => relative,
            _ => continue,
        };
        if !entry.file_type().is_some_and(|ft| ft.is_file()) {
            continue;
        }
        if let Ok(wire) = path_to_wire(relative) {
            if wire_path_has_prefix(&wire, prefix) {
                paths.push(wire);
            }
        }
    }
    paths
}

fn should_index_path(root: &Path, profile: IndexIgnoreProfile, wire_path: &str) -> bool {
    let relative = match WorkspaceRelativePath::file(wire_path) {
        Ok(relative) => relative,
        Err(_) => return false,
    };
    if contains_git_component(relative.as_path()) || is_internal_temp_wire_path(wire_path) {
        return false;
    }
    let root = std::fs::canonicalize(root).unwrap_or_else(|_| root.to_path_buf());
    let abs = root.join(relative.as_path());
    let abs = std::fs::canonicalize(&abs).unwrap_or(abs);
    if !abs.is_file() {
        return false;
    }
    for entry in content_search_walk_builder(&root, profile.include_ignored()).build().flatten() {
        if entry.path() == abs && entry.file_type().is_some_and(|ft| ft.is_file()) {
            return true;
        }
    }
    false
}

fn wire_path_has_prefix(wire_path: &str, prefix: &str) -> bool {
    let prefix = prefix.trim_end_matches('/');
    if wire_path == prefix {
        return true;
    }
    wire_path.starts_with(&format!("{prefix}/"))
}

fn delete_wire_path_prefix(conn: &Connection, prefix: &str) -> Result<(), WorkspaceFilesError> {
    let prefix = prefix.trim_end_matches('/');
    let mut stmt = conn
        .prepare("SELECT id, wire_path FROM file")
        .map_err(sqlite_err)?;
    let rows = stmt
        .query_map([], |row| Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?)))
        .map_err(sqlite_err)?;
    let mut file_ids = Vec::new();
    for row in rows {
        let (id, wire_path) = row.map_err(sqlite_err)?;
        if wire_path_has_prefix(&wire_path, prefix) {
            file_ids.push(id);
        }
    }
    for file_id in file_ids {
        conn.execute("DELETE FROM line WHERE file_id = ?1", params![file_id])
            .map_err(sqlite_err)?;
        conn.execute("DELETE FROM file WHERE id = ?1", params![file_id])
            .map_err(sqlite_err)?;
    }
    Ok(())
}

fn delete_file_rows(conn: &Connection, wire_path: &str) -> Result<(), WorkspaceFilesError> {
    let file_id: Option<i64> = conn
        .query_row(
            "SELECT id FROM file WHERE wire_path = ?1",
            params![wire_path],
            |row| row.get(0),
        )
        .optional()
        .map_err(sqlite_err)?;
    if let Some(file_id) = file_id {
        conn.execute("DELETE FROM line WHERE file_id = ?1", params![file_id])
            .map_err(sqlite_err)?;
        conn.execute("DELETE FROM file WHERE id = ?1", params![file_id])
            .map_err(sqlite_err)?;
    }
    Ok(())
}

fn index_one_file(conn: &Connection, root: &Path, wire_path: &str) -> Result<(), WorkspaceFilesError> {
    delete_file_rows(conn, wire_path)?;
    let metadata = match metadata_for_workspace_file(root, wire_path) {
        Ok(metadata) => metadata,
        Err(WorkspaceFilesError::Unsupported(_)) => {
            return insert_skipped_file(conn, wire_path, 0, 0, 1, 0, &[]);
        }
        Err(error) => return Err(error),
    };
    if metadata.len() > CONTENT_SEARCH_MAX_BYTES_PER_FILE {
        return insert_skipped_file(conn, wire_path, 0, 1, 0, 0, &[]);
    }
    let relative = WorkspaceRelativePath::file(wire_path)?;
    let collected = collect_file_lines_for_index(root, &relative, wire_path);
    match collected {
        Ok(content) => {
            let (mtime_nsec, size_bytes) = file_identity(&metadata);
            insert_file_with_lines(
                conn,
                wire_path,
                mtime_nsec,
                size_bytes,
                &content.content_hash,
                content.skipped_binary,
                false,
                content.skipped_unsupported,
                content.lines.as_slice(),
            )?;
            Ok(())
        }
        Err(error) => Err(error),
    }
}

fn insert_skipped_file(
    conn: &Connection,
    wire_path: &str,
    skipped_binary: u8,
    skipped_too_large: u8,
    skipped_unsupported: u8,
    _skipped_errors: u8,
    content_hash: &[u8],
) -> Result<(), WorkspaceFilesError> {
    insert_file_with_lines(
        conn,
        wire_path,
        0,
        0,
        content_hash,
        skipped_binary != 0,
        skipped_too_large != 0,
        skipped_unsupported != 0,
        &[],
    )
}

fn insert_file_with_lines(
    conn: &Connection,
    wire_path: &str,
    mtime_nsec: i64,
    size_bytes: i64,
    content_hash: &[u8],
    skipped_binary: bool,
    skipped_too_large: bool,
    skipped_unsupported: bool,
    lines: &[(u32, String)],
) -> Result<(), WorkspaceFilesError> {
    conn.execute(
        "INSERT INTO file(wire_path, mtime_nsec, size_bytes, content_hash, skipped_binary, skipped_too_large, skipped_unsupported)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        params![
            wire_path,
            mtime_nsec,
            size_bytes,
            content_hash,
            skipped_binary as i32,
            skipped_too_large as i32,
            skipped_unsupported as i32,
        ],
    )
    .map_err(sqlite_err)?;
    let file_id = conn.last_insert_rowid();
    for (line_no, text) in lines {
        conn.execute(
            "INSERT INTO line(file_id, line_no, text) VALUES (?1, ?2, ?3)",
            params![file_id, line_no, text],
        )
        .map_err(sqlite_err)?;
    }
    Ok(())
}

fn file_identity(metadata: &std::fs::Metadata) -> (i64, i64) {
    let size_bytes = metadata.len() as i64;
    let mtime = metadata.modified().unwrap_or(SystemTime::UNIX_EPOCH);
    let mtime_nsec = mtime
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_nanos() as i64)
        .unwrap_or(0);
    (mtime_nsec, size_bytes)
}

fn live_metadata_matches(
    root: &Path,
    wire_path: &str,
    stored_mtime_nsec: i64,
    stored_size: i64,
) -> bool {
    let metadata = match metadata_for_workspace_file(root, wire_path) {
        Ok(metadata) => metadata,
        Err(_) => return false,
    };
    let (mtime_nsec, size_bytes) = file_identity(&metadata);
    mtime_nsec == stored_mtime_nsec && size_bytes == stored_size
}

fn content_index_disk_headroom(index_dir: &Path, active_db: &Path) -> bool {
    let used = total_index_disk_bytes(index_dir);
    let active = index_disk_bytes(active_db);
    used + active <= CONTENT_INDEX_GLOBAL_DISK_BYTES
}

fn total_index_disk_bytes(index_dir: &Path) -> u64 {
    let mut total = 0u64;
    if let Ok(read_dir) = std::fs::read_dir(index_dir) {
        for entry in read_dir.flatten() {
            let path = entry.path();
            if path.extension().is_some_and(|ext| ext == "sqlite") {
                total += index_disk_bytes(&path);
            }
        }
    }
    total
}

fn enforce_global_disk_cap(manager: &ContentIndexManager, index_dir: &Path) {
    let mut entries = Vec::new();
    if let Ok(read_dir) = std::fs::read_dir(index_dir) {
        for entry in read_dir.flatten() {
            let path = entry.path();
            if path.extension().is_some_and(|ext| ext == "sqlite") {
                let size = index_disk_bytes(&path);
                let modified = entry
                    .metadata()
                    .and_then(|meta| meta.modified())
                    .unwrap_or(SystemTime::UNIX_EPOCH);
                entries.push((path, size, modified));
            }
        }
    }
    let total = entries.iter().map(|(_, size, _)| size).sum::<u64>();
    if total <= CONTENT_INDEX_GLOBAL_DISK_BYTES {
        return;
    }
    entries.sort_by_key(|(_, _, modified)| *modified);
    let mut freed = 0u64;
    for (path, size, _) in entries {
        if total - freed <= CONTENT_INDEX_GLOBAL_DISK_BYTES {
            break;
        }
        if manager.is_db_in_active_use(&path) {
            continue;
        }
        manager.mark_db_evicted(&path);
        remove_index_files(&path);
        freed += size;
    }
}

impl ContentIndexManager {
    fn mark_db_evicted(&self, db_path: &Path) {
        let mut checkouts = self.checkouts.lock().expect("index checkouts");
        for state in checkouts.values_mut() {
            for profile_state in state.profiles.values_mut() {
                if profile_state.db_path == db_path {
                    profile_state.phase = ProfilePhase::Missing;
                }
            }
        }
    }
}

fn related_index_path(sqlite_path: &Path, suffix: &str) -> PathBuf {
    if suffix.is_empty() {
        sqlite_path.to_path_buf()
    } else {
        PathBuf::from(format!("{}{}", sqlite_path.to_string_lossy(), suffix))
    }
}

fn index_disk_bytes(sqlite_path: &Path) -> u64 {
    let mut total = 0u64;
    for suffix in ["", "-wal", "-shm"] {
        let path = related_index_path(sqlite_path, suffix);
        if let Ok(meta) = std::fs::metadata(&path) {
            total += meta.len();
        }
    }
    total
}

fn remove_index_files(sqlite_path: &Path) {
    for suffix in ["", "-wal", "-shm"] {
        std::fs::remove_file(related_index_path(sqlite_path, suffix)).ok();
    }
}

/// Test-only: block until both profiles reach Ready or timeout.
pub fn wait_for_index_ready(
    manager: &ContentIndexManager,
    checkout_id: &str,
    include_ignored: bool,
    timeout: Duration,
) -> bool {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if manager
            .profile_revision_if_ready(checkout_id, include_ignored)
            .is_some()
        {
            return true;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    false
}
