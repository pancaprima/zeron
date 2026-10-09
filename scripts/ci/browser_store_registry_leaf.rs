//! CI leaf tests for the production `RegistryCore` (same source as macOS registry).

#[path = "../../crates/ui/src/browser/browser_store_registry_core.rs"]
mod browser_store_registry_core;

use std::cell::RefCell;
use std::rc::Rc;

use browser_store_registry_core::RegistryCore;

#[test]
fn distinct_uuids_map_to_distinct_pins() {
    let reg = RegistryCore::new();
    let a = [1; 16];
    let b = [2; 16];
    let id_a = reg.get_or_open(a, || Rc::new(1_u32));
    let id_b = reg.get_or_open(b, || Rc::new(2_u32));
    assert_ne!(id_a, id_b);
    assert_eq!(reg.len(), 2);
}

#[test]
fn same_uuid_reuses_one_pin() {
    let reg = RegistryCore::new();
    let key = [0xab; 16];
    let first = reg.get_or_open(key, || Rc::new(10_u32));
    let second = reg.get_or_open(key, || Rc::new(99_u32));
    assert_eq!(first, second);
    assert_eq!(*first, 10);
    assert_eq!(reg.len(), 1);
}

#[test]
fn caller_handle_drop_keeps_registry_pin_alive() {
    let reg = RegistryCore::new();
    let key = [3; 16];
    let handle = reg.get_or_open(key, || Rc::new(7_u32));
    let weak = Rc::downgrade(&handle);
    drop(handle);
    assert!(
        weak.upgrade().is_some(),
        "registry must keep the canonical handle"
    );
    assert_eq!(reg.len(), 1);
    let again = reg.get_or_open(key, || Rc::new(8_u32));
    assert_eq!(*again, 7);
}

#[test]
fn reentrant_open_same_key_no_panic_and_canonical_pin() {
    let reg = RegistryCore::new();
    let key = [4; 16];
    let outer = reg.get_or_open(key, || {
        let inner = reg.get_or_open(key, || Rc::new(42_u32));
        assert_eq!(*inner, 42);
        Rc::new(99_u32)
    });
    assert_eq!(
        *outer, 42,
        "reentrant insert must win over outer open result"
    );
    assert_eq!(reg.len(), 1);
}

#[test]
fn reentrant_open_different_key_nested() {
    let reg = RegistryCore::new();
    let key_outer = [5; 16];
    let key_inner = [6; 16];
    let outer = reg.get_or_open(key_outer, || {
        let inner = reg.get_or_open(key_inner, || Rc::new(200_u32));
        assert_eq!(*inner, 200);
        Rc::new(100_u32)
    });
    assert_eq!(*outer, 100);
    assert_eq!(reg.len(), 2);
    assert_eq!(*reg.get(&key_inner).unwrap(), 200);
}

/// Core-only: registry never evicts on external "clear"; native `removeDataOfTypes` is CI.
#[test]
fn external_clear_does_not_evict_registry_entry() {
    let reg = RegistryCore::new();
    let key = [9; 16];
    let shared: Rc<RefCell<Option<u32>>> = Rc::new(RefCell::new(None));
    let pin = reg.get_or_open(key, || {
        *shared.borrow_mut() = Some(42);
        shared.clone()
    });
    assert_eq!(*pin.borrow(), Some(42));

    *pin.borrow_mut() = None;
    assert_eq!(*shared.borrow(), None);

    let id_before = Rc::as_ptr(&pin);
    drop(pin);
    assert_eq!(reg.len(), 1);

    let opener_calls = Rc::new(RefCell::new(0_u32));
    let again = reg.get_or_open(key, || {
        *opener_calls.borrow_mut() += 1;
        Rc::new(RefCell::new(Some(99)))
    });
    assert_eq!(
        *opener_calls.borrow(),
        0,
        "opener must not run for existing key"
    );
    assert_eq!(Rc::as_ptr(&again), id_before);
    assert_eq!(*again.borrow(), None, "cleared payload must stay cleared");
}

fn main() {}
