//! Std-only registry map used by macOS production and CI leaf tests (included by path).
//!
//! One `H` per store UUID for the process lifetime; `get_or_open` never removes entries.

use std::cell::RefCell;
use std::collections::HashMap;

/// In-process pins keyed by `dataStoreForIdentifier` UUID bytes.
pub struct RegistryCore<H> {
    entries: RefCell<HashMap<[u8; 16], H>>,
}

impl<H: Clone> RegistryCore<H> {
    pub fn new() -> Self {
        Self {
            entries: RefCell::new(HashMap::new()),
        }
    }

    pub fn len(&self) -> usize {
        self.entries.borrow().len()
    }

    pub fn get(&self, key: &[u8; 16]) -> Option<H> {
        self.entries.borrow().get(key).cloned()
    }

    /// Returns the pinned handle for `key`, calling `open` only on first use for that key.
    ///
    /// Does not hold a registry borrow across `open`. Nested `get_or_open` on the same
    /// `RegistryCore` is safe while `open` runs.
    pub fn get_or_open(&self, key: [u8; 16], open: impl FnOnce() -> H) -> H {
        if let Some(existing) = self.entries.borrow().get(&key).cloned() {
            return existing;
        }
        let opened = open();
        let mut entries = self.entries.borrow_mut();
        if let Some(existing) = entries.get(&key).cloned() {
            return existing;
        }
        entries.insert(key, opened.clone());
        opened
    }
}
