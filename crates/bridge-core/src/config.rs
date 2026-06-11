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
    /// Replay-finalized watermark: the highest replay mtime (ms since epoch)
    /// a finalized event has been emitted for.  The startup catch-up scan only
    /// emits for files newer than this, so events never repeat across restarts.
    /// `None` until the first bridge start seeds it.
    #[serde(default)]
    pub replay_watermark_ms: Option<u64>,
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
            replay_watermark_ms: None,
        };
        assert!(!cfg.needs_onboarding());
    }

    #[test]
    fn path_set_but_flag_false_still_needs_onboarding() {
        let cfg = AppConfig {
            replays_path: Some(PathBuf::from("/some/replays")),
            onboarding_done: false,
            replay_watermark_ms: None,
        };
        assert!(cfg.needs_onboarding());
    }

    #[test]
    fn roundtrip_json() {
        let cfg = AppConfig {
            replays_path: Some(PathBuf::from("/game/replays")),
            onboarding_done: true,
            replay_watermark_ms: Some(1_770_000_000_000),
        };
        let json = serde_json::to_string(&cfg).unwrap();
        let restored: AppConfig = serde_json::from_str(&json).unwrap();
        assert_eq!(restored.replays_path, cfg.replays_path);
        assert_eq!(restored.onboarding_done, cfg.onboarding_done);
        assert_eq!(restored.replay_watermark_ms, cfg.replay_watermark_ms);
    }

    /// The watermark serialises under the camelCase key the store uses.
    #[test]
    fn watermark_serialises_camel_case() {
        let cfg = AppConfig {
            replays_path: None,
            onboarding_done: false,
            replay_watermark_ms: Some(42),
        };
        let v = serde_json::to_value(&cfg).unwrap();
        assert_eq!(v["replayWatermarkMs"], 42);
        assert!(v.get("replay_watermark_ms").is_none());
    }

    /// Configs persisted before the watermark existed must still deserialise
    /// (missing key → None), so updating the app never breaks the stored config.
    #[test]
    fn missing_watermark_field_deserialises_to_none() {
        let json = r#"{"replaysPath":"/game/replays","onboardingDone":true}"#;
        let cfg: AppConfig = serde_json::from_str(json).unwrap();
        assert_eq!(cfg.replay_watermark_ms, None);
    }
}
