//! Fixture-only `WKHTTPCookieStore` readback diagnostics.
//!
//! `-[WKHTTPCookieStore getAllCookies:]` delivers the in-memory cookie list via a
//! completion handler. Apple’s public API does not document this call as flushing
//! cookies to on-disk storage.

use block2::RcBlock;
use objc2::rc::Retained;
use objc2_foundation::{NSArray, NSHTTPCookie, NSString};
use objc2_web_kit::WKWebsiteDataStore;
use std::ptr::NonNull;
use std::sync::{Arc, Mutex};

fn datastore_meta(store: &WKWebsiteDataStore) -> String {
    unsafe {
        let persistent = store.isPersistent();
        let uuid = store
            .identifier()
            .map(|id| id.to_string())
            .unwrap_or_else(|| "none".to_string());
        format!("datastore_persistent={persistent} datastore_uuid={uuid}")
    }
}

/// Async cookie **name** presence (never value) plus data-store metadata.
/// Retains `store` until the `getAllCookies` handler fires.
pub async fn native_cookie_diag_line(
    store: Retained<WKWebsiteDataStore>,
    cookie_name: String,
) -> Result<String, String> {
    let target = NSString::from_str(&cookie_name);
    let (tx, rx) = tokio::sync::oneshot::channel();
    let reply = Arc::new(Mutex::new(Some(tx)));
    let reply_for_block = reply.clone();
    let block = RcBlock::new(move |cookies: NonNull<NSArray<NSHTTPCookie>>| {
        let cookies = unsafe { cookies.as_ref() };
        let mut found = false;
        for cookie in cookies {
            if cookie.name().isEqualToString(&target) {
                found = true;
                break;
            }
        }
        if let Some(tx) = reply_for_block.lock().unwrap().take() {
            let _ = tx.send(found);
        }
    });
    unsafe {
        store.httpCookieStore().getAllCookies(&block);
    }
    let present = rx
        .await
        .map_err(|_| "WKHTTPCookieStore getAllCookies did not finish".to_string())?;
    Ok(format!(
        "native_cookie name={cookie_name} present={} {}",
        if present { 1 } else { 0 },
        datastore_meta(&store)
    ))
}
