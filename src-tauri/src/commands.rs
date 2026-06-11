/// Tauri commands for first-start onboarding and replays-folder management.
use crate::apply_replays_path;
use crate::donation::DonationConsent;
use bridge_core::{
    config::AppConfig,
    detection::{detect_replays_paths, validate_replays_folder, DetectedPath, SearchRoots},
};
use serde::Serialize;
use std::path::PathBuf;
use tauri::{AppHandle, Emitter};
use tauri_plugin_dialog::DialogExt;
use tauri_plugin_store::StoreExt;

const STORE_FILE: &str = "config.json";
const KEY_REPLAYS_PATH: &str = "replaysPath";
const KEY_ONBOARDING_DONE: &str = "onboardingDone";
const KEY_LAUNCH_ON_LOGIN: &str = "launchOnLogin";
const KEY_REPLAY_WATERMARK: &str = "replayWatermarkMs";
const KEY_DONATION_CONSENT: &str = "donationConsent";

/// Event fired at the dashboard whenever the donation status may have changed
/// (a consent decision, or an engine feature-flag refresh). Payload is a
/// `DonationStatus`.
pub const EVENT_DONATION_STATUS: &str = "donation-status-changed";

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

/// Snapshot of the replay-donation status for the dashboard: the persisted
/// tri-state consent plus the engine's `replay_donation` feature flag. The
/// UI keeps the consent controls visible either way and only annotates the
/// flag; the actual uploads-allowed AND-gate lives with the uploader
/// (td-c8973d).
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DonationStatus {
    pub consent: DonationConsent,
    pub remote_enabled: bool,
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

/// Navigate the main window to the embedded Battle Monitor.
/// Callable only from the local dashboard: the "main" capability is local-only,
/// so once the window shows the remote monitor page that page gets no IPC.
#[tauri::command]
pub fn open_monitor(app: AppHandle) {
    crate::open_monitor_window(&app);
}

/// Return the donation consent + remote-flag snapshot for the dashboard.
/// While consent reads `unset` the UI shows the one active ask (onboarding
/// for new installs, first dashboard run after update for existing ones).
#[tauri::command]
pub fn get_donation_status(app: AppHandle) -> DonationStatus {
    donation_status(&app)
}

/// Persist a donation decision (from the active ask or the settings toggle).
/// `opted_in: true` → OptedIn, `false` → Declined — the UI can never write
/// `Unset` back, so once answered the ask never returns. The in-memory cache
/// is synced before the event fires, so revoking stops the uploader
/// (td-c8973d) immediately.
#[tauri::command]
pub fn set_donation_consent(app: AppHandle, opted_in: bool) {
    let consent = if opted_in {
        DonationConsent::OptedIn
    } else {
        DonationConsent::Declined
    };
    save_donation_consent(&app, consent);
    crate::donation::set_consent(consent);
    emit_donation_status(&app);
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
    let replay_watermark_ms = store.get(KEY_REPLAY_WATERMARK).and_then(|v| v.as_u64());

    AppConfig {
        replays_path,
        onboarding_done,
        replay_watermark_ms,
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
    if let Some(wm) = cfg.replay_watermark_ms {
        store.set(KEY_REPLAY_WATERMARK, serde_json::json!(wm));
    }

    if let Err(e) = store.save() {
        log::error!("Failed to save store: {e}");
    }
}

/// Persist the replay-finalized watermark if `candidate` advances it.
/// Called from the bridge's finalized-event callback (worker thread) and from
/// the first-run seeding in `lib.rs` — the monotonic guard makes both safe.
pub fn save_replay_watermark(app: &AppHandle, candidate_ms: u64) {
    let Ok(store) = app.store(STORE_FILE) else {
        log::error!("Failed to open store when saving replay watermark");
        return;
    };
    let existing = store.get(KEY_REPLAY_WATERMARK).and_then(|v| v.as_u64());
    let Some(next) = advance_watermark(existing, candidate_ms) else {
        return; // candidate does not advance the watermark — nothing to write
    };
    store.set(KEY_REPLAY_WATERMARK, serde_json::json!(next));
    if let Err(e) = store.save() {
        log::error!("Failed to save replay watermark: {e}");
    }
}

/// Pure monotonic-advance rule for the watermark: returns the new value only
/// when `candidate` is strictly greater than the existing one.
fn advance_watermark(existing: Option<u64>, candidate: u64) -> Option<u64> {
    match existing {
        Some(e) if candidate <= e => None,
        _ => Some(candidate),
    }
}

/// Build the current donation snapshot, reading consent through the store so
/// the answer is correct even if a call races the startup cache seeding; the
/// uploader's cache is re-synced from the same read.
fn donation_status(app: &AppHandle) -> DonationStatus {
    let consent = read_donation_consent(app);
    crate::donation::set_consent(consent);
    DonationStatus {
        consent,
        remote_enabled: crate::engine::features().replay_donation,
    }
}

/// Emit the donation-status event so the dashboard updates live — fired after
/// a consent decision and after an engine feature-flag refresh (lib.rs).
pub fn emit_donation_status(app: &AppHandle) {
    if let Err(e) = app.emit(EVENT_DONATION_STATUS, donation_status(app)) {
        log::warn!("Failed to emit donation status: {e}");
    }
}

/// Read the persisted donation consent from the store. Absent or garbled
/// values read as `Unset` (see `DonationConsent::from_store_value`). Public
/// so lib.rs can seed the uploader-facing cache at startup.
pub fn read_donation_consent(app: &AppHandle) -> DonationConsent {
    let Ok(store) = app.store(STORE_FILE) else {
        return DonationConsent::Unset;
    };
    DonationConsent::from_store_value(store.get(KEY_DONATION_CONSENT).as_ref())
}

/// Persist the donation consent decision.
fn save_donation_consent(app: &AppHandle, consent: DonationConsent) {
    let Ok(store) = app.store(STORE_FILE) else {
        log::error!("Failed to open store when saving donation consent");
        return;
    };
    store.set(
        KEY_DONATION_CONSENT,
        serde_json::to_value(consent).unwrap_or(serde_json::Value::Null),
    );
    if let Err(e) = store.save() {
        log::error!("Failed to save donation consent: {e}");
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

    // ── replay watermark advance rule ────────────────────────────────────────

    #[test]
    fn watermark_seeds_when_absent() {
        assert_eq!(advance_watermark(None, 100), Some(100));
    }

    #[test]
    fn watermark_advances_on_newer_candidate() {
        assert_eq!(advance_watermark(Some(100), 200), Some(200));
    }

    #[test]
    fn watermark_never_moves_backwards_or_repeats() {
        assert_eq!(advance_watermark(Some(200), 200), None);
        assert_eq!(advance_watermark(Some(200), 100), None);
    }

    /// Guard the donation-status wire contract: camelCase field names, and
    /// the consent enum as its snake_case string value (what the UI's
    /// renderDonation switches on).
    #[test]
    fn donation_status_serialises_camel_case() {
        let status = DonationStatus {
            consent: DonationConsent::OptedIn,
            remote_enabled: true,
        };
        let v = serde_json::to_value(&status).expect("serialisation failed");
        assert_eq!(
            v.get("consent").and_then(|c| c.as_str()),
            Some("opted_in"),
            "consent must serialise as its snake_case string, got: {v}"
        );
        assert_eq!(
            v.get("remoteEnabled").and_then(|r| r.as_bool()),
            Some(true),
            "expected 'remoteEnabled', got: {v}"
        );
        assert!(
            v.get("remote_enabled").is_none(),
            "snake_case 'remote_enabled' must not appear in the serialised output"
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
