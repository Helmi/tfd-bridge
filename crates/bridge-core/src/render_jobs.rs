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
}

impl VideoOptions {
    fn valid(&self) -> bool {
        matches!(self.resolution.as_str(), "1080" | "1440")
            && matches!(self.fps, 24 | 30 | 60)
            && (1_000_000..=12_000_000).contains(&self.bitrate)
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
pub struct VideoInfo { pub duration_s: f64, pub width: u32, pub height: u32 }

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
        if !info.duration_s.is_finite() || info.duration_s <= 0.0 || info.width == 0 || info.height == 0 { return Err(JobError::InvalidRequest); }
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
        if bytes == 0 || bytes > job.request.max_bytes.unwrap_or(512 * 1024 * 1024) {
            return Err(JobError::ExceedsLimit);
        }
        job.status.state = RenderState::Rendered;
        job.status.bytes = Some(bytes);
        job.output = Some(path);
        Ok(())
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
        VideoOptions { map_supersampling: false, resolution: "1080".into(), fps: 30, bitrate: 4_000_000, speed: 10 }
    }

    #[test]
    fn local_video_preferences_are_validated_and_preserved_for_worker() {
        for resolution in ["1080", "1440"] {
            for fps in [24, 30, 60] {
                for speed in [5, 10, 20] {
                    for bitrate in [1_000_000, 4_000_000, 12_000_000] {
                        let mut jobs = RenderJobs::default();
                        let expected = local_request(VideoOptions { map_supersampling: false, resolution: resolution.into(), fps, bitrate, speed });
                        let StartRender::New { worker, .. } = jobs.start(expected.clone()).unwrap() else { panic!("expected new job") };
                        assert_eq!(jobs.claim(&worker).unwrap(), expected);
                    }
                }
            }
        }
        let invalid = [
            VideoOptions { resolution: "2160".into(), ..video_options() },
            VideoOptions { fps: 0, ..video_options() },
            VideoOptions { fps: 25, ..video_options() },
            VideoOptions { fps: 61, ..video_options() },
            VideoOptions { bitrate: 999_999, ..video_options() },
            VideoOptions { bitrate: 12_000_001, ..video_options() },
            VideoOptions { speed: 0, ..video_options() },
            VideoOptions { speed: 15, ..video_options() },
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
        value["mapSupersampling"] = true.into();
        let enhanced: VideoOptions = serde_json::from_value(value).unwrap();
        assert!(enhanced.map_supersampling);
        assert!(RenderJobs::default().start(local_request(enhanced)).is_ok());
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
