//! Std-only env gate for the fixture store retention arm (included by path from
//! `macos_fixture_store_retention` and the CI leaf test).

pub fn retain_website_data_store_enabled() -> bool {
    matches!(
        std::env::var("ZERON_BROWSER_FIXTURE_RETAIN_WEBSITE_DATA_STORE").as_deref(),
        Ok("1")
    )
}
