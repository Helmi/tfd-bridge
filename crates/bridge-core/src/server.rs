/// Local loopback bridge server.
///
/// Binds 127.0.0.1 starting at port 43210, falling back through 43211-43214
/// if the canonical port is occupied.  Serves read-only replay files from the
/// configured replays directory.
///
/// Endpoints
///   GET /v1/health             → JSON {name, version, capabilities}
///   GET /v1/replays            → JSON [{name, size, modified_ms}]  (*.wowsreplay + tempArenaInfo.json)
///   GET /v1/replays/latest     → JSON {name, size, modified_ms} or 404  (newest *.wowsreplay; excludes the live file)
///   GET /v1/replays/{name}     → file bytes
///
/// The `tempArenaInfo.json` file is the live battle roster written by WoWS at
/// battle start and deleted at battle end.  It is included in the replays list
/// and in the file-watcher generation counter so the Battle Monitor can detect
/// live battles in real time.
///
/// All responses include CORS headers that allow the canonical origin
/// `https://engine.tfd.rocks`.  A secondary `dev_origin` can be passed for
/// local development.  Requests from all other origins get no ACAO header.
///
/// As a DNS-rebinding defence, every request must carry a Host header of
/// exactly `127.0.0.1:<bound-port>` or `localhost:<bound-port>`; anything
/// else is rejected with 403 (and no CORS headers) before routing.
use notify::{RecursiveMode, Watcher};
use percent_encoding::percent_decode_str;
use serde::Serialize;
use std::io::Read;
use std::path::{Component, Path, PathBuf};
use std::sync::{
    atomic::{AtomicU64, Ordering},
    Arc,
};
use std::thread;
use std::time::UNIX_EPOCH;
use tiny_http::{Header, Response, Server, StatusCode};
use walkdir::WalkDir;

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

    // File watcher: bump generation on any change to served files in the replays dir.
    // Covers *.wowsreplay archives and tempArenaInfo.json (live battle roster).
    let mut watcher = notify::recommended_watcher(move |res: notify::Result<notify::Event>| {
        if let Ok(event) = res {
            if event.kind.is_create() || event.kind.is_modify() || event.kind.is_remove() {
                let affects_replays = event.paths.iter().any(|p| is_served_file(p));
                if affects_replays {
                    gen_clone.fetch_add(1, Ordering::SeqCst);
                }
            }
        }
    })
    .map_err(|e| BridgeError::Watch(e.to_string()))?;

    watcher
        .watch(&dir_clone, RecursiveMode::Recursive)
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
        handle_requests(&server_clone, &replays_dir, &allowed_origins, &gen_clone2, port);
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
    port: u16,
) {
    loop {
        let request = match server.recv() {
            Ok(r) => r,
            Err(_) => break,
        };

        // DNS-rebinding defence: validate the Host header BEFORE routing so
        // every endpoint is covered.  A browser request that was DNS-rebound
        // to 127.0.0.1 still carries the attacker's hostname in Host, so only
        // the two loopback spellings with the actual bound port are accepted.
        // Rejections get a 403 with no CORS headers.
        let host = request
            .headers()
            .iter()
            .find(|h| h.field.equiv("Host"))
            .map(|h| h.value.as_str().to_string());
        let host_ok = host
            .as_deref()
            .map(|h| is_allowed_host(h, port))
            .unwrap_or(false);
        if !host_ok {
            let response =
                make_json_response(StatusCode(403), r#"{"error":"forbidden"}"#, None);
            if let Err(e) = request.respond(response) {
                log::warn!("Bridge: failed to send response: {e}");
            }
            continue;
        }

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

        let response = match request.method() {
            tiny_http::Method::Get => match path_no_qs {
                "/v1/health" => handle_health(),
                "/v1/replays" => handle_list(replays_dir, generation),
                "/v1/replays/latest" => handle_latest(replays_dir),
                p if p.starts_with("/v1/replays/") => {
                    let name = &p["/v1/replays/".len()..];
                    handle_fetch(replays_dir, name)
                }
                _ => make_json_response(StatusCode(404), r#"{"error":"not found"}"#, None),
            },
            tiny_http::Method::Options => {
                make_json_response(StatusCode(204), "", None)
            }
            _ => make_json_response(
                StatusCode(405),
                r#"{"error":"method not allowed"}"#,
                None,
            ),
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
        capabilities: &["replays-v1", "live-v1"],
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
        Ok(entries) => {
            // Only consider archive files — tempArenaInfo.json is excluded here
            // even though list_replays() includes it for the /v1/replays list.
            let mut archives: Vec<ReplayEntry> = entries
                .into_iter()
                .filter(|e| e.name.to_ascii_lowercase().ends_with(".wowsreplay"))
                .collect();
            if archives.is_empty() {
                return make_json_response(StatusCode(404), r#"{"error":"no replays found"}"#, None);
            }
            archives.sort_by(|a, b| b.modified_ms.cmp(&a.modified_ms));
            let body = serde_json::to_string(&archives[0]).unwrap_or_default();
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
    // Percent-decode the name: the monitor sends encodeURIComponent() so nested
    // paths arrive as e.g. "13.1.0%2Ffile.wowsreplay".  Decode first, then
    // resolve_safe_path validates component-by-component.
    let decoded = match percent_decode_str(name).decode_utf8() {
        Ok(s) => s.into_owned(),
        Err(_) => {
            return make_json_response(StatusCode(400), r#"{"error":"invalid UTF-8 in path"}"#, None);
        }
    };
    match resolve_safe_path(replays_dir, &decoded) {
        Err(e) => make_json_response(StatusCode(400), &format!(r#"{{"error":"{}"}}"#, e), None),
        Ok(path) => {
            // Only serve files that are part of the listing contract:
            // *.wowsreplay archives and tempArenaInfo.json.
            if !is_served_file(&path) {
                return make_json_response(StatusCode(404), r#"{"error":"not found"}"#, None);
            }
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

/// Returns `true` iff `host` is exactly `127.0.0.1:<port>` or
/// `localhost:<port>` for the actually-bound port.  This defeats DNS
/// rebinding: a rebound request always carries the attacker's hostname in
/// Host, never a loopback spelling.  The match is deliberately exact
/// (fail-closed) — browsers send lowercase hostnames and always include a
/// non-default port, so no other spellings are needed.
fn is_allowed_host(host: &str, port: u16) -> bool {
    host == format!("127.0.0.1:{port}") || host == format!("localhost:{port}")
}

/// Returns `true` for files that the bridge serves and watches:
/// - `*.wowsreplay` archive files
/// - `tempArenaInfo.json` (the live battle roster, case-insensitive)
fn is_served_file(path: &Path) -> bool {
    if path
        .extension()
        .map(|ext| ext.eq_ignore_ascii_case("wowsreplay"))
        .unwrap_or(false)
    {
        return true;
    }
    path.file_name()
        .map(|n| n.eq_ignore_ascii_case("tempArenaInfo.json"))
        .unwrap_or(false)
}

/// List all `.wowsreplay` files and `tempArenaInfo.json` in `dir`, recursively.
///
/// Returns entries with forward-slash relative paths so that nested files
/// appear as e.g. `"13.1.0/20260119_x.wowsreplay"`.  Symlinked directories
/// are not followed (follow_links(false)) to prevent symlink escape.
pub fn list_replays(dir: &Path) -> Result<Vec<ReplayEntry>, std::io::Error> {
    let mut entries = Vec::new();
    for result in WalkDir::new(dir).follow_links(false).min_depth(1) {
        let entry = match result {
            Ok(e) => e,
            Err(_) => continue, // skip unreadable entries gracefully
        };
        // Skip symlinks (leaf files and dirs).
        if entry.path_is_symlink() {
            continue;
        }
        let path = entry.path();
        if entry.file_type().is_file() && is_served_file(path) {
            let meta = entry.metadata()?;
            let modified_ms = meta
                .modified()
                .ok()
                .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
                .map(|d| d.as_millis() as u64)
                .unwrap_or(0);
            // Build a forward-slash relative name from path components so it
            // is correct on both Unix and Windows.
            let rel = path.strip_prefix(dir).map_err(|e| {
                std::io::Error::other(e.to_string())
            })?;
            let name = rel
                .components()
                .map(|c| c.as_os_str().to_string_lossy())
                .collect::<Vec<_>>()
                .join("/");
            entries.push(ReplayEntry {
                name,
                size: meta.len(),
                modified_ms,
            });
        }
    }
    Ok(entries)
}

/// Resolve a (possibly multi-segment, percent-decoded) relative path to an
/// absolute path that is confirmed to live inside `replays_dir`.
///
/// # Security guarantees
///
/// - Rejects empty input.
/// - Validates every path component: rejects `..` (`ParentDir`), absolute
///   roots (`RootDir`), Windows drive prefixes (`Prefix`), and empty
///   components.  Only `Normal` components are accepted.
/// - Rejects backslash (`\`) anywhere in the input (on Windows `\` is a path
///   separator — rejecting it here keeps behaviour consistent cross-platform).
/// - After joining onto the *canonicalized* replays dir, if the file exists,
///   canonicalizes the candidate and asserts it still starts with the
///   canonicalized replays dir.  This catches symlink chains and intermediate
///   symlinked directories that point outside.
/// - Rejects leaf symlinks (file or directory symlink at the final path).
/// - Non-existent files pass component validation and return the candidate
///   path; the caller receives a 404 when opening.
pub fn resolve_safe_path(replays_dir: &Path, name: &str) -> Result<PathBuf, String> {
    if name.is_empty() {
        return Err("empty name".into());
    }

    // Reject backslash unconditionally: on Windows it is a path separator,
    // so allowing it would bypass component-level validation on the real
    // deployment target even though macOS treats it as a Normal character.
    if name.contains('\\') {
        return Err("backslash is not allowed in paths".into());
    }

    // Validate every component of the relative path.
    let rel = Path::new(name);
    let mut validated_components: Vec<&std::ffi::OsStr> = Vec::new();
    for component in rel.components() {
        match component {
            Component::Normal(seg) => {
                validated_components.push(seg);
            }
            Component::ParentDir => {
                return Err("path traversal is not allowed".into());
            }
            Component::RootDir | Component::Prefix(_) => {
                return Err("absolute paths are not allowed".into());
            }
            Component::CurDir => {
                // Silently skip "." segments — they are harmless but unusual.
            }
        }
    }

    if validated_components.is_empty() {
        return Err("empty or root-only path".into());
    }

    // Canonicalize the replays dir so symlinks in its own path are resolved.
    let canonical_dir = replays_dir
        .canonicalize()
        .map_err(|e| format!("replays dir not accessible: {e}"))?;

    // Build the candidate by joining validated components.
    let mut candidate = canonical_dir.clone();
    for seg in &validated_components {
        candidate.push(seg);
    }

    // Reject leaf symlinks before attempting canonicalization: a symlink
    // inside the replays dir could point to an arbitrary file elsewhere.
    // symlink_metadata does NOT follow the link, so we see the link itself.
    if let Ok(meta) = std::fs::symlink_metadata(&candidate) {
        if meta.file_type().is_symlink() {
            return Err("symlinks are not allowed".into());
        }
    }

    // If the target exists, canonicalize and confirm it is still inside the
    // replays dir.  This catches intermediate symlinked directories that point
    // outside and any remaining `..`-equivalent escape vectors.
    if candidate.exists() {
        let canonical_candidate = candidate
            .canonicalize()
            .map_err(|e| format!("cannot canonicalize path: {e}"))?;
        if !canonical_candidate.starts_with(&canonical_dir) {
            return Err("path traversal is not allowed".into());
        }
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

    // Forward-slash nested paths are now ALLOWED (the resolver walks components
    // and validates each one).  A safe nested path must resolve successfully.
    #[test]
    fn resolve_safe_path_accepts_nested_path() {
        let tmp = TempDir::new().unwrap();
        let sub = tmp.path().join("13.1.0");
        fs::create_dir_all(&sub).unwrap();
        fs::write(sub.join("battle.wowsreplay"), b"data").unwrap();
        let result = resolve_safe_path(tmp.path(), "13.1.0/battle.wowsreplay");
        assert!(result.is_ok(), "nested path must be accepted: {result:?}");
        let p = result.unwrap();
        assert!(p.starts_with(tmp.path().canonicalize().unwrap()));
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

    /// A symlink inside the replays dir that points outside must be rejected by
    /// resolve_safe_path (symlink path-escape attack, AC#3).
    #[cfg(unix)]
    #[test]
    fn resolve_safe_path_rejects_symlink_escape() {
        use std::os::unix::fs::symlink;

        let tmp = TempDir::new().unwrap();
        // Create a real target file outside the replays dir.
        let target = tmp.path().parent().unwrap().join("secret.txt");
        fs::write(&target, b"secret data").unwrap();
        // Create a symlink inside the replays dir pointing to the external file.
        let link = tmp.path().join("evil.wowsreplay");
        symlink(&target, &link).unwrap();

        // resolve_safe_path must reject the symlink.
        let result = resolve_safe_path(tmp.path(), "evil.wowsreplay");
        assert!(
            result.is_err(),
            "symlink pointing outside replays dir must be rejected"
        );
    }

    /// is_allowed_host accepts exactly the two loopback spellings with the
    /// bound port (DNS-rebinding defence, td-a5cdbb).
    #[test]
    fn is_allowed_host_accepts_loopback_spellings() {
        assert!(is_allowed_host("127.0.0.1:43210", 43210));
        assert!(is_allowed_host("localhost:43210", 43210));
    }

    /// is_allowed_host rejects everything that is not an exact loopback match.
    #[test]
    fn is_allowed_host_rejects_everything_else() {
        // Attacker hostname — the DNS-rebinding case.
        assert!(!is_allowed_host("attacker.com:43210", 43210));
        // Right hostname, wrong port.
        assert!(!is_allowed_host("127.0.0.1:43211", 43210));
        assert!(!is_allowed_host("localhost:43211", 43210));
        // Missing port (browsers always send the non-default port).
        assert!(!is_allowed_host("127.0.0.1", 43210));
        assert!(!is_allowed_host("localhost", 43210));
        // Exact match is deliberate: no alternative loopback spellings.
        assert!(!is_allowed_host("LOCALHOST:43210", 43210));
        assert!(!is_allowed_host("[::1]:43210", 43210));
        // Hostname that merely starts with the loopback IP.
        assert!(!is_allowed_host("127.0.0.1.attacker.com:43210", 43210));
        assert!(!is_allowed_host("", 43210));
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

    /// Send a raw HTTP/1.1 GET with an arbitrary Host header.  ureq always
    /// derives Host from the URL, so Host-spoofing tests need a raw TCP
    /// request.  Always sends the allowed Origin so that CORS-header
    /// assertions on the response are meaningful.
    /// Returns (status, full raw response text incl. headers).
    fn raw_get_with_host(port: u16, path: &str, host: &str) -> (u16, String) {
        use std::io::Write;
        use std::net::TcpStream;

        let mut stream = TcpStream::connect(("127.0.0.1", port)).unwrap();
        let request = format!(
            "GET {path} HTTP/1.1\r\nHost: {host}\r\nOrigin: https://engine.tfd.rocks\r\nConnection: close\r\n\r\n"
        );
        stream.write_all(request.as_bytes()).unwrap();
        let mut raw = String::new();
        stream.read_to_string(&mut raw).unwrap();
        // Status line looks like "HTTP/1.1 403 Forbidden".
        let status: u16 = raw
            .split_whitespace()
            .nth(1)
            .and_then(|s| s.parse().ok())
            .unwrap_or_else(|| panic!("no status code in response: {raw:?}"));
        (status, raw)
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

    // ── Host-header validation tests (DNS-rebinding defence, td-a5cdbb) ──────

    /// A request whose Host is a foreign hostname (the DNS-rebound case) must
    /// be rejected with 403 on EVERY endpoint — the check runs before routing.
    #[test]
    fn host_validation_rejects_foreign_host_on_all_endpoints() {
        let tmp = TempDir::new().unwrap();
        fs::write(tmp.path().join("game.wowsreplay"), b"data").unwrap();
        let bridge = start_test_bridge(&tmp);
        let host = format!("attacker.com:{}", bridge.port());
        for path in [
            "/v1/health",
            "/v1/replays",
            "/v1/replays/latest",
            "/v1/replays/game.wowsreplay",
        ] {
            let (status, _) = raw_get_with_host(bridge.port(), path, &host);
            assert_eq!(status, 403, "{path} must return 403 for foreign Host");
        }
        bridge.stop();
    }

    /// The 403 rejection must NOT carry CORS headers, even though the request
    /// sends the allowed Origin (raw_get_with_host always does).
    #[test]
    fn host_validation_403_has_no_cors_headers() {
        let tmp = TempDir::new().unwrap();
        let bridge = start_test_bridge(&tmp);
        let host = format!("attacker.com:{}", bridge.port());
        let (status, raw) = raw_get_with_host(bridge.port(), "/v1/health", &host);
        assert_eq!(status, 403);
        assert!(
            !raw.to_ascii_lowercase().contains("access-control-allow-"),
            "403 must not include CORS headers, got:\n{raw}"
        );
        bridge.stop();
    }

    /// Right hostname but wrong port in Host must be rejected.
    #[test]
    fn host_validation_rejects_wrong_port() {
        let tmp = TempDir::new().unwrap();
        let bridge = start_test_bridge(&tmp);
        let host = format!("127.0.0.1:{}", bridge.port().wrapping_add(1));
        let (status, _) = raw_get_with_host(bridge.port(), "/v1/health", &host);
        assert_eq!(status, 403, "wrong port in Host must return 403");
        bridge.stop();
    }

    /// Regression: the genuine loopback Host (`127.0.0.1:<port>`, what browsers
    /// send when fetching the bridge URL) must still get 200 on every endpoint.
    #[test]
    fn host_validation_accepts_loopback_ip_on_all_endpoints() {
        let tmp = TempDir::new().unwrap();
        fs::write(tmp.path().join("game.wowsreplay"), b"data").unwrap();
        let bridge = start_test_bridge(&tmp);
        let host = format!("127.0.0.1:{}", bridge.port());
        for path in [
            "/v1/health",
            "/v1/replays",
            "/v1/replays/latest",
            "/v1/replays/game.wowsreplay",
        ] {
            let (status, _) = raw_get_with_host(bridge.port(), path, &host);
            assert_eq!(status, 200, "{path} must return 200 for loopback Host");
        }
        bridge.stop();
    }

    /// `localhost:<port>` is the other accepted Host spelling.
    #[test]
    fn host_validation_accepts_localhost() {
        let tmp = TempDir::new().unwrap();
        let bridge = start_test_bridge(&tmp);
        let host = format!("localhost:{}", bridge.port());
        let (status, _) = raw_get_with_host(bridge.port(), "/v1/health", &host);
        assert_eq!(status, 200, "localhost Host must be accepted");
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

    /// Regression: /v1/replays/latest must return the newest .wowsreplay even
    /// when a newer tempArenaInfo.json is also present in the directory.
    #[test]
    fn latest_ignores_temp_arena_info_prefers_wowsreplay() {
        let tmp = TempDir::new().unwrap();
        // Write the replay first (older mtime).
        fs::write(tmp.path().join("battle.wowsreplay"), b"replay").unwrap();
        // Small sleep to ensure mtime differs.
        std::thread::sleep(std::time::Duration::from_millis(10));
        // Write tempArenaInfo.json second so it has a newer mtime.
        fs::write(tmp.path().join("tempArenaInfo.json"), b"{}").unwrap();
        let bridge = start_test_bridge(&tmp);
        let url = format!("http://127.0.0.1:{}/v1/replays/latest", bridge.port());
        let (status, body, _) = get(&url);
        assert_eq!(status, 200);
        let v: serde_json::Value = serde_json::from_str(&body).unwrap();
        assert_eq!(
            v["name"], "battle.wowsreplay",
            "latest must return the .wowsreplay, not tempArenaInfo.json"
        );
        bridge.stop();
    }

    /// /v1/replays/latest returns 404 when the directory contains only
    /// tempArenaInfo.json (no .wowsreplay archives).
    #[test]
    fn latest_returns_404_when_only_temp_arena_info_present() {
        let tmp = TempDir::new().unwrap();
        fs::write(tmp.path().join("tempArenaInfo.json"), b"{}").unwrap();
        let bridge = start_test_bridge(&tmp);
        let url = format!("http://127.0.0.1:{}/v1/replays/latest", bridge.port());
        let (status, _, _) = get(&url);
        assert_eq!(status, 404, "latest must return 404 when no .wowsreplay archives exist");
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

    /// A symlink placed inside the replays dir that points to an external file
    /// must not be served over the loopback bridge (symlink path-escape, AC#3).
    #[cfg(unix)]
    #[test]
    fn fetch_symlink_escape_rejected() {
        use std::os::unix::fs::symlink;

        let tmp = TempDir::new().unwrap();
        // Create a sensitive file outside the replays dir.
        let target = tmp.path().parent().unwrap().join("external_secret.txt");
        fs::write(&target, b"should not be served").unwrap();
        // Place a symlink with a .wowsreplay extension inside the replays dir.
        symlink(&target, tmp.path().join("evil.wowsreplay")).unwrap();

        let bridge = start_test_bridge(&tmp);
        let url = format!(
            "http://127.0.0.1:{}/v1/replays/evil.wowsreplay",
            bridge.port()
        );
        let (status, _, _) = get(&url);
        assert_ne!(
            status, 200,
            "symlink-escape: serving a symlink must not return 200"
        );
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
        assert_eq!(
            bridge.port(),
            43211,
            "must bind the first fallback port (43211) when only 43210 is occupied, got {}",
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

    // ── tempArenaInfo.json (live battle) tests ────────────────────────────────

    /// list_replays must include tempArenaInfo.json alongside *.wowsreplay.
    #[test]
    fn list_replays_includes_temp_arena_info() {
        let tmp = TempDir::new().unwrap();
        fs::write(tmp.path().join("battle.wowsreplay"), b"replay").unwrap();
        fs::write(tmp.path().join("tempArenaInfo.json"), b"{}").unwrap();
        fs::write(tmp.path().join("other.txt"), b"ignored").unwrap();
        let entries = list_replays(tmp.path()).unwrap();
        assert_eq!(entries.len(), 2, "expected wowsreplay + tempArenaInfo.json");
        let names: Vec<&str> = entries.iter().map(|e| e.name.as_str()).collect();
        assert!(names.contains(&"battle.wowsreplay"));
        assert!(names.contains(&"tempArenaInfo.json"));
    }

    /// GET /v1/replays must include tempArenaInfo.json in the JSON list.
    #[test]
    fn list_endpoint_includes_temp_arena_info() {
        let tmp = TempDir::new().unwrap();
        fs::write(tmp.path().join("battle.wowsreplay"), b"replay").unwrap();
        fs::write(tmp.path().join("tempArenaInfo.json"), b"{}").unwrap();
        let bridge = start_test_bridge(&tmp);
        let url = format!("http://127.0.0.1:{}/v1/replays", bridge.port());
        let (status, body, _) = get(&url);
        assert_eq!(status, 200);
        let v: serde_json::Value = serde_json::from_str(&body).unwrap();
        let replays = v["replays"].as_array().unwrap();
        let names: Vec<&str> = replays.iter().map(|r| r["name"].as_str().unwrap()).collect();
        assert!(
            names.contains(&"tempArenaInfo.json"),
            "list must contain tempArenaInfo.json, got: {names:?}"
        );
        assert!(names.contains(&"battle.wowsreplay"));
        bridge.stop();
    }

    /// GET /v1/replays/tempArenaInfo.json must return the file bytes.
    #[test]
    fn fetch_temp_arena_info_returns_bytes() {
        let tmp = TempDir::new().unwrap();
        let content = br#"{"vehicles":[]}"#;
        fs::write(tmp.path().join("tempArenaInfo.json"), content).unwrap();
        let bridge = start_test_bridge(&tmp);
        let url = format!(
            "http://127.0.0.1:{}/v1/replays/tempArenaInfo.json",
            bridge.port()
        );
        let (status, body, _) = get(&url);
        assert_eq!(status, 200);
        assert_eq!(body.as_bytes(), content);
        bridge.stop();
    }

    /// The watcher must bump generation when tempArenaInfo.json is CREATED.
    #[test]
    fn watch_generation_increments_on_temp_arena_info_create() {
        let tmp = TempDir::new().unwrap();
        let bridge = start_test_bridge(&tmp);
        let initial_gen = bridge.generation();

        fs::write(tmp.path().join("tempArenaInfo.json"), b"{}").unwrap();

        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        loop {
            if bridge.generation() > initial_gen {
                break;
            }
            if std::time::Instant::now() >= deadline {
                panic!("generation did not increment within 5 seconds after tempArenaInfo.json create");
            }
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
        bridge.stop();
    }

    /// The watcher must bump generation when tempArenaInfo.json is DELETED.
    /// We write the file before starting the bridge so the create event does
    /// not pollute the baseline; the only bump we observe is the delete.
    #[test]
    fn watch_generation_increments_on_temp_arena_info_delete() {
        let tmp = TempDir::new().unwrap();
        // Write the file BEFORE starting the bridge so no create event fires.
        let live_file = tmp.path().join("tempArenaInfo.json");
        fs::write(&live_file, b"{}").unwrap();

        let bridge = start_test_bridge(&tmp);
        let initial_gen = bridge.generation();

        fs::remove_file(&live_file).unwrap();

        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        loop {
            if bridge.generation() > initial_gen {
                break;
            }
            if std::time::Instant::now() >= deadline {
                panic!("generation did not increment within 5 seconds after tempArenaInfo.json delete");
            }
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
        bridge.stop();
    }

    // ── Recursive listing tests ───────────────────────────────────────────────

    /// list_replays must walk subdirectories and return forward-slash relative
    /// paths for nested files.
    #[test]
    fn list_replays_recursive_returns_nested_files() {
        let tmp = TempDir::new().unwrap();
        // Top-level file
        fs::write(tmp.path().join("top.wowsreplay"), b"top").unwrap();
        // Nested file under a version subdirectory
        let sub = tmp.path().join("13.1.0");
        fs::create_dir_all(&sub).unwrap();
        fs::write(sub.join("nested.wowsreplay"), b"nested").unwrap();
        // Ignored file
        fs::write(tmp.path().join("readme.txt"), b"ignored").unwrap();

        let entries = list_replays(tmp.path()).unwrap();
        assert_eq!(entries.len(), 2);
        let names: Vec<&str> = entries.iter().map(|e| e.name.as_str()).collect();
        assert!(names.contains(&"top.wowsreplay"), "top-level file must be listed");
        assert!(
            names.contains(&"13.1.0/nested.wowsreplay"),
            "nested file must use forward-slash: {names:?}"
        );
    }

    /// GET /v1/replays must include nested files with forward-slash names.
    #[test]
    fn list_endpoint_returns_nested_files() {
        let tmp = TempDir::new().unwrap();
        let sub = tmp.path().join("13.1.0");
        fs::create_dir_all(&sub).unwrap();
        fs::write(sub.join("nested.wowsreplay"), b"data").unwrap();
        let bridge = start_test_bridge(&tmp);
        let url = format!("http://127.0.0.1:{}/v1/replays", bridge.port());
        let (status, body, _) = get(&url);
        assert_eq!(status, 200);
        let v: serde_json::Value = serde_json::from_str(&body).unwrap();
        let replays = v["replays"].as_array().unwrap();
        let names: Vec<&str> = replays.iter().map(|r| r["name"].as_str().unwrap()).collect();
        assert!(
            names.contains(&"13.1.0/nested.wowsreplay"),
            "nested file must appear in list with forward-slash path: {names:?}"
        );
        bridge.stop();
    }

    /// GET /v1/replays/{url-encoded nested path} must return the file bytes.
    #[test]
    fn fetch_nested_file_returns_bytes() {
        let tmp = TempDir::new().unwrap();
        let sub = tmp.path().join("13.1.0");
        fs::create_dir_all(&sub).unwrap();
        let content = b"nested replay bytes";
        fs::write(sub.join("battle.wowsreplay"), content).unwrap();
        let bridge = start_test_bridge(&tmp);
        // The monitor sends encodeURIComponent("13.1.0/battle.wowsreplay")
        // which encodes the "/" as "%2F".
        let url = format!(
            "http://127.0.0.1:{}/v1/replays/13.1.0%2Fbattle.wowsreplay",
            bridge.port()
        );
        let (status, body, _) = get(&url);
        assert_eq!(status, 200, "nested file must be served");
        assert_eq!(body.as_bytes(), content);
        bridge.stop();
    }

    /// Nested traversal attempts must be rejected.
    #[test]
    fn fetch_nested_traversal_rejected() {
        let tmp = TempDir::new().unwrap();
        let bridge = start_test_bridge(&tmp);
        // "13.1.0/../../secret" encodes as "13.1.0%2F..%2F..%2Fsecret"
        let url = format!(
            "http://127.0.0.1:{}/v1/replays/13.1.0%2F..%2F..%2Fsecret",
            bridge.port()
        );
        let (status, _, _) = get(&url);
        assert_ne!(status, 200, "nested traversal must not return 200");
        bridge.stop();
    }

    /// "../secret" (bare traversal) must be rejected even when percent-encoded.
    #[test]
    fn fetch_bare_traversal_rejected() {
        let tmp = TempDir::new().unwrap();
        let bridge = start_test_bridge(&tmp);
        let url = format!(
            "http://127.0.0.1:{}/v1/replays/..%2Fsecret.txt",
            bridge.port()
        );
        let (status, _, _) = get(&url);
        assert_ne!(status, 200, "bare traversal must not return 200");
        bridge.stop();
    }

    /// resolve_safe_path must reject a path with ".." in the middle of nested segments.
    #[test]
    fn resolve_safe_path_rejects_nested_dotdot() {
        let tmp = TempDir::new().unwrap();
        let sub = tmp.path().join("13.1.0");
        fs::create_dir_all(&sub).unwrap();
        assert!(
            resolve_safe_path(tmp.path(), "13.1.0/../../secret").is_err(),
            "nested .. must be rejected"
        );
    }

    /// resolve_safe_path must reject absolute paths.
    #[test]
    fn resolve_safe_path_rejects_absolute_path() {
        let tmp = TempDir::new().unwrap();
        assert!(
            resolve_safe_path(tmp.path(), "/etc/passwd").is_err(),
            "absolute path must be rejected"
        );
    }

    /// The recursive watcher must bump generation when a file is created in a subfolder.
    #[test]
    fn watch_generation_increments_on_nested_create() {
        let tmp = TempDir::new().unwrap();
        let sub = tmp.path().join("13.1.0");
        fs::create_dir_all(&sub).unwrap();
        let bridge = start_test_bridge(&tmp);
        let initial_gen = bridge.generation();
        // Create a replay in the subfolder.
        fs::write(sub.join("new_battle.wowsreplay"), b"data").unwrap();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        loop {
            if bridge.generation() > initial_gen {
                break;
            }
            if std::time::Instant::now() >= deadline {
                panic!("generation did not increment within 5 seconds after nested create");
            }
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
        bridge.stop();
    }

    /// The recursive watcher must bump generation when a nested replay is deleted.
    #[test]
    fn watch_generation_increments_on_nested_delete() {
        let tmp = TempDir::new().unwrap();
        let sub = tmp.path().join("13.1.0");
        fs::create_dir_all(&sub).unwrap();
        let replay = sub.join("old_battle.wowsreplay");
        fs::write(&replay, b"data").unwrap();
        // Start bridge after file exists so no create event pollutes the baseline.
        let bridge = start_test_bridge(&tmp);
        let initial_gen = bridge.generation();
        fs::remove_file(&replay).unwrap();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        loop {
            if bridge.generation() > initial_gen {
                break;
            }
            if std::time::Instant::now() >= deadline {
                panic!("generation did not increment within 5 seconds after nested delete");
            }
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
        bridge.stop();
    }

    /// A symlinked subdirectory inside the replays dir pointing outside must not
    /// allow files inside it to be listed or fetched.
    #[cfg(unix)]
    #[test]
    fn resolve_safe_path_rejects_symlinked_subdir_escape() {
        use std::os::unix::fs::symlink;

        let tmp = TempDir::new().unwrap();
        // Create a secret directory outside the replays dir with a file inside.
        let outside_dir = tmp.path().parent().unwrap().join("outside_dir");
        fs::create_dir_all(&outside_dir).unwrap();
        fs::write(outside_dir.join("secret.wowsreplay"), b"secret").unwrap();
        // Symlink the outside directory INTO the replays dir.
        let evil_sub = tmp.path().join("evil");
        symlink(&outside_dir, &evil_sub).unwrap();

        // Accessing a file through the symlinked directory must be rejected.
        let result = resolve_safe_path(tmp.path(), "evil/secret.wowsreplay");
        assert!(
            result.is_err(),
            "file accessed through a symlinked subdir pointing outside must be rejected"
        );
    }

    /// GET /v1/replays/<non-served-file> must return 404 even when the file
    /// exists on disk.  The allowlist is *.wowsreplay + tempArenaInfo.json;
    /// any other file type must not be readable through the bridge.
    #[test]
    fn fetch_non_served_file_returns_404() {
        let tmp = TempDir::new().unwrap();
        // Create a file that is NOT in the served-file allowlist.
        fs::write(tmp.path().join("notes.txt"), b"sensitive data").unwrap();
        let bridge = start_test_bridge(&tmp);
        let url = format!(
            "http://127.0.0.1:{}/v1/replays/notes.txt",
            bridge.port()
        );
        let (status, _, _) = get(&url);
        assert_eq!(status, 404, "non-served file type must return 404, not {status}");
        bridge.stop();
    }

    /// Regression: *.wowsreplay files are still served after the allowlist fix.
    #[test]
    fn fetch_allowlisted_wowsreplay_still_200() {
        let tmp = TempDir::new().unwrap();
        let content = b"replay bytes";
        fs::write(tmp.path().join("game.wowsreplay"), content).unwrap();
        let bridge = start_test_bridge(&tmp);
        let url = format!(
            "http://127.0.0.1:{}/v1/replays/game.wowsreplay",
            bridge.port()
        );
        let (status, body, _) = get(&url);
        assert_eq!(status, 200, "wowsreplay must still be served");
        assert_eq!(body.as_bytes(), content);
        bridge.stop();
    }

    /// Regression: tempArenaInfo.json is still served after the allowlist fix.
    #[test]
    fn fetch_allowlisted_temp_arena_info_still_200() {
        let tmp = TempDir::new().unwrap();
        let content = br#"{"vehicles":[]}"#;
        fs::write(tmp.path().join("tempArenaInfo.json"), content).unwrap();
        let bridge = start_test_bridge(&tmp);
        let url = format!(
            "http://127.0.0.1:{}/v1/replays/tempArenaInfo.json",
            bridge.port()
        );
        let (status, body, _) = get(&url);
        assert_eq!(status, 200, "tempArenaInfo.json must still be served");
        assert_eq!(body.as_bytes(), content);
        bridge.stop();
    }

    /// /v1/health capabilities must include "live-v1".
    #[test]
    fn health_advertises_live_capability() {
        let tmp = TempDir::new().unwrap();
        let bridge = start_test_bridge(&tmp);
        let url = format!("http://127.0.0.1:{}/v1/health", bridge.port());
        let (status, body, _) = get(&url);
        assert_eq!(status, 200);
        let v: serde_json::Value = serde_json::from_str(&body).expect("valid JSON");
        let caps: Vec<&str> = v["capabilities"]
            .as_array()
            .unwrap()
            .iter()
            .map(|c| c.as_str().unwrap())
            .collect();
        assert!(
            caps.contains(&"live-v1"),
            "health capabilities must include 'live-v1', got: {caps:?}"
        );
        bridge.stop();
    }
}
