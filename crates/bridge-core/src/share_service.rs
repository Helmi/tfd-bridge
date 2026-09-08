use crate::render_jobs::RenderJobs;
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
}
pub type ReplayResolver = Box<dyn FnOnce() -> Result<String, String> + Send>;
pub trait ShareService: Send + Sync {
    fn open_authorization(&self, _url: &str) -> bool {
        false
    }
    fn start(
        &self,
        claim: ShareClaim,
        resolve: ReplayResolver,
        port: u16,
        jobs: Arc<Mutex<RenderJobs>>,
    ) -> Result<String, String>;
    fn status(&self, id: &str) -> Option<serde_json::Value>;
}
