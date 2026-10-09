//! Dependency-free leaf: env gate for fixture store retention (mirrors macos_fixture_store_retention).

fn retain_website_data_store_enabled() -> bool {
    matches!(
        std::env::var("ZERON_BROWSER_FIXTURE_RETAIN_WEBSITE_DATA_STORE").as_deref(),
        Ok("1")
    )
}

fn main() {
    let cases: [(&str, bool); 4] = [
        ("unset", false),
        ("", false),
        ("0", false),
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
