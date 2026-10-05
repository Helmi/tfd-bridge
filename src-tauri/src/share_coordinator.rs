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
use serde_json::{json, Value};
use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
    time::{Duration, SystemTime},
};

// Parked: Discord video sharing is not wired in until account authorization ships.
#[allow(dead_code)]
pub struct Coordinator {
    app: tauri::AppHandle,
    dispatch: RenderDispatcher,
    entries: Arc<Mutex<HashMap<String, Value>>>,
}
impl Coordinator {
    #[allow(dead_code)]
    pub fn new(app: tauri::AppHandle, dispatch: RenderDispatcher) -> Self {
        Self {
            app,
            dispatch,
            entries: Default::default(),
        }
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
    fn start(
        &self,
        claim: ShareClaim,
        resolve: ReplayResolver,
        port: u16,
        jobs: Arc<Mutex<RenderJobs>>,
    ) -> Result<String, String> {
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
        let task_id = id.clone();
        let task = async move {
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
                if SystemTime::now()>=expiry {return Err("authorization_expired".into())}
                let start=jobs.lock().unwrap().start(RenderRequest{replay_name:name,share_id:Some(claim.share_id.clone()),arena_unique_id:Some(claim.arena_unique_id),max_bytes:Some(claim.max_bytes),video_options:None}).map_err(|_|"render_busy")?;
                let job_id=match start {
                    StartRender::Existing(status)=>status.id,
                    StartRender::New{status,worker}=>{
                        if dispatch(port,worker.clone(),jobs.clone()).is_err(){let _=jobs.lock().unwrap().fail(&worker,"worker_start_failed");return Err("worker_start_failed".into())}
                        status.id
                    }
                };
                let rendered=loop{
                    let s=jobs.lock().unwrap().status(&job_id).map_err(|_|"render_unavailable")?;
                    if s.state==RenderState::Rendered {break s}
                    if matches!(s.state,RenderState::Failed|RenderState::Cancelled){return Err("render_failed".into())}
                    if SystemTime::now()>=expiry {let _=jobs.lock().unwrap().cancel(&job_id);return Err("authorization_expired".into())}
                    update("rendering",json!({"job_id":job_id,"frame":s.frame,"total":s.total,"attempt":s.attempt}));
                    tokio::time::sleep(Duration::from_secs(1)).await;
                };
                let info=rendered.video.ok_or("missing_video_metadata")?;
                let path=jobs.lock().unwrap().output(&job_id).map_err(|_|"video_unavailable")?.to_path_buf();
                let video=tauri::async_runtime::spawn_blocking(move||PreparedVideo::read(&path,claim.max_bytes)).await.map_err(|_|"video_read_failed")?.map_err(|_|"video_read_failed")?;
                update("uploading",json!({"job_id":job_id,"bytes":video.receipt().bytes}));
                if client.upload(&auth,video,VideoMetadata{duration_s:info.duration_s,width:info.width,height:info.height}).await.is_err(){
                    // A completion response can be lost after Engine queued it.
                    let remote=client.status(&auth).await.map_err(|_|"upload_failed")?;
                    if !matches!(remote.status,DeliveryState::Queued|DeliveryState::Posting|DeliveryState::Posted){return Err("upload_failed".into())}
                }
                update("queued",json!({"job_id":job_id}));
                loop{
                    if SystemTime::now()>=expiry+Duration::from_secs(900){return Err("status_expired".into())}
                    let remote=client.status(&auth).await.map_err(|_|"status_unavailable")?;
                    let state=serde_json::to_value(&remote.status).unwrap();
                    let url=remote.message_url.filter(|s|s.starts_with("https://discord.com/channels/"));
                    update(state.as_str().unwrap(),json!({"job_id":job_id,"message_url":url}));
                    if matches!(remote.status,DeliveryState::Posted|DeliveryState::Failed|DeliveryState::Expired){return Ok(())}
                    tokio::time::sleep(Duration::from_secs(10)).await;
                }
            }.await;
            if let Err(code) = result {
                update("failed", json!({"error":code}));
            }
        };
        tauri::async_runtime::spawn(task);
        Ok(id)
    }
}
