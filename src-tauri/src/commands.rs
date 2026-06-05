/// Tauri commands for first-start onboarding and replays-folder management.
use bridge_core::{
    config::AppConfig,
    detection::{detect_replays_paths, validate_replays_folder, DetectedPath, SearchRoots},
};
use serde::Serialize;
use std::path::PathBuf;
use tauri::AppHandle;
use tauri_plugin_dialog::DialogExt;
use tauri_plugin_store::StoreExt;

const STORE_FILE: &str = "config.json";
const KEY_REPLAYS_PATH: &str = "replaysPath";
const KEY_ONBOARDING_DONE: &str = "onboardingDone";

// ── Response types ─────────────────────────────────────────────────────────────

#[derive(Debug, Serialize)]
pub struct OnboardingStatus {
    pub needs_onboarding: bool,
    pub detected: Vec<DetectedPath>,
    pub current_path: Option<PathBuf>,
}

#[derive(Debug, Serialize)]
pub struct SetPathResult {
    pub ok: bool,
    pub path: Option<PathBuf>,
    pub error: Option<String>,
}

// ── Commands ───────────────────────────────────────────────────────────────────

/// Return current onboarding status and any auto-detected candidates.
#[tauri::command]
pub fn get_onboarding_status(app: AppHandle) -> OnboardingStatus {
    let cfg = load_config(&app);

    let detected = if cfg.needs_onboarding() {
        let roots = platform_search_roots();
        detect_replays_paths(&roots)
    } else {
        vec![]
    };

    OnboardingStatus {
        needs_onboarding: cfg.needs_onboarding(),
        detected,
        current_path: cfg.replays_path,
    }
}

/// Confirm a detected or manually entered path and persist it.
#[tauri::command]
pub fn confirm_replays_path(app: AppHandle, path: String) -> SetPathResult {
    let candidate = PathBuf::from(&path);
    if !validate_replays_folder(&candidate) {
        return SetPathResult {
            ok: false,
            path: None,
            error: Some(format!(
                "The folder '{}' does not appear to be a WoWS replays directory.",
                path
            )),
        };
    }
    let mut cfg = load_config(&app);
    cfg.replays_path = Some(candidate.clone());
    cfg.onboarding_done = true;
    save_config(&app, &cfg);

    SetPathResult {
        ok: true,
        path: Some(candidate),
        error: None,
    }
}

/// Open a native folder-picker dialog and validate + persist the result.
/// Returns the selected path or an error message.
#[tauri::command]
pub fn pick_replays_folder(app: AppHandle) -> SetPathResult {
    let picked = app
        .dialog()
        .file()
        .set_title("Select WoWS Replays Folder")
        .blocking_pick_folder();

    let folder_path = match picked {
        Some(fp) => fp.as_path().map(PathBuf::from).unwrap_or_else(|| {
            PathBuf::from(fp.to_string())
        }),
        None => {
            // User cancelled
            return SetPathResult {
                ok: false,
                path: None,
                error: None,
            };
        }
    };

    if !validate_replays_folder(&folder_path) {
        return SetPathResult {
            ok: false,
            path: None,
            error: Some(format!(
                "The folder '{}' does not appear to be a WoWS replays directory.",
                folder_path.display()
            )),
        };
    }

    let mut cfg = load_config(&app);
    cfg.replays_path = Some(folder_path.clone());
    cfg.onboarding_done = true;
    save_config(&app, &cfg);

    SetPathResult {
        ok: true,
        path: Some(folder_path),
        error: None,
    }
}

// ── Internal helpers ───────────────────────────────────────────────────────────

fn load_config(app: &AppHandle) -> AppConfig {
    let store = match app.store(STORE_FILE) {
        Ok(s) => s,
        Err(_) => return AppConfig::new(),
    };
    let replays_path = store
        .get(KEY_REPLAYS_PATH)
        .and_then(|v| serde_json::from_value::<PathBuf>(v).ok());
    let onboarding_done = store
        .get(KEY_ONBOARDING_DONE)
        .and_then(|v| v.as_bool())
        .unwrap_or(false);

    AppConfig {
        replays_path,
        onboarding_done,
    }
}

fn save_config(app: &AppHandle, cfg: &AppConfig) {
    let Ok(store) = app.store(STORE_FILE) else {
        log::error!("Failed to open store for writing");
        return;
    };

    if let Some(ref path) = cfg.replays_path {
        store.set(
            KEY_REPLAYS_PATH,
            serde_json::to_value(path).unwrap_or(serde_json::Value::Null),
        );
    }
    store.set(KEY_ONBOARDING_DONE, serde_json::json!(cfg.onboarding_done));

    if let Err(e) = store.save() {
        log::error!("Failed to save store: {e}");
    }
}

/// Returns the platform-appropriate search roots.
/// On non-Windows platforms this returns empty roots — detection simply
/// finds nothing, which is the correct behaviour on macOS in production.
fn platform_search_roots() -> SearchRoots {
    #[cfg(target_os = "windows")]
    {
        SearchRoots::windows_defaults()
    }
    #[cfg(not(target_os = "windows"))]
    {
        SearchRoots {
            steam_roots: vec![],
            wgc_roots: vec![],
        }
    }
}
