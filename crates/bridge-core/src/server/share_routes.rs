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
        let body = r#"{"share_id":"share-123","capability":"test-capability","arena_unique_id":"9007199254740993","max_bytes":19922944}"#;
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
        std::fs::write(&replay, b"fixture").unwrap();
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
            for _ in 0..3 {
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
            .unwrap()
            .into_string()
            .unwrap();
        let start: serde_json::Value = serde_json::from_str(&start).unwrap();
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
    if (path == "/v1/share/authorize" && !local) || (path != "/v1/share/authorize" && !engine) {
        return error(403, "forbidden");
    }
    let mut bytes = Vec::new();
    if req.as_reader().take(16385).read_to_end(&mut bytes).is_err() {
        return error(400, "read_failed");
    }
    if bytes.len() > 16384 {
        return error(413, "request_too_large");
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
        // Bind to the selected on-disk scene; don't trust a supplied battle ID.
        let Ok(replay) = resolve_safe_path(dir, &input.replay_name) else {
            return error(400, "invalid_replay");
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
                    serde_json::json!({"authorization_url":start.url,"state":start.state,"opened":opened}),
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
        claim.replay_name = match player
            .share_auth
            .lock()
            .unwrap()
            .claim(state, &claim.arena_unique_id)
        {
            Ok(name) => Some(name),
            Err(_) => return error(403, "invalid_state"),
        };
    }
    let requested = claim.replay_name.clone();
    let arena = claim.arena_unique_id.clone();
    let dir = dir.to_path_buf();
    let owner = player.clone();
    // Resolve off the HTTP thread. The worker checks the decoded arena again.
    let resolve = Box::new(move || {
        let names = if let Some(name) = requested {
            vec![name]
        } else {
            let mut entries = list_replays(&dir).map_err(|_| "replay_unavailable".to_string())?;
            entries.sort_by_key(|e| std::cmp::Reverse(e.modified_ms));
            entries
                .into_iter()
                .filter(|e| e.name.ends_with(".wowsreplay"))
                .map(|e| e.name)
                .collect()
        };
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
