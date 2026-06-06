/// Tauri commands for first-start onboarding and replays-folder management.
use crate::apply_replays_path;
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
const KEY_LAUNCH_ON_LOGIN: &str = "launchOnLogin";

// ── Response types ─────────────────────────────────────────────────────────────

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OnboardingStatus {
    pub needs_onboarding: bool,
    pub detected: Vec<DetectedPath>,
    pub current_path: Option<PathBuf>,
    pub launch_on_login: bool,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
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
    let launch_on_login = read_launch_on_login_pref(&app);

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
        launch_on_login,
    }
}

/// Enable or disable launch-on-login. Toggles OS autostart, persists the pref,
/// and syncs the tray checkmark via the shared helper.
#[tauri::command]
pub fn set_launch_on_login(app: AppHandle, enabled: bool) {
    crate::set_launch_on_login_internal(&app, enabled);
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
    apply_replays_path(&app, candidate.clone());

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
    apply_replays_path(&app, folder_path.clone());

    SetPathResult {
        ok: true,
        path: Some(folder_path),
        error: None,
    }
}

// ── Internal helpers ───────────────────────────────────────────────────────────

/// Read the persisted config.  Exposed so `lib.rs` can check the replays path
/// at startup without duplicating store access logic.
pub fn read_config(app: &AppHandle) -> AppConfig {
    load_config(app)
}

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

/// Read the persisted launch-on-login pref from the store.
fn read_launch_on_login_pref(app: &AppHandle) -> bool {
    let Ok(store) = app.store(STORE_FILE) else {
        return false;
    };
    store
        .get(KEY_LAUNCH_ON_LOGIN)
        .and_then(|v| v.as_bool())
        .unwrap_or(false)
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

// ── Tests ──────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    /// Guard the serde field-name contract. Tauri v2 does not auto-camelCase
    /// return values; the struct must declare #[serde(rename_all = "camelCase")].
    /// This test will fail if the derive attribute is removed.
    #[test]
    fn onboarding_status_serialises_camel_case() {
        let status = OnboardingStatus {
            needs_onboarding: true,
            detected: vec![],
            current_path: Some(std::path::PathBuf::from("/some/path")),
            launch_on_login: false,
        };
        let v = serde_json::to_value(&status).expect("serialisation failed");
        // camelCase keys must be present
        assert!(
            v.get("needsOnboarding").is_some(),
            "expected 'needsOnboarding', got: {v}"
        );
        assert!(
            v.get("currentPath").is_some(),
            "expected 'currentPath', got: {v}"
        );
        assert!(
            v.get("launchOnLogin").is_some(),
            "expected 'launchOnLogin', got: {v}"
        );
        // snake_case keys must NOT be present
        assert!(
            v.get("needs_onboarding").is_none(),
            "snake_case 'needs_onboarding' must not appear in the serialised output"
        );
        assert!(
            v.get("current_path").is_none(),
            "snake_case 'current_path' must not appear in the serialised output"
        );
        assert!(
            v.get("launch_on_login").is_none(),
            "snake_case 'launch_on_login' must not appear in the serialised output"
        );
    }

    #[test]
    fn set_path_result_serialises_camel_case() {
        let res = SetPathResult {
            ok: true,
            path: Some(std::path::PathBuf::from("/replays")),
            error: None,
        };
        let v = serde_json::to_value(&res).expect("serialisation failed");
        // Single-word fields are unchanged by camelCase, but the attribute must
        // still be present and must not break serialisation.
        assert!(v.get("ok").is_some());
        assert!(v.get("path").is_some());
        assert!(v.get("error").is_some());
    }
}
