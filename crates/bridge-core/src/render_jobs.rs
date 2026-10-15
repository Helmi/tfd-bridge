//! Local render ownership shared by the Replayer and post-battle entry points.
//! This registry does not authorize a share or perform network requests.
use serde::{Deserialize, Serialize};
use std::collections::VecDeque;
use std::path::PathBuf;

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct RenderRequest {
    pub replay_name: String,
    /// Engine share identity, when rendering for an authorized Discord share.
    /// Both entry points must use the same identity returned by Engine.
    pub share_id: Option<String>,
    pub arena_unique_id: Option<String>,
    pub max_bytes: Option<u64>,
    /// Local-save preferences. Discord shares use Engine's upload budget.
    #[serde(default)]
    pub video_options: Option<VideoOptions>,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct VideoOptions {
    #[serde(default, rename = "mapSupersampling")]
    pub map_supersampling: bool,
    pub resolution: String,
    pub fps: u32,
    pub bitrate: u32,
    pub speed: u32,
    /// Video codec; requests without it render H.264.
    #[serde(default = "default_codec")]
    pub codec: String,
}

fn default_codec() -> String {
    "h264".into()
}

fn valid_codec(codec: &str) -> bool {
    matches!(codec, "h264" | "av1" | "hevc")
}

impl VideoOptions {
    fn valid(&self) -> bool {
        valid_codec(&self.codec)
            && matches!(self.resolution.as_str(), "1080" | "1440" | "2160")
            && matches!(self.fps, 24 | 30 | 60)
            && (1_000_000..=80_000_000).contains(&self.bitrate)
            && matches!(self.speed, 5 | 10 | 20)
    }
}

#[derive(Clone, Copy, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum RenderState {
    Queued,
    Rendering,
    Cancelling,
    Rendered,
    Failed,
    Cancelled,
}

#[derive(Clone, Debug, Serialize)]
pub struct RenderStatus {
    pub video: Option<VideoInfo>,
    pub id: String,
    pub state: RenderState,
    pub frame: u32,
    pub total: u32,
    pub attempt: u8,
    pub bytes: Option<u64>,
    pub error: Option<String>,
}

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct VideoInfo {
    pub duration_s: f64,
    pub width: u32,
    pub height: u32,
    /// Encoder diagnostics reported by the worker; absent from older workers.
    #[serde(default)]
    pub codec: Option<String>,
    /// True when the accepted encoder config was hardware (GPU) accelerated.
    #[serde(default)]
    pub hardware: Option<bool>,
    #[serde(default)]
    pub render_ms_per_frame: Option<f64>,
    #[serde(default)]
    pub wait_ms_per_frame: Option<f64>,
    #[serde(default)]
    pub fps: Option<f64>,
}

impl VideoInfo {
    fn valid(&self) -> bool {
        self.duration_s.is_finite()
            && self.duration_s > 0.0
            && self.width != 0
            && self.height != 0
            && self.codec.as_deref().is_none_or(valid_codec)
            && [self.render_ms_per_frame, self.wait_ms_per_frame, self.fps]
                .iter()
                .all(|v| v.is_none_or(|v| v.is_finite() && v >= 0.0))
    }
}

/// Worker-only capability. Do not serialize it into public status responses.
#[derive(Clone)]
pub struct WorkerLease {
    pub job_id: String,
    pub token: String,
}

pub enum StartRender {
    New {
        status: RenderStatus,
        worker: WorkerLease,
    },
    Existing(RenderStatus),
}

#[derive(Debug, PartialEq, Eq)]
pub enum JobError {
    Busy,
    NotFound,
    Forbidden,
    InvalidRequest,
    InvalidTransition,
    InvalidProgress,
    ExceedsLimit,
}

struct Job {
    request: RenderRequest,
    status: RenderStatus,
    token: String,
    output: Option<PathBuf>,
    /// Streamed local-save upload in progress; deleted on failure/cancel.
    partial: Option<PathBuf>,
}

/// Largest local-save (unbudgeted) video; budgeted jobs use their `max_bytes`.
pub const STREAM_MAX_BYTES: u64 = 8 * 1024 * 1024 * 1024;

fn remove_partial(job: &mut Job) {
    if let Some(path) = job.partial.take() {
        let _ = std::fs::remove_file(path);
    }
}

/// One active render keeps CPU/GPU and encoded-memory use bounded. Wrap in the
/// server's mutex; no lock is held while rendering or uploading.
#[derive(Default)]
pub struct RenderJobs {
    jobs: VecDeque<Job>,
}

impl RenderJobs {
    pub fn start(&mut self, request: RenderRequest) -> Result<StartRender, JobError> {
        if request.replay_name.is_empty()
            || request.replay_name.len() > 1024
            || request
                .share_id
                .as_ref()
                .is_some_and(|id| id.is_empty() || id.len() > 128)
            || request
                .arena_unique_id
                .as_ref()
                .is_some_and(|id| !valid_arena(id))
            || request
                .max_bytes
                .is_some_and(|cap| cap == 0 || cap > 512 * 1024 * 1024)
            || request
                .video_options
                .as_ref()
                .is_some_and(|options| !options.valid() || request.share_id.is_some())
        {
            return Err(JobError::InvalidRequest);
        }
        for job in &self.jobs {
            if matches!(
                job.status.state,
                RenderState::Queued | RenderState::Rendering | RenderState::Cancelling
            ) {
                return if job.request == request {
                    Ok(StartRender::Existing(job.status.clone()))
                } else {
                    Err(JobError::Busy)
                };
            }
        }
        // A share already rendered locally can be uploaded/retried without
        // running the entire battle again. Local save clicks can render anew.
        if request.share_id.is_some() {
            if let Some(job) = self
                .jobs
                .iter()
                .find(|job| job.request == request && job.status.state == RenderState::Rendered)
            {
                return Ok(StartRender::Existing(job.status.clone()));
            }
        }
        while self.jobs.len() >= 16 {
            self.jobs.pop_front();
        }
        let id = uuid::Uuid::new_v4().to_string();
        let token = uuid::Uuid::new_v4().to_string();
        let status = RenderStatus {
            video: None,
            id: id.clone(),
            state: RenderState::Queued,
            frame: 0,
            total: 0,
            attempt: 0,
            bytes: None,
            error: None,
        };
        self.jobs.push_back(Job {
            request,
            status: status.clone(),
            token: token.clone(),
            output: None,
            partial: None,
        });
        Ok(StartRender::New {
            status,
            worker: WorkerLease { job_id: id, token },
        })
    }

    pub fn status(&self, id: &str) -> Result<RenderStatus, JobError> {
        self.jobs
            .iter()
            .find(|j| j.status.id == id)
            .map(|j| j.status.clone())
            .ok_or(JobError::NotFound)
    }

    pub fn video_info(&mut self, lease: &WorkerLease, info: VideoInfo) -> Result<(), JobError> {
        if !info.valid() { return Err(JobError::InvalidRequest); }
        let job=self.worker(lease)?;
        if job.status.state != RenderState::Rendering {return Err(JobError::InvalidTransition)}
        job.status.video=Some(info); Ok(())
    }

    fn worker(&mut self, lease: &WorkerLease) -> Result<&mut Job, JobError> {
        let job = self
            .jobs
            .iter_mut()
            .find(|j| j.status.id == lease.job_id)
            .ok_or(JobError::NotFound)?;
        if job.token != lease.token {
            return Err(JobError::Forbidden);
        }
        Ok(job)
    }

    pub fn claim(&mut self, lease: &WorkerLease) -> Result<RenderRequest, JobError> {
        let job = self.worker(lease)?;
        if job.status.state != RenderState::Queued {
            return Err(JobError::InvalidTransition);
        }
        job.status.state = RenderState::Rendering;
        Ok(job.request.clone())
    }

    pub fn request_for_worker(&mut self, lease: &WorkerLease) -> Result<RenderRequest, JobError> {
        Ok(self.worker(lease)?.request.clone())
    }

    pub fn progress(
        &mut self,
        lease: &WorkerLease,
        attempt: u8,
        frame: u32,
        total: u32,
    ) -> Result<(), JobError> {
        let job = self.worker(lease)?;
        if job.status.state != RenderState::Rendering {
            return Err(JobError::InvalidTransition);
        }
        if !(1..=3).contains(&attempt)
            || attempt < job.status.attempt
            || attempt > job.status.attempt + 1
            || total == 0
            || frame > total
            || (attempt == job.status.attempt && frame < job.status.frame)
            || (job.status.total != 0 && total != job.status.total)
        {
            return Err(JobError::InvalidProgress);
        }
        job.status.frame = frame;
        job.status.attempt = attempt;
        job.status.total = total;
        Ok(())
    }

    /// Called by native persistence after writing and verifying the output,
    /// never directly with a browser-provided filesystem path.
    pub fn rendered(
        &mut self,
        lease: &WorkerLease,
        path: PathBuf,
        bytes: u64,
    ) -> Result<(), JobError> {
        let job = self.worker(lease)?;
        if job.status.state != RenderState::Rendering {
            return Err(JobError::InvalidTransition);
        }
        if bytes == 0 || bytes > job.request.max_bytes.unwrap_or(STREAM_MAX_BYTES) {
            return Err(JobError::ExceedsLimit);
        }
        job.partial = None; // renamed to `path` by the caller
        job.status.state = RenderState::Rendered;
        job.status.bytes = Some(bytes);
        job.output = Some(path);
        Ok(())
    }

    /// Partial file for a streamed upload of an unbudgeted, rendering job.
    pub fn partial_path(&mut self, lease: &WorkerLease, dir: &std::path::Path) -> Result<PathBuf, JobError> {
        let job = self.worker(lease)?;
        if job.status.state != RenderState::Rendering {
            return Err(JobError::InvalidTransition);
        }
        if job.request.max_bytes.is_some() {
            return Err(JobError::Forbidden);
        }
        let path = dir.join(format!("{}.mp4.part", job.status.id));
        job.partial = Some(path.clone());
        Ok(path)
    }

    pub fn fail(&mut self, lease: &WorkerLease, error_code: &str) -> Result<(), JobError> {
        let job = self.worker(lease)?;
        if !matches!(
            job.status.state,
            RenderState::Queued | RenderState::Rendering
        ) {
            return Err(JobError::InvalidTransition);
        }
        // Codes only: never relay raw encoder/network errors with local paths or tickets.
        if error_code.is_empty()
            || error_code.len() > 64
            || !error_code
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b == b'_')
        {
            return Err(JobError::InvalidRequest);
        }
        remove_partial(job);
        job.status.state = RenderState::Failed;
        job.status.error = Some(error_code.into());
        Ok(())
    }

    pub fn cancel(&mut self, id: &str) -> Result<(), JobError> {
        let job = self
            .jobs
            .iter_mut()
            .find(|j| j.status.id == id)
            .ok_or(JobError::NotFound)?;
        if !matches!(
            job.status.state,
            RenderState::Queued | RenderState::Rendering
        ) {
            return Err(JobError::InvalidTransition);
        }
        job.status.state = if job.status.state == RenderState::Queued {
            RenderState::Cancelled
        } else {
            RenderState::Cancelling
        };
        Ok(())
    }

    /// Cancellation releases the GPU slot only after the owning worker has
    /// stopped and destroyed its renderer (or the native worker has exited).
    pub fn stopped(&mut self, lease: &WorkerLease) -> Result<(), JobError> {
        let job = self.worker(lease)?;
        if job.status.state != RenderState::Cancelling {
            return Err(JobError::InvalidTransition);
        }
        remove_partial(job);
        job.status.state = RenderState::Cancelled;
        Ok(())
    }

    /// Native uploader access only; public status intentionally omits local paths.
    pub fn output(&self, id: &str) -> Result<&std::path::Path, JobError> {
        self.jobs
            .iter()
            .find(|j| j.status.id == id)
            .ok_or(JobError::NotFound)?
            .output
            .as_deref()
            .ok_or(JobError::InvalidTransition)
    }
}

fn valid_arena(id: &str) -> bool {
    !id.is_empty()
        && id.bytes().all(|b| b.is_ascii_digit())
        && id
            .parse::<u64>()
            .is_ok_and(|n| n > 0 && n.to_string() == id)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn request() -> RenderRequest {
        RenderRequest {
            replay_name: "battle.wowsreplay".into(),
            share_id: Some("share-1".into()),
            arena_unique_id: Some("9007199254740993".into()),
            max_bytes: Some(1024),
            video_options: None,
        }
    }
    fn local_request(options: VideoOptions) -> RenderRequest {
        RenderRequest {
            share_id: None,
            video_options: Some(options),
            ..request()
        }
    }
    fn video_options() -> VideoOptions {
        VideoOptions { map_supersampling: false, resolution: "1080".into(), fps: 30, bitrate: 4_000_000, speed: 10, codec: "h264".into() }
    }

    #[test]
    fn local_video_preferences_are_validated_and_preserved_for_worker() {
        for resolution in ["1080", "1440", "2160"] {
            for fps in [24, 30, 60] {
                for speed in [5, 10, 20] {
                    for bitrate in [1_000_000, 4_000_000, 80_000_000] {
                        let mut jobs = RenderJobs::default();
                        let expected = local_request(VideoOptions { map_supersampling: false, resolution: resolution.into(), fps, bitrate, speed, codec: "h264".into() });
                        let StartRender::New { worker, .. } = jobs.start(expected.clone()).unwrap() else { panic!("expected new job") };
                        assert_eq!(jobs.claim(&worker).unwrap(), expected);
                    }
                }
            }
        }
        let invalid = [
            VideoOptions { fps: 0, ..video_options() },
            VideoOptions { fps: 25, ..video_options() },
            VideoOptions { fps: 61, ..video_options() },
            VideoOptions { bitrate: 999_999, ..video_options() },
            VideoOptions { bitrate: 80_000_001, ..video_options() },
            VideoOptions { speed: 0, ..video_options() },
            VideoOptions { speed: 15, ..video_options() },
            VideoOptions { codec: "vp9".into(), ..video_options() },
            VideoOptions { codec: String::new(), ..video_options() },
        ];
        for options in invalid {
            assert!(matches!(RenderJobs::default().start(local_request(options)), Err(JobError::InvalidRequest)));
        }
    }

    #[test]
    fn local_preferences_cannot_override_share_policy_or_attach_to_other_settings() {
        let mut shared = request();
        shared.video_options = Some(video_options());
        assert!(matches!(RenderJobs::default().start(shared), Err(JobError::InvalidRequest)));

        let mut jobs = RenderJobs::default();
        let local = local_request(video_options());
        jobs.start(local.clone()).unwrap();
        assert!(matches!(jobs.start(local), Ok(StartRender::Existing(_))));
        let changed = local_request(VideoOptions { fps: 60, ..video_options() });
        assert!(matches!(jobs.start(changed), Err(JobError::Busy)));
    }

    #[test]
    fn old_render_requests_keep_default_video_settings() {
        let request: RenderRequest = serde_json::from_value(serde_json::json!({"replay_name": "battle.wowsreplay"})).unwrap();
        assert!(request.video_options.is_none());
        assert!(RenderJobs::default().start(request).is_ok());
        let mut value = serde_json::json!({"resolution":"1080","fps":30,"bitrate":4_000_000,"speed":10});
        let old: VideoOptions = serde_json::from_value(value.clone()).unwrap();
        assert!(!old.map_supersampling);
        assert_eq!(old.codec, "h264");
        value["mapSupersampling"] = true.into();
        let enhanced: VideoOptions = serde_json::from_value(value).unwrap();
        assert!(enhanced.map_supersampling);
        assert!(RenderJobs::default().start(local_request(enhanced)).is_ok());
    }
    #[test]
    fn codec_is_validated_and_defaults_to_h264() {
        for codec in ["h264", "av1", "hevc"] {
            let options = VideoOptions { codec: codec.into(), ..video_options() };
            assert!(RenderJobs::default().start(local_request(options)).is_ok());
        }
        let value = serde_json::json!({"resolution":"1080","fps":30,"bitrate":4_000_000,"speed":10,"codec":"av1"});
        assert_eq!(serde_json::from_value::<VideoOptions>(value).unwrap().codec, "av1");
    }
    #[test]
    fn video_info_accepts_old_and_new_fields_and_exposes_no_paths() {
        let old: VideoInfo = serde_json::from_value(serde_json::json!({"duration_s":9.0,"width":1920,"height":1080})).unwrap();
        assert!(old.valid() && old.codec.is_none() && old.fps.is_none());
        let new: VideoInfo = serde_json::from_value(serde_json::json!({"duration_s":9.0,"width":1920,"height":1080,
            "codec":"av1","hardware":true,"render_ms_per_frame":12.5,"wait_ms_per_frame":3.0,"fps":41.0})).unwrap();
        assert!(new.valid());
        assert!(!VideoInfo { codec: Some("vp9".into()), ..new.clone() }.valid());
        assert!(!VideoInfo { fps: Some(-1.0), ..new.clone() }.valid());
        assert!(!VideoInfo { wait_ms_per_frame: Some(f64::INFINITY), ..new.clone() }.valid());
        let mut jobs = RenderJobs::default();
        let StartRender::New { worker, status } = jobs.start(local_request(video_options())).unwrap() else { panic!("expected new job") };
        jobs.claim(&worker).unwrap();
        jobs.video_info(&worker, new).unwrap();
        let json = serde_json::to_value(jobs.status(&status.id).unwrap()).unwrap();
        assert_eq!(json["video"]["codec"], "av1");
        assert_eq!(json["video"]["hardware"], true);
        let text = json.to_string();
        assert!(!text.contains("path") && !text.contains(":\\") && !text.contains(".mp4"));
    }
    fn start(jobs: &mut RenderJobs) -> WorkerLease {
        match jobs.start(request()).unwrap() {
            StartRender::New { worker, .. } => worker,
            _ => panic!("expected new job"),
        }
    }
    #[test]
    fn shared_identity_deduplicates_and_only_one_worker_claims() {
        let mut jobs = RenderJobs::default();
        let lease = start(&mut jobs);
        assert!(matches!(
            jobs.start(request()),
            Ok(StartRender::Existing(_))
        ));
        let mut other = request();
        other.share_id = Some("share-2".into());
        assert!(matches!(jobs.start(other), Err(JobError::Busy)));
        assert!(jobs
            .claim(&WorkerLease {
                job_id: lease.job_id.clone(),
                token: "wrong".into()
            })
            .is_err());
        jobs.claim(&lease).unwrap();
        assert_eq!(jobs.claim(&lease), Err(JobError::InvalidTransition));
        jobs.progress(&lease, 1, 1, 10).unwrap();
        assert_eq!(
            jobs.progress(&lease, 1, 0, 10),
            Err(JobError::InvalidProgress)
        );
        assert_eq!(
            jobs.progress(&lease, 1, 2, 11),
            Err(JobError::InvalidProgress)
        );
        jobs.progress(&lease, 2, 0, 10).unwrap();
        assert_eq!(
            jobs.progress(&lease, 1, 3, 10),
            Err(JobError::InvalidProgress)
        );
    }
    #[test]
    fn cancellation_rejects_late_result_and_allows_new_work() {
        let mut jobs = RenderJobs::default();
        let lease = start(&mut jobs);
        jobs.claim(&lease).unwrap();
        jobs.cancel(&lease.job_id).unwrap();
        assert_eq!(
            jobs.rendered(&lease, "out.mp4".into(), 100),
            Err(JobError::InvalidTransition)
        );
        assert_eq!(
            jobs.fail(&lease, "late_error"),
            Err(JobError::InvalidTransition)
        );
        let mut other = request();
        other.share_id = Some("other".into());
        assert!(matches!(jobs.start(other), Err(JobError::Busy)));
        jobs.stopped(&lease).unwrap();
        assert_ne!(start(&mut jobs).job_id, lease.job_id);
    }
    #[test]
    fn unbudgeted_jobs_accept_large_files_budgeted_do_not() {
        let mib600 = 600 * 1024 * 1024;
        let mut jobs = RenderJobs::default();
        let mut local = request();
        local.share_id = None;
        local.max_bytes = None;
        let StartRender::New { worker, .. } = jobs.start(local).unwrap() else { panic!() };
        jobs.claim(&worker).unwrap();
        assert_eq!(jobs.rendered(&worker, "a.mp4".into(), STREAM_MAX_BYTES + 1), Err(JobError::ExceedsLimit));
        jobs.rendered(&worker, "a.mp4".into(), mib600).unwrap();
        let mut jobs = RenderJobs::default();
        let mut budgeted = request();
        budgeted.max_bytes = Some(512 * 1024 * 1024);
        let StartRender::New { worker, .. } = jobs.start(budgeted).unwrap() else { panic!() };
        jobs.claim(&worker).unwrap();
        assert_eq!(jobs.rendered(&worker, "a.mp4".into(), mib600), Err(JobError::ExceedsLimit));
    }

    #[test]
    fn partial_file_is_deleted_on_fail_and_stopped() {
        let dir = std::env::temp_dir().join(format!("tfd-partial-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let mut local = request();
        local.share_id = None;
        local.max_bytes = None;
        for cancel in [false, true] {
            let mut jobs = RenderJobs::default();
            let StartRender::New { worker, .. } = jobs.start(local.clone()).unwrap() else { panic!() };
            jobs.claim(&worker).unwrap();
            let part = jobs.partial_path(&worker, &dir).unwrap();
            std::fs::write(&part, b"x").unwrap();
            if cancel {
                jobs.cancel(&worker.job_id).unwrap();
                jobs.stopped(&worker).unwrap();
            } else {
                jobs.fail(&worker, "render_failed").unwrap();
            }
            assert!(!part.exists());
            let status = serde_json::to_string(&jobs.status(&worker.job_id).unwrap()).unwrap();
            assert!(!status.contains(".part"));
        }
        let mut jobs = RenderJobs::default();
        let StartRender::New { worker, .. } = jobs.start(request()).unwrap() else { panic!() };
        jobs.claim(&worker).unwrap();
        assert_eq!(jobs.partial_path(&worker, &dir), Err(JobError::Forbidden));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn output_is_bounded_reusable_and_private() {
        let mut jobs = RenderJobs::default();
        let lease = start(&mut jobs);
        jobs.claim(&lease).unwrap();
        assert_eq!(
            jobs.rendered(&lease, "private.mp4".into(), 1025),
            Err(JobError::ExceedsLimit)
        );
        jobs.rendered(&lease, "private.mp4".into(), 100).unwrap();
        assert_eq!(
            jobs.output(&lease.job_id).unwrap(),
            std::path::Path::new("private.mp4")
        );
        assert!(matches!(
            jobs.start(request()),
            Ok(StartRender::Existing(_))
        ));
        let status = serde_json::to_string(&jobs.status(&lease.job_id).unwrap()).unwrap();
        assert!(!status.contains(&lease.token));
        assert!(!status.contains("private.mp4"));
    }
}
