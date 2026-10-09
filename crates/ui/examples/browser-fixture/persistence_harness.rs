//! Native macOS browser persistence acceptance harness (env-gated phases).
//!
//! Real WebKit storage across process relaunch, identity isolation, and clear UI.
//! Invoked when `ZERON_BROWSER_PERSISTENCE_PHASE` is set; never touches user data.

use super::loopback::{LoopbackSite, persistence_port, start};
use gpui::{AppContext, AsyncApp, Bounds, WindowBounds, WindowHandle, WindowOptions, px, size};
use std::{
    path::{Path, PathBuf},
    time::Duration,
};
use zeron_ui::{
    browser::{BrowserSurface, persistence_probe},
    shell::Shell,
    *,
};

pub const COOKIE_NAME: &str = "zeron_persist_fixture";
pub const LS_KEY: &str = "zeron_persist_fixture";
pub const MARKER_VALUE: &str = "zeron-persist-marker-v1";
pub const DEVICE_A: &str = "local";
pub const DEVICE_B: &str = "fixture-device-b";

pub fn persistence_root() -> PathBuf {
    std::env::var("ZERON_BROWSER_PERSISTENCE_ROOT")
        .map(PathBuf::from)
        .unwrap_or_else(|_| {
            let base = std::env::var("RUNNER_TEMP")
                .or_else(|_| std::env::var("TMPDIR"))
                .unwrap_or_else(|_| std::env::temp_dir().to_string_lossy().into());
            PathBuf::from(base).join("zeron-browser-persistence")
        })
}

pub fn app_data_dir(root: &Path) -> PathBuf {
    root.join("app-data")
}

async fn pause(cx: &mut AsyncApp, ms: u64) {
    cx.background_executor()
        .timer(Duration::from_millis(ms))
        .await;
}

fn write_success(output: &Path, message: &str) -> anyhow::Result<()> {
    std::fs::write(output.join("persistence-result.txt"), message)?;
    // `run-macos-fixture.sh` treats an empty/missing result.txt as failure.
    std::fs::write(output.join("result.txt"), message)?;
    Ok(())
}

fn persist_write_script(nonce: &str) -> anyhow::Result<String> {
    persistence_probe::persist_write_script(COOKIE_NAME, MARKER_VALUE, LS_KEY, nonce)
        .map_err(anyhow::Error::msg)
}

fn persist_read_probe_script(nonce: &str) -> anyhow::Result<String> {
    persistence_probe::persist_read_probe_script(COOKIE_NAME, MARKER_VALUE, LS_KEY, nonce)
        .map_err(anyhow::Error::msg)
}

fn log_persistence_boundary(
    phase: &str,
    origin: &str,
    window: WindowHandle<Shell>,
    cx: &mut AsyncApp,
) -> anyhow::Result<()> {
    let profile = window.update(cx, |shell, _, _| shell.fixture_browser_persistence_diag())?;
    eprintln!("persistence-diag phase={phase} origin={origin} profile={profile}");
    Ok(())
}

async fn wait_for_storage_probe(
    browser: &gpui::Entity<BrowserSurface>,
    context: &str,
    probe_nonce: &str,
    want_cookie: bool,
    want_local_storage: bool,
    timeout: Duration,
    cx: &mut AsyncApp,
) -> anyhow::Result<()> {
    let deadline = std::time::Instant::now() + timeout;
    while !browser.read_with(cx, |b, _| {
        persistence_probe::probe_satisfied(
            &b.page.title,
            probe_nonce,
            want_cookie,
            want_local_storage,
        )
    }) {
        let title = browser.read_with(cx, |b, _| b.page.title.clone());
        if let Some(probe) = persistence_probe::parse_probe_title(&title) {
            if probe.nonce != probe_nonce {
                // Stale probe from a prior navigation or evaluation.
            } else {
                let satisfied =
                    probe.cookie == want_cookie && probe.local_storage == want_local_storage;
                if !satisfied {
                    if want_cookie && want_local_storage && (probe.cookie ^ probe.local_storage) {
                        anyhow::bail!(persistence_probe::storage_probe_error(context, probe));
                    }
                    if !want_cookie && !want_local_storage && !probe.both_absent() {
                        anyhow::bail!(persistence_probe::storage_probe_error(context, probe));
                    }
                }
            }
        }
        anyhow::ensure!(
            std::time::Instant::now() < deadline,
            "{context}: timed out waiting for storage probe nonce={probe_nonce} cookie={want_cookie} localStorage={want_local_storage} (title=`{title}`)"
        );
        pause(cx, 50).await;
    }
    Ok(())
}

async fn load_origin(
    window: WindowHandle<Shell>,
    browser: &gpui::Entity<BrowserSurface>,
    origin: &str,
    cx: &mut AsyncApp,
) -> anyhow::Result<()> {
    window.update(cx, |_, w, cx| {
        browser.update(cx, |b, cx| b.navigate(origin, w, cx))
    })?;
    let deadline = std::time::Instant::now() + Duration::from_secs(25);
    while !browser.read_with(cx, |b, _| b.page.title == "Fieldnotes" && !b.page.loading) {
        anyhow::ensure!(
            std::time::Instant::now() < deadline,
            "page did not load at {origin}: {:?}",
            browser.read_with(cx, |b, _| b.page.clone())
        );
        pause(cx, 50).await;
    }
    Ok(())
}

async fn open_browser(
    window: WindowHandle<Shell>,
    cx: &mut AsyncApp,
) -> anyhow::Result<(u64, gpui::Entity<BrowserSurface>)> {
    pause(cx, 800).await;
    window.update(cx, |shell, w, cx| shell.fixture_open_browser(None, w, cx))
}

async fn wait_clear_idle(window: WindowHandle<Shell>, cx: &mut AsyncApp) -> anyhow::Result<()> {
    let deadline = std::time::Instant::now() + Duration::from_secs(30);
    loop {
        let busy = window.update(cx, |shell, _, _| {
            shell.fixture_browser_clear_busy() || shell.fixture_browser_clear_confirm_open()
        })?;
        if !busy {
            return Ok(());
        }
        anyhow::ensure!(std::time::Instant::now() < deadline, "clear operation hung");
        pause(cx, 50).await;
    }
}

async fn write_storage(
    window: WindowHandle<Shell>,
    browser: &gpui::Entity<BrowserSurface>,
    origin: &str,
    cx: &mut AsyncApp,
) -> anyhow::Result<()> {
    log_persistence_boundary("write-storage:before", origin, window, cx)?;
    load_origin(window, browser, origin, cx).await?;
    let probe_nonce = persistence_probe::new_probe_nonce();
    let script = persist_write_script(&probe_nonce)?;
    browser.read_with(cx, |b, _| b.fixture_eval(&script));
    wait_for_storage_probe(
        browser,
        "write-storage readback",
        &probe_nonce,
        true,
        true,
        Duration::from_secs(15),
        cx,
    )
    .await?;
    log_persistence_boundary("write-storage:after-readback", origin, window, cx)?;
    pause(cx, 300).await;
    Ok(())
}

async fn expect_storage(
    window: WindowHandle<Shell>,
    browser: &gpui::Entity<BrowserSurface>,
    origin: &str,
    present: bool,
    cx: &mut AsyncApp,
) -> anyhow::Result<()> {
    let context = if present {
        "restart-verify"
    } else {
        "storage-absent-check"
    };
    log_persistence_boundary(context, origin, window, cx)?;
    load_origin(window, browser, origin, cx).await?;
    let probe_nonce = persistence_probe::new_probe_nonce();
    let script = persist_read_probe_script(&probe_nonce)?;
    browser.read_with(cx, |b, _| b.fixture_eval(&script));
    let (want_cookie, want_ls) = if present {
        (true, true)
    } else {
        (false, false)
    };
    wait_for_storage_probe(
        browser,
        context,
        &probe_nonce,
        want_cookie,
        want_ls,
        Duration::from_secs(15),
        cx,
    )
    .await?;
    Ok(())
}

fn write_relaunch_marker(
    root: &Path,
    site: &LoopbackSite,
    profile_diag: &str,
) -> anyhow::Result<()> {
    std::fs::create_dir_all(root)?;
    std::fs::write(
        root.join("relaunch-marker.json"),
        serde_json::to_string_pretty(&serde_json::json!({
            "port": site.port,
            "origin": site.origin,
            "cookieName": COOKIE_NAME,
            "localStorageKey": LS_KEY,
            "markerValue": MARKER_VALUE,
            "deviceA": DEVICE_A,
            "profileDiag": profile_diag,
        }))?,
    )?;
    Ok(())
}

fn require_relaunch_marker(root: &Path) -> anyhow::Result<serde_json::Value> {
    let path = root.join("relaunch-marker.json");
    anyhow::ensure!(
        path.is_file(),
        "missing relaunch marker at {}",
        path.display()
    );
    let contents = std::fs::read_to_string(&path)?;
    serde_json::from_str(&contents)
        .map_err(|error| anyhow::anyhow!("invalid relaunch marker: {error}"))
}

fn require_marker_origin<'a>(marker: &'a serde_json::Value) -> anyhow::Result<&'a str> {
    marker["origin"]
        .as_str()
        .filter(|origin| !origin.is_empty())
        .ok_or_else(|| anyhow::anyhow!("relaunch marker missing required origin"))
}

fn validate_marker_fields(marker: &serde_json::Value, site: &LoopbackSite) -> anyhow::Result<()> {
    let port = marker["port"]
        .as_u64()
        .and_then(|p| u16::try_from(p).ok())
        .ok_or_else(|| anyhow::anyhow!("relaunch marker missing required port"))?;
    anyhow::ensure!(
        site.port == port,
        "loopback port mismatch: live={} marker={}",
        site.port,
        port
    );
    anyhow::ensure!(
        marker.get("cookieName").and_then(|v| v.as_str()) == Some(COOKIE_NAME),
        "relaunch marker cookieName mismatch"
    );
    anyhow::ensure!(
        marker.get("localStorageKey").and_then(|v| v.as_str()) == Some(LS_KEY),
        "relaunch marker localStorageKey mismatch"
    );
    anyhow::ensure!(
        marker.get("markerValue").and_then(|v| v.as_str()) == Some(MARKER_VALUE),
        "relaunch marker markerValue mismatch"
    );
    Ok(())
}

fn ensure_loopback_origin<'a>(
    site: &LoopbackSite,
    marker: &'a serde_json::Value,
) -> anyhow::Result<&'a str> {
    validate_marker_fields(marker, site)?;
    let expected = require_marker_origin(marker)?;
    anyhow::ensure!(
        site.origin == expected,
        "loopback origin mismatch: {} vs {}",
        site.origin,
        expected
    );
    Ok(expected)
}

pub async fn run_phase(
    phase: &str,
    root: &Path,
    output: &Path,
    window: WindowHandle<Shell>,
    cx: &mut AsyncApp,
) -> anyhow::Result<()> {
    let port = persistence_port(root);
    let site = start(port)?;
    match phase {
        "relaunch-write" => {
            log_persistence_boundary("relaunch-write:start", &site.origin, window, cx)?;
            let marker_path = root.join("relaunch-marker.json");
            if marker_path.is_file() {
                std::fs::remove_file(&marker_path)?;
            }
            let (_id, browser) = open_browser(window, cx).await?;
            write_storage(window, &browser, &site.origin, cx).await?;
            let profile_diag =
                window.update(cx, |shell, _, _| shell.fixture_browser_persistence_diag())?;
            write_relaunch_marker(root, &site, &profile_diag)?;
            log_persistence_boundary("relaunch-write:marker-written", &site.origin, window, cx)?;
            window.update(cx, |shell, w, cx| shell.fixture_close_browser(_id, w, cx))?;
            pause(cx, 400).await;
            write_success(
                output,
                "PASS: relaunch-write stored persistent cookie and localStorage; marker written.\n",
            )
        }
        "relaunch-verify" => {
            let marker = require_relaunch_marker(root)?;
            if let Some(diag) = marker.get("profileDiag").and_then(|v| v.as_str()) {
                eprintln!("persistence-diag phase=relaunch-verify:marker profile={diag}");
            }
            let expected_origin = ensure_loopback_origin(&site, &marker)?;
            log_persistence_boundary("relaunch-verify:start", expected_origin, window, cx)?;
            let (_id, browser) = open_browser(window, cx).await?;
            expect_storage(window, &browser, expected_origin, true, cx).await?;
            window.update(cx, |shell, w, cx| shell.fixture_close_browser(_id, w, cx))?;
            pause(cx, 200).await;
            write_success(
                output,
                "PASS: relaunch-verify read persistent cookie and localStorage after process restart.\n",
            )
        }
        "isolation" => {
            let marker = require_relaunch_marker(root)?;
            let origin = ensure_loopback_origin(&site, &marker)?;
            let (_id_a, browser_a) = open_browser(window, cx).await?;
            expect_storage(window, &browser_a, origin, true, cx).await?;
            window.update(cx, |shell, w, cx| shell.fixture_close_browser(_id_a, w, cx))?;
            pause(cx, 200).await;
            window.update(cx, |shell, _, cx| {
                shell.fixture_set_local_device_id(DEVICE_B, cx)
            })?;
            pause(cx, 300).await;
            let (_id_b, browser_b) = open_browser(window, cx).await?;
            expect_storage(window, &browser_b, origin, false, cx).await?;
            window.update(cx, |shell, w, cx| shell.fixture_close_browser(_id_b, w, cx))?;
            pause(cx, 200).await;
            window.update(cx, |shell, _, cx| {
                shell.fixture_set_local_device_id(DEVICE_A, cx)
            })?;
            pause(cx, 300).await;
            let (_id_a2, browser_a2) = open_browser(window, cx).await?;
            expect_storage(window, &browser_a2, origin, true, cx).await?;
            write_success(
                output,
                "PASS: isolation hid profile data from identity B and restored it for identity A.\n",
            )
        }
        "clear-cancel" => {
            let marker = require_relaunch_marker(root)?;
            let origin = ensure_loopback_origin(&site, &marker)?;
            let (_id, browser) = open_browser(window, cx).await?;
            write_storage(window, &browser, origin, cx).await?;
            window.update(cx, |shell, _, cx| {
                shell.fixture_browser_clear_dialog(true, cx)
            })?;
            pause(cx, 200).await;
            anyhow::ensure!(
                window.update(cx, |shell, _, _| shell.fixture_browser_clear_confirm_open())?,
                "clear confirmation dialog did not open"
            );
            window.update(cx, |shell, _, cx| shell.fixture_cancel_browser_clear(cx))?;
            pause(cx, 200).await;
            anyhow::ensure!(
                !window.update(cx, |shell, _, _| shell.fixture_browser_clear_confirm_open())?,
                "clear dialog stayed open after cancel"
            );
            anyhow::ensure!(
                !window.update(cx, |shell, _, _| shell.fixture_browser_clear_busy())?,
                "clear remained busy after cancel"
            );
            anyhow::ensure!(
                window
                    .update(cx, |shell, _, _| shell.fixture_browser_clear_error())?
                    .is_none(),
                "cancel should not surface a clear error"
            );
            expect_storage(window, &browser, origin, true, cx).await?;
            write_success(output, "PASS: clear-cancel left website data unchanged.\n")
        }
        "clear-confirm" => {
            let marker = require_relaunch_marker(root)?;
            let origin = ensure_loopback_origin(&site, &marker)?;
            window.update(cx, |shell, _, cx| {
                shell.fixture_set_local_device_id(DEVICE_B, cx)
            })?;
            pause(cx, 300).await;
            let (_id_b, browser_b) = open_browser(window, cx).await?;
            write_storage(window, &browser_b, origin, cx).await?;
            window.update(cx, |shell, w, cx| shell.fixture_close_browser(_id_b, w, cx))?;
            pause(cx, 200).await;
            window.update(cx, |shell, _, cx| {
                shell.fixture_set_local_device_id(DEVICE_A, cx)
            })?;
            pause(cx, 300).await;
            let (_id_a, browser_a) = open_browser(window, cx).await?;
            write_storage(window, &browser_a, origin, cx).await?;
            window.update(cx, |shell, _, cx| {
                shell.fixture_browser_clear_dialog(true, cx)
            })?;
            pause(cx, 200).await;
            window.update(cx, |shell, w, cx| {
                shell.fixture_confirm_browser_clear(w, cx)
            })?;
            wait_clear_idle(window, cx).await?;
            anyhow::ensure!(
                window
                    .update(cx, |shell, _, _| shell.fixture_browser_clear_error())?
                    .is_none(),
                "unexpected clear error"
            );
            expect_storage(window, &browser_a, origin, false, cx).await?;
            window.update(cx, |shell, w, cx| shell.fixture_close_browser(_id_a, w, cx))?;
            pause(cx, 200).await;
            window.update(cx, |shell, _, cx| {
                shell.fixture_set_local_device_id(DEVICE_B, cx)
            })?;
            pause(cx, 300).await;
            let (_id_b2, browser_b2) = open_browser(window, cx).await?;
            expect_storage(window, &browser_b2, origin, true, cx).await?;
            window.update(cx, |shell, w, cx| {
                shell.fixture_close_browser(_id_b2, w, cx)
            })?;
            pause(cx, 200).await;
            window.update(cx, |shell, _, cx| {
                shell.fixture_set_local_device_id(DEVICE_A, cx)
            })?;
            pause(cx, 200).await;
            write_success(
                output,
                "PASS: clear-confirm removed website data for identity A only; identity B unchanged.\n",
            )
        }
        "clear-failure" => {
            anyhow::ensure!(
                std::env::var_os("ZERON_BROWSER_FIXTURE_INJECT_CLEAR_ERROR").is_some(),
                "clear-failure phase requires ZERON_BROWSER_FIXTURE_INJECT_CLEAR_ERROR set before process start"
            );
            let marker = require_relaunch_marker(root)?;
            let origin = ensure_loopback_origin(&site, &marker)?;
            let (_id, browser) = open_browser(window, cx).await?;
            write_storage(window, &browser, origin, cx).await?;
            window.update(cx, |shell, _, cx| {
                shell.fixture_browser_clear_dialog(true, cx)
            })?;
            pause(cx, 200).await;
            window.update(cx, |shell, w, cx| {
                shell.fixture_confirm_browser_clear(w, cx)
            })?;
            wait_clear_idle(window, cx).await?;
            let error = window.update(cx, |shell, _, _| shell.fixture_browser_clear_error())?;
            anyhow::ensure!(
                error
                    .as_deref()
                    .is_some_and(|e| e.contains("Fixture-injected clear failure")),
                "expected injected clear failure dialog, got {error:?}"
            );
            anyhow::ensure!(
                !window.update(cx, |shell, _, _| shell.fixture_browser_clear_confirm_open())?,
                "clear confirm dialog should be closed after failure"
            );
            anyhow::ensure!(
                !window.update(cx, |shell, _, _| shell.fixture_browser_clear_busy())?,
                "clear remained busy after injected failure"
            );
            expect_storage(window, &browser, origin, true, cx).await?;
            window.update(cx, |shell, _, cx| {
                shell.fixture_dismiss_browser_clear_error(cx)
            })?;
            pause(cx, 200).await;
            write_success(
                output,
                "PASS: clear-failure surfaced an error without hanging and left data intact.\n",
            )
        }
        other => anyhow::bail!("unknown ZERON_BROWSER_PERSISTENCE_PHASE: {other}"),
    }
}

pub fn bootstrap_state(data: PathBuf, cx: &mut gpui::App) -> gpui::Entity<state::AppState> {
    let settings = settings::UiSettings::default();
    settings::init(settings.clone(), data.clone(), cx);
    let fonts = typography::register_fonts(cx);
    typography::init(
        settings.ui_font_family.clone(),
        settings.ui_font_size,
        settings.terminal_font_family.clone(),
        settings.terminal_font_size,
        settings.code_font_family.clone(),
        settings.code_font_size,
        fonts,
        cx,
    );
    theme_library::init(data.clone(), cx);
    appearance::init(
        appearance::AppearanceMode::Dark,
        settings.theme_selection,
        settings.accent,
        settings.surface,
        cx,
    );
    history::init(
        settings.git_history_columns,
        settings.git_history_column_widths,
        settings.git_history_column_order,
        settings.git_history_author_display,
        cx,
    );
    composer::init(cx, settings.composer_send_behavior);
    terminal::panel::init(cx);
    app_menus::init(cx);
    cx.new(|_| {
        let mut s = state::AppState::new();
        s.connection = zeron_proto::view::ConnectionStatus::Ready;
        s.workspace_scope = Some(zeron_proto::WorkspaceScope::Local);
        s.local_device_id = Some(DEVICE_A.into());
        s.devices = vec![
            serde_json::from_value(serde_json::json!({
                "id": DEVICE_A,
                "name": "This device",
                "platform": std::env::consts::OS,
                "lastSeenAt": null
            }))
            .unwrap(),
            serde_json::from_value(serde_json::json!({
                "id": DEVICE_B,
                "name": "Fixture device B",
                "platform": std::env::consts::OS,
                "lastSeenAt": null
            }))
            .unwrap(),
        ];
        s.selected_chat = Some("browser-fixture".into());
        s.selected_space = Some("project".into());
        s.auto_selected = true;
        s.chats_synced = true;
        s.spaces_synced = true;
        s.spaces = vec![
            serde_json::from_value(serde_json::json!({
                "id": "project",
                "deviceId": DEVICE_A,
                "path": "/tmp/fieldnotes",
                "createdAt": "2026-09-08T00:00:00Z"
            }))
            .unwrap(),
        ];
        s.chats = vec![
            serde_json::from_value(serde_json::json!({
                "id": "browser-fixture",
                "deviceId": DEVICE_A,
                "spaceId": "project",
                "title": "Build the Fieldnotes workspace",
                "archived": false,
                "createdAt": "2026-09-08T00:00:00Z",
                "config": {
                    "harness": "claude-code",
                    "model": "claude-sonnet-4-6",
                    "reasoning": null,
                    "sandbox": "workspace-write"
                }
            }))
            .unwrap(),
        ];
        s
    })
}

pub fn run_app(phase: &str, data: PathBuf, output: PathBuf) -> anyhow::Result<()> {
    anyhow::ensure!(
        std::env::consts::OS == "macos",
        "ZERON_BROWSER_PERSISTENCE_PHASE harness requires native macOS WebKit"
    );
    let root = persistence_root();
    std::fs::create_dir_all(&root)?;
    std::fs::create_dir_all(&data)?;
    std::fs::create_dir_all(&output)?;
    let failure = std::sync::Arc::new(std::sync::Mutex::new(None));
    let result = failure.clone();
    let phase = phase.to_owned();
    gpui_platform::application()
        .with_assets(icons::Assets)
        .run(move |cx| {
            gpui_tokio::init(cx);
            gpui_base::init(cx);
            let state = bootstrap_state(data.clone(), cx);
            let boot = EngineBootConfig {
                data_dir: data,
                ipc_port: 0,
                edge_url: String::new(),
                edge_token: None,
                org_id: None,
                workos_client_id: None,
                default_harness: HarnessId::ClaudeCode,
            };
            let window = cx
                .open_window(
                    WindowOptions {
                        window_bounds: Some(WindowBounds::Windowed(Bounds::new(
                            gpui::point(px(12.), px(30.)),
                            size(px(1000.), px(680.)),
                        ))),
                        titlebar: Some(gpui::TitlebarOptions {
                            title: None,
                            appears_transparent: true,
                            traffic_light_position: Some(gpui::point(px(14.), px(14.))),
                        }),
                        app_owns_titlebar_drag: true,
                        ..Default::default()
                    },
                    |_, cx| cx.new(|cx| shell::Shell::new(state.clone(), boot, cx)),
                )
                .unwrap();
            state.update(cx, |_, cx| cx.notify());
            cx.activate(true);
            cx.spawn(async move |cx| {
                let run: anyhow::Result<()> = async {
                    run_phase(&phase, &root, &output, window, cx).await?;
                    Ok(())
                }
                .await;
                if let Err(error) = run {
                    eprintln!("Browser persistence harness failed: {error:#}");
                    *result.lock().unwrap() = Some(error.to_string());
                }
                let _ = window.update(cx, |shell, window, cx| {
                    shell.fixture_blur_browser(window, cx)
                });
                pause(cx, 200).await;
                drop(state);
                let _ = window.update(cx, |_, window, _| window.remove_window());
                pause(cx, 100).await;
                cx.update(|cx| cx.quit());
            })
            .detach();
        });
    if let Some(error) = failure.lock().unwrap().take() {
        anyhow::bail!(error);
    }
    Ok(())
}
