//! In-window profile tabs (Experimental — the `Tab` value of `LinkTarget`).
//!
//! Goal: keep the live Battle Monitor mounted in the MAIN window while a
//! player-profile link opens "beside" it as a tab — so the monitor never
//! reloads. The implementation hosts a single reusable `tabs` window that
//! contains:
//!
//! - one LOCAL chrome webview (`tabbar`, the bundled `ui/tabbar.html`) pinned at
//!   the top, ~74 px tall (a 40 px drag/title bar + a 34 px tab strip), and
//! - one REMOTE content webview per open profile (`profile-tab-<n>` on
//!   `engine.tfd.rocks`), of which only the active one is shown.
//!
//! Layout is driven manually (NOT `auto_resize`, which would make a webview
//! track the WHOLE window and cover the chrome): on every
//! `WindowEvent::Resized` the global `on_window_event` hook in `lib.rs` calls
//! [`reflow`], which re-applies position/size to the chrome and the content
//! webviews.
//!
//! Multi-webview is gated on the tauri `unstable` Cargo feature
//! (`Window::add_child` / `WebviewBuilder` / `Manager::get_webview`); the crate
//! enables it (see `Cargo.toml`).
//!
//! SECURITY: every entry point here is only ever reached for an
//! already-validated `https`/`http` `engine.tfd.rocks` URL — the host gate lives
//! in `lib.rs::dispatch_external`. Nothing here re-opens its own arbitrary URLs:
//! [`open_profile_tab`] takes the trusted engine URL the dispatcher passed, and
//! the content webview's `on_navigation` keeps the page on `engine.tfd.rocks`
//! (off-origin clicks are routed through the same `SENTINEL_EXTERNAL` → browser
//! path as the embedded monitor, because the content webview shares
//! `MONITOR_EMBED_JS`). The remote content webviews get ONLY window-controls IPC
//! via `capabilities/profile.json` (label glob `profile-tab-*`); the local
//! chrome gets the tab IPC via `capabilities/tabs.json`.

use std::sync::Mutex;

use serde::Serialize;
use tauri::{
    AppHandle, Emitter, LogicalPosition, LogicalSize, Manager, WebviewBuilder, WebviewUrl, Window,
    WindowBuilder,
};

// ── Constants ─────────────────────────────────────────────────────────────────

/// Label of the single reusable tabs container window.
const TABS_WINDOW: &str = "tabs";
/// Label of the local chrome webview inside the tabs window.
const CHROME_LABEL: &str = "tabbar";
/// Label prefix for remote content webviews. MUST match the `profile-tab-*`
/// glob in `capabilities/profile.json` (which grants window-controls-only IPC to
/// these remote engine pages) and the `profile-tab-` branch of the injected bar
/// in `MONITOR_EMBED_JS` (so the content page shows a Back/min/close bar).
const CONTENT_LABEL_PREFIX: &str = "profile-tab-";
/// Combined height of the chrome (40 px topbar + 34 px tab strip). The content
/// webviews start at `y = CHROME_H` so they never cover the tab strip.
const CHROME_H: f64 = 74.0;
/// Event emitted to the chrome after every registry mutation. `tabbar.html`
/// re-renders the tab strip on receipt (it then calls `tab_list` for the data).
const TABS_CHANGED: &str = "tabs-changed";
/// Initial / minimum size of the tabs window (logical px).
const TABS_W: f64 = 1100.0;
const TABS_H: f64 = 760.0;
const TABS_MIN_W: f64 = 640.0;
const TABS_MIN_H: f64 = 480.0;

// ── Wire types ────────────────────────────────────────────────────────────────

/// One open profile tab, as surfaced to the local tab-bar chrome.
/// camelCase to match the rest of the IPC surface (Tauri v2 does not
/// auto-camelCase return values).
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TabInfo {
    /// The content webview's label (`profile-tab-<n>`).
    pub label: String,
    /// Display title for the tab strip (derived from the URL path for now).
    pub title: String,
    /// The engine URL the tab is showing.
    pub url: String,
    /// Whether this tab is the currently-visible one.
    pub active: bool,
}

// ── Registry (Tauri managed state) ────────────────────────────────────────────

/// One open tab's bookkeeping. The webview itself lives in the window; this is
/// just the metadata the chrome needs plus the label used to find the webview.
struct TabEntry {
    /// The content webview's label (`profile-tab-<n>`).
    label: String,
    /// Display title for the tab strip.
    title: String,
    /// The engine URL the tab is showing.
    url: String,
}

/// The tab registry: the open tabs (in strip order) and which one is active.
#[derive(Default)]
pub struct TabsReg {
    /// Monotonic label counter — values are never reused within a run, so a
    /// plain incrementing sequence is enough for uniqueness.
    seq: u64,
    /// Open tabs in tab-strip order.
    tabs: Vec<TabEntry>,
    /// Label of the active (visible) tab, if any.
    active: Option<String>,
}

impl TabsReg {
    /// Snapshot the registry for the chrome (`tab_list`).
    fn snapshot(&self) -> Vec<TabInfo> {
        self.tabs
            .iter()
            .map(|t| TabInfo {
                label: t.label.clone(),
                title: t.title.clone(),
                url: t.url.clone(),
                active: self.active.as_deref() == Some(t.label.as_str()),
            })
            .collect()
    }
}

/// Managed state wrapper around the registry.
#[derive(Default)]
pub struct TabsState(pub Mutex<TabsReg>);

// ── Pure helpers (unit-tested) ────────────────────────────────────────────────

/// Mint the next unique content-webview label, advancing the sequence.
fn mint_label(seq: &mut u64) -> String {
    *seq += 1;
    format!("{CONTENT_LABEL_PREFIX}{seq}")
}

/// Content rect below the chrome, in LOGICAL px. Pure → unit-tested.
///
/// The content webview starts at `y = CHROME_H` and fills the remaining height
/// (clamped at 0 so a window shorter than the chrome never yields a negative
/// size). Mixing physical and logical units is the classic multi-webview bug, so
/// callers MUST convert `inner_size()` (physical) via `to_logical(scale_factor)`
/// before calling this.
fn content_rect(win_w: f64, win_h: f64) -> (LogicalPosition<f64>, LogicalSize<f64>) {
    (
        LogicalPosition::new(0.0, CHROME_H),
        LogicalSize::new(win_w.max(0.0), (win_h - CHROME_H).max(0.0)),
    )
}

/// Derive a short, human-ish tab title from an engine profile URL.
///
/// We cannot read the remote page's `document.title` from Rust (the content
/// webview deliberately has no IPC to call back with), so for v1 we derive the
/// label from the last non-empty path segment (URL-decoded), falling back to
/// `"Profile"`. Pure → unit-tested.
fn title_from_url(url: &str) -> String {
    let parsed = match tauri::Url::parse(url) {
        Ok(u) => u,
        Err(_) => return "Profile".to_string(),
    };
    let last = parsed
        .path_segments()
        .and_then(|mut segs| segs.rfind(|s| !s.is_empty()))
        .map(|s| s.to_string());
    match last {
        Some(seg) => {
            // Percent-decode for display; fall back to the raw segment.
            let decoded = percent_decode(&seg);
            if decoded.is_empty() {
                "Profile".to_string()
            } else {
                decoded
            }
        }
        None => "Profile".to_string(),
    }
}

/// Minimal percent-decoder for display titles (no extra dependency). Decodes
/// `%XX` byte escapes and `+` → space; invalid escapes are left verbatim. Pure.
fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out: Vec<u8> = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'%' if i + 2 < bytes.len() => {
                let hi = (bytes[i + 1] as char).to_digit(16);
                let lo = (bytes[i + 2] as char).to_digit(16);
                if let (Some(h), Some(l)) = (hi, lo) {
                    out.push((h * 16 + l) as u8);
                    i += 3;
                } else {
                    out.push(b'%');
                    i += 1;
                }
            }
            b'+' => {
                out.push(b' ');
                i += 1;
            }
            b => {
                out.push(b);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

// ── Window + chrome lifecycle ─────────────────────────────────────────────────

/// Ensure the reusable `tabs` window exists, with its local chrome webview, and
/// return it. Idempotent: if the window already exists it is returned as-is
/// (its chrome and any open content webviews are preserved).
///
/// The window is frameless (`decorations(false)`) to match the app; the chrome
/// webview supplies the topbar (drag region + window controls) and the tab
/// strip. The chrome is added FIRST so that — on Windows/WebView2, where
/// later-added child webviews stack ON TOP — it is never covered by a content
/// webview.
fn ensure_tabs_window(app: &AppHandle) -> tauri::Result<Window> {
    if let Some(w) = app.get_window(TABS_WINDOW) {
        return Ok(w);
    }

    let win = WindowBuilder::new(app, TABS_WINDOW)
        .title("TFD Bridge — Profiles")
        .inner_size(TABS_W, TABS_H)
        .min_inner_size(TABS_MIN_W, TABS_MIN_H)
        .decorations(false)
        .build()?;

    let size = logical_inner_size(&win).unwrap_or(LogicalSize::new(TABS_W, TABS_H));

    // Chrome pinned at the top, full width, fixed height. Added FIRST so it
    // stays beneath nothing (content webviews added later stack on top, but
    // they start at y = CHROME_H so they never overlap the chrome band).
    win.add_child(
        WebviewBuilder::new(CHROME_LABEL, WebviewUrl::App("tabbar.html".into())),
        LogicalPosition::new(0.0, 0.0),
        LogicalSize::new(size.width, CHROME_H),
    )?;

    Ok(win)
}

/// Read the window's inner size as LOGICAL pixels (physical → logical via the
/// scale factor). Centralised so every caller handles HiDPI consistently.
fn logical_inner_size(win: &Window) -> tauri::Result<LogicalSize<f64>> {
    let scale = win.scale_factor()?;
    Ok(win.inner_size()?.to_logical::<f64>(scale))
}

// ── Entry point (called from the dispatch in lib.rs) ──────────────────────────

/// Open an already-validated `engine.tfd.rocks` profile link as an in-window
/// tab. The caller (`lib.rs::dispatch_external`) has confirmed scheme + host, so
/// `url` is a trusted engine http(s) URL.
///
/// Ensures the tabs window + chrome, adds a remote content webview beside the
/// chrome, activates it (shows it, hides the others), shows/focuses the window,
/// and emits `tabs-changed` so the chrome re-renders the strip.
pub fn open_profile_tab(app: &AppHandle, url: &str) {
    // Creation MUST be deferred off the synchronous on_navigation handler:
    // building a window/webview inline there deadlocks WebView2 on Windows
    // (wry #583) — the observed symptom was "opens a window then hangs". This
    // mirrors open_profile_window, which already hops onto the async runtime.
    let app = app.clone();
    let url = url.to_string();
    tauri::async_runtime::spawn(async move {
        open_profile_tab_inner(&app, &url);
    });
}

fn open_profile_tab_inner(app: &AppHandle, url: &str) {
    let parsed = match tauri::Url::parse(url) {
        Ok(u) => u,
        Err(e) => {
            log::error!("open_profile_tab: target URL no longer parses ({e}): {url}");
            return;
        }
    };
    // Defence in depth: the dispatcher already gated host == engine.tfd.rocks,
    // but this module's contract is "engine only", so re-assert it here. An
    // unexpected host means a bug upstream — fail closed (do nothing) rather
    // than load a foreign origin into a Tauri webview.
    if parsed.host_str() != Some("engine.tfd.rocks") {
        log::error!("open_profile_tab: refusing non-engine host {:?}", parsed.host_str());
        return;
    }

    let win = match ensure_tabs_window(app) {
        Ok(w) => w,
        Err(e) => {
            log::error!("open_profile_tab: cannot build tabs window: {e}");
            return;
        }
    };

    let size = logical_inner_size(&win).unwrap_or(LogicalSize::new(TABS_W, TABS_H));
    let (pos, sz) = content_rect(size.width, size.height);

    let state = app.state::<TabsState>();
    let mut reg = state.0.lock().unwrap();
    let label = mint_label(&mut reg.seq);

    // The content webview shares MONITOR_EMBED_JS so off-origin new-tab clicks
    // route through the SENTINEL_EXTERNAL → system-browser path (same as the
    // embedded monitor), and the injected bar renders (the `profile-tab-`
    // branch gives it Back/min/close). on_navigation keeps the page itself on
    // engine.tfd.rocks; the sentinel handling lives in the main window's
    // monitor-embed plugin, so here we only need to pin the origin.
    let builder = WebviewBuilder::new(&label, WebviewUrl::External(parsed.clone()))
        .initialization_script(crate::MONITOR_EMBED_JS)
        .on_navigation(|u| u.host_str() == Some("engine.tfd.rocks"));

    match win.add_child(builder, pos, sz) {
        Ok(_wv) => {
            reg.tabs.push(TabEntry {
                label: label.clone(),
                title: title_from_url(url),
                url: url.to_string(),
            });
            set_active(&win, &mut reg, &label);
        }
        Err(e) => {
            log::error!("open_profile_tab: add content webview failed: {e}");
            return;
        }
    }
    drop(reg);

    let _ = win.show();
    let _ = win.set_focus();
    emit_changed(app);
}

// ── Activation / reflow ───────────────────────────────────────────────────────

/// Make `label` the active tab: show its content webview, hide every other, and
/// record it as active. Caller holds the registry lock.
fn set_active(win: &Window, reg: &mut TabsReg, label: &str) {
    for t in &reg.tabs {
        if let Some(wv) = win.get_webview(&t.label) {
            if t.label == label {
                let _ = wv.show();
                let _ = wv.set_focus();
            } else {
                // On Windows, hide() is required — moving a webview offscreen is
                // not a reliable way to keep it from covering the active one.
                let _ = wv.hide();
            }
        }
    }
    reg.active = Some(label.to_string());
}

/// Re-apply layout to the chrome and every content webview. Called from the
/// global `on_window_event` `Resized` branch in `lib.rs` (filtered to the
/// `tabs` window). No-ops cleanly if the window or a webview has gone away.
pub fn reflow(win: &Window) {
    let Ok(size) = logical_inner_size(win) else {
        return;
    };

    if let Some(chrome) = win.get_webview(CHROME_LABEL) {
        let _ = chrome.set_position(LogicalPosition::new(0.0, 0.0));
        let _ = chrome.set_size(LogicalSize::new(size.width, CHROME_H));
    }

    let (pos, sz) = content_rect(size.width, size.height);
    let state = win.app_handle().state::<TabsState>();
    let reg = state.0.lock().unwrap();
    for t in &reg.tabs {
        if let Some(wv) = win.get_webview(&t.label) {
            let _ = wv.set_position(pos);
            let _ = wv.set_size(sz);
        }
    }
}

/// Clear the registry after the tabs window is destroyed (called from the
/// `lib.rs` window-event hook). The window's webviews are torn down by Tauri; we
/// only drop our bookkeeping so the next `open_profile_tab` rebuilds cleanly and
/// the sequence counter restarts implicitly (labels stay unique per window
/// instance because the window — and thus all `profile-tab-*` webviews — is gone).
pub fn clear_registry(app: &AppHandle) {
    let state = app.state::<TabsState>();
    let mut reg = state.0.lock().unwrap();
    *reg = TabsReg::default();
}

/// Emit the `tabs-changed` event so the chrome re-renders.
fn emit_changed(app: &AppHandle) {
    if let Err(e) = app.emit(TABS_CHANGED, ()) {
        log::warn!("Failed to emit {TABS_CHANGED}: {e}");
    }
}

// ── IPC commands the tab-bar chrome calls ─────────────────────────────────────
//
// Registered in `lib.rs` `generate_handler!` as `tabs::<name>`. They take only
// an opaque label / no args and act SOLELY on the tabs window's own webviews —
// none of them can load an arbitrary URL (`open_profile_tab` is the only URL
// entry point and it is host-gated). The local chrome holds the only IPC surface
// (these commands + window controls via `capabilities/tabs.json`).

/// Snapshot the open tabs for the tab-bar chrome.
#[tauri::command]
pub fn tab_list(app: AppHandle) -> Vec<TabInfo> {
    let state = app.state::<TabsState>();
    let reg = state.0.lock().unwrap();
    reg.snapshot()
}

/// Activate the tab with `label`, hiding the others.
#[tauri::command]
pub fn tab_switch(app: AppHandle, label: String) -> Result<(), String> {
    let Some(win) = app.get_window(TABS_WINDOW) else {
        return Err("tabs window is not open".into());
    };
    {
        let state = app.state::<TabsState>();
        let mut reg = state.0.lock().unwrap();
        // Ignore unknown labels rather than activating a non-existent tab.
        if !reg.tabs.iter().any(|t| t.label == label) {
            return Err(format!("unknown tab: {label}"));
        }
        set_active(&win, &mut reg, &label);
    }
    emit_changed(&app);
    Ok(())
}

/// Close the tab with `label`. If it was active, the neighbour to its left (or
/// the new last tab) becomes active. Closing the last tab closes the tabs
/// window and clears the registry.
#[tauri::command]
pub fn tab_close(app: AppHandle, label: String) -> Result<(), String> {
    let Some(win) = app.get_window(TABS_WINDOW) else {
        return Err("tabs window is not open".into());
    };

    let close_window = {
        let state = app.state::<TabsState>();
        let mut reg = state.0.lock().unwrap();

        let Some(idx) = reg.tabs.iter().position(|t| t.label == label) else {
            return Err(format!("unknown tab: {label}"));
        };

        // Close the actual webview first; then drop the entry.
        if let Some(wv) = win.get_webview(&label) {
            let _ = wv.close();
        }
        reg.tabs.remove(idx);

        if reg.tabs.is_empty() {
            reg.active = None;
            true
        } else {
            // If we closed the active tab, pick the neighbour at the same index
            // (now the old right neighbour), or the new last tab.
            let was_active = reg.active.as_deref() == Some(label.as_str());
            if was_active {
                let next = idx.min(reg.tabs.len() - 1);
                let next_label = reg.tabs[next].label.clone();
                set_active(&win, &mut reg, &next_label);
            }
            false
        }
    };

    if close_window {
        // Truly close the window (recommended lifecycle): the chrome webview is
        // destroyed and `ensure_tabs_window` rebuilds it on the next open. The
        // window's CloseRequested/Destroyed hook in lib.rs clears the registry.
        let _ = win.close();
    }
    emit_changed(&app);
    Ok(())
}

/// Open a fresh tab on the monitor root. Convenience for a chrome "+" button.
#[tauri::command]
pub fn tab_new(app: AppHandle) -> Result<(), String> {
    open_profile_tab(&app, crate::MONITOR_URL);
    Ok(())
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mint_label_is_sequential_and_unique() {
        let mut seq = 0u64;
        assert_eq!(mint_label(&mut seq), "profile-tab-1");
        assert_eq!(mint_label(&mut seq), "profile-tab-2");
        assert_eq!(mint_label(&mut seq), "profile-tab-3");
        assert_eq!(seq, 3);
    }

    #[test]
    fn mint_label_uses_the_profile_tab_prefix() {
        // The label MUST carry the prefix the remote capability globs on
        // (capabilities/profile.json: "profile-tab-*"); otherwise the remote
        // page would get NO window-controls IPC.
        let mut seq = 0u64;
        assert!(mint_label(&mut seq).starts_with(CONTENT_LABEL_PREFIX));
        assert!(CONTENT_LABEL_PREFIX.starts_with("profile-tab-"));
    }

    #[test]
    fn content_rect_starts_below_the_chrome() {
        let (pos, sz) = content_rect(1000.0, 700.0);
        assert_eq!(pos.x, 0.0);
        assert_eq!(pos.y, CHROME_H, "content must start below the 74px chrome");
        assert_eq!(sz.width, 1000.0);
        assert_eq!(sz.height, 700.0 - CHROME_H);
    }

    #[test]
    fn content_rect_never_goes_negative() {
        // A window shorter than the chrome must clamp height (and width) at 0,
        // never produce a negative size (which the platform would reject).
        let (_pos, sz) = content_rect(-5.0, 10.0);
        assert_eq!(sz.width, 0.0);
        assert_eq!(sz.height, 0.0);
    }

    #[test]
    fn title_from_url_uses_last_path_segment() {
        assert_eq!(
            title_from_url("https://engine.tfd.rocks/player/Helmi"),
            "Helmi"
        );
        assert_eq!(
            title_from_url("https://engine.tfd.rocks/monitor/"),
            "monitor"
        );
    }

    #[test]
    fn title_from_url_decodes_percent_escapes() {
        assert_eq!(
            title_from_url("https://engine.tfd.rocks/player/Hello%20World"),
            "Hello World"
        );
    }

    #[test]
    fn title_from_url_falls_back_to_profile() {
        assert_eq!(title_from_url("https://engine.tfd.rocks"), "Profile");
        assert_eq!(title_from_url("https://engine.tfd.rocks/"), "Profile");
        assert_eq!(title_from_url("not a url"), "Profile");
    }

    #[test]
    fn percent_decode_handles_plus_and_invalid_escapes() {
        assert_eq!(percent_decode("a+b"), "a b");
        assert_eq!(percent_decode("%2F"), "/");
        // Invalid escape is left verbatim.
        assert_eq!(percent_decode("%ZZ"), "%ZZ");
        assert_eq!(percent_decode("100%"), "100%");
    }

    /// Guard the TabInfo wire contract: camelCase field names (Tauri v2 does not
    /// auto-camelCase return values; the UI reads `label`/`title`/`url`/`active`).
    #[test]
    fn tab_info_serialises_camel_case() {
        let info = TabInfo {
            label: "profile-tab-1".into(),
            title: "Helmi".into(),
            url: "https://engine.tfd.rocks/player/Helmi".into(),
            active: true,
        };
        let v = serde_json::to_value(&info).expect("serialisation failed");
        assert_eq!(v.get("label").and_then(|x| x.as_str()), Some("profile-tab-1"));
        assert_eq!(v.get("title").and_then(|x| x.as_str()), Some("Helmi"));
        assert_eq!(v.get("active").and_then(|x| x.as_bool()), Some(true));
    }

    /// The registry snapshot marks exactly the active tab.
    #[test]
    fn snapshot_marks_active_tab() {
        let mut reg = TabsReg::default();
        for url in ["https://engine.tfd.rocks/a", "https://engine.tfd.rocks/b"] {
            let label = mint_label(&mut reg.seq);
            reg.tabs.push(TabEntry {
                title: title_from_url(url),
                url: url.to_string(),
                label,
            });
        }
        reg.active = Some("profile-tab-2".into());
        let snap = reg.snapshot();
        assert_eq!(snap.len(), 2);
        assert!(!snap[0].active);
        assert!(snap[1].active);
        assert_eq!(snap[1].title, "b");
    }
}
