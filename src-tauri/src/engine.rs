//! Engine-facing HTTP client: version headers + remote config/feature flags.
//!
//! TFD Bridge is a pure client of the tfd-engine stack; this module owns the
//! bridge→engine direction of the API contract (the loopback `/v1/health`
//! covers the browser→bridge direction). Every HTTPS call to engine.tfd.rocks
//! identifies the running bridge version, and the engine's bridge-config
//! endpoint tells us which features are currently allowed — a remote kill
//! switch that can pause client behaviour without shipping an app release.
//!
//! Contract (authoritative spec: `docs/bridge-contract.md` in
//! github.com/Helmi/tfd-engine):
//! - Every request carries `X-TFD-Bridge-Version: <semver>` and
//!   `User-Agent: TFD-Bridge/<semver> (<OS>)` — version from the package
//!   info, never hardcoded.
//! - `GET /api/v1/bridge/config` (unauthenticated) → 200
//!   `{"min_bridge_version":"0.1.0","features":{"replay_donation":bool,"battle_data_submission":bool}}`.
//! - HTTP 426 `{"error":"upgrade_required","min_bridge_version":...,"message":...}`
//!   when the engine rejects a too-old bridge — config refresh stops until
//!   the next app start (an update restarts the app anyway).
//!
//! Failure posture is conservative: features read as DISABLED until a config
//! has been seen, and the last good response is kept when a refresh fails.

use reqwest::header::{HeaderMap, HeaderValue};
use reqwest::StatusCode;
use serde::Deserialize;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{OnceLock, RwLock};
use std::time::Duration;

// ── Contract constants ───────────────────────────────────────────────────────

const ENGINE_BASE_URL: &str = "https://engine.tfd.rocks";
const CONFIG_PATH: &str = "/api/v1/bridge/config";
/// Replay-donation intake (the uploader's endpoint, td-c8973d).
pub(crate) const DONATE_REPLAY_PATH: &str = "/api/v1/bridge/donate-replay";
const VERSION_HEADER: &str = "X-TFD-Bridge-Version";
/// The running bridge version, straight from the package metadata.
const BRIDGE_VERSION: &str = env!("CARGO_PKG_VERSION");
/// Generous-but-bounded request timeout; refreshes are background work.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(10);

// ── Contract types ───────────────────────────────────────────────────────────

/// Engine feature flags gating client behaviour. `Default` is the
/// conservative all-disabled state used until a config has been fetched.
/// Unknown future flags in the response are ignored; missing known flags
/// fall back to disabled.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize)]
pub struct EngineFeatures {
    #[serde(default)]
    pub replay_donation: bool,
    #[serde(default)]
    pub battle_data_submission: bool,
}

/// The engine's bridge-config response (`GET /api/v1/bridge/config`).
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct BridgeConfig {
    pub min_bridge_version: String,
    #[serde(default)]
    pub features: EngineFeatures,
}

/// Body of the structured HTTP 426 "upgrade required" rejection.
/// Parsed best-effort for logging — a missing/garbled body must not change
/// the latching behaviour.
#[derive(Debug, Default, Deserialize)]
struct UpgradeRequiredBody {
    #[serde(default)]
    min_bridge_version: Option<String>,
    #[serde(default)]
    message: Option<String>,
}

/// What a config refresh attempt did — the caller (lib.rs) only has to map
/// this to UI concerns (the "update required" nudge).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RefreshOutcome {
    /// Fresh config fetched and cached. `upgrade_nudge` is `true` when the
    /// engine's `min_bridge_version` is above the running version.
    Updated { upgrade_nudge: bool },
    /// Engine answered 426 — refresh is latched off until the next app start.
    UpgradeRequired,
    /// A previous 426 latched refresh off; no request was made.
    Skipped,
    /// Network/HTTP/parse failure — cached values were kept (fail soft).
    Failed,
}

// ── State ────────────────────────────────────────────────────────────────────

/// Engine-config cache + refresh latches. A module-level static (not Tauri
/// managed state) so any module can read the flags through the cheap free
/// functions below without an `AppHandle`; tests construct their own instances.
struct EngineState {
    /// Last good config response; `None` until the first successful fetch.
    config: RwLock<Option<BridgeConfig>>,
    /// Latched on HTTP 426: stop refreshing until the next app start.
    upgrade_required: AtomicBool,
    /// Latched by `claim_update_nudge` so the upgrade nudge fires once per run.
    nudge_claimed: AtomicBool,
}

impl EngineState {
    const fn new() -> Self {
        Self {
            config: RwLock::new(None),
            upgrade_required: AtomicBool::new(false),
            nudge_claimed: AtomicBool::new(false),
        }
    }
}

static STATE: EngineState = EngineState::new();

// ── Public API ───────────────────────────────────────────────────────────────

/// Current engine feature flags — the remote kill switch consumers gate on,
/// e.g. `engine::features().replay_donation` (donation uploader, consent UX).
/// Conservative by design: every feature reads as DISABLED until a config has
/// been fetched this run.
pub fn features() -> EngineFeatures {
    features_of(&STATE)
}

/// Fetch the engine bridge-config and cache it. Called on startup and from
/// the hourly update-check timer. Never panics, never blocks the caller on
/// UI — the returned outcome tells lib.rs whether to surface the updater.
pub async fn refresh_config() -> RefreshOutcome {
    refresh_config_against(&STATE, http_client(), ENGINE_BASE_URL).await
}

/// Claim the once-per-run permission to surface the "update required" nudge.
/// Returns `true` exactly once so hourly refreshes can never nag repeatedly.
pub fn claim_update_nudge() -> bool {
    claim_nudge_of(&STATE)
}

/// The shared engine client, for other engine-facing modules (the donation
/// uploader): every request through it carries the contract headers.
pub(crate) fn shared_client() -> &'static reqwest::Client {
    http_client()
}

/// Absolute engine URL for an API `path` — always the production base.
pub(crate) fn endpoint(path: &str) -> String {
    format!("{ENGINE_BASE_URL}{path}")
}

// ── Implementation ───────────────────────────────────────────────────────────

fn features_of(state: &EngineState) -> EngineFeatures {
    state
        .config
        .read()
        .unwrap()
        .as_ref()
        .map(|c| c.features)
        .unwrap_or_default()
}

fn claim_nudge_of(state: &EngineState) -> bool {
    !state.nudge_claimed.swap(true, Ordering::SeqCst)
}

/// The shared engine HTTP client. Built once; every request through it
/// carries the version header + User-Agent from `build_client`.
fn http_client() -> &'static reqwest::Client {
    static CLIENT: OnceLock<reqwest::Client> = OnceLock::new();
    CLIENT.get_or_init(|| build_client(BRIDGE_VERSION))
}

/// Build a client whose default headers satisfy the engine contract:
/// `X-TFD-Bridge-Version` + `User-Agent: TFD-Bridge/<semver> (<OS>)`.
fn build_client(version: &str) -> reqwest::Client {
    // reqwest's `rustls-no-provider` feature panics on client build unless a
    // process-default crypto provider is installed. Mirror what
    // tauri-plugin-updater does: install ring on demand, first caller wins.
    if rustls::crypto::CryptoProvider::get_default().is_none() {
        let _ = rustls::crypto::ring::default_provider().install_default();
    }
    reqwest::Client::builder()
        .default_headers(default_headers(version))
        .user_agent(user_agent(version))
        .timeout(REQUEST_TIMEOUT)
        .build()
        .expect("static engine client config must be valid")
}

/// Default headers for every engine request (the User-Agent is set separately
/// via `ClientBuilder::user_agent`).
fn default_headers(version: &str) -> HeaderMap {
    let mut headers = HeaderMap::new();
    headers.insert(
        VERSION_HEADER,
        HeaderValue::from_str(version).expect("package version is a valid header value"),
    );
    headers
}

/// `TFD-Bridge/<semver> (<OS>)`, e.g. `TFD-Bridge/0.2.4 (Windows)`.
fn user_agent(version: &str) -> String {
    format!("TFD-Bridge/{version} ({})", platform_label())
}

/// Human-readable OS label for the User-Agent.
fn platform_label() -> &'static str {
    match std::env::consts::OS {
        "windows" => "Windows",
        "macos" => "macOS",
        "linux" => "Linux",
        other => other,
    }
}

/// `true` when `current` is a semver strictly below `min`. Unparseable
/// versions count as "not below" — a malformed `min_bridge_version` from the
/// server must not nag users to update.
fn version_below(current: &str, min: &str) -> bool {
    match (semver::Version::parse(current), semver::Version::parse(min)) {
        (Ok(current), Ok(min)) => current < min,
        _ => {
            log::warn!("Unparseable version in min-version check: current={current:?} min={min:?}");
            false
        }
    }
}

/// The actual refresh, parameterised over state/client/base URL so tests can
/// run it against a local mock server with an isolated `EngineState`.
async fn refresh_config_against(
    state: &EngineState,
    client: &reqwest::Client,
    base_url: &str,
) -> RefreshOutcome {
    if state.upgrade_required.load(Ordering::SeqCst) {
        log::debug!("Engine config refresh skipped: upgrade required (latched)");
        return RefreshOutcome::Skipped;
    }

    let url = format!("{base_url}{CONFIG_PATH}");
    let resp = match client.get(&url).send().await {
        Ok(r) => r,
        Err(e) => {
            // Offline / DNS / TLS failure — fail soft, keep cached values.
            log::warn!("Engine config fetch failed (keeping cached config): {e}");
            return RefreshOutcome::Failed;
        }
    };

    if resp.status() == StatusCode::UPGRADE_REQUIRED {
        let body = resp.text().await.unwrap_or_default();
        let detail: UpgradeRequiredBody = serde_json::from_str(&body).unwrap_or_default();
        state.upgrade_required.store(true, Ordering::SeqCst);
        log::warn!(
            "Engine rejected bridge v{BRIDGE_VERSION} as too old (min {}): {} — pausing config refresh until the next app start",
            detail.min_bridge_version.as_deref().unwrap_or("unknown"),
            detail.message.as_deref().unwrap_or("no message"),
        );
        return RefreshOutcome::UpgradeRequired;
    }

    if !resp.status().is_success() {
        log::warn!(
            "Engine config fetch returned HTTP {} (keeping cached config)",
            resp.status()
        );
        return RefreshOutcome::Failed;
    }

    let config = match resp.json::<BridgeConfig>().await {
        Ok(c) => c,
        Err(e) => {
            log::warn!("Engine config response did not parse (keeping cached config): {e}");
            return RefreshOutcome::Failed;
        }
    };

    let upgrade_nudge = version_below(BRIDGE_VERSION, &config.min_bridge_version);
    log::info!(
        "Engine bridge-config: min_bridge_version={} replay_donation={} battle_data_submission={}",
        config.min_bridge_version,
        config.features.replay_donation,
        config.features.battle_data_submission
    );
    *state.config.write().unwrap() = Some(config);
    RefreshOutcome::Updated { upgrade_nudge }
}

// ── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc;
    use std::thread;

    /// The exact 200 payload shape from the contract (live-verified).
    const CONTRACT_CONFIG: &str = r#"{"min_bridge_version":"0.1.0","features":{"replay_donation":false,"battle_data_submission":false}}"#;
    /// The exact 426 payload shape from the contract (live-verified).
    const CONTRACT_426: &str = r#"{"error":"upgrade_required","min_bridge_version":"0.1.0","message":"This bridge version is no longer supported. Please update to v0.1.0 or newer."}"#;

    /// Spawn a local HTTP server answering every request with `status` +
    /// `body`; returns the base URL and a receiver yielding each request's
    /// headers as lowercase (name, value) pairs. No live network — the real
    /// engine.tfd.rocks is never contacted from tests.
    fn spawn_mock_engine(
        status: u16,
        body: &'static str,
    ) -> (String, mpsc::Receiver<Vec<(String, String)>>) {
        let server = tiny_http::Server::http("127.0.0.1:0").expect("bind mock engine");
        let port = server
            .server_addr()
            .to_ip()
            .expect("mock engine binds a TCP socket")
            .port();
        let (tx, rx) = mpsc::channel();
        thread::spawn(move || {
            while let Ok(request) = server.recv() {
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
                let _ = tx.send(headers);
                let _ = request.respond(
                    tiny_http::Response::from_string(body)
                        .with_status_code(tiny_http::StatusCode(status)),
                );
            }
        });
        (format!("http://127.0.0.1:{port}"), rx)
    }

    // ── Version-header / User-Agent construction ─────────────────────────────

    #[test]
    fn default_headers_carry_bridge_version() {
        let headers = default_headers("1.2.3");
        assert_eq!(
            headers
                .get(VERSION_HEADER)
                .expect("version header present")
                .to_str()
                .unwrap(),
            "1.2.3"
        );
    }

    #[test]
    fn version_header_uses_package_version() {
        // The constant must follow the package version — no drift, never
        // hardcoded.
        assert_eq!(BRIDGE_VERSION, env!("CARGO_PKG_VERSION"));
        let headers = default_headers(BRIDGE_VERSION);
        assert_eq!(
            headers.get(VERSION_HEADER).unwrap().to_str().unwrap(),
            env!("CARGO_PKG_VERSION")
        );
    }

    #[test]
    fn user_agent_matches_contract_format() {
        let ua = user_agent("1.2.3");
        assert!(ua.starts_with("TFD-Bridge/1.2.3 ("), "unexpected UA: {ua}");
        assert!(ua.ends_with(')'), "unexpected UA: {ua}");
        #[cfg(target_os = "windows")]
        assert_eq!(ua, "TFD-Bridge/1.2.3 (Windows)");
    }

    // ── Config parsing ────────────────────────────────────────────────────────

    #[test]
    fn config_parses_contract_shape() {
        let cfg: BridgeConfig = serde_json::from_str(CONTRACT_CONFIG).unwrap();
        assert_eq!(cfg.min_bridge_version, "0.1.0");
        assert!(!cfg.features.replay_donation);
        assert!(!cfg.features.battle_data_submission);
    }

    #[test]
    fn config_parses_with_missing_features_as_disabled() {
        // Features the engine omits must read as disabled (conservative).
        let cfg: BridgeConfig =
            serde_json::from_str(r#"{"min_bridge_version":"0.1.0"}"#).unwrap();
        assert_eq!(cfg.features, EngineFeatures::default());

        let cfg: BridgeConfig = serde_json::from_str(
            r#"{"min_bridge_version":"0.1.0","features":{"replay_donation":true}}"#,
        )
        .unwrap();
        assert!(cfg.features.replay_donation);
        assert!(!cfg.features.battle_data_submission);
    }

    #[test]
    fn config_tolerates_unknown_future_fields() {
        let cfg: BridgeConfig = serde_json::from_str(
            r#"{"min_bridge_version":"0.1.0","features":{"replay_donation":true,"future_flag":true},"future_key":1}"#,
        )
        .unwrap();
        assert!(cfg.features.replay_donation);
    }

    #[test]
    fn upgrade_required_body_parses_contract_shape() {
        let body: UpgradeRequiredBody = serde_json::from_str(CONTRACT_426).unwrap();
        assert_eq!(body.min_bridge_version.as_deref(), Some("0.1.0"));
        assert!(body.message.is_some());
    }

    // ── Conservative defaults ────────────────────────────────────────────────

    #[test]
    fn features_default_disabled_until_config_seen() {
        let state = EngineState::new();
        let features = features_of(&state);
        assert!(!features.replay_donation);
        assert!(!features.battle_data_submission);
    }

    #[test]
    fn claim_nudge_fires_exactly_once() {
        let state = EngineState::new();
        assert!(claim_nudge_of(&state), "first claim must succeed");
        assert!(!claim_nudge_of(&state), "second claim must be denied");
        assert!(!claim_nudge_of(&state), "third claim must be denied");
    }

    // ── Version comparison ───────────────────────────────────────────────────

    #[test]
    fn version_below_semantics() {
        assert!(version_below("0.2.4", "0.3.0"));
        assert!(!version_below("0.3.0", "0.3.0"));
        assert!(!version_below("0.3.1", "0.3.0"));
        // Unparseable input must never demand an update.
        assert!(!version_below("0.2.4", "latest"));
        assert!(!version_below("not-a-version", "0.3.0"));
    }

    // ── Refresh behaviour (against a local mock — no live calls) ────────────

    #[tokio::test]
    async fn refresh_caches_config_and_sends_contract_headers() {
        let (base, rx) = spawn_mock_engine(
            200,
            r#"{"min_bridge_version":"0.1.0","features":{"replay_donation":true,"battle_data_submission":false}}"#,
        );
        let state = EngineState::new();
        let client = build_client(BRIDGE_VERSION);

        let outcome = refresh_config_against(&state, &client, &base).await;
        assert_eq!(outcome, RefreshOutcome::Updated { upgrade_nudge: false });

        // The flag flipped by the server is now visible through the getter.
        let features = features_of(&state);
        assert!(features.replay_donation);
        assert!(!features.battle_data_submission);

        // The request carried the contract headers.
        let headers = rx
            .recv_timeout(Duration::from_secs(5))
            .expect("request reached mock engine");
        let get = |name: &str| {
            headers
                .iter()
                .find(|(n, _)| n == name)
                .map(|(_, v)| v.clone())
        };
        assert_eq!(
            get("x-tfd-bridge-version").as_deref(),
            Some(BRIDGE_VERSION),
            "every engine request must carry the version header"
        );
        let ua = get("user-agent").expect("user-agent present");
        assert!(
            ua.starts_with(&format!("TFD-Bridge/{BRIDGE_VERSION} (")),
            "unexpected UA: {ua}"
        );
    }

    #[tokio::test]
    async fn refresh_flags_upgrade_nudge_when_min_above_current() {
        let (base, _rx) =
            spawn_mock_engine(200, r#"{"min_bridge_version":"999.0.0","features":{}}"#);
        let state = EngineState::new();
        let client = build_client(BRIDGE_VERSION);

        let outcome = refresh_config_against(&state, &client, &base).await;
        assert_eq!(outcome, RefreshOutcome::Updated { upgrade_nudge: true });
    }

    #[tokio::test]
    async fn refresh_426_latches_and_stops_retrying() {
        let (base, rx) = spawn_mock_engine(426, CONTRACT_426);
        let state = EngineState::new();
        let client = build_client(BRIDGE_VERSION);

        assert_eq!(
            refresh_config_against(&state, &client, &base).await,
            RefreshOutcome::UpgradeRequired
        );
        // The latch must stop the second refresh before any request is made.
        assert_eq!(
            refresh_config_against(&state, &client, &base).await,
            RefreshOutcome::Skipped
        );

        // Exactly one request hit the mock.
        rx.recv_timeout(Duration::from_secs(5)).expect("first request");
        assert!(
            rx.try_recv().is_err(),
            "latched refresh must not issue a second request"
        );
        // Features stay at the conservative defaults.
        assert_eq!(features_of(&state), EngineFeatures::default());
    }

    #[tokio::test]
    async fn refresh_parse_failure_keeps_cached_config() {
        let (base, _rx) = spawn_mock_engine(200, "definitely not json");
        let state = EngineState::new();
        *state.config.write().unwrap() = Some(BridgeConfig {
            min_bridge_version: "0.1.0".into(),
            features: EngineFeatures {
                replay_donation: true,
                battle_data_submission: true,
            },
        });
        let client = build_client(BRIDGE_VERSION);

        assert_eq!(
            refresh_config_against(&state, &client, &base).await,
            RefreshOutcome::Failed
        );
        let features = features_of(&state);
        assert!(
            features.replay_donation && features.battle_data_submission,
            "cached config must survive a failed refresh"
        );
    }

    #[tokio::test]
    async fn refresh_http_error_fails_soft_without_latching() {
        let (base, _rx) = spawn_mock_engine(500, "boom");
        let state = EngineState::new();
        let client = build_client(BRIDGE_VERSION);

        assert_eq!(
            refresh_config_against(&state, &client, &base).await,
            RefreshOutcome::Failed
        );
        // A 5xx is transient: the next refresh must still go out (not Skipped).
        assert_eq!(
            refresh_config_against(&state, &client, &base).await,
            RefreshOutcome::Failed
        );
    }

    #[tokio::test]
    async fn refresh_offline_fails_soft() {
        let state = EngineState::new();
        let client = build_client(BRIDGE_VERSION);

        // Nothing listens on the discard port — connection refused stands in
        // for the offline case.
        assert_eq!(
            refresh_config_against(&state, &client, "http://127.0.0.1:9").await,
            RefreshOutcome::Failed
        );
        assert_eq!(features_of(&state), EngineFeatures::default());
    }
}
