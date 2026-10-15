//! Local worker routes. Remote Engine start remains disabled until the
//! authorization handoff contract is implemented; no share is authorized here.
use super::*;
use crate::render_jobs::{
    JobError, RenderRequest, RenderState, StartRender, WorkerLease, STREAM_MAX_BYTES,
};
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
            let meta = state.meta_for(replays_dir, &input.replay_name);
            let ship = meta.as_ref().and_then(|m| {
                replays_dir
                    .parent()
                    .and_then(|game_dir| scene_export::ship_display_name(game_dir, &m.player_vehicle))
            });
            let filename = render_file_name(meta.as_ref(), ship.as_deref(), id);
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
        "video-chunk" => {
            let position = request
                .headers()
                .iter()
                .find(|h| h.field.equiv("X-TFD-Chunk-Position"))
                .and_then(|h| h.value.as_str().parse::<u64>().ok());
            let Some(position) = position else {
                return error("invalid_position", 400);
            };
            let partial = {
                let mut jobs = state.jobs.lock().unwrap();
                match jobs.partial_path(&lease, &render_output_dir()) {
                    Ok(p) => p,
                    Err(JobError::InvalidTransition) => return error("job_not_rendering", 409),
                    Err(e) => return job_error(e),
                }
            };
            let mut bytes = Vec::new();
            if request
                .as_reader()
                .take(CHUNK_MAX_BYTES + 1)
                .read_to_end(&mut bytes)
                .is_err()
            {
                return error("read_failed", 400);
            }
            if let Some(code) = chunk_rejection(bytes.len() as u64, position) {
                return error(code, 413);
            }
            match write_chunk(&partial, position, &bytes) {
                Ok(()) => reply(200, serde_json::json!({"ok":true})),
                Err(_) => error("save_failed", 500),
            }
        }
        "video-finish" => {
            let partial = {
                let mut jobs = state.jobs.lock().unwrap();
                match jobs.partial_path(&lease, &render_output_dir()) {
                    Ok(p) => p,
                    Err(JobError::InvalidTransition) => return error("job_not_rendering", 409),
                    Err(e) => return job_error(e),
                }
            };
            let meta = state.meta_for(replays_dir, &input.replay_name);
            let ship = meta.as_ref().and_then(|m| {
                replays_dir
                    .parent()
                    .and_then(|game_dir| scene_export::ship_display_name(game_dir, &m.player_vehicle))
            });
            let filename = render_file_name(meta.as_ref(), ship.as_deref(), id);
            let (output, size) = match finish_partial(&partial, &render_output_dir(), &filename) {
                Ok(v) => v,
                Err(e) if e.kind() == std::io::ErrorKind::InvalidData => {
                    return error("invalid_mp4", 400)
                }
                Err(_) => return error("save_failed", 500),
            };
            let result = state.jobs.lock().unwrap().rendered(&lease, output.clone(), size);
            if let Err(e) = result {
                let _ = std::fs::remove_file(&output); // created exclusively above
                return job_error(e);
            }
            reply(200, serde_json::json!({"bytes":size}))
        }
        _ => error("not_found", 404),
    }
}

const CHUNK_MAX_BYTES: u64 = 32 * 1024 * 1024;

fn chunk_rejection(len: u64, position: u64) -> Option<&'static str> {
    if len > CHUNK_MAX_BYTES {
        Some("chunk_too_large")
    } else if position.saturating_add(len) > STREAM_MAX_BYTES {
        Some("exceeds_max_bytes")
    } else {
        None
    }
}

/// Writes `bytes` at `position`, creating the partial file on first use.
fn write_chunk(path: &Path, position: u64, bytes: &[u8]) -> std::io::Result<()> {
    use std::io::{Seek, SeekFrom, Write};
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let mut file = std::fs::OpenOptions::new().write(true).create(true).truncate(false).open(path)?;
    file.seek(SeekFrom::Start(position))?;
    file.write_all(bytes)
}

/// Checks the MP4 signature, then moves the partial file onto a fresh, collision-free
/// name in `dir`. Returns the final path and size. The partial file is kept on error.
fn finish_partial(partial: &Path, dir: &Path, filename: &str) -> std::io::Result<(PathBuf, u64)> {
    use std::io::{Error, ErrorKind, Read};
    let mut file = std::fs::File::open(partial)?;
    let size = file.metadata()?.len();
    let mut head = [0u8; 8];
    if file.read_exact(&mut head).is_err() || &head[4..8] != b"ftyp" {
        return Err(Error::new(ErrorKind::InvalidData, "not an mp4"));
    }
    drop(file);
    std::fs::OpenOptions::new().write(true).open(partial)?.sync_all()?;
    let (output, reserved) = reserve_render_file(dir, filename)?;
    drop(reserved);
    if let Err(e) = std::fs::rename(partial, &output) {
        let _ = std::fs::remove_file(&output);
        return Err(e);
    }
    Ok((output, size))
}

const NAME_PART_MAX: usize = 40;

/// Keeps letters, digits, `_` and `-`; whitespace becomes `-`; the rest is dropped.
fn sanitize_name_part(value: &str) -> String {
    let mut out = String::new();
    for c in value.chars() {
        let c = if c.is_whitespace() { '-' } else { c };
        if (c.is_alphanumeric() || c == '_' || c == '-') && !(c == '-' && out.ends_with('-')) {
            out.push(c);
        }
    }
    out.trim_matches('-').chars().take(NAME_PART_MAX).collect::<String>().trim_matches('-').to_string()
}

/// `spaces/22_tierra_del_fuego` -> `Tierra-del-Fuego` (mirrors the RePlayer's `readableName`).
fn readable_map_name(map_name: &str) -> String {
    let last = map_name.rsplit(['/', '\\']).next().unwrap_or(map_name);
    let digits = last.chars().take_while(char::is_ascii_digit).count();
    let mut rest = last;
    if digits > 0 && last[digits..].starts_with('_') {
        rest = &last[digits + 1..];
        let lower = rest.to_ascii_lowercase();
        if lower.starts_with("ne_") || lower.starts_with("oc_") {
            rest = &rest[3..];
        }
    }
    let words: Vec<String> = rest
        .split(['-', '_', ' '])
        .filter(|w| !w.is_empty())
        .enumerate()
        .map(|(i, w)| {
            // Connector words stay lowercase ("Tierra-del-Fuego"), unlike the RePlayer.
            if i > 0 && ["del", "de", "of", "the", "la"].contains(&w.to_ascii_lowercase().as_str()) {
                return w.to_ascii_lowercase();
            }
            let mut chars = w.chars();
            chars.next().map(|f| f.to_uppercase().chain(chars).collect()).unwrap_or_default()
        })
        .collect();
    sanitize_name_part(&words.join("-"))
}

/// Parses strict `DD.MM.YYYY HH:MM:SS` into `YYYY-MM-DD_HH-MM`.
fn date_name_part(date_time: &str) -> Option<String> {
    let (date, time) = date_time.split_once(' ')?;
    let nums = |s: &str, sep: char, widths: &[usize]| -> Option<Vec<String>> {
        let parts: Vec<&str> = s.split(sep).collect();
        (parts.len() == widths.len()
            && parts.iter().zip(widths).all(|(p, w)| p.len() == *w && p.bytes().all(|b| b.is_ascii_digit())))
        .then(|| parts.iter().map(|p| p.to_string()).collect())
    };
    let d = nums(date, '.', &[2, 2, 4])?;
    let t = nums(time, ':', &[2, 2, 2])?;
    Some(format!("{}-{}-{}_{}-{}", d[2], d[1], d[0], t[0], t[1]))
}

/// Readable `<date>_<Ship>_<Map>.mp4`; any missing part falls back to `replay-<id>.mp4`.
fn render_file_name(meta: Option<&scene_export::ReplayMeta>, ship: Option<&str>, id: &str) -> String {
    let fallback = || format!("replay-{id}.mp4");
    let Some(meta) = meta else { return fallback() };
    let Some(date) = date_name_part(&meta.date_time) else { return fallback() };
    let ship = ship
        .map(str::to_string)
        .or_else(|| meta.player_vehicle.split_once('-').map(|(_, n)| n.to_string()))
        .map(|s| sanitize_name_part(&s))
        .unwrap_or_default();
    let map = readable_map_name(&meta.map_name);
    if ship.is_empty() || map.is_empty() {
        return fallback();
    }
    format!("{date}_{ship}_{map}.mp4")
}

#[cfg(test)]
mod stream_tests {
    use super::*;

    fn temp() -> PathBuf {
        let dir = std::env::temp_dir().join(format!("tfd-stream-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn chunks_are_written_at_their_position_including_a_patch() {
        let dir = temp();
        let part = dir.join("j.mp4.part");
        write_chunk(&part, 0, b"\0\0\0\0ftypXXXX").unwrap();
        write_chunk(&part, 12, b"tail").unwrap();
        write_chunk(&part, 0, b"\0\0\0\x10").unwrap(); // out-of-order patch at the start
        assert_eq!(std::fs::read(&part).unwrap(), b"\0\0\0\x10ftypXXXXtail");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn finish_checks_ftyp_and_never_overwrites() {
        let dir = temp();
        let part = dir.join("j.mp4.part");
        assert!(finish_partial(&part, &dir, "a.mp4").is_err()); // missing
        write_chunk(&part, 0, b"notmp4data").unwrap();
        assert_eq!(finish_partial(&part, &dir, "a.mp4").unwrap_err().kind(), std::io::ErrorKind::InvalidData);
        assert!(part.exists());
        std::fs::write(dir.join("a.mp4"), b"existing").unwrap();
        std::fs::write(dir.join("a-1.mp4"), b"existing").unwrap();
        write_chunk(&part, 0, b"\0\0\0\x10ftypdata").unwrap();
        let (out, size) = finish_partial(&part, &dir, "a.mp4").unwrap();
        assert_eq!(out, dir.join("a-2.mp4"));
        assert_eq!(size, 12);
        assert!(!part.exists());
        assert_eq!(std::fs::read(dir.join("a.mp4")).unwrap(), b"existing");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn limits_are_as_specified() {
        assert_eq!(chunk_rejection(CHUNK_MAX_BYTES, 0), None);
        assert_eq!(chunk_rejection(CHUNK_MAX_BYTES + 1, 0), Some("chunk_too_large"));
        assert_eq!(chunk_rejection(10, STREAM_MAX_BYTES - 10), None);
        assert_eq!(chunk_rejection(11, STREAM_MAX_BYTES - 10), Some("exceeds_max_bytes"));
        assert_eq!(chunk_rejection(1, u64::MAX), Some("exceeds_max_bytes"));
    }
}

#[cfg(test)]
mod name_tests {
    use super::*;

    fn meta(date: &str, map: &str, vehicle: &str) -> scene_export::ReplayMeta {
        scene_export::ReplayMeta {
            battle_type: None,
            game_version_short: None,
            map_name: map.into(),
            date_time: date.into(),
            player_name: String::new(),
            player_vehicle: vehicle.into(),
            complete: true,
            header: None,
            battle_results: None,
        }
    }

    #[test]
    fn normal() {
        let m = meta("10.10.2026 19:59:03", "spaces/22_tierra_del_fuego", "PVSB010-Libertad");
        assert_eq!(render_file_name(Some(&m), Some("Libertad"), "j"), "2026-10-10_19-59_Libertad_Tierra-del-Fuego.mp4");
    }

    #[test]
    fn ship_falls_back_to_vehicle_suffix() {
        let m = meta("10.10.2026 19:59:03", "spaces/22_tierra_del_fuego", "PVSB010-Libertad");
        assert_eq!(render_file_name(Some(&m), None, "j"), "2026-10-10_19-59_Libertad_Tierra-del-Fuego.mp4");
    }

    #[test]
    fn map_prefixes() {
        assert_eq!(readable_map_name("spaces/15_NE_north"), "North");
        assert_eq!(readable_map_name("spaces/04_OC_islands"), "Islands");
        assert_eq!(readable_map_name("spaces/10_NE_big_race"), "Big-Race");
    }

    #[test]
    fn ship_names_are_sanitized() {
        let m = meta("10.10.2026 19:59:03", "spaces/22_tierra_del_fuego", "X-Y");
        assert!(render_file_name(Some(&m), Some("Fusō"), "j").contains("_Fusō_"));
        assert!(render_file_name(Some(&m), Some("Orion\u{a0}'44"), "j").contains("_Orion-44_"));
    }

    #[test]
    fn missing_or_bad_data_falls_back() {
        let ok = meta("10.10.2026 19:59:03", "spaces/22_tierra_del_fuego", "A-B");
        assert_eq!(render_file_name(None, Some("B"), "j"), "replay-j.mp4");
        for bad in ["", "2026-10-10 19:59:03", "10.10.2026", "1.10.2026 19:59:03", "10.10.2026 19:59", "aa.bb.cccc dd:ee:ff"] {
            let mut m = ok.clone();
            m.date_time = bad.into();
            assert_eq!(render_file_name(Some(&m), Some("B"), "j"), "replay-j.mp4", "{bad}");
        }
    }

    #[test]
    fn long_parts_are_capped() {
        let m = meta("10.10.2026 19:59:03", &format!("spaces/1_{}", "a".repeat(200)), "A-B");
        let name = render_file_name(Some(&m), Some(&"s".repeat(200)), "j");
        assert!(name.len() < 120, "{name}");
    }
}
