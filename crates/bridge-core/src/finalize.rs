/// Replay-finalized detection — a reliable "this .wowsreplay is complete" event.
///
/// WoWS writes `tempArenaInfo.json` into the replays folder at battle start and
/// deletes it at battle end.  The battle's `.wowsreplay` archive is created at
/// battle *start* and finalized (fully written) at battle *end* — so the
/// deletion of the live roster is the trigger: at that moment the newest
/// `.wowsreplay` is the just-finished battle's file.
///
/// The [`FinalizeDetector`] is fed `tempArenaInfo.json` transitions by the
/// bridge's existing notify watcher (see `server.rs` — there is exactly one
/// watcher per bridge).  On a live→gone transition it identifies the newest
/// archive and declares it final only after:
///   1. a **stability check** — file size unchanged over a configurable window
///      (partial writes at battle end settle first), and
///   2. a **structural sanity check** — `.wowsreplay` magic at offset 0, a
///      parseable JSON first block at offset 12 (length at offset 8), and
///      nonzero packet payload after it (see [`is_structurally_complete`]).
///
/// Consumers (e.g. the donation uploader, td-c8973d) subscribe via the
/// [`FinalizedCallback`] passed in [`FinalizeOptions`]; the callback runs on a
/// detector worker thread and should hand heavy work off to its own queue.
///
/// Duplicate suppression uses a **watermark** (ms since epoch): an event is
/// emitted only for files modified strictly after it, and every emission
/// advances it.  The caller persists the watermark (the app stores it in its
/// config) and passes it back on the next start — the startup **catch-up scan**
/// then emits events for replays finalized while the app was not running,
/// without re-emitting anything from before the watermark.
///
/// Handled edge cases:
/// - "Save replays" disabled in WoWS → no archive appears → no event, no error.
/// - Leaving a battle early → same roster-deletion signal, same path.
/// - Bridge started mid-battle → `live` is seeded from disk and the catch-up
///   scan excludes the newest archive (the in-progress battle's file); it is
///   emitted by the live path when the battle ends.
use serde::Serialize;
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc, Mutex,
};
use std::thread;
use std::time::{Duration, Instant, UNIX_EPOCH};

// ── Constants ──────────────────────────────────────────────────────────────────

/// The live-battle roster file WoWS writes at battle start and deletes at end.
pub const TEMP_ARENA_INFO: &str = "tempArenaInfo.json";

/// `.wowsreplay` magic. On disk the first four bytes are `12 32 34 11`;
/// read as a little-endian u32 at offset 0 that is `0x1134_3212`.
pub const REPLAY_MAGIC: u32 = 0x1134_3212;

/// Fixed header: magic (4) + block count (4) + first block length (4).
const REPLAY_HEADER_LEN: u64 = 12;

/// Upper bound on the first (JSON) block — real headers are a few KB; anything
/// claiming more than this is garbage and must not drive a huge allocation.
const MAX_FIRST_BLOCK_LEN: u32 = 16 * 1024 * 1024;

/// Default stability window: the archive size must be unchanged this long
/// before the file is declared final.  (To be tuned on the real rig.)
pub const DEFAULT_STABILITY_WINDOW: Duration = Duration::from_secs(3);
/// Default interval between size samples while waiting for stability.
pub const DEFAULT_POLL_INTERVAL: Duration = Duration::from_millis(500);
/// Default cap on the total stability wait — a file still changing after this
/// is abandoned (logged, no event); the next catch-up can pick it up.
pub const DEFAULT_STABILITY_TIMEOUT: Duration = Duration::from_secs(60);

// ── Public types ───────────────────────────────────────────────────────────────

/// A finalized replay, ready to be read in full.
#[derive(Debug, Clone, Serialize)]
pub struct ReplayFinalizedEvent {
    /// Absolute path of the finalized `.wowsreplay` file.
    pub path: PathBuf,
    /// Forward-slash path relative to the replays dir — matches the names the
    /// `/v1/replays` endpoint serves (e.g. `"13.1.0/20260119_x.wowsreplay"`).
    pub name: String,
    pub size: u64,
    /// Milliseconds since UNIX epoch; also the watermark value this event
    /// advanced to (persist it to suppress re-emission across restarts).
    pub modified_ms: u64,
}

/// Subscriber callback.  Invoked on a detector worker thread while the
/// watermark lock is held — keep it quick and queue heavy work elsewhere.
pub type FinalizedCallback = Arc<dyn Fn(ReplayFinalizedEvent) + Send + Sync + 'static>;

/// Configuration for replay-finalized detection, passed to
/// `server::start_with_finalize`.
#[derive(Clone)]
pub struct FinalizeOptions {
    /// Only files modified strictly after this (ms since epoch) emit events.
    /// Pass the persisted value from the last run; `0` emits for everything.
    pub watermark_ms: u64,
    /// Size must be unchanged for this long before a file is declared final.
    pub stability_window: Duration,
    /// Interval between size samples during the stability check.
    pub poll_interval: Duration,
    /// Give up on a candidate that is still changing after this total wait.
    pub stability_timeout: Duration,
    /// Receives every finalized-replay event (live and catch-up).
    pub on_finalized: FinalizedCallback,
}

impl FinalizeOptions {
    /// Options with production-default timings.
    pub fn new(watermark_ms: u64, on_finalized: FinalizedCallback) -> Self {
        Self {
            watermark_ms,
            stability_window: DEFAULT_STABILITY_WINDOW,
            poll_interval: DEFAULT_POLL_INTERVAL,
            stability_timeout: DEFAULT_STABILITY_TIMEOUT,
            on_finalized,
        }
    }
}

// ── Detector ───────────────────────────────────────────────────────────────────

/// Tracks the live-battle state and turns roster deletions into finalized
/// events.  Created by the bridge and fed from its notify watcher; all blocking
/// work (stability waits, catch-up) happens on short-lived worker threads so
/// the watcher callback never blocks.
pub struct FinalizeDetector {
    replays_dir: PathBuf,
    /// `true` while a battle is live (tempArenaInfo.json present).  Seeded from
    /// disk at construction so a bridge started mid-battle behaves correctly.
    live: AtomicBool,
    /// Highest modified_ms an event has been emitted for.  Locked across the
    /// check-emit-advance sequence so concurrent workers (live + catch-up)
    /// can never emit the same file twice.
    watermark_ms: Mutex<u64>,
    stability_window: Duration,
    poll_interval: Duration,
    stability_timeout: Duration,
    on_finalized: FinalizedCallback,
    /// Set by [`FinalizeDetector::cancel`] (bridge stop) — workers bail out and
    /// no further events are emitted.
    cancelled: AtomicBool,
}

impl FinalizeDetector {
    /// Build a detector for `replays_dir`.  The live flag is seeded from disk;
    /// there is a tiny window between this check and the watcher starting, but
    /// any transition inside it is re-observed by the next roster event.
    pub fn new(replays_dir: PathBuf, opts: FinalizeOptions) -> Arc<Self> {
        let live = replays_dir.join(TEMP_ARENA_INFO).exists();
        Arc::new(Self {
            replays_dir,
            live: AtomicBool::new(live),
            watermark_ms: Mutex::new(opts.watermark_ms),
            stability_window: opts.stability_window,
            poll_interval: opts.poll_interval,
            stability_timeout: opts.stability_timeout,
            on_finalized: opts.on_finalized,
            cancelled: AtomicBool::new(false),
        })
    }

    /// Feed one `tempArenaInfo.json` filesystem event from the watcher.  The
    /// event kind is irrelevant — existence on disk is re-checked, so renames,
    /// duplicate notifications and out-of-order kinds all resolve correctly.
    /// A live→gone transition (battle end) triggers exactly one finalize pass.
    pub fn observe_temp_file(self: &Arc<Self>, path: &Path) {
        let now_exists = path.exists();
        let was_live = self.live.swap(now_exists, Ordering::SeqCst);
        if was_live && !now_exists {
            log::info!("Battle end detected ({TEMP_ARENA_INFO} removed) — checking newest replay");
            let det = Arc::clone(self);
            thread::spawn(move || det.finalize_newest());
        }
    }

    /// Spawn the startup catch-up scan: emit events for archives modified after
    /// the watermark, i.e. battles finished while the bridge was not running.
    /// Call AFTER the watcher is active so a battle ending mid-scan is still seen.
    pub fn start_catch_up(self: &Arc<Self>) {
        let det = Arc::clone(self);
        thread::spawn(move || det.run_catch_up());
    }

    /// Stop emitting: in-flight workers bail at the next checkpoint.
    pub fn cancel(&self) {
        self.cancelled.store(true, Ordering::SeqCst);
    }

    fn is_cancelled(&self) -> bool {
        self.cancelled.load(Ordering::SeqCst)
    }

    // ── Workers ────────────────────────────────────────────────────────────────

    /// Battle-end pass: the newest archive is the just-finished battle's file.
    /// No archive at all (replay saving disabled in WoWS) is a normal outcome.
    fn finalize_newest(&self) {
        let Some(newest) = self
            .list_archives()
            .into_iter()
            .max_by_key(|(_, modified_ms)| *modified_ms)
        else {
            log::info!("Battle ended but no .wowsreplay present (replay saving disabled?) — no event");
            return;
        };
        self.finalize_and_emit(&newest.0);
    }

    /// Catch-up pass: every archive newer than the watermark, oldest first.
    /// While a battle is live the newest archive is excluded — it is the
    /// in-progress battle's (incomplete) file; the live path emits it at end.
    fn run_catch_up(&self) {
        let watermark = *self.watermark_ms.lock().unwrap();
        let mut archives = self.list_archives();
        if self.live.load(Ordering::SeqCst) {
            if let Some(newest_idx) = archives
                .iter()
                .enumerate()
                .max_by_key(|(_, (_, modified_ms))| *modified_ms)
                .map(|(idx, _)| idx)
            {
                archives.remove(newest_idx);
            }
        }
        let mut candidates: Vec<(PathBuf, u64)> = archives
            .into_iter()
            .filter(|(_, modified_ms)| *modified_ms > watermark)
            .collect();
        candidates.sort_by_key(|(_, modified_ms)| *modified_ms);

        for (path, _) in candidates {
            if self.is_cancelled() {
                return;
            }
            self.finalize_and_emit(&path);
        }
    }

    /// All `.wowsreplay` archives under the replays dir as (path, modified_ms).
    fn list_archives(&self) -> Vec<(PathBuf, u64)> {
        match crate::server::list_replays(&self.replays_dir) {
            Ok(entries) => entries
                .into_iter()
                .filter(|e| e.name.to_ascii_lowercase().ends_with(".wowsreplay"))
                .map(|e| (self.replays_dir.join(&e.name), e.modified_ms))
                .collect(),
            Err(e) => {
                log::warn!("Finalize: cannot list replays dir: {e}");
                Vec::new()
            }
        }
    }

    /// Shared pipeline: stability check → structural sanity → emit (once).
    fn finalize_and_emit(&self, path: &Path) {
        if self.is_cancelled() {
            return;
        }
        if !self.wait_for_stable(path) {
            log::warn!("Finalize: {} did not stabilise — no event", path.display());
            return;
        }
        if !is_structurally_complete(path) {
            log::warn!("Finalize: {} failed the structural check — no event", path.display());
            return;
        }
        self.emit_if_new(path);
    }

    /// Wait until the file size is unchanged over `stability_window`, sampling
    /// every `poll_interval`.  Fails on disappearance, cancellation, or when
    /// the size is still changing after `stability_timeout`.
    fn wait_for_stable(&self, path: &Path) -> bool {
        let deadline = Instant::now() + self.stability_timeout;
        let Ok(meta) = fs::metadata(path) else {
            return false;
        };
        let mut last_size = meta.len();
        let mut stable_since = Instant::now();
        loop {
            if self.is_cancelled() {
                return false;
            }
            thread::sleep(self.poll_interval);
            let Ok(meta) = fs::metadata(path) else {
                return false; // file vanished mid-check
            };
            let size = meta.len();
            if size != last_size {
                last_size = size;
                stable_since = Instant::now();
            }
            if stable_since.elapsed() >= self.stability_window {
                return true;
            }
            if Instant::now() >= deadline {
                return false;
            }
        }
    }

    /// Emit an event for `path` iff its mtime is past the watermark, then
    /// advance the watermark.  The lock is held across check + callback +
    /// advance so the live path and the catch-up scan cannot double-emit.
    fn emit_if_new(&self, path: &Path) {
        let Ok(meta) = fs::metadata(path) else {
            return;
        };
        let modified_ms = meta
            .modified()
            .ok()
            .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0);

        let mut watermark = self.watermark_ms.lock().unwrap();
        if modified_ms <= *watermark {
            log::debug!(
                "Finalize: {} at/behind watermark ({modified_ms} <= {}) — already emitted",
                path.display(),
                *watermark
            );
            return;
        }
        if self.is_cancelled() {
            return;
        }
        let event = ReplayFinalizedEvent {
            path: path.to_path_buf(),
            name: relative_name(&self.replays_dir, path),
            size: meta.len(),
            modified_ms,
        };
        log::info!("Replay finalized: {} ({} bytes)", event.name, event.size);
        (self.on_finalized)(event);
        *watermark = modified_ms;
    }
}

// ── Pure helpers ───────────────────────────────────────────────────────────────

/// Structural sanity check for a finalized `.wowsreplay`:
/// - magic `0x12323411` (little-endian u32) at offset 0,
/// - a parseable JSON first block at offset 12, length given at offset 8,
/// - nonzero packet payload remaining after the first block.
///
/// Any I/O error, truncation or parse failure returns `false` — the caller
/// treats that as "not finalized" and emits nothing.
pub fn is_structurally_complete(path: &Path) -> bool {
    let Ok(mut file) = fs::File::open(path) else {
        return false;
    };
    let Ok(meta) = file.metadata() else {
        return false;
    };
    let file_len = meta.len();
    if file_len <= REPLAY_HEADER_LEN {
        return false;
    }

    let mut header = [0u8; REPLAY_HEADER_LEN as usize];
    if file.read_exact(&mut header).is_err() {
        return false;
    }
    let magic = u32::from_le_bytes(header[0..4].try_into().unwrap());
    if magic != REPLAY_MAGIC {
        return false;
    }
    let first_block_len = u32::from_le_bytes(header[8..12].try_into().unwrap());
    if first_block_len == 0 || first_block_len > MAX_FIRST_BLOCK_LEN {
        return false;
    }
    // Nonzero packet payload must remain after the JSON block.
    if file_len <= REPLAY_HEADER_LEN + u64::from(first_block_len) {
        return false;
    }

    let mut json_block = vec![0u8; first_block_len as usize];
    if file.read_exact(&mut json_block).is_err() {
        return false;
    }
    serde_json::from_slice::<serde_json::Value>(&json_block).is_ok()
}

/// Forward-slash path of `path` relative to `dir` (matching `/v1/replays`
/// names); falls back to the bare file name if `path` is not under `dir`.
fn relative_name(dir: &Path, path: &Path) -> String {
    match path.strip_prefix(dir) {
        Ok(rel) => rel
            .components()
            .map(|c| c.as_os_str().to_string_lossy())
            .collect::<Vec<_>>()
            .join("/"),
        Err(_) => path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default(),
    }
}

/// Build valid `.wowsreplay` bytes for tests: magic + block count 1 + first
/// block length + JSON block + packet payload.
#[cfg(test)]
pub(crate) fn build_replay_bytes(json: &str, payload: &[u8]) -> Vec<u8> {
    let json_bytes = json.as_bytes();
    let mut buf = Vec::new();
    buf.extend_from_slice(&REPLAY_MAGIC.to_le_bytes());
    buf.extend_from_slice(&1u32.to_le_bytes());
    buf.extend_from_slice(&(json_bytes.len() as u32).to_le_bytes());
    buf.extend_from_slice(json_bytes);
    buf.extend_from_slice(payload);
    buf
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::server::{self, Bridge};
    use std::fs;
    use tempfile::TempDir;

    type EventSink = Arc<Mutex<Vec<ReplayFinalizedEvent>>>;

    fn event_sink() -> EventSink {
        Arc::new(Mutex::new(Vec::new()))
    }

    /// Tight timings so tests complete quickly; semantics identical to prod.
    fn test_options(watermark_ms: u64, sink: &EventSink) -> FinalizeOptions {
        let sink = Arc::clone(sink);
        FinalizeOptions {
            watermark_ms,
            stability_window: Duration::from_millis(120),
            poll_interval: Duration::from_millis(20),
            stability_timeout: Duration::from_secs(5),
            on_finalized: Arc::new(move |ev| sink.lock().unwrap().push(ev)),
        }
    }

    /// Start a bridge (OS-assigned port) with finalize detection enabled.
    fn start_bridge(tmp: &TempDir, watermark_ms: u64, sink: &EventSink) -> Bridge {
        server::start_on_ports(
            tmp.path().to_path_buf(),
            None,
            &[0],
            Some(test_options(watermark_ms, sink)),
        )
        .expect("bridge start failed")
    }

    /// Poll until `cond` holds or panic after `secs` seconds.
    fn wait_until(secs: u64, what: &str, cond: impl Fn() -> bool) {
        let deadline = Instant::now() + Duration::from_secs(secs);
        while !cond() {
            if Instant::now() >= deadline {
                panic!("timed out waiting for {what}");
            }
            thread::sleep(Duration::from_millis(25));
        }
    }

    /// Wait until the watcher has processed events past generation `gen`.
    /// The watcher feeds the finalize detector BEFORE bumping the generation,
    /// so once this returns the detector has seen the same events.
    fn wait_for_generation_past(bridge: &Bridge, gen: u64) {
        wait_until(5, "watcher generation bump", || bridge.generation() > gen);
    }

    /// Long enough for any pending (test-tuned) stability check to conclude.
    fn settle() {
        thread::sleep(Duration::from_millis(500));
    }

    fn valid_replay() -> Vec<u8> {
        build_replay_bytes(r#"{"playerName":"helmi","matchGroup":"pvp"}"#, b"packet-payload")
    }

    // ── Structural sanity check ───────────────────────────────────────────────

    #[test]
    fn structural_check_accepts_valid_replay() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("good.wowsreplay");
        fs::write(&path, valid_replay()).unwrap();
        assert!(is_structurally_complete(&path));
    }

    /// Regression guard: pin the literal on-disk magic bytes a real WoWS
    /// `.wowsreplay` starts with (`12 32 34 11`). The other fixtures derive
    /// their magic from REPLAY_MAGIC, so they'd silently follow a wrong
    /// constant — this one would not. (A reversed constant shipped in v0.3.0
    /// and rejected every real replay.)
    #[test]
    fn structural_check_accepts_real_on_disk_magic_bytes() {
        let tmp = TempDir::new().unwrap();
        let json = br#"{"clientVersionFromExe":"15,4,0,0"}"#;
        let mut bytes = vec![0x12u8, 0x32, 0x34, 0x11]; // real first 4 bytes on disk
        bytes.extend_from_slice(&1u32.to_le_bytes());
        bytes.extend_from_slice(&(json.len() as u32).to_le_bytes());
        bytes.extend_from_slice(json);
        bytes.extend_from_slice(b"packet-payload");
        let path = tmp.path().join("real_magic.wowsreplay");
        fs::write(&path, bytes).unwrap();
        assert!(is_structurally_complete(&path));
    }

    #[test]
    fn structural_check_rejects_bad_magic() {
        let tmp = TempDir::new().unwrap();
        let mut bytes = valid_replay();
        bytes[0] = 0xFF; // corrupt the magic
        let path = tmp.path().join("bad_magic.wowsreplay");
        fs::write(&path, bytes).unwrap();
        assert!(!is_structurally_complete(&path));
    }

    #[test]
    fn structural_check_rejects_truncated_header() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("truncated.wowsreplay");
        fs::write(&path, &valid_replay()[..10]).unwrap();
        assert!(!is_structurally_complete(&path));
    }

    #[test]
    fn structural_check_rejects_zero_length_first_block() {
        let tmp = TempDir::new().unwrap();
        let mut bytes = Vec::new();
        bytes.extend_from_slice(&REPLAY_MAGIC.to_le_bytes());
        bytes.extend_from_slice(&1u32.to_le_bytes());
        bytes.extend_from_slice(&0u32.to_le_bytes()); // first block length 0
        bytes.extend_from_slice(b"payload");
        let path = tmp.path().join("zero_block.wowsreplay");
        fs::write(&path, bytes).unwrap();
        assert!(!is_structurally_complete(&path));
    }

    #[test]
    fn structural_check_rejects_invalid_json_block() {
        let tmp = TempDir::new().unwrap();
        let mut bytes = Vec::new();
        bytes.extend_from_slice(&REPLAY_MAGIC.to_le_bytes());
        bytes.extend_from_slice(&1u32.to_le_bytes());
        bytes.extend_from_slice(&7u32.to_le_bytes());
        bytes.extend_from_slice(b"not [{ valid json"); // 7-byte block is "not [{ "
        let path = tmp.path().join("bad_json.wowsreplay");
        fs::write(&path, bytes).unwrap();
        assert!(!is_structurally_complete(&path));
    }

    /// A file that ends exactly after the JSON block has no packet payload —
    /// that is what a not-yet-finalized (or stillborn) replay looks like.
    #[test]
    fn structural_check_rejects_missing_payload() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("no_payload.wowsreplay");
        fs::write(&path, build_replay_bytes(r#"{"a":1}"#, b"")).unwrap();
        assert!(!is_structurally_complete(&path));
    }

    #[test]
    fn structural_check_rejects_missing_file() {
        let tmp = TempDir::new().unwrap();
        assert!(!is_structurally_complete(&tmp.path().join("nope.wowsreplay")));
    }

    #[test]
    fn structural_check_rejects_arbitrary_garbage() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("garbage.wowsreplay");
        fs::write(&path, b"this is not a replay file at all, just bytes").unwrap();
        assert!(!is_structurally_complete(&path));
    }

    // ── Stability check ───────────────────────────────────────────────────────

    fn bare_detector(tmp: &TempDir, window_ms: u64, timeout_ms: u64) -> Arc<FinalizeDetector> {
        FinalizeDetector::new(
            tmp.path().to_path_buf(),
            FinalizeOptions {
                watermark_ms: 0,
                stability_window: Duration::from_millis(window_ms),
                poll_interval: Duration::from_millis(20),
                stability_timeout: Duration::from_millis(timeout_ms),
                on_finalized: Arc::new(|_| {}),
            },
        )
    }

    #[test]
    fn stability_check_passes_for_static_file() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("static.wowsreplay");
        fs::write(&path, b"data").unwrap();
        let det = bare_detector(&tmp, 100, 5000);
        assert!(det.wait_for_stable(&path));
    }

    #[test]
    fn stability_check_fails_for_missing_file() {
        let tmp = TempDir::new().unwrap();
        let det = bare_detector(&tmp, 100, 5000);
        assert!(!det.wait_for_stable(&tmp.path().join("missing.wowsreplay")));
    }

    /// A file that keeps growing must not be declared stable within the timeout.
    #[test]
    fn stability_check_fails_while_file_keeps_growing() {
        use std::io::Write;

        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("growing.wowsreplay");
        fs::write(&path, b"start").unwrap();

        // Appender: writes every 40ms for ~1s — far longer than the 400ms
        // timeout, so the 200ms-stability window can never be satisfied.
        let writer_path = path.clone();
        let writer = thread::spawn(move || {
            for _ in 0..25 {
                let mut f = fs::OpenOptions::new()
                    .append(true)
                    .open(&writer_path)
                    .unwrap();
                f.write_all(b"more").unwrap();
                drop(f);
                thread::sleep(Duration::from_millis(40));
            }
        });

        let det = bare_detector(&tmp, 200, 400);
        assert!(
            !det.wait_for_stable(&path),
            "a continuously growing file must fail the stability check"
        );
        writer.join().unwrap();
    }

    // ── Watermark gate (emit_if_new) ──────────────────────────────────────────

    #[test]
    fn emit_if_new_emits_once_then_respects_watermark() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("battle.wowsreplay");
        fs::write(&path, valid_replay()).unwrap();

        let sink = event_sink();
        let det = FinalizeDetector::new(tmp.path().to_path_buf(), test_options(0, &sink));

        det.emit_if_new(&path);
        assert_eq!(sink.lock().unwrap().len(), 1, "first emit must fire");

        det.emit_if_new(&path);
        assert_eq!(
            sink.lock().unwrap().len(),
            1,
            "second emit for the same file must be suppressed by the watermark"
        );
    }

    #[test]
    fn emit_if_new_skips_file_at_or_behind_initial_watermark() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("battle.wowsreplay");
        fs::write(&path, valid_replay()).unwrap();
        let modified_ms = fs::metadata(&path)
            .unwrap()
            .modified()
            .unwrap()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_millis() as u64;

        let sink = event_sink();
        let det = FinalizeDetector::new(tmp.path().to_path_buf(), test_options(modified_ms, &sink));
        det.emit_if_new(&path);
        assert!(
            sink.lock().unwrap().is_empty(),
            "file at the watermark must not emit"
        );
    }

    // ── Live battle-end detection (through the real bridge watcher) ──────────

    /// The core flow: roster appears (battle start) → archive written → roster
    /// removed (battle end) → exactly one finalized event, and none before.
    #[test]
    fn finalized_event_fires_once_after_temp_removal() {
        let tmp = TempDir::new().unwrap();
        let sink = event_sink();
        let bridge = start_bridge(&tmp, 0, &sink);

        // Battle start: live roster appears.
        let g0 = bridge.generation();
        fs::write(tmp.path().join(TEMP_ARENA_INFO), b"{}").unwrap();
        wait_for_generation_past(&bridge, g0);

        // The archive exists during the battle (created at battle start).
        let g1 = bridge.generation();
        let replay = tmp.path().join("battle.wowsreplay");
        fs::write(&replay, valid_replay()).unwrap();
        wait_for_generation_past(&bridge, g1);

        // No event while the battle is ongoing.
        settle();
        assert!(
            sink.lock().unwrap().is_empty(),
            "no event may fire during an ongoing battle"
        );

        // Battle end: roster removed → exactly one finalized event.
        fs::remove_file(tmp.path().join(TEMP_ARENA_INFO)).unwrap();
        wait_until(5, "finalized event", || !sink.lock().unwrap().is_empty());
        settle(); // grace period: assert no duplicate arrives

        let events = sink.lock().unwrap();
        assert_eq!(events.len(), 1, "exactly one finalized event per battle");
        assert_eq!(events[0].name, "battle.wowsreplay");
        assert_eq!(events[0].path, replay);
        assert_eq!(events[0].size, valid_replay().len() as u64);
        assert!(events[0].modified_ms > 0);
        drop(events);
        bridge.stop();
    }

    /// "Save replays" disabled in WoWS: the roster comes and goes but no
    /// archive ever appears — no event and no error.
    #[test]
    fn no_event_when_no_replay_file_exists() {
        let tmp = TempDir::new().unwrap();
        let sink = event_sink();
        let bridge = start_bridge(&tmp, 0, &sink);

        let g0 = bridge.generation();
        fs::write(tmp.path().join(TEMP_ARENA_INFO), b"{}").unwrap();
        wait_for_generation_past(&bridge, g0);
        fs::remove_file(tmp.path().join(TEMP_ARENA_INFO)).unwrap();

        settle();
        assert!(
            sink.lock().unwrap().is_empty(),
            "no archive present must mean no event"
        );
        bridge.stop();
    }

    /// A structurally broken newest archive must not be announced as finalized.
    #[test]
    fn no_event_when_newest_replay_is_garbage() {
        let tmp = TempDir::new().unwrap();
        let sink = event_sink();
        let bridge = start_bridge(&tmp, 0, &sink);

        let g0 = bridge.generation();
        fs::write(tmp.path().join(TEMP_ARENA_INFO), b"{}").unwrap();
        wait_for_generation_past(&bridge, g0);

        let g1 = bridge.generation();
        fs::write(tmp.path().join("broken.wowsreplay"), b"garbage bytes").unwrap();
        wait_for_generation_past(&bridge, g1);

        fs::remove_file(tmp.path().join(TEMP_ARENA_INFO)).unwrap();
        settle();
        assert!(
            sink.lock().unwrap().is_empty(),
            "a structurally incomplete archive must not emit"
        );
        bridge.stop();
    }

    /// A second battle that produces no new archive (saving turned off
    /// mid-session, or a duplicate roster removal) must not re-emit the
    /// previous battle's file.
    #[test]
    fn second_battle_end_does_not_duplicate_event() {
        let tmp = TempDir::new().unwrap();
        let sink = event_sink();
        let bridge = start_bridge(&tmp, 0, &sink);

        // First battle with an archive → one event.
        let g0 = bridge.generation();
        fs::write(tmp.path().join(TEMP_ARENA_INFO), b"{}").unwrap();
        wait_for_generation_past(&bridge, g0);
        let g1 = bridge.generation();
        fs::write(tmp.path().join("battle.wowsreplay"), valid_replay()).unwrap();
        wait_for_generation_past(&bridge, g1);
        fs::remove_file(tmp.path().join(TEMP_ARENA_INFO)).unwrap();
        wait_until(5, "first finalized event", || !sink.lock().unwrap().is_empty());

        // Second battle, no new archive → no second event for the old file.
        let g2 = bridge.generation();
        fs::write(tmp.path().join(TEMP_ARENA_INFO), b"{}").unwrap();
        wait_for_generation_past(&bridge, g2);
        fs::remove_file(tmp.path().join(TEMP_ARENA_INFO)).unwrap();

        settle();
        assert_eq!(
            sink.lock().unwrap().len(),
            1,
            "the previous battle's archive must not be re-emitted"
        );
        bridge.stop();
    }

    // ── Startup catch-up ──────────────────────────────────────────────────────

    /// Archives finalized while the app was closed are emitted at startup,
    /// oldest first, with /v1/replays-style forward-slash names for nested files.
    #[test]
    fn catch_up_emits_replays_newer_than_watermark() {
        let tmp = TempDir::new().unwrap();
        fs::write(tmp.path().join("first.wowsreplay"), valid_replay()).unwrap();
        thread::sleep(Duration::from_millis(30)); // distinct mtimes for ordering
        let sub = tmp.path().join("13.1.0");
        fs::create_dir_all(&sub).unwrap();
        fs::write(sub.join("second.wowsreplay"), valid_replay()).unwrap();

        let sink = event_sink();
        let bridge = start_bridge(&tmp, 0, &sink);
        wait_until(10, "catch-up events", || sink.lock().unwrap().len() >= 2);

        let events = sink.lock().unwrap();
        assert_eq!(events.len(), 2);
        assert_eq!(events[0].name, "first.wowsreplay");
        assert_eq!(events[1].name, "13.1.0/second.wowsreplay");
        assert!(events[0].modified_ms <= events[1].modified_ms, "oldest first");
        drop(events);
        bridge.stop();
    }

    /// Restart simulation: a second bridge started with the watermark from the
    /// first run's event must not re-emit the same archive.
    #[test]
    fn no_duplicate_events_across_restart_via_watermark() {
        let tmp = TempDir::new().unwrap();
        fs::write(tmp.path().join("battle.wowsreplay"), valid_replay()).unwrap();

        // First run: catch-up emits the archive once.
        let sink1 = event_sink();
        let bridge1 = start_bridge(&tmp, 0, &sink1);
        wait_until(5, "first-run catch-up event", || !sink1.lock().unwrap().is_empty());
        let watermark = sink1.lock().unwrap()[0].modified_ms;
        bridge1.stop();

        // Second run with the persisted watermark: nothing to emit.
        let sink2 = event_sink();
        let bridge2 = start_bridge(&tmp, watermark, &sink2);
        settle();
        assert!(
            sink2.lock().unwrap().is_empty(),
            "restart with persisted watermark must not re-emit"
        );
        bridge2.stop();
    }

    /// Catch-up must skip structurally incomplete files (and still emit the
    /// valid ones).
    #[test]
    fn catch_up_skips_structurally_incomplete_files() {
        let tmp = TempDir::new().unwrap();
        fs::write(tmp.path().join("broken.wowsreplay"), b"garbage").unwrap();
        thread::sleep(Duration::from_millis(30));
        fs::write(tmp.path().join("good.wowsreplay"), valid_replay()).unwrap();

        let sink = event_sink();
        let bridge = start_bridge(&tmp, 0, &sink);
        wait_until(5, "catch-up event for the valid file", || {
            !sink.lock().unwrap().is_empty()
        });
        settle();

        let events = sink.lock().unwrap();
        assert_eq!(events.len(), 1, "only the valid archive may emit");
        assert_eq!(events[0].name, "good.wowsreplay");
        drop(events);
        bridge.stop();
    }

    /// Bridge started mid-battle: the newest archive belongs to the live battle
    /// and must be excluded from catch-up; it is emitted when the battle ends.
    #[test]
    fn catch_up_excludes_newest_replay_while_battle_live() {
        let tmp = TempDir::new().unwrap();
        fs::write(tmp.path().join("old.wowsreplay"), valid_replay()).unwrap();
        thread::sleep(Duration::from_millis(30));
        fs::write(tmp.path().join("current.wowsreplay"), valid_replay()).unwrap();
        // Live battle in progress when the bridge starts.
        fs::write(tmp.path().join(TEMP_ARENA_INFO), b"{}").unwrap();

        let sink = event_sink();
        let bridge = start_bridge(&tmp, 0, &sink);
        wait_until(5, "catch-up event for the older archive", || {
            !sink.lock().unwrap().is_empty()
        });
        settle();
        {
            let events = sink.lock().unwrap();
            assert_eq!(events.len(), 1, "the live battle's archive must be excluded");
            assert_eq!(events[0].name, "old.wowsreplay");
        }

        // Battle ends → the excluded newest archive is emitted by the live path.
        fs::remove_file(tmp.path().join(TEMP_ARENA_INFO)).unwrap();
        wait_until(5, "live finalized event after battle end", || {
            sink.lock().unwrap().len() >= 2
        });
        let events = sink.lock().unwrap();
        assert_eq!(events.len(), 2);
        assert_eq!(events[1].name, "current.wowsreplay");
        drop(events);
        bridge.stop();
    }

    // ── Misc ─────────────────────────────────────────────────────────────────

    #[test]
    fn relative_name_is_forward_slash_and_falls_back_to_file_name() {
        let dir = Path::new("/replays");
        assert_eq!(
            relative_name(dir, &dir.join("13.1.0").join("x.wowsreplay")),
            "13.1.0/x.wowsreplay"
        );
        assert_eq!(
            relative_name(dir, Path::new("/elsewhere/y.wowsreplay")),
            "y.wowsreplay"
        );
    }
}
