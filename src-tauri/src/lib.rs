mod commands;
pub mod donation;
pub mod engine;
pub mod link_target;
pub mod uploader;

use bridge_core::battle_result::{DecodeConfig, Tables};
use bridge_core::detection::derive_game_dir;
use bridge_core::finalize::{FinalizeOptions, FinalizedCallback};
use bridge_core::server::{self, Bridge, DecodeContext};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use tauri::menu::{CheckMenuItem, Menu, MenuItem, PredefinedMenuItem};
use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
use tauri::{AppHandle, Manager, RunEvent};
use tauri_plugin_store::StoreExt;
use tauri_plugin_window_state::{AppHandleExt, StateFlags};

// ── Auto-update ──────────────────────────────────────────────────────────────

/// Controls how `check_for_update` behaves on each call site.
#[derive(Clone, Copy, PartialEq, Eq)]
enum CheckMode {
    /// Login / `--autostart` launch: run the check but never show any dialog.
    Login,
    /// Hourly background check: prompt only when an update is actually available.
    Hourly,
    /// Manual tray item: prompt on update AND show an "up to date" confirmation.
    Manual,
}

/// Check for an update and handle the result according to `mode`:
/// - `Login`  — silent; no dialog even when an update is found.
/// - `Hourly` — quiet when up to date; prompts when an update is available.
/// - `Manual` — always shows a result (update prompt or "up to date" dialog).
async fn check_for_update(app: AppHandle, mode: CheckMode) {
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
            if mode == CheckMode::Manual {
                use tauri_plugin_dialog::DialogExt;
                app.dialog()
                    .message("TFD Bridge is already up to date.")
                    .title("No Update Available")
                    .blocking_show();
            }
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

    if mode == CheckMode::Login {
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

// ── Engine remote config ─────────────────────────────────────────────────────

/// Refresh the engine bridge-config (see `engine.rs`) and, when the engine
/// demands a newer bridge (HTTP 426, or `min_bridge_version` above the
/// running version), surface the existing updater flow as a visible nudge.
async fn refresh_engine_config(app: AppHandle) {
    let outcome = engine::refresh_config().await;
    // A fresh config may have flipped the replay_donation flag — let the
    // dashboard's donation card reflect it without waiting for a reload.
    if matches!(outcome, engine::RefreshOutcome::Updated { .. }) {
        commands::emit_donation_status(&app);
    }
    let needs_nudge = match outcome {
        engine::RefreshOutcome::Updated { upgrade_nudge } => upgrade_nudge,
        engine::RefreshOutcome::UpgradeRequired => true,
        engine::RefreshOutcome::Skipped | engine::RefreshOutcome::Failed => false,
    };
    // claim_update_nudge() succeeds once per run, so the nudge can never nag
    // on every hourly refresh.
    if needs_nudge && engine::claim_update_nudge() {
        check_for_update(app, CheckMode::Hourly).await;
    }
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

/// Managed state for the donation upload pipeline (td-c8973d).  One uploader
/// per active bridge — created alongside it in `apply_replays_path`, so
/// `None` exactly while no bridge runs.
struct UploaderState(Mutex<Option<Arc<uploader::Uploader>>>);

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
const KEY_LAST_VIEW: &str = "lastView";

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

// ── Decode context wiring (td-865788) ────────────────────────────────────────

/// Resolve the resources directory (constants.json + ship_index.json).
///
/// Resources are declared in tauri.conf.json as `"resources/constants.json"` etc.,
/// so Tauri's bundler preserves the `resources/` path component: the files land at
/// `<resource_dir>/resources/constants.json`, NOT `<resource_dir>/constants.json`.
/// On Windows `resource_dir()` always returns the executable's own directory, so we
/// must look inside the `resources/` subdirectory.
///
/// During `cargo tauri dev` / raw `cargo run` we fall back to the repo's
/// `src-tauri/resources/` directory (gated to debug builds).
fn resolve_resources_dir(app: &tauri::AppHandle) -> Option<PathBuf> {
    // Prefer the Tauri path resolver: resources land in <resource_dir>/resources/.
    use tauri::Manager;
    if let Ok(dir) = app.path().resource_dir() {
        let candidate = dir.join("resources");
        if candidate.join("constants.json").exists() {
            log::info!("Resources found at {}", candidate.display());
            return Some(candidate);
        }
    }

    // Debug-only fallback: repo src-tauri/resources/ (walk up from the binary).
    // Gated to debug builds so release installs rely solely on the Tauri resolver.
    #[cfg(debug_assertions)]
    {
        if let Ok(exe) = std::env::current_exe() {
            let mut dir = exe
                .parent()
                .unwrap_or(std::path::Path::new("."))
                .to_path_buf();
            for _ in 0..6 {
                let candidate = dir.join("src-tauri").join("resources");
                if candidate.join("constants.json").exists() {
                    log::info!("Dev resources found at {}", candidate.display());
                    return Some(candidate);
                }
                if !dir.pop() {
                    break;
                }
            }
        }
    }

    log::warn!(
        "Resource dir (constants.json / ship_index.json) not found; battle-result feature disabled"
    );
    None
}

/// Build the `DecodeContext` from the resolved replays path.  Returns `None`
/// when the resources or game directory cannot be resolved, or when
/// `Tables::load` fails. In all error cases the bridge still starts and serves
/// replays/donation — only the `/result` endpoints are unavailable.
fn build_decode_context(app: &tauri::AppHandle, replays_path: &Path) -> Option<Arc<DecodeContext>> {
    let resources_dir = resolve_resources_dir(app)?;

    let constants_path = resources_dir.join("constants.json");
    let ship_index_path = resources_dir.join("ship_index.json");

    if !constants_path.exists() {
        log::warn!(
            "constants.json not found at {}; battle-result feature disabled",
            constants_path.display()
        );
        return None;
    }
    if !ship_index_path.exists() {
        log::warn!(
            "ship_index.json not found at {}; battle-result feature disabled",
            ship_index_path.display()
        );
        return None;
    }

    // Derive game dir from replays path (e.g. C:\Games\WoWS\replays → C:\Games\WoWS).
    let game_dir = match derive_game_dir(replays_path) {
        Some(d) => {
            log::info!("Derived game_dir: {}", d.display());
            d
        }
        None => {
            log::warn!(
                "Could not derive game_dir from replays path {}; battle-result feature disabled",
                replays_path.display()
            );
            return None;
        }
    };

    let cfg = DecodeConfig {
        game_dir,
        constants_path: constants_path.clone(),
        ship_index_path: ship_index_path.clone(),
    };

    match Tables::load(&constants_path, &ship_index_path) {
        Ok(tables) => {
            log::info!("Tables loaded; battle-result feature active");
            Some(Arc::new(DecodeContext::new(cfg, tables)))
        }
        Err(e) => {
            log::error!("Tables::load failed ({e}); battle-result feature disabled");
            None
        }
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
            // Stop the existing bridge (if any) before starting a new one,
            // and the donation uploader tied to it — a fresh one is created
            // below against the (possibly different) replays dir.
            if let Some(ab) = guard.take() {
                ab.bridge.stop();
            }
            if let Some(up) = app.state::<UploaderState>().0.lock().unwrap().take() {
                up.cancel();
            }
            // Only honour TFD_BRIDGE_DEV_ORIGIN in debug builds so release
            // builds keep CORS strictly limited to https://engine.tfd.rocks.
            let dev_origin = if cfg!(debug_assertions) {
                std::env::var("TFD_BRIDGE_DEV_ORIGIN").ok()
            } else {
                None
            };
            // Donation upload pipeline (td-c8973d): consumes the finalized
            // events below; gates on consent + the engine feature flag itself.
            let donation_uploader = uploader::Uploader::new(uploader::UploaderOptions::production(
                path.clone(),
                donation_ledger_path(app),
            ));
            // Replay-finalized detection: subscribe with a callback that logs
            // the event, persists the watermark so catch-up after a restart
            // never re-emits, and hands the file to the donation uploader
            // (which only queues it — heavy work stays off this callback).
            let watermark_ms = ensure_replay_watermark(app);
            let handle = app.clone();
            let uploader_cb = Arc::clone(&donation_uploader);
            let on_finalized: FinalizedCallback = Arc::new(move |event| {
                log::info!(
                    "Replay finalized: {} ({} bytes, mtime {} ms)",
                    event.name,
                    event.size,
                    event.modified_ms
                );
                commands::save_replay_watermark(&handle, event.modified_ms);
                uploader_cb.on_replay_finalized(&event.path);
            });
            let finalize = FinalizeOptions::new(watermark_ms, on_finalized);
            // Build the battle-result decode context from the configured replays
            // path.  `None` when the sidecar or resources are missing — the
            // bridge still starts and the /result endpoints return 501 instead.
            let decode_ctx = build_decode_context(app, &path);
            match server::start_full(path.clone(), dev_origin, Some(finalize), decode_ctx) {
                Ok(bridge) => {
                    log::info!("Bridge started on port {}", bridge.port());
                    *guard = Some(ActiveBridge { path, bridge });
                    // Startup catch-up + backfill resume: scan for donate-able
                    // replays (30-day window, not in the ledger).  No-op while
                    // consent is not opted in; the opt-in command triggers the
                    // scan for users who consent later.
                    if donation::consent().is_opted_in() {
                        donation_uploader.spawn_scan();
                    }
                    *app.state::<UploaderState>().0.lock().unwrap() = Some(donation_uploader);
                }
                Err(e) => {
                    log::error!("Failed to start bridge: {e}");
                    donation_uploader.cancel();
                }
            }
        }
    }
}

/// Read the persisted replay-finalized watermark, seeding it with "now" on
/// first run.  Seeding (instead of 0) keeps the first catch-up scan from
/// re-announcing the user's entire replay history as freshly finalized — the
/// deliberate, throttled 30-day backfill is the donation uploader's concern
/// (td-c8973d).  The seed is persisted immediately so battles finished while
/// the app is closed are caught up from here on.
fn ensure_replay_watermark(app: &tauri::AppHandle) -> u64 {
    if let Some(wm) = commands::read_config(app).replay_watermark_ms {
        return wm;
    }
    let now_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0);
    commands::save_replay_watermark(app, now_ms);
    now_ms
}

/// Where the donation upload ledger lives — next to the config store in the
/// per-app data dir (Windows: %APPDATA%/rocks.tfd.bridge/donation-ledger.json).
fn donation_ledger_path(app: &tauri::AppHandle) -> PathBuf {
    match app.path().app_data_dir() {
        Ok(dir) => dir.join("donation-ledger.json"),
        Err(e) => {
            // Practically unreachable on desktop; fall back to the working
            // dir rather than disabling the (fail-safe) ledger entirely.
            log::error!("Cannot resolve the app data dir for the donation ledger: {e}");
            PathBuf::from("donation-ledger.json")
        }
    }
}

/// React to a donation-consent decision (called by the consent command):
/// opting in triggers the one-time 30-day backfill scan; revoking clears the
/// upload queue immediately (the consent cache already gates the worker).
pub(crate) fn on_donation_consent_changed(
    app: &tauri::AppHandle,
    consent: donation::DonationConsent,
) {
    let state = app.state::<UploaderState>();
    let guard = state.0.lock().unwrap();
    let Some(up) = guard.as_ref() else {
        // No bridge yet (onboarding incomplete) — apply_replays_path runs the
        // scan itself once the uploader exists, consent permitting.
        return;
    };
    match consent {
        donation::DonationConsent::OptedIn => up.spawn_scan(),
        donation::DonationConsent::Declined => up.revoke(),
        donation::DonationConsent::Unset => {}
    }
}

// ── Autostart helpers ────────────────────────────────────────────────────────

/// Read the persisted launch-on-login preference.
/// Returns `false` when the key is absent or cannot be parsed. Fresh installs
/// still get launch-on-login via the pre-checked onboarding option, which
/// persists the value on completion.
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

/// Persist the last-active view (`"dashboard"` or `"monitor"`).
fn save_last_view(app: &tauri::AppHandle, view: &str) {
    let Ok(store) = app.store(STORE_FILE) else {
        log::error!("Failed to open store when saving last-view pref");
        return;
    };
    store.set(KEY_LAST_VIEW, serde_json::json!(view));
    if let Err(e) = store.save() {
        log::error!("Failed to save last-view pref: {e}");
    }
}

/// Read the persisted last-active view.
/// Returns `None` when the key is absent (first run) or cannot be parsed.
fn read_last_view(app: &tauri::AppHandle) -> Option<String> {
    let Ok(store) = app.store(STORE_FILE) else {
        return None;
    };
    store
        .get(KEY_LAST_VIEW)
        .and_then(|v| v.as_str().map(|s| s.to_owned()))
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
    if let Some(item) = app.state::<LaunchOnLoginItem>().0.lock().unwrap().as_ref() {
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
  function winLabel() {
    var w = winApi();
    return (w && typeof w.label === 'string') ? w.label : '';
  }
  function injectBar() {
    if (document.getElementById('tfd-embed-bar') || !document.body) return;
    // The bar shows in two contexts (read synchronously off the global Tauri
    // window API — no IPC, no permission):
    //  - profile-* window → New Window mode: one profile in its own window; the
    //    main window owns cross-view navigation, so this bar is just a Close.
    //  - main window      → a PERSISTENT nav bar on EVERY engine page (monitor,
    //    profile, clan, …): history Back/Forward + Dashboard + Battle Monitor,
    //    ALWAYS, independent of the current page.
    var isProfile = winLabel().indexOf('profile-') === 0;
    var bar = document.createElement('div');
    bar.id = 'tfd-embed-bar';
    // The bar itself is the drag handle (buttons inside stay clickable: Tauri
    // only starts a drag when the mousedown target carries the attribute).
    bar.setAttribute('data-tauri-drag-region', '');
    bar.style.cssText = 'position:fixed;top:0;left:0;right:0;height:34px;z-index:2147483647;display:flex;align-items:center;gap:8px;padding:0 8px;background:#05070e;border-bottom:1px solid rgba(255,255,255,0.1);font:600 12px/1 -apple-system,Segoe UI,sans-serif;color:#dfe6e8;-webkit-user-select:none;user-select:none;';
    var leftControls = [];
    var rightControls = [];
    var closeTip;
    if (isProfile) {
      // New Window mode: a single profile in its own top-level window. No
      // cross-view nav here — just a REAL close (the CloseRequested handler
      // only closes-to-tray for label "main").
      closeTip = 'Close';
    } else {
      // Main window: the same persistent controls on EVERY engine page. Left:
      // history Back/Forward + the two engine destinations — Dashboard (engine
      // home '/') and Battle Monitor ('/monitor'). Right (pushed over): Settings
      // = the local TFD Bridge page, reached via the same-origin sentinel
      // /__tfd_dashboard (named for the sentinel, NOT the engine Dashboard).
      leftControls.push(mkBtn('‹', 'Back', function () { history.back(); }));
      leftControls.push(mkBtn('›', 'Forward', function () { history.forward(); }));
      leftControls.push(mkBtn('Dashboard', 'Go to the engine Dashboard', function () { location.assign(ORIGIN + '/'); }));
      leftControls.push(mkBtn('Battle Monitor', 'Go to the live Battle Monitor', function () { location.assign(ORIGIN + '/monitor'); }));
      rightControls.push(mkBtn('Settings', 'TFD Bridge settings', function () { location.assign(ORIGIN + '/__tfd_dashboard'); }));
      closeTip = 'Close to tray';
    }
    var title = document.createElement('span');
    title.setAttribute('data-tauri-drag-region', '');
    // The title is also the flexible drag area (it pushes Settings + the window
    // controls to the right); it truncates so they always stay visible.
    title.style.cssText = 'flex:1;min-width:0;overflow:hidden;text-overflow:ellipsis;white-space:nowrap;text-align:center;pointer-events:none;opacity:0.7;padding:0 6px;';
    title.appendChild(document.createTextNode(isProfile ? 'Player Profile' : (document.title || 'TFD Bridge')));
    var min = mkBtn('—', 'Minimize', function () { var w = winApi(); if (w) w.minimize(); });
    var close = mkBtn('✕', closeTip, function () { var w = winApi(); if (w) w.close(); });
    leftControls.forEach(function (b) { bar.appendChild(b); });
    bar.appendChild(title);
    rightControls.forEach(function (b) { bar.appendChild(b); });
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
      // The engine app-shell anchors its top elements with fixed/sticky
      // positioning, which ignore `body{padding-top}` (content slid UNDER the
      // bar). Transform <body> (NOT <html>): a transformed element becomes the
      // containing block for its fixed descendants, so the page content — fixed
      // elements included — shifts down by the bar height. The bar is appended to
      // <html> (a sibling of <body>), so transforming <body> leaves the bar
      // pinned at top:0 while everything inside <body> moves below it. Cap body
      // height so the page is not left 34px too tall.
      'body{transform:translateY(34px)!important;height:calc(100vh - 34px)!important;}',
      // Full-viewport-height containers must shrink by the bar height too — left
      // a whole viewport tall inside the 34px-shorter <body> they overflow it and
      // add a SECOND scrollbar (the page's own scroll PLUS the body overflow).
      // Cover Tailwind's screen + dynamic-viewport (dvh/svh/lvh) height utilities.
      '.h-screen,.h-dvh,.h-svh,.h-lvh{height:calc(100vh - 34px)!important;}',
      '.min-h-screen,.min-h-dvh,.min-h-svh,.min-h-lvh{min-height:calc(100vh - 34px)!important;}'
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
                } else {
                    save_last_view(app, "monitor");
                }
            }
            Err(e) => log::error!("open_monitor: bad monitor URL: {e}"),
        }
        let _ = win.unminimize();
        let _ = win.show();
        let _ = win.set_focus();
    }
}

/// Label prefix for profile-link windows (the `Window` link target) and tabs
/// (the `Tab` link target). The capability scoping (`profile.json`), the
/// label-aware injected bar, the CloseRequested guard, and any future
/// window-state filter MUST all agree on this prefix.
const PROFILE_LABEL_PREFIX: &str = "profile-";

/// Monotonic counter for unique profile-window labels within a run. Labels only
/// need to be unique while the process lives, so a plain incrementing counter is
/// enough — values never need to be reused.
static PROFILE_SEQ: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);

/// Decide what to do with an intercepted external link from the Battle Monitor.
///
/// SECURITY — the gates run BEFORE any in-app load and BEFORE the setting is
/// consulted:
/// 1. Unparseable URL → do nothing.
/// 2. Non-`http(s)` scheme (e.g. `file://`, custom schemes) → do nothing.
/// 3. Any host other than `engine.tfd.rocks` → ALWAYS the system browser,
///    regardless of the `LinkTarget` setting. Only the engine origin may ever
///    load in-app, so an attacker-controlled monitor page cannot use the setting
///    to load a foreign origin in a Tauri window.
///
/// Only once the target is confirmed to be `engine.tfd.rocks` over http(s) is
/// the stored `LinkTarget` read and the Browser/Window/SameWindow branch taken.
fn dispatch_external(app: &AppHandle, raw: &str) {
    use tauri_plugin_opener::OpenerExt;

    let Ok(parsed) = tauri::Url::parse(raw) else {
        log::warn!("dispatch_external: unparseable target URL, ignoring");
        return;
    };
    if !matches!(parsed.scheme(), "http" | "https") {
        log::warn!(
            "dispatch_external: non-http(s) scheme '{}', ignoring",
            parsed.scheme()
        );
        return;
    }
    if parsed.host_str() != Some("engine.tfd.rocks") {
        // Foreign host: ALWAYS the system browser, setting or not.
        let _ = app.opener().open_url(parsed.as_str(), None::<&str>);
        return;
    }

    match link_target::read_link_target(app) {
        link_target::LinkTarget::Browser => {
            let _ = app.opener().open_url(parsed.as_str(), None::<&str>);
        }
        link_target::LinkTarget::Window => open_profile_window(app, parsed.as_str()),
        link_target::LinkTarget::SameWindow => navigate_main_in_place(app, parsed.as_str()),
    }
}

/// Same-window link target: navigate the MAIN window's webview in place to the
/// (already scheme+host validated) engine `url`. Uses `location.assign` via
/// `eval` rather than a bare load so it pushes a real session-history entry —
/// the injected title-bar Back/Forward + "Battle Monitor" controls then behave
/// like a browser. The live monitor is replaced by the profile and reloads when
/// the user navigates back: the accepted trade for staying in one window without
/// the (Windows/WebView2-unstable) multi-webview path.
fn navigate_main_in_place(app: &AppHandle, url: &str) {
    let Some(win) = app.get_webview_window("main") else {
        log::error!("same-window navigate: main window missing");
        return;
    };
    // JSON-encode the URL so it is a safe JS string literal inside location.assign().
    let script = format!(
        "location.assign({});",
        serde_json::to_string(url).unwrap_or_else(|_| "'/monitor'".to_string())
    );
    if let Err(e) = win.eval(&script) {
        log::error!("same-window navigate failed: {e}");
    }
}

/// Open an `engine.tfd.rocks` profile link in a NEW frameless top-level in-app
/// window. The caller (`dispatch_external`) has already validated scheme + host,
/// so `url` is a trusted engine http(s) URL.
///
/// The actual `WebviewWindowBuilder::build` is deferred onto
/// `async_runtime::spawn`: on Windows it deadlocks when called from a
/// synchronous Tauri command / event handler (Webview2 / wry #583), and
/// `on_navigation` — where this is reached from — is synchronous.
///
/// The window is labelled `profile-<n>` so `capabilities/profile.json` grants it
/// window-controls-only perms (drag / minimize / close) and nothing else; it is
/// frameless (`decorations(false)`) to match the app, and the app-global
/// `monitor-embed` init script renders a Back/min/close bar on it (the bar
/// branches on the `profile-` label).
///
/// SECURITY — an `on_navigation` host gate pins the window to
/// `engine.tfd.rocks` for its whole lifetime, so a later same-window
/// navigation (link click / `location` set / redirect) cannot load a foreign
/// origin in this window.
pub(crate) fn open_profile_window(app: &AppHandle, url: &str) {
    let app = app.clone();
    let url = url.to_string();
    tauri::async_runtime::spawn(async move {
        let Ok(parsed) = tauri::Url::parse(&url) else {
            log::error!("open_profile_window: target URL no longer parses: {url}");
            return;
        };
        let n = PROFILE_SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let label = format!("{PROFILE_LABEL_PREFIX}{n}");
        match tauri::WebviewWindowBuilder::new(&app, &label, tauri::WebviewUrl::External(parsed))
            .title("TFD Bridge — Profile")
            .inner_size(900.0, 720.0)
            .min_inner_size(480.0, 400.0)
            .decorations(false)
            .focused(true)
            // SECURITY — origin pin. The
            // INITIAL load is already engine-only (dispatch_external host-gates
            // before reaching here), but without this hook a SUBSEQUENT
            // same-window navigation — a plain `<a>` click, a JS `location`
            // assignment, or a server redirect — could load a foreign origin
            // in-app. Returning `false` cancels any navigation off
            // engine.tfd.rocks, so it is IMPOSSIBLE for a non-engine origin to
            // load in this window. (`target=_blank` links still route through
            // MONITOR_EMBED_JS → SENTINEL_EXTERNAL → system browser.)
            .on_navigation(|u| u.host_str() == Some("engine.tfd.rocks"))
            .build()
        {
            Ok(_win) => log::info!("Opened profile window {label}"),
            Err(e) => log::error!("open_profile_window failed: {e}"),
        }
    });
}

/// Label for the disabled tray version item, e.g. "TFD Bridge v0.2.4".
/// The version comes from the bundle version (`tauri.conf.json`) via
/// `app.package_info().version` — never a hardcoded literal.
fn tray_version_label(version: impl std::fmt::Display) -> String {
    format!("TFD Bridge v{version}")
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
        .plugin(
            // Persist SIZE + POSITION + MAXIMIZED only.
            // VISIBLE is intentionally excluded: show/hide is managed by the
            // tray and the --autostart path; restoring VISIBLE would fight that.
            tauri_plugin_window_state::Builder::new()
                .with_state_flags(StateFlags::SIZE | StateFlags::POSITION | StateFlags::MAXIMIZED)
                // Don't persist geometry for the ephemeral profile-link windows
                // (label `profile-<n>`, the `Window` target): the labels are
                // minted per run and never recur, so a saved entry is dead
                // clutter a later run could even misapply to a different profile
                // window. The main + reusable `tabs` windows still persist.
                .with_filter(|label| !label.starts_with(PROFILE_LABEL_PREFIX))
                .build(),
        )
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
                                if let Some(target) = url
                                    .query_pairs()
                                    .find(|(k, _)| k == "url")
                                    .map(|(_, v)| v.into_owned())
                                {
                                    dispatch_external(webview.app_handle(), &target);
                                }
                                // ALWAYS cancel the sentinel navigation so the live
                                // monitor page stays mounted — for every outcome
                                // (parse failure, bad scheme, foreign host, every
                                // LinkTarget). Returning `true` would let the
                                // http-style sentinel URL navigate the monitor away.
                                return false;
                            }
                            SENTINEL_DASHBOARD => {
                                save_last_view(webview.app_handle(), "dashboard");
                                let app = webview.app_handle().clone();
                                tauri::async_runtime::spawn(async move {
                                    let dash =
                                        app.state::<DashboardUrl>().0.lock().unwrap().clone();
                                    log::info!("back-to-dashboard: target {dash:?}");
                                    match (app.get_webview_window("main"), dash) {
                                        (Some(win), Some(url)) => {
                                            if let Err(e) = win.navigate(url) {
                                                log::error!(
                                                    "back-to-dashboard navigate failed: {e}"
                                                );
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
        .manage(BridgeState(Mutex::new(None)))
        .manage(UploaderState(Mutex::new(None)));

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
                // Only the main window closes-to-tray. Profile windows (label
                // `profile-*`, the New Window link target) must close for REAL —
                // otherwise their bar's ✕ would merely hide them, leaking a
                // hidden engine webview.
                if window.label() != "main" {
                    return;
                }
                // Once Quit has set the flag, do NOT prevent the close: vetoing
                // it here deadlocks `app.exit()` (each window would veto its own
                // close), which is what made the app unquittable. Let it close.
                if window.state::<Quitting>().0.load(Ordering::SeqCst) {
                    return;
                }
                // Otherwise close-to-tray: keep the app alive in the tray so the
                // tray "Open" item can revive the window.
                // Persist geometry now — the plugin's auto-save fires on RunEvent::Exit
                // (real quit), but prevent_close() blocks the window's own close event
                // from reaching the disk-write path, so we must flush explicitly here.
                if let Err(e) = window.app_handle().save_window_state(
                    StateFlags::SIZE | StateFlags::POSITION | StateFlags::MAXIMIZED,
                ) {
                    log::warn!("Failed to save window state on close-to-tray: {e}");
                }
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

            // Disabled (greyed, non-clickable) version label at the top of the
            // menu — an inert build indicator, so it gets no click-handler arm.
            let version = MenuItem::with_id(
                app,
                "version",
                tray_version_label(&app.package_info().version),
                false,
                None::<&str>,
            )?;
            let open = MenuItem::with_id(app, "open", "Open", true, None::<&str>)?;
            let open_monitor = MenuItem::with_id(
                app,
                "open_monitor",
                "Open Battle Monitor",
                true,
                None::<&str>,
            )?;
            let check_updates = MenuItem::with_id(
                app,
                "check_updates",
                "Check for updates now",
                true,
                None::<&str>,
            )?;
            let launch_on_login = CheckMenuItem::with_id(
                app,
                "launch_on_login",
                "Launch on login",
                true,
                launch_on_login_checked,
                None::<&str>,
            )?;
            let quit = MenuItem::with_id(app, "quit", "Quit", true, None::<&str>)?;
            let sep_version = PredefinedMenuItem::separator(app)?;
            let sep = PredefinedMenuItem::separator(app)?;
            let sep2 = PredefinedMenuItem::separator(app)?;
            let menu = Menu::with_items(
                app,
                &[
                    &version,
                    &sep_version,
                    &open,
                    &open_monitor,
                    &sep,
                    &check_updates,
                    &sep2,
                    &launch_on_login,
                    &quit,
                ],
            )?;

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
                    "check_updates" => {
                        let handle = app.clone();
                        tauri::async_runtime::spawn(async move {
                            check_for_update(handle, CheckMode::Manual).await;
                        });
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

            // ── Check for updates + fetch engine config in the background ────
            {
                let handle = app.handle().clone();
                let startup_mode = if autostart_launch {
                    CheckMode::Login
                } else {
                    CheckMode::Hourly
                };
                tauri::async_runtime::spawn(async move {
                    check_for_update(handle.clone(), startup_mode).await;
                    // Engine config follows the update check sequentially so a
                    // possible update dialog never overlaps the 426 nudge.
                    refresh_engine_config(handle).await;
                });
            }

            // ── Hourly background update checks + engine-config refresh ──────
            // First fire is +1h so it doesn't overlap the startup check.
            // Hourly: only surfaces a prompt when an update is actually available.
            // The engine bridge-config refresh piggybacks the same timer, so a
            // server-side feature-flag flip takes effect within the hour.
            {
                let handle = app.handle().clone();
                tauri::async_runtime::spawn(async move {
                    loop {
                        tokio::time::sleep(std::time::Duration::from_secs(3600)).await;
                        check_for_update(handle.clone(), CheckMode::Hourly).await;
                        refresh_engine_config(handle.clone()).await;
                    }
                });
            }

            // ── Replay-donation defaults + cache seed ───────────────────────
            // Fresh-install opt-out default: a brand-new install (onboarding not
            // done, no decision stored) starts with replay donation ON. Existing
            // installs are untouched. Must run BEFORE seeding the cache below.
            commands::seed_fresh_install_donation_default(app.handle());
            // The uploader (td-c8973d) reads donation::consent() without an
            // AppHandle — seed it here so an autostart run where the dashboard
            // never loads still sees the persisted decision; the donation
            // commands keep it in sync from then on.
            donation::set_consent(commands::read_donation_consent(app.handle()));

            // ── Start the bridge if onboarding is complete ──────────────────
            let cfg = commands::read_config(app.handle());
            let onboarding_complete = !cfg.needs_onboarding();
            if let Some(replays_path) = cfg.replays_path {
                apply_replays_path(app.handle(), replays_path);
            }

            // ── Restore last-active view (monitor or dashboard) ─────────────
            // Only restore monitor when onboarding is complete — navigating to
            // the remote monitor before the replays path is configured is wrong.
            // Navigate directly (not via open_monitor_window) so window
            // visibility is not touched; the autostart-hide block already handled that.
            if onboarding_complete && read_last_view(app.handle()).as_deref() == Some("monitor") {
                if let Some(win) = app.get_webview_window("main") {
                    match MONITOR_URL.parse::<tauri::Url>() {
                        Ok(url) => {
                            if let Err(e) = win.navigate(url) {
                                log::error!(
                                    "startup view restore: navigate to monitor failed: {e}"
                                );
                            }
                        }
                        Err(e) => log::error!("startup view restore: bad monitor URL: {e}"),
                    }
                }
            }

            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            commands::get_onboarding_status,
            commands::confirm_replays_path,
            commands::pick_replays_folder,
            commands::set_launch_on_login,
            commands::open_monitor,
            commands::get_donation_status,
            commands::set_donation_consent,
            commands::get_link_target,
            commands::set_link_target,
        ])
        .build(tauri::generate_context!())
        .expect("error while building tauri application")
        .run(|app, event| {
            if let RunEvent::Exit = event {
                if let Some(ab) = app.state::<BridgeState>().0.lock().unwrap().take() {
                    ab.bridge.stop();
                }
                if let Some(up) = app.state::<UploaderState>().0.lock().unwrap().take() {
                    up.cancel();
                }
            }
        });
}

// ── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    /// Verify the pref-parsing logic: None → false (absent key parses to off;
    /// fresh installs get launch-on-login via the pre-checked onboarding option).
    #[test]
    fn launch_on_login_default_is_false() {
        let v: Option<serde_json::Value> = None;
        let result = v.and_then(|v| v.as_bool()).unwrap_or(false);
        assert!(!result, "absent key should parse to false");
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

    // ── last-view store parsing ───────────────────────────────────────────────

    #[test]
    fn last_view_absent_is_none() {
        let v: Option<serde_json::Value> = None;
        let result = v.and_then(|v| v.as_str().map(|s| s.to_owned()));
        assert_eq!(result, None);
    }

    #[test]
    fn last_view_monitor_parses() {
        let v = serde_json::json!("monitor");
        let result = Some(v).and_then(|v| v.as_str().map(|s| s.to_owned()));
        assert_eq!(result.as_deref(), Some("monitor"));
    }

    #[test]
    fn last_view_dashboard_parses() {
        let v = serde_json::json!("dashboard");
        let result = Some(v).and_then(|v| v.as_str().map(|s| s.to_owned()));
        assert_eq!(result.as_deref(), Some("dashboard"));
    }

    // ── tray version label ────────────────────────────────────────────────────

    #[test]
    fn tray_version_label_formats_semver() {
        assert_eq!(tray_version_label("0.2.4"), "TFD Bridge v0.2.4");
    }

    #[test]
    fn tray_version_label_tracks_package_version() {
        // The label must follow whatever the package version is — no drift.
        let label = tray_version_label(env!("CARGO_PKG_VERSION"));
        assert_eq!(label, format!("TFD Bridge v{}", env!("CARGO_PKG_VERSION")));
        assert!(label.starts_with("TFD Bridge v"));
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
        assert_eq!(decide_bridge_action(Some(path), path), BridgeAction::Noop);
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

    // ── bundled-UI CSP (tauri.conf.json + ui/index.html) ─────────────────────
    //
    // The CSP only ships with bundled assets served over the tauri protocol;
    // Tauri appends a nonce for the inline <style> to style-src and a sha256
    // hash for the inline <script> to script-src at serve time. These tests
    // guard the invariants that keep that working.

    const TAURI_CONF: &str = include_str!("../tauri.conf.json");
    const INDEX_HTML: &str = include_str!("../../ui/index.html");

    fn bundled_csp() -> String {
        let conf: serde_json::Value = serde_json::from_str(TAURI_CONF).unwrap();
        conf["app"]["security"]["csp"]
            .as_str()
            .expect("security.csp must be a non-null string")
            .to_owned()
    }

    fn csp_directive(csp: &str, name: &str) -> String {
        csp.split(';')
            .map(str::trim)
            .find(|d| d.starts_with(name))
            .unwrap_or_else(|| panic!("CSP must have a {name} directive"))
            .to_owned()
    }

    #[test]
    fn csp_is_set_and_restrictive() {
        let csp = bundled_csp();
        assert!(csp.contains("default-src 'self'"));
        // Tauri injects the nonces/hashes for the bundled inline <style> and
        // <script> itself — adding 'unsafe-inline' manually would be both
        // pointless (ignored next to a nonce) and a loosening elsewhere.
        assert!(!csp.contains("unsafe-inline"));
        assert!(!csp.contains("unsafe-eval"));
        // Pure local UI: no remote script/style/connect sources.
        assert!(!csp.contains("https://"));
    }

    #[test]
    fn csp_allows_tauri_ipc() {
        // Tauri v2 IPC is a fetch() of ipc://localhost (macOS/Linux) or
        // http://ipc.localhost (Windows); both belong in connect-src.
        let connect = csp_directive(&bundled_csp(), "connect-src");
        assert!(connect.contains(" ipc:"));
        assert!(connect.contains("http://ipc.localhost"));
    }

    #[test]
    fn csp_allows_data_images() {
        // The onboarding checkbox check-mark is a data: SVG CSS background,
        // which is governed by img-src.
        let img = csp_directive(&bundled_csp(), "img-src");
        assert!(img.contains("data:"));
    }

    #[test]
    fn index_html_has_no_inline_style_attributes() {
        // Inline style attributes cannot carry the Tauri-injected style-src
        // nonce, and that nonce makes 'unsafe-inline' ineffective — under the
        // CSP they would be silently dropped. Static styling must live in the
        // <style> block (CSSOM toggling from JS is unaffected).
        assert!(
            !INDEX_HTML.contains("style=\""),
            "ui/index.html must not use inline style attributes (CSP-blocked)"
        );
    }
}
