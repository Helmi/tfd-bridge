//! Local worker routes. Remote Engine start remains disabled until the
//! authorization handoff contract is implemented; no share is authorized here.
use super::*;
use crate::render_jobs::{JobError, RenderRequest, RenderState, StartRender, WorkerLease};
use serde::de::DeserializeOwned;

type Reply = Response<std::io::Cursor<Vec<u8>>>;
fn reply(status: u16, value: serde_json::Value) -> Reply {
    make_json_response(StatusCode(status), &value.to_string(), None)
}
fn error(code: &str, status: u16) -> Reply {
    reply(status, serde_json::json!({"error":code}))
}
fn job_error(e: JobError) -> Reply {
    let status = match e {
        JobError::NotFound => 404,
        JobError::Forbidden => 403,
        JobError::Busy | JobError::InvalidTransition => 409,
        _ => 400,
    };
    reply(status, serde_json::json!({"error":format!("{e:?}")}))
}
fn read_json<T: DeserializeOwned>(request: &mut tiny_http::Request) -> Result<T, Reply> {
    let mut bytes = Vec::new();
    request
        .as_reader()
        .take(16_385)
        .read_to_end(&mut bytes)
        .map_err(|_| error("read_failed", 400))?;
    if bytes.len() > 16_384 {
        return Err(error("request_too_large", 413));
    }
    serde_json::from_slice(&bytes).map_err(|_| error("invalid_request", 400))
}
fn enabled(player: Option<&Arc<PlayerState>>) -> Result<&Arc<PlayerState>, Reply> {
    player
        .filter(|p| p.config.render_dispatcher.is_some())
        .ok_or_else(|| error("not_found", 404))
}

pub(super) fn get(player: Option<&Arc<PlayerState>>, path: &str) -> Reply {
    let state = match enabled(player) {
        Ok(s) => s,
        Err(r) => return r,
    };
    let id = path.trim_start_matches("/v1/render/jobs/");
    match state.jobs.lock().unwrap().status(id) {
        Ok(status) => reply(200, serde_json::to_value(status).unwrap()),
        Err(e) => job_error(e),
    }
}

pub(super) fn post(
    request: &mut tiny_http::Request,
    replays_dir: &Path,
    player: Option<&Arc<PlayerState>>,
    port: u16,
    path: &str,
) -> Reply {
    let state = match enabled(player) {
        Ok(s) => s,
        Err(r) => return r,
    };
    let host = request
        .headers()
        .iter()
        .find(|h| h.field.equiv("Host"))
        .map(|h| h.value.as_str());
    let origin = request
        .headers()
        .iter()
        .find(|h| h.field.equiv("Origin"))
        .map(|h| h.value.as_str());
    if !host
        .zip(origin)
        .is_some_and(|(h, o)| o == format!("http://{h}"))
    {
        return error("same_origin_player_required", 403);
    }
    if path == "/v1/render/jobs" {
        let input: RenderRequest = match read_json(request) {
            Ok(v) => v,
            Err(r) => return r,
        };
        // Job identity alone is not authorization to post to Discord.
        if input.share_id.is_some() {
            return error("share_authorization_unavailable", 409);
        }
        let replay = match resolve_safe_path(replays_dir, &input.replay_name) {
            Ok(p) => p,
            Err(_) => return error("invalid_replay", 400),
        };
        if !replay
            .extension()
            .is_some_and(|e| e.eq_ignore_ascii_case("wowsreplay"))
            || !replay.is_file()
        {
            return error("replay_not_found", 404);
        }
        if !state
            .meta_for(replays_dir, &input.replay_name)
            .is_some_and(|meta| meta.complete)
        {
            return error("replay_not_ready", 409);
        }
        let result = state.jobs.lock().unwrap().start(input);
        return match result {
            Ok(StartRender::Existing(status)) => reply(200, serde_json::to_value(status).unwrap()),
            Ok(StartRender::New { status, worker }) => {
                if (state.config.render_dispatcher.as_ref().unwrap())(
                    port,
                    worker.clone(),
                    state.jobs.clone(),
                )
                .is_err()
                {
                    let _ = state
                        .jobs
                        .lock()
                        .unwrap()
                        .fail(&worker, "worker_start_failed");
                    return error("worker_start_failed", 500);
                }
                reply(202, serde_json::to_value(status).unwrap())
            }
            Err(e) => job_error(e),
        };
    }
    let Some((id, action)) = path
        .strip_prefix("/v1/render/jobs/")
        .and_then(|s| s.split_once('/'))
    else {
        return error("not_found", 404);
    };
    if action == "cancel" {
        return match state.jobs.lock().unwrap().cancel(id) {
            Ok(()) => reply(200, serde_json::json!({"ok":true})),
            Err(e) => job_error(e),
        };
    }
    let token = request
        .headers()
        .iter()
        .find(|h| h.field.equiv("X-TFD-Render-Worker"))
        .map(|h| h.value.as_str().to_string())
        .unwrap_or_default();
    let lease = WorkerLease {
        job_id: id.into(),
        token,
    };
    // Authenticate before reading potentially large bodies.
    let input = match state.jobs.lock().unwrap().request_for_worker(&lease) {
        Ok(v) => v,
        Err(e) => return job_error(e),
    };
    match action {
        "metadata" => {
            let info=match read_json(request) {Ok(v)=>v,Err(r)=>return r};
            match state.jobs.lock().unwrap().video_info(&lease,info) {
                Ok(())=>reply(200,serde_json::json!({"ok":true})),Err(e)=>job_error(e)
            }
        },
        "claim" => match state.jobs.lock().unwrap().claim(&lease) {
            Ok(v) => reply(200, serde_json::to_value(v).unwrap()),
            Err(e) => job_error(e),
        },
        "progress" => {
            #[derive(serde::Deserialize)]
            struct Progress {
                attempt: u8,
                frame: u32,
                total: u32,
            }
            let value: Progress = match read_json(request) {
                Ok(v) => v,
                Err(r) => return r,
            };
            match state.jobs.lock().unwrap().progress(
                &lease,
                value.attempt,
                value.frame,
                value.total,
            ) {
                Ok(()) => reply(200, serde_json::json!({"ok":true})),
                Err(e) => job_error(e),
            }
        }
        "stopped" => match state.jobs.lock().unwrap().stopped(&lease) {
            Ok(()) => reply(200, serde_json::json!({"ok":true})),
            Err(e) => job_error(e),
        },
        "fail" => {
            #[derive(serde::Deserialize)]
            struct Failure {
                error: String,
            }
            let value: Failure = match read_json(request) {
                Ok(v) => v,
                Err(r) => return r,
            };
            match state.jobs.lock().unwrap().fail(&lease, &value.error) {
                Ok(()) => reply(200, serde_json::json!({"ok":true})),
                Err(e) => job_error(e),
            }
        }
        "video" => {
            if state.jobs.lock().unwrap().status(id).unwrap().state != RenderState::Rendering {
                return error("job_not_rendering", 409);
            }
            let max = input.max_bytes.unwrap_or(512 * 1024 * 1024);
            let mut bytes = Vec::new();
            if request
                .as_reader()
                .take(max + 1)
                .read_to_end(&mut bytes)
                .is_err()
            {
                return error("read_failed", 400);
            }
            if bytes.len() as u64 > max {
                return error("exceeds_max_bytes", 413);
            }
            if bytes.get(4..8) != Some(b"ftyp") {
                return error("invalid_mp4", 400);
            }
            let dir = render_output_dir();
            if std::fs::create_dir_all(&dir).is_err() {
                return error("save_failed", 500);
            }
            let filename = format!("replay-{id}.mp4");
            let output = match persist_render(&dir, &filename, &bytes) {
                Ok(p) => p,
                Err(_) => return error("save_failed", 500),
            };
            // Cancellation can arrive from the native worker owner during I/O.
            let result =
                state
                    .jobs
                    .lock()
                    .unwrap()
                    .rendered(&lease, output.clone(), bytes.len() as u64);
            if let Err(e) = result {
                let _ = std::fs::remove_file(&output); // file created exclusively above
                return job_error(e);
            }
            reply(200, serde_json::json!({"bytes":bytes.len()}))
        }
        _ => error("not_found", 404),
    }
}
