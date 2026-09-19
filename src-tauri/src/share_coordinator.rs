//! Owns authorized jobs independently of the Engine tab and Replayer window.
use crate::{
    video_share::{
        AuthorizedShare, DeliveryState, DeliveryStatus, VideoMetadata, VideoShareClient,
        VideoShareTransport,
    },
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
    future::Future,
    pin::Pin,
    sync::{Arc, Mutex},
    time::{Duration, SystemTime},
};

trait AccountSnapshots: Send + Sync {
    fn snapshot(&self) -> crate::account::AccountSnapshot;
    fn subscribe(&self) -> tokio::sync::watch::Receiver<crate::account::AccountSnapshot>;
}

impl AccountSnapshots for crate::account::AccountService {
    fn snapshot(&self) -> crate::account::AccountSnapshot {
        self.snapshot()
    }

    fn subscribe(&self) -> tokio::sync::watch::Receiver<crate::account::AccountSnapshot> {
        self.subscribe()
    }
}

type Clock = Arc<dyn Fn() -> SystemTime + Send + Sync>;
type VideoClientFactory = Arc<dyn Fn() -> Result<Arc<dyn VideoShareTransport>, ()> + Send + Sync>;
type ReplayEvidenceReader = Arc<
    dyn Fn(std::path::PathBuf) -> Result<bridge_core::replay_identity::ReplayEvidence, ()>
        + Send
        + Sync,
>;
type SleepFuture = Pin<Box<dyn Future<Output = ()> + Send>>;
type Sleeper = Arc<dyn Fn(Duration) -> SleepFuture + Send + Sync>;

/// Never relay arbitrary Engine text to the local page or logs. These are the
/// public contract codes the player can turn into user-facing copy.
fn delivery_error(status: &DeliveryStatus, fallback: &str) -> String {
    match status.error.as_deref() {
        Some(
            "not_battle_owner"
            | "recorder_realm_mismatch"
            | "recorder_account_mismatch"
            | "capability_revoked",
        ) => status.error.clone().unwrap(),
        _ => fallback.into(),
    }
}

trait AuthorizationOpener: Send + Sync {
    fn open_authorization(&self, url: &str) -> bool;
}

#[cfg(not(test))]
struct TauriAuthorizationOpener {
    app: tauri::AppHandle,
}

#[cfg(not(test))]
impl AuthorizationOpener for TauriAuthorizationOpener {
    fn open_authorization(&self, url: &str) -> bool {
        use tauri_plugin_opener::OpenerExt;
        self.app.opener().open_url(url, None::<&str>).is_ok()
    }
}

#[cfg(test)]
struct UnavailableAuthorizationOpener;

#[cfg(test)]
impl AuthorizationOpener for UnavailableAuthorizationOpener {
    fn open_authorization(&self, _url: &str) -> bool {
        false
    }
}

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

/// Safe, native-only result of invalidating a share generation. A cancellation
/// request for a rendering job is asynchronous: `jobs_cancelling` means its
/// worker still owns the renderer and must later call `RenderJobs::stopped`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct AccountLifecycleAcknowledgement {
    pub preflights_invalidated: usize,
    pub jobs_cancelled: usize,
    pub jobs_cancelling: usize,
    pub jobs_already_terminal_or_uncancellable: usize,
}

pub struct Coordinator {
    authorization_opener: Arc<dyn AuthorizationOpener>,
    dispatch: RenderDispatcher,
    account: Arc<dyn AccountSnapshots>,
    clock: Clock,
    video_client: VideoClientFactory,
    replay_evidence: ReplayEvidenceReader,
    sleep: Sleeper,
    entries: Arc<Mutex<HashMap<String, Value>>>,
    preflights: Arc<Mutex<HashMap<String, PreflightBinding>>>,
    active_jobs: Arc<Mutex<HashMap<String, ActiveJob>>>,
}

/// Managed state used only by native account lifecycle code. The registry never
/// creates a coordinator: player attachment remains its owner. Before the
/// player attaches, after it detaches, and between replacements it acknowledges
/// zero work. Attaching a replacement drops this registry's old strong handle.
#[derive(Default)]
pub(crate) struct ShareLifecycle {
    coordinator: Mutex<Option<Arc<Coordinator>>>,
}

impl ShareLifecycle {
    pub(crate) fn attach(&self, coordinator: Arc<Coordinator>) {
        *self.coordinator.lock().unwrap() = Some(coordinator);
    }

    pub(crate) fn detach(&self) {
        self.coordinator.lock().unwrap().take();
    }

    // The debug-reset command calls this after it advances the account epoch.
    // It intentionally has no current command caller until that reset lands.
    #[allow(dead_code)]
    pub(crate) fn invalidate_for_account(
        &self,
        snapshot: &crate::account::AccountSnapshot,
    ) -> AccountLifecycleAcknowledgement {
        let coordinator = self.coordinator.lock().unwrap().clone();
        coordinator
            .map(|coordinator| coordinator.invalidate_for_account(snapshot))
            .unwrap_or_default()
    }
}

fn invalid_for_snapshot(
    snapshot: &crate::account::AccountSnapshot,
    account_id: u64,
    lifecycle_epoch: u64,
) -> bool {
    !snapshot.can_use_account
        || snapshot.account_id != Some(account_id)
        || snapshot.lifecycle_epoch != lifecycle_epoch
}

impl Coordinator {
    pub fn new(
        app: tauri::AppHandle,
        dispatch: RenderDispatcher,
        account: Arc<crate::account::AccountService>,
    ) -> Arc<Self> {
        let snapshots = AccountSnapshots::subscribe(account.as_ref());
        #[cfg(not(test))]
        let authorization_opener = Arc::new(TauriAuthorizationOpener { app });
        #[cfg(test)]
        let authorization_opener: Arc<dyn AuthorizationOpener> = {
            let _ = app;
            Arc::new(UnavailableAuthorizationOpener)
        };
        Self::with_dependencies(
            authorization_opener,
            dispatch,
            account,
            snapshots,
            Arc::new(SystemTime::now),
            Arc::new(|| {
                VideoShareClient::new()
                    .map(|client| Arc::new(client) as Arc<dyn VideoShareTransport>)
                    .map_err(|_| ())
            }),
            Arc::new(|path| {
                bridge_core::replay_identity::read_replay_evidence(path).map_err(|_| ())
            }),
            Arc::new(|duration| Box::pin(tokio::time::sleep(duration))),
        )
    }

    fn with_dependencies(
        authorization_opener: Arc<dyn AuthorizationOpener>,
        dispatch: RenderDispatcher,
        account: Arc<dyn AccountSnapshots>,
        snapshots: tokio::sync::watch::Receiver<crate::account::AccountSnapshot>,
        clock: Clock,
        video_client: VideoClientFactory,
        replay_evidence: ReplayEvidenceReader,
        sleep: Sleeper,
    ) -> Arc<Self> {
        let coordinator = Arc::new(Self {
            authorization_opener,
            dispatch,
            account,
            clock,
            video_client,
            replay_evidence,
            sleep,
            entries: Default::default(),
            preflights: Default::default(),
            active_jobs: Default::default(),
        });
        coordinator.spawn_account_watch(snapshots);
        coordinator
    }

    fn spawn_account_watch(
        self: &Arc<Self>,
        mut snapshots: tokio::sync::watch::Receiver<crate::account::AccountSnapshot>,
    ) {
        let coordinator = Arc::downgrade(self);
        tauri::async_runtime::spawn(async move {
            loop {
                let snapshot = snapshots.borrow_and_update().clone();
                let Some(coordinator) = coordinator.upgrade() else {
                    break;
                };
                coordinator.invalidate_for_account(&snapshot);
                if snapshots.changed().await.is_err() {
                    break;
                }
            }
        });
    }

    /// Revoke work bound to any account generation other than `snapshot`.
    ///
    /// Locks are deliberately never nested: take stale job references under
    /// `active_jobs`, release it, then request cancellation under each
    /// `RenderJobs` lock, and finally update public status. No lock spans an
    /// await, and `Cancelling` means worker shutdown is still in progress.
    pub(crate) fn invalidate_for_account(
        &self,
        snapshot: &crate::account::AccountSnapshot,
    ) -> AccountLifecycleAcknowledgement {
        let mut acknowledgement = AccountLifecycleAcknowledgement::default();
        {
            let mut preflights = self.preflights.lock().unwrap();
            let before = preflights.len();
            preflights.retain(|_, binding| {
                !invalid_for_snapshot(snapshot, binding.account_id, binding.lifecycle_epoch)
            });
            acknowledgement.preflights_invalidated = before - preflights.len();
        }

        let affected_jobs = {
            let mut active_jobs = self.active_jobs.lock().unwrap();
            let affected = active_jobs
                .iter()
                .filter(|(_, job)| {
                    invalid_for_snapshot(snapshot, job.account_id, job.lifecycle_epoch)
                })
                .map(|(share_id, job)| (share_id.clone(), job.jobs.clone(), job.job_id.clone()))
                .collect::<Vec<_>>();
            active_jobs.retain(|_, job| {
                !invalid_for_snapshot(snapshot, job.account_id, job.lifecycle_epoch)
            });
            affected
        };

        for (_, jobs, job_id) in &affected_jobs {
            let mut jobs = jobs.lock().unwrap();
            match jobs.status(job_id).map(|status| status.state) {
                Ok(RenderState::Queued) if jobs.cancel(job_id).is_ok() => {
                    acknowledgement.jobs_cancelled += 1;
                }
                Ok(RenderState::Rendering) if jobs.cancel(job_id).is_ok() => {
                    acknowledgement.jobs_cancelling += 1;
                }
                _ => acknowledgement.jobs_already_terminal_or_uncancellable += 1,
            }
        }

        if !affected_jobs.is_empty() {
            let mut entries = self.entries.lock().unwrap();
            for (share_id, _, _) in affected_jobs {
                if let Some(entry) = entries.get_mut(&share_id).and_then(Value::as_object_mut) {
                    entry.insert("state".into(), Value::String("failed".into()));
                    entry.insert(
                        "error".into(),
                        Value::String("account_lifecycle_changed".into()),
                    );
                    entry.remove("message_url");
                }
            }
        }
        acknowledgement
    }

    #[cfg(test)]
    fn reconcile_account(&self) -> AccountLifecycleAcknowledgement {
        self.invalidate_for_account(&self.account.snapshot())
    }

    fn valid_account(&self, account_id: u64, epoch: u64) -> bool {
        let current = self.account.snapshot();
        !invalid_for_snapshot(&current, account_id, epoch)
    }
}
impl ShareService for Coordinator {
    fn open_authorization(&self, url: &str) -> bool {
        self.authorization_opener.open_authorization(url)
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
            expires_at: (self.clock)() + Duration::from_secs(120),
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
            if binding.used || (self.clock)() >= binding.expires_at {
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
            .unwrap_or((self.clock)() + Duration::from_secs(2 * 3600));
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
        let clock = self.clock.clone();
        let video_client = self.video_client.clone();
        let replay_evidence = self.replay_evidence.clone();
        let sleep = self.sleep.clone();
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
                let mut values = entries.lock().unwrap();
                // Lifecycle invalidation wins over a stale completion that was
                // already between its account check and this status write.
                if values
                    .get(&task_id)
                    .is_some_and(|value| value["error"] == "account_lifecycle_changed")
                {
                    return;
                }
                let mut value = json!({"id":task_id,"share_id":claim.share_id,"state":state});
                if let (Some(dest), Some(src)) = (value.as_object_mut(), extra.as_object()) {
                    dest.extend(src.clone());
                }
                values.insert(task_id.clone(), value);
            };
            let result:Result<(),String>=async {
                let client=(video_client)().map_err(|_|"engine_unavailable")?;
                let initial=client.status(&auth).await.map_err(|_|"authorization_rejected")?;
                if !account_valid() { return Err("account_lifecycle_changed".into()); }
                match initial.status {
                    DeliveryState::Authorized | DeliveryState::Uploading => {}
                    DeliveryState::Failed | DeliveryState::Expired => {
                        return Err(delivery_error(&initial, "authorization_rejected"));
                    }
                    _ => return Err("share_not_authorized".into()),
                }
                update("resolving",json!({}));
                let name=tauri::async_runtime::spawn_blocking(resolve).await.map_err(|_|"replay_resolution_failed")??;
                let evidence=tauri::async_runtime::spawn_blocking({
                    let path=canonical_path.clone();
                    let replay_evidence=replay_evidence.clone();
                    move || replay_evidence(path)
                }).await.map_err(|_|"replay_unavailable")?.map_err(|_|"replay_replaced")?;
                if evidence.artifact.source_sha256_hex()!=expected_sha || !account_valid() { return Err("account_lifecycle_changed".into()); }
                if (clock)()>=expiry {return Err("authorization_expired".into())}
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
                    if (clock)()>=expiry {let _=jobs.lock().unwrap().cancel(&job_id);return Err("authorization_expired".into())}
                    update("rendering",json!({"job_id":job_id,"frame":s.frame,"total":s.total,"attempt":s.attempt}));
                    (sleep)(Duration::from_secs(1)).await;
                };
                if !account_valid() { return Err("account_lifecycle_changed".into()); }
                let info=rendered.video.ok_or("missing_video_metadata")?;
                let path=jobs.lock().unwrap().output(&job_id).map_err(|_|"video_unavailable")?.to_path_buf();
                let video=tauri::async_runtime::spawn_blocking(move||PreparedVideo::read(&path,claim.max_bytes)).await.map_err(|_|"video_read_failed")?.map_err(|_|"video_read_failed")?;
                if !account_valid() { return Err("account_lifecycle_changed".into()); }
                update("uploading",json!({"job_id":job_id,"bytes":video.receipt().bytes}));
                if client.upload(&auth,video,VideoMetadata{duration_s:info.duration_s,width:info.width,height:info.height}).await.is_err(){
                    if !account_valid() { return Err("account_lifecycle_changed".into()); }
                    // A completion response can be lost after Engine queued it.
                    let remote=client.status(&auth).await.map_err(|_|"upload_failed")?;
                    if !account_valid() { return Err("account_lifecycle_changed".into()); }
                    if !matches!(remote.status,DeliveryState::Queued|DeliveryState::Posting|DeliveryState::Posted){
                        return Err(delivery_error(&remote, "upload_failed"));
                    }
                }
                if !account_valid() { return Err("account_lifecycle_changed".into()); }
                update("queued",json!({"job_id":job_id}));
                loop{
                    if !account_valid() { return Err("account_lifecycle_changed".into()); }
                    if (clock)()>=expiry+Duration::from_secs(900){return Err("status_expired".into())}
                    let remote=client.status(&auth).await.map_err(|_|"status_unavailable")?;
                    if !account_valid() { return Err("account_lifecycle_changed".into()); }
                    let state=serde_json::to_value(&remote.status).unwrap();
                    let error=if matches!(remote.status, DeliveryState::Failed) {
                        Some(delivery_error(&remote, "delivery_failed"))
                    } else { None };
                    let url=remote.message_url.filter(|s|s.starts_with("https://discord.com/channels/"));
                    update(state.as_str().unwrap(),json!({"job_id":job_id,"message_url":url,"error":error}));
                    if matches!(remote.status,DeliveryState::Posted|DeliveryState::Failed|DeliveryState::Expired){return Ok(())}
                    (sleep)(Duration::from_secs(10)).await;
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::account::{AccountSnapshot, AccountStatus};
    use crate::video_share::ShareFuture;
    use bridge_core::replay_identity::{RecorderIdentity, ReplayArtifact, ReplayEvidence};
    use std::{
        collections::VecDeque,
        sync::atomic::{AtomicUsize, Ordering},
    };

    struct FakeAccount {
        snapshots: tokio::sync::watch::Sender<AccountSnapshot>,
    }
    impl FakeAccount {
        fn new(snapshot: AccountSnapshot) -> Self {
            let (snapshots, _) = tokio::sync::watch::channel(snapshot);
            Self { snapshots }
        }
        fn set(&self, snapshot: AccountSnapshot) {
            self.snapshots.send_replace(snapshot);
        }
    }
    impl AccountSnapshots for FakeAccount {
        fn snapshot(&self) -> AccountSnapshot {
            self.snapshots.borrow().clone()
        }
        fn subscribe(&self) -> tokio::sync::watch::Receiver<AccountSnapshot> {
            self.snapshots.subscribe()
        }
    }

    fn snapshot(account_id: Option<u64>, epoch: u64, can_use_account: bool) -> AccountSnapshot {
        AccountSnapshot {
            status: AccountStatus::SignedOut,
            can_use_account,
            lifecycle_epoch: epoch,
            account_id,
            nickname: None,
            clan_tag: None,
            realm: None,
            web_lease_expires_at: None,
            access_expires_at_unix: None,
            end_reason: None,
        }
    }

    #[derive(Default)]
    struct FakeAuthorizationOpener {
        opened_urls: Mutex<Vec<String>>,
    }

    impl AuthorizationOpener for FakeAuthorizationOpener {
        fn open_authorization(&self, url: &str) -> bool {
            self.opened_urls.lock().unwrap().push(url.into());
            true
        }
    }

    enum StatusStep {
        Reply(DeliveryStatus),
        Wait {
            entered: Arc<tokio::sync::Notify>,
            release: Arc<tokio::sync::Notify>,
            reply: DeliveryStatus,
        },
    }

    struct UploadStep {
        entered: Arc<tokio::sync::Notify>,
        release: Arc<tokio::sync::Notify>,
    }

    struct ScriptedVideoClient {
        statuses: Mutex<VecDeque<StatusStep>>,
        status_calls: AtomicUsize,
        upload_calls: AtomicUsize,
        upload_wait: Mutex<Option<UploadStep>>,
    }

    impl ScriptedVideoClient {
        fn new(statuses: Vec<StatusStep>) -> Self {
            Self {
                statuses: Mutex::new(statuses.into()),
                status_calls: AtomicUsize::new(0),
                upload_calls: AtomicUsize::new(0),
                upload_wait: Mutex::new(None),
            }
        }

        fn wait_during_upload(
            &self,
            entered: Arc<tokio::sync::Notify>,
            release: Arc<tokio::sync::Notify>,
        ) {
            *self.upload_wait.lock().unwrap() = Some(UploadStep { entered, release });
        }
    }

    impl VideoShareTransport for ScriptedVideoClient {
        fn status<'a>(&'a self, _: &'a AuthorizedShare) -> ShareFuture<'a, DeliveryStatus> {
            self.status_calls.fetch_add(1, Ordering::SeqCst);
            let step = self.statuses.lock().unwrap().pop_front();
            Box::pin(async move {
                match step.expect("unexpected status request") {
                    StatusStep::Reply(status) => Ok(status),
                    StatusStep::Wait {
                        entered,
                        release,
                        reply,
                    } => {
                        entered.notify_one();
                        release.notified().await;
                        Ok(reply)
                    }
                }
            })
        }

        fn upload<'a>(
            &'a self,
            _: &'a AuthorizedShare,
            _: PreparedVideo,
            _: VideoMetadata,
        ) -> ShareFuture<'a, ()> {
            self.upload_calls.fetch_add(1, Ordering::SeqCst);
            let wait = self.upload_wait.lock().unwrap().take();
            Box::pin(async move {
                if let Some(wait) = wait {
                    wait.entered.notify_one();
                    wait.release.notified().await;
                }
                Ok(())
            })
        }
    }

    fn delivery(status: DeliveryState, error: Option<&str>) -> DeliveryStatus {
        DeliveryStatus {
            status,
            attempt: 1,
            message_url: None,
            error: error.map(str::to_owned),
            capability_expires_at: None,
        }
    }

    fn dispatcher() -> RenderDispatcher {
        Arc::new(|_, _, _| Ok(()))
    }

    fn coordinator_with_opener(
        account: Arc<FakeAccount>,
        now: Arc<Mutex<SystemTime>>,
        authorization_opener: Arc<dyn AuthorizationOpener>,
    ) -> Arc<Coordinator> {
        let watch = account.subscribe();
        Coordinator::with_dependencies(
            authorization_opener,
            dispatcher(),
            account,
            watch,
            Arc::new(move || *now.lock().unwrap()),
            Arc::new(|| Err(())),
            Arc::new(|_| Err(())),
            Arc::new(|_| Box::pin(async {})),
        )
    }

    fn coordinator(account: Arc<FakeAccount>, now: Arc<Mutex<SystemTime>>) -> Arc<Coordinator> {
        coordinator_with_opener(account, now, Arc::new(FakeAuthorizationOpener::default()))
    }

    fn coordinator_with_transport(
        account: Arc<FakeAccount>,
        now: Arc<Mutex<SystemTime>>,
        transport: Arc<dyn VideoShareTransport>,
        dispatch: RenderDispatcher,
        replay_evidence: ReplayEvidenceReader,
    ) -> Arc<Coordinator> {
        let watch = account.subscribe();
        Coordinator::with_dependencies(
            Arc::new(FakeAuthorizationOpener::default()),
            dispatch,
            account,
            watch,
            Arc::new(move || *now.lock().unwrap()),
            Arc::new(move || Ok(transport.clone())),
            replay_evidence,
            Arc::new(|_| Box::pin(tokio::task::yield_now())),
        )
    }

    async fn wait_for_state(coordinator: &Coordinator, id: &str, state: &str) -> Value {
        for _ in 0..1000 {
            if let Some(value) = coordinator.status(id) {
                if value["state"] == state {
                    return value;
                }
            }
            tokio::task::yield_now().await;
        }
        panic!("coordinator did not reach {state}");
    }

    #[test]
    fn authorization_opener_is_injected_without_tauri_dependencies() {
        let account = Arc::new(FakeAccount::new(snapshot(Some(42), 3, true)));
        let opener = Arc::new(FakeAuthorizationOpener::default());
        let coordinator = coordinator_with_opener(
            account,
            Arc::new(Mutex::new(SystemTime::now())),
            opener.clone(),
        );

        assert!(coordinator.open_authorization("https://engine.tfd.rocks/authorize"));
        assert_eq!(
            *opener.opened_urls.lock().unwrap(),
            ["https://engine.tfd.rocks/authorize"]
        );
    }

    fn evidence(path: &std::path::Path, account_id: u64) -> ReplayEvidence {
        ReplayEvidence {
            artifact: ReplayArtifact {
                path: path.to_path_buf(),
                source_sha256: [7; 32],
            },
            recorder: RecorderIdentity {
                account_id,
                realm: None,
            },
        }
    }

    fn claim(replay_name: Option<&str>, arena: &str, preflight_id: Option<String>) -> ShareClaim {
        ShareClaim {
            state: None,
            replay_name: replay_name.map(str::to_owned),
            arena_unique_id: arena.into(),
            share_id: "share-1".into(),
            capability: "capability".into(),
            capability_expires_at: None,
            max_bytes: 1024,
            preflight_id,
        }
    }

    fn render_request() -> RenderRequest {
        RenderRequest {
            replay_name: "battle.wowsreplay".into(),
            share_id: Some("share-1".into()),
            arena_unique_id: Some("9007199254740993".into()),
            max_bytes: Some(1024),
            preflight_id: Some("opaque-preflight".into()),
            source_sha256: Some("a".repeat(64)),
            video_options: None,
        }
    }

    #[test]
    fn preflight_binds_the_current_account_and_permits_an_absent_realm() {
        let file = tempfile::NamedTempFile::new().unwrap();
        let account = Arc::new(FakeAccount::new(snapshot(Some(42), 3, true)));
        let now = Arc::new(Mutex::new(SystemTime::now()));
        let coordinator = coordinator(account.clone(), now);
        let preflight = coordinator
            .preflight(
                "battle.wowsreplay".into(),
                "9007199254740993".into(),
                evidence(file.path(), 42),
            )
            .unwrap();
        assert_eq!(preflight.recorder_account_id, "42");
        assert_eq!(preflight.recorder_realm, None);
        assert_eq!(preflight.preflight_id.len(), 64);

        assert!(matches!(
            coordinator.preflight("battle.wowsreplay".into(), "9007199254740993".into(), evidence(file.path(), 7)),
            Err(ref error) if error == "recorder_account_mismatch"
        ));
        account.set(snapshot(None, 4, false));
        assert!(matches!(
            coordinator.preflight("battle.wowsreplay".into(), "9007199254740993".into(), evidence(file.path(), 42)),
            Err(ref error) if error == "account_unavailable"
        ));
    }

    #[test]
    fn start_consumes_a_live_binding_once_and_checks_its_identity_and_ttl() {
        let file = tempfile::NamedTempFile::new().unwrap();
        let account = Arc::new(FakeAccount::new(snapshot(Some(42), 3, true)));
        let now = Arc::new(Mutex::new(SystemTime::now()));
        let coordinator = coordinator(account.clone(), now.clone());
        let preflight = coordinator
            .preflight(
                "battle.wowsreplay".into(),
                "9007199254740993".into(),
                evidence(file.path(), 42),
            )
            .unwrap();

        assert_eq!(
            coordinator.start(
                claim(
                    Some("other.wowsreplay"),
                    "9007199254740993",
                    Some(preflight.preflight_id.clone())
                ),
                Box::new(|| Ok("battle.wowsreplay".into())),
                43210,
                Arc::new(Mutex::new(RenderJobs::default())),
            ),
            Err("preflight_mismatch".into())
        );
        assert_eq!(
            coordinator.start(
                claim(
                    Some("battle.wowsreplay"),
                    "wrong-battle",
                    Some(preflight.preflight_id.clone())
                ),
                Box::new(|| Ok("battle.wowsreplay".into())),
                43210,
                Arc::new(Mutex::new(RenderJobs::default())),
            ),
            Err("preflight_mismatch".into())
        );
        account.set(snapshot(Some(42), 4, true));
        assert_eq!(
            coordinator.start(
                claim(
                    Some("battle.wowsreplay"),
                    "9007199254740993",
                    Some(preflight.preflight_id.clone())
                ),
                Box::new(|| Ok("battle.wowsreplay".into())),
                43210,
                Arc::new(Mutex::new(RenderJobs::default())),
            ),
            Err("account_lifecycle_changed".into())
        );

        account.set(snapshot(Some(42), 5, true));
        let valid = coordinator
            .preflight(
                "battle.wowsreplay".into(),
                "9007199254740993".into(),
                evidence(file.path(), 42),
            )
            .unwrap();
        let jobs = Arc::new(Mutex::new(RenderJobs::default()));
        assert_eq!(
            coordinator.start(
                claim(
                    Some("battle.wowsreplay"),
                    "9007199254740993",
                    Some(valid.preflight_id.clone())
                ),
                Box::new(|| Ok("battle.wowsreplay".into())),
                43210,
                jobs.clone(),
            ),
            Ok("share-1".into()),
            "a valid opaque binding starts exactly one authorized share"
        );
        assert_eq!(
            coordinator.start(
                claim(
                    Some("battle.wowsreplay"),
                    "9007199254740993",
                    Some(valid.preflight_id)
                ),
                Box::new(|| Ok("battle.wowsreplay".into())),
                43210,
                jobs,
            ),
            Err("preflight_expired".into()),
            "a claimed binding cannot be reused by a concurrent callback"
        );

        let expiring = coordinator
            .preflight(
                "battle.wowsreplay".into(),
                "9007199254740993".into(),
                evidence(file.path(), 42),
            )
            .unwrap();
        *now.lock().unwrap() += Duration::from_secs(121);
        assert_eq!(
            coordinator.start(
                claim(
                    Some("battle.wowsreplay"),
                    "9007199254740993",
                    Some(expiring.preflight_id)
                ),
                Box::new(|| Ok("battle.wowsreplay".into())),
                43210,
                Arc::new(Mutex::new(RenderJobs::default())),
            ),
            Err("preflight_expired".into())
        );
    }

    #[test]
    fn watcher_reconciliation_cancels_queued_and_rendering_jobs_on_logout_or_switch() {
        let account = Arc::new(FakeAccount::new(snapshot(Some(42), 3, true)));
        let coordinator = coordinator(account.clone(), Arc::new(Mutex::new(SystemTime::now())));
        let queued = Arc::new(Mutex::new(RenderJobs::default()));
        let queued_lease = match queued.lock().unwrap().start(render_request()).unwrap() {
            StartRender::New { worker, .. } => worker,
            StartRender::Existing(_) => unreachable!(),
        };
        let rendering = Arc::new(Mutex::new(RenderJobs::default()));
        let rendering_lease = match rendering.lock().unwrap().start(render_request()).unwrap() {
            StartRender::New { worker, .. } => worker,
            StartRender::Existing(_) => unreachable!(),
        };
        rendering.lock().unwrap().claim(&rendering_lease).unwrap();
        coordinator.active_jobs.lock().unwrap().insert(
            "queued".into(),
            ActiveJob {
                jobs: queued.clone(),
                job_id: queued_lease.job_id.clone(),
                account_id: 42,
                lifecycle_epoch: 3,
            },
        );
        coordinator.active_jobs.lock().unwrap().insert(
            "rendering".into(),
            ActiveJob {
                jobs: rendering.clone(),
                job_id: rendering_lease.job_id.clone(),
                account_id: 42,
                lifecycle_epoch: 3,
            },
        );

        account.set(snapshot(Some(99), 4, true));
        coordinator.reconcile_account();
        assert_eq!(
            queued
                .lock()
                .unwrap()
                .status(&queued_lease.job_id)
                .unwrap()
                .state,
            RenderState::Cancelled
        );
        assert_eq!(
            rendering
                .lock()
                .unwrap()
                .status(&rendering_lease.job_id)
                .unwrap()
                .state,
            RenderState::Cancelling
        );
        assert_eq!(
            rendering
                .lock()
                .unwrap()
                .rendered(&rendering_lease, "late.mp4".into(), 10),
            Err(bridge_core::render_jobs::JobError::InvalidTransition),
            "a late worker completion loses to the watcher cancellation"
        );

        account.set(snapshot(None, 5, false));
        coordinator.reconcile_account();
        assert_eq!(
            queued
                .lock()
                .unwrap()
                .status(&queued_lease.job_id)
                .unwrap()
                .state,
            RenderState::Cancelled
        );
    }
    #[test]
    fn account_lifecycle_invalidation_revokes_bindings_cancels_jobs_and_marks_status() {
        let replay = tempfile::NamedTempFile::new().unwrap();
        let account = Arc::new(FakeAccount::new(snapshot(Some(42), 3, true)));
        let coordinator = coordinator(account, Arc::new(Mutex::new(SystemTime::now())));
        for arena in ["9007199254740993", "9007199254740994"] {
            coordinator
                .preflight(
                    "battle.wowsreplay".into(),
                    arena.into(),
                    evidence(replay.path(), 42),
                )
                .unwrap();
        }

        let queued = Arc::new(Mutex::new(RenderJobs::default()));
        let queued_lease = match queued.lock().unwrap().start(render_request()).unwrap() {
            StartRender::New { worker, .. } => worker,
            StartRender::Existing(_) => unreachable!(),
        };
        let rendering = Arc::new(Mutex::new(RenderJobs::default()));
        let rendering_lease = match rendering.lock().unwrap().start(render_request()).unwrap() {
            StartRender::New { worker, .. } => worker,
            StartRender::Existing(_) => unreachable!(),
        };
        rendering.lock().unwrap().claim(&rendering_lease).unwrap();
        let cancelling = Arc::new(Mutex::new(RenderJobs::default()));
        let cancelling_lease = match cancelling.lock().unwrap().start(render_request()).unwrap() {
            StartRender::New { worker, .. } => worker,
            StartRender::Existing(_) => unreachable!(),
        };
        cancelling.lock().unwrap().claim(&cancelling_lease).unwrap();
        cancelling
            .lock()
            .unwrap()
            .cancel(&cancelling_lease.job_id)
            .unwrap();
        let terminal = Arc::new(Mutex::new(RenderJobs::default()));
        let terminal_lease = match terminal.lock().unwrap().start(render_request()).unwrap() {
            StartRender::New { worker, .. } => worker,
            StartRender::Existing(_) => unreachable!(),
        };
        terminal
            .lock()
            .unwrap()
            .fail(&terminal_lease, "worker_failed")
            .unwrap();

        for (share_id, jobs, job_id) in [
            ("queued", queued.clone(), queued_lease.job_id.clone()),
            (
                "rendering",
                rendering.clone(),
                rendering_lease.job_id.clone(),
            ),
            (
                "cancelling",
                cancelling.clone(),
                cancelling_lease.job_id.clone(),
            ),
            ("terminal", terminal.clone(), terminal_lease.job_id.clone()),
        ] {
            coordinator.active_jobs.lock().unwrap().insert(
                share_id.into(),
                ActiveJob {
                    jobs,
                    job_id,
                    account_id: 42,
                    lifecycle_epoch: 3,
                },
            );
            coordinator.entries.lock().unwrap().insert(
                share_id.into(),
                json!({"id":share_id,"share_id":share_id,"state":"rendering"}),
            );
        }

        let acknowledgement = coordinator.invalidate_for_account(&snapshot(None, 4, false));
        assert_eq!(
            acknowledgement,
            AccountLifecycleAcknowledgement {
                preflights_invalidated: 2,
                jobs_cancelled: 1,
                jobs_cancelling: 1,
                jobs_already_terminal_or_uncancellable: 2,
            }
        );
        assert_eq!(
            queued
                .lock()
                .unwrap()
                .status(&queued_lease.job_id)
                .unwrap()
                .state,
            RenderState::Cancelled
        );
        assert_eq!(
            rendering
                .lock()
                .unwrap()
                .status(&rendering_lease.job_id)
                .unwrap()
                .state,
            RenderState::Cancelling
        );
        for share_id in ["queued", "rendering", "cancelling", "terminal"] {
            let status = coordinator.status(share_id).unwrap();
            assert_eq!(status["state"], "failed");
            assert_eq!(status["error"], "account_lifecycle_changed");
        }
        assert!(coordinator.preflights.lock().unwrap().is_empty());
    }

    #[test]
    fn same_account_generation_is_a_noop_but_switching_account_or_epoch_is_not() {
        let replay = tempfile::NamedTempFile::new().unwrap();
        let account = Arc::new(FakeAccount::new(snapshot(Some(42), 3, true)));
        let coordinator = coordinator(account, Arc::new(Mutex::new(SystemTime::now())));
        coordinator
            .preflight(
                "battle.wowsreplay".into(),
                "9007199254740993".into(),
                evidence(replay.path(), 42),
            )
            .unwrap();
        assert_eq!(
            coordinator.invalidate_for_account(&snapshot(Some(42), 3, true)),
            AccountLifecycleAcknowledgement::default()
        );
        assert_eq!(coordinator.preflights.lock().unwrap().len(), 1);
        assert_eq!(
            coordinator
                .invalidate_for_account(&snapshot(Some(99), 3, true))
                .preflights_invalidated,
            1,
            "an account switch invalidates even if the epoch was not advanced"
        );
        coordinator
            .preflight(
                "battle.wowsreplay".into(),
                "9007199254740994".into(),
                evidence(replay.path(), 42),
            )
            .unwrap();
        assert_eq!(
            coordinator
                .invalidate_for_account(&snapshot(Some(42), 4, true))
                .preflights_invalidated,
            1,
            "an epoch change invalidates even when the account ID is unchanged"
        );
    }

    #[tokio::test]
    async fn account_watch_delegates_to_the_lifecycle_invalidator() {
        let account = Arc::new(FakeAccount::new(snapshot(Some(42), 3, true)));
        let coordinator = coordinator(account.clone(), Arc::new(Mutex::new(SystemTime::now())));
        let jobs = Arc::new(Mutex::new(RenderJobs::default()));
        let lease = match jobs.lock().unwrap().start(render_request()).unwrap() {
            StartRender::New { worker, .. } => worker,
            StartRender::Existing(_) => unreachable!(),
        };
        coordinator.active_jobs.lock().unwrap().insert(
            "watcher".into(),
            ActiveJob {
                jobs: jobs.clone(),
                job_id: lease.job_id.clone(),
                account_id: 42,
                lifecycle_epoch: 3,
            },
        );
        coordinator.entries.lock().unwrap().insert(
            "watcher".into(),
            json!({"id":"watcher","share_id":"watcher","state":"rendering"}),
        );
        account.set(snapshot(Some(99), 4, true));
        for _ in 0..1000 {
            if jobs.lock().unwrap().status(&lease.job_id).unwrap().state == RenderState::Cancelled {
                break;
            }
            tokio::task::yield_now().await;
        }
        assert_eq!(
            jobs.lock().unwrap().status(&lease.job_id).unwrap().state,
            RenderState::Cancelled
        );
        assert_eq!(
            coordinator.status("watcher").unwrap()["error"],
            "account_lifecycle_changed"
        );
        assert_eq!(
            coordinator.reconcile_account(),
            AccountLifecycleAcknowledgement::default()
        );
    }

    #[test]
    fn managed_lifecycle_is_unattached_until_player_attachment_and_detaches_cleanly() {
        let lifecycle = ShareLifecycle::default();
        assert_eq!(
            lifecycle.invalidate_for_account(&snapshot(None, 1, false)),
            AccountLifecycleAcknowledgement::default()
        );
        let replay = tempfile::NamedTempFile::new().unwrap();
        let account = Arc::new(FakeAccount::new(snapshot(Some(42), 3, true)));
        let first = coordinator(account.clone(), Arc::new(Mutex::new(SystemTime::now())));
        first
            .preflight(
                "battle.wowsreplay".into(),
                "9007199254740993".into(),
                evidence(replay.path(), 42),
            )
            .unwrap();
        lifecycle.attach(first.clone());

        let second = coordinator(account, Arc::new(Mutex::new(SystemTime::now())));
        second
            .preflight(
                "battle.wowsreplay".into(),
                "9007199254740994".into(),
                evidence(replay.path(), 42),
            )
            .unwrap();
        lifecycle.attach(second.clone());
        assert_eq!(
            lifecycle
                .invalidate_for_account(&snapshot(None, 4, false))
                .preflights_invalidated,
            1,
            "replacement targets only the newly attached coordinator"
        );
        assert_eq!(first.preflights.lock().unwrap().len(), 1);
        lifecycle.detach();
        assert_eq!(
            lifecycle.invalidate_for_account(&snapshot(None, 5, false)),
            AccountLifecycleAcknowledgement::default()
        );
    }

    #[test]
    fn delivery_errors_are_strictly_whitelisted() {
        for code in [
            "not_battle_owner",
            "recorder_realm_mismatch",
            "recorder_account_mismatch",
            "capability_revoked",
        ] {
            assert_eq!(
                delivery_error(
                    &delivery(DeliveryState::Failed, Some(code)),
                    "delivery_failed"
                ),
                code
            );
        }
        assert_eq!(
            delivery_error(
                &delivery(
                    DeliveryState::Failed,
                    Some("Engine said: capability=secret")
                ),
                "delivery_failed"
            ),
            "delivery_failed"
        );
    }

    #[tokio::test]
    async fn initial_engine_rejection_is_exposed_only_as_a_safe_code() {
        for (engine_error, expected) in [
            (Some("not_battle_owner"), "not_battle_owner"),
            (
                Some("raw Engine rejection with secret context"),
                "authorization_rejected",
            ),
        ] {
            let file = tempfile::NamedTempFile::new().unwrap();
            let account = Arc::new(FakeAccount::new(snapshot(Some(42), 3, true)));
            let now = Arc::new(Mutex::new(SystemTime::now()));
            let transport = Arc::new(ScriptedVideoClient::new(vec![StatusStep::Reply(delivery(
                DeliveryState::Failed,
                engine_error,
            ))]));
            let coordinator = coordinator_with_transport(
                account,
                now,
                transport,
                dispatcher(),
                Arc::new(|_| Err(())),
            );
            let preflight = coordinator
                .preflight(
                    "battle.wowsreplay".into(),
                    "9007199254740993".into(),
                    evidence(file.path(), 42),
                )
                .unwrap();
            let id = coordinator
                .start(
                    claim(
                        Some("battle.wowsreplay"),
                        "9007199254740993",
                        Some(preflight.preflight_id),
                    ),
                    Box::new(|| Ok("battle.wowsreplay".into())),
                    43210,
                    Arc::new(Mutex::new(RenderJobs::default())),
                )
                .unwrap();
            assert_eq!(
                wait_for_state(&coordinator, &id, "failed").await["error"],
                expected
            );
        }
    }

    #[tokio::test]
    async fn account_change_while_initial_status_waits_cannot_render_or_upload() {
        let file = tempfile::NamedTempFile::new().unwrap();
        let account = Arc::new(FakeAccount::new(snapshot(Some(42), 3, true)));
        let now = Arc::new(Mutex::new(SystemTime::now()));
        let entered = Arc::new(tokio::sync::Notify::new());
        let release = Arc::new(tokio::sync::Notify::new());
        let transport = Arc::new(ScriptedVideoClient::new(vec![StatusStep::Wait {
            entered: entered.clone(),
            release: release.clone(),
            reply: delivery(DeliveryState::Authorized, None),
        }]));
        let dispatches = Arc::new(AtomicUsize::new(0));
        let dispatch_count = dispatches.clone();
        let dispatch: RenderDispatcher = Arc::new(move |_, _, _| {
            dispatch_count.fetch_add(1, Ordering::SeqCst);
            Ok(())
        });
        let coordinator = coordinator_with_transport(
            account.clone(),
            now,
            transport.clone(),
            dispatch,
            Arc::new(|_| Err(())),
        );
        let preflight = coordinator
            .preflight(
                "battle.wowsreplay".into(),
                "9007199254740993".into(),
                evidence(file.path(), 42),
            )
            .unwrap();
        let id = coordinator
            .start(
                claim(
                    Some("battle.wowsreplay"),
                    "9007199254740993",
                    Some(preflight.preflight_id),
                ),
                Box::new(|| Ok("battle.wowsreplay".into())),
                43210,
                Arc::new(Mutex::new(RenderJobs::default())),
            )
            .unwrap();
        entered.notified().await;
        account.set(snapshot(Some(99), 4, true));
        release.notify_one();
        let final_status = wait_for_state(&coordinator, &id, "failed").await;
        assert_eq!(final_status["error"], "account_lifecycle_changed");
        assert_eq!(dispatches.load(Ordering::SeqCst), 0);
        assert_eq!(transport.upload_calls.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn account_change_during_upload_cannot_poll_or_publish_the_old_result() {
        let replay = tempfile::NamedTempFile::new().unwrap();
        let video = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(video.path(), b"\0\0\0\x18ftypisom\0\0\0\0isomavc1").unwrap();
        let account = Arc::new(FakeAccount::new(snapshot(Some(42), 3, true)));
        let now = Arc::new(Mutex::new(SystemTime::now()));
        let upload_entered = Arc::new(tokio::sync::Notify::new());
        let upload_release = Arc::new(tokio::sync::Notify::new());
        let transport = Arc::new(ScriptedVideoClient::new(vec![StatusStep::Reply(delivery(
            DeliveryState::Authorized,
            None,
        ))]));
        transport.wait_during_upload(upload_entered.clone(), upload_release.clone());
        let (workers, mut received_workers) = tokio::sync::mpsc::unbounded_channel();
        let dispatch: RenderDispatcher = Arc::new(move |_, worker, jobs| {
            workers.send((worker, jobs)).unwrap();
            Ok(())
        });
        let replay_path = replay.path().to_path_buf();
        let coordinator = coordinator_with_transport(
            account.clone(),
            now,
            transport.clone(),
            dispatch,
            Arc::new(move |_| Ok(evidence(&replay_path, 42))),
        );
        let preflight = coordinator
            .preflight(
                "battle.wowsreplay".into(),
                "9007199254740993".into(),
                evidence(replay.path(), 42),
            )
            .unwrap();
        let expected_hash = hex::encode([7; 32]);
        let id = coordinator
            .start(
                claim(
                    Some("battle.wowsreplay"),
                    "9007199254740993",
                    Some(preflight.preflight_id.clone()),
                ),
                Box::new(|| Ok("battle.wowsreplay".into())),
                43210,
                Arc::new(Mutex::new(RenderJobs::default())),
            )
            .unwrap();
        let (worker, jobs) = received_workers.recv().await.unwrap();
        let request = jobs.lock().unwrap().request_for_worker(&worker).unwrap();
        assert_eq!(
            request.preflight_id.as_deref(),
            Some(preflight.preflight_id.as_str())
        );
        assert_eq!(
            request.source_sha256.as_deref(),
            Some(expected_hash.as_str())
        );
        let mut jobs_guard = jobs.lock().unwrap();
        jobs_guard.claim(&worker).unwrap();
        jobs_guard
            .video_info(
                &worker,
                bridge_core::render_jobs::VideoInfo {
                    duration_s: 1.0,
                    width: 1920,
                    height: 1080,
                },
            )
            .unwrap();
        jobs_guard
            .rendered(&worker, video.path().to_path_buf(), 24)
            .unwrap();
        drop(jobs_guard);
        upload_entered.notified().await;
        account.set(snapshot(Some(99), 4, true));
        upload_release.notify_one();
        let final_status = wait_for_state(&coordinator, &id, "failed").await;
        assert_eq!(final_status["error"], "account_lifecycle_changed");
        assert_eq!(transport.status_calls.load(Ordering::SeqCst), 1);
        assert!(final_status.get("message_url").is_none());
    }

    #[tokio::test]
    async fn final_engine_rejection_is_mapped_and_unknown_error_is_redacted() {
        for (remote_state, engine_error, expected_state, expected_error) in [
            (
                DeliveryState::Failed,
                Some("capability_revoked"),
                "failed",
                Some("capability_revoked"),
            ),
            (
                DeliveryState::Failed,
                Some("internal remote detail"),
                "failed",
                Some("delivery_failed"),
            ),
            (DeliveryState::Posted, None, "posted", None),
        ] {
            let replay = tempfile::NamedTempFile::new().unwrap();
            let video = tempfile::NamedTempFile::new().unwrap();
            std::fs::write(video.path(), b"\0\0\0\x18ftypisom\0\0\0\0isomavc1").unwrap();
            let account = Arc::new(FakeAccount::new(snapshot(Some(42), 3, true)));
            let now = Arc::new(Mutex::new(SystemTime::now()));
            let transport = Arc::new(ScriptedVideoClient::new(vec![
                StatusStep::Reply(delivery(DeliveryState::Authorized, None)),
                StatusStep::Reply(delivery(remote_state, engine_error)),
            ]));
            let (workers, mut received_workers) = tokio::sync::mpsc::unbounded_channel();
            let dispatch: RenderDispatcher = Arc::new(move |_, worker, jobs| {
                workers.send((worker, jobs)).unwrap();
                Ok(())
            });
            let replay_path = replay.path().to_path_buf();
            let coordinator = coordinator_with_transport(
                account,
                now,
                transport,
                dispatch,
                Arc::new(move |_| Ok(evidence(&replay_path, 42))),
            );
            let preflight = coordinator
                .preflight(
                    "battle.wowsreplay".into(),
                    "9007199254740993".into(),
                    evidence(replay.path(), 42),
                )
                .unwrap();
            let id = coordinator
                .start(
                    claim(
                        Some("battle.wowsreplay"),
                        "9007199254740993",
                        Some(preflight.preflight_id),
                    ),
                    Box::new(|| Ok("battle.wowsreplay".into())),
                    43210,
                    Arc::new(Mutex::new(RenderJobs::default())),
                )
                .unwrap();
            let (worker, jobs) = received_workers.recv().await.unwrap();
            let mut jobs_guard = jobs.lock().unwrap();
            jobs_guard.claim(&worker).unwrap();
            jobs_guard
                .video_info(
                    &worker,
                    bridge_core::render_jobs::VideoInfo {
                        duration_s: 1.0,
                        width: 1920,
                        height: 1080,
                    },
                )
                .unwrap();
            jobs_guard
                .rendered(&worker, video.path().to_path_buf(), 24)
                .unwrap();
            drop(jobs_guard);
            let final_status = wait_for_state(&coordinator, &id, expected_state).await;
            assert_eq!(final_status["error"].as_str(), expected_error);
        }
    }
}
