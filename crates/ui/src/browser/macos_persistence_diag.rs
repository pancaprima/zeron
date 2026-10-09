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

/// Own properties of the first cookie matching the name; never its value.
struct CookieShape {
    matches: usize,
    session_only: bool,
    has_expiry: bool,
}

/// Async cookie **name** presence (never value), the matched cookie's
/// session-only flag and expiry presence, plus data-store metadata.
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
        let mut shape: Option<CookieShape> = None;
        for cookie in cookies {
            if !cookie.name().isEqualToString(&target) {
                continue;
            }
            match shape.as_mut() {
                Some(shape) => shape.matches += 1,
                None => {
                    // objc2-foundation 0.3.2 safety markings for these getters
                    // were not verifiable offline.
                    #[allow(unused_unsafe)]
                    let (session_only, has_expiry) =
                        unsafe { (cookie.isSessionOnly(), cookie.expiresDate().is_some()) };
                    shape = Some(CookieShape {
                        matches: 1,
                        session_only,
                        has_expiry,
                    })
                }
            }
        }
        if let Some(tx) = reply_for_block.lock().unwrap().take() {
            let _ = tx.send(shape);
        }
    });
    unsafe {
        store.httpCookieStore().getAllCookies(&block);
    }
    let shape = rx
        .await
        .map_err(|_| "WKHTTPCookieStore getAllCookies did not finish".to_string())?;
    let flag = |value: bool| if value { "1" } else { "0" };
    let detail = match &shape {
        Some(shape) => format!(
            "matches={} session_only={} has_expiry={}",
            shape.matches,
            flag(shape.session_only),
            flag(shape.has_expiry)
        ),
        None => "matches=0 session_only=na has_expiry=na".to_string(),
    };
    Ok(format!(
        "native_cookie name={cookie_name} present={} {detail} {}",
        flag(shape.is_some()),
        datastore_meta(&store)
    ))
}
