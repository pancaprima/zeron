//! CI leaf: production fixture retention env gate (same source as macos_fixture_store_retention).

#[path = "../../crates/ui/src/browser/fixture_store_retention_env.rs"]
mod fixture_store_retention_env;

use fixture_store_retention_env::retain_website_data_store_enabled;

fn main() {
    let cases: [(&str, bool); 5] = [
        ("unset", false),
        ("", false),
        ("0", false),
        ("true", false),
        ("1", true),
    ];
    for (value, expect) in cases {
        if value == "unset" {
            std::env::remove_var("ZERON_BROWSER_FIXTURE_RETAIN_WEBSITE_DATA_STORE");
        } else {
            std::env::set_var("ZERON_BROWSER_FIXTURE_RETAIN_WEBSITE_DATA_STORE", value);
        }
        let got = retain_website_data_store_enabled();
        assert_eq!(got, expect, "value={value:?}");
    }
    eprintln!("browser_persistence_fixture_retain_env: ok");
}
