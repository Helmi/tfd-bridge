//! Profile-link target: where a Battle-Monitor "open player profile" link goes.
//!
//! This is a store-only preference (key `linkTarget` in `config.json`), persisted
//! and read exactly the way the donation consent is — `commands.rs` exposes the
//! thin IPC surface, the store IO lives here. There is intentionally NO field on
//! `bridge_core::AppConfig`: like the `lastView` pref, this never enters the typed
//! config struct (which would force a new field + a camelCase serde-contract test).
//!
//! The decision of what to DO with a link stays entirely in Rust (the
//! `SENTINEL_EXTERNAL` arm of the `monitor-embed` `on_navigation` hook reads this
//! pref) — the injected JS needs no IPC to make the choice.
//!
//! The three states map to the three behaviours:
//! - `Browser`    — open the link in the system browser (the default; the
//!   behaviour every existing install has had, so an absent or garbled value
//!   MUST read as `Browser`).
//! - `Window`     — open the engine link in a new in-app top-level window.
//! - `SameWindow` — navigate the main window in place to the engine link; the
//!   title bar gains a "Battle Monitor" return + history back/forward (the live
//!   monitor reloads when you go back).
//!
//! SECURITY: this pref is only ever consulted AFTER the caller has confirmed the
//! target is `https`/`http` on `engine.tfd.rocks`. Any other host always goes to
//! the system browser regardless of the setting — see `lib.rs::dispatch_external`.

use serde::{Deserialize, Serialize};
use tauri::AppHandle;
use tauri_plugin_store::StoreExt;

const STORE_FILE: &str = "config.json";
const KEY_LINK_TARGET: &str = "linkTarget";

/// Where a Battle-Monitor profile link opens. The wire AND store representation
/// is the snake_case string (`"browser"` / `"window"` / `"same_window"`);
/// anything unrecognised reads as `Browser` — the safe, no-behaviour-change
/// default. An unknown value must never silently load a page in-app.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LinkTarget {
    /// Open in the system browser (default).
    #[default]
    Browser,
    /// Open the engine link in a new in-app top-level window.
    Window,
    /// Navigate the main window in place to the engine link. The injected title
    /// bar then shows a "Battle Monitor" return + history Back/Forward; the live
    /// monitor reloads when the user navigates back to it.
    SameWindow,
}

impl LinkTarget {
    /// Parse the persisted store value. An absent key and an unrecognised value
    /// both read as `Browser` (conservative: never silently load in-app).
    pub fn from_store_value(value: Option<&serde_json::Value>) -> Self {
        value
            .and_then(|v| serde_json::from_value(v.clone()).ok())
            .unwrap_or(LinkTarget::Browser)
    }
}

// ── Store helpers ─────────────────────────────────────────────────────────────

/// Read the persisted profile-link target. Absent or garbled values read as
/// `Browser` (see `LinkTarget::from_store_value`).
pub fn read_link_target(app: &AppHandle) -> LinkTarget {
    let Ok(store) = app.store(STORE_FILE) else {
        return LinkTarget::Browser;
    };
    LinkTarget::from_store_value(store.get(KEY_LINK_TARGET).as_ref())
}

/// Persist the profile-link target.
pub fn save_link_target(app: &AppHandle, target: LinkTarget) {
    let Ok(store) = app.store(STORE_FILE) else {
        log::error!("Failed to open store when saving link-target pref");
        return;
    };
    store.set(
        KEY_LINK_TARGET,
        serde_json::to_value(target).unwrap_or(serde_json::Value::Null),
    );
    if let Err(e) = store.save() {
        log::error!("Failed to save link-target pref: {e}");
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_is_browser() {
        assert_eq!(LinkTarget::default(), LinkTarget::Browser);
    }

    /// The store/wire form is the snake_case string the UI switches on.
    #[test]
    fn serialises_snake_case() {
        assert_eq!(
            serde_json::to_value(LinkTarget::Browser).unwrap(),
            serde_json::json!("browser")
        );
        assert_eq!(
            serde_json::to_value(LinkTarget::Window).unwrap(),
            serde_json::json!("window")
        );
        assert_eq!(
            serde_json::to_value(LinkTarget::SameWindow).unwrap(),
            serde_json::json!("same_window")
        );
    }

    /// Persistence round-trip: what the store writes reads back unchanged, for
    /// every value.
    #[test]
    fn store_round_trip_all() {
        for t in [
            LinkTarget::Browser,
            LinkTarget::Window,
            LinkTarget::SameWindow,
        ] {
            let stored = serde_json::to_value(t).unwrap();
            assert_eq!(
                LinkTarget::from_store_value(Some(&stored)),
                t,
                "round trip failed for {t:?}"
            );
        }
    }

    /// An absent store key (fresh install, or an install updated to a build
    /// that introduces the setting) must read as Browser — no behaviour change.
    #[test]
    fn absent_is_browser() {
        assert_eq!(LinkTarget::from_store_value(None), LinkTarget::Browser);
    }

    /// Unrecognised store values must never select an in-app target.
    #[test]
    fn garbled_is_browser() {
        for g in [
            serde_json::json!("WINDOW"),
            serde_json::json!("nonsense"),
            serde_json::json!(true),
            serde_json::json!(1),
            serde_json::json!(null),
            serde_json::json!({ "target": "window" }),
        ] {
            assert_eq!(
                LinkTarget::from_store_value(Some(&g)),
                LinkTarget::Browser,
                "garbage {g} must read as Browser"
            );
        }
    }
}
