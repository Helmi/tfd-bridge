use super::*;
use crate::share_service::ShareClaim;
type Reply = Response<std::io::Cursor<Vec<u8>>>;
fn json(status: u16, value: serde_json::Value) -> Reply {
    make_json_response(StatusCode(status), &value.to_string(), None)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::share_service::{ReplayResolver, ShareService};
    struct FakeService;
    impl ShareService for FakeService {
        fn open_authorization(&self, url: &str) -> bool {
            assert!(url.starts_with("https://engine.tfd.rocks/share/video/authorize?state="));
            true
        }
        fn preflight(
            &self,
            _replay_name: String,
            _arena_unique_id: String,
            evidence: crate::replay_identity::ReplayEvidence,
        ) -> Result<crate::share_service::SharePreflight, String> {
            Ok(crate::share_service::SharePreflight {
                preflight_id: "preflight-test".into(),
                recorder_account_id: evidence.recorder.account_id.to_string(),
                recorder_realm: None,
            })
        }
        fn start(
            &self,
            claim: ShareClaim,
            _resolve: ReplayResolver,
            _port: u16,
            _jobs: Arc<Mutex<crate::render_jobs::RenderJobs>>,
        ) -> Result<String, String> {
            assert_eq!(claim.capability, "test-capability");
            assert_eq!(claim.arena_unique_id, "9007199254740993");
            Ok("test-job".into())
        }
        fn status(&self, _id: &str) -> Option<serde_json::Value> {
            Some(serde_json::json!({"state":"queued"}))
        }
    }
    #[test]
    fn engine_start_cors_state_guards_and_public_status() {
        let dir = tempfile::tempdir().unwrap();
        let bridge = start_on_ports_full(
            dir.path().into(),
            None,
            &[0],
            None,
            None,
            Some(PlayerConfig {
                player_dist: dir.path().into(),
                render_dispatcher: None,
                share_service: Some(Arc::new(FakeService)),
            }),
        )
        .unwrap();
        let base = format!("http://127.0.0.1:{}", bridge.port());
        let preflight = ureq::request("OPTIONS", &format!("{base}/v1/share/claim"))
            .set("Origin", "https://engine.tfd.rocks")
            .set("Access-Control-Request-Method", "POST")
            .set("Access-Control-Request-Headers", "content-type")
            .set("Access-Control-Request-Private-Network", "true")
            .call()
            .unwrap();
        assert_eq!(
            preflight.header("Access-Control-Allow-Methods"),
            Some("GET, POST, OPTIONS")
        );
        assert_eq!(
            preflight.header("Access-Control-Allow-Private-Network"),
            Some("true")
        );
        assert_eq!(
            preflight.header("Access-Control-Allow-Origin"),
            Some("https://engine.tfd.rocks")
        );
        let body = r#"{"share_id":"share-123","capability":"test-capability","arena_unique_id":"9007199254740993","max_bytes":19922944,"preflight_id":"preflight-test"}"#;
        for origin in ["https://evil.test", base.as_str()] {
            assert!(matches!(
                ureq::post(&format!("{base}/v1/share/claim"))
                    .set("Origin", origin)
                    .send_string(body),
                Err(ureq::Error::Status(403, _))
            ));
        }
        assert!(matches!(
            ureq::post(&format!("{base}/v1/share/claim"))
                .set("Origin", "https://engine.tfd.rocks")
                .send_string(body),
            Err(ureq::Error::Status(400, _))
        ));
        let result = ureq::post(&format!("{base}/v1/render/jobs"))
            .set("Origin", "https://engine.tfd.rocks")
            .send_string(body)
            .unwrap();
        assert_eq!(result.status(), 202);
        let response = result.into_string().unwrap();
        assert!(response.contains("test-job"));
        assert!(!response.contains("test-capability"));
        let status = ureq::get(&format!("{base}/v1/share/status/test-job"))
            .call()
            .unwrap()
            .into_string()
            .unwrap();
        assert!(status.contains("queued"));
        assert!(!status.contains("capability"));
    }
    #[test]
    fn replayer_authorization_claim_consumes_matching_state_once() {
        let dir = tempfile::tempdir().unwrap();
        let replay = dir.path().join("test.wowsreplay");
        let metadata = serde_json::json!({"gameMode":1,"clientVersionFromExe":"0,15,7,0","mapDisplayName":"Test","mapId":1,"clientVersionFromXml":"0,15,7,0","duration":1,"name":"test","scenario":"test","playerID":42,"vehicles":[],"playersPerTeam":1,"dateTime":"2026-01-01T00:00:00","mapName":"test","playerName":"test","scenarioConfigId":1,"teamsCount":2,"playerVehicle":"test"});
        let meta = serde_json::to_vec(&metadata).unwrap();
        let mut bytes = Vec::new();
        bytes.extend_from_slice(&0x1234_5678u32.to_le_bytes());
        bytes.extend_from_slice(&1u32.to_le_bytes());
        bytes.extend_from_slice(&(meta.len() as u32).to_le_bytes());
        bytes.extend_from_slice(&meta);
        std::fs::write(&replay, bytes).unwrap();
        let player = Arc::new(PlayerState::new(PlayerConfig {
            player_dist: dir.path().into(),
            render_dispatcher: None,
            share_service: Some(Arc::new(FakeService)),
        }));
        player.cache.lock().unwrap().insert(
            file_key(&replay.canonicalize().unwrap()).unwrap(),
            r#"{"replay":{"arenaUniqueId":"9007199254740993"}}"#.into(),
        );
        let server = Server::http("127.0.0.1:0").unwrap();
        let port = server.server_addr().to_ip().unwrap().port();
        let base = format!("http://127.0.0.1:{port}");
        let root = dir.path().to_path_buf();
        let worker = std::thread::spawn(move || {
            for _ in 0..6 {
                let mut request = server.recv().unwrap();
                let path = request.url().to_string();
                let response = post(&mut request, &root, Some(&player), port, &path);
                request.respond(response).unwrap();
            }
        });
        let start = ureq::post(&format!("{base}/v1/share/authorize"))
            .set("Origin", &base)
            .send_string(
                r#"{"replay_name":"test.wowsreplay","arena_unique_id":"9007199254740993"}"#,
            )
            .unwrap();
        let start = start.into_string().unwrap();
        let start: serde_json::Value = serde_json::from_str(&start).unwrap();
        assert_eq!(start["opened"], true);
        let reopen = serde_json::json!({"state":start["state"]}).to_string();
        // Engine pages may claim a share, but cannot trigger browser popups.
        assert!(matches!(
            ureq::post(&format!("{base}/v1/share/authorize/open"))
                .set("Origin", "https://engine.tfd.rocks")
                .send_string(&reopen),
            Err(ureq::Error::Status(403, _))
        ));
        assert!(matches!(
            ureq::post(&format!("{base}/v1/share/authorize/open"))
                .set("Origin", &base)
                .send_string(r#"{"state":"wrong","url":"https://evil.test"}"#),
            Err(ureq::Error::Status(403, _))
        ));
        let opened = ureq::post(&format!("{base}/v1/share/authorize/open"))
            .set("Origin", &base)
            .send_string(&reopen)
            .unwrap()
            .into_string()
            .unwrap();
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&opened).unwrap()["opened"],
            true
        );
        // Reopening must not consume the original callback state.
        let claim=serde_json::json!({"state":start["state"],"share_id":"share-123","capability":"test-capability","arena_unique_id":"9007199254740993","max_bytes":19922944}).to_string();
        assert_eq!(
            ureq::post(&format!("{base}/v1/share/claim"))
                .set("Origin", "https://engine.tfd.rocks")
                .send_string(&claim)
                .unwrap()
                .status(),
            202
        );
        assert!(matches!(
            ureq::post(&format!("{base}/v1/share/claim"))
                .set("Origin", "https://engine.tfd.rocks")
                .send_string(&claim),
            Err(ureq::Error::Status(403, _))
        ));
        worker.join().unwrap();
    }
}
fn error(status: u16, code: &str) -> Reply {
    json(status, serde_json::json!({"error":code}))
}

pub(super) fn status(player: Option<&Arc<PlayerState>>, path: &str) -> Reply {
    let Some(value) = player
        .and_then(|p| p.config.share_service.as_ref())
        .and_then(|s| s.status(path.trim_start_matches("/v1/share/status/")))
    else {
        return error(404, "not_found");
    };
    json(200, value)
}

pub(super) fn post(
    req: &mut tiny_http::Request,
    dir: &Path,
    player: Option<&Arc<PlayerState>>,
    port: u16,
    path: &str,
) -> Reply {
    let Some(player) = player.filter(|p| p.config.share_service.is_some()) else {
        return error(404, "sharing_unavailable");
    };
    let origin = req
        .headers()
        .iter()
        .find(|h| h.field.equiv("Origin"))
        .map(|h| h.value.as_str());
    let host = req
        .headers()
        .iter()
        .find(|h| h.field.equiv("Host"))
        .map(|h| h.value.as_str());
    let local = origin
        .zip(host)
        .is_some_and(|(o, h)| o == format!("http://{h}"));
    let engine = origin == Some("https://engine.tfd.rocks");
    let local_authorization = matches!(path, "/v1/share/authorize" | "/v1/share/authorize/open");
    if (local_authorization && !local) || (!local_authorization && !engine) {
        return error(403, "forbidden");
    }
    let mut bytes = Vec::new();
    if req.as_reader().take(16385).read_to_end(&mut bytes).is_err() {
        return error(400, "read_failed");
    }
    if bytes.len() > 16384 {
        return error(413, "request_too_large");
    }
    if path == "/v1/share/authorize/open" {
        #[derive(serde::Deserialize)]
        struct Input {
            state: String,
        }
        let Ok(input) = serde_json::from_slice::<Input>(&bytes) else {
            return error(400, "invalid_request");
        };
        let Ok(url) = player
            .share_auth
            .lock()
            .unwrap()
            .authorization_url(&input.state, port)
        else {
            return error(403, "invalid_state");
        };
        let opened = player
            .config
            .share_service
            .as_ref()
            .unwrap()
            .open_authorization(&url);
        return json(200, serde_json::json!({"opened":opened}));
    }
    if path == "/v1/share/preflight" {
        #[derive(serde::Deserialize)]
        struct Input {
            replay_name: String,
            arena_unique_id: String,
        }
        let Ok(input) = serde_json::from_slice::<Input>(&bytes) else {
            return error(400, "invalid_request");
        };
        let Ok(replay) = resolve_safe_path(dir, &input.replay_name)
            .and_then(|p| p.canonicalize().map_err(|_| "invalid_replay".to_string()))
        else {
            return error(400, "invalid_replay");
        };
        let evidence = match crate::replay_identity::read_replay_evidence(&replay) {
            Ok(value) => value,
            Err(crate::replay_identity::ReplayIdentityError::Missing) => {
                return error(409, "recorder_identity_missing")
            }
            Err(crate::replay_identity::ReplayIdentityError::Ambiguous) => {
                return error(409, "recorder_identity_ambiguous")
            }
            Err(_) => return error(409, "replay_unavailable"),
        };
        let Ok(body) = scene_cached(&replay, dir, player) else {
            return error(409, "replay_unavailable");
        };
        let Ok(scene) = serde_json::from_str::<serde_json::Value>(&body) else {
            return error(409, "replay_unavailable");
        };
        if scene["replay"]["arenaUniqueId"].as_str() != Some(&input.arena_unique_id) {
            return error(409, "battle_mismatch");
        }
        return match player.config.share_service.as_ref().unwrap().preflight(
            input.replay_name,
            input.arena_unique_id,
            evidence,
        ) {
            Ok(value) => json(200, serde_json::to_value(value).unwrap()),
            Err(code) => error(409, &code),
        };
    }
    if path == "/v1/share/authorize" {
        #[derive(serde::Deserialize)]
        struct Input {
            replay_name: String,
            arena_unique_id: String,
        }
        let Ok(input) = serde_json::from_slice::<Input>(&bytes) else {
            return error(400, "invalid_request");
        };
        // Establish recorder evidence from this exact canonical artifact before
        // decoding a scene. Names, selected entities and result blobs are never used.
        let Ok(replay) = resolve_safe_path(dir, &input.replay_name)
            .and_then(|p| p.canonicalize().map_err(|_| "invalid_replay".to_string()))
        else {
            return error(400, "invalid_replay");
        };
        let evidence = match crate::replay_identity::read_replay_evidence(&replay) {
            Ok(value) => value,
            Err(crate::replay_identity::ReplayIdentityError::Missing) => {
                return error(409, "recorder_identity_missing")
            }
            Err(crate::replay_identity::ReplayIdentityError::Ambiguous) => {
                return error(409, "recorder_identity_ambiguous")
            }
            Err(_) => return error(409, "replay_unavailable"),
        };
        let preflight = match player.config.share_service.as_ref().unwrap().preflight(
            input.replay_name.clone(),
            input.arena_unique_id.clone(),
            evidence,
        ) {
            Ok(value) => value,
            Err(code) => return error(409, &code),
        };
        let Ok(body) = scene_cached(&replay, dir, player) else {
            return error(409, "replay_unavailable");
        };
        let Ok(scene) = serde_json::from_str::<serde_json::Value>(&body) else {
            return error(409, "replay_unavailable");
        };
        if scene["replay"]["arenaUniqueId"].as_str() != Some(&input.arena_unique_id) {
            return error(409, "battle_mismatch");
        }
        return match player.share_auth.lock().unwrap().begin(
            input.replay_name,
            input.arena_unique_id,
            port,
            preflight.preflight_id.clone(),
        ) {
            Ok(start) => {
                let opened = player
                    .config
                    .share_service
                    .as_ref()
                    .unwrap()
                    .open_authorization(&start.url);
                json(
                    200,
                    serde_json::json!({"authorization_url":start.url,"state":start.state,"opened":opened,"preflight":preflight}),
                )
            }
            Err(_) => error(400, "invalid_request"),
        };
    }
    if path != "/v1/share/claim" && path != "/v1/render/jobs" {
        return error(404, "not_found");
    }
    let Ok(mut claim) = serde_json::from_slice::<ShareClaim>(&bytes) else {
        return error(400, "invalid_request");
    };
    if claim.arena_unique_id.is_empty()
        || claim.arena_unique_id.len() > 20
        || !claim.arena_unique_id.bytes().all(|b| b.is_ascii_digit())
    {
        return error(400, "invalid_battle");
    }
    if path == "/v1/share/claim" {
        let Some(state) = claim.state.as_deref() else {
            return error(400, "missing_state");
        };
        let bound = match player
            .share_auth
            .lock()
            .unwrap()
            .claim(state, &claim.arena_unique_id)
        {
            Ok(value) => value,
            Err(_) => return error(403, "invalid_state"),
        };
        claim.replay_name = Some(bound.replay_name);
        claim.preflight_id = Some(bound.preflight_id);
    }
    if claim.preflight_id.as_ref().is_none_or(|id| id.is_empty()) {
        return error(400, "missing_preflight");
    }
    let requested = claim.replay_name.clone();
    let arena = claim.arena_unique_id.clone();
    let dir = dir.to_path_buf();
    let owner = player.clone();
    // Resolve off the HTTP thread. The worker checks the decoded arena again.
    let resolve = Box::new(move || {
        let names = requested.into_iter().collect::<Vec<_>>();
        if names.len() != 1 {
            return Err("replay_not_bound".into());
        }
        for name in names {
            let Ok(path) = resolve_safe_path(&dir, &name) else {
                continue;
            };
            if !owner.meta_for(&dir, &name).is_some_and(|m| m.complete) {
                continue;
            }
            let Ok(body) = scene_cached(&path, &dir, &owner) else {
                continue;
            };
            let Ok(scene) = serde_json::from_str::<serde_json::Value>(&body) else {
                continue;
            };
            if scene["replay"]["arenaUniqueId"].as_str() == Some(&arena) {
                return Ok(name);
            }
        }
        Err("replay_not_found".into())
    });
    match player.config.share_service.as_ref().unwrap().start(
        claim,
        resolve,
        port,
        player.jobs.clone(),
    ) {
        Ok(id) => json(
            202,
            serde_json::json!({"id":id,"state":"authorizing","status_url":format!("/v1/share/status/{id}")}),
        ),
        Err(code) => error(409, &code),
    }
}
