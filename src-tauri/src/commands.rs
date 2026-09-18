/// Tauri commands for first-start onboarding and replays-folder management.
use crate::apply_replays_path;
use crate::donation::DonationConsent;
use crate::link_target::{self, LinkTarget};
use bridge_core::{
    config::AppConfig,
    detection::{
        detect_replays_paths, resolve_replays_folder, validate_replays_folder, DetectedPath,
        SearchRoots,
    },
};
use serde::Serialize;
use std::path::PathBuf;
use std::sync::Arc;
use tauri::{AppHandle, Emitter, Manager};
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
    /// True when the stored `path` differs from what the user picked because we
    /// auto-resolved it to the canonical replays folder (descended into a
    /// `replays` child, or ascended from a version-sorter subfolder). The UI
    /// shows a brief notice when this is set.
    pub corrected: bool,
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

// ── Account gate (td-980190) ─────────────────────────────────────────────
// Single decision point every account-dependent native navigation entry
// point goes through: this file's three commands below, lib.rs's tray
// "Open Battle Monitor" `MenuEvent` arm, and the startup saved-Monitor-view
// restore block. Denied = log-and-no-op, never a silent hide (native gates
// must refuse, not merely hide a button) — see docs/account-authorization.md.

/// True iff `snapshot` currently permits account-dependent native
/// navigation. Mirrors `AccountSession::can_use_account`'s fail-closed
/// semantics at the native snapshot layer (td-811657: false at access
/// expiry even while a renewal retry is in flight) — never re-derived from
/// `AccountStatus` matching, which is exactly the gap `AccountSnapshot`
/// exists to close.
pub(crate) fn account_navigation_allowed(snapshot: &crate::account::AccountSnapshot) -> bool {
    snapshot.can_use_account
}

/// Runs `navigate` iff `snapshot` currently allows account-dependent
/// navigation; otherwise logs `context` and no-ops. Returns whether it
/// navigated, so this file's commands and their tests can assert
/// call-vs-no-call against the exact same decision without a real
/// `AppHandle`.
pub(crate) fn gate_account_navigation(
    snapshot: &crate::account::AccountSnapshot,
    context: &str,
    navigate: impl FnOnce(),
) -> bool {
    if !account_navigation_allowed(snapshot) {
        log::info!("{context}: refused, account not usable");
        return false;
    }
    navigate();
    true
}

/// Navigate the main window to the embedded Battle Monitor.
/// Callable only from the local dashboard: the "main" capability is local-only,
/// so once the window shows the remote monitor page that page gets no IPC.
/// Refuses (log-and-no-op) when the account is not currently usable — the
/// local dashboard that this command is reachable from already re-renders
/// its own connect gate from the same `account_status` poll, so no
/// additional navigation/recovery is needed here.
///
/// Split from the `#[tauri::command]` below into a plain function (the
/// testable command implementation/action seam) so a test can invoke the
/// EXACT decision `open_monitor` makes with a spy in place of
/// `crate::open_monitor_window` — removing the gate from here fails those
/// tests, unlike calling `gate_account_navigation` directly with an
/// unrelated closure.
pub(crate) fn open_monitor_impl(
    snapshot: &crate::account::AccountSnapshot,
    navigate: impl FnOnce(),
) -> bool {
    gate_account_navigation(snapshot, "open_monitor", navigate)
}

#[tauri::command]
pub fn open_monitor(app: AppHandle) {
    let snapshot = app.state::<Arc<crate::account::AccountService>>().snapshot();
    open_monitor_impl(&snapshot, || crate::open_monitor_window(&app));
}

/// Open the in-app RePlayer (WoWS replay player) by navigating the main window to
/// the bridge's own `/player/` route — RePlayer lives IN the main window, not a
/// separate one. Reached from the "RePlayer" title-bar nav button and the
/// double-right-click gesture on the brand (`MONITOR_EMBED_JS`). No-op (logged) if
/// the account is not currently usable (checked first) or the bridge is not
/// running.
///
/// Testable command implementation/action seam (see `open_monitor_impl`):
/// `port` is the already-looked-up bridge port (or `None` if no bridge is
/// running), and `navigate` is the exact `crate::navigate_to_replay_player`
/// call `open_replay_player` makes. Returns whether it navigated.
pub(crate) fn open_replay_player_impl(
    snapshot: &crate::account::AccountSnapshot,
    port: Option<u16>,
    navigate: impl FnOnce(u16),
) -> bool {
    if !account_navigation_allowed(snapshot) {
        log::info!("open_replay_player: refused, account not usable");
        return false;
    }
    match port {
        Some(port) => {
            navigate(port);
            true
        }
        None => {
            log::info!("player: no bridge");
            false
        }
    }
}

#[tauri::command]
pub fn open_replay_player(app: AppHandle) {
    use crate::BridgeState;

    let snapshot = app.state::<Arc<crate::account::AccountService>>().snapshot();
    let port = app
        .state::<BridgeState>()
        .0
        .lock()
        .unwrap()
        .as_ref()
        .map(|ab| ab.bridge.port());
    open_replay_player_impl(&snapshot, port, |port| crate::navigate_to_replay_player(&app, port));
}

/// Navigate the main window to the engine home ("Dashboard" in the unified bar).
/// Used by the local dashboard's nav so it offers the same destinations as the
/// engine-page bar. Routed through `navigate_engine` so the dashboard return-URL
/// is captured first (keeps "Settings" working). Refuses (log-and-no-op) when
/// the account is not currently usable.
///
/// Testable command implementation/action seam (see `open_monitor_impl`).
pub(crate) fn open_engine_home_impl(
    snapshot: &crate::account::AccountSnapshot,
    navigate: impl FnOnce(),
) -> bool {
    gate_account_navigation(snapshot, "open_engine_home", navigate)
}

#[tauri::command]
pub fn open_engine_home(app: AppHandle) {
    let snapshot = app.state::<Arc<crate::account::AccountService>>().snapshot();
    open_engine_home_impl(&snapshot, || crate::navigate_engine(&app, "/"));
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
    // The uploader (td-c8973d) reacts to the decision: opting in triggers
    // the one-time 30-day backfill scan, revoking clears its queue.  The
    // cache write above is what stops uploads immediately — this only
    // handles the queue/backfill bookkeeping.
    crate::on_donation_consent_changed(&app, consent);
    emit_donation_status(&app);
}

/// Return the stored profile-link target as its snake_case string
/// (`"browser"` | `"window"` | `"tab"`). Default is `"browser"`. Thin wrapper
/// over the store helper in `link_target.rs` (store-only pref, like `lastView`).
#[tauri::command]
pub fn get_link_target(app: AppHandle) -> String {
    serde_json::to_value(link_target::read_link_target(&app))
        .ok()
        .and_then(|v| v.as_str().map(str::to_owned))
        .unwrap_or_else(|| "browser".to_string())
}

/// Persist the profile-link target. Unknown strings fall back to `Browser`
/// (no behaviour change), mirroring the conservative store-read default — so a
/// stray IPC value can never select an in-app target.
#[tauri::command]
pub fn set_link_target(app: AppHandle, target: String) {
    let parsed = serde_json::from_value::<LinkTarget>(serde_json::Value::String(target))
        .unwrap_or(LinkTarget::Browser);
    link_target::save_link_target(&app, parsed);
}

// ── Account (td-27bcc9) ──────────────────────────────────────────────────────
// Exactly four commands, matching the architecture directive: safe status,
// begin/cancel/reopen-pending. No token IPC, no caller-supplied URL opener,
// no secret Debug/Serialize/logs, no config.json credentials.

/// Safe account status for the local UI: no tokens, no device key, no flow
/// state — see `account::AccountStatus`.
#[tauri::command]
pub fn account_status(app: AppHandle) -> crate::account::AccountStatus {
    app.state::<Arc<crate::account::AccountService>>().status()
}

/// Begin (or restart) native browser sign-in. Always replaces any existing
/// pending attempt, so a late callback for an old attempt becomes harmless.
#[tauri::command]
pub async fn account_begin_login(app: AppHandle) -> Result<(), String> {
    let service = app.state::<Arc<crate::account::AccountService>>().inner().clone();
    service.begin_login().await.map_err(|e| e.to_string())
}

/// Cancel the pending sign-in attempt, if any.
#[tauri::command]
pub fn account_cancel_login(app: AppHandle) {
    app.state::<Arc<crate::account::AccountService>>().cancel_login();
}

/// Reopen the OS browser on the exact URL already sent for the pending
/// attempt. Takes no URL from the caller — there is nothing here for a
/// compromised or buggy frontend to redirect.
#[tauri::command]
pub fn account_reopen_pending(app: AppHandle) -> Result<(), String> {
    app.state::<Arc<crate::account::AccountService>>()
        .reopen_pending()
        .map_err(|e| e.to_string())
}

/// Confirm a detected or manually entered path and persist it.
#[tauri::command]
pub fn confirm_replays_path(app: AppHandle, path: String) -> SetPathResult {
    store_replays_pick(&app, PathBuf::from(&path))
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
        Some(fp) => fp
            .as_path()
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from(fp.to_string())),
        None => {
            // User cancelled
            return SetPathResult {
                ok: false,
                path: None,
                corrected: false,
                error: None,
            };
        }
    };

    store_replays_pick(&app, folder_path)
}

/// Resolve a user-picked folder to the canonical replays directory, validate
/// it, and persist it. Shared by `confirm_replays_path` (a detected or typed
/// path) and `pick_replays_folder` (the native dialog).
///
/// The resolver fixes the two common mis-picks — selecting the game folder that
/// contains `replays`, or a version-sorter subfolder inside it — and reports
/// whether the stored path was corrected so the UI can show a brief notice. The
/// error message references the original pick, which is what the user saw.
fn store_replays_pick(app: &AppHandle, picked: PathBuf) -> SetPathResult {
    let (final_path, corrected) = match resolve_replays_folder(&picked) {
        Some(r) => (r.path, r.corrected),
        None => (picked.clone(), false),
    };

    if !validate_replays_folder(&final_path) {
        return SetPathResult {
            ok: false,
            path: None,
            corrected: false,
            error: Some(format!(
                "The folder '{}' does not appear to be a WoWS replays directory.",
                picked.display()
            )),
        };
    }

    let mut cfg = load_config(app);
    cfg.replays_path = Some(final_path.clone());
    cfg.onboarding_done = true;
    save_config(app, &cfg);
    apply_replays_path(app, final_path.clone());

    SetPathResult {
        ok: true,
        path: Some(final_path),
        corrected,
        error: None,
    }
}

// ── Internal helpers ───────────────────────────────────────────────────────────

/// Read the persisted config.  Exposed so `lib.rs` can check the replays path
/// at startup without duplicating store access logic.
pub fn read_config(app: &AppHandle) -> AppConfig {
    load_config(app)
}

/// The store-reading half of `load_config`, seamed behind a plain key getter
/// instead of a live `tauri_plugin_store::Store` so a migration test can feed
/// it a synthetic 0.18.0 `config.json` fixture and exercise the SAME parsing
/// this function's only caller (`load_config`) uses — no separate manual
/// `serde_json::Value` field lookup in the test.
pub(crate) fn load_config_from(get: impl Fn(&str) -> Option<serde_json::Value>) -> AppConfig {
    let replays_path = get(KEY_REPLAYS_PATH).and_then(|v| serde_json::from_value::<PathBuf>(v).ok());
    let onboarding_done = get(KEY_ONBOARDING_DONE).and_then(|v| v.as_bool()).unwrap_or(false);
    let replay_watermark_ms = get(KEY_REPLAY_WATERMARK).and_then(|v| v.as_u64());

    AppConfig {
        replays_path,
        onboarding_done,
        replay_watermark_ms,
    }
}

fn load_config(app: &AppHandle) -> AppConfig {
    let store = match app.store(STORE_FILE) {
        Ok(s) => s,
        Err(_) => return AppConfig::new(),
    };
    load_config_from(|key| store.get(key))
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
/// Store-reading half of `read_donation_consent` (see `load_config_from`):
/// same seam, so a migration test exercises the exact parsing the real
/// reader uses against a synthetic fixture instead of a manual field lookup.
pub(crate) fn read_donation_consent_from(get: impl Fn(&str) -> Option<serde_json::Value>) -> DonationConsent {
    DonationConsent::from_store_value(get(KEY_DONATION_CONSENT).as_ref())
}

pub fn read_donation_consent(app: &AppHandle) -> DonationConsent {
    let Ok(store) = app.store(STORE_FILE) else {
        return DonationConsent::Unset;
    };
    read_donation_consent_from(|key| store.get(key))
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

/// Replay donation is ON by default (opt-out) for FRESH installs only. On first
/// launch — onboarding not yet completed AND no donation decision stored — this
/// persists `OptedIn`, so a new user starts opted in (the onboarding card shows
/// it on, with the privacy copy, and they can decline). Existing installs are
/// never touched: a user who never answered the original prompt keeps `Unset`
/// and still gets the one-time ask — no silent opt-in on update. Idempotent.
pub fn seed_fresh_install_donation_default(app: &AppHandle) {
    let Ok(store) = app.store(STORE_FILE) else {
        return;
    };
    let decided = store.get(KEY_DONATION_CONSENT).is_some();
    let onboarding_done = store
        .get(KEY_ONBOARDING_DONE)
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    if decided || onboarding_done {
        return; // existing install, or already decided — leave as-is
    }
    save_donation_consent(app, DonationConsent::OptedIn);
}

/// Launch-on-login defaults ON for FRESH installs only. On first launch — no
/// pref stored AND onboarding not yet completed — this persists `true`, so a new
/// user starts with autostart enabled (the startup reconciliation in `lib.rs`
/// then flips the OS state to match). Existing installs, and anyone who
/// explicitly turned it off (pref present = `false`), are never touched.
/// Idempotent; must run BEFORE the autostart reconciliation.
pub fn seed_fresh_install_launch_on_login(app: &AppHandle) {
    let Ok(store) = app.store(STORE_FILE) else {
        return;
    };
    let pref_set = store.get(KEY_LAUNCH_ON_LOGIN).is_some();
    let onboarding_done = store
        .get(KEY_ONBOARDING_DONE)
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    if pref_set || onboarding_done {
        return;
    }
    store.set(KEY_LAUNCH_ON_LOGIN, serde_json::json!(true));
    if let Err(e) = store.save() {
        log::error!("Failed to seed launch-on-login default: {e}");
    }
}

/// Read the persisted launch-on-login pref from the store. Returns `false` when
/// the key is absent or unparseable — fresh installs still get launch-on-login
/// via the pre-checked onboarding option, which persists the value on completion.
/// Store-reading half of `read_launch_on_login_pref` (see `load_config_from`).
pub(crate) fn read_launch_on_login_pref_from(get: impl Fn(&str) -> Option<serde_json::Value>) -> bool {
    get(KEY_LAUNCH_ON_LOGIN).and_then(|v| v.as_bool()).unwrap_or(false)
}

fn read_launch_on_login_pref(app: &AppHandle) -> bool {
    let Ok(store) = app.store(STORE_FILE) else {
        return false;
    };
    read_launch_on_login_pref_from(|key| store.get(key))
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
            corrected: true,
            error: None,
        };
        let v = serde_json::to_value(&res).expect("serialisation failed");
        // Single-word fields are unchanged by camelCase, but the attribute must
        // still be present and must not break serialisation.
        assert!(v.get("ok").is_some());
        assert!(v.get("path").is_some());
        assert!(v.get("error").is_some());
        // The auto-correct flag the UI reads must be present.
        assert_eq!(v.get("corrected").and_then(|c| c.as_bool()), Some(true));
    }

    // ── Account gate (td-980190) ────────────────────────────────────────────

    use crate::account::{AccountEndReason, AccountService, AccountSnapshot, AccountStatus, ClientProfile};
    use crate::account::traits::{
        AccountTransport, BrowserLauncher, Clock, HttpResponse, SecretStore, SecureRandom,
        StoreError, StoredAccount, StoredProfileData, StoredRefresh, TransportError,
        STORED_PROFILE_SCHEMA_VERSION,
    };
    use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};
    use std::sync::atomic::{AtomicUsize, Ordering as AtomicOrdering};
    use std::sync::Mutex as StdMutex;
    use std::time::{Duration, Instant};

    fn connected_usable_snapshot() -> AccountSnapshot {
        AccountSnapshot {
            status: AccountStatus::Connected {
                nickname: "Helmi".into(),
                clan_tag: Some("TFD".into()),
                realm: Some("eu".into()),
                web_lease_expires_at: Some(1_800_000_900),
            },
            can_use_account: true,
            lifecycle_epoch: 3,
            nickname: Some("Helmi".into()),
            clan_tag: Some("TFD".into()),
            realm: Some("eu".into()),
            web_lease_expires_at: Some(1_800_000_900),
            access_expires_at_unix: Some(1_800_000_900),
            end_reason: None,
        }
    }

    fn signed_out_snapshot() -> AccountSnapshot {
        AccountSnapshot {
            status: AccountStatus::SignedOut,
            can_use_account: false,
            lifecycle_epoch: 0,
            nickname: None,
            clan_tag: None,
            realm: None,
            web_lease_expires_at: None,
            access_expires_at_unix: None,
            end_reason: None,
        }
    }

    /// Access token expired, a renewal retry may still be in flight — the
    /// exact "expired while renewing" gap `AccountSnapshot` exists to close
    /// (`status()` alone maps `Renewing` to the same public `Connected`
    /// shape, but `can_use_account` is already false).
    fn access_expired_while_renewing_snapshot() -> AccountSnapshot {
        AccountSnapshot {
            status: AccountStatus::Connected {
                nickname: "Helmi".into(),
                clan_tag: Some("TFD".into()),
                realm: Some("eu".into()),
                web_lease_expires_at: Some(1_800_000_000),
            },
            can_use_account: false,
            lifecycle_epoch: 3,
            nickname: Some("Helmi".into()),
            clan_tag: Some("TFD".into()),
            realm: Some("eu".into()),
            web_lease_expires_at: Some(1_800_000_000),
            access_expires_at_unix: Some(1_800_000_000),
            end_reason: None,
        }
    }

    fn revoked_snapshot() -> AccountSnapshot {
        AccountSnapshot {
            status: AccountStatus::SignedOut,
            can_use_account: false,
            lifecycle_epoch: 4,
            nickname: None,
            clan_tag: None,
            realm: None,
            web_lease_expires_at: None,
            access_expires_at_unix: None,
            end_reason: Some(AccountEndReason::Revoked),
        }
    }

    fn account_changed_snapshot() -> AccountSnapshot {
        AccountSnapshot {
            status: AccountStatus::SignedOut,
            can_use_account: false,
            lifecycle_epoch: 5,
            nickname: None,
            clan_tag: None,
            realm: None,
            web_lease_expires_at: None,
            access_expires_at_unix: None,
            end_reason: Some(AccountEndReason::AccountChanged),
        }
    }

    fn renewal_exhausted_snapshot() -> AccountSnapshot {
        AccountSnapshot {
            status: AccountStatus::RenewalFailed {
                nickname: "Helmi".into(),
                clan_tag: Some("TFD".into()),
                realm: Some("eu".into()),
            },
            can_use_account: false,
            lifecycle_epoch: 3,
            nickname: Some("Helmi".into()),
            clan_tag: Some("TFD".into()),
            realm: Some("eu".into()),
            web_lease_expires_at: None,
            access_expires_at_unix: None,
            end_reason: Some(AccountEndReason::RenewalExhausted),
        }
    }

    // Each `open_monitor`/`open_replay_player`/`open_engine_home` command
    // body is exactly "gate then navigate" (see the source above) — testing
    // `gate_account_navigation`/`account_navigation_allowed` with the same
    // fixtures a real call site would see IS testing each call site's full
    // decision, without needing a real `AppHandle`/window.

    #[test]
    fn open_monitor_refuses_navigation_when_account_not_connected() {
        // Calls the EXACT function `open_monitor` (the `#[tauri::command]`)
        // delegates to, with a spy in place of `crate::open_monitor_window` —
        // removing the account check from `open_monitor_impl` fails this test.
        let mut navigated = false;
        let did = open_monitor_impl(&signed_out_snapshot(), || navigated = true);
        assert!(!did, "open_monitor must not report navigation when signed out");
        assert!(!navigated, "open_monitor must not call open_monitor_window when signed out");
    }

    #[test]
    fn open_monitor_navigates_when_connected() {
        let mut navigated = false;
        let did = open_monitor_impl(&connected_usable_snapshot(), || navigated = true);
        assert!(did);
        assert!(navigated, "open_monitor must navigate when Connected");
    }

    #[test]
    fn open_replay_player_refuses_navigation_when_account_not_connected() {
        // Same seam as `open_monitor_impl`: a spy in place of
        // `crate::navigate_to_replay_player`, fed a bridge port that IS
        // running, so a `None` result can only come from the account check.
        let mut navigated = None;
        let did = open_replay_player_impl(&signed_out_snapshot(), Some(43210), |port| navigated = Some(port));
        assert!(!did, "open_replay_player must not report navigation when signed out");
        assert_eq!(navigated, None, "open_replay_player must not navigate when signed out");
    }

    #[test]
    fn open_replay_player_allows_navigation_when_connected() {
        let mut navigated = None;
        let did = open_replay_player_impl(&connected_usable_snapshot(), Some(43210), |port| navigated = Some(port));
        assert!(did);
        assert_eq!(navigated, Some(43210), "open_replay_player must navigate to the running bridge's port");
    }

    #[test]
    fn open_replay_player_no_ops_when_connected_but_no_bridge_running() {
        let mut navigated = None;
        let did = open_replay_player_impl(&connected_usable_snapshot(), None, |port| navigated = Some(port));
        assert!(!did);
        assert_eq!(navigated, None);
    }

    #[test]
    fn open_engine_home_refuses_navigation_when_account_not_connected() {
        let mut navigated = false;
        let did = open_engine_home_impl(&signed_out_snapshot(), || navigated = true);
        assert!(!did);
        assert!(!navigated);
    }

    #[test]
    fn open_engine_home_navigates_when_connected() {
        let mut navigated = false;
        let did = open_engine_home_impl(&connected_usable_snapshot(), || navigated = true);
        assert!(did);
        assert!(navigated);
    }

    // ── Mid-session transitions: the native gate flips within one snapshot ──
    // (`account_status`/`AccountSnapshot` is always read fresh — there is no
    // native-side caching to go stale across a poll, so "flips within one
    // poll" reduces to "the gate is a pure function of the latest snapshot",
    // asserted per transition below.)

    #[test]
    fn native_gate_refuses_on_access_token_expiry_even_while_renewing() {
        assert!(!account_navigation_allowed(&access_expired_while_renewing_snapshot()));
    }

    #[test]
    fn native_gate_refuses_on_revoked() {
        let snap = revoked_snapshot();
        assert!(!account_navigation_allowed(&snap));
        assert_eq!(snap.end_reason, Some(AccountEndReason::Revoked));
    }

    #[test]
    fn native_gate_refuses_on_account_changed() {
        let snap = account_changed_snapshot();
        assert!(!account_navigation_allowed(&snap));
        assert_eq!(snap.end_reason, Some(AccountEndReason::AccountChanged));
    }

    #[test]
    fn native_gate_refuses_on_renewal_exhausted() {
        let snap = renewal_exhausted_snapshot();
        assert!(!account_navigation_allowed(&snap));
        assert_eq!(snap.end_reason, Some(AccountEndReason::RenewalExhausted));
    }

    #[test]
    fn renewal_exhausted_signal_is_stable_across_two_immediately_following_polls() {
        // `AccountService::status()` (not re-tested here; owned by
        // account/service.rs) keeps returning the same `RenewalFailed`
        // variant on every poll while still failing, and logs the
        // exhaustion exactly once. This proves OUR gate agrees with itself
        // (no flapping) when handed the same still-failing snapshot twice in
        // a row, matching the once-only UI banner semantics.
        let first = renewal_exhausted_snapshot();
        let second = renewal_exhausted_snapshot();
        assert_eq!(account_navigation_allowed(&first), account_navigation_allowed(&second));
        assert!(!account_navigation_allowed(&first));
    }

    // ── 0.18.0 migration fixture (config.json: replaysPath, onboardingDone,
    // launchOnLogin, donationConsent, lastView; no account keys) ───────────
    //
    // Loads the fixture through the ACTUAL production readers
    // (`load_config_from`, `read_donation_consent_from`,
    // `read_launch_on_login_pref_from`, `lib::read_last_view_from`) via their
    // shared key-getter seam, rather than a manual `serde_json::Value` field
    // lookup — the fixture's only interface is the same `get(key)` closure
    // the real `tauri_plugin_store::Store` handle is wrapped in at each
    // reader's `AppHandle`-taking call site. The account-side half of "not
    // yet migrated" (empty credential store → `SignedOut`, no real keyring)
    // is `fresh_or_upgraded_profile_with_no_stored_credentials_starts_signed_out`
    // below — `AccountService` never reads `config.json`/onboarding keys at
    // all, so the two are asserted independently.

    #[test]
    fn config_migration_0_18_0_fixture_loads_via_production_readers_unchanged() {
        let fixture: serde_json::Value = serde_json::from_str(
            r#"{
                "replaysPath": "C:/Games/World_of_Warships/replays",
                "onboardingDone": true,
                "launchOnLogin": true,
                "donationConsent": "opted_in",
                "lastView": "monitor"
            }"#,
        )
        .expect("fixture must parse");
        let get = |key: &str| fixture.get(key).cloned();

        // AppConfig (bridge_core), via the production loader `load_config`
        // delegates to — the fields it owns deserialise unchanged;
        // replayWatermarkMs did not exist in 0.18.0 and stays None.
        let cfg: AppConfig = load_config_from(get);
        assert_eq!(
            cfg.replays_path,
            Some(std::path::PathBuf::from("C:/Games/World_of_Warships/replays"))
        );
        assert!(cfg.onboarding_done);
        assert_eq!(cfg.replay_watermark_ms, None);

        // Store-only keys this task must never read/clear/reset, through the
        // exact readers `commands.rs`'s/`lib.rs`'s `AppHandle`-taking
        // functions delegate to.
        assert!(read_launch_on_login_pref_from(get));
        assert_eq!(read_donation_consent_from(get), DonationConsent::OptedIn);
        assert_eq!(crate::read_last_view_from(get).as_deref(), Some("monitor"));

        // No account keys present in a 0.18.0 config.json.
        assert!(fixture.get("account").is_none());
        assert!(fixture.get("accountStatus").is_none());
    }

    // ── Account status starts SignedOut on a fresh/upgraded profile ────────
    // (empty mock credential store, independent of folder-onboarding state —
    // `AccountService` never reads `config.json`/onboarding keys at all).

    struct EmptyStore;
    impl SecretStore for EmptyStore {
        fn install_id(&self) -> Result<String, StoreError> {
            Ok("test-install".into())
        }
        fn load_profile(&self, _profile: ClientProfile) -> Result<Option<StoredProfileData>, StoreError> {
            Ok(None)
        }
        fn save_profile(&self, _profile: ClientProfile, _data: &StoredProfileData) -> Result<(), StoreError> {
            Ok(())
        }
        fn clear_profile(&self, _profile: ClientProfile) -> Result<(), StoreError> {
            Ok(())
        }
    }

    struct LiveProfileStore(StdMutex<Option<StoredProfileData>>);
    impl SecretStore for LiveProfileStore {
        fn install_id(&self) -> Result<String, StoreError> {
            Ok("test-install".into())
        }
        fn load_profile(&self, _profile: ClientProfile) -> Result<Option<StoredProfileData>, StoreError> {
            Ok(self.0.lock().unwrap().clone())
        }
        fn save_profile(&self, _profile: ClientProfile, data: &StoredProfileData) -> Result<(), StoreError> {
            *self.0.lock().unwrap() = Some(data.clone());
            Ok(())
        }
        fn clear_profile(&self, _profile: ClientProfile) -> Result<(), StoreError> {
            *self.0.lock().unwrap() = None;
            Ok(())
        }
    }

    struct FixedClock;
    impl Clock for FixedClock {
        fn now_unix(&self) -> Duration {
            Duration::from_secs(2_000_000_000)
        }
        fn now_instant(&self) -> Instant {
            Instant::now()
        }
    }

    struct ZeroRandom;
    impl SecureRandom for ZeroRandom {
        fn fill(&self, buf: &mut [u8]) {
            buf.fill(7);
        }
    }

    #[derive(Default)]
    struct RecordingBrowser {
        calls: AtomicUsize,
    }
    impl BrowserLauncher for RecordingBrowser {
        fn open(&self, _url: &str) -> bool {
            self.calls.fetch_add(1, AtomicOrdering::SeqCst);
            true
        }
    }

    struct AlwaysFailTransport;
    #[async_trait::async_trait]
    impl AccountTransport for AlwaysFailTransport {
        async fn post_json(
            &self,
            _path: &str,
            _headers: Vec<(String, String)>,
            _body: serde_json::Value,
        ) -> Result<HttpResponse, TransportError> {
            Err(TransportError::Network)
        }
    }

    #[tokio::test]
    async fn fresh_or_upgraded_profile_with_no_stored_credentials_starts_signed_out() {
        let service = AccountService::new(
            Arc::new(AlwaysFailTransport),
            Arc::new(EmptyStore),
            Arc::new(RecordingBrowser::default()),
            Arc::new(FixedClock),
            Arc::new(ZeroRandom),
            ClientProfile::Debug,
        );
        assert_eq!(service.status(), AccountStatus::SignedOut);
        assert!(!service.snapshot().can_use_account);
    }

    #[tokio::test]
    async fn bootstrap_never_invokes_the_browser_launcher_on_the_autostart_startup_path() {
        // A live stored refresh (the 0.18.0-upgrade-with-an-existing-Bridge-
        // login case bootstrap actually tries to recover) — the point is
        // proving the one-shot restart refresh never opens the OS browser
        // regardless of whether the refresh attempt itself succeeds.
        let stored = StoredProfileData {
            schema_version: STORED_PROFILE_SCHEMA_VERSION,
            device_key_scalar_b64: URL_SAFE_NO_PAD.encode([9u8; 32]),
            device_id: Some("dev_test".into()),
            refresh_current: Some(StoredRefresh {
                refresh_token: "refresh_test".into(),
                refresh_expires_at_unix: 4_000_000_000,
                refresh_absolute_expires_at_unix: 4_000_000_000,
                account: StoredAccount {
                    nickname: "Helmi".into(),
                    clan_tag: Some("TFD".into()),
                    realm: Some("eu".into()),
                },
            }),
            refresh_pending: None,
        };
        let browser = Arc::new(RecordingBrowser::default());
        let service = Arc::new(AccountService::new(
            Arc::new(AlwaysFailTransport),
            Arc::new(LiveProfileStore(StdMutex::new(Some(stored)))),
            browser.clone(),
            Arc::new(FixedClock),
            Arc::new(ZeroRandom),
            ClientProfile::Debug,
        ));

        service.bootstrap().await;

        assert_eq!(
            browser.calls.load(AtomicOrdering::SeqCst),
            0,
            "bootstrap's one-shot restart refresh must never call the browser launcher \
             — begin_login (the only browser-opening call) is reachable only from the \
             explicit Connect/Retry button IPC command, never the startup/autostart path"
        );
    }
}
