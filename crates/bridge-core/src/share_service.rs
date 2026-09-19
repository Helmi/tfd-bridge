use crate::{render_jobs::RenderJobs, replay_identity::ReplayEvidence};
use std::sync::{Arc, Mutex};

/// Native-only handoff. Never derive Debug/Serialize for capability-bearing data.
#[derive(serde::Deserialize)]
pub struct ShareClaim {
    pub state: Option<String>,
    pub replay_name: Option<String>,
    pub arena_unique_id: String,
    pub share_id: String,
    pub capability: String,
    pub capability_expires_at: Option<String>,
    pub max_bytes: u64,
    /// Opaque, short-lived local binding. It is never an Engine authority.
    pub preflight_id: Option<String>,
}

/// Safe local response for the Engine page. Account IDs remain decimal strings
/// so JavaScript cannot round a WG u64. The artifact hash and path stay native.
#[derive(serde::Serialize)]
pub struct SharePreflight {
    pub preflight_id: String,
    pub recorder_account_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub recorder_realm: Option<String>,
}

pub type ReplayResolver = Box<dyn FnOnce() -> Result<String, String> + Send>;
pub trait ShareService: Send + Sync {
    fn open_authorization(&self, _url: &str) -> bool {
        false
    }
    /// Bind evidence from one canonical local artifact to the current account.
    fn preflight(
        &self,
        replay_name: String,
        arena_unique_id: String,
        evidence: ReplayEvidence,
    ) -> Result<SharePreflight, String>;
    fn start(
        &self,
        claim: ShareClaim,
        resolve: ReplayResolver,
        port: u16,
        jobs: Arc<Mutex<RenderJobs>>,
    ) -> Result<String, String>;
    fn status(&self, id: &str) -> Option<serde_json::Value>;
}
