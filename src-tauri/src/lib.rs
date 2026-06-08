mod commands;

use bridge_core::server::{self, Bridge};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;
use tauri::menu::{CheckMenuItem, Menu, MenuItem, PredefinedMenuItem};
use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
use tauri::{AppHandle, Manager, RunEvent};
use tauri_plugin_store::StoreExt;

// ── Auto-update ──────────────────────────────────────────────────────────────

/// Check for an update and, when found, prompt the user to install it.
/// On an `--autostart` (silent/login) launch, the modal is skipped — the check
/// still runs so the result is logged, but we never pop a dialog at login.
async fn check_for_update(app: AppHandle, silent: bool) {
    use tauri_plugin_updater::UpdaterExt;

    let updater = match app.updater() {
        Ok(u) => u,
        Err(e) => {
            log::warn!("Updater not available: {e}");
            return;
        }
    };

    let update = match updater.check().await {
        Ok(Some(u)) => u,
        Ok(None) => {
            log::info!("TFD Bridge is up to date");
            return;
        }
        Err(e) => {
            log::warn!("Update check failed (will retry next launch): {e}");
            return;
        }
    };

    log::info!(
        "Update available: {} → {}",
        env!("CARGO_PKG_VERSION"),
        update.version
    );

    if silent {
        // Do not interrupt a login-triggered launch with a modal.
        return;
    }

    let confirmed = {
        use tauri_plugin_dialog::{DialogExt, MessageDialogButtons};
        app.dialog()
            .message(format!(
                "TFD Bridge {} is available. Install and restart now?",
                update.version
            ))
            .title("Update Available")
            .buttons(MessageDialogButtons::OkCancelCustom(
                "Install".to_string(),
                "Later".to_string(),
            ))
            .blocking_show()
    };

    if !confirmed {
        return;
    }

    if let Err(e) = update
        .download_and_install(
            |_chunk, _total| {},
            || log::info!("Update download finished"),
        )
        .await
    {
        log::error!("Failed to install update: {e}");
        return;
    }

    app.restart();
}

// ── Bridge state ─────────────────────────────────────────────────────────────

/// A running bridge paired with the path it is serving.
struct ActiveBridge {
    path: PathBuf,
    bridge: Bridge,
}

/// Managed state that holds the bridge handle.
/// `None` when the replays path is not yet configured.
struct BridgeState(Mutex<Option<ActiveBridge>>);

// ── Tray state ────────────────────────────────────────────────────────────────

/// Managed state for the launch-on-login tray item.
/// Held so we can update the checkmark from the menu event handler.
struct LaunchOnLoginItem(Mutex<Option<CheckMenuItem<tauri::Wry>>>);

/// Set to `true` just before `app.exit()` (from the tray Quit item).
/// The window-close handler checks this: while `false` it closes-to-tray
/// (prevent_close + hide); once `true` it lets every window close so the
/// exit is not vetoed by `prevent_close` (which deadlocks shutdown).
struct Quitting(AtomicBool);

/// The main window's initial (local dashboard) URL, captured at setup so the
/// embedded monitor's "← Dashboard" sentinel can navigate back to it.
struct DashboardUrl(Mutex<Option<tauri::Url>>);

// ── Store constants ─────────────────────────────────────────────────────────

const STORE_FILE: &str = "config.json";
const KEY_LAUNCH_ON_LOGIN: &str = "launchOnLogin";

// ── Bridge action logic ──────────────────────────────────────────────────────

/// What should happen to the bridge when a new replays path is applied.
#[derive(Debug, PartialEq, Eq)]
pub enum BridgeAction {
    /// No bridge is running — start one.
    Start,
    /// The bridge is running on a different path — restart it.
    Restart,
    /// The bridge is already serving this exact path — nothing to do.
    Noop,
}

/// Decide what bridge action to take based on the currently-served path and
/// the newly-requested path.  Pure function — no I/O, easily testable.
pub fn decide_bridge_action(current: Option<&Path>, requested: &Path) -> BridgeAction {
    match current {
        None => BridgeAction::Start,
        Some(cur) if cur == requested => BridgeAction::Noop,
        Some(_) => BridgeAction::Restart,
    }
}

// ── Bridge management ────────────────────────────────────────────────────────

/// Apply a new replays path: start, restart, or leave the bridge unchanged.
/// Called from setup (on existing path) and from commands (after onboarding).
pub(crate) fn apply_replays_path(app: &tauri::AppHandle, path: PathBuf) {
    let state = app.state::<BridgeState>();
    let mut guard = state.0.lock().unwrap();
    let current = guard.as_ref().map(|ab| ab.path.as_path());
    let action = decide_bridge_action(current, &path);

    match action {
        BridgeAction::Noop => {
            log::info!("Bridge already serving the configured path — no change");
        }
        BridgeAction::Start | BridgeAction::Restart => {
            // Stop the existing bridge (if any) before starting a new one.
            if let Some(ab) = guard.take() {
                ab.bridge.stop();
            }
            // Only honour TFD_BRIDGE_DEV_ORIGIN in debug builds so release
            // builds keep CORS strictly limited to https://engine.tfd.rocks.
            let dev_origin = if cfg!(debug_assertions) {
                std::env::var("TFD_BRIDGE_DEV_ORIGIN").ok()
            } else {
                None
            };
            match server::start(path.clone(), dev_origin) {
                Ok(bridge) => {
                    log::info!("Bridge started on port {}", bridge.port());
                    *guard = Some(ActiveBridge { path, bridge });
                }
                Err(e) => {
                    log::error!("Failed to start bridge: {e}");
                }
            }
        }
    }
}

// ── Autostart helpers ────────────────────────────────────────────────────────

/// Read the persisted launch-on-login preference.
/// Returns `false` when the key is absent or cannot be parsed (opt-in default).
fn read_launch_on_login(app: &tauri::AppHandle) -> bool {
    let Ok(store) = app.store(STORE_FILE) else {
        return false;
    };
    store
        .get(KEY_LAUNCH_ON_LOGIN)
        .and_then(|v| v.as_bool())
        .unwrap_or(false)
}

/// Persist the launch-on-login preference.
fn save_launch_on_login(app: &tauri::AppHandle, enabled: bool) {
    let Ok(store) = app.store(STORE_FILE) else {
        log::error!("Failed to open store when saving launch-on-login pref");
        return;
    };
    store.set(KEY_LAUNCH_ON_LOGIN, serde_json::json!(enabled));
    if let Err(e) = store.save() {
        log::error!("Failed to save launch-on-login pref: {e}");
    }
}

/// Toggle OS autostart, persist the pref, and sync the tray checkmark.
/// Single source of truth for all callers (tray handler, IPC command).
pub(crate) fn set_launch_on_login_internal(app: &tauri::AppHandle, enabled: bool) {
    #[cfg(desktop)]
    {
        use tauri_plugin_autostart::ManagerExt;
        let autostart = app.autolaunch();
        if enabled {
            if let Err(e) = autostart.enable() {
                log::error!("Failed to enable autostart: {e}");
                return;
            }
        } else if let Err(e) = autostart.disable() {
            log::error!("Failed to disable autostart: {e}");
            return;
        }
    }

    save_launch_on_login(app, enabled);

    // Sync the tray checkmark via the stored reference.
    if let Some(item) = app
        .state::<LaunchOnLoginItem>()
        .0
        .lock()
        .unwrap()
        .as_ref()
    {
        let _ = item.set_checked(enabled);
    }
}

// ── Battle Monitor (embedded in the main window) ───────────────────────────────

const MONITOR_URL: &str = "https://engine.tfd.rocks/monitor";
/// Same-origin sentinel paths the injected JS navigates to. The `monitor-embed`
/// plugin's `on_navigation` hook intercepts and cancels them BEFORE the request
/// goes out, then performs the real action (open browser / go to dashboard).
const SENTINEL_EXTERNAL: &str = "/__tfd_open_external";
const SENTINEL_DASHBOARD: &str = "/__tfd_dashboard";

/// JS injected into every page (via the `monitor-embed` plugin). On the remote
/// Battle Monitor origin it adds a slim top bar with a Back-to-Dashboard control
/// and routes explicit new-tab links to the system browser through a same-origin
/// sentinel — so the OAuth redirect flow and internal navigation stay in-app and
/// the remote page needs NO Tauri IPC. On the local dashboard it is a no-op.
const MONITOR_EMBED_JS: &str = r#"
(function () {
  if (location.origin !== 'https://engine.tfd.rocks') return;
  var ORIGIN = 'https://engine.tfd.rocks';
  function openExternal(href) {
    // Defence in depth: only route http(s) links to the system browser.
    if (href && /^https?:\/\//i.test(href)) {
      location.assign(ORIGIN + '/__tfd_open_external?url=' + encodeURIComponent(href));
    }
  }
  // Only intercept explicit new-tab links; same-window navigation (incl. the
  // Wargaming OAuth redirect) is left untouched so login still works in-app.
  document.addEventListener('click', function (e) {
    var a = e.target && e.target.closest ? e.target.closest('a[href]') : null;
    if (a && a.target === '_blank') { e.preventDefault(); openExternal(a.href); }
  }, true);
  function winApi() {
    try { return window.__TAURI__.window.getCurrentWindow(); } catch (e) { return null; }
  }
  function mkBtn(label, tip, onClick) {
    var b = document.createElement('button');
    b.textContent = label;
    b.title = tip;
    b.style.cssText = 'background:transparent;border:1px solid rgba(255,255,255,0.16);color:#dfe6e8;border-radius:6px;padding:4px 9px;cursor:pointer;font:inherit;line-height:1;';
    b.addEventListener('click', onClick);
    return b;
  }
  function injectBar() {
    if (document.getElementById('tfd-embed-bar') || !document.body) return;
    var bar = document.createElement('div');
    bar.id = 'tfd-embed-bar';
    // The bar itself is the drag handle (buttons inside stay clickable: Tauri
    // only starts a drag when the mousedown target carries the attribute).
    bar.setAttribute('data-tauri-drag-region', '');
    bar.style.cssText = 'position:fixed;top:0;left:0;right:0;height:34px;z-index:2147483647;display:flex;align-items:center;gap:8px;padding:0 8px;background:#05070e;border-bottom:1px solid rgba(255,255,255,0.1);font:600 12px/1 -apple-system,Segoe UI,sans-serif;color:#dfe6e8;-webkit-user-select:none;user-select:none;';
    var back = mkBtn('← Dashboard', 'Back to Dashboard', function () { location.assign(ORIGIN + '/__tfd_dashboard'); });
    var title = document.createElement('span');
    title.setAttribute('data-tauri-drag-region', '');
    title.style.cssText = 'pointer-events:none;opacity:0.7;';
    title.appendChild(document.createTextNode('Battle Monitor'));
    var spacer = document.createElement('div');
    spacer.setAttribute('data-tauri-drag-region', '');
    spacer.style.cssText = 'flex:1;';
    var min = mkBtn('—', 'Minimize', function () { var w = winApi(); if (w) w.minimize(); });
    var close = mkBtn('✕', 'Close to tray', function () { var w = winApi(); if (w) w.close(); });
    bar.appendChild(back);
    bar.appendChild(title);
    bar.appendChild(spacer);
    bar.appendChild(min);
    bar.appendChild(close);
    document.documentElement.appendChild(bar);
    // Push page content below the fixed bar WITHOUT adding to total scroll height.
    // body { padding-top } alone makes 100vh children overflow by the bar height
    // (100vh + 34px = overscroll). Instead: offset body down by 34px AND shrink
    // min-h-screen / h-screen by the same amount so the net height stays 100vh.
    var style = document.createElement('style');
    style.id = 'tfd-embed-bar-style';
    style.textContent = [
      'body{padding-top:34px!important;}',
      '.min-h-screen{min-height:calc(100vh - 34px)!important;}',
      '.h-screen{height:calc(100vh - 34px)!important;}'
    ].join('');
    document.head.appendChild(style);
  }
  if (document.readyState === 'loading') {
    document.addEventListener('DOMContentLoaded', injectBar);
  } else {
    injectBar();
  }
})();
"#;

/// Navigate the main window to the embedded Battle Monitor and bring it forward.
pub(crate) fn open_monitor_window(app: &AppHandle) {
    if let Some(win) = app.get_webview_window("main") {
        // Capture the dashboard URL we are currently on (fully loaded) so the
        // monitor's "← Dashboard" can navigate back to the exact same URL.
        match win.url() {
            Ok(current) => {
                log::info!("open_monitor: captured dashboard URL {current}");
                *app.state::<DashboardUrl>().0.lock().unwrap() = Some(current);
            }
            Err(e) => log::warn!("open_monitor: could not capture dashboard URL: {e}"),
        }
        match MONITOR_URL.parse::<tauri::Url>() {
            Ok(url) => {
                if let Err(e) = win.navigate(url) {
                    log::error!("open_monitor: navigate to monitor failed: {e}");
                }
            }
            Err(e) => log::error!("open_monitor: bad monitor URL: {e}"),
        }
        let _ = win.unminimize();
        let _ = win.show();
        let _ = win.set_focus();
    }
}

/// Bring the main window forward (tray "Open" / tray left-click).
fn show_main_window(app: &AppHandle) {
    if let Some(win) = app.get_webview_window("main") {
        let _ = win.unminimize();
        let _ = win.show();
        let _ = win.set_focus();
    }
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    let builder = tauri::Builder::default()
        .plugin(tauri_plugin_updater::Builder::new().build())
        .plugin(tauri_plugin_store::Builder::new().build())
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_opener::init())
        .plugin(
            // Embeds the Battle Monitor in the main window: injects a slim
            // top bar + new-tab→browser routing, and intercepts the same-origin
            // sentinel URLs. The remote page never gets Tauri IPC.
            tauri::plugin::Builder::<tauri::Wry>::new("monitor-embed")
                .js_init_script(MONITOR_EMBED_JS.to_string())
                .on_navigation(|webview, url| {
                    log::info!("on_navigation: {url}");
                    if url.host_str() == Some("engine.tfd.rocks") {
                        match url.path() {
                            SENTINEL_EXTERNAL => {
                                use tauri_plugin_opener::OpenerExt;
                                if let Some(target) = url
                                    .query_pairs()
                                    .find(|(k, _)| k == "url")
                                    .map(|(_, v)| v.into_owned())
                                {
                                    // Only hand http/https to the system opener — never
                                    // file://, custom schemes, or anything else the OS
                                    // default handler might act on.
                                    if let Ok(parsed) = tauri::Url::parse(&target) {
                                        if matches!(parsed.scheme(), "http" | "https") {
                                            let _ = webview
                                                .app_handle()
                                                .opener()
                                                .open_url(parsed.as_str(), None::<&str>);
                                        }
                                    }
                                }
                                return false;
                            }
                            SENTINEL_DASHBOARD => {
                                let app = webview.app_handle().clone();
                                tauri::async_runtime::spawn(async move {
                                    let dash =
                                        app.state::<DashboardUrl>().0.lock().unwrap().clone();
                                    log::info!("back-to-dashboard: target {dash:?}");
                                    match (app.get_webview_window("main"), dash) {
                                        (Some(win), Some(url)) => {
                                            if let Err(e) = win.navigate(url) {
                                                log::error!("back-to-dashboard navigate failed: {e}");
                                            }
                                        }
                                        _ => log::error!(
                                            "back-to-dashboard: missing window or captured URL"
                                        ),
                                    }
                                });
                                return false;
                            }
                            _ => {}
                        }
                    }
                    true
                })
                .build(),
        )
        .manage(BridgeState(Mutex::new(None)));

    // Register autostart plugin on desktop platforms only.
    // Pass --autostart so we can detect a login-triggered launch and stay
    // in the tray without raising a window.
    #[cfg(desktop)]
    let builder = builder.plugin(tauri_plugin_autostart::init(
        tauri_plugin_autostart::MacosLauncher::LaunchAgent,
        Some(vec!["--autostart"]),
    ));

    builder
        .manage(LaunchOnLoginItem(Mutex::new(None)))
        .manage(Quitting(AtomicBool::new(false)))
        .manage(DashboardUrl(Mutex::new(None)))
        .on_window_event(|window, event| {
            if let tauri::WindowEvent::CloseRequested { api, .. } = event {
                // Once Quit has set the flag, do NOT prevent the close: vetoing
                // it here deadlocks `app.exit()` (each window would veto its own
                // close), which is what made the app unquittable. Let it close.
                if window.state::<Quitting>().0.load(Ordering::SeqCst) {
                    return;
                }
                // Otherwise close-to-tray: keep the app alive in the tray so the
                // tray "Open" item can revive the window.
                api.prevent_close();
                let _ = window.hide();
            }
        })
        .setup(|app| {
            // Log to a file (and stdout) in ALL builds so field issues on
            // Windows are debuggable without a custom build. Written to the
            // platform log dir (Windows: %APPDATA%/rocks.tfd.bridge/logs).
            app.handle().plugin(
                tauri_plugin_log::Builder::default()
                    .level(log::LevelFilter::Info)
                    .build(),
            )?;

            // ── Reconcile autostart OS state with stored pref ──────────────
            #[cfg(desktop)]
            {
                use tauri_plugin_autostart::ManagerExt;
                let autostart = app.autolaunch();
                let pref = read_launch_on_login(app.handle());
                match autostart.is_enabled() {
                    Ok(os_enabled) if os_enabled != pref => {
                        if pref {
                            let _ = autostart.enable();
                        } else {
                            let _ = autostart.disable();
                        }
                    }
                    _ => {}
                }
            }

            // ── Build tray menu ──────────────────────────────────────────────
            let launch_on_login_checked = read_launch_on_login(app.handle());

            let open = MenuItem::with_id(app, "open", "Open", true, None::<&str>)?;
            let open_monitor =
                MenuItem::with_id(app, "open_monitor", "Open Battle Monitor", true, None::<&str>)?;
            let launch_on_login = CheckMenuItem::with_id(
                app,
                "launch_on_login",
                "Launch on login",
                true,
                launch_on_login_checked,
                None::<&str>,
            )?;
            let quit = MenuItem::with_id(app, "quit", "Quit", true, None::<&str>)?;
            let sep = PredefinedMenuItem::separator(app)?;
            let menu =
                Menu::with_items(app, &[&open, &open_monitor, &sep, &launch_on_login, &quit])?;

            // Store a reference to the check item so the event handler can
            // update the checkmark state without needing tray.menu().
            *app.state::<LaunchOnLoginItem>().0.lock().unwrap() = Some(launch_on_login);

            let _tray = TrayIconBuilder::new()
                .icon(app.default_window_icon().unwrap().clone())
                .menu(&menu)
                .show_menu_on_left_click(false)
                .on_tray_icon_event(|tray, event| {
                    if let TrayIconEvent::Click {
                        button: MouseButton::Left,
                        button_state: MouseButtonState::Up,
                        ..
                    } = event
                    {
                        show_main_window(tray.app_handle());
                    }
                })
                .on_menu_event(|app, event| match event.id.as_ref() {
                    "open" => {
                        if let Some(window) = app.get_webview_window("main") {
                            let _ = window.unminimize();
                            let _ = window.show();
                            let _ = window.set_focus();
                        }
                    }
                    "open_monitor" => {
                        open_monitor_window(app);
                    }
                    "launch_on_login" => {
                        // Read current OS state to determine new toggle value.
                        #[cfg(desktop)]
                        let currently_enabled = {
                            use tauri_plugin_autostart::ManagerExt;
                            app.autolaunch().is_enabled().unwrap_or(false)
                        };
                        #[cfg(not(desktop))]
                        let currently_enabled = read_launch_on_login(app);

                        set_launch_on_login_internal(app, !currently_enabled);
                    }
                    "quit" => {
                        // Mark quitting so the window-close handler stops vetoing
                        // closes, then exit (RunEvent::Exit stops the bridge).
                        app.state::<Quitting>().0.store(true, Ordering::SeqCst);
                        app.exit(0);
                    }
                    _ => {}
                })
                .build(app)?;

            // ── Capture the local dashboard URL for the monitor "← Dashboard" ─
            if let Some(main) = app.get_webview_window("main") {
                if let Ok(url) = main.url() {
                    *app.state::<DashboardUrl>().0.lock().unwrap() = Some(url);
                }
            }

            // ── If launched via autostart, stay silent in the tray ──────────
            let autostart_launch = std::env::args().any(|a| a == "--autostart");
            if autostart_launch {
                for (_, window) in app.webview_windows() {
                    let _ = window.hide();
                }
            }

            // ── Check for updates in the background ──────────────────────────
            {
                let handle = app.handle().clone();
                tauri::async_runtime::spawn(async move {
                    check_for_update(handle, autostart_launch).await;
                });
            }

            // ── Start the bridge if onboarding is complete ──────────────────
            let cfg = commands::read_config(app.handle());
            if let Some(replays_path) = cfg.replays_path {
                apply_replays_path(app.handle(), replays_path);
            }

            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            commands::get_onboarding_status,
            commands::confirm_replays_path,
            commands::pick_replays_folder,
            commands::set_launch_on_login,
            commands::open_monitor,
        ])
        .build(tauri::generate_context!())
        .expect("error while building tauri application")
        .run(|app, event| {
            if let RunEvent::Exit = event {
                if let Some(ab) = app
                    .state::<BridgeState>()
                    .0
                    .lock()
                    .unwrap()
                    .take()
                {
                    ab.bridge.stop();
                }
            }
        });
}

// ── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    /// Verify the pref-parsing logic: None → false (opt-in: OFF by default).
    #[test]
    fn launch_on_login_default_is_false() {
        let v: Option<serde_json::Value> = None;
        let result = v.and_then(|v| v.as_bool()).unwrap_or(false);
        assert!(!result, "default should be false (opt-in)");
    }

    #[test]
    fn launch_on_login_persists_true() {
        let v = serde_json::json!(true);
        let result = Some(v).and_then(|v| v.as_bool()).unwrap_or(false);
        assert!(result);
    }

    #[test]
    fn launch_on_login_persists_false() {
        let v = serde_json::json!(false);
        let result = Some(v).and_then(|v| v.as_bool()).unwrap_or(false);
        assert!(!result);
    }

    // ── decide_bridge_action tests ────────────────────────────────────────────

    #[test]
    fn bridge_action_start_when_no_current() {
        let requested = Path::new("/game/replays");
        assert_eq!(decide_bridge_action(None, requested), BridgeAction::Start);
    }

    #[test]
    fn bridge_action_noop_when_same_path() {
        let path = Path::new("/game/replays");
        assert_eq!(
            decide_bridge_action(Some(path), path),
            BridgeAction::Noop
        );
    }

    #[test]
    fn bridge_action_restart_when_path_changes() {
        let current = Path::new("/game/replays");
        let new_path = Path::new("/other/replays");
        assert_eq!(
            decide_bridge_action(Some(current), new_path),
            BridgeAction::Restart
        );
    }
}
