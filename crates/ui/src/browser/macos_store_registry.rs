//! Process-lifetime `WKWebsiteDataStore` handles for persistent Zeron profiles.
//!
//! WebKit tears down the network session when the last strong reference to a
//! persistent store is released, which can drop HTTP cookies before they are
//! written to disk. GPUI tab/window teardown drops web views while the process
//! is still running; this registry keeps one `Retained` per profile store UUID
//! until process exit so teardown order does not destroy the session early.
//!
//! **Lifetime:** Main-thread only (`MainThreadMarker` at the ObjC open boundary).
//! The map lives in a single deliberately leaked `Box` (one bounded allocation for
//! the container). A `thread_local` holds `&'static` to that box so thread
//! shutdown does not run a `RefCell`/map destructor and drop `Retained` stores
//! before WebKit process exit. ObjC handles are not `Send`/`Sync`; no cross-thread
//! registry access.
//!
//! **Memory:** One WebKit store (and its network session) per distinct persistent
//! UUID opened in the process, until exit; map entries are not evicted.

#[path = "browser_store_registry_core.rs"]
mod browser_store_registry_core;

use browser_store_registry_core::RegistryCore;

use objc2::MainThreadMarker;
use objc2::rc::Retained;
use objc2_web_kit::WKWebsiteDataStore;

type StoreRegistry = RegistryCore<Retained<WKWebsiteDataStore>>;

thread_local! {
    /// Reference to a leaked registry; TLS drop does not free the box or its `Retained` values.
    static REGISTRY: &'static StoreRegistry = Box::leak(Box::new(StoreRegistry::new()));
}

/// Stable registry key for a profile's `dataStoreForIdentifier` UUID.
pub(crate) fn store_key(store_uuid: uuid::Uuid) -> [u8; 16] {
    store_uuid.into_bytes()
}

/// Returns the pinned store for `store_uuid`, calling `open` only on first use.
///
/// `open` may re-enter this function for the same or a different key; the registry
/// borrow is not held across `open`.
pub(crate) fn get_or_open_persistent_store(
    store_uuid: uuid::Uuid,
    mtm: MainThreadMarker,
    open: impl FnOnce(MainThreadMarker) -> Retained<WKWebsiteDataStore>,
) -> Retained<WKWebsiteDataStore> {
    let key = store_key(store_uuid);
    REGISTRY.with(|registry| {
        registry.get_or_open(key, || {
            let store = open(mtm);
            tracing::debug!(
                store_uuid = %store_uuid,
                "retained persistent WKWebsiteDataStore for process lifetime"
            );
            store
        })
    })
}
