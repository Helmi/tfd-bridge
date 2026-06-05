mod commands;

use bridge_core::server::{self, Bridge};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use tauri::menu::{CheckMenuItem, Menu, MenuItem};
use tauri::tray::TrayIconBuilder;
use tauri::{Manager, RunEvent};
use tauri_plugin_store::StoreExt;

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
            let launch_on_login = CheckMenuItem::with_id(
                app,
                "launch_on_login",
                "Launch on login",
                true,
                launch_on_login_checked,
                None::<&str>,
            )?;
            let quit = MenuItem::with_id(app, "quit", "Quit", true, None::<&str>)?;
            let menu = Menu::with_items(app, &[&open, &launch_on_login, &quit])?;

            // Store a reference to the check item so the event handler can
            // update the checkmark state without needing tray.menu().
            *app.state::<LaunchOnLoginItem>().0.lock().unwrap() = Some(launch_on_login);

            let _tray = TrayIconBuilder::new()
                .icon(app.default_window_icon().unwrap().clone())
                .menu(&menu)
                .on_menu_event(|app, event| match event.id.as_ref() {
                    "open" => {
                        if let Some(window) = app.get_webview_window("main") {
                            let _ = window.unminimize();
                            let _ = window.show();
                            let _ = window.set_focus();
                        }
                    }
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
                apply_replays_path(app.handle(), replays_path);
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
