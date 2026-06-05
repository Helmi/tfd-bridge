/// Local loopback bridge server.
///
/// Binds 127.0.0.1 starting at port 43210, falling back through 43211-43214
/// if the canonical port is occupied.  Serves read-only replay files from the
/// configured replays directory.
///
/// Endpoints
///   GET /v1/health             → JSON {name, version, capabilities}
///   GET /v1/replays            → JSON [{name, size, modified_ms}]
///   GET /v1/replays/latest     → JSON {name, size, modified_ms} or 404
///   GET /v1/replays/{name}     → file bytes
///
/// All responses include CORS headers that allow the canonical origin
/// `https://engine.tfd.rocks`.  A secondary `dev_origin` can be passed for
/// local development.  Requests from all other origins get no ACAO header.
use notify::{RecursiveMode, Watcher};
use serde::Serialize;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::{
    atomic::{AtomicU64, Ordering},
    Arc,
};
use std::thread;
use std::time::UNIX_EPOCH;
use tiny_http::{Header, Response, Server, StatusCode};

// ── Constants ──────────────────────────────────────────────────────────────────

pub const CANONICAL_PORT: u16 = 43210;
pub const FALLBACK_PORTS: [u16; 4] = [43211, 43212, 43213, 43214];
const PROD_ORIGIN: &str = "https://engine.tfd.rocks";

// ── Error ─────────────────────────────────────────────────────────────────────

#[derive(Debug, thiserror::Error)]
pub enum BridgeError {
    #[error("All bridge ports (43210-43214) are occupied")]
    AllPortsOccupied,
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
    #[error("Watcher error: {0}")]
    Watch(String),
}

// ── Public types ───────────────────────────────────────────────────────────────

/// A running bridge instance.  Drop or call [`Bridge::stop`] to shut it down.
pub struct Bridge {
    port: u16,
    server: Arc<Server>,
    /// Monotonically increasing counter incremented on every replay-dir change.
    generation: Arc<AtomicU64>,
    /// Keep the watcher alive as long as the bridge is alive.
    _watcher: notify::RecommendedWatcher,
}

impl Bridge {
    /// The port this bridge is actually bound to.
    pub fn port(&self) -> u16 {
        self.port
    }

    /// Current generation count (poll this to detect replay-dir changes).
    pub fn generation(&self) -> u64 {
        self.generation.load(Ordering::SeqCst)
    }

    /// Signal the HTTP server to stop and wait for the handler thread to exit.
    /// Consuming `self` ensures the watcher is also dropped.
    pub fn stop(self) {
        self.server.unblock();
    }
}

/// Per-replay-file metadata returned by the list endpoint.
#[derive(Debug, Clone, Serialize)]
pub struct ReplayEntry {
    pub name: String,
    pub size: u64,
    /// Milliseconds since UNIX epoch (for ordering by recency).
    pub modified_ms: u64,
}

// ── Startup ────────────────────────────────────────────────────────────────────

/// Start the bridge server.
///
/// - `replays_dir` — the validated replays directory to serve.
/// - `dev_origin`  — optional additional CORS origin (e.g. `http://localhost:3000`).
///
/// Tries to bind 43210 first, falls back through 43211-43214.
/// Returns an error only if all five ports are occupied.
pub fn start(replays_dir: PathBuf, dev_origin: Option<String>) -> Result<Bridge, BridgeError> {
    let candidates: Vec<u16> = std::iter::once(CANONICAL_PORT)
        .chain(FALLBACK_PORTS)
        .collect();
    start_on_ports(replays_dir, dev_origin, &candidates)
}

/// Internal start that accepts an explicit list of ports to try in order.
/// Port 0 means "let the OS pick" (used in tests to avoid collisions).
fn start_on_ports(
    replays_dir: PathBuf,
    dev_origin: Option<String>,
    ports: &[u16],
) -> Result<Bridge, BridgeError> {
    let (server, port) = bind_server(ports)?;

    log::info!("Bridge listening on http://127.0.0.1:{port}");

    let generation = Arc::new(AtomicU64::new(0));
    let gen_clone = Arc::clone(&generation);
    let dir_clone = replays_dir.clone();

    // File watcher: bump generation on any change in the replays dir.
    let mut watcher = notify::recommended_watcher(move |res: notify::Result<notify::Event>| {
        if let Ok(event) = res {
            if event.kind.is_create() || event.kind.is_modify() || event.kind.is_remove() {
                // Only care about .wowsreplay files
                let affects_replays = event.paths.iter().any(|p| {
                    p.extension()
                        .map(|ext| ext.eq_ignore_ascii_case("wowsreplay"))
                        .unwrap_or(false)
                });
                if affects_replays {
                    gen_clone.fetch_add(1, Ordering::SeqCst);
                }
            }
        }
    })
    .map_err(|e| BridgeError::Watch(e.to_string()))?;

    watcher
        .watch(&dir_clone, RecursiveMode::NonRecursive)
        .map_err(|e| BridgeError::Watch(e.to_string()))?;

    // Spawn handler thread.
    let server_clone = Arc::clone(&server);
    let gen_clone2 = Arc::clone(&generation);
    let allowed_origins: Vec<String> = {
        let mut v = vec![PROD_ORIGIN.to_string()];
        if let Some(ref o) = dev_origin {
            if !o.is_empty() {
                v.push(o.clone());
            }
        }
        v
    };

    thread::spawn(move || {
        handle_requests(&server_clone, &replays_dir, &allowed_origins, &gen_clone2);
    });

    Ok(Bridge {
        port,
        server,
        generation,
        _watcher: watcher,
    })
}

// ── Port binding ───────────────────────────────────────────────────────────────

/// Try each port in `ports` in order; return the first that succeeds.
/// Port 0 causes the OS to assign a free port.
fn bind_server(ports: &[u16]) -> Result<(Arc<Server>, u16), BridgeError> {
    for &port in ports {
        match Server::http(format!("127.0.0.1:{port}")) {
            Ok(srv) => {
                // When port=0 the OS assigned a port; read it back from the socket.
                let actual_port = if port == 0 {
                    srv.server_addr()
                        .to_ip()
                        .map(|a| a.port())
                        .unwrap_or(0)
                } else {
                    port
                };
                return Ok((Arc::new(srv), actual_port));
            }
            Err(_) => continue,
        }
    }
    Err(BridgeError::AllPortsOccupied)
}

// ── Request handler loop ───────────────────────────────────────────────────────

fn handle_requests(
    server: &Server,
    replays_dir: &Path,
    allowed_origins: &[String],
    generation: &AtomicU64,
) {
    loop {
        let request = match server.recv() {
            Ok(r) => r,
            Err(_) => break,
        };

        let origin = request
            .headers()
            .iter()
            .find(|h| h.field.equiv("Origin"))
            .map(|h| h.value.as_str().to_string());

        let cors_origin = origin
            .as_deref()
            .and_then(|o| {
                if allowed_origins.iter().any(|allowed| allowed == o) {
                    Some(o.to_string())
                } else {
                    None
                }
            });

        let path = request.url().to_string();
        // Strip query string for routing
        let path_no_qs = path.split('?').next().unwrap_or(&path);

        let response = match path_no_qs {
            "/v1/health" => handle_health(),
            "/v1/replays" => handle_list(replays_dir, generation),
            "/v1/replays/latest" => handle_latest(replays_dir),
            p if p.starts_with("/v1/replays/") => {
                let name = &p["/v1/replays/".len()..];
                handle_fetch(replays_dir, name)
            }
            _ => make_json_response(StatusCode(404), r#"{"error":"not found"}"#, None),
        };

        let response = attach_cors(response, cors_origin.as_deref());

        if let Err(e) = request.respond(response) {
            log::warn!("Bridge: failed to send response: {e}");
        }
    }
}

// ── Endpoint handlers ─────────────────────────────────────────────────────────

fn handle_health() -> Response<std::io::Cursor<Vec<u8>>> {
    #[derive(Serialize)]
    struct Health {
        name: &'static str,
        version: &'static str,
        capabilities: &'static [&'static str],
    }
    let body = serde_json::to_string(&Health {
        name: "tfd-bridge",
        version: crate::version(),
        capabilities: &["replays-v1"],
    })
    .unwrap_or_default();

    make_json_response(StatusCode(200), &body, None)
}

fn handle_list(
    replays_dir: &Path,
    generation: &AtomicU64,
) -> Response<std::io::Cursor<Vec<u8>>> {
    match list_replays(replays_dir) {
        Ok(entries) => {
            let gen = generation.load(Ordering::SeqCst);
            let body = serde_json::json!({
                "generation": gen,
                "replays": entries,
            })
            .to_string();
            make_json_response(StatusCode(200), &body, None)
        }
        Err(e) => make_json_response(
            StatusCode(500),
            &format!(r#"{{"error":"{}"}}"#, e),
            None,
        ),
    }
}

fn handle_latest(replays_dir: &Path) -> Response<std::io::Cursor<Vec<u8>>> {
    match list_replays(replays_dir) {
        Ok(mut entries) => {
            if entries.is_empty() {
                return make_json_response(StatusCode(404), r#"{"error":"no replays found"}"#, None);
            }
            entries.sort_by(|a, b| b.modified_ms.cmp(&a.modified_ms));
            let body = serde_json::to_string(&entries[0]).unwrap_or_default();
            make_json_response(StatusCode(200), &body, None)
        }
        Err(e) => make_json_response(
            StatusCode(500),
            &format!(r#"{{"error":"{}"}}"#, e),
            None,
        ),
    }
}

fn handle_fetch(replays_dir: &Path, name: &str) -> Response<std::io::Cursor<Vec<u8>>> {
    match resolve_safe_path(replays_dir, name) {
        Err(e) => make_json_response(StatusCode(400), &format!(r#"{{"error":"{}"}}"#, e), None),
        Ok(path) => {
            let mut file = match std::fs::File::open(&path) {
                Ok(f) => f,
                Err(_) => {
                    return make_json_response(StatusCode(404), r#"{"error":"not found"}"#, None)
                }
            };
            let mut buf = Vec::new();
            if file.read_to_end(&mut buf).is_err() {
                return make_json_response(StatusCode(500), r#"{"error":"read error"}"#, None);
            }
            let response = Response::from_data(buf).with_status_code(StatusCode(200));
            let header = Header::from_bytes(
                b"Content-Type",
                b"application/octet-stream",
            )
            .unwrap();
            response.with_header(header)
        }
    }
}

// ── Pure helpers ───────────────────────────────────────────────────────────────

/// List all `.wowsreplay` files in `dir`.
pub fn list_replays(dir: &Path) -> Result<Vec<ReplayEntry>, std::io::Error> {
    let mut entries = Vec::new();
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        if path
            .extension()
            .map(|ext| ext.eq_ignore_ascii_case("wowsreplay"))
            .unwrap_or(false)
        {
            let meta = entry.metadata()?;
            let modified_ms = meta
                .modified()
                .ok()
                .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
                .map(|d| d.as_millis() as u64)
                .unwrap_or(0);
            entries.push(ReplayEntry {
                name: path
                    .file_name()
                    .unwrap_or_default()
                    .to_string_lossy()
                    .into_owned(),
                size: meta.len(),
                modified_ms,
            });
        }
    }
    Ok(entries)
}

/// Resolve a request name to an absolute path that is confirmed to live inside
/// `replays_dir`.  Returns an error if the name contains any path separators,
/// is absolute, or would escape the directory.
pub fn resolve_safe_path(replays_dir: &Path, name: &str) -> Result<PathBuf, String> {
    // Reject multi-segment, absolute, or obviously traversal-flavoured names.
    if name.is_empty() {
        return Err("empty name".into());
    }
    if name.contains('/') || name.contains('\\') {
        return Err("name must be a single filename segment".into());
    }
    if Path::new(name).is_absolute() {
        return Err("absolute paths are not allowed".into());
    }
    if name.contains("..") {
        return Err("path traversal is not allowed".into());
    }

    // Canonicalize the replays dir and compute candidate.
    let canonical_dir = replays_dir
        .canonicalize()
        .map_err(|e| format!("replays dir not accessible: {e}"))?;
    let candidate = canonical_dir.join(name);

    // Verify the result is still inside the replays dir.
    // `starts_with` on a canonicalized parent is the safe check.
    if !candidate.starts_with(&canonical_dir) {
        return Err("path traversal is not allowed".into());
    }

    Ok(candidate)
}

// ── Response helpers ───────────────────────────────────────────────────────────

fn make_json_response(
    status: StatusCode,
    body: &str,
    _extra_headers: Option<Vec<Header>>,
) -> Response<std::io::Cursor<Vec<u8>>> {
    let data = body.as_bytes().to_vec();
    let content_type = Header::from_bytes(b"Content-Type", b"application/json").unwrap();
    Response::from_data(data)
        .with_status_code(status)
        .with_header(content_type)
}

fn attach_cors(
    response: Response<std::io::Cursor<Vec<u8>>>,
    allowed_origin: Option<&str>,
) -> Response<std::io::Cursor<Vec<u8>>> {
    if let Some(origin) = allowed_origin {
        let acao =
            Header::from_bytes(b"Access-Control-Allow-Origin", origin.as_bytes()).unwrap();
        let acam =
            Header::from_bytes(b"Access-Control-Allow-Methods", b"GET, OPTIONS").unwrap();
        response.with_header(acao).with_header(acam)
    } else {
        response
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::net::TcpListener;
    use tempfile::TempDir;

    // ── Pure helper tests ──────────────────────────────────────────────────────

    #[test]
    fn list_replays_empty_dir() {
        let tmp = TempDir::new().unwrap();
        let entries = list_replays(tmp.path()).unwrap();
        assert!(entries.is_empty());
    }

    #[test]
    fn list_replays_only_wowsreplay_files() {
        let tmp = TempDir::new().unwrap();
        fs::write(tmp.path().join("battle.wowsreplay"), b"data").unwrap();
        fs::write(tmp.path().join("other.txt"), b"ignored").unwrap();
        let entries = list_replays(tmp.path()).unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].name, "battle.wowsreplay");
    }

    #[test]
    fn resolve_safe_path_accepts_valid_name() {
        let tmp = TempDir::new().unwrap();
        let result = resolve_safe_path(tmp.path(), "battle.wowsreplay");
        assert!(result.is_ok());
        let p = result.unwrap();
        assert!(p.starts_with(tmp.path().canonicalize().unwrap()));
    }

    #[test]
    fn resolve_safe_path_rejects_traversal_dotdot() {
        let tmp = TempDir::new().unwrap();
        assert!(resolve_safe_path(tmp.path(), "../secret.txt").is_err());
    }

    #[test]
    fn resolve_safe_path_rejects_slash_in_name() {
        let tmp = TempDir::new().unwrap();
        assert!(resolve_safe_path(tmp.path(), "sub/dir.wowsreplay").is_err());
    }

    #[test]
    fn resolve_safe_path_rejects_backslash_in_name() {
        let tmp = TempDir::new().unwrap();
        assert!(resolve_safe_path(tmp.path(), "sub\\dir.wowsreplay").is_err());
    }

    #[test]
    fn resolve_safe_path_rejects_empty_name() {
        let tmp = TempDir::new().unwrap();
        assert!(resolve_safe_path(tmp.path(), "").is_err());
    }

    // ── Integration tests ─────────────────────────────────────────────────────

    /// Bind port 0 (OS-assigned) instead of a fixed port to avoid collisions
    /// between parallel tests or an already-running bridge.
    fn start_test_bridge(tmp_dir: &TempDir) -> Bridge {
        start_on_ports(tmp_dir.path().to_path_buf(), None, &[0])
            .expect("bridge start failed")
    }

    fn get(url: &str) -> (u16, String, Vec<(String, String)>) {
        let response = ureq::get(url)
            .set("Origin", "https://engine.tfd.rocks")
            .call();
        match response {
            Ok(resp) => {
                let status = resp.status();
                let headers: Vec<(String, String)> = resp
                    .headers_names()
                    .into_iter()
                    .filter_map(|name| {
                        resp.header(&name).map(|v| (name.to_lowercase(), v.to_string()))
                    })
                    .collect();
                let body = resp.into_string().unwrap_or_default();
                (status, body, headers)
            }
            Err(ureq::Error::Status(code, resp)) => {
                let body = resp.into_string().unwrap_or_default();
                (code, body, vec![])
            }
            Err(e) => panic!("request failed: {e}"),
        }
    }

    fn get_with_origin(url: &str, origin: &str) -> (u16, String, Vec<(String, String)>) {
        let response = ureq::get(url).set("Origin", origin).call();
        match response {
            Ok(resp) => {
                let status = resp.status();
                let headers: Vec<(String, String)> = resp
                    .headers_names()
                    .into_iter()
                    .filter_map(|name| {
                        resp.header(&name).map(|v| (name.to_lowercase(), v.to_string()))
                    })
                    .collect();
                let body = resp.into_string().unwrap_or_default();
                (status, body, headers)
            }
            Err(ureq::Error::Status(code, resp)) => {
                let body = resp.into_string().unwrap_or_default();
                (code, body, vec![])
            }
            Err(e) => panic!("request failed: {e}"),
        }
    }

    fn find_header(headers: &[(String, String)], name: &str) -> Option<String> {
        headers
            .iter()
            .find(|(k, _)| k == name)
            .map(|(_, v)| v.clone())
    }

    #[test]
    fn health_returns_200_with_json() {
        let tmp = TempDir::new().unwrap();
        let bridge = start_test_bridge(&tmp);
        let url = format!("http://127.0.0.1:{}/v1/health", bridge.port());
        let (status, body, _) = get(&url);
        assert_eq!(status, 200);
        let v: serde_json::Value = serde_json::from_str(&body).expect("valid JSON");
        assert_eq!(v["name"], "tfd-bridge");
        assert!(v["version"].as_str().is_some());
        assert!(v["capabilities"].is_array());
        // Health must NOT include a "port" field (per spec)
        assert!(v["port"].is_null(), "health must not report its own port");
        bridge.stop();
    }

    #[test]
    fn health_cors_allowed_origin() {
        let tmp = TempDir::new().unwrap();
        let bridge = start_test_bridge(&tmp);
        let url = format!("http://127.0.0.1:{}/v1/health", bridge.port());
        let (_, _, headers) = get(&url);
        let acao = find_header(&headers, "access-control-allow-origin");
        assert_eq!(
            acao.as_deref(),
            Some("https://engine.tfd.rocks"),
            "ACAO header must be set for allowed origin"
        );
        bridge.stop();
    }

    #[test]
    fn health_cors_rejected_for_unknown_origin() {
        let tmp = TempDir::new().unwrap();
        let bridge = start_test_bridge(&tmp);
        let url = format!("http://127.0.0.1:{}/v1/health", bridge.port());
        let (_, _, headers) = get_with_origin(&url, "https://evil.example.com");
        let acao = find_header(&headers, "access-control-allow-origin");
        assert!(
            acao.is_none(),
            "ACAO header must NOT be present for unknown origin, got: {acao:?}"
        );
        bridge.stop();
    }

    #[test]
    fn list_returns_replay_files() {
        let tmp = TempDir::new().unwrap();
        fs::write(tmp.path().join("battle1.wowsreplay"), b"data1").unwrap();
        fs::write(tmp.path().join("battle2.wowsreplay"), b"data2").unwrap();
        fs::write(tmp.path().join("ignore.txt"), b"nope").unwrap();
        let bridge = start_test_bridge(&tmp);
        let url = format!("http://127.0.0.1:{}/v1/replays", bridge.port());
        let (status, body, _) = get(&url);
        assert_eq!(status, 200);
        let v: serde_json::Value = serde_json::from_str(&body).unwrap();
        let replays = v["replays"].as_array().unwrap();
        assert_eq!(replays.len(), 2);
        let names: Vec<&str> = replays
            .iter()
            .map(|r| r["name"].as_str().unwrap())
            .collect();
        assert!(names.contains(&"battle1.wowsreplay"));
        assert!(names.contains(&"battle2.wowsreplay"));
        bridge.stop();
    }

    #[test]
    fn fetch_returns_file_bytes() {
        let tmp = TempDir::new().unwrap();
        let content = b"replay bytes";
        fs::write(tmp.path().join("game.wowsreplay"), content).unwrap();
        let bridge = start_test_bridge(&tmp);
        let url = format!("http://127.0.0.1:{}/v1/replays/game.wowsreplay", bridge.port());
        let (status, body, _) = get(&url);
        assert_eq!(status, 200);
        assert_eq!(body.as_bytes(), content);
        bridge.stop();
    }

    #[test]
    fn fetch_missing_file_returns_404() {
        let tmp = TempDir::new().unwrap();
        let bridge = start_test_bridge(&tmp);
        let url = format!(
            "http://127.0.0.1:{}/v1/replays/nonexistent.wowsreplay",
            bridge.port()
        );
        let (status, _, _) = get(&url);
        assert_eq!(status, 404);
        bridge.stop();
    }

    #[test]
    fn fetch_traversal_rejected() {
        let tmp = TempDir::new().unwrap();
        let bridge = start_test_bridge(&tmp);
        // URL-encode the traversal attempt
        let url = format!(
            "http://127.0.0.1:{}/v1/replays/..%2Fsecret.txt",
            bridge.port()
        );
        let (status, _, _) = get(&url);
        // Must not be 200; either 400 (safe-path rejected) or 404 is fine
        assert_ne!(status, 200, "traversal attempt must not return 200");
        bridge.stop();
    }

    #[test]
    fn latest_returns_newest_file() {
        let tmp = TempDir::new().unwrap();
        fs::write(tmp.path().join("old.wowsreplay"), b"old").unwrap();
        // Small sleep to ensure mtime differs
        std::thread::sleep(std::time::Duration::from_millis(10));
        fs::write(tmp.path().join("new.wowsreplay"), b"new").unwrap();
        let bridge = start_test_bridge(&tmp);
        let url = format!("http://127.0.0.1:{}/v1/replays/latest", bridge.port());
        let (status, body, _) = get(&url);
        assert_eq!(status, 200);
        let v: serde_json::Value = serde_json::from_str(&body).unwrap();
        assert_eq!(v["name"], "new.wowsreplay");
        bridge.stop();
    }

    #[test]
    fn latest_returns_404_when_no_replays() {
        let tmp = TempDir::new().unwrap();
        let bridge = start_test_bridge(&tmp);
        let url = format!("http://127.0.0.1:{}/v1/replays/latest", bridge.port());
        let (status, _, _) = get(&url);
        assert_eq!(status, 404);
        bridge.stop();
    }

    /// Verify that the public `start()` entry point falls back from 43210 to one
    /// of the fallback ports when 43210 is already occupied.
    #[test]
    fn port_fallback_when_canonical_is_occupied() {
        // Pre-occupy port 43210.  If something else already holds it we skip —
        // that would also mean start() would fall back, but we can't assert cleanly.
        let occupied = match TcpListener::bind("127.0.0.1:43210") {
            Ok(l) => l,
            Err(_) => {
                // Port already taken externally; skip this test to avoid flakiness.
                eprintln!("port_fallback: 43210 already in use, skipping");
                return;
            }
        };

        let tmp = TempDir::new().unwrap();
        // Call the real public entry point — it must skip 43210 and pick a fallback.
        let bridge = start(tmp.path().to_path_buf(), None)
            .expect("start() must succeed by falling back to a higher port");

        assert_ne!(bridge.port(), 43210, "must not bind the occupied canonical port");
        assert!(
            FALLBACK_PORTS.contains(&bridge.port()),
            "must bind one of the declared fallback ports, got {}",
            bridge.port()
        );

        drop(occupied); // release 43210 before stopping
        bridge.stop();
    }

    #[test]
    fn watch_generation_increments_on_new_replay() {
        let tmp = TempDir::new().unwrap();
        let bridge = start_test_bridge(&tmp);
        let initial_gen = bridge.generation();
        // Drop a new replay file
        fs::write(tmp.path().join("new_battle.wowsreplay"), b"data").unwrap();
        // Poll with timeout (FSEvents on macOS can have latency).
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        loop {
            if bridge.generation() > initial_gen {
                break;
            }
            if std::time::Instant::now() >= deadline {
                panic!("generation did not increment within 5 seconds");
            }
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
        bridge.stop();
    }
}
