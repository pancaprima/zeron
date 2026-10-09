//! Fixture-only causal lifecycle experiment: keep the exact `WKWebsiteDataStore`
//! instance alive until process exit even when normal browser/window teardown runs.
//!
//! **Not production.** Activated only with `browser-fixture` and
//! `ZERON_BROWSER_FIXTURE_RETAIN_WEBSITE_DATA_STORE=1`. Does not survive across
//! verify processes (no in-memory persistence in the verifier).

use objc2::rc::Retained;
use objc2_web_kit::WKWebsiteDataStore;

pub fn retain_website_data_store_enabled() -> bool {
    matches!(
        std::env::var("ZERON_BROWSER_FIXTURE_RETAIN_WEBSITE_DATA_STORE").as_deref(),
        Ok("1")
    )
}

/// Retain the same store object WebKit already opened for this profile (no recreation).
///
/// One `Retained` clone per opened store is intentionally leaked with [`std::mem::forget`]
/// so the Objective-C `WKWebsiteDataStore` outlives GPUI/browser teardown until process
/// exit. No static registry (would need `Send`/`Sync` for a main-thread ObjC object) and
/// no thread-local destructor (drops too early relative to process lifetime).
pub fn retain_exact_store_if_enabled(store: Retained<WKWebsiteDataStore>) {
    if !retain_website_data_store_enabled() {
        return;
    }
    let uuid = unsafe {
        store
            .identifier()
            .map(|id| id.to_string())
            .unwrap_or_else(|| "none".to_string())
    };
    eprintln!(
        "persistence-diag fixture-lifecycle=retain-website-data-store action=pin-until-process-exit datastore_uuid={uuid}"
    );
    std::mem::forget(store);
}
