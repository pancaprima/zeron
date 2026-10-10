//! Content index parity vs scanner and benchmark harness (env-gated).

use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::time::{Duration, Instant};

use zeron_engine::workspace_content_index::{
    ContentIndexEnvConfig, ContentIndexManager, content_index_reads_enabled,
    parse_content_index_env_flag, wait_for_index_ready,
};
use zeron_proto::{WorkspaceContentMatchMode, WorkspaceFileChange, WorkspaceFileChangeKind};

fn no_cancel() -> AtomicBool {
    AtomicBool::new(false)
}

fn test_manager(index_dir: std::path::PathBuf, reads: bool, writer: bool) -> Arc<ContentIndexManager> {
    ContentIndexManager::new_for_tests(
        index_dir,
        ContentIndexEnvConfig::for_tests(reads, writer),
    )
}

fn fixture_tree(root: &Path) {
    std::fs::create_dir_all(root.join("src")).unwrap();
    std::fs::write(root.join(".gitignore"), "ignored.txt\n").unwrap();
    std::fs::write(root.join("README.md"), "hello world\n").unwrap();
    std::fs::write(root.join("src/lib.rs"), "pub fn needle() {}\n").unwrap();
    std::fs::write(root.join("binary-tail.txt"), b"needle\n\x00\n").unwrap();
    std::fs::write(root.join("ignored.txt"), "needle in ignored\n").unwrap();
    std::fs::write(
        root.join("mixed-utf8.txt"),
        b"needle valid line\n\xff\xfe not utf8\nneedle again\n",
    )
    .unwrap();
}

#[test]
fn content_index_reads_disabled_by_default() {
    assert!(!content_index_reads_enabled());
    assert!(!parse_content_index_env_flag(None));
    assert!(!parse_content_index_env_flag(Some("0")));
    assert!(parse_content_index_env_flag(Some("1")));
    assert!(parse_content_index_env_flag(Some("true")));
}

fn run_indexed_parity(
    root: &Path,
    manager: &ContentIndexManager,
    checkout_id: &str,
    include_ignored: bool,
) {
    for (query, mode) in [
        ("needle", WorkspaceContentMatchMode::Literal),
        ("ndl", WorkspaceContentMatchMode::Fuzzy),
        ("zzznomatch", WorkspaceContentMatchMode::Literal),
    ] {
        let fallback = zeron_engine::workspace_content_search::search_workspace_content_blocking(
            root,
            query,
            mode,
            include_ignored,
            200,
            &no_cancel(),
        )
        .unwrap();
        let revision = manager
            .can_serve_indexed_read(checkout_id, include_ignored)
            .expect("ready revision");
        let indexed = manager
            .search_indexed(
                checkout_id,
                root,
                query,
                mode,
                include_ignored,
                200,
                &no_cancel(),
                revision,
            )
            .unwrap();
        assert_eq!(
            serde_json::to_value(&fallback).unwrap(),
            serde_json::to_value(&indexed).unwrap(),
            "parity failed for query {query} include_ignored={include_ignored}"
        );
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn indexed_search_matches_scanner_on_exhaustive_fixture() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("repo");
    std::fs::create_dir_all(&root).unwrap();
    fixture_tree(&root);
    let root = std::fs::canonicalize(&root).unwrap();
    let checkout_id = "fixture-checkout";

    let index_dir = temp.path().join("index");
    let manager = test_manager(index_dir, true, true);
    manager.on_watch_activated(
        checkout_id.to_string(),
        root.clone(),
        manager.watch_bridge(checkout_id),
    );
    assert!(
        wait_for_index_ready(&manager, checkout_id, false, Duration::from_secs(60)),
        "index did not become ready"
    );
    run_indexed_parity(&root, &manager, checkout_id, false);
    manager.ensure_profile_writer(checkout_id, &root, true);
    assert!(
        wait_for_index_ready(&manager, checkout_id, true, Duration::from_secs(60)),
        "include_all profile did not become ready"
    );
    run_indexed_parity(&root, &manager, checkout_id, true);
    manager.shutdown_and_join();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn content_index_stale_revision_falls_back_via_error() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("repo");
    fixture_tree(&root);
    let root = std::fs::canonicalize(&root).unwrap();
    let checkout_id = "rev-checkout";
    let manager = test_manager(temp.path().join("index"), true, true);
    manager.on_watch_activated(
        checkout_id.to_string(),
        root.clone(),
        manager.watch_bridge(checkout_id),
    );
    assert!(wait_for_index_ready(&manager, checkout_id, false, Duration::from_secs(30)));
    let revision = manager.can_serve_indexed_read(checkout_id, false).unwrap();
    manager.mark_dirty_raw(checkout_id);
    let err = manager
        .search_indexed(
            checkout_id,
            &root,
            "needle",
            WorkspaceContentMatchMode::Literal,
            false,
            200,
            &no_cancel(),
            revision,
        )
        .expect_err("stale revision");
    assert!(err.to_string().contains("stale revision"));
    manager.shutdown_and_join();
}

fn median_nanos(samples: &[u128]) -> u128 {
    let mut sorted = samples.to_vec();
    sorted.sort_unstable();
    sorted[sorted.len() / 2]
}

fn write_bench_fixture(root: &Path, files: usize) {
    std::fs::create_dir_all(root.join("bulk")).unwrap();
    for i in 0..files {
        std::fs::write(
            root.join(format!("bulk/file_{:04}.txt", i)),
            format!(
                "lorem ipsum needle padding line two\nsecond line {i}\nthird padding\n"
            ),
        )
        .unwrap();
    }
}

fn bench_fixture_root() -> (tempfile::TempDir, std::path::PathBuf) {
    let scratch = std::path::Path::new("/root/.hermes/profiles/girlfriend/cache/scratch");
    if scratch.is_dir() {
        let temp = tempfile::TempDir::new_in(scratch).expect("bench tempdir");
        let root = temp.path().join("repo");
        std::fs::create_dir_all(&root).unwrap();
        write_bench_fixture(&root, 1000);
        return (temp, root);
    }
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("repo");
    std::fs::create_dir_all(&root).unwrap();
    write_bench_fixture(&root, 1000);
    (temp, root)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn content_index_benchmark_records_timings() {
    if std::env::var("ZERONA_CONTENT_INDEX_BENCH").as_deref() != Ok("1") {
        eprintln!("skip content_index_benchmark_records_timings (set ZERONA_CONTENT_INDEX_BENCH=1)");
        return;
    }
    let (_temp, root) = bench_fixture_root();
    let root = std::fs::canonicalize(&root).unwrap();
    let checkout_id = "bench-checkout";
    let index_dir = root.parent().unwrap().join("index-store");
    let manager = test_manager(index_dir, true, true);
    manager.on_watch_activated(
        checkout_id.to_string(),
        root.clone(),
        manager.watch_bridge(checkout_id),
    );
    assert!(wait_for_index_ready(&manager, checkout_id, false, Duration::from_secs(180)));
    let revision = manager.can_serve_indexed_read(checkout_id, false).unwrap();

    let rare_query = "needlezzzz";
    let common_query = "needle";
    let mode = WorkspaceContentMatchMode::Literal;

    for query in [rare_query, common_query] {
        let fallback = zeron_engine::workspace_content_search::search_workspace_content_blocking(
            &root,
            query,
            mode,
            false,
            200,
            &no_cancel(),
        )
        .unwrap();
        let indexed = manager
            .search_indexed(
                checkout_id,
                &root,
                query,
                mode,
                false,
                200,
                &no_cancel(),
                revision,
            )
            .unwrap();
        assert_eq!(
            fallback.completion,
            indexed.completion,
            "bench completion parity for {query}"
        );
        if query == rare_query {
            assert_eq!(
                fallback.completion,
                zeron_proto::WorkspaceContentSearchCompletion::Complete
            );
        }
        assert_eq!(
            serde_json::to_value(&fallback).unwrap(),
            serde_json::to_value(&indexed).unwrap(),
            "bench parity for {query}"
        );
    }

    let corpus_bytes = std::fs::read_dir(&root)
        .unwrap()
        .flatten()
        .filter_map(|e| e.metadata().ok())
        .filter(|m| m.is_file())
        .map(|m| m.len())
        .sum::<u64>();

    for _ in 0..3 {
        let _ = zeron_engine::workspace_content_search::search_workspace_content_blocking(
            &root,
            common_query,
            mode,
            false,
            200,
            &no_cancel(),
        );
    }

    let mut scan_samples = Vec::new();
    for _ in 0..9 {
        let start = Instant::now();
        let _ = zeron_engine::workspace_content_search::search_workspace_content_blocking(
            &root,
            common_query,
            mode,
            false,
            200,
            &no_cancel(),
        )
        .unwrap();
        scan_samples.push(start.elapsed().as_nanos());
    }

    let mut index_samples = Vec::new();
    for _ in 0..9 {
        let start = Instant::now();
        let _ = manager
            .search_indexed(
                checkout_id,
                &root,
                common_query,
                mode,
                false,
                200,
                &no_cancel(),
                revision,
            )
            .unwrap();
        index_samples.push(start.elapsed().as_nanos());
    }

    let warm_scan_median_ns = median_nanos(&scan_samples);
    let warm_index_median_ns = median_nanos(&index_samples);
    let ratio = warm_index_median_ns as f64 / warm_scan_median_ns.max(1) as f64;

    eprintln!(
        "BENCH files={} corpus_bytes={} warm_scan_median_ns={} warm_index_median_ns={} ratio={:.3}",
        1000,
        corpus_bytes,
        warm_scan_median_ns,
        warm_index_median_ns,
        ratio
    );
    manager.shutdown_and_join();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn content_index_incremental_file_change() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("repo");
    fixture_tree(&root);
    let root = std::fs::canonicalize(&root).unwrap();
    let checkout_id = "incr-checkout";
    let manager = test_manager(temp.path().join("index"), true, true);
    manager.on_watch_activated(
        checkout_id.to_string(),
        root.clone(),
        manager.watch_bridge(checkout_id),
    );
    assert!(wait_for_index_ready(&manager, checkout_id, false, Duration::from_secs(30)));
    run_indexed_parity(&root, &manager, checkout_id, false);

    std::fs::write(root.join("new.txt"), "brand new needle\n").unwrap();
    manager.on_watch_changes(
        checkout_id,
        &root,
        &zeron_proto::WorkspaceFileChanges {
            sequence: 2,
            resync_required: false,
            changes: vec![WorkspaceFileChange {
                operation_id: None,
                path: "new.txt".into(),
                kind: WorkspaceFileChangeKind::Created,
                old_path: None,
            }],
        },
    );
    assert!(wait_for_index_ready(&manager, checkout_id, false, Duration::from_secs(30)));
    run_indexed_parity(&root, &manager, checkout_id, false);
    manager.shutdown_and_join();
}
