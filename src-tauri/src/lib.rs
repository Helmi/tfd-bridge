mod commands;

use bridge_core::server::{self, Bridge};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use tauri::menu::{CheckMenuItem, Menu, MenuItem, PredefinedMenuItem};
use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
use tauri::{AppHandle, Manager, RunEvent, WebviewUrl, WebviewWindowBuilder};
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
            log::info!("Bridge already serving {:?} — no change", path);
        }
        BridgeAction::Start | BridgeAction::Restart => {
            // Stop the existing bridge (if any) before starting a new one.
            if let Some(ab) = guard.take() {
                ab.bridge.stop();
            }
            let dev_origin = std::env::var("TFD_BRIDGE_DEV_ORIGIN").ok();
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

// ── Monitor window ───────────────────────────────────────────────────────────

/// Open (or focus) the Battle Monitor webview window.
///
/// If the window already exists (possibly hidden), it is shown and focused.
/// Otherwise a new WebviewWindow is created loading the remote monitor URL.
///
/// SECURITY: This window has label "monitor", which is absent from every
/// capability's `windows` list (default.json only covers "main").
/// Tauri v2 grants no IPC permissions to windows not listed in any
/// capability, so the remote origin cannot invoke app commands.
/// `withGlobalTauri=true` injects window.__TAURI__ but invoke() calls
/// are blocked by the capability model — not by this code.
///
/// NOTE: In-webview login depends on the identity provider permitting
/// requests from a desktop WebView user-agent. If engine.tfd.rocks blocks
/// embedded-webview login, users must log in via their browser; the local
/// bridge (the core feature) is unaffected either way.
pub(crate) fn open_monitor_window(app: &AppHandle) {
    if let Some(win) = app.get_webview_window("monitor") {
        let _ = win.show();
        let _ = win.set_focus();
    } else {
        let url: tauri::Url = "https://engine.tfd.rocks/monitor".parse().unwrap();
        match WebviewWindowBuilder::new(app, "monitor", WebviewUrl::External(url))
            .title("Battle Monitor")
            .inner_size(1280.0, 800.0)
            .build()
        {
            Ok(_) => {}
            Err(e) => log::error!("Failed to open monitor window: {e}"),
        }
    }
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    let builder = tauri::Builder::default()
        .plugin(tauri_plugin_updater::Builder::new().build())
        .plugin(tauri_plugin_store::Builder::new().build())
        .plugin(tauri_plugin_dialog::init())
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
        .on_window_event(|window, event| {
            if let tauri::WindowEvent::CloseRequested { api, .. } = event {
                // Hide instead of destroying the window so the tray Open item
                // can revive it and the app stays alive in the tray.
                api.prevent_close();
                let _ = window.hide();
            }
        })
        .setup(|app| {
            if cfg!(debug_assertions) {
                app.handle().plugin(
                    tauri_plugin_log::Builder::default()
                        .level(log::LevelFilter::Info)
                        .build(),
                )?;
            }

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
                        open_monitor_window(tray.app_handle());
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
                        app.exit(0);
                    }
                    _ => {}
                })
                .build(app)?;

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
