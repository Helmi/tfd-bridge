/// App configuration — persisted to JSON in the OS app-data directory.
///
/// The Tauri layer owns reading/writing the file (via tauri-plugin-store);
/// this module defines the schema and helpers so the bridge-core crate
/// can reason about config values without touching Tauri APIs.
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// Persisted application configuration.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct AppConfig {
    /// The confirmed replays folder path.  `None` until the user completes
    /// first-start onboarding.
    pub replays_path: Option<PathBuf>,
    /// Whether first-start onboarding has been completed.
    pub onboarding_done: bool,
}

impl AppConfig {
    pub fn new() -> Self {
        Self::default()
    }

    /// Return true if the app still needs to run first-start onboarding.
    pub fn needs_onboarding(&self) -> bool {
        !self.onboarding_done || self.replays_path.is_none()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_needs_onboarding() {
        let cfg = AppConfig::new();
        assert!(cfg.needs_onboarding());
    }

    #[test]
    fn completed_onboarding_does_not_need_it() {
        let cfg = AppConfig {
            replays_path: Some(PathBuf::from("/some/replays")),
            onboarding_done: true,
        };
        assert!(!cfg.needs_onboarding());
    }

    #[test]
    fn path_set_but_flag_false_still_needs_onboarding() {
        let cfg = AppConfig {
            replays_path: Some(PathBuf::from("/some/replays")),
            onboarding_done: false,
        };
        assert!(cfg.needs_onboarding());
    }

    #[test]
    fn roundtrip_json() {
        let cfg = AppConfig {
            replays_path: Some(PathBuf::from("/game/replays")),
            onboarding_done: true,
        };
        let json = serde_json::to_string(&cfg).unwrap();
        let restored: AppConfig = serde_json::from_str(&json).unwrap();
        assert_eq!(restored.replays_path, cfg.replays_path);
        assert_eq!(restored.onboarding_done, cfg.onboarding_done);
    }
}
