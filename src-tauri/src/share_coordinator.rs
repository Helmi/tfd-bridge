//! Owns authorized jobs independently of the Engine tab and Replayer window.
use crate::{
    video_share::{AuthorizedShare, DeliveryState, VideoMetadata, VideoShareClient},
    video_upload::PreparedVideo,
};
use bridge_core::{
    render_jobs::{RenderJobs, RenderRequest, RenderState, StartRender},
    server::RenderDispatcher,
    share_service::{ReplayResolver, ShareClaim, ShareService},
};
use rand::RngCore;
use serde_json::{json, Value};
use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
    time::{Duration, SystemTime},
};

struct ActiveJob {
    jobs: Arc<Mutex<RenderJobs>>,
    job_id: String,
    account_id: u64,
    lifecycle_epoch: u64,
}
struct PreflightBinding {
    replay_name: String,
    canonical_path: std::path::PathBuf,
    arena_unique_id: String,
    source_sha256: String,
    account_id: u64,
    lifecycle_epoch: u64,
    expires_at: SystemTime,
    used: bool,
}
pub struct Coordinator {
    app: tauri::AppHandle,
    dispatch: RenderDispatcher,
    account: Arc<crate::account::AccountService>,
    entries: Arc<Mutex<HashMap<String, Value>>>,
    preflights: Arc<Mutex<HashMap<String, PreflightBinding>>>,
    active_jobs: Arc<Mutex<HashMap<String, ActiveJob>>>,
}
impl Coordinator {
    pub fn new(
        app: tauri::AppHandle,
        dispatch: RenderDispatcher,
        account: Arc<crate::account::AccountService>,
    ) -> Self {
        let active_jobs: Arc<Mutex<HashMap<String, ActiveJob>>> = Default::default();
        let watched = active_jobs.clone();
        let mut snapshots = account.subscribe();
        tauri::async_runtime::spawn(async move {
            loop {
                let snapshot = snapshots.borrow_and_update().clone();
                for job in watched.lock().unwrap().values() {
                    if !snapshot.can_use_account
                        || snapshot.account_id != Some(job.account_id)
                        || snapshot.lifecycle_epoch != job.lifecycle_epoch
                    {
                        let _ = job.jobs.lock().unwrap().cancel(&job.job_id);
                    }
                }
                if snapshots.changed().await.is_err() {
                    break;
                }
            }
        });
        Self {
            app,
            dispatch,
            account,
            entries: Default::default(),
            preflights: Default::default(),
            active_jobs,
        }
    }
    fn valid_account(&self, account_id: u64, epoch: u64) -> bool {
        let current = self.account.snapshot();
        current.can_use_account
            && current.account_id == Some(account_id)
            && current.lifecycle_epoch == epoch
    }
}
impl ShareService for Coordinator {
    fn open_authorization(&self, url: &str) -> bool {
        use tauri_plugin_opener::OpenerExt;
        self.app.opener().open_url(url, None::<&str>).is_ok()
    }
    fn status(&self, id: &str) -> Option<Value> {
        self.entries.lock().unwrap().get(id).cloned()
    }
    fn preflight(
        &self,
        replay_name: String,
        arena_unique_id: String,
        evidence: bridge_core::replay_identity::ReplayEvidence,
    ) -> Result<bridge_core::share_service::SharePreflight, String> {
        let snapshot = self.account.snapshot();
        let Some(account_id) = snapshot.account_id.filter(|_| snapshot.can_use_account) else {
            return Err("account_unavailable".into());
        };
        if evidence.recorder.account_id != account_id {
            return Err("recorder_account_mismatch".into());
        }
        let canonical_path = evidence
            .artifact
            .path
            .canonicalize()
            .map_err(|_| "replay_unavailable")?;
        let mut random = [0u8; 32];
        rand::rngs::OsRng.fill_bytes(&mut random);
        let id = hex::encode(random);
        let realm = evidence.recorder.realm.map(|r| {
            match r {
                bridge_core::replay_identity::RecorderRealm::Eu => "eu",
                bridge_core::replay_identity::RecorderRealm::Na => "na",
                bridge_core::replay_identity::RecorderRealm::Asia => "asia",
                bridge_core::replay_identity::RecorderRealm::Cis => "cis",
            }
            .to_string()
        });
        let binding = PreflightBinding {
            replay_name,
            canonical_path,
            arena_unique_id,
            source_sha256: evidence.artifact.source_sha256_hex(),
            account_id,
            lifecycle_epoch: snapshot.lifecycle_epoch,
            expires_at: SystemTime::now() + Duration::from_secs(120),
            used: false,
        };
        self.preflights.lock().unwrap().insert(id.clone(), binding);
        Ok(bridge_core::share_service::SharePreflight {
            preflight_id: id,
            recorder_account_id: account_id.to_string(),
            recorder_realm: realm,
        })
    }
    fn start(
        &self,
        claim: ShareClaim,
        resolve: ReplayResolver,
        port: u16,
        jobs: Arc<Mutex<RenderJobs>>,
    ) -> Result<String, String> {
        let preflight_id = claim.preflight_id.clone().ok_or("missing_preflight")?;
        let binding = {
            let mut values = self.preflights.lock().unwrap();
            let Some(binding) = values.get_mut(&preflight_id) else {
                return Err("preflight_invalid".into());
            };
            if binding.used || SystemTime::now() >= binding.expires_at {
                return Err("preflight_expired".into());
            }
            if binding.replay_name != claim.replay_name.clone().unwrap_or_default()
                || binding.arena_unique_id != claim.arena_unique_id
            {
                return Err("preflight_mismatch".into());
            }
            if !self.valid_account(binding.account_id, binding.lifecycle_epoch) {
                return Err("account_lifecycle_changed".into());
            }
            binding.used = true;
            (
                binding.canonical_path.clone(),
                binding.source_sha256.clone(),
                binding.account_id,
                binding.lifecycle_epoch,
                binding.arena_unique_id.clone(),
            )
        };
        let expiry = claim
            .capability_expires_at
            .as_deref()
            .map(chrono::DateTime::parse_from_rfc3339)
            .transpose()
            .map_err(|_| "invalid_expiry")?
            .map(SystemTime::from)
            .unwrap_or(SystemTime::now() + Duration::from_secs(2 * 3600));
        let auth = AuthorizedShare::new(
            claim.share_id.clone(),
            claim.capability,
            expiry,
            claim.max_bytes,
        )
        .map_err(|_| "invalid_authorization")?;
        let id = claim.state.unwrap_or_else(|| claim.share_id.clone());
        let mut entries = self.entries.lock().unwrap();
        if entries
            .get(&id)
            .is_some_and(|v| v["share_id"] == claim.share_id && v["state"] != "failed")
        {
            return Ok(id);
        }
        if entries
            .values()
            .any(|v| v["share_id"] == claim.share_id && v["state"] != "failed")
        {
            return Err("share_already_active".into());
        }
        if entries
            .values()
            .any(|v| !matches!(v["state"].as_str(), Some("posted" | "failed" | "expired")))
        {
            return Err("share_busy".into());
        }
        if entries.len() >= 16 {
            entries.clear();
        }
        entries.insert(
            id.clone(),
            json!({"id":id,"share_id":claim.share_id,"state":"authorizing"}),
        );
        drop(entries);
        let entries = self.entries.clone();
        let dispatch = self.dispatch.clone();
        let account = self.account.clone();
        let active_jobs = self.active_jobs.clone();
        let task_id = id.clone();
        let task = async move {
            let account_valid = || {
                let current = account.snapshot();
                current.can_use_account
                    && current.account_id == Some(binding.2)
                    && current.lifecycle_epoch == binding.3
            };
            if !account_valid() {
                return;
            }
            let canonical_path = binding.0;
            let expected_sha = binding.1;
            let expected_arena = binding.4;

            let update = |state: &str, extra: Value| {
                let mut value = json!({"id":task_id,"share_id":claim.share_id,"state":state});
                if let (Some(dest), Some(src)) = (value.as_object_mut(), extra.as_object()) {
                    dest.extend(src.clone());
                }
                entries.lock().unwrap().insert(task_id.clone(), value);
            };
            let result:Result<(),String>=async {
                let client=VideoShareClient::new().map_err(|_|"engine_unavailable")?;
                let initial=client.status(&auth).await.map_err(|_|"authorization_rejected")?;
                if !matches!(initial.status,DeliveryState::Authorized|DeliveryState::Uploading) {return Err("share_not_authorized".into())}
                update("resolving",json!({}));
                let name=tauri::async_runtime::spawn_blocking(resolve).await.map_err(|_|"replay_resolution_failed")??;
                let evidence=tauri::async_runtime::spawn_blocking({ let path=canonical_path.clone(); move || bridge_core::replay_identity::read_replay_evidence(path) }).await.map_err(|_|"replay_unavailable")?.map_err(|_|"replay_replaced")?;
                if evidence.artifact.source_sha256_hex()!=expected_sha || !account_valid() { return Err("account_lifecycle_changed".into()); }
                if SystemTime::now()>=expiry {return Err("authorization_expired".into())}
                let start=jobs.lock().unwrap().start(RenderRequest{replay_name:name,share_id:Some(claim.share_id.clone()),arena_unique_id:Some(expected_arena.clone()),max_bytes:Some(claim.max_bytes),preflight_id:Some(preflight_id.clone()),source_sha256:Some(expected_sha.clone()),video_options:None}).map_err(|_|"render_busy")?;
                let job_id=match start {
                    StartRender::Existing(status)=>status.id,
                    StartRender::New{status,worker}=>{
                        if dispatch(port,worker.clone(),jobs.clone()).is_err(){let _=jobs.lock().unwrap().fail(&worker,"worker_start_failed");return Err("worker_start_failed".into())}
                        status.id
                    }
                };
                active_jobs.lock().unwrap().insert(task_id.clone(), ActiveJob { jobs:jobs.clone(), job_id:job_id.clone(), account_id:binding.2, lifecycle_epoch:binding.3 });
                let rendered=loop{
                    if !account_valid() { let _=jobs.lock().unwrap().cancel(&job_id); return Err("account_lifecycle_changed".into()); }
                    let s=jobs.lock().unwrap().status(&job_id).map_err(|_|"render_unavailable")?;
                    if s.state==RenderState::Rendered {break s}
                    if matches!(s.state,RenderState::Failed|RenderState::Cancelled){return Err("render_failed".into())}
                    if SystemTime::now()>=expiry {let _=jobs.lock().unwrap().cancel(&job_id);return Err("authorization_expired".into())}
                    update("rendering",json!({"job_id":job_id,"frame":s.frame,"total":s.total,"attempt":s.attempt}));
                    tokio::time::sleep(Duration::from_secs(1)).await;
                };
                if !account_valid() { return Err("account_lifecycle_changed".into()); }
                let info=rendered.video.ok_or("missing_video_metadata")?;
                let path=jobs.lock().unwrap().output(&job_id).map_err(|_|"video_unavailable")?.to_path_buf();
                let video=tauri::async_runtime::spawn_blocking(move||PreparedVideo::read(&path,claim.max_bytes)).await.map_err(|_|"video_read_failed")?.map_err(|_|"video_read_failed")?;
                if !account_valid() { return Err("account_lifecycle_changed".into()); }
                update("uploading",json!({"job_id":job_id,"bytes":video.receipt().bytes}));
                if client.upload(&auth,video,VideoMetadata{duration_s:info.duration_s,width:info.width,height:info.height}).await.is_err(){
                    // A completion response can be lost after Engine queued it.
                    let remote=client.status(&auth).await.map_err(|_|"upload_failed")?;
                    if !matches!(remote.status,DeliveryState::Queued|DeliveryState::Posting|DeliveryState::Posted){return Err("upload_failed".into())}
                }
                update("queued",json!({"job_id":job_id}));
                loop{
                    if !account_valid() { return Err("account_lifecycle_changed".into()); }
                    if SystemTime::now()>=expiry+Duration::from_secs(900){return Err("status_expired".into())}
                    let remote=client.status(&auth).await.map_err(|_|"status_unavailable")?;
                    let state=serde_json::to_value(&remote.status).unwrap();
                    let url=remote.message_url.filter(|s|s.starts_with("https://discord.com/channels/"));
                    update(state.as_str().unwrap(),json!({"job_id":job_id,"message_url":url}));
                    if matches!(remote.status,DeliveryState::Posted|DeliveryState::Failed|DeliveryState::Expired){return Ok(())}
                    tokio::time::sleep(Duration::from_secs(10)).await;
                }
            }.await;
            active_jobs.lock().unwrap().remove(&task_id);
            if let Err(code) = result {
                update("failed", json!({"error":code}));
            }
        };
        tauri::async_runtime::spawn(task);
        Ok(id)
    }
}
