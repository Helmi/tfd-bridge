/// Local loopback bridge server.
///
/// Binds 127.0.0.1 starting at port 43210, falling back through 43211-43214
/// if the canonical port is occupied.  Serves read-only replay files from the
/// configured replays directory.
///
/// Endpoints
///   GET /v1/health                          → JSON {name, version, capabilities}
///   GET /v1/replays                         → JSON [{name, size, modified_ms}]  (*.wowsreplay + tempArenaInfo.json)
///   GET /v1/replays/latest                  → JSON {name, size, modified_ms} or 404  (newest *.wowsreplay; excludes the live file)
///   GET /v1/replays/{name}                  → file bytes
///   GET /v1/replays/latest/result           → JSON BattleData or 404/501/504/500
///   GET /v1/replays/{name}/result           → JSON BattleData or 404/501/504/500
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
///
/// The same file watcher that drives the generation counter also feeds the
/// optional replay-finalized detector (see `finalize.rs`): `tempArenaInfo.json`
/// transitions are battle start/end signals, and a battle end triggers the
/// stability + structural checks that announce the newest `.wowsreplay` as
/// finalized.  There is exactly ONE watcher per bridge.
use crate::battle_result::{BattleData, DecodeConfig, DecodeError, Tables};
use crate::finalize::{FinalizeDetector, FinalizeOptions, TEMP_ARENA_INFO};
use notify::{RecursiveMode, Watcher};
use percent_encoding::percent_decode_str;
use serde::Serialize;
use std::collections::VecDeque;
use std::io::Read;
use std::path::{Component, Path, PathBuf};
use std::sync::{
    atomic::{AtomicU64, Ordering},
    Arc, Mutex,
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

// ── Decode context + cache ─────────────────────────────────────────────────────

/// Signature of the decode function: same as `battle_result::decode_battle_result`,
/// injectable via a closure for tests.
pub type DecodeFn =
    Arc<dyn Fn(&Path, &DecodeConfig, &Tables) -> Result<BattleData, DecodeError> + Send + Sync>;

/// Secondary index key: used to look up the hash without re-reading the file.
#[derive(PartialEq, Eq, Hash, Clone)]
struct FileKey {
    path: PathBuf,
    mtime_ms: u64,
    size: u64,
}

/// Bounded (≤32 entries) FIFO result cache.  Keyed by source_file_hash
/// (content-addressed), with a secondary `(path, mtime, size) → hash` index
/// to skip re-reading unmodified files.  Hand-rolled — no new dependency.
pub struct ResultCache {
    /// FIFO order for bounded eviction.
    order: VecDeque<String>,
    /// hash → serialised JSON body.
    by_hash: std::collections::HashMap<String, String>,
    /// (path, mtime, size) → hash  (secondary index).
    by_file: std::collections::HashMap<FileKey, String>,
    /// Maximum number of entries.
    cap: usize,
}

impl ResultCache {
    fn new(cap: usize) -> Self {
        Self {
            order: VecDeque::new(),
            by_hash: std::collections::HashMap::new(),
            by_file: std::collections::HashMap::new(),
            cap,
        }
    }

    /// Look up a cached result by file key; returns the serialised JSON body.
    fn get_by_file(&self, key: &FileKey) -> Option<&str> {
        let hash = self.by_file.get(key)?;
        self.by_hash.get(hash.as_str()).map(|s| s.as_str())
    }

    /// Insert a decoded result; evicts the oldest entry if at capacity.
    fn insert(&mut self, file_key: FileKey, hash: String, body: String) {
        if self.by_hash.contains_key(&hash) {
            // Same content already cached; just update the secondary index.
            self.by_file.insert(file_key, hash);
            return;
        }
        // Evict oldest entry if full.
        if self.order.len() >= self.cap {
            if let Some(old_hash) = self.order.pop_front() {
                self.by_hash.remove(&old_hash);
                // Remove all secondary-index entries pointing at the evicted hash.
                self.by_file.retain(|_, v| v != &old_hash);
            }
        }
        self.order.push_back(hash.clone());
        self.by_hash.insert(hash.clone(), body);
        self.by_file.insert(file_key, hash);
    }
}

/// All state needed to decode replay files and cache their results.
/// Passed to the bridge as `Option<Arc<DecodeContext>>`.  When `None` the
/// `/result` endpoints return 501 and the capability is omitted from health.
pub struct DecodeContext {
    pub config: DecodeConfig,
    pub tables: Tables,
    pub cache: Mutex<ResultCache>,
    /// The decode function — defaults to `battle_result::decode_battle_result`;
    /// injectable via a closure in tests.
    pub decode_fn: DecodeFn,
}

impl DecodeContext {
    /// Construct with the production decode function.
    pub fn new(config: DecodeConfig, tables: Tables) -> Self {
        Self {
            config,
            tables,
            cache: Mutex::new(ResultCache::new(32)),
            decode_fn: Arc::new(crate::battle_result::decode_battle_result),
        }
    }

    /// Construct with an injected decode function (for tests).
    pub fn with_decode_fn(config: DecodeConfig, tables: Tables, decode_fn: DecodeFn) -> Self {
        Self {
            config,
            tables,
            cache: Mutex::new(ResultCache::new(32)),
            decode_fn,
        }
    }
}

// ── Public types ───────────────────────────────────────────────────────────────

/// A running bridge instance.  Drop or call [`Bridge::stop`] to shut it down.
pub struct Bridge {
    port: u16,
    server: Arc<Server>,
    /// Monotonically increasing counter incremented on every replay-dir change.
    generation: Arc<AtomicU64>,
    /// Replay-finalized detector (None when started without finalize options).
    /// Held so `stop()` can cancel in-flight finalize workers.
    detector: Option<Arc<FinalizeDetector>>,
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
    /// Consuming `self` ensures the watcher is also dropped.  In-flight
    /// finalize workers are cancelled so no events fire after stop.
    pub fn stop(self) {
        if let Some(det) = &self.detector {
            det.cancel();
        }
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
    start_with_finalize(replays_dir, dev_origin, None)
}

/// Start the bridge server with optional replay-finalized detection.
///
/// When `finalize` is `Some`, the bridge's file watcher additionally drives a
/// [`FinalizeDetector`]: battle-end transitions (and a startup catch-up scan
/// gated by the caller's watermark) emit [`crate::finalize::ReplayFinalizedEvent`]s
/// through the callback in the options.
pub fn start_with_finalize(
    replays_dir: PathBuf,
    dev_origin: Option<String>,
    finalize: Option<FinalizeOptions>,
) -> Result<Bridge, BridgeError> {
    start_full(replays_dir, dev_origin, finalize, None)
}

/// Start the bridge server with all optional components.
///
/// - `replays_dir` — the validated replays directory to serve.
/// - `dev_origin`  — optional additional CORS origin for development.
/// - `finalize`    — optional replay-finalized detection.
/// - `decode`      — optional battle-result decode context; when `Some` the
///   `/result` endpoints are active and health advertises `"battle-result-v1"`.
pub fn start_full(
    replays_dir: PathBuf,
    dev_origin: Option<String>,
    finalize: Option<FinalizeOptions>,
    decode: Option<Arc<DecodeContext>>,
) -> Result<Bridge, BridgeError> {
    let candidates: Vec<u16> = std::iter::once(CANONICAL_PORT)
        .chain(FALLBACK_PORTS)
        .collect();
    start_on_ports_full(replays_dir, dev_origin, &candidates, finalize, decode)
}

/// Internal start that accepts an explicit list of ports to try in order.
/// Port 0 means "let the OS pick" (used in tests to avoid collisions).
#[allow(dead_code)]
pub(crate) fn start_on_ports(
    replays_dir: PathBuf,
    dev_origin: Option<String>,
    ports: &[u16],
    finalize: Option<FinalizeOptions>,
) -> Result<Bridge, BridgeError> {
    start_on_ports_full(replays_dir, dev_origin, ports, finalize, None)
}

/// Internal start with all options including decode context.
pub(crate) fn start_on_ports_full(
    replays_dir: PathBuf,
    dev_origin: Option<String>,
    ports: &[u16],
    finalize: Option<FinalizeOptions>,
    decode: Option<Arc<DecodeContext>>,
) -> Result<Bridge, BridgeError> {
    let (server, port) = bind_server(ports)?;

    log::info!("Bridge listening on http://127.0.0.1:{port}");

    let generation = Arc::new(AtomicU64::new(0));
    let gen_clone = Arc::clone(&generation);
    let dir_clone = replays_dir.clone();

    // Replay-finalized detection shares THIS watcher: the detector is fed
    // tempArenaInfo.json transitions from the same callback that bumps the
    // generation counter, so there is exactly one notify watcher per bridge.
    let detector = finalize.map(|opts| FinalizeDetector::new(replays_dir.clone(), opts));
    let det_clone = detector.clone();

    // File watcher: bump generation on any change to served files in the replays dir.
    // Covers *.wowsreplay archives and tempArenaInfo.json (live battle roster).
    let mut watcher = notify::recommended_watcher(move |res: notify::Result<notify::Event>| {
        if let Ok(event) = res {
            if event.kind.is_create() || event.kind.is_modify() || event.kind.is_remove() {
                // Feed roster transitions to the finalize detector BEFORE the
                // generation bump, so anyone who observed the bump can rely on
                // the detector having seen the same event.
                if let Some(det) = &det_clone {
                    for p in event.paths.iter().filter(|p| is_temp_arena_info(p)) {
                        det.observe_temp_file(p);
                    }
                }
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

    // Startup catch-up: emit finalized events for archives that landed while
    // the app was not running (modified after the caller's watermark).  Spawned
    // AFTER the watcher is active so a battle ending mid-scan is still seen.
    if let Some(det) = &detector {
        det.start_catch_up();
    }

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
        handle_requests(
            &server_clone,
            &replays_dir,
            &allowed_origins,
            &gen_clone2,
            port,
            decode.as_ref(),
        );
    });

    Ok(Bridge {
        port,
        server,
        generation,
        detector,
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
                    srv.server_addr().to_ip().map(|a| a.port()).unwrap_or(0)
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
    decode_ctx: Option<&Arc<DecodeContext>>,
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
            let response = make_json_response(StatusCode(403), r#"{"error":"forbidden"}"#, None);
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

        let cors_origin = origin.as_deref().and_then(|o| {
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
                "/v1/health" => handle_health(decode_ctx),
                "/v1/replays" => handle_list(replays_dir, generation),
                // /result routes MUST be matched BEFORE the generic /latest and /{name} fetch routes.
                "/v1/replays/latest/result" => handle_latest_result(replays_dir, decode_ctx),
                p if p.starts_with("/v1/replays/") && p.ends_with("/result") => {
                    // Extract the name segment between "/v1/replays/" and "/result".
                    // Use strip_prefix/strip_suffix to avoid a slice-bounds panic when
                    // the path is exactly "/v1/replays/result" (prefix and suffix overlap).
                    match p
                        .strip_prefix("/v1/replays/")
                        .and_then(|r| r.strip_suffix("/result"))
                    {
                        Some(mid) if !mid.is_empty() => handle_result(replays_dir, mid, decode_ctx),
                        _ => make_json_response(StatusCode(404), r#"{"error":"not found"}"#, None),
                    }
                }
                "/v1/replays/latest" => handle_latest(replays_dir),
                p if p.starts_with("/v1/replays/") => {
                    let name = &p["/v1/replays/".len()..];
                    handle_fetch(replays_dir, name)
                }
                _ => make_json_response(StatusCode(404), r#"{"error":"not found"}"#, None),
            },
            tiny_http::Method::Options => make_json_response(StatusCode(204), "", None),
            _ => make_json_response(StatusCode(405), r#"{"error":"method not allowed"}"#, None),
        };

        let response = attach_cors(response, cors_origin.as_deref());

        if let Err(e) = request.respond(response) {
            log::warn!("Bridge: failed to send response: {e}");
        }
    }
}

// ── Endpoint handlers ─────────────────────────────────────────────────────────

fn handle_health(decode_ctx: Option<&Arc<DecodeContext>>) -> Response<std::io::Cursor<Vec<u8>>> {
    #[derive(Serialize)]
    struct Health<'a> {
        name: &'static str,
        version: &'static str,
        capabilities: &'a [String],
    }
    // Build capabilities dynamically: always include the base set, and append
    // "battle-result-v1" only when the decode feature is wired.
    let mut caps: Vec<String> = vec![
        "replays-v1".to_string(),
        "live-v1".to_string(),
        // "replay_donation" advertises the donation upload pipeline
        // (td-c8973d) for the browser-side probe; the bridge uploads
        // directly to the engine — the loopback API itself is unchanged.
        "replay_donation".to_string(),
    ];
    if decode_ctx.is_some() {
        caps.push("battle-result-v1".to_string());
    }
    let body = serde_json::to_string(&Health {
        name: "tfd-bridge",
        version: crate::version(),
        capabilities: &caps,
    })
    .unwrap_or_default();

    make_json_response(StatusCode(200), &body, None)
}

fn handle_list(replays_dir: &Path, generation: &AtomicU64) -> Response<std::io::Cursor<Vec<u8>>> {
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
        Err(e) => make_json_response(StatusCode(500), &format!(r#"{{"error":"{}"}}"#, e), None),
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
                return make_json_response(
                    StatusCode(404),
                    r#"{"error":"no replays found"}"#,
                    None,
                );
            }
            archives.sort_by_key(|b| std::cmp::Reverse(b.modified_ms));
            let body = serde_json::to_string(&archives[0]).unwrap_or_default();
            make_json_response(StatusCode(200), &body, None)
        }
        Err(e) => make_json_response(StatusCode(500), &format!(r#"{{"error":"{}"}}"#, e), None),
    }
}

/// GET /v1/replays/{name}/result — decode and return the battle result for a named replay.
fn handle_result(
    replays_dir: &Path,
    name: &str,
    decode_ctx: Option<&Arc<DecodeContext>>,
) -> Response<std::io::Cursor<Vec<u8>>> {
    let ctx = match decode_ctx {
        Some(c) => c,
        None => {
            return make_json_response(
                StatusCode(501),
                r#"{"error":"battle-result feature not available"}"#,
                None,
            );
        }
    };

    // Percent-decode + safe-path validation (reuses same logic as handle_fetch).
    let decoded = match percent_decode_str(name).decode_utf8() {
        Ok(s) => s.into_owned(),
        Err(_) => {
            return make_json_response(
                StatusCode(400),
                r#"{"error":"invalid UTF-8 in path"}"#,
                None,
            );
        }
    };
    let path = match resolve_safe_path(replays_dir, &decoded) {
        Ok(p) => p,
        Err(e) => {
            return make_json_response(StatusCode(400), &format!(r#"{{"error":"{}"}}"#, e), None);
        }
    };

    // Must exist and be a .wowsreplay file.
    if !path.exists() {
        return make_json_response(StatusCode(404), r#"{"error":"not found"}"#, None);
    }
    if !path
        .extension()
        .map(|e| e.eq_ignore_ascii_case("wowsreplay"))
        .unwrap_or(false)
    {
        return make_json_response(StatusCode(404), r#"{"error":"not a replay file"}"#, None);
    }

    decode_and_respond(&path, ctx)
}

/// GET /v1/replays/latest/result — decode the newest replay and return its battle result.
fn handle_latest_result(
    replays_dir: &Path,
    decode_ctx: Option<&Arc<DecodeContext>>,
) -> Response<std::io::Cursor<Vec<u8>>> {
    let ctx = match decode_ctx {
        Some(c) => c,
        None => {
            return make_json_response(
                StatusCode(501),
                r#"{"error":"battle-result feature not available"}"#,
                None,
            );
        }
    };

    let entries = match list_replays(replays_dir) {
        Ok(e) => e,
        Err(e) => {
            return make_json_response(StatusCode(500), &format!(r#"{{"error":"{}"}}"#, e), None);
        }
    };
    // Finalized archives only: exclude the live in-progress `temp.wowsreplay`
    // (it is the newest by mtime while a battle is running, but it has no
    // results and the decoder cannot parse an incomplete packet stream).
    let mut archives: Vec<ReplayEntry> = entries
        .into_iter()
        .filter(|e| {
            let base = e.name.rsplit('/').next().unwrap_or(e.name.as_str());
            base.to_ascii_lowercase().ends_with(".wowsreplay")
                && !base.eq_ignore_ascii_case("temp.wowsreplay")
        })
        .collect();
    if archives.is_empty() {
        return make_json_response(StatusCode(404), r#"{"error":"no replays found"}"#, None);
    }
    archives.sort_by_key(|b| std::cmp::Reverse(b.modified_ms));

    // Return the newest replay that actually decodes to a battle result, skipping
    // early-quits (no results) and any that fail to decode (e.g. unsupported
    // older versions). Bounded so a streak of resultless replays cannot stall the
    // serial request loop; decoded results are cached for instant repeats.
    const MAX_TRIES: usize = 12;
    for entry in archives.iter().take(MAX_TRIES) {
        let path = match resolve_safe_path(replays_dir, &entry.name) {
            Ok(p) => p,
            Err(_) => continue,
        };
        match decode_cached(&path, ctx) {
            Ok(body) => return make_json_response(StatusCode(200), &body, None),
            Err(DecodeError::NoBattleResults) => continue,
            Err(e) => {
                log::warn!("latest/result: skipping {} ({e})", entry.name);
                continue;
            }
        }
    }
    make_json_response(
        StatusCode(404),
        r#"{"error":"no decodable battle result in recent replays"}"#,
        None,
    )
}

/// Core decode logic used by both result endpoints.
/// Implements caching (check → release lock → decode → insert) and maps
/// `DecodeError` variants to the spec status codes.
fn decode_and_respond(path: &Path, ctx: &Arc<DecodeContext>) -> Response<std::io::Cursor<Vec<u8>>> {
    match decode_cached(path, ctx) {
        Ok(body) => make_json_response(StatusCode(200), &body, None),
        Err(DecodeError::NoBattleResults) => make_json_response(
            StatusCode(404),
            r#"{"error":"no battle result (battle not finished or left early)"}"#,
            None,
        ),
        Err(e) => {
            // Log detail but don't leak stderr to the client.
            log::error!("Battle-result decode failed for {}: {e}", path.display());
            make_json_response(StatusCode(500), r#"{"error":"decode failed"}"#, None)
        }
    }
}

/// Decode `path` with caching (check under lock → release → decode → insert).
/// Returns the serialized `BattleData` JSON body on success. Shared by both
/// `decode_and_respond` (named replay) and `handle_latest_result` (which
/// iterates candidates and needs to distinguish a result from a skip).
fn decode_cached(path: &Path, ctx: &Arc<DecodeContext>) -> Result<String, DecodeError> {
    // Secondary file key (mtime + size) so unchanged files skip re-reading.
    let file_key = std::fs::metadata(path).ok().map(|m| FileKey {
        path: path.to_path_buf(),
        mtime_ms: m
            .modified()
            .ok()
            .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0),
        size: m.len(),
    });

    // Cache lookup under the lock; clone the body out before releasing it.
    if let Some(ref fk) = file_key {
        let guard = ctx.cache.lock().unwrap();
        if let Some(cached_body) = guard.get_by_file(fk) {
            return Ok(cached_body.to_string());
        }
    }
    // Lock released before the (potentially slow) decode.

    let data = (ctx.decode_fn)(path, &ctx.config, &ctx.tables)?;
    let body = serde_json::to_string(&data).map_err(|e| {
        log::error!("Battle-result serialise failed: {e}");
        DecodeError::Malformed(format!("serialise: {e}"))
    })?;
    if let Some(fk) = file_key {
        let mut guard = ctx.cache.lock().unwrap();
        guard.insert(fk, data.meta.source_file_hash.clone(), body.clone());
    }
    Ok(body)
}

fn handle_fetch(replays_dir: &Path, name: &str) -> Response<std::io::Cursor<Vec<u8>>> {
    // Percent-decode the name: the monitor sends encodeURIComponent() so nested
    // paths arrive as e.g. "13.1.0%2Ffile.wowsreplay".  Decode first, then
    // resolve_safe_path validates component-by-component.
    let decoded = match percent_decode_str(name).decode_utf8() {
        Ok(s) => s.into_owned(),
        Err(_) => {
            return make_json_response(
                StatusCode(400),
                r#"{"error":"invalid UTF-8 in path"}"#,
                None,
            );
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
            let header = Header::from_bytes(b"Content-Type", b"application/octet-stream").unwrap();
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

/// Returns `true` iff `path` is the live battle roster file
/// `tempArenaInfo.json` (case-insensitive), at any depth.
fn is_temp_arena_info(path: &Path) -> bool {
    path.file_name()
        .map(|n| n.eq_ignore_ascii_case(TEMP_ARENA_INFO))
        .unwrap_or(false)
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
    is_temp_arena_info(path)
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
            let rel = path
                .strip_prefix(dir)
                .map_err(|e| std::io::Error::other(e.to_string()))?;
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
        let acao = Header::from_bytes(b"Access-Control-Allow-Origin", origin.as_bytes()).unwrap();
        let acam = Header::from_bytes(b"Access-Control-Allow-Methods", b"GET, OPTIONS").unwrap();
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
        start_on_ports(tmp_dir.path().to_path_buf(), None, &[0], None).expect("bridge start failed")
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
                        resp.header(&name)
                            .map(|v| (name.to_lowercase(), v.to_string()))
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
                        resp.header(&name)
                            .map(|v| (name.to_lowercase(), v.to_string()))
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
        let url = format!(
            "http://127.0.0.1:{}/v1/replays/game.wowsreplay",
            bridge.port()
        );
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
        assert_eq!(
            status, 404,
            "latest must return 404 when no .wowsreplay archives exist"
        );
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

        assert_ne!(
            bridge.port(),
            43210,
            "must not bind the occupied canonical port"
        );
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
        let names: Vec<&str> = replays
            .iter()
            .map(|r| r["name"].as_str().unwrap())
            .collect();
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
                panic!(
                    "generation did not increment within 5 seconds after tempArenaInfo.json create"
                );
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
                panic!(
                    "generation did not increment within 5 seconds after tempArenaInfo.json delete"
                );
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
        assert!(
            names.contains(&"top.wowsreplay"),
            "top-level file must be listed"
        );
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
        let names: Vec<&str> = replays
            .iter()
            .map(|r| r["name"].as_str().unwrap())
            .collect();
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
        let url = format!("http://127.0.0.1:{}/v1/replays/notes.txt", bridge.port());
        let (status, _, _) = get(&url);
        assert_eq!(
            status, 404,
            "non-served file type must return 404, not {status}"
        );
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

    /// /v1/health capabilities must include "replay_donation" (td-c8973d) so
    /// the browser-side probe can detect the donation upload pipeline.
    #[test]
    fn health_advertises_replay_donation_capability() {
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
            caps.contains(&"replay_donation"),
            "health capabilities must include 'replay_donation', got: {caps:?}"
        );
        bridge.stop();
    }

    // ── Battle-result endpoint tests (td-865788, SPEC §10c) ───────────────────
    //
    // These tests inject a stub decode_fn (canned Ok/Err + call counter) so
    // no real sidecar or replay files are needed.

    use crate::battle_result::{
        BattleData, BattleMeta, BattlePlayer, DecodeConfig, DecodeError, DecodeStatus, Tables,
    };
    use std::sync::atomic::AtomicU32;

    /// Build a minimal stub `Tables` — we only need a non-panicking value for
    /// the DecodeContext; no actual decode happens (stub decode_fn is used).
    fn stub_tables() -> Tables {
        Tables {
            public_indices: std::collections::HashMap::new(),
            common_results: Vec::new(),
            interaction_details: Vec::new(),
            interaction_index: std::collections::HashMap::new(),
            building_interaction_index: std::collections::HashMap::new(),
            private_results: Vec::new(),
            init_economics_indices: std::collections::HashMap::new(),
            common_economics_indices: std::collections::HashMap::new(),
            ships: std::collections::HashMap::new(),
            achievements: std::collections::HashMap::new(),
            bonus_index: std::collections::HashMap::new(),
            skill_costs: std::collections::HashMap::new(),
        }
    }

    /// Build a minimal stub `DecodeConfig` — paths don't need to exist because
    /// the stub decode_fn never reads them.
    fn stub_config() -> DecodeConfig {
        DecodeConfig {
            game_dir: PathBuf::from("/stub/game"),
            constants_path: PathBuf::from("/stub/constants.json"),
            ship_index_path: PathBuf::from("/stub/ship_index.json"),
            achievement_index_path: PathBuf::from("/stub/achievement_index.json"),
            bonus_index_path: PathBuf::from("/stub/bonus_index.json"),
        }
    }

    /// Build a minimal `BattleData` whose JSON serialises cleanly.
    fn stub_battle_data(hash: &str) -> BattleData {
        BattleData {
            meta: BattleMeta {
                schema_version: "1.0".into(),
                arena_unique_id: 1234,
                map_name: "Shards".into(),
                game_version: "15,4,0,1".into(),
                game_version_short: Some("15.4".into()),
                match_group: Some("pvp".into()),
                duration_seconds: Some(600),
                winner_team: Some(1),
                battle_time: Some(1700000000),
                source_file_hash: hash.to_string(),
                owner_account_db_id: Some(591735977),
                decode_status: DecodeStatus::Ok,
                decode_checks: Vec::new(),
                warnings: Vec::new(),
            },
            players: vec![BattlePlayer {
                account_db_id: 591735977,
                player_name: Some("FrankDrake".into()),
                clan_id: None,
                clan_tag: Some("-TFD-".into()),
                ship_id: Some(42),
                ship_name: Some("TestShip".into()),
                ship_tier: Some(9),
                ship_class: None,
                team_id: Some(1),
                prebattle_id: Some(0),
                exp: Some(1000),
                raw_exp: Some(700),
                damage_dealt: Some(50000),
                damage_to_buildings: None,
                damage_potential: Some(200000),
                shots_fired: Some(40),
                hits: Some(20),
                frags: Some(1),
                xp_contribution: None,
                ribbons_torpedo_hits: Some(0),
                planes_killed: Some(0),
                ribbons_hits: Some(5),
                spotting_damage: Some(12345),
                damage_received: Some(6789),
                credits: Some(192382),
                afk: Some(false),
                survived: Some(true),
                is_self: true,
                won: Some(true),
                interactions: Vec::new(),
                damage_dealt_by_type: Default::default(),
                detection: Default::default(),
                modules: Default::default(),
                damage_main_by_shell: Default::default(),
                economics: None,
                achievements: Vec::new(),
                main_hits_quality: Default::default(),
                secondary_hits: 0,
                torpedo_protection_hits: 0,
                ship_efficiency: None,
                economic_bonuses: None,
                ribbons: std::collections::BTreeMap::new(),
                victory_points: std::collections::BTreeMap::new(),
                build: None,
            }],
        }
    }

    /// Start a bridge with an injected stub decode_fn.  The counter is shared
    /// so callers can assert how many times the function was called (cache test).
    fn start_bridge_with_stub_decode(
        tmp: &TempDir,
        result: Result<BattleData, DecodeError>,
        call_counter: Arc<AtomicU32>,
    ) -> Bridge {
        let decode_fn: DecodeFn = Arc::new(move |_path, _cfg, _tables| {
            call_counter.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            result.clone()
        });
        let ctx = Arc::new(DecodeContext::with_decode_fn(
            stub_config(),
            stub_tables(),
            decode_fn,
        ));
        start_on_ports_full(tmp.path().to_path_buf(), None, &[0], None, Some(ctx))
            .expect("bridge start failed")
    }

    // ── Helper: error results need Clone ─────────────────────────────────────

    impl Clone for DecodeError {
        fn clone(&self) -> Self {
            match self {
                DecodeError::NoBattleResults => DecodeError::NoBattleResults,
                DecodeError::Resources(s) => DecodeError::Resources(s.clone()),
                DecodeError::Malformed(s) => DecodeError::Malformed(s.clone()),
                DecodeError::Io(e) => DecodeError::Resources(format!("io: {e}")),
            }
        }
    }

    // ── 501 when feature off ──────────────────────────────────────────────────

    /// /v1/replays/game.wowsreplay/result returns 501 when decode ctx is None.
    #[test]
    fn result_501_when_feature_off() {
        let tmp = TempDir::new().unwrap();
        fs::write(tmp.path().join("game.wowsreplay"), b"data").unwrap();
        // start_test_bridge uses start_on_ports with None decode ctx.
        let bridge = start_test_bridge(&tmp);
        let url = format!(
            "http://127.0.0.1:{}/v1/replays/game.wowsreplay/result",
            bridge.port()
        );
        let (status, _, _) = get(&url);
        assert_eq!(status, 501, "must return 501 when decode feature is off");
        bridge.stop();
    }

    /// /v1/replays/latest/result returns 501 when decode ctx is None.
    #[test]
    fn latest_result_501_when_feature_off() {
        let tmp = TempDir::new().unwrap();
        fs::write(tmp.path().join("game.wowsreplay"), b"data").unwrap();
        let bridge = start_test_bridge(&tmp);
        let url = format!(
            "http://127.0.0.1:{}/v1/replays/latest/result",
            bridge.port()
        );
        let (status, _, _) = get(&url);
        assert_eq!(
            status, 501,
            "latest/result must return 501 when decode feature is off"
        );
        bridge.stop();
    }

    // ── Health omits / includes battle-result-v1 capability ──────────────────

    /// Health must NOT include "battle-result-v1" when decode ctx is None.
    #[test]
    fn health_omits_battle_result_cap_when_feature_off() {
        let tmp = TempDir::new().unwrap();
        let bridge = start_test_bridge(&tmp);
        let url = format!("http://127.0.0.1:{}/v1/health", bridge.port());
        let (status, body, _) = get(&url);
        assert_eq!(status, 200);
        let v: serde_json::Value = serde_json::from_str(&body).unwrap();
        let caps: Vec<&str> = v["capabilities"]
            .as_array()
            .unwrap()
            .iter()
            .map(|c| c.as_str().unwrap())
            .collect();
        assert!(
            !caps.contains(&"battle-result-v1"),
            "health must NOT include 'battle-result-v1' when feature off, got: {caps:?}"
        );
        bridge.stop();
    }

    /// Health MUST include "battle-result-v1" when decode ctx is provided.
    #[test]
    fn health_includes_battle_result_cap_when_feature_on() {
        let tmp = TempDir::new().unwrap();
        let counter = Arc::new(AtomicU32::new(0));
        let bridge = start_bridge_with_stub_decode(&tmp, Ok(stub_battle_data("abc")), counter);
        let url = format!("http://127.0.0.1:{}/v1/health", bridge.port());
        let (status, body, _) = get(&url);
        assert_eq!(status, 200);
        let v: serde_json::Value = serde_json::from_str(&body).unwrap();
        let caps: Vec<&str> = v["capabilities"]
            .as_array()
            .unwrap()
            .iter()
            .map(|c| c.as_str().unwrap())
            .collect();
        assert!(
            caps.contains(&"battle-result-v1"),
            "health MUST include 'battle-result-v1' when feature on, got: {caps:?}"
        );
        bridge.stop();
    }

    // ── 200 OK with valid decode ──────────────────────────────────────────────

    /// /result returns 200 + valid BattleData JSON with is_self true for owner.
    #[test]
    fn result_200_ok_with_battle_data() {
        let tmp = TempDir::new().unwrap();
        fs::write(tmp.path().join("game.wowsreplay"), b"data").unwrap();
        let data = stub_battle_data("deadbeef");
        let counter = Arc::new(AtomicU32::new(0));
        let bridge = start_bridge_with_stub_decode(&tmp, Ok(data), counter);
        let url = format!(
            "http://127.0.0.1:{}/v1/replays/game.wowsreplay/result",
            bridge.port()
        );
        let (status, body, _) = get(&url);
        assert_eq!(status, 200);
        let v: serde_json::Value = serde_json::from_str(&body).expect("must be valid JSON");
        assert_eq!(v["meta"]["schema_version"], "1.0");
        assert!(v["players"].is_array());
        let players = v["players"].as_array().unwrap();
        assert_eq!(players.len(), 1);
        assert_eq!(players[0]["is_self"], true);
        bridge.stop();
    }

    // ── 404 for NoBattleResults ───────────────────────────────────────────────

    #[test]
    fn result_404_no_battle_results() {
        let tmp = TempDir::new().unwrap();
        fs::write(tmp.path().join("game.wowsreplay"), b"data").unwrap();
        let counter = Arc::new(AtomicU32::new(0));
        let bridge =
            start_bridge_with_stub_decode(&tmp, Err(DecodeError::NoBattleResults), counter);
        let url = format!(
            "http://127.0.0.1:{}/v1/replays/game.wowsreplay/result",
            bridge.port()
        );
        let (status, body, _) = get(&url);
        assert_eq!(status, 404, "NoBattleResults must map to 404");
        let v: serde_json::Value = serde_json::from_str(&body).unwrap();
        assert_eq!(
            v["error"],
            "no battle result (battle not finished or left early)"
        );
        bridge.stop();
    }

    // ── 504 for Timeout ──────────────────────────────────────────────────────

    // ── 500 for decode failure (Malformed) ───────────────────────────────────

    #[test]
    fn result_500_decode_failed() {
        let tmp = TempDir::new().unwrap();
        fs::write(tmp.path().join("game.wowsreplay"), b"data").unwrap();
        let counter = Arc::new(AtomicU32::new(0));
        let bridge = start_bridge_with_stub_decode(
            &tmp,
            Err(DecodeError::Malformed("internal detail".into())),
            counter,
        );
        let url = format!(
            "http://127.0.0.1:{}/v1/replays/game.wowsreplay/result",
            bridge.port()
        );
        let (status, body, _) = get(&url);
        assert_eq!(status, 500, "decode failure must map to 500");
        // Must NOT leak internal error detail to the client.
        assert!(
            !body.contains("internal detail"),
            "client response must not contain internal error detail: {body}"
        );
        bridge.stop();
    }

    // ── 404 for missing file ──────────────────────────────────────────────────

    #[test]
    fn result_404_missing_file() {
        let tmp = TempDir::new().unwrap();
        // Do NOT write the file — it must be absent.
        let counter = Arc::new(AtomicU32::new(0));
        let bridge = start_bridge_with_stub_decode(&tmp, Ok(stub_battle_data("x")), counter);
        let url = format!(
            "http://127.0.0.1:{}/v1/replays/nonexistent.wowsreplay/result",
            bridge.port()
        );
        let (status, _, _) = get(&url);
        assert_eq!(status, 404, "missing file must return 404");
        bridge.stop();
    }

    // ── 400 for traversal ────────────────────────────────────────────────────

    #[test]
    fn result_traversal_not_200() {
        let tmp = TempDir::new().unwrap();
        let counter = Arc::new(AtomicU32::new(0));
        let bridge = start_bridge_with_stub_decode(&tmp, Ok(stub_battle_data("x")), counter);
        let url = format!(
            "http://127.0.0.1:{}/v1/replays/..%2Fsecret.wowsreplay/result",
            bridge.port()
        );
        let (status, _, _) = get(&url);
        assert_ne!(status, 200, "traversal must not return 200");
        bridge.stop();
    }

    // ── 404 for latest when no replays ──────────────────────────────────────

    #[test]
    fn latest_result_404_no_replays() {
        let tmp = TempDir::new().unwrap();
        let counter = Arc::new(AtomicU32::new(0));
        let bridge = start_bridge_with_stub_decode(&tmp, Ok(stub_battle_data("x")), counter);
        let url = format!(
            "http://127.0.0.1:{}/v1/replays/latest/result",
            bridge.port()
        );
        let (status, _, _) = get(&url);
        assert_eq!(status, 404, "latest/result must return 404 when no replays");
        bridge.stop();
    }

    // ── 200 for latest/result when replay exists ─────────────────────────────

    #[test]
    fn latest_result_200_when_replay_exists() {
        let tmp = TempDir::new().unwrap();
        fs::write(tmp.path().join("game.wowsreplay"), b"data").unwrap();
        let counter = Arc::new(AtomicU32::new(0));
        let bridge = start_bridge_with_stub_decode(&tmp, Ok(stub_battle_data("cafebabe")), counter);
        let url = format!(
            "http://127.0.0.1:{}/v1/replays/latest/result",
            bridge.port()
        );
        let (status, body, _) = get(&url);
        assert_eq!(
            status, 200,
            "latest/result must return 200 when replay exists"
        );
        let v: serde_json::Value = serde_json::from_str(&body).unwrap();
        assert_eq!(v["meta"]["schema_version"], "1.0");
        bridge.stop();
    }

    // ── CORS on /result ──────────────────────────────────────────────────────

    /// /result must carry CORS headers for the allowed origin.
    #[test]
    fn result_cors_allowed_origin() {
        let tmp = TempDir::new().unwrap();
        fs::write(tmp.path().join("game.wowsreplay"), b"data").unwrap();
        let counter = Arc::new(AtomicU32::new(0));
        let bridge = start_bridge_with_stub_decode(&tmp, Ok(stub_battle_data("hash1")), counter);
        let url = format!(
            "http://127.0.0.1:{}/v1/replays/game.wowsreplay/result",
            bridge.port()
        );
        let (status, _, headers) = get(&url);
        assert_eq!(status, 200);
        let acao = find_header(&headers, "access-control-allow-origin");
        assert_eq!(
            acao.as_deref(),
            Some("https://engine.tfd.rocks"),
            "ACAO header must be set for allowed origin on /result"
        );
        bridge.stop();
    }

    // ── Cache: decode_fn called once for two identical requests ──────────────

    /// Two identical requests to /result must invoke the decode_fn only once;
    /// the second response comes from the cache.
    #[test]
    fn result_cache_decode_fn_called_once() {
        let tmp = TempDir::new().unwrap();
        fs::write(tmp.path().join("game.wowsreplay"), b"data").unwrap();
        let counter = Arc::new(AtomicU32::new(0));
        let counter_clone = Arc::clone(&counter);
        let bridge =
            start_bridge_with_stub_decode(&tmp, Ok(stub_battle_data("cachehash")), counter_clone);
        let url = format!(
            "http://127.0.0.1:{}/v1/replays/game.wowsreplay/result",
            bridge.port()
        );
        // First request — triggers the decode.
        let (s1, body1, _) = get(&url);
        assert_eq!(s1, 200);
        // Second request — must be served from cache.
        let (s2, body2, _) = get(&url);
        assert_eq!(s2, 200);
        // Bodies must be identical.
        assert_eq!(body1, body2, "cached response must match original");
        // decode_fn must have been called exactly once.
        let calls = counter.load(std::sync::atomic::Ordering::SeqCst);
        assert_eq!(
            calls, 1,
            "decode_fn must be called once (second hit from cache), got {calls}"
        );
        bridge.stop();
    }

    // ── Regression: crafted /v1/replays/result must not panic ───────────────

    /// GET /v1/replays/result (no name segment — prefix and suffix overlap) must
    /// return a non-200 status code (404) and must NOT crash the handler thread.
    /// A subsequent request must still succeed, proving the bridge is still alive.
    #[test]
    fn result_no_name_segment_returns_non_200_and_bridge_survives() {
        let tmp = TempDir::new().unwrap();
        let counter = Arc::new(AtomicU32::new(0));
        let bridge = start_bridge_with_stub_decode(&tmp, Ok(stub_battle_data("x")), counter);
        let port = bridge.port();

        // Crafted path that previously caused a slice-bounds panic.
        let (status, _, _) = get(&format!("http://127.0.0.1:{port}/v1/replays/result"));
        assert_ne!(
            status, 200,
            "ambiguous /v1/replays/result must not return 200"
        );

        // Bridge must still be alive and serving health.
        let (health_status, _, _) = get(&format!("http://127.0.0.1:{port}/v1/health"));
        assert_eq!(
            health_status, 200,
            "bridge must still serve /v1/health after the crafted request"
        );
        bridge.stop();
    }

    // ── 404 for non-.wowsreplay file via /result ─────────────────────────────

    /// Requesting /result for a file that exists but is not .wowsreplay → 404.
    #[test]
    fn result_404_non_replay_extension() {
        let tmp = TempDir::new().unwrap();
        fs::write(tmp.path().join("notes.txt"), b"text").unwrap();
        let counter = Arc::new(AtomicU32::new(0));
        let bridge = start_bridge_with_stub_decode(&tmp, Ok(stub_battle_data("x")), counter);
        let url = format!(
            "http://127.0.0.1:{}/v1/replays/notes.txt/result",
            bridge.port()
        );
        let (status, _, _) = get(&url);
        assert_eq!(
            status, 404,
            "non-.wowsreplay file must return 404 via /result"
        );
        bridge.stop();
    }
}
