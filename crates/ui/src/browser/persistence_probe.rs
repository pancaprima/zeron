//! Title-based cookie/localStorage probes for the native persistence fixture.
//!
//! Probe titles encode only boolean presence flags — never stored values.

const PROBE_PREFIX: &str = "persist-probe";

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StorageProbe {
    pub nonce: String,
    pub cookie: bool,
    pub local_storage: bool,
}

impl StorageProbe {
    pub fn both_present(&self) -> bool {
        self.cookie && self.local_storage
    }

    pub fn both_absent(&self) -> bool {
        !self.cookie && !self.local_storage
    }
}

fn valid_nonce(nonce: &str) -> bool {
    !nonce.is_empty()
        && nonce.len() <= 64
        && nonce
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

/// Fresh token for one probe evaluation; must be passed into the JS scripts and the waiter.
pub fn new_probe_nonce() -> String {
    use std::sync::atomic::{AtomicU64, Ordering};
    static NEXT: AtomicU64 = AtomicU64::new(0);
    format!(
        "{:016x}",
        NEXT.fetch_add(1, Ordering::Relaxed) ^ std::process::id() as u64
    )
}

pub fn parse_probe_title(title: &str) -> Option<StorageProbe> {
    let rest = title.strip_prefix(PROBE_PREFIX)?;
    if rest.is_empty() {
        return None;
    }
    if !rest.starts_with(';') {
        return None;
    }
    let mut nonce = None;
    let mut cookie = None;
    let mut local_storage = None;
    for segment in rest[1..].split(';') {
        if segment.is_empty() {
            return None;
        }
        let (key, value) = segment.split_once('=')?;
        if key.is_empty() || value.is_empty() {
            return None;
        }
        match key {
            "nonce" => {
                if nonce.is_some() || !valid_nonce(value) {
                    return None;
                }
                nonce = Some(value.to_owned());
            }
            "cookie" | "localStorage" => {
                let flag = match value {
                    "1" => true,
                    "0" => false,
                    _ => return None,
                };
                let slot = if key == "cookie" {
                    &mut cookie
                } else {
                    &mut local_storage
                };
                if slot.is_some() {
                    return None;
                }
                *slot = Some(flag);
            }
            _ => return None,
        }
    }
    Some(StorageProbe {
        nonce: nonce?,
        cookie: cookie?,
        local_storage: local_storage?,
    })
}

pub fn probe_satisfied(
    title: &str,
    expected_nonce: &str,
    want_cookie: bool,
    want_local_storage: bool,
) -> bool {
    parse_probe_title(title).is_some_and(|probe| {
        probe.nonce == expected_nonce
            && probe.cookie == want_cookie
            && probe.local_storage == want_local_storage
    })
}

pub fn storage_probe_error(context: &str, probe: StorageProbe) -> String {
    if probe.both_present() {
        return format!("{context}: unexpected storage present (cookie and localStorage)");
    }
    if probe.both_absent() {
        return format!("{context}: cookie and localStorage both missing");
    }
    let mut missing = Vec::new();
    if !probe.cookie {
        missing.push("cookie");
    }
    if !probe.local_storage {
        missing.push("localStorage");
    }
    format!(
        "{context}: {} missing (other storage present)",
        missing.join(" and ")
    )
}

fn assert_safe_probe_literal(value: &str, label: &str) -> Result<(), String> {
    if value.is_empty()
        || value
            .chars()
            .any(|c| !(c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.')))
    {
        return Err(format!("invalid {label} for persistence probe script"));
    }
    Ok(())
}

fn probe_title_js(prefix: &str, nonce: &str) -> String {
    format!(
        "'{prefix};nonce={nonce};cookie=' + (cookieOk ? '1' : '0') + ';localStorage=' + (lsOk ? '1' : '0')"
    )
}

pub fn persist_write_script(
    cookie_name: &str,
    marker_value: &str,
    ls_key: &str,
    nonce: &str,
) -> Result<String, String> {
    assert_safe_probe_literal(cookie_name, "cookie name")?;
    assert_safe_probe_literal(marker_value, "marker value")?;
    assert_safe_probe_literal(ls_key, "localStorage key")?;
    if !valid_nonce(nonce) {
        return Err("invalid probe nonce".into());
    }
    let title_expr = probe_title_js(PROBE_PREFIX, nonce);
    Ok(format!(
        "document.title = 'Fieldnotes'; \
         document.cookie = '{name}={value}; path=/; max-age=31536000'; \
         localStorage.setItem('{ls}', '{value}'); \
         (function() {{ \
         var cookieOk = document.cookie.includes('{name}={value}'); \
         var lsOk = localStorage.getItem('{ls}') === '{value}'; \
         document.title = {title_expr}; \
         }})();",
        name = cookie_name,
        value = marker_value,
        ls = ls_key,
        title_expr = title_expr,
    ))
}

pub fn persist_read_probe_script(
    cookie_name: &str,
    marker_value: &str,
    ls_key: &str,
    nonce: &str,
) -> Result<String, String> {
    assert_safe_probe_literal(cookie_name, "cookie name")?;
    assert_safe_probe_literal(marker_value, "marker value")?;
    assert_safe_probe_literal(ls_key, "localStorage key")?;
    if !valid_nonce(nonce) {
        return Err("invalid probe nonce".into());
    }
    let title_expr = probe_title_js(PROBE_PREFIX, nonce);
    Ok(format!(
        "document.title = 'Fieldnotes'; \
         (function() {{ \
         var cookieOk = document.cookie.includes('{name}={value}'); \
         var lsOk = localStorage.getItem('{ls}') === '{value}'; \
         document.title = {title_expr}; \
         }})();",
        name = cookie_name,
        value = marker_value,
        ls = ls_key,
        title_expr = title_expr,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_probe_title_reads_boolean_flags() {
        let probe =
            parse_probe_title("persist-probe;nonce=abc123;cookie=1;localStorage=0").unwrap();
        assert_eq!(probe.nonce, "abc123");
        assert!(probe.cookie);
        assert!(!probe.local_storage);
    }

    #[test]
    fn parse_probe_title_rejects_wrong_prefix_and_malformed() {
        assert!(parse_probe_title("persist-probe-extra;nonce=a;cookie=1;localStorage=0").is_none());
        assert!(parse_probe_title("persist-probe").is_none());
        assert!(parse_probe_title("persist-probe;nonce=a;cookie=2;localStorage=0").is_none());
        assert!(parse_probe_title("persist-probe;nonce=a;cookie=1").is_none());
        assert!(
            parse_probe_title("persist-probe;nonce=a;cookie=1;localStorage=0;extra=1").is_none()
        );
        assert!(
            parse_probe_title("persist-probe;nonce=a;cookie=1;cookie=0;localStorage=0").is_none()
        );
        assert!(
            parse_probe_title("persist-probe;nonce=a;nonce=b;cookie=1;localStorage=0").is_none()
        );
        assert!(parse_probe_title("persist-probe;;cookie=1;localStorage=0").is_none());
    }

    #[test]
    fn probe_satisfied_requires_matching_nonce() {
        let nonce = "deadbeef";
        assert!(probe_satisfied(
            "persist-probe;nonce=deadbeef;cookie=1;localStorage=1",
            nonce,
            true,
            true,
        ));
        assert!(!probe_satisfied(
            "persist-probe;nonce=other;cookie=1;localStorage=1",
            nonce,
            true,
            true,
        ));
        assert!(!probe_satisfied(
            "persist-probe;nonce=deadbeef;cookie=1;localStorage=0",
            nonce,
            true,
            true,
        ));
    }

    #[test]
    fn storage_probe_error_is_actionable_without_values() {
        let err = storage_probe_error(
            "relaunch-verify",
            StorageProbe {
                nonce: "n".into(),
                cookie: true,
                local_storage: false,
            },
        );
        assert!(err.contains("localStorage"));
        assert!(!err.contains("zeron-persist"));
    }

    #[test]
    fn persist_scripts_embed_nonce() {
        let script = persist_write_script("c", "m", "ls", "nonce1").unwrap();
        assert!(script.contains("document.title = 'Fieldnotes'"));
        assert!(script.contains(";nonce=nonce1;cookie="));
    }
}
