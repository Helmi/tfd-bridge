mod commands;

use bridge_core::server::{self, Bridge};
use std::sync::Mutex;
use tauri::menu::{Menu, MenuItem};
use tauri::tray::TrayIconBuilder;
use tauri::{Manager, RunEvent};

/// Managed state that holds the bridge handle.
/// `None` when the replays path is not yet configured.
struct BridgeState(Mutex<Option<Bridge>>);

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_store::Builder::new().build())
        .plugin(tauri_plugin_dialog::init())
        .manage(BridgeState(Mutex::new(None)))
        .setup(|app| {
            if cfg!(debug_assertions) {
                app.handle().plugin(
                    tauri_plugin_log::Builder::default()
                        .level(log::LevelFilter::Info)
                        .build(),
                )?;
            }

            let quit = MenuItem::with_id(app, "quit", "Quit", true, None::<&str>)?;
            let menu = Menu::with_items(app, &[&quit])?;
            let _tray = TrayIconBuilder::new()
                .icon(app.default_window_icon().unwrap().clone())
                .menu(&menu)
                .on_menu_event(|app, event| {
                    if event.id.as_ref() == "quit" {
                        app.exit(0);
                    }
                })
                .build(app)?;

            // Start the bridge if onboarding is complete and a replays path is set.
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
                // Stop the bridge cleanly on exit.
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
