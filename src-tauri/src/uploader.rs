//! Replay-donation upload pipeline: hash ledger, finalize-triggered uploads,
//! and the throttled 30-day backfill (td-c8973d).
//!
//! The uploader consumes the building blocks merged before it: the engine
//! HTTP client + remote feature flags (`engine.rs`, td-e99d65), the
//! replay-finalized events from the bridge watcher (bridge-core `finalize.rs`,
//! td-445f39), and the tri-state donation consent (`donation.rs`, td-bd6b98).
//!
//! Contract (live in production; source of truth:
//! `tfd_engine/api/replay_donation.py` in github.com/Helmi/tfd-engine):
//! `POST /api/v1/bridge/donate-replay` — multipart/form-data with `file` (the
//! `.wowsreplay` bytes), `sha256` (hex form field, re-computed and verified
//! server-side) and optional client meta `filename` + `mtime` (Unix seconds).
//! Responses: 202 accepted / 200 duplicate (both terminal success — the
//! server dedups by content hash), 400/413/415/422 (this file can never
//! succeed — terminal rejection), 426 upgrade required (stop until the next
//! app start), 429 rate limited (20/h per IP — wait long), 503 feature
//! disabled or storage unconfigured (wait much longer, never an error loop).
//!
//! Pipeline rules:
//! - **Ledger** (`donation-ledger.json` in the app data dir): sha256 →
//!   (path, status, mtime, size).  An accepted hash is never uploaded again.
//!   The ledger is an optimization — the server's hash dedup stays
//!   authoritative, so a lost/corrupt ledger only costs duplicate-answered
//!   uploads, never duplicate stored data.
//! - **Triggers**: (1) every replay-finalized event (live battle ends AND the
//!   detector's startup watermark catch-up) enqueues that file; (2)+(3)
//!   [`Uploader::spawn_scan`] — run at startup while opted in, and on each
//!   opt-in — walks the replays folder for `.wowsreplay` files modified
//!   within the last 30 days that are not in the ledger, oldest first.  The
//!   same scan is the one-time backfill on opt-in, the restart-survival path
//!   (interrupted uploads are not in the ledger and get re-queued), and the
//!   recovery for events the finalize watermark raced past.
//! - **Throttle**: deliberately low priority — one file at a time on a single
//!   worker, a multi-minute pause between files (12/h worst case, far below
//!   the engine's 20/h per-IP rate limit), and a FULL pause while a live
//!   battle is running (`tempArenaInfo.json` present) so uploads never
//!   compete with the game for bandwidth.
//! - **Gating**: enqueueing requires consent == opted-in (the privacy gate);
//!   the upload itself additionally requires the engine's `replay_donation`
//!   feature flag (the remote kill switch).  Revoking consent clears the
//!   queue and stops the worker before its next request — an in-flight
//!   upload may complete, nothing new starts.  A server-side flag flip
//!   pauses uploads within the hourly config refresh and resumes when the
//!   flag returns.

use bridge_core::finalize::{is_structurally_complete, TEMP_ARENA_INFO};
use bridge_core::server::{list_replays, ReplayEntry};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::{HashMap, HashSet, VecDeque};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::thread;
use std::time::{Duration, Instant, UNIX_EPOCH};

// ── Constants ────────────────────────────────────────────────────────────────

/// Client-side size sanity cap — mirrors the server's 30 MB limit
/// (`TFD_REPLAY_MAX_FILE_BYTES` default); anything larger is skipped without
/// a request.
pub const MAX_UPLOAD_BYTES: u64 = 30 * 1024 * 1024;

/// Backfill window: only replays modified within the last 30 days are ever
/// picked up by a scan.  Older files are deliberately never uploaded.
pub const BACKFILL_WINDOW_MS: u64 = 30 * 24 * 60 * 60 * 1000;

/// Pause between consecutive uploads.  Five minutes caps the steady rate at
/// 12 uploads/hour — far below the engine's 20/h per-IP limit — and a single
/// just-finished battle still uploads immediately (the pause only spaces
/// queued bursts like the backfill).
pub const DEFAULT_PAUSE_BETWEEN_FILES: Duration = Duration::from_secs(5 * 60);
/// How often to re-check for battle end while paused on a live battle.
pub const DEFAULT_LIVE_POLL: Duration = Duration::from_secs(15);
/// How often to re-check the remote feature flag while it reads disabled.
pub const DEFAULT_FLAG_POLL: Duration = Duration::from_secs(60);
/// First retry delay after a transient failure; doubles per attempt.
pub const DEFAULT_BACKOFF_BASE: Duration = Duration::from_secs(60);
/// Upper bound on the per-attempt backoff delay.
pub const DEFAULT_BACKOFF_CAP: Duration = Duration::from_secs(15 * 60);
/// Transient-failure attempts per file before deferring to a later scan.
pub const DEFAULT_MAX_ATTEMPTS: u32 = 5;
/// Wait after an HTTP 429 before retrying (the engine said "slow down").
pub const DEFAULT_RATE_LIMIT_PAUSE: Duration = Duration::from_secs(20 * 60);
/// Wait after an HTTP 503 (feature off / storage unconfigured): "stop and
/// retry much later", never a tight error loop.
pub const DEFAULT_UNAVAILABLE_PAUSE: Duration = Duration::from_secs(60 * 60);

/// Per-request timeout for the upload itself — generous because a 30 MB file
/// on a slow uplink needs far longer than the shared client's 10 s default.
const UPLOAD_TIMEOUT: Duration = Duration::from_secs(5 * 60);

/// Granularity of interruptible waits (cancellation/consent checks).
const PAUSE_STEP: Duration = Duration::from_millis(50);

// ── Transport contract ───────────────────────────────────────────────────────

/// One prepared upload, handed to the transport.
pub struct UploadRequest {
    /// Absolute path of the `.wowsreplay` file.
    pub path: PathBuf,
    /// Bare file name — the optional `filename` meta form field.
    pub file_name: String,
    /// File mtime (ms since epoch) — sent as the `mtime` form field (seconds).
    pub mtime_ms: u64,
    /// Lowercase hex SHA-256 of `bytes` — the `sha256` form field.
    pub sha256: String,
    /// The full file content.
    pub bytes: Vec<u8>,
}

/// What an upload attempt resolved to, mapped from the endpoint contract.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UploadOutcome {
    /// 2xx — accepted or already-known duplicate; terminal success.
    Accepted,
    /// 400/413/415/422 — this file can never succeed; terminal rejection.
    Rejected(String),
    /// 429 — engine rate limit; wait long, then retry the same file.
    RateLimited,
    /// 503 — feature disabled or storage unconfigured; wait much longer.
    Unavailable,
    /// 426 — bridge too old; stop uploading until the next app start.
    UpgradeRequired,
    /// Network failure or unexpected status — retry with backoff.
    Transient(String),
}

/// The upload transport.  Production uses [`engine_transport`]; tests inject
/// a recording mock so `cargo test` never touches the network.
pub type TransportFn = Arc<dyn Fn(&UploadRequest) -> UploadOutcome + Send + Sync>;

/// A boolean gate read before every step (consent / feature flag).
pub type GateFn = Arc<dyn Fn() -> bool + Send + Sync>;

// ── Ledger ───────────────────────────────────────────────────────────────────

/// Terminal per-hash upload status.  Only terminal states are persisted —
/// anything else is simply absent and will be retried by a later scan.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LedgerStatus {
    /// Uploaded (or the server already had it) — never upload again.
    Accepted,
    /// The server rejected the content permanently — never upload again.
    Rejected,
}

/// One ledger record, keyed externally by the file's sha256 hex.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LedgerEntry {
    pub path: String,
    pub status: LedgerStatus,
    pub mtime_ms: u64,
    pub size: u64,
}

/// On-disk shape of the ledger file.
#[derive(Debug, Default, Serialize, Deserialize)]
struct LedgerData {
    #[serde(default)]
    entries: HashMap<String, LedgerEntry>,
}

/// The persisted sha256 ledger.  Load is fail-safe: a missing or corrupt
/// file starts empty — worst case the server answers "duplicate" to a few
/// re-uploads (its hash dedup is authoritative).
pub struct Ledger {
    path: PathBuf,
    data: LedgerData,
}

impl Ledger {
    pub fn load(path: PathBuf) -> Self {
        let data = match fs::read(&path) {
            Ok(bytes) => match serde_json::from_slice::<LedgerData>(&bytes) {
                Ok(data) => data,
                Err(e) => {
                    log::warn!(
                        "Donation ledger {} is corrupt ({e}) — starting empty (server dedup covers re-uploads)",
                        path.display()
                    );
                    LedgerData::default()
                }
            },
            Err(_) => LedgerData::default(), // first run — no ledger yet
        };
        Self { path, data }
    }

    /// Terminal status for a content hash, if any.
    pub fn status_of(&self, sha256: &str) -> Option<LedgerStatus> {
        self.data.entries.get(sha256).map(|e| e.status)
    }

    /// `true` when some entry matches this exact path + mtime + size — the
    /// cheap "already handled, don't even hash it" check used by the scan.
    /// A touched/changed file fails the check and gets re-hashed; the hash
    /// lookup then decides.
    pub fn has_path_unchanged(&self, path: &str, mtime_ms: u64, size: u64) -> bool {
        self.data
            .entries
            .values()
            .any(|e| e.path == path && e.mtime_ms == mtime_ms && e.size == size)
    }

    /// Record a terminal status and persist immediately (the ledger must
    /// survive restarts mid-queue).
    pub fn record(&mut self, sha256: String, entry: LedgerEntry) {
        self.data.entries.insert(sha256, entry);
        self.save();
    }

    fn save(&self) {
        if let Some(parent) = self.path.parent() {
            if !parent.as_os_str().is_empty() {
                if let Err(e) = fs::create_dir_all(parent) {
                    log::warn!("Failed to create donation-ledger dir: {e}");
                }
            }
        }
        match serde_json::to_vec_pretty(&self.data) {
            Ok(bytes) => {
                if let Err(e) = fs::write(&self.path, bytes) {
                    log::warn!("Failed to save donation ledger: {e}");
                }
            }
            Err(e) => log::warn!("Failed to serialise donation ledger: {e}"),
        }
    }
}

// ── Uploader ─────────────────────────────────────────────────────────────────

/// Configuration for an [`Uploader`].  Production code uses
/// [`UploaderOptions::production`]; tests shrink the timings and inject mock
/// gates + transport.
pub struct UploaderOptions {
    pub replays_dir: PathBuf,
    pub ledger_path: PathBuf,
    pub pause_between_files: Duration,
    pub live_poll: Duration,
    pub flag_poll: Duration,
    pub backoff_base: Duration,
    pub backoff_cap: Duration,
    pub max_attempts: u32,
    pub rate_limit_pause: Duration,
    pub unavailable_pause: Duration,
    /// The privacy gate: consent == opted-in.  Checked at enqueue and before
    /// every wait step — a revoke stops the pipeline immediately.
    pub consent_gate: GateFn,
    /// The remote kill switch: `engine::features().replay_donation`.  While
    /// false the worker pauses (queue kept) and re-checks periodically.
    pub flag_gate: GateFn,
    pub transport: TransportFn,
}

impl UploaderOptions {
    /// Production wiring: default timings, the real consent + feature-flag
    /// gates, and the engine multipart transport.
    pub fn production(replays_dir: PathBuf, ledger_path: PathBuf) -> Self {
        Self {
            replays_dir,
            ledger_path,
            pause_between_files: DEFAULT_PAUSE_BETWEEN_FILES,
            live_poll: DEFAULT_LIVE_POLL,
            flag_poll: DEFAULT_FLAG_POLL,
            backoff_base: DEFAULT_BACKOFF_BASE,
            backoff_cap: DEFAULT_BACKOFF_CAP,
            max_attempts: DEFAULT_MAX_ATTEMPTS,
            rate_limit_pause: DEFAULT_RATE_LIMIT_PAUSE,
            unavailable_pause: DEFAULT_UNAVAILABLE_PAUSE,
            consent_gate: Arc::new(|| crate::donation::consent().is_opted_in()),
            flag_gate: Arc::new(|| crate::engine::features().replay_donation),
            transport: engine_transport(),
        }
    }
}

/// FIFO upload queue with path-level dedup (a file already waiting is not
/// queued twice; the ledger handles dedup across uploads).
#[derive(Default)]
struct QueueState {
    jobs: VecDeque<PathBuf>,
    queued: HashSet<PathBuf>,
}

/// Outcome of a gate/cancellation wait.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Clear {
    Proceed,
    Stop,
}

/// The donation upload pipeline: one worker thread draining a queue fed by
/// finalized events and folder scans, gated, throttled, ledger-backed.
/// Created per active bridge (`lib.rs` owns the lifecycle); `cancel()` stops
/// the worker.
pub struct Uploader {
    replays_dir: PathBuf,
    ledger: Mutex<Ledger>,
    queue: Mutex<QueueState>,
    queue_cv: Condvar,
    cancelled: AtomicBool,
    /// Latched on HTTP 426: no more uploads until the next app start (the
    /// updater nudge / hourly check handles getting the user onto a new
    /// version; an update restarts the app).
    latched_off: AtomicBool,
    pause_between_files: Duration,
    live_poll: Duration,
    flag_poll: Duration,
    backoff_base: Duration,
    backoff_cap: Duration,
    max_attempts: u32,
    rate_limit_pause: Duration,
    unavailable_pause: Duration,
    consent_gate: GateFn,
    flag_gate: GateFn,
    transport: TransportFn,
}

impl Uploader {
    /// Build the uploader and start its worker thread.
    pub fn new(opts: UploaderOptions) -> Arc<Self> {
        let uploader = Arc::new(Self {
            replays_dir: opts.replays_dir,
            ledger: Mutex::new(Ledger::load(opts.ledger_path)),
            queue: Mutex::new(QueueState::default()),
            queue_cv: Condvar::new(),
            cancelled: AtomicBool::new(false),
            latched_off: AtomicBool::new(false),
            pause_between_files: opts.pause_between_files,
            live_poll: opts.live_poll,
            flag_poll: opts.flag_poll,
            backoff_base: opts.backoff_base,
            backoff_cap: opts.backoff_cap,
            max_attempts: opts.max_attempts,
            rate_limit_pause: opts.rate_limit_pause,
            unavailable_pause: opts.unavailable_pause,
            consent_gate: opts.consent_gate,
            flag_gate: opts.flag_gate,
            transport: opts.transport,
        });
        let worker = Arc::clone(&uploader);
        thread::spawn(move || worker.run_worker());
        uploader
    }

    /// Trigger (1): a replay just finalized (live battle end, or the
    /// detector's watermark catch-up).  Quick by design — called from the
    /// finalize callback, which runs under the detector's watermark lock.
    pub fn on_replay_finalized(&self, path: &Path) {
        if !is_replay_file(path) {
            return; // never tempArenaInfo.json or other non-replay files
        }
        self.enqueue(path.to_path_buf());
    }

    /// Triggers (2)+(3): scan the replays folder for donate-able files —
    /// `.wowsreplay`, modified within the last 30 days, within the size cap,
    /// not in the ledger — and enqueue them oldest first.  Run at startup
    /// while opted in (catch-up + restart survival) and on each opt-in (the
    /// one-time backfill).  Idempotent: the ledger and the queue's path
    /// dedup make repeat scans cheap no-ops.
    pub fn spawn_scan(self: &Arc<Self>) {
        let uploader = Arc::clone(self);
        thread::spawn(move || uploader.scan_and_enqueue());
    }

    /// Consent was revoked: drop everything queued.  The worker also checks
    /// the consent gate before every step, so nothing new starts; an
    /// in-flight request may complete.
    pub fn revoke(&self) {
        log::info!("Donation consent revoked — clearing the upload queue");
        self.clear_queue();
    }

    /// Stop the pipeline: the worker exits and in-flight waits bail at the
    /// next checkpoint.  Called on bridge restart/replacement and app exit.
    pub fn cancel(&self) {
        self.cancelled.store(true, Ordering::SeqCst);
        self.queue_cv.notify_all();
    }

    // ── Queue ────────────────────────────────────────────────────────────────

    fn enqueue(&self, path: PathBuf) {
        if self.cancelled() || self.latched() {
            return;
        }
        if !(self.consent_gate)() {
            return; // privacy gate: nothing queues without an explicit opt-in
        }
        let mut queue = self.queue.lock().unwrap();
        if queue.queued.insert(path.clone()) {
            log::debug!("Donation queued: {}", path.display());
            queue.jobs.push_back(path);
            self.queue_cv.notify_all();
        }
    }

    fn clear_queue(&self) {
        let mut queue = self.queue.lock().unwrap();
        queue.jobs.clear();
        queue.queued.clear();
    }

    /// Block until a job is available; `None` once cancelled.
    fn pop_job(&self) -> Option<PathBuf> {
        let mut queue = self.queue.lock().unwrap();
        loop {
            if self.cancelled() {
                return None;
            }
            if let Some(path) = queue.jobs.pop_front() {
                queue.queued.remove(&path);
                return Some(path);
            }
            let (guard, _) = self
                .queue_cv
                .wait_timeout(queue, Duration::from_millis(200))
                .unwrap();
            queue = guard;
        }
    }

    // ── Worker ───────────────────────────────────────────────────────────────

    fn run_worker(&self) {
        log::debug!("Donation upload worker started");
        while let Some(path) = self.pop_job() {
            self.process_job(&path);
        }
        log::debug!("Donation upload worker stopped");
    }

    /// Handle one queued file end-to-end: wait for the gates + battle end,
    /// validate, hash, dedup against the ledger, upload, record the outcome.
    fn process_job(&self, path: &Path) {
        if !is_replay_file(path) {
            return;
        }
        let mut attempts: u32 = 0;
        loop {
            if self.wait_until_clear() == Clear::Stop {
                return;
            }
            // Validate AFTER the live-battle wait: the file is only read once
            // any battle is over, so an in-progress archive is never hashed
            // or uploaded half-written.
            if !is_structurally_complete(path) {
                log::info!(
                    "Donation: {} is not a complete replay — skipped (no ledger entry; a later scan re-checks)",
                    path.display()
                );
                return;
            }
            let Ok(meta) = fs::metadata(path) else {
                return; // vanished — drop silently
            };
            if meta.len() == 0 || meta.len() > MAX_UPLOAD_BYTES {
                log::info!(
                    "Donation: {} is {} bytes — outside the size cap, skipped",
                    path.display(),
                    meta.len()
                );
                return;
            }
            let mtime_ms = meta
                .modified()
                .ok()
                .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
                .map(|d| d.as_millis() as u64)
                .unwrap_or(0);
            let Ok(bytes) = fs::read(path) else {
                log::warn!("Donation: cannot read {} — skipped", path.display());
                return;
            };
            let sha256 = sha256_hex(&bytes);
            if let Some(status) = self.ledger.lock().unwrap().status_of(&sha256) {
                log::debug!(
                    "Donation: {} already {status:?} in the ledger — skipped",
                    path.display()
                );
                return;
            }

            let request = UploadRequest {
                path: path.to_path_buf(),
                file_name: path
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_default(),
                mtime_ms,
                sha256: sha256.clone(),
                bytes,
            };
            log::info!(
                "Donating replay {} ({} bytes, attempt {})",
                request.file_name,
                request.bytes.len(),
                attempts + 1
            );

            match (self.transport)(&request) {
                UploadOutcome::Accepted => {
                    self.record(sha256, &request, LedgerStatus::Accepted);
                    log::info!("Donation accepted: {}", request.file_name);
                    self.pause(self.pause_between_files);
                    return;
                }
                UploadOutcome::Rejected(reason) => {
                    self.record(sha256, &request, LedgerStatus::Rejected);
                    log::warn!(
                        "Donation rejected permanently ({reason}): {}",
                        request.file_name
                    );
                    self.pause(self.pause_between_files);
                    return;
                }
                UploadOutcome::UpgradeRequired => {
                    log::warn!(
                        "Engine demands a newer bridge — pausing replay donation until the next app start"
                    );
                    self.latched_off.store(true, Ordering::SeqCst);
                    self.clear_queue();
                    return;
                }
                UploadOutcome::RateLimited => {
                    log::warn!(
                        "Donation rate-limited by the engine — waiting before retrying {}",
                        request.file_name
                    );
                    if !self.pause(self.rate_limit_pause) {
                        return;
                    }
                }
                UploadOutcome::Unavailable => {
                    log::warn!(
                        "Donation unavailable (feature off or storage unconfigured) — retrying much later"
                    );
                    if !self.pause(self.unavailable_pause) {
                        return;
                    }
                }
                UploadOutcome::Transient(reason) => {
                    attempts += 1;
                    if attempts >= self.max_attempts {
                        log::warn!(
                            "Donation failed {attempts} times ({reason}) — deferring {} to a later scan",
                            request.file_name
                        );
                        return; // not in the ledger → the next scan retries it
                    }
                    log::warn!("Donation attempt {attempts} failed ({reason}) — backing off");
                    if !self.pause(backoff_delay(self.backoff_base, self.backoff_cap, attempts)) {
                        return;
                    }
                }
            }
        }
    }

    /// Wait until uploading is allowed: cancelled/latched → stop; consent
    /// revoked → clear the queue and stop; flag off or battle live → wait
    /// and re-check.
    fn wait_until_clear(&self) -> Clear {
        loop {
            if self.cancelled() || self.latched() {
                return Clear::Stop;
            }
            if !(self.consent_gate)() {
                // Revoking consent stops the pipeline immediately, queue
                // included; an in-flight request may complete, nothing new starts.
                self.clear_queue();
                return Clear::Stop;
            }
            if !(self.flag_gate)() {
                // Remote kill switch: keep the queue, wait for the flag to
                // return (lib.rs refreshes the engine config hourly).
                if !self.pause(self.flag_poll) {
                    return Clear::Stop;
                }
                continue;
            }
            if self.live_battle() {
                // Never compete with a running battle for bandwidth.
                if !self.pause(self.live_poll) {
                    return Clear::Stop;
                }
                continue;
            }
            return Clear::Proceed;
        }
    }

    /// Interruptible sleep: `false` when cancelled, latched, or consent was
    /// revoked mid-wait (queue cleared in that case); `true` after the full
    /// duration.
    fn pause(&self, total: Duration) -> bool {
        let deadline = Instant::now() + total;
        loop {
            if self.cancelled() || self.latched() {
                return false;
            }
            if !(self.consent_gate)() {
                self.clear_queue();
                return false;
            }
            let now = Instant::now();
            if now >= deadline {
                return true;
            }
            thread::sleep(PAUSE_STEP.min(deadline - now));
        }
    }

    fn scan_and_enqueue(&self) {
        if self.cancelled() || self.latched() || !(self.consent_gate)() {
            return;
        }
        let entries = match list_replays(&self.replays_dir) {
            Ok(entries) => entries,
            Err(e) => {
                log::warn!("Donation scan: cannot list replays dir: {e}");
                return;
            }
        };
        let candidates = {
            let ledger = self.ledger.lock().unwrap();
            select_candidates(&entries, now_ms(), &ledger, &self.replays_dir)
        };
        if candidates.is_empty() {
            log::info!("Donation scan: nothing new to upload");
            return;
        }
        log::info!(
            "Donation scan: queueing {} replay(s), oldest first",
            candidates.len()
        );
        for entry in candidates {
            self.enqueue(self.replays_dir.join(&entry.name));
        }
    }

    fn record(&self, sha256: String, request: &UploadRequest, status: LedgerStatus) {
        let entry = LedgerEntry {
            path: request.path.to_string_lossy().into_owned(),
            status,
            mtime_ms: request.mtime_ms,
            size: request.bytes.len() as u64,
        };
        self.ledger.lock().unwrap().record(sha256, entry);
    }

    fn live_battle(&self) -> bool {
        self.replays_dir.join(TEMP_ARENA_INFO).exists()
    }

    fn cancelled(&self) -> bool {
        self.cancelled.load(Ordering::SeqCst)
    }

    fn latched(&self) -> bool {
        self.latched_off.load(Ordering::SeqCst)
    }
}

// ── Engine transport ─────────────────────────────────────────────────────────

/// The production transport: POST the multipart donation to the engine
/// through the shared client (which carries the bridge version headers).
pub(crate) fn engine_transport() -> TransportFn {
    Arc::new(|request| {
        tauri::async_runtime::block_on(send_donation(
            crate::engine::shared_client(),
            &crate::engine::endpoint(crate::engine::DONATE_REPLAY_PATH),
            request,
        ))
    })
}

/// Send one donation request.  Parameterised over client + URL so tests run
/// it against a local mock server — the real engine is never contacted from
/// `cargo test`.
async fn send_donation(
    client: &reqwest::Client,
    url: &str,
    request: &UploadRequest,
) -> UploadOutcome {
    let (content_type, body) = multipart_body(request);
    let response = match client
        .post(url)
        .header("Content-Type", content_type)
        .timeout(UPLOAD_TIMEOUT)
        .body(body)
        .send()
        .await
    {
        Ok(r) => r,
        Err(e) => return UploadOutcome::Transient(format!("request failed: {e}")),
    };
    let status = response.status().as_u16();
    let body = response.text().await.unwrap_or_default();
    map_response(status, &body)
}

/// Map an endpoint response to an [`UploadOutcome`] per the live contract.
fn map_response(status: u16, body: &str) -> UploadOutcome {
    match status {
        // 202 accepted / 200 duplicate — both terminal success.
        200..=299 => UploadOutcome::Accepted,
        // empty/hash-mismatch (400), too large (413), bad content type (415),
        // invalid replay (422): this exact content can never succeed.
        400 | 413 | 415 | 422 => UploadOutcome::Rejected(short_error(status, body)),
        426 => UploadOutcome::UpgradeRequired,
        429 => UploadOutcome::RateLimited,
        503 => UploadOutcome::Unavailable,
        _ => UploadOutcome::Transient(short_error(status, body)),
    }
}

/// Compact error string for logs — status plus a truncated body.
fn short_error(status: u16, body: &str) -> String {
    let snippet: String = body.chars().take(200).collect();
    format!("HTTP {status}: {snippet}")
}

/// Build the multipart/form-data body for the donation endpoint:
/// `sha256` + `filename` + `mtime` text fields, then the `file` part.
/// The boundary is derived from the content hash, so the file bytes cannot
/// contain it (a file embedding its own SHA-256 is a fixed point — not a
/// thing).  Returns `(content-type header value, body bytes)`.
fn multipart_body(request: &UploadRequest) -> (String, Vec<u8>) {
    let boundary = format!("tfd-bridge-{}", &request.sha256[..request.sha256.len().min(32)]);
    let file_name = sanitize_header_value(&request.file_name);
    let mut body = Vec::with_capacity(request.bytes.len() + 512);

    let text_field = |body: &mut Vec<u8>, name: &str, value: &str| {
        body.extend_from_slice(
            format!(
                "--{boundary}\r\nContent-Disposition: form-data; name=\"{name}\"\r\n\r\n{value}\r\n"
            )
            .as_bytes(),
        );
    };
    text_field(&mut body, "sha256", &request.sha256);
    text_field(&mut body, "filename", &file_name);
    // The endpoint takes mtime as a float Unix timestamp in seconds.
    text_field(
        &mut body,
        "mtime",
        &format!("{:.3}", request.mtime_ms as f64 / 1000.0),
    );

    body.extend_from_slice(
        format!(
            "--{boundary}\r\nContent-Disposition: form-data; name=\"file\"; filename=\"{file_name}\"\r\nContent-Type: application/octet-stream\r\n\r\n"
        )
        .as_bytes(),
    );
    body.extend_from_slice(&request.bytes);
    body.extend_from_slice(format!("\r\n--{boundary}--\r\n").as_bytes());

    (format!("multipart/form-data; boundary={boundary}"), body)
}

/// Strip characters that would break the multipart headers out of a file
/// name (quotes, CR/LF).  Replay names never contain them — defence in depth.
fn sanitize_header_value(value: &str) -> String {
    value.replace(['"', '\r', '\n'], "_")
}

// ── Pure helpers ─────────────────────────────────────────────────────────────

/// Lowercase hex SHA-256 of `bytes` — the ledger key and the `sha256` field.
pub fn sha256_hex(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}

/// Exponential backoff: `base * 2^(attempt-1)`, capped.
fn backoff_delay(base: Duration, cap: Duration, attempt: u32) -> Duration {
    let factor = 1u32 << attempt.saturating_sub(1).min(16);
    cap.min(base.saturating_mul(factor))
}

/// Select scan candidates: `.wowsreplay` only, nonzero and within the size
/// cap, modified within the backfill window, not already in the ledger for
/// this exact path+mtime+size — sorted oldest first.
fn select_candidates(
    entries: &[ReplayEntry],
    now_ms: u64,
    ledger: &Ledger,
    replays_dir: &Path,
) -> Vec<ReplayEntry> {
    let mut candidates: Vec<ReplayEntry> = entries
        .iter()
        .filter(|e| {
            e.name.to_ascii_lowercase().ends_with(".wowsreplay")
                && e.size > 0
                && e.size <= MAX_UPLOAD_BYTES
                && now_ms.saturating_sub(e.modified_ms) <= BACKFILL_WINDOW_MS
                && !ledger.has_path_unchanged(
                    &replays_dir.join(&e.name).to_string_lossy(),
                    e.modified_ms,
                    e.size,
                )
        })
        .cloned()
        .collect();
    candidates.sort_by_key(|e| e.modified_ms);
    candidates
}

/// `true` only for `.wowsreplay` archives (case-insensitive) — the uploader
/// must never touch `tempArenaInfo.json` or any other file.
fn is_replay_file(path: &Path) -> bool {
    path.extension()
        .map(|ext| ext.eq_ignore_ascii_case("wowsreplay"))
        .unwrap_or(false)
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

// ── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use bridge_core::finalize::REPLAY_MAGIC;
    use std::sync::mpsc;
    use tempfile::TempDir;

    // ── Fixtures ─────────────────────────────────────────────────────────────

    /// Valid `.wowsreplay` bytes (magic + block count + JSON block + payload);
    /// `tag` varies the content so different files get different hashes.
    fn replay_bytes(tag: &str) -> Vec<u8> {
        let json = format!(r#"{{"playerName":"helmi","tag":"{tag}"}}"#);
        let json_bytes = json.as_bytes();
        let mut buf = Vec::new();
        buf.extend_from_slice(&REPLAY_MAGIC.to_le_bytes());
        buf.extend_from_slice(&2u32.to_le_bytes());
        buf.extend_from_slice(&(json_bytes.len() as u32).to_le_bytes());
        buf.extend_from_slice(json_bytes);
        buf.extend_from_slice(b"packet-payload");
        buf
    }

    fn write_replay(dir: &Path, name: &str, tag: &str) -> PathBuf {
        let path = dir.join(name);
        fs::write(&path, replay_bytes(tag)).unwrap();
        path
    }

    /// One recorded mock-transport call.
    #[derive(Debug, Clone)]
    struct RecordedCall {
        file_name: String,
        sha256: String,
        bytes: Vec<u8>,
        at: Instant,
    }

    type Calls = Arc<Mutex<Vec<RecordedCall>>>;

    /// Everything a pipeline test needs: tempdir, gates, recorded calls, and
    /// the uploader under test with a scripted mock transport (outcomes are
    /// consumed in order; once exhausted every call returns `Accepted`).
    struct Rig {
        dir: TempDir,
        consent: Arc<AtomicBool>,
        flag: Arc<AtomicBool>,
        calls: Calls,
        uploader: Arc<Uploader>,
    }

    impl Rig {
        fn new(outcomes: Vec<UploadOutcome>) -> Self {
            Self::with_pause(outcomes, Duration::from_millis(30))
        }

        fn with_pause(outcomes: Vec<UploadOutcome>, pause_between: Duration) -> Self {
            let dir = TempDir::new().unwrap();
            Self::on_dir(dir, outcomes, pause_between, true, true)
        }

        fn on_dir(
            dir: TempDir,
            outcomes: Vec<UploadOutcome>,
            pause_between: Duration,
            consent: bool,
            flag: bool,
        ) -> Self {
            let consent = Arc::new(AtomicBool::new(consent));
            let flag = Arc::new(AtomicBool::new(flag));
            let calls: Calls = Arc::new(Mutex::new(Vec::new()));
            let script = Arc::new(Mutex::new(VecDeque::from(outcomes)));

            let calls_clone = Arc::clone(&calls);
            let transport: TransportFn = Arc::new(move |request| {
                calls_clone.lock().unwrap().push(RecordedCall {
                    file_name: request.file_name.clone(),
                    sha256: request.sha256.clone(),
                    bytes: request.bytes.clone(),
                    at: Instant::now(),
                });
                script
                    .lock()
                    .unwrap()
                    .pop_front()
                    .unwrap_or(UploadOutcome::Accepted)
            });

            let consent_gate = Arc::clone(&consent);
            let flag_gate = Arc::clone(&flag);
            let uploader = Uploader::new(UploaderOptions {
                replays_dir: dir.path().to_path_buf(),
                ledger_path: dir.path().join("ledger.json"),
                pause_between_files: pause_between,
                live_poll: Duration::from_millis(20),
                flag_poll: Duration::from_millis(20),
                backoff_base: Duration::from_millis(10),
                backoff_cap: Duration::from_millis(40),
                max_attempts: 3,
                rate_limit_pause: Duration::from_millis(60),
                unavailable_pause: Duration::from_millis(60),
                consent_gate: Arc::new(move || consent_gate.load(Ordering::SeqCst)),
                flag_gate: Arc::new(move || flag_gate.load(Ordering::SeqCst)),
                transport,
            });

            Self {
                dir,
                consent,
                flag,
                calls,
                uploader,
            }
        }

        fn call_count(&self) -> usize {
            self.calls.lock().unwrap().len()
        }
    }

    /// Poll until `cond` holds or panic after `secs` seconds.
    fn wait_until(secs: u64, what: &str, cond: impl Fn() -> bool) {
        let deadline = Instant::now() + Duration::from_secs(secs);
        while !cond() {
            if Instant::now() >= deadline {
                panic!("timed out waiting for {what}");
            }
            thread::sleep(Duration::from_millis(10));
        }
    }

    /// Long enough for the (test-tuned) worker to act if it were going to.
    fn settle() {
        thread::sleep(Duration::from_millis(150));
    }

    fn contains_subslice(haystack: &[u8], needle: &[u8]) -> bool {
        haystack.windows(needle.len()).any(|w| w == needle)
    }

    // ── Ledger ───────────────────────────────────────────────────────────────

    /// Round trip: what one process records, a fresh load (restart) reads
    /// back — statuses and the cheap path check included.
    #[test]
    fn ledger_round_trip_persists_across_reload() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("ledger.json");

        let mut ledger = Ledger::load(path.clone());
        ledger.record(
            "aaa111".into(),
            LedgerEntry {
                path: "C:/replays/a.wowsreplay".into(),
                status: LedgerStatus::Accepted,
                mtime_ms: 1000,
                size: 42,
            },
        );
        ledger.record(
            "bbb222".into(),
            LedgerEntry {
                path: "C:/replays/b.wowsreplay".into(),
                status: LedgerStatus::Rejected,
                mtime_ms: 2000,
                size: 43,
            },
        );

        let reloaded = Ledger::load(path);
        assert_eq!(reloaded.status_of("aaa111"), Some(LedgerStatus::Accepted));
        assert_eq!(reloaded.status_of("bbb222"), Some(LedgerStatus::Rejected));
        assert_eq!(reloaded.status_of("ccc333"), None);
        assert!(reloaded.has_path_unchanged("C:/replays/a.wowsreplay", 1000, 42));
    }

    /// Missing and corrupt ledger files must both start empty (fail-safe —
    /// the server's hash dedup is authoritative).
    #[test]
    fn ledger_missing_or_corrupt_loads_empty() {
        let tmp = TempDir::new().unwrap();

        let missing = Ledger::load(tmp.path().join("nope.json"));
        assert_eq!(missing.status_of("anything"), None);

        let corrupt_path = tmp.path().join("corrupt.json");
        fs::write(&corrupt_path, b"definitely { not json").unwrap();
        let corrupt = Ledger::load(corrupt_path);
        assert_eq!(corrupt.status_of("anything"), None);
    }

    /// The cheap scan-side check must require the exact path+mtime+size —
    /// a touched or changed file must be re-hashed, not silently skipped.
    #[test]
    fn ledger_path_check_requires_exact_mtime_and_size() {
        let tmp = TempDir::new().unwrap();
        let mut ledger = Ledger::load(tmp.path().join("ledger.json"));
        ledger.record(
            "abc".into(),
            LedgerEntry {
                path: "/r/x.wowsreplay".into(),
                status: LedgerStatus::Accepted,
                mtime_ms: 1000,
                size: 5,
            },
        );
        assert!(ledger.has_path_unchanged("/r/x.wowsreplay", 1000, 5));
        assert!(!ledger.has_path_unchanged("/r/x.wowsreplay", 1001, 5), "mtime changed");
        assert!(!ledger.has_path_unchanged("/r/x.wowsreplay", 1000, 6), "size changed");
        assert!(!ledger.has_path_unchanged("/r/y.wowsreplay", 1000, 5), "other path");
    }

    // ── Pure helpers ─────────────────────────────────────────────────────────

    #[test]
    fn sha256_hex_known_vector() {
        // NIST test vector for "abc".
        assert_eq!(
            sha256_hex(b"abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }

    #[test]
    fn backoff_delay_doubles_and_caps() {
        let base = Duration::from_secs(60);
        let cap = Duration::from_secs(900);
        assert_eq!(backoff_delay(base, cap, 1), Duration::from_secs(60));
        assert_eq!(backoff_delay(base, cap, 2), Duration::from_secs(120));
        assert_eq!(backoff_delay(base, cap, 3), Duration::from_secs(240));
        assert_eq!(backoff_delay(base, cap, 5), Duration::from_secs(900), "capped");
        assert_eq!(backoff_delay(base, cap, 30), Duration::from_secs(900), "no overflow");
    }

    #[test]
    fn map_response_covers_contract_statuses() {
        // 202 accepted / 200 duplicate — both terminal success.
        assert_eq!(map_response(202, r#"{"status":"accepted"}"#), UploadOutcome::Accepted);
        assert_eq!(map_response(200, r#"{"status":"duplicate"}"#), UploadOutcome::Accepted);
        // Permanent rejections.
        for status in [400u16, 413, 415, 422] {
            assert!(
                matches!(map_response(status, "{}"), UploadOutcome::Rejected(_)),
                "HTTP {status} must be a permanent rejection"
            );
        }
        assert_eq!(map_response(426, "{}"), UploadOutcome::UpgradeRequired);
        assert_eq!(map_response(429, "{}"), UploadOutcome::RateLimited);
        assert_eq!(map_response(503, "{}"), UploadOutcome::Unavailable);
        // Anything unexpected is transient (retry with backoff).
        assert!(matches!(map_response(500, "boom"), UploadOutcome::Transient(_)));
        assert!(matches!(map_response(404, ""), UploadOutcome::Transient(_)));
    }

    /// The multipart body must match what the engine endpoint parses:
    /// `sha256`/`filename`/`mtime` text fields plus the `file` part, with a
    /// terminating boundary and the matching Content-Type header value.
    #[test]
    fn multipart_body_matches_engine_contract() {
        let bytes = replay_bytes("multipart");
        let request = UploadRequest {
            path: PathBuf::from("/r/battle.wowsreplay"),
            file_name: "battle.wowsreplay".into(),
            mtime_ms: 1_770_000_000_500,
            sha256: sha256_hex(&bytes),
            bytes: bytes.clone(),
        };
        let (content_type, body) = multipart_body(&request);

        let boundary = content_type
            .strip_prefix("multipart/form-data; boundary=")
            .expect("content type declares the boundary");
        assert!(boundary.starts_with("tfd-bridge-"));

        // All four parts present, with the contract field names.
        for needle in [
            format!("Content-Disposition: form-data; name=\"sha256\"\r\n\r\n{}", request.sha256),
            "Content-Disposition: form-data; name=\"filename\"\r\n\r\nbattle.wowsreplay".into(),
            // 1_770_000_000_500 ms → 1770000000.500 s
            "Content-Disposition: form-data; name=\"mtime\"\r\n\r\n1770000000.500".into(),
            "Content-Disposition: form-data; name=\"file\"; filename=\"battle.wowsreplay\"\r\nContent-Type: application/octet-stream"
                .into(),
        ] {
            assert!(
                contains_subslice(&body, needle.as_bytes()),
                "multipart body is missing: {needle}"
            );
        }
        // The raw file bytes and the closing boundary.
        assert!(contains_subslice(&body, &bytes), "file bytes must be embedded verbatim");
        assert!(
            body.ends_with(format!("\r\n--{boundary}--\r\n").as_bytes()),
            "body must end with the closing boundary"
        );
        // The boundary derives from the content hash, so the payload cannot
        // contain it anywhere except the markers we wrote.
        assert!(!contains_subslice(&bytes, boundary.as_bytes()));
    }

    /// Backfill selection: window, extension, size, and ledger filters plus
    /// oldest-first ordering — as a pure function over synthetic entries.
    #[test]
    fn select_candidates_filters_window_size_extension_and_ledger() {
        let now = 1_000_000_000_000u64;
        let day_ms = 24 * 60 * 60 * 1000u64;
        let dir = Path::new("/replays");

        let entry = |name: &str, age_days: u64, size: u64| ReplayEntry {
            name: name.into(),
            size,
            modified_ms: now - age_days * day_ms,
        };
        let entries = vec![
            entry("recent_b.wowsreplay", 2, 100),
            entry("recent_a.wowsreplay", 5, 100),
            entry("too_old.wowsreplay", 31, 100),        // outside the 30-day window
            entry("tempArenaInfo.json", 0, 100),         // not a replay
            entry("empty.wowsreplay", 1, 0),             // zero bytes
            entry("huge.wowsreplay", 1, MAX_UPLOAD_BYTES + 1), // over the cap
            entry("13.1.0/nested.wowsreplay", 10, 100),  // nested files are fine
            entry("already.wowsreplay", 3, 100),         // in the ledger
        ];

        let tmp = TempDir::new().unwrap();
        let mut ledger = Ledger::load(tmp.path().join("ledger.json"));
        ledger.record(
            "known".into(),
            LedgerEntry {
                path: dir.join("already.wowsreplay").to_string_lossy().into_owned(),
                status: LedgerStatus::Accepted,
                mtime_ms: now - 3 * day_ms,
                size: 100,
            },
        );

        let picked = select_candidates(&entries, now, &ledger, dir);
        let names: Vec<&str> = picked.iter().map(|e| e.name.as_str()).collect();
        assert_eq!(
            names,
            vec![
                "13.1.0/nested.wowsreplay",
                "recent_a.wowsreplay",
                "recent_b.wowsreplay"
            ],
            "filtered to in-window unledgered replays, oldest first"
        );

        // Exactly-30-days-old is still inside the window.
        let edge = vec![entry("edge.wowsreplay", 30, 100)];
        assert_eq!(select_candidates(&edge, now, &ledger, dir).len(), 1);
    }

    #[test]
    fn is_replay_file_only_accepts_wowsreplay() {
        assert!(is_replay_file(Path::new("/r/battle.wowsreplay")));
        assert!(is_replay_file(Path::new("/r/BATTLE.WOWSREPLAY")));
        assert!(!is_replay_file(Path::new("/r/tempArenaInfo.json")));
        assert!(!is_replay_file(Path::new("/r/notes.txt")));
        assert!(!is_replay_file(Path::new("/r/noextension")));
    }

    // ── Trigger logic (simulated finalized events) ───────────────────────────

    /// A finalized event uploads the file once: correct bytes + hash on the
    /// wire, Accepted recorded in the ledger (persisted to disk).
    #[test]
    fn finalized_event_uploads_and_records_ledger() {
        let rig = Rig::new(vec![]);
        let path = write_replay(rig.dir.path(), "battle.wowsreplay", "one");
        let expected_sha = sha256_hex(&replay_bytes("one"));

        rig.uploader.on_replay_finalized(&path);
        wait_until(5, "upload call", || rig.call_count() == 1);

        let calls = rig.calls.lock().unwrap();
        assert_eq!(calls[0].file_name, "battle.wowsreplay");
        assert_eq!(calls[0].sha256, expected_sha);
        assert_eq!(calls[0].bytes, replay_bytes("one"));
        drop(calls);

        // The ledger on disk knows the hash now.
        wait_until(5, "ledger write", || {
            Ledger::load(rig.dir.path().join("ledger.json")).status_of(&expected_sha)
                == Some(LedgerStatus::Accepted)
        });
        rig.uploader.cancel();
    }

    /// Non-replay files must never upload, whatever the trigger claims.
    #[test]
    fn non_replay_files_are_never_uploaded() {
        let rig = Rig::new(vec![]);
        let temp = rig.dir.path().join("other.json");
        fs::write(&temp, b"{}").unwrap();
        rig.uploader.on_replay_finalized(&temp);
        settle();
        assert_eq!(rig.call_count(), 0, "non-replay file must not upload");
        rig.uploader.cancel();
    }

    /// A structurally incomplete archive is skipped without a request and
    /// without a ledger entry (a later scan re-checks it).
    #[test]
    fn incomplete_replay_is_skipped_without_upload() {
        let rig = Rig::new(vec![]);
        let path = rig.dir.path().join("broken.wowsreplay");
        fs::write(&path, b"garbage, not a replay").unwrap();
        rig.uploader.on_replay_finalized(&path);
        settle();
        assert_eq!(rig.call_count(), 0);
        rig.uploader.cancel();
    }

    /// Re-announcing an already-accepted file (duplicate event, re-run) must
    /// not upload again — the ledger short-circuits before any request.
    #[test]
    fn accepted_hash_is_never_reuploaded() {
        let rig = Rig::new(vec![]);
        let path = write_replay(rig.dir.path(), "battle.wowsreplay", "dup");

        rig.uploader.on_replay_finalized(&path);
        wait_until(5, "first upload", || rig.call_count() == 1);

        rig.uploader.on_replay_finalized(&path);
        settle();
        assert_eq!(rig.call_count(), 1, "accepted hash must not re-upload");
        rig.uploader.cancel();
    }

    /// Restart simulation: a second uploader over the same ledger file must
    /// not re-upload what the first one already donated — and its scan must
    /// pick up a file the first run never got to.
    #[test]
    fn restart_with_ledger_survives_and_resumes() {
        let rig = Rig::new(vec![]);
        let done = write_replay(rig.dir.path(), "done.wowsreplay", "done");
        rig.uploader.on_replay_finalized(&done);
        wait_until(5, "first-run upload", || rig.call_count() == 1);
        rig.uploader.cancel();

        // A replay the first run never uploaded (e.g. killed mid-queue).
        write_replay(rig.dir.path(), "missed.wowsreplay", "missed");

        // "Restart": fresh uploader, same dir + ledger; startup scan runs.
        let rig2 = Rig::on_dir(rig.dir, vec![], Duration::from_millis(30), true, true);
        rig2.uploader.spawn_scan();
        wait_until(5, "scan upload of the missed file", || rig2.call_count() == 1);
        settle();

        let calls = rig2.calls.lock().unwrap();
        assert_eq!(calls.len(), 1, "only the missed file may upload after restart");
        assert_eq!(calls[0].file_name, "missed.wowsreplay");
        drop(calls);
        rig2.uploader.cancel();
    }

    // ── Backfill selection (end-to-end scan) ─────────────────────────────────

    /// The scan queues everything donate-able oldest-first and skips junk
    /// (tempArenaInfo.json must not even be considered — and here no battle
    /// is live, the roster file is stale).
    #[test]
    fn scan_uploads_oldest_first_and_skips_non_replays() {
        let dir = TempDir::new().unwrap();
        let oldest = write_replay(dir.path(), "oldest.wowsreplay", "1");
        thread::sleep(Duration::from_millis(30)); // distinct mtimes
        let middle = write_replay(dir.path(), "middle.wowsreplay", "2");
        thread::sleep(Duration::from_millis(30));
        let newest = write_replay(dir.path(), "newest.wowsreplay", "3");
        fs::write(dir.path().join("notes.txt"), b"ignored").unwrap();
        let _ = (oldest, middle, newest);

        let rig = Rig::on_dir(dir, vec![], Duration::from_millis(10), true, true);
        rig.uploader.spawn_scan();
        wait_until(5, "all three uploads", || rig.call_count() == 3);
        settle();

        let calls = rig.calls.lock().unwrap();
        assert_eq!(calls.len(), 3, "exactly the three replays upload");
        let order: Vec<&str> = calls.iter().map(|c| c.file_name.as_str()).collect();
        assert_eq!(
            order,
            vec!["oldest.wowsreplay", "middle.wowsreplay", "newest.wowsreplay"],
            "backfill must run oldest first"
        );
        drop(calls);
        rig.uploader.cancel();
    }

    /// Running the scan twice (opt-in plus startup, say) must not duplicate
    /// anything: queue-level path dedup plus the ledger cover it.
    #[test]
    fn repeated_scans_do_not_duplicate_uploads() {
        let dir = TempDir::new().unwrap();
        write_replay(dir.path(), "battle.wowsreplay", "once");
        let rig = Rig::on_dir(dir, vec![], Duration::from_millis(10), true, true);

        rig.uploader.spawn_scan();
        rig.uploader.spawn_scan();
        wait_until(5, "the upload", || rig.call_count() >= 1);
        settle();
        rig.uploader.spawn_scan();
        settle();

        assert_eq!(rig.call_count(), 1, "scans must never duplicate an upload");
        rig.uploader.cancel();
    }

    // ── Gating (consent × flag matrix) ───────────────────────────────────────

    /// Consent × feature-flag matrix: uploads happen only when BOTH are true.
    #[test]
    fn gating_requires_consent_and_flag() {
        for (consent, flag) in [(false, false), (false, true), (true, false)] {
            let dir = TempDir::new().unwrap();
            let path = write_replay(dir.path(), "battle.wowsreplay", "gate");
            let rig = Rig::on_dir(dir, vec![], Duration::from_millis(30), consent, flag);
            rig.uploader.on_replay_finalized(&path);
            rig.uploader.spawn_scan();
            settle();
            assert_eq!(
                rig.call_count(),
                0,
                "consent={consent} flag={flag} must not upload"
            );
            rig.uploader.cancel();
        }

        // Both true → uploads.
        let dir = TempDir::new().unwrap();
        let path = write_replay(dir.path(), "battle.wowsreplay", "gate");
        let rig = Rig::on_dir(dir, vec![], Duration::from_millis(30), true, true);
        rig.uploader.on_replay_finalized(&path);
        wait_until(5, "gated upload", || rig.call_count() == 1);
        rig.uploader.cancel();
    }

    /// The remote kill switch pauses (queue kept) and resumes within the
    /// flag-poll interval once the flag returns — no restart needed.
    #[test]
    fn flag_off_pauses_then_flag_on_resumes() {
        let dir = TempDir::new().unwrap();
        let path = write_replay(dir.path(), "battle.wowsreplay", "flag");
        let rig = Rig::on_dir(dir, vec![], Duration::from_millis(30), true, false);

        rig.uploader.on_replay_finalized(&path);
        settle();
        assert_eq!(rig.call_count(), 0, "flag off must hold the upload");

        rig.flag.store(true, Ordering::SeqCst);
        wait_until(5, "upload after flag flip", || rig.call_count() == 1);
        rig.uploader.cancel();
    }

    /// Revoking consent mid-queue stops everything: the file being waited on
    /// is abandoned and the rest of the queue is dropped.
    #[test]
    fn revoking_consent_mid_queue_stops_uploads() {
        // A long pause between files keeps the worker busy after the first
        // upload so the revoke demonstrably lands mid-queue.
        let rig = Rig::with_pause(vec![], Duration::from_millis(400));
        let first = write_replay(rig.dir.path(), "first.wowsreplay", "1");
        let second = write_replay(rig.dir.path(), "second.wowsreplay", "2");
        rig.uploader.on_replay_finalized(&first);
        rig.uploader.on_replay_finalized(&second);

        wait_until(5, "first upload", || rig.call_count() == 1);
        // Revoke: cache flips first (gate), then the queue is cleared.
        rig.consent.store(false, Ordering::SeqCst);
        rig.uploader.revoke();

        thread::sleep(Duration::from_millis(600)); // well past the pause
        assert_eq!(rig.call_count(), 1, "revocation must stop the queued upload");
        rig.uploader.cancel();
    }

    // ── Throttle / live-battle pause ─────────────────────────────────────────

    /// One at a time with a deliberate pause: the second upload must start
    /// no earlier than `pause_between_files` after the first.
    #[test]
    fn throttle_pauses_between_files() {
        let pause = Duration::from_millis(120);
        let rig = Rig::with_pause(vec![], pause);
        let a = write_replay(rig.dir.path(), "a.wowsreplay", "a");
        let b = write_replay(rig.dir.path(), "b.wowsreplay", "b");
        rig.uploader.on_replay_finalized(&a);
        rig.uploader.on_replay_finalized(&b);

        wait_until(5, "both uploads", || rig.call_count() == 2);
        let calls = rig.calls.lock().unwrap();
        let gap = calls[1].at.duration_since(calls[0].at);
        assert!(
            gap >= pause,
            "second upload must wait the between-files pause (gap {gap:?} < {pause:?})"
        );
        drop(calls);
        rig.uploader.cancel();
    }

    /// While a battle is live (tempArenaInfo.json present) the pipeline
    /// pauses ENTIRELY; the upload happens promptly once the roster is gone.
    #[test]
    fn uploads_pause_entirely_while_battle_live() {
        let dir = TempDir::new().unwrap();
        fs::write(dir.path().join(TEMP_ARENA_INFO), b"{}").unwrap();
        let path = write_replay(dir.path(), "battle.wowsreplay", "live");

        let rig = Rig::on_dir(dir, vec![], Duration::from_millis(30), true, true);
        rig.uploader.on_replay_finalized(&path);
        settle();
        assert_eq!(rig.call_count(), 0, "no upload may start during a live battle");

        fs::remove_file(rig.dir.path().join(TEMP_ARENA_INFO)).unwrap();
        wait_until(5, "upload after battle end", || rig.call_count() == 1);
        rig.uploader.cancel();
    }

    // ── Robustness (outcome handling) ────────────────────────────────────────

    /// Transient failures retry with backoff up to max_attempts, then defer:
    /// no ledger entry, so a later scan picks the file up again.
    #[test]
    fn transient_failures_retry_then_defer_without_ledger_entry() {
        let rig = Rig::new(vec![
            UploadOutcome::Transient("t1".into()),
            UploadOutcome::Transient("t2".into()),
            UploadOutcome::Transient("t3".into()),
        ]);
        let path = write_replay(rig.dir.path(), "battle.wowsreplay", "flaky");
        let sha = sha256_hex(&replay_bytes("flaky"));

        rig.uploader.on_replay_finalized(&path);
        wait_until(5, "three attempts", || rig.call_count() == 3);
        settle();
        assert_eq!(rig.call_count(), 3, "max_attempts (3) tries, then give up");
        assert_eq!(
            Ledger::load(rig.dir.path().join("ledger.json")).status_of(&sha),
            None,
            "a deferred file must NOT be in the ledger (later scans retry it)"
        );
        rig.uploader.cancel();
    }

    /// A permanent rejection (e.g. 422 invalid replay) is recorded and never
    /// retried — not even by a fresh trigger for the same content.
    #[test]
    fn permanent_rejection_is_recorded_and_not_retried() {
        let rig = Rig::new(vec![UploadOutcome::Rejected("HTTP 422: invalid".into())]);
        let path = write_replay(rig.dir.path(), "battle.wowsreplay", "bad");
        let sha = sha256_hex(&replay_bytes("bad"));

        rig.uploader.on_replay_finalized(&path);
        wait_until(5, "the rejected attempt", || rig.call_count() == 1);
        wait_until(5, "ledger write", || {
            Ledger::load(rig.dir.path().join("ledger.json")).status_of(&sha)
                == Some(LedgerStatus::Rejected)
        });

        rig.uploader.on_replay_finalized(&path);
        settle();
        assert_eq!(rig.call_count(), 1, "rejected hash must never retry");
        rig.uploader.cancel();
    }

    /// HTTP 426 latches the whole pipeline off until the next app start:
    /// queued and newly-triggered files alike stop uploading.
    #[test]
    fn upgrade_required_latches_until_restart() {
        let rig = Rig::new(vec![UploadOutcome::UpgradeRequired]);
        let a = write_replay(rig.dir.path(), "a.wowsreplay", "a");
        let b = write_replay(rig.dir.path(), "b.wowsreplay", "b");
        rig.uploader.on_replay_finalized(&a);
        rig.uploader.on_replay_finalized(&b);

        wait_until(5, "the 426 attempt", || rig.call_count() == 1);
        settle();
        assert_eq!(rig.call_count(), 1, "426 must stop the queue");

        let c = write_replay(rig.dir.path(), "c.wowsreplay", "c");
        rig.uploader.on_replay_finalized(&c);
        settle();
        assert_eq!(rig.call_count(), 1, "426 must latch new triggers off too");
        rig.uploader.cancel();
    }

    /// 429 waits out the rate-limit pause, then retries the SAME file and
    /// succeeds — without burning the transient-attempt budget.
    #[test]
    fn rate_limited_waits_then_retries_same_file() {
        let rig = Rig::new(vec![UploadOutcome::RateLimited]);
        let path = write_replay(rig.dir.path(), "battle.wowsreplay", "limited");
        let sha = sha256_hex(&replay_bytes("limited"));

        rig.uploader.on_replay_finalized(&path);
        wait_until(5, "retry after rate limit", || rig.call_count() == 2);

        let calls = rig.calls.lock().unwrap();
        assert_eq!(calls[0].sha256, sha);
        assert_eq!(calls[1].sha256, sha, "the same file retries after 429");
        let gap = calls[1].at.duration_since(calls[0].at);
        assert!(
            gap >= Duration::from_millis(60),
            "429 must wait the rate-limit pause (gap {gap:?})"
        );
        drop(calls);
        wait_until(5, "ledger write", || {
            Ledger::load(rig.dir.path().join("ledger.json")).status_of(&sha)
                == Some(LedgerStatus::Accepted)
        });
        rig.uploader.cancel();
    }

    /// 503 (feature off / storage unconfigured) waits much longer and then
    /// retries — never a tight error loop, never a ledger entry.
    #[test]
    fn unavailable_waits_then_retries() {
        let rig = Rig::new(vec![UploadOutcome::Unavailable]);
        let path = write_replay(rig.dir.path(), "battle.wowsreplay", "storage");

        rig.uploader.on_replay_finalized(&path);
        wait_until(5, "retry after 503", || rig.call_count() == 2);
        let calls = rig.calls.lock().unwrap();
        let gap = calls[1].at.duration_since(calls[0].at);
        assert!(gap >= Duration::from_millis(60), "503 must wait before retrying");
        drop(calls);
        rig.uploader.cancel();
    }

    // ── Transport (against a local mock — no live calls) ─────────────────────

    /// A captured request: lowercased (header, value) pairs + the raw body bytes.
    type CapturedReq = (Vec<(String, String)>, Vec<u8>);

    /// Spawn a one-shot mock engine answering with `status` + `body`;
    /// captures the request headers (lowercased) and raw body bytes.
    fn spawn_mock_intake(status: u16, body: &'static str) -> (String, mpsc::Receiver<CapturedReq>) {
        let server = tiny_http::Server::http("127.0.0.1:0").expect("bind mock intake");
        let port = server
            .server_addr()
            .to_ip()
            .expect("mock intake binds a TCP socket")
            .port();
        let (tx, rx) = mpsc::channel();
        thread::spawn(move || {
            while let Ok(mut request) = server.recv() {
                let headers = request
                    .headers()
                    .iter()
                    .map(|h| {
                        (
                            h.field.as_str().as_str().to_ascii_lowercase(),
                            h.value.as_str().to_owned(),
                        )
                    })
                    .collect();
                let mut raw_body = Vec::new();
                let _ = std::io::Read::read_to_end(request.as_reader(), &mut raw_body);
                let _ = tx.send((headers, raw_body));
                let _ = request.respond(
                    tiny_http::Response::from_string(body)
                        .with_status_code(tiny_http::StatusCode(status)),
                );
            }
        });
        (format!("http://127.0.0.1:{port}"), rx)
    }

    /// The real HTTP send: posts the contract multipart through the shared
    /// engine client (so the bridge version header rides along) and maps a
    /// 202 "accepted" response.  Runs against a local mock only.
    #[tokio::test]
    async fn send_donation_posts_contract_multipart_with_version_header() {
        let (base, rx) = spawn_mock_intake(
            202,
            r#"{"status":"accepted","sha256":"x","s3_key":"replays/h/x.wowsreplay","player_name":"helmi"}"#,
        );
        let bytes = replay_bytes("wire");
        let request = UploadRequest {
            path: PathBuf::from("/r/battle.wowsreplay"),
            file_name: "battle.wowsreplay".into(),
            mtime_ms: 1_770_000_000_000,
            sha256: sha256_hex(&bytes),
            bytes: bytes.clone(),
        };

        let outcome = send_donation(
            crate::engine::shared_client(),
            &format!("{base}/api/v1/bridge/donate-replay"),
            &request,
        )
        .await;
        assert_eq!(outcome, UploadOutcome::Accepted);

        let (headers, raw_body) = rx
            .recv_timeout(Duration::from_secs(5))
            .expect("request reached the mock intake");
        let get = |name: &str| {
            headers
                .iter()
                .find(|(n, _)| n == name)
                .map(|(_, v)| v.clone())
        };
        assert_eq!(
            get("x-tfd-bridge-version").as_deref(),
            Some(env!("CARGO_PKG_VERSION")),
            "the donation upload must carry the bridge version header"
        );
        let content_type = get("content-type").expect("content-type present");
        assert!(content_type.starts_with("multipart/form-data; boundary=tfd-bridge-"));
        assert!(contains_subslice(
            &raw_body,
            format!("name=\"sha256\"\r\n\r\n{}", request.sha256).as_bytes()
        ));
        assert!(contains_subslice(&raw_body, &bytes), "file bytes on the wire");
    }

    /// Contract statuses map correctly through the real HTTP path too.
    #[tokio::test]
    async fn send_donation_maps_429_and_unreachable() {
        let (base, _rx) = spawn_mock_intake(429, r#"{"error":"rate_limited"}"#);
        let bytes = replay_bytes("mapped");
        let request = UploadRequest {
            path: PathBuf::from("/r/battle.wowsreplay"),
            file_name: "battle.wowsreplay".into(),
            mtime_ms: 0,
            sha256: sha256_hex(&bytes),
            bytes,
        };
        let outcome = send_donation(
            crate::engine::shared_client(),
            &format!("{base}/api/v1/bridge/donate-replay"),
            &request,
        )
        .await;
        assert_eq!(outcome, UploadOutcome::RateLimited);

        // Nothing listens on the discard port — connection refused stands in
        // for the offline case and must be a retryable transient.
        let offline = send_donation(
            crate::engine::shared_client(),
            "http://127.0.0.1:9/api/v1/bridge/donate-replay",
            &request,
        )
        .await;
        assert!(matches!(offline, UploadOutcome::Transient(_)));
    }
}
