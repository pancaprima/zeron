//! Persistence and isolation scenarios for the browser fixture.
//!
//! These tests document expected behavior for relaunch and profile isolation.
//! They are not executed as part of this VPS matrix (no GUI/native build here).

#[cfg(test)]
mod spec {
    use std::path::PathBuf;
    use zeron_ui::browser::BrowserContext;

    fn scratch() -> PathBuf {
        let base = std::env::var("RUNNER_TEMP")
            .or_else(|_| std::env::var("TMPDIR"))
            .unwrap_or_else(|_| std::env::temp_dir().to_string_lossy().into());
        PathBuf::from(base).join("zeron-browser-fixture-spec")
    }

    #[test]
    fn persistent_storage_paths_are_stable_for_one_locator() {
        let dir = scratch();
        std::fs::create_dir_all(&dir).unwrap();
        let locator = "0123456789abcdef";
        let first = BrowserContext::persistent(&dir, locator).unwrap();
        let second = BrowserContext::persistent(&dir, locator).unwrap();
        let a = first.profile_mode().storage().unwrap();
        let b = second.profile_mode().storage().unwrap();
        assert_eq!(a.storage_root(), b.storage_root());
        assert_eq!(a.store_uuid(), b.store_uuid());
    }

    #[test]
    fn distinct_locators_do_not_share_storage_roots() {
        let dir = scratch();
        std::fs::create_dir_all(&dir).unwrap();
        let a = BrowserContext::persistent(&dir, "aaaaaaaaaaaaaaaa")
            .unwrap()
            .profile_mode()
            .storage()
            .unwrap();
        let b = BrowserContext::persistent(&dir, "bbbbbbbbbbbbbbbb")
            .unwrap()
            .profile_mode()
            .storage()
            .unwrap();
        assert_ne!(a.storage_root(), b.storage_root());
    }

    #[test]
    fn rebuilding_browser_context_keeps_same_partition() {
        let dir = scratch();
        std::fs::create_dir_all(&dir).unwrap();
        let locator = "0123456789abcdef";
        let first = BrowserContext::persistent(&dir, locator).unwrap();
        let second = BrowserContext::persistent(&dir, locator).unwrap();
        assert_eq!(
            first.profile_mode().storage().unwrap().storage_root(),
            second.profile_mode().storage().unwrap().storage_root()
        );
    }
}
