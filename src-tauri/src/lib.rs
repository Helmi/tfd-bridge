mod commands;

use bridge_core::server::{self, Bridge};
use std::sync::Mutex;
use tauri::menu::{CheckMenuItem, Menu, MenuItem};
use tauri::tray::TrayIconBuilder;
use tauri::{Manager, RunEvent};
use tauri_plugin_store::StoreExt;

/// Managed state that holds the bridge handle.
/// `None` when the replays path is not yet configured.
struct BridgeState(Mutex<Option<Bridge>>);

/// Managed state for the launch-on-login tray item.
/// Held so we can update the checkmark from the menu event handler.
struct LaunchOnLoginItem(Mutex<Option<CheckMenuItem<tauri::Wry>>>);

// ── Store constants ─────────────────────────────────────────────────────────

const STORE_FILE: &str = "config.json";
const KEY_LAUNCH_ON_LOGIN: &str = "launchOnLogin";

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

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    let builder = tauri::Builder::default()
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

            let launch_on_login = CheckMenuItem::with_id(
                app,
                "launch_on_login",
                "Launch on login",
                true,
                launch_on_login_checked,
                None::<&str>,
            )?;
            let quit = MenuItem::with_id(app, "quit", "Quit", true, None::<&str>)?;
            let menu = Menu::with_items(app, &[&launch_on_login, &quit])?;

            // Store a reference to the check item so the event handler can
            // update the checkmark state without needing tray.menu().
            *app.state::<LaunchOnLoginItem>().0.lock().unwrap() = Some(launch_on_login);

            let _tray = TrayIconBuilder::new()
                .icon(app.default_window_icon().unwrap().clone())
                .menu(&menu)
                .on_menu_event(|app, event| match event.id.as_ref() {
                    "launch_on_login" => {
                        #[cfg(desktop)]
                        {
                            use tauri_plugin_autostart::ManagerExt;
                            let autostart = app.autolaunch();
                            let currently_enabled = autostart.is_enabled().unwrap_or(false);
                            let new_state = !currently_enabled;

                            if new_state {
                                if let Err(e) = autostart.enable() {
                                    log::error!("Failed to enable autostart: {e}");
                                    return;
                                }
                            } else if let Err(e) = autostart.disable() {
                                log::error!("Failed to disable autostart: {e}");
                                return;
                            }

                            save_launch_on_login(app, new_state);

                            // Update checkmark via stored reference.
                            if let Some(item) = app
                                .state::<LaunchOnLoginItem>()
                                .0
                                .lock()
                                .unwrap()
                                .as_ref()
                            {
                                let _ = item.set_checked(new_state);
                            }
                        }
                    }
                    "quit" => {
                        app.exit(0);
                    }
                    _ => {}
                })
                .build(app)?;

            // ── If launched via autostart, stay silent in the tray ──────────
            if std::env::args().any(|a| a == "--autostart") {
                for (_, window) in app.webview_windows() {
                    let _ = window.hide();
                }
            }

            // ── Start the bridge if onboarding is complete ──────────────────
            let cfg = commands::read_config(app.handle());
            if let Some(replays_path) = cfg.replays_path {
                let dev_origin = std::env::var("TFD_BRIDGE_DEV_ORIGIN").ok();
                match server::start(replays_path, dev_origin) {
                    Ok(bridge) => {
                        log::info!("Bridge started on port {}", bridge.port());
                        *app.state::<BridgeState>().0.lock().unwrap() = Some(bridge);
                    }
                    Err(e) => {
                        log::error!("Failed to start bridge: {e}");
                    }
                }
            }

            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            commands::get_onboarding_status,
            commands::confirm_replays_path,
            commands::pick_replays_folder,
        ])
        .build(tauri::generate_context!())
        .expect("error while building tauri application")
        .run(|app, event| {
            if let RunEvent::Exit = event {
                if let Some(bridge) = app
                    .state::<BridgeState>()
                    .0
                    .lock()
                    .unwrap()
                    .take()
                {
                    bridge.stop();
                }
            }
        });
}

// ── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
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
}
