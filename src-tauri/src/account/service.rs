//! The native account service: one instance, owning the device key, secure
//! credential storage, native login orchestration and the (memory-only)
//! access token. Every I/O boundary is injected (`traits.rs`) so this module
//! is fully unit-testable without the network, the OS credential store, a
//! real browser or wall-clock time.
//!
//! Scope (td-27bcc9): device registration, PKCE/DPoP browser sign-in, the
//! loopback callback, the initial token exchange, durable two-slot refresh
//! persistence, and a one-shot restart refresh. Recurring proactive renewal
//! scheduling/backoff is td-cab09a; the full debug-reset UI is td-96a620;
//! this module only exposes the primitives (`invalidate_local`,
//! `revoke_device`) those tasks build on.
use super::traits::{
    AccountTransport, BrowserLauncher, ClientProfile, Clock, HttpResponse,
    STORED_PROFILE_SCHEMA_VERSION, SecretStore, SecureRandom, StoreError, StoredAccount,
    StoredProfileData, StoredRefresh, TransportError,
};
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use bridge_core::account::{RenewalFailure, RenewalTicket};
use bridge_core::dpop::DeviceKey;
use bridge_core::login_flow::{ConsumedLogin, LoginFlow, LoginFlowError, TerminalReason};
use bridge_core::server::{AuthCallback, AuthCallbackAck, AuthCallbackOutcome, AuthCallbackSink};
use serde_json::json;
use std::sync::atomic::{AtomicU16, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// `bridge.submit bridge.share bridge.profile` (contract v5 section 4.3
/// example) — the one scope this native client ever requests.
const SCOPE: &str = "bridge.submit bridge.share bridge.profile";

// ── Public status ────────────────────────────────────────────────────────────

/// Everything the local UI is allowed to see. No tokens, no device key, no
/// verifier/state, no raw HTTP detail — the "safe status" the architecture
/// directive requires.
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum AccountStatus {
    SignedOut,
    AwaitingBrowser,
    Exchanging,
    Connected {
        nickname: String,
        clan_tag: Option<String>,
        realm: Option<String>,
        /// Contract v5 section 7.1.1: the current access token's own expiry
        /// (Unix seconds), captured verbatim from the last successful
        /// `/token` response's required `web_lease_expires_at` field. Safe
        /// to expose (timestamp only, no token) — for td-9994bd to consume
        /// later; no handoff/cookie code lives here.
        web_lease_expires_at: Option<u64>,
    },
    /// The recurring renewal scheduler (td-cab09a) exhausted its 3-attempt
    /// transient-retry budget. Distinct from `Connected` so the local UI can
    /// offer an explicit recovery action instead of looking silently healthy;
    /// recovery is the SAME connection flow (`account_begin_login`), not a
    /// new command. Surfaced once per exhaustion episode — `status()` keeps
    /// returning this same variant on every subsequent poll while it lasts,
    /// but never causes any additional retry attempt or repeated log line.
    RenewalFailed {
        nickname: String,
        clan_tag: Option<String>,
        realm: Option<String>,
    },
    Canceled,
    Denied,
    AccountNotAuthorized,
    Expired,
    OpenFailed,
    StoreUnavailable,
}

// ── Native lifecycle snapshot / subscription / signing seam (td-811657) ─────

/// Safe, non-network reason the account most recently stopped being usable.
/// Derived from the SAME `RenewalFailure`/`invalid_grant` reason
/// classification `perform_renewal_attempt` already computes (the frozen
/// v5 section 11 `DEFINITIVE_REFRESH_REASONS` match) — never a second,
/// independently-maintained taxonomy, and never the raw Engine error body.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AccountEndReason {
    /// `invalid_grant` reason `device_revoked`.
    Revoked,
    /// `invalid_grant` reason `account_changed`.
    AccountChanged,
    /// The recurring renewal scheduler exhausted its 3-attempt transient
    /// retry budget (`AccountSession::status() == RenewalFailed`). Derived
    /// live from session state, not stored — it is never a stale flag.
    RenewalExhausted,
    /// A manual `invalidate_local` (logout/reset), or an `invalid_grant`
    /// reason whose only safe meaning is "sign in again" (`refresh_expired`,
    /// `refresh_absolute_expired`, `refresh_reuse_detected`,
    /// `refresh_orphaned`).
    SignedOut,
}

/// One atomic, native-only read of everything a caller needs to decide
/// "can I use this account right now" and "has the identity changed under
/// me since I last looked" — without ever seeing a token, the device key or
/// raw HTTP detail. Mirrors `AccountStatus`'s existing no-secrets boundary;
/// carries the SAME safe `status` value `AccountService::status()` already
/// exposes, plus the fields `status()` alone cannot express (contract
/// td-811657: `status()` maps both `Connected` and `Renewing` to the same
/// public `Connected` shape, so it cannot by itself tell "usable" from
/// "access expired, renewal retrying").
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct AccountSnapshot {
    /// The same safe status `AccountService::status()` returns.
    pub status: AccountStatus,
    /// Same semantics as `AccountSession::can_use_account` at the injected
    /// clock's current instant: false at access expiry even while a
    /// renewal retry is in flight or waiting.
    pub can_use_account: bool,
    /// Stable across ordinary access/refresh rotations; increments only on
    /// a fresh login-exchange commit, `invalidate_local`, or a definitive
    /// revoke/`invalid_grant` outcome. See `Inner::lifecycle_epoch`.
    pub lifecycle_epoch: u64,
    pub nickname: Option<String>,
    pub clan_tag: Option<String>,
    pub realm: Option<String>,
    /// Contract v5 section 7.1.1: the current access token's own expiry
    /// (Unix seconds), verbatim from the last successful `/token` response.
    pub web_lease_expires_at: Option<u64>,
    /// The same access-token expiry, independently derived from the
    /// memory-only `Instant`-based expiry `AccountSession`/`can_use_account`
    /// itself gate on — a caller doing its own `can_use_account`-style
    /// comparison should use this rather than assume it always agrees with
    /// `web_lease_expires_at` (the two are contractually equal today, but
    /// this field tracks the value this service actually enforces).
    pub access_expires_at_unix: Option<u64>,
    /// `None` while usable, or while never having been connected at all.
    pub end_reason: Option<AccountEndReason>,
}

/// The exact, task-owned allowlist of Engine endpoints the native signing
/// seam may target — no caller-supplied URL/method, mirroring
/// `BrowserLauncher`'s "never a caller-supplied URL" boundary in
/// `traits.rs`. Exactly one entry today; `POST /handoff` itself is NOT
/// called from here (td-9994bd owns that).
// This native seam is consumed by follow-up tasks; it is intentionally not
// wired to IPC in this task.
#[allow(dead_code)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SignedEndpoint {
    /// `POST https://engine.tfd.rocks/api/v1/bridge/auth/handoff`.
    Handoff,
}

#[allow(dead_code)]
impl SignedEndpoint {
    fn htm(self) -> &'static str {
        match self {
            Self::Handoff => "POST",
        }
    }

    fn path(self) -> &'static str {
        match self {
            Self::Handoff => "/handoff",
        }
    }
}

/// Native request material for one allowlisted, DPoP-signed Engine call:
/// the built headers (`DPoP`, `Authorization: DPoP <access token>`, version/
/// User-Agent) and the `lifecycle_epoch` captured at signing time, so a
/// consumer can recheck it immediately before applying any response —
/// mirroring the existing stale-response rejection pattern already used by
/// `renewal_tick`/`commit_renewal_*`. Deliberately NOT `Serialize`/`Debug`
/// (the `Authorization` header carries the raw access token) and never
/// passed to `log::*`.
#[allow(dead_code)]
pub struct SignedRequestMaterial {
    pub headers: Vec<(String, String)>,
    pub lifecycle_epoch: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Phase {
    Idle,
    AwaitingBrowser,
    Exchanging,
    Canceled,
    Denied,
    AccountNotAuthorized,
    Expired,
    OpenFailed,
    StoreUnavailable,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AccountError {
    NotReady,
    Transport(TransportError),
    Store(StoreError),
    InvalidResponse,
    Http { status: u16, error: String },
    NoPendingLogin,
    OpenFailed,
}

impl From<TransportError> for AccountError {
    fn from(e: TransportError) -> Self {
        Self::Transport(e)
    }
}

impl std::fmt::Display for AccountError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotReady => write!(f, "the bridge is not ready yet"),
            Self::Transport(_) => write!(f, "could not reach the Engine"),
            Self::Store(_) => write!(f, "the secure credential store is unavailable"),
            Self::InvalidResponse => write!(f, "the Engine sent an unexpected response"),
            Self::Http { error, .. } => write!(f, "sign-in failed ({error})"),
            Self::NoPendingLogin => write!(f, "no sign-in is in progress"),
            Self::OpenFailed => write!(f, "could not open your default browser"),
        }
    }
}

impl std::error::Error for AccountError {}

struct Inner {
    login_flow: LoginFlow,
    phase: Phase,
    device_key: Option<DeviceKey>,
    device_id: Option<String>,
    account_display: Option<StoredAccount>,
    /// Memory-only, per contract v5 section 2 ("Access token ... memory
    /// only"). Never written to `SecretStore`, never logged, never exposed
    /// by `status()`.
    access_token: Option<(String, Instant)>,
    session: bridge_core::account::AccountSession,
    /// The ONE credential-commit generation, advanced every time the active
    /// credential set changes: a successful login-exchange commit, a
    /// successful refresh commit, or `invalidate_local`'s wipe (advanced
    /// BEFORE the wipe, so a check made just after still sees it moved). A
    /// bootstrap refresh (or anything else) captures this value before its
    /// network call and rechecks it under `commit_lock` immediately before
    /// writing anything — a mismatch means zero writes, zero status change.
    credential_generation: u64,
    /// Memory-only (contract v5 section 7.1.1): the current access token's
    /// own expiry, captured verbatim from the last successful `/token`
    /// response's required `web_lease_expires_at` field. Only a successful
    /// `/token` call (exchange, bootstrap refresh or scheduled renewal) ever
    /// advances it; nothing else moves it forward.
    web_lease_expires_at: Option<u64>,
    /// The DPoP proof/`jti` most recently sent for the CURRENT renewal
    /// chain (td-cab09a, contract v5 section 5.5's transport-retry rule).
    /// `None` at the start of every fresh chain (`RenewalTicket.attempt ==
    /// 1`); read back on the next attempt to decide reuse (`iat` still
    /// under 60 s old) vs. a fresh mint.
    renewal_proof: Option<PendingRenewalProof>,
    /// Guards the exhausted-renewal log line against firing more than once
    /// per exhaustion episode. `status()`'s return value itself is a plain
    /// poll of current state and is unaffected by this flag.
    renewal_failure_logged: bool,
    /// td-811657: a NEW counter, distinct from `credential_generation` and
    /// from `AccountSession`'s own internal `generation` (both of which
    /// already advance on an ordinary successful renewal). Stable across
    /// ordinary access/refresh rotations; increments only at a fresh
    /// login-exchange commit, `invalidate_local`, and a definitive
    /// revoke/`invalid_grant` outcome (`RenewalFailure::ReauthenticationRequired`).
    lifecycle_epoch: u64,
    /// td-811657: the safe, non-network reason the account most recently
    /// stopped being usable — set only at the same points that bump
    /// `lifecycle_epoch` to a disconnected state (`invalidate_local`, a
    /// definitive revoke/`invalid_grant`), cleared on the next successful
    /// login-exchange commit. `None` for "never disconnected this way" —
    /// the exhausted-transient-retry case is derived live from
    /// `AccountSession::status()` instead of stored here (see
    /// `snapshot_from_inner`).
    last_end_reason: Option<AccountEndReason>,
}

impl Default for Inner {
    fn default() -> Self {
        Self {
            login_flow: LoginFlow::default(),
            phase: Phase::Idle,
            device_key: None,
            device_id: None,
            account_display: None,
            access_token: None,
            session: bridge_core::account::AccountSession::default(),
            credential_generation: 0,
            web_lease_expires_at: None,
            renewal_proof: None,
            renewal_failure_logged: false,
            lifecycle_epoch: 0,
            last_end_reason: None,
        }
    }
}

/// One reusable/re-mintable DPoP proof for the renewal scheduler's own
/// transport-retry chain (contract v5 section 5.5) — deliberately separate
/// from `build_headers`'s always-fresh minting, and from `call`'s own
/// always-fresh, exactly-once nonce-challenge retry.
#[derive(Clone)]
struct PendingRenewalProof {
    headers: Vec<(String, String)>,
    iat_unix: u64,
}

/// Classification of one `/token` renewal-scheduler refresh attempt, before
/// any commit decision (contract v5 section 5.6: DPoP failures never
/// revoke; section 11's `invalid_grant` reason list is the only definitive
/// set).
enum RenewalAttemptOutcome {
    Success(ParsedRefresh),
    /// `invalid_grant` with a reason in [`DEFINITIVE_REFRESH_REASONS`],
    /// carrying that SAME match's safe classification (td-811657) — never a
    /// second, independent re-derivation of which of the six reasons this
    /// was.
    Definitive(AccountEndReason),
    /// Everything else — network/timeout/DPoP-proof errors, 429, 5xx, or a
    /// malformed 200 body — retries within the existing budget.
    Transient {
        retry_after: Option<Duration>,
    },
}

/// The ONE place a raw `invalid_grant` reason (already confirmed to be in
/// [`DEFINITIVE_REFRESH_REASONS`]) is mapped to a safe, no-raw-body end
/// reason — called only from the same match that classifies it, so this can
/// never disagree with [`DEFINITIVE_REFRESH_REASONS`] itself.
fn map_definitive_reason(reason: &str) -> AccountEndReason {
    match reason {
        "device_revoked" => AccountEndReason::Revoked,
        "account_changed" => AccountEndReason::AccountChanged,
        // "refresh_expired" | "refresh_absolute_expired"
        // | "refresh_reuse_detected" | "refresh_orphaned"
        _ => AccountEndReason::SignedOut,
    }
}

/// The frozen v5 section 11 `invalid_grant` reasons that mean the refresh
/// credential itself is dead: retrying cannot repair the session, so these
/// skip the transient-retry budget entirely and go straight to
/// sign-in-required. `code_expired`/`code_already_used`/
/// `code_verifier_mismatch` are login-exchange-only reasons and are
/// deliberately excluded — they can never appear on a refresh grant.
/// Scheduler sleep bounds (td-cab09a). The lower bound keeps a loop that
/// cannot make progress from spinning; the upper bound is the blind re-check
/// cadence that makes sleep/resume, clock jumps and offline gaps self-correct.
const MIN_RENEWAL_SLEEP: Duration = Duration::from_secs(1);
const MAX_RENEWAL_SLEEP: Duration = Duration::from_secs(60);

const DEFINITIVE_REFRESH_REASONS: [&str; 6] = [
    "refresh_expired",
    "refresh_absolute_expired",
    "refresh_reuse_detected",
    "refresh_orphaned",
    "device_revoked",
    "account_changed",
];

pub struct AccountService {
    transport: Arc<dyn AccountTransport>,
    store: Arc<dyn SecretStore>,
    browser: Arc<dyn BrowserLauncher>,
    clock: Arc<dyn Clock>,
    rng: Arc<dyn SecureRandom>,
    profile: ClientProfile,
    port: AtomicU16,
    inner: Mutex<Inner>,
    /// The ONE shared serialization boundary for every credential-mutation
    /// and key-initialization commit: device-key load/generate/save, device
    /// registration, the login-exchange commit, refresh persistence and
    /// invalidate/logout's wipe. Never held across the login/refresh network
    /// `.await` itself (that response is produced uncommitted, then this
    /// lock guards only the recheck-and-commit phase); device registration
    /// is the sole intentional exception, since making that single network
    /// call itself single-flight is the point (see `ensure_registered`).
    commit_lock: tokio::sync::Mutex<()>,
    /// td-811657: one `watch` sender owned by this service, seeded at
    /// construction and re-sent on every mutation that already exists
    /// (login-exchange commit, renewal success/failure commit,
    /// `invalidate_local`, `revoke_device`, `status()`'s own expiry
    /// transition) plus every renewal-scheduler tick — reusing the
    /// scheduler's existing `watch` pattern (`renewal.rs`) rather than
    /// adding a second timer/task. `watch` itself coalesces a slow
    /// receiver to the latest value.
    snapshot_tx: tokio::sync::watch::Sender<AccountSnapshot>,
}

// ── Wire-shape response fragments (only the fields this service needs) ──────

#[derive(serde::Deserialize)]
struct AuthorizeResponse {
    authorization_url: String,
    expires_in: u64,
}

#[derive(serde::Deserialize)]
struct TokenResponse {
    access_token: String,
    expires_in: u64,
    refresh_token: String,
    /// Frozen v5 section 4.4: the absolute UTC instant this refresh token's
    /// own (90-day sliding) window expires, RFC 3339. Required — the
    /// contract always sends an absolute timestamp here, never a relative
    /// duration the client would have to trust its own clock to convert.
    refresh_expires_at: String,
    /// Frozen v5 section 4.4: the absolute UTC instant of the 365-day
    /// vacation-guarantee cap, RFC 3339. Required, and always persisted
    /// verbatim from this field — never locally recomputed from "now", so a
    /// later refresh/restart can never silently push this cap into the
    /// future.
    refresh_absolute_expires_at: String,
    /// Frozen v5 section 7.1.1: equals the minted access token's own `exp`,
    /// RFC 3339. Required on every `/token` response (initial exchange and
    /// refresh) — only a successful `/token` call ever advances the stored
    /// value (section 7.1). Malformed/missing rejects the whole response,
    /// same rule as the two refresh-expiry fields above.
    web_lease_expires_at: String,
    account: AccountObject,
}

#[derive(serde::Deserialize, Clone)]
struct AccountObject {
    nickname: String,
    #[serde(default)]
    clan_tag: Option<String>,
    #[serde(default)]
    realm: Option<String>,
}

fn base64_decode_32(value: &str) -> Result<[u8; 32], ()> {
    let bytes = URL_SAFE_NO_PAD.decode(value).map_err(|_| ())?;
    bytes.try_into().map_err(|_| ())
}

/// Parses one of the frozen v5 RFC 3339 absolute-UTC expiry fields
/// (`refresh_expires_at`, `refresh_absolute_expires_at`) into Unix seconds.
/// Malformed or pre-epoch values are rejected outright — this never falls
/// back to a locally-guessed duration, which is exactly the defect that let
/// a refresh silently reset the 365-day absolute cap.
fn parse_rfc3339_unix(value: &str) -> Result<u64, ()> {
    let parsed = chrono::DateTime::parse_from_rfc3339(value).map_err(|_| ())?;
    u64::try_from(parsed.timestamp()).map_err(|_| ())
}

/// Everything a successful `/token` response commits, shared by the initial
/// exchange, the one-shot bootstrap/restart refresh and the recurring
/// renewal scheduler so the parse/validation logic lives in exactly one
/// place (td-cab09a: "adopts one-shot bootstrap try_refresh transport/parser
/// rather than duplicating it").
struct ParsedRefresh {
    refresh: StoredRefresh,
    access_token: String,
    lifetime: Duration,
    renew_in: Duration,
    web_lease_expires_at_unix: u64,
}

/// Parses one `/token` success body. Every required absolute-UTC timestamp
/// (v5 section 4.4/7.1.1) is validated here; a missing or malformed value
/// rejects the WHOLE response (`None`) rather than falling back to a locally
/// guessed duration — never a partial commit.
fn parse_token_response(body: &serde_json::Value) -> Option<ParsedRefresh> {
    let parsed: TokenResponse = serde_json::from_value(body.clone()).ok()?;
    let refresh_expires_at_unix = parse_rfc3339_unix(&parsed.refresh_expires_at).ok()?;
    let refresh_absolute_expires_at_unix =
        parse_rfc3339_unix(&parsed.refresh_absolute_expires_at).ok()?;
    let web_lease_expires_at_unix = parse_rfc3339_unix(&parsed.web_lease_expires_at).ok()?;
    let lifetime = Duration::from_secs(parsed.expires_in.max(1));
    let renew_in = lifetime.mul_f32(0.5);
    Some(ParsedRefresh {
        refresh: StoredRefresh {
            refresh_token: parsed.refresh_token,
            refresh_expires_at_unix,
            refresh_absolute_expires_at_unix,
            account: StoredAccount {
                nickname: parsed.account.nickname,
                clan_tag: parsed.account.clan_tag,
                realm: parsed.account.realm,
            },
        },
        access_token: parsed.access_token,
        lifetime,
        renew_in,
        web_lease_expires_at_unix,
    })
}

/// The Engine's own production origin, the only value this checks against
/// — matches `http::PRODUCTION_BASE_URL`'s origin.
const ENGINE_ORIGIN_HOST: &str = "engine.tfd.rocks";
/// The frozen v5 browser-authorization route. Any other path is rejected
/// outright rather than accepted as an unspecified "variant".
const AUTHORIZATION_URL_PATH: &str = "/auth/bridge";

/// Validates a `POST /authorize` response's `authorization_url` against the
/// frozen v5 contract shape before it is ever stored (for reopen) or handed
/// to the OS browser opener: exact Engine origin, default HTTPS port only,
/// the frozen `/auth/bridge` route, no embedded credentials, no fragment.
/// Query parameters (e.g. `request_id`) are the only variable part. A
/// compromised/misrouted Engine returning anything else must never reach
/// the browser opener.
fn validate_authorization_url(url: &str) -> Result<(), ()> {
    let parsed = reqwest::Url::parse(url).map_err(|_| ())?;
    if parsed.scheme() != "https" {
        return Err(());
    }
    if parsed.host_str() != Some(ENGINE_ORIGIN_HOST) {
        return Err(());
    }
    // `Url::port()` is `None` exactly when the port is the scheme's default
    // (443 for https); any explicit port — default or not — is rejected.
    if parsed.port().is_some() {
        return Err(());
    }
    if parsed.path() != AUTHORIZATION_URL_PATH {
        return Err(());
    }
    if !parsed.username().is_empty() || parsed.password().is_some() {
        return Err(());
    }
    if parsed.fragment().is_some() {
        return Err(());
    }
    Ok(())
}

impl AccountService {
    pub fn new(
        transport: Arc<dyn AccountTransport>,
        store: Arc<dyn SecretStore>,
        browser: Arc<dyn BrowserLauncher>,
        clock: Arc<dyn Clock>,
        rng: Arc<dyn SecureRandom>,
        profile: ClientProfile,
    ) -> Self {
        // Matches `Inner::default()` exactly: signed out, unusable, epoch 0,
        // no identity, no end reason — so a subscriber attaching before any
        // mutation still observes a well-formed initial snapshot.
        let (snapshot_tx, _) = tokio::sync::watch::channel(AccountSnapshot {
            status: AccountStatus::SignedOut,
            can_use_account: false,
            lifecycle_epoch: 0,
            nickname: None,
            clan_tag: None,
            realm: None,
            web_lease_expires_at: None,
            access_expires_at_unix: None,
            end_reason: None,
        });
        Self {
            transport,
            store,
            browser,
            clock,
            rng,
            profile,
            port: AtomicU16::new(0),
            inner: Mutex::new(Inner::default()),
            commit_lock: tokio::sync::Mutex::new(()),
            snapshot_tx,
        }
    }

    /// Set once the bridge listener's actual bound port is known. Must be
    /// called before `begin_login` (the redirect_uri needs it).
    pub fn set_port(&self, port: u16) {
        self.port.store(port, Ordering::SeqCst);
    }

    /// Safe, IPC-exposable status. No tokens, no device key, no flow state.
    pub fn status(&self) -> AccountStatus {
        let now = self.clock.now_instant();
        let mut inner = self.inner.lock().unwrap();
        // Production expiry path: nothing else polls the pending attempt's
        // own deadline when the browser is simply never brought back (no
        // callback, no manual reopen), so without this every such attempt
        // would report `AwaitingBrowser` forever. `expire_if_due` is a
        // no-op once nothing is pending (including after a normal
        // consume/cancel), so this is safe to call unconditionally.
        if inner.login_flow.expire_if_due(now) {
            inner.phase = Phase::Expired;
            // td-811657: this phase transition is externally visible (a
            // pending sign-in silently became `Expired`) and nothing else
            // would otherwise notify a snapshot subscriber of it.
            let snap = self.snapshot_from_inner(&inner, now);
            self.snapshot_tx.send_replace(snap);
        }
        match inner.session.status() {
            bridge_core::account::AccountStatus::Connected
            | bridge_core::account::AccountStatus::Renewing => {
                // Silent per contract §0: a proactive renewal in flight or
                // being retried never surfaces differently from a plain
                // connected account.
                let account = inner.account_display.clone().unwrap_or(StoredAccount {
                    nickname: String::new(),
                    clan_tag: None,
                    realm: None,
                });
                return AccountStatus::Connected {
                    nickname: account.nickname,
                    clan_tag: account.clan_tag,
                    realm: account.realm,
                    web_lease_expires_at: inner.web_lease_expires_at,
                };
            }
            bridge_core::account::AccountStatus::RenewalFailed => {
                // Log exactly once per exhaustion episode; every poll still
                // returns the same `RenewalFailed` status below (that is
                // current state, not a repeated notification).
                if !inner.renewal_failure_logged {
                    inner.renewal_failure_logged = true;
                    log::warn!(
                        "Account renewal exhausted its 3-attempt retry budget; sign-in is required to recover."
                    );
                }
                let account = inner.account_display.clone().unwrap_or(StoredAccount {
                    nickname: String::new(),
                    clan_tag: None,
                    realm: None,
                });
                return AccountStatus::RenewalFailed {
                    nickname: account.nickname,
                    clan_tag: account.clan_tag,
                    realm: account.realm,
                };
            }
            bridge_core::account::AccountStatus::SignInRequired => {}
        }
        match inner.phase {
            Phase::Idle => AccountStatus::SignedOut,
            Phase::AwaitingBrowser => AccountStatus::AwaitingBrowser,
            Phase::Exchanging => AccountStatus::Exchanging,
            Phase::Canceled => AccountStatus::Canceled,
            Phase::Denied => AccountStatus::Denied,
            Phase::AccountNotAuthorized => AccountStatus::AccountNotAuthorized,
            Phase::Expired => AccountStatus::Expired,
            Phase::OpenFailed => AccountStatus::OpenFailed,
            Phase::StoreUnavailable => AccountStatus::StoreUnavailable,
        }
    }

    // ── Native lifecycle snapshot / subscription / signing seam (td-811657) ─

    /// Pure mapping from internal state to the safe public `status` value —
    /// no mutation, no logging (the log-once-per-exhaustion-episode side
    /// effect stays exclusively `status()`'s own, so this can be called from
    /// a context that already holds `inner`'s lock without re-entering it).
    /// Deliberately mirrors `status()`'s own match arms so both stay in
    /// agreement; kept separate rather than having `status()` call this (and
    /// risk perturbing that hot, already-tested path) per the surgical-change
    /// principle for this task.
    fn account_status_pure(inner: &Inner) -> AccountStatus {
        match inner.session.status() {
            bridge_core::account::AccountStatus::Connected
            | bridge_core::account::AccountStatus::Renewing => {
                let account = inner.account_display.clone().unwrap_or(StoredAccount {
                    nickname: String::new(),
                    clan_tag: None,
                    realm: None,
                });
                AccountStatus::Connected {
                    nickname: account.nickname,
                    clan_tag: account.clan_tag,
                    realm: account.realm,
                    web_lease_expires_at: inner.web_lease_expires_at,
                }
            }
            bridge_core::account::AccountStatus::RenewalFailed => {
                let account = inner.account_display.clone().unwrap_or(StoredAccount {
                    nickname: String::new(),
                    clan_tag: None,
                    realm: None,
                });
                AccountStatus::RenewalFailed {
                    nickname: account.nickname,
                    clan_tag: account.clan_tag,
                    realm: account.realm,
                }
            }
            bridge_core::account::AccountStatus::SignInRequired => match inner.phase {
                Phase::Idle => AccountStatus::SignedOut,
                Phase::AwaitingBrowser => AccountStatus::AwaitingBrowser,
                Phase::Exchanging => AccountStatus::Exchanging,
                Phase::Canceled => AccountStatus::Canceled,
                Phase::Denied => AccountStatus::Denied,
                Phase::AccountNotAuthorized => AccountStatus::AccountNotAuthorized,
                Phase::Expired => AccountStatus::Expired,
                Phase::OpenFailed => AccountStatus::OpenFailed,
                Phase::StoreUnavailable => AccountStatus::StoreUnavailable,
            },
        }
    }

    /// Builds one `AccountSnapshot` atomically from an already-locked
    /// `Inner` and an injected `now` — used both by `snapshot()` (which
    /// locks fresh) and by `status()`'s own expire-transition publish
    /// (which already holds the lock and must not re-enter it).
    fn snapshot_from_inner(&self, inner: &Inner, now: Instant) -> AccountSnapshot {
        let status = Self::account_status_pure(inner);
        // SAME semantics as `AccountSession::can_use_account`: false at
        // access expiry even while a renewal retry is in flight or waiting.
        // `status()` itself never surfaces this distinction (`Connected`
        // and `Renewing` both map to the public `Connected` shape) — this
        // is exactly the gap this snapshot exists to close.
        let can_use_account = inner.session.can_use_account(now);
        let account = inner.account_display.clone();
        let access_expires_at_unix = inner.access_token.as_ref().map(|(_, expiry)| {
            let now_unix = self.clock.now_unix().as_secs();
            now_unix + expiry.saturating_duration_since(now).as_secs()
        });
        let end_reason = if can_use_account {
            None
        } else {
            match inner.session.status() {
                // Derived live from session state, never stored — exhaustion
                // is a fact about the session, not a one-time event that
                // could go stale.
                bridge_core::account::AccountStatus::RenewalFailed => {
                    Some(AccountEndReason::RenewalExhausted)
                }
                _ => inner.last_end_reason,
            }
        };
        AccountSnapshot {
            status,
            can_use_account,
            lifecycle_epoch: inner.lifecycle_epoch,
            nickname: account.as_ref().map(|a| a.nickname.clone()),
            clan_tag: account.as_ref().and_then(|a| a.clan_tag.clone()),
            realm: account.as_ref().and_then(|a| a.realm.clone()),
            web_lease_expires_at: inner.web_lease_expires_at,
            access_expires_at_unix,
            end_reason,
        }
    }

    /// Safe, atomic, native-only snapshot: the same status `status()`
    /// exposes, plus `can_use_account`/`lifecycle_epoch`/identity/expiries/
    /// end-reason — built fresh from `Inner` at the injected clock's current
    /// instant, so it fails closed even if nobody has polled `status()`
    /// recently (an access-token expiry becomes visible here purely by
    /// comparing `now`, with no stored mutation required).
    pub fn snapshot(&self) -> AccountSnapshot {
        let now = self.clock.now_instant();
        let inner = self.inner.lock().unwrap();
        self.snapshot_from_inner(&inner, now)
    }

    /// Re-derives the current snapshot and publishes it to every
    /// subscriber. Called from every mutation that already exists — never a
    /// second timer/service. `send_replace` retains the current value even
    /// when no receiver is subscribed, so a later subscriber cannot miss a
    /// completed lifecycle mutation.
    fn publish_snapshot(&self) {
        let snap = self.snapshot();
        self.snapshot_tx.send_replace(snap);
    }

    /// A subscriber immediately observes the current snapshot (the `watch`
    /// channel is seeded at construction and re-sent on every mutation), and
    /// every later mutation without polling. `watch` itself coalesces a slow
    /// receiver to the latest value.
    #[allow(dead_code)]
    pub fn subscribe(&self) -> tokio::sync::watch::Receiver<AccountSnapshot> {
        self.snapshot_tx.subscribe()
    }

    /// Native-only signed-request seam for the exact, task-owned allowlist
    /// in [`SignedEndpoint`] — never a caller-supplied URL/method. Requires
    /// `can_use_account` true at call time under `commit_lock` (the SAME
    /// lock `invalidate_local`/renewal commits use), so this cannot race a
    /// concurrent commit that is about to make the account unusable. Returns
    /// native header material only; the consumer must recheck
    /// `lifecycle_epoch` against a fresh `snapshot()` immediately before
    /// applying any response built from these headers (mirroring the
    /// existing stale-response rejection pattern in
    /// `renewal_tick`/`commit_renewal_*`). Does NOT call the endpoint
    /// itself — building the request is this task's whole job.
    #[allow(dead_code)]
    pub async fn sign_request(
        &self,
        endpoint: SignedEndpoint,
    ) -> Result<SignedRequestMaterial, AccountError> {
        let _commit = self.commit_lock.lock().await;
        let now = self.clock.now_instant();
        let inner = self.inner.lock().unwrap();
        if !inner.session.can_use_account(now) {
            return Err(AccountError::NotReady);
        }
        let Some(device_key) = inner.device_key.clone() else {
            return Err(AccountError::NotReady);
        };
        let Some((access_token, _)) = inner.access_token.clone() else {
            return Err(AccountError::NotReady);
        };
        let lifecycle_epoch = inner.lifecycle_epoch;
        drop(inner);
        let htu = format!("{}{}", super::http::PRODUCTION_BASE_URL, endpoint.path());
        let headers =
            self.build_headers(&device_key, endpoint.htm(), &htu, Some(&access_token), None);
        Ok(SignedRequestMaterial {
            headers,
            lifecycle_epoch,
        })
    }

    // ── Device key / registration ────────────────────────────────────────────

    /// Serializes the device-key load/generate/save sequence through
    /// `commit_lock` so two concurrent callers (e.g. two `begin_login`
    /// clicks) can never each generate and save a distinct key. A caller
    /// that finds `device_key` already set (either originally, or set by
    /// whoever held `commit_lock` first while this call was waiting for it)
    /// never touches the store at all.
    async fn ensure_device_key(&self) -> Result<DeviceKey, AccountError> {
        if let Some(key) = self.inner.lock().unwrap().device_key.clone() {
            return Ok(key);
        }
        let _commit = self.commit_lock.lock().await;
        self.ensure_device_key_locked()
    }

    /// The load/generate/save body, factored out so `ensure_registered` can
    /// run it while already holding `commit_lock` for the whole
    /// registration flow, instead of re-entering the (non-reentrant) lock.
    fn ensure_device_key_locked(&self) -> Result<DeviceKey, AccountError> {
        if let Some(key) = self.inner.lock().unwrap().device_key.clone() {
            return Ok(key);
        }
        let existing = self
            .store
            .load_profile(self.profile)
            .map_err(AccountError::Store)?;
        if let Some(data) = &existing {
            if let Ok(bytes) = base64_decode_32(&data.device_key_scalar_b64) {
                if let Ok(key) = DeviceKey::from_scalar_bytes(&bytes) {
                    let mut inner = self.inner.lock().unwrap();
                    inner.device_key = Some(key.clone());
                    inner.device_id = data.device_id.clone();
                    return Ok(key);
                }
            }
        }
        // Persist BEFORE any network call (contract: "Persist key before
        // /device"), so a crash between generation and registration never
        // leaves an unregistered key that /device might reject or a
        // registered key we forgot to keep.
        let mut key = None;
        for _ in 0..4 {
            let mut scalar = [0u8; 32];
            self.rng.fill(&mut scalar);
            if let Ok(k) = DeviceKey::from_scalar_bytes(&scalar) {
                key = Some((k, scalar));
                break;
            }
        }
        let (key, scalar) = key.ok_or(AccountError::InvalidResponse)?;
        let data = StoredProfileData {
            schema_version: STORED_PROFILE_SCHEMA_VERSION,
            device_key_scalar_b64: URL_SAFE_NO_PAD.encode(scalar),
            device_id: existing.as_ref().and_then(|d| d.device_id.clone()),
            refresh_current: existing.as_ref().and_then(|d| d.refresh_current.clone()),
            refresh_pending: existing.as_ref().and_then(|d| d.refresh_pending.clone()),
        };
        self.store
            .save_profile(self.profile, &data)
            .map_err(AccountError::Store)?;
        self.inner.lock().unwrap().device_key = Some(key.clone());
        Ok(key)
    }

    /// Registration is single-flight through `commit_lock`, held across the
    /// one `/device` network call: a second concurrent `begin_login` blocks
    /// here instead of generating a second key or registering a second,
    /// inconsistent device — it re-checks `device_id` after acquiring the
    /// lock and, finding it already set by the first caller, returns the
    /// same cached metadata without ever calling the network again.
    async fn ensure_registered(&self) -> Result<String, AccountError> {
        if let Some(id) = self.inner.lock().unwrap().device_id.clone() {
            return Ok(id);
        }
        let _commit = self.commit_lock.lock().await;
        if let Some(id) = self.inner.lock().unwrap().device_id.clone() {
            return Ok(id);
        }
        let device_key = self.ensure_device_key_locked()?;
        let install_id = self.store.install_id().map_err(AccountError::Store)?;
        let jwk = device_key.public_jwk();
        let body = json!({
            "public_jwk": {"kty": "EC", "crv": "P-256", "x": jwk.x, "y": jwk.y},
            "device_name": device_name(),
            "platform": platform_name(),
            "bridge_version": crate::engine::BRIDGE_VERSION,
            "install_id": install_id,
            "client_profile": self.profile.as_str(),
        });
        let response = self.call("POST", "/device", body, None).await?;
        if response.status != 200 && response.status != 201 {
            return Err(error_from_body(response));
        }
        let device_id = response
            .body
            .get("device_id")
            .and_then(|v| v.as_str())
            .ok_or(AccountError::InvalidResponse)?
            .to_string();
        let scalar_b64 = URL_SAFE_NO_PAD.encode(device_key.to_scalar_bytes());
        let mut data = self
            .store
            .load_profile(self.profile)
            .map_err(AccountError::Store)?
            .unwrap_or_else(|| StoredProfileData {
                schema_version: STORED_PROFILE_SCHEMA_VERSION,
                device_key_scalar_b64: scalar_b64.clone(),
                device_id: None,
                refresh_current: None,
                refresh_pending: None,
            });
        data.device_id = Some(device_id.clone());
        self.store
            .save_profile(self.profile, &data)
            .map_err(AccountError::Store)?;
        self.inner.lock().unwrap().device_id = Some(device_id.clone());
        Ok(device_id)
    }

    // ── Login: begin / cancel / reopen ───────────────────────────────────────

    pub async fn begin_login(self: &Arc<Self>) -> Result<(), AccountError> {
        let port = self.port.load(Ordering::SeqCst);
        if port == 0 {
            return Err(AccountError::NotReady);
        }
        let device_id = self.ensure_registered().await?;

        let mut verifier_bytes = [0u8; bridge_core::pkce::VERIFIER_ENTROPY_BYTES];
        self.rng.fill(&mut verifier_bytes);
        let verifier = bridge_core::pkce::code_verifier_from_bytes(&verifier_bytes);
        let challenge = bridge_core::pkce::code_challenge_s256(&verifier);
        let mut state_bytes = [0u8; bridge_core::pkce::STATE_ENTROPY_BYTES];
        self.rng.fill(&mut state_bytes);
        let state = bridge_core::pkce::state_from_bytes(&state_bytes);
        let redirect_uri = format!("http://127.0.0.1:{port}/auth/callback");

        let body = json!({
            "device_id": device_id,
            "code_challenge": challenge,
            "code_challenge_method": "S256",
            "redirect_uri": redirect_uri,
            "state": state,
            "scope": SCOPE,
        });
        let response = self.call("POST", "/authorize", body, None).await?;
        if response.status != 200 {
            return Err(error_from_body(response));
        }
        let parsed: AuthorizeResponse =
            serde_json::from_value(response.body).map_err(|_| AccountError::InvalidResponse)?;
        // Validate BEFORE it is ever stored or handed to the OS browser
        // opener: a compromised/misrouted Engine returning anything outside
        // the frozen contract shape must never reach either.
        validate_authorization_url(&parsed.authorization_url)
            .map_err(|_| AccountError::InvalidResponse)?;
        let ttl = Duration::from_secs(
            parsed
                .expires_in
                .min(bridge_core::login_flow::MAX_TTL.as_secs()),
        );
        let now = self.clock.now_instant();
        {
            let mut inner = self.inner.lock().unwrap();
            inner
                .login_flow
                .begin(
                    state,
                    verifier,
                    redirect_uri,
                    parsed.authorization_url.clone(),
                    now,
                    ttl,
                )
                .map_err(|_| AccountError::InvalidResponse)?;
            inner.phase = Phase::AwaitingBrowser;
        }
        if !self.browser.open(&parsed.authorization_url) {
            self.inner.lock().unwrap().phase = Phase::OpenFailed;
        }
        Ok(())
    }

    /// Cancels any pending attempt. A late callback for it becomes
    /// `UnknownState` at the flow layer and a stale in-flight exchange is
    /// discarded by the generation recheck in `finish_login_exchange`.
    pub fn cancel_login(&self) {
        let mut inner = self.inner.lock().unwrap();
        inner.login_flow.cancel();
        inner.phase = Phase::Canceled;
    }

    /// Reopens the OS browser on the SAME stored, already-validated URL.
    /// Never accepts a caller-supplied URL and never creates a second
    /// pending attempt.
    pub fn reopen_pending(&self) -> Result<(), AccountError> {
        let now = self.clock.now_instant();
        let url = {
            let inner = self.inner.lock().unwrap();
            inner.login_flow.reopen_url(now).map(|s| s.to_string())
        };
        match url {
            Ok(url) => {
                if self.browser.open(&url) {
                    self.inner.lock().unwrap().phase = Phase::AwaitingBrowser;
                    Ok(())
                } else {
                    self.inner.lock().unwrap().phase = Phase::OpenFailed;
                    Err(AccountError::OpenFailed)
                }
            }
            Err(bridge_core::login_flow::LoginFlowError::Expired) => {
                self.inner.lock().unwrap().phase = Phase::Expired;
                Err(AccountError::NoPendingLogin)
            }
            Err(_) => Err(AccountError::NoPendingLogin),
        }
    }

    // ── Callback (AuthCallbackSink) ──────────────────────────────────────────

    /// Called synchronously from the bridge's HTTP handler thread. Consumes
    /// the callback (if valid) under a short lock, then returns immediately;
    /// the actual `/token` exchange runs afterwards on the async runtime —
    /// "atomically consume before async exchange".
    fn on_callback(self: &Arc<Self>, callback: AuthCallback) -> AuthCallbackAck {
        let now = self.clock.now_instant();
        enum Outcome {
            Exchange(String, ConsumedLogin),
            Terminal(AuthCallbackAck),
            Rejected,
        }
        let outcome = {
            let mut inner = self.inner.lock().unwrap();
            match callback.outcome {
                AuthCallbackOutcome::Code(code) => {
                    match inner.login_flow.consume_code(&callback.state, now) {
                        Ok(consumed) => {
                            inner.phase = Phase::Exchanging;
                            Outcome::Exchange(code, consumed)
                        }
                        // A late callback whose own deadline already passed is
                        // reported as Expired (not a generic rejection), and the
                        // safe status must reflect that without waiting for a
                        // separate `status()` poll to notice.
                        Err(LoginFlowError::Expired) => {
                            inner.phase = Phase::Expired;
                            Outcome::Terminal(AuthCallbackAck::Expired)
                        }
                        Err(_) => Outcome::Rejected,
                    }
                }
                AuthCallbackOutcome::AccessDenied => {
                    match inner.login_flow.consume_terminal(
                        &callback.state,
                        now,
                        TerminalReason::Denied,
                    ) {
                        Ok(_) => {
                            inner.phase = Phase::Denied;
                            Outcome::Terminal(AuthCallbackAck::Denied)
                        }
                        Err(LoginFlowError::Expired) => {
                            inner.phase = Phase::Expired;
                            Outcome::Terminal(AuthCallbackAck::Expired)
                        }
                        Err(_) => Outcome::Rejected,
                    }
                }
                AuthCallbackOutcome::AccountNotAuthorized => {
                    match inner.login_flow.consume_terminal(
                        &callback.state,
                        now,
                        TerminalReason::AccountNotAuthorized,
                    ) {
                        Ok(_) => {
                            inner.phase = Phase::AccountNotAuthorized;
                            Outcome::Terminal(AuthCallbackAck::AccountNotAuthorized)
                        }
                        Err(LoginFlowError::Expired) => {
                            inner.phase = Phase::Expired;
                            Outcome::Terminal(AuthCallbackAck::Expired)
                        }
                        Err(_) => Outcome::Rejected,
                    }
                }
                AuthCallbackOutcome::ExpiredRequest => {
                    match inner.login_flow.consume_terminal(
                        &callback.state,
                        now,
                        TerminalReason::Expired,
                    ) {
                        Ok(_) => {
                            inner.phase = Phase::Expired;
                            Outcome::Terminal(AuthCallbackAck::Expired)
                        }
                        Err(LoginFlowError::Expired) => {
                            inner.phase = Phase::Expired;
                            Outcome::Terminal(AuthCallbackAck::Expired)
                        }
                        Err(_) => Outcome::Rejected,
                    }
                }
            }
        };
        match outcome {
            Outcome::Rejected => AuthCallbackAck::Rejected,
            Outcome::Terminal(ack) => ack,
            Outcome::Exchange(code, consumed) => {
                let service = Arc::clone(self);
                tauri::async_runtime::spawn(async move {
                    service.complete_exchange(code, consumed).await;
                });
                // Never claim a connection here: the exchange above still
                // has to succeed AND its refresh credentials still have to
                // be durably persisted before `status()` may ever report
                // `Connected` (see `finish_login_exchange`).
                AuthCallbackAck::Pending
            }
        }
    }

    async fn complete_exchange(self: Arc<Self>, code: String, consumed: ConsumedLogin) {
        let device_id = self.inner.lock().unwrap().device_id.clone();
        let Some(device_id) = device_id else { return };
        let body = json!({
            "grant_type": "authorization_code",
            "code": code,
            "code_verifier": consumed.code_verifier,
            "device_id": device_id,
            "redirect_uri": consumed.redirect_uri,
        });
        let result = self.call("POST", "/token", body, None).await;
        self.finish_login_exchange(consumed.generation, result)
            .await;
    }

    /// Parsing happens first and produces an uncommitted result; only the
    /// generation recheck, durable persistence and `Inner`/session update
    /// happen under `commit_lock` — the SAME boundary `try_refresh` and
    /// `invalidate_local` use — so a `cancel_login`/`invalidate_local`/
    /// racing refresh that lands while this network call was in flight
    /// cannot be raced by a response that commits credentials afterwards,
    /// and this commit cannot be raced by one of theirs either.
    async fn finish_login_exchange(
        &self,
        generation: u64,
        result: Result<HttpResponse, AccountError>,
    ) {
        self.finish_login_exchange_impl(generation, result).await;
        // td-811657: a fresh login-exchange commit is a lifecycle mutation
        // (identity establishment/replacement) even when this call ends up
        // being a stale/rejected no-op — publishing the unchanged current
        // snapshot in that case is harmless (watch coalesces).
        self.publish_snapshot();
    }

    async fn finish_login_exchange_impl(
        &self,
        generation: u64,
        result: Result<HttpResponse, AccountError>,
    ) {
        let response = match result {
            Ok(r) if r.status == 200 => r,
            _ => return,
        };
        // Every required absolute-UTC field (v5 section 4.4/7.1.1) is
        // validated here; a malformed/missing value rejects the whole
        // response outright rather than guessing — never commit a
        // credential whose expiry we could not establish, and never log the
        // response that carried it.
        let Some(parsed) = parse_token_response(&response.body) else {
            return;
        };
        let now_instant = self.clock.now_instant();

        let _commit = self.commit_lock.lock().await;
        let mut inner = self.inner.lock().unwrap();
        if inner.login_flow.current_generation() != generation {
            // Stale: cancelled, replaced by a new attempt, or logged out
            // while the network call was outstanding. Never commit it.
            return;
        }
        let Some(device_id) = inner.device_id.clone() else {
            return;
        };
        let Some(device_key) = inner.device_key.clone() else {
            return;
        };
        let scalar_b64 = URL_SAFE_NO_PAD.encode(device_key.to_scalar_bytes());
        if self
            .persist_new_refresh(parsed.refresh.clone(), &device_id, &scalar_b64)
            .is_err()
        {
            // "Initial exchange store failure stays disconnected and
            // requires fresh login" — never show Connected here.
            inner.phase = Phase::StoreUnavailable;
            return;
        }
        if inner
            .session
            .connect(now_instant, parsed.lifetime, parsed.renew_in)
        {
            inner.access_token = Some((parsed.access_token, now_instant + parsed.lifetime));
            inner.account_display = Some(parsed.refresh.account);
            inner.web_lease_expires_at = Some(parsed.web_lease_expires_at_unix);
            inner.phase = Phase::Idle;
            inner.credential_generation = inner.credential_generation.wrapping_add(1);
            inner.renewal_failure_logged = false;
            inner.renewal_proof = None;
            // td-811657: a fresh login-exchange commit — first login OR an
            // account switch — always establishes/replaces identity, so it
            // always advances the lifecycle epoch and clears any prior
            // end reason, distinct from `credential_generation` above (which
            // an ordinary renewal also advances).
            inner.lifecycle_epoch = inner.lifecycle_epoch.wrapping_add(1);
            inner.last_end_reason = None;
        }
    }

    /// Contract v5 section 5.8's recommended two-slot shape: write `pending`
    /// first and verify it, then promote it to `current`. Never touches
    /// `current` until the `pending` write has already succeeded, so a
    /// crash/store failure between the two calls always leaves at least one
    /// slot usable on the next restart.
    fn persist_new_refresh(
        &self,
        refresh: StoredRefresh,
        device_id: &str,
        scalar_b64: &str,
    ) -> Result<(), StoreError> {
        let existing = self.store.load_profile(self.profile)?;
        let mut data = existing.unwrap_or_else(|| StoredProfileData {
            schema_version: STORED_PROFILE_SCHEMA_VERSION,
            device_key_scalar_b64: scalar_b64.to_string(),
            device_id: Some(device_id.to_string()),
            refresh_current: None,
            refresh_pending: None,
        });
        data.device_id = Some(device_id.to_string());
        data.refresh_pending = Some(refresh.clone());
        self.store.save_profile(self.profile, &data)?;
        data.refresh_current = Some(refresh);
        data.refresh_pending = None;
        self.store.save_profile(self.profile, &data)
    }

    // ── Restart: one-shot refresh, no recurring schedule (td-cab09a) ────────

    /// Loads the device key/id and attempts exactly one refresh. `pending`
    /// (the section 5.8 crash-recovery slot) is only tried when `current` is
    /// locally absent or past its absolute cap — never after the Engine
    /// rejects a present, locally-live `current`, which would be exactly the
    /// "unsafe fallback after semantic corruption/rejection" the contract
    /// forbids (a rejection there can mean the whole family, `pending`
    /// included, was just revoked for reuse/theft).
    pub async fn bootstrap(self: &Arc<Self>) {
        let existing = match self.store.load_profile(self.profile) {
            Ok(Some(d)) => d,
            Ok(None) => return,
            Err(_) => {
                self.inner.lock().unwrap().phase = Phase::StoreUnavailable;
                return;
            }
        };
        {
            // Same boundary as `ensure_device_key`: a concurrent
            // `begin_login` may be generating/saving a fresh key right now
            // (true first run) or may already have primed `Inner` from this
            // same stored profile while this read was racing it — either
            // way, never overwrite an already-primed key with a redundant
            // decode of a possibly-stale read.
            let _commit = self.commit_lock.lock().await;
            let mut inner = self.inner.lock().unwrap();
            if inner.device_key.is_none() {
                if let Ok(bytes) = base64_decode_32(&existing.device_key_scalar_b64) {
                    if let Ok(key) = DeviceKey::from_scalar_bytes(&bytes) {
                        inner.device_key = Some(key);
                        inner.device_id = existing.device_id.clone();
                    }
                }
            }
        }
        let now_unix = self.clock.now_unix().as_secs();
        let is_live = |r: &StoredRefresh| r.refresh_absolute_expires_at_unix > now_unix;

        if let Some(current) = existing.refresh_current.clone().filter(is_live) {
            if self.try_refresh(current).await {
                return;
            }
            // The Engine rejected (or a transport error prevented using) a
            // PRESENT, locally-live `current`. Falling back to `pending` here
            // would be exactly the unsafe fallback the contract forbids after
            // a semantic rejection (e.g. theft/reuse detection revokes the
            // whole family, `pending` included) — stop, a fresh login is
            // required.
            return;
        }
        // `current` was absent or locally past its absolute cap: `pending`
        // (the section 5.8 crash-recovery slot, written but never promoted)
        // is the only legitimate remaining recovery path.
        if let Some(pending) = existing.refresh_pending.clone().filter(is_live) {
            let _ = self.try_refresh(pending).await;
        }
        // Nothing usable: stays disconnected, requires fresh login.
    }

    /// Network parse produces an uncommitted result: the request is built
    /// from a `device_id`/`credential_generation` fingerprint captured
    /// BEFORE the network call, and nothing is written to the store or
    /// `Inner` until that fingerprint is rechecked under `commit_lock` —
    /// the SAME boundary `finish_login_exchange` and `invalidate_local`
    /// use — immediately before the two-slot persist and the
    /// session/access/display update. A response for a credential set that
    /// is no longer current (a fresh login committed, a refresh already
    /// rotated it, or `invalidate_local` ran) performs zero writes and zero
    /// status changes.
    async fn try_refresh(&self, refresh: StoredRefresh) -> bool {
        let ok = self.try_refresh_impl(refresh).await;
        // td-811657: bootstrap's one-shot refresh is a "refresh" mutation
        // like any other — publish, but (per contract) never touch
        // `lifecycle_epoch` here; see `try_refresh_impl`.
        self.publish_snapshot();
        ok
    }

    async fn try_refresh_impl(&self, refresh: StoredRefresh) -> bool {
        let (device_id, generation_before) = {
            let inner = self.inner.lock().unwrap();
            let Some(device_id) = inner.device_id.clone() else {
                return false;
            };
            (device_id, inner.credential_generation)
        };
        let body = json!({
            "grant_type": "refresh_token",
            "refresh_token": refresh.refresh_token,
            "device_id": device_id,
        });
        let response = match self.call("POST", "/token", body, None).await {
            Ok(r) if r.status == 200 => r,
            _ => return false,
        };
        // Same required, absolute-UTC parsing as the initial exchange (v5
        // section 4.4/7.1.1) — never re-derive an expiry from "now" here,
        // which is exactly what let a refresh silently reset the 365-day
        // absolute cap.
        let Some(parsed) = parse_token_response(&response.body) else {
            return false;
        };
        let now_instant = self.clock.now_instant();

        let _commit = self.commit_lock.lock().await;
        let mut inner = self.inner.lock().unwrap();
        if inner.credential_generation != generation_before
            || inner.device_id.as_deref() != Some(device_id.as_str())
        {
            // Stale: a newer login/refresh committed, or invalidate_local
            // wiped the account, while this response was in flight. Never
            // commit it — zero writes, zero status change.
            return false;
        }
        let Some(device_key) = inner.device_key.clone() else {
            return false;
        };
        let scalar_b64 = URL_SAFE_NO_PAD.encode(device_key.to_scalar_bytes());
        if self
            .persist_new_refresh(parsed.refresh.clone(), &device_id, &scalar_b64)
            .is_err()
        {
            return false;
        }
        if inner
            .session
            .connect(now_instant, parsed.lifetime, parsed.renew_in)
        {
            inner.access_token = Some((parsed.access_token, now_instant + parsed.lifetime));
            inner.account_display = Some(parsed.refresh.account);
            inner.web_lease_expires_at = Some(parsed.web_lease_expires_at_unix);
            inner.phase = Phase::Idle;
            inner.credential_generation = inner.credential_generation.wrapping_add(1);
            inner.renewal_failure_logged = false;
            inner.renewal_proof = None;
        }
        true
    }

    // ── Recurring renewal scheduler (td-cab09a) ─────────────────────────────

    /// Mints a fresh `/token` DPoP proof (new `jti`, current `iat`) with the
    /// same header shape `build_headers` uses for every other call —
    /// re-derived here (not shared) because the renewal path needs the
    /// minted `iat` back for the next attempt's section 5.5 reuse-window
    /// decision, which `build_headers`'s existing signature does not
    /// expose.
    fn mint_renewal_headers(
        &self,
        device_key: &DeviceKey,
        htu: &str,
        nonce: Option<&str>,
    ) -> PendingRenewalProof {
        let mut jti_bytes = [0u8; 16];
        self.rng.fill(&mut jti_bytes);
        let jti = bridge_core::dpop::jti_from_bytes(jti_bytes);
        let iat_unix = self.clock.now_unix().as_secs();
        let proof =
            bridge_core::dpop::build_proof(device_key, "POST", htu, iat_unix, &jti, nonce, None)
                .expect("htu is built from a constant base + path, never a query/fragment");
        let headers = vec![
            ("DPoP".to_string(), proof),
            (
                crate::engine::VERSION_HEADER.to_string(),
                crate::engine::BRIDGE_VERSION.to_string(),
            ),
            (
                "User-Agent".to_string(),
                crate::engine::user_agent(crate::engine::BRIDGE_VERSION),
            ),
        ];
        PendingRenewalProof { headers, iat_unix }
    }

    /// Sends one `/token` request for the recurring renewal scheduler,
    /// applying contract v5 section 5.5's transport-retry rule: reuse the
    /// exact same DPoP proof/`jti` from `reuse` as long as its `iat` is
    /// still under 60 s old at the moment of THIS send; otherwise —
    /// including the first attempt of a chain, when `reuse` is `None` —
    /// mint a fresh proof/`jti` (which safely re-rotates: the predecessor is
    /// `ROTATED_UNCONFIRMED`, so the Engine mints a fresh pair rather than
    /// erroring). The nonce-challenge retry below is `call`'s existing
    /// separate, always-fresh, exactly-once boundary — unrelated to this
    /// reuse decision.
    async fn call_for_renewal(
        &self,
        body: serde_json::Value,
        reuse: Option<PendingRenewalProof>,
    ) -> (Result<HttpResponse, TransportError>, PendingRenewalProof) {
        let device_key = match self.ensure_device_key().await {
            Ok(k) => k,
            Err(_) => {
                // Renewal only ever runs once connected, so the device key
                // is already cached in `Inner`; this path is defensive, not
                // expected in practice. Fail this attempt like a transport
                // error rather than panicking.
                return (
                    Err(TransportError::Network),
                    reuse.unwrap_or(PendingRenewalProof {
                        headers: Vec::new(),
                        iat_unix: 0,
                    }),
                );
            }
        };
        let htu = format!("{}/token", super::http::PRODUCTION_BASE_URL);
        let now_unix = self.clock.now_unix().as_secs();
        let used = match reuse {
            Some(prev) if now_unix.saturating_sub(prev.iat_unix) < 60 => prev,
            _ => self.mint_renewal_headers(&device_key, &htu, None),
        };
        let response = match self
            .transport
            .post_json("/token", used.headers.clone(), body.clone())
            .await
        {
            Ok(r) => r,
            Err(e) => return (Err(e), used),
        };
        if response.status == 401 {
            let use_nonce = response
                .header("www-authenticate")
                .is_some_and(|v| v.contains("use_dpop_nonce"));
            if let (true, Some(nonce)) =
                (use_nonce, response.header("dpop-nonce").map(str::to_string))
            {
                let retried = self.mint_renewal_headers(&device_key, &htu, Some(&nonce));
                let response2 = self
                    .transport
                    .post_json("/token", retried.headers.clone(), body)
                    .await;
                return (response2, retried);
            }
        }
        (Ok(response), used)
    }

    /// One `/token` refresh attempt: builds the grant-B body, sends it via
    /// [`Self::call_for_renewal`], and classifies the result. Shares
    /// [`parse_token_response`] with `try_refresh`/`finish_login_exchange`
    /// rather than duplicating the HTTP/parse logic.
    async fn perform_renewal_attempt(
        &self,
        refresh: &StoredRefresh,
        device_id: &str,
        reuse: Option<PendingRenewalProof>,
    ) -> (RenewalAttemptOutcome, PendingRenewalProof) {
        let body = json!({
            "grant_type": "refresh_token",
            "refresh_token": refresh.refresh_token,
            "device_id": device_id,
        });
        let (result, used) = self.call_for_renewal(body, reuse).await;
        let response = match result {
            Err(_) => return (RenewalAttemptOutcome::Transient { retry_after: None }, used),
            Ok(r) => r,
        };
        if response.status == 200 {
            return match parse_token_response(&response.body) {
                Some(parsed) => (RenewalAttemptOutcome::Success(parsed), used),
                // A malformed 200 body is not a contract error code at all;
                // treat it like any other unclassifiable failure — retry
                // within the existing budget rather than committing garbage
                // or declaring a definitive revoke it never claimed to be.
                None => (RenewalAttemptOutcome::Transient { retry_after: None }, used),
            };
        }
        if response.status == 400
            && response.body.get("error").and_then(|v| v.as_str()) == Some("invalid_grant")
        {
            let reason = response.body.get("reason").and_then(|v| v.as_str());
            if let Some(r) = reason.filter(|r| DEFINITIVE_REFRESH_REASONS.contains(r)) {
                return (
                    RenewalAttemptOutcome::Definitive(map_definitive_reason(r)),
                    used,
                );
            }
        }
        let retry_after = response
            .header("retry-after")
            .and_then(|v| v.trim().parse::<u64>().ok())
            .map(Duration::from_secs);
        (RenewalAttemptOutcome::Transient { retry_after }, used)
    }

    /// One recurring-renewal scheduler tick (td-cab09a): if
    /// `AccountSession::begin_renewal` finds a renewal due against the
    /// injected `Clock`, performs exactly one `/token` refresh attempt and
    /// commits the result strictly through
    /// `session.is_current`/`renewed`/`renewal_failed`, so a response for a
    /// ticket that logout, account-switch or a newer renewal cycle has
    /// already superseded can never write credentials or resurrect a stale
    /// session. Never sleeps and never loops — the caller decides tick
    /// cadence, always by re-deriving due-ness from `now` rather than
    /// trusting elapsed wall time, so this self-heals after sleep/suspend/
    /// offline without any special-case detection.
    pub async fn renewal_tick(&self) {
        self.renewal_tick_impl().await;
        // td-811657: extends this existing tick — not a second timer/
        // service — so an access-expiry-while-renewal-pending transition
        // (or any other tick outcome, including a no-op "nothing due" tick)
        // always reaches a subscriber without requiring a `status()` poll.
        self.publish_snapshot();
    }

    async fn renewal_tick_impl(&self) {
        let now = self.clock.now_instant();
        let ticket = {
            let mut inner = self.inner.lock().unwrap();
            match inner.session.begin_renewal(now) {
                Some(t) => t,
                None => return,
            }
        };
        let (device_id, reuse) = {
            let inner = self.inner.lock().unwrap();
            // No need to special-case "first attempt of a chain" here:
            // `renewal_proof` is always cleared to `None` at every
            // connect/disconnect boundary (fresh login, scheduled renewal
            // success, definitive revoke, `invalidate_local`), so a truly
            // fresh chain always starts from `None` — which
            // `call_for_renewal` already mints fresh for. Every later
            // attempt in the SAME chain reads back whatever the previous
            // attempt actually sent, and the section 5.5 age check decides
            // reuse vs. re-mint from there.
            (inner.device_id.clone(), inner.renewal_proof.clone())
        };
        let Some(device_id) = device_id else {
            self.commit_renewal_failure(
                ticket,
                RenewalFailure::Transient { retry_after: None },
                None,
            )
            .await;
            return;
        };
        let refresh = match self.store.load_profile(self.profile) {
            Ok(Some(data)) => data.refresh_current,
            _ => None,
        };
        let Some(refresh) = refresh else {
            self.commit_renewal_failure(
                ticket,
                RenewalFailure::Transient { retry_after: None },
                None,
            )
            .await;
            return;
        };
        let (outcome, used_proof) = self
            .perform_renewal_attempt(&refresh, &device_id, reuse)
            .await;
        {
            let mut inner = self.inner.lock().unwrap();
            inner.renewal_proof = Some(used_proof);
        }
        match outcome {
            RenewalAttemptOutcome::Success(parsed) => {
                self.commit_renewal_success(ticket, parsed).await
            }
            RenewalAttemptOutcome::Definitive(reason) => {
                self.commit_renewal_failure(
                    ticket,
                    RenewalFailure::ReauthenticationRequired,
                    Some(reason),
                )
                .await
            }
            RenewalAttemptOutcome::Transient { retry_after } => {
                self.commit_renewal_failure(ticket, RenewalFailure::Transient { retry_after }, None)
                    .await
            }
        }
    }

    /// How long the renewal scheduler may sleep before its next
    /// [`Self::renewal_tick`]. Derived from the session's own next deadline
    /// (proactive renew time or backoff deadline), clamped into
    /// `[MIN_RENEWAL_SLEEP, MAX_RENEWAL_SLEEP]`: the lower bound keeps a
    /// loop that cannot make progress from spinning, the upper bound means
    /// a suspend/resume or clock jump is re-checked within a minute without
    /// any special-case detector. Nothing here decides due-ness —
    /// `begin_renewal(now)` inside the tick stays the only authority.
    pub fn next_tick_delay(&self) -> Duration {
        let now = self.clock.now_instant();
        let next = self.inner.lock().unwrap().session.next_renewal_at();
        match next {
            Some(at) => at
                .saturating_duration_since(now)
                .clamp(MIN_RENEWAL_SLEEP, MAX_RENEWAL_SLEEP),
            // Signed out, renewal in flight, or the retry budget is
            // exhausted: nothing is scheduled, so just re-check on the
            // regular cadence.
            None => MAX_RENEWAL_SLEEP,
        }
    }

    /// Commits a successful scheduled renewal under `commit_lock` — the
    /// SAME boundary `finish_login_exchange`, `try_refresh` and
    /// `invalidate_local` use. `session.is_current(ticket)` (rather than
    /// `try_refresh`'s `credential_generation` counter) is this path's
    /// staleness gate, checked BEFORE any store write, so a response for a
    /// ticket that logout/account-switch/a newer renewal attempt already
    /// superseded performs zero writes and zero status changes.
    async fn commit_renewal_success(&self, ticket: RenewalTicket, parsed: ParsedRefresh) {
        let _commit = self.commit_lock.lock().await;
        let (device_id, device_key) = {
            let inner = self.inner.lock().unwrap();
            if !inner.session.is_current(ticket) {
                return;
            }
            let Some(device_id) = inner.device_id.clone() else {
                return;
            };
            let Some(device_key) = inner.device_key.clone() else {
                return;
            };
            (device_id, device_key)
        };
        let scalar_b64 = URL_SAFE_NO_PAD.encode(device_key.to_scalar_bytes());
        let now = self.clock.now_instant();
        if self
            .persist_new_refresh(parsed.refresh.clone(), &device_id, &scalar_b64)
            .is_err()
        {
            // A store failure here must not silently strand the schedule:
            // fold it into the same transient retry budget (the OLD
            // `current`/`pending` slots are untouched — `persist_new_refresh`
            // never overwrites `current` until `pending` is confirmed
            // written) rather than leaving the session waiting forever for
            // a tick that will never come.
            let mut inner = self.inner.lock().unwrap();
            inner.session.renewal_failed(
                ticket,
                now,
                RenewalFailure::Transient { retry_after: None },
            );
            return;
        }
        let mut inner = self.inner.lock().unwrap();
        if inner
            .session
            .renewed(ticket, now, parsed.lifetime, parsed.renew_in)
        {
            inner.access_token = Some((parsed.access_token, now + parsed.lifetime));
            inner.account_display = Some(parsed.refresh.account);
            inner.web_lease_expires_at = Some(parsed.web_lease_expires_at_unix);
            inner.credential_generation = inner.credential_generation.wrapping_add(1);
            inner.renewal_failure_logged = false;
            inner.renewal_proof = None;
        }
    }

    /// Commits a renewal failure under `commit_lock`, gated by the same
    /// `is_current(ticket)` check as [`Self::commit_renewal_success`]. A
    /// transient failure only advances `AccountSession`'s own backoff state
    /// (`renewal_failed` itself no-ops on a stale ticket). A definitive
    /// `invalid_grant` additionally clears the memory-only access token/
    /// display and wipes the durable store — mirroring `invalidate_local`,
    /// but only once the ticket check confirms this response was not
    /// superseded by the user already re-authenticating in the meantime.
    async fn commit_renewal_failure(
        &self,
        ticket: RenewalTicket,
        failure: RenewalFailure,
        end_reason: Option<AccountEndReason>,
    ) {
        let _commit = self.commit_lock.lock().await;
        let now = self.clock.now_instant();
        let mut inner = self.inner.lock().unwrap();
        if !inner.session.is_current(ticket) {
            return;
        }
        if !inner.session.renewal_failed(ticket, now, failure) {
            return;
        }
        if matches!(failure, RenewalFailure::ReauthenticationRequired) {
            inner.account_display = None;
            inner.access_token = None;
            inner.web_lease_expires_at = None;
            inner.phase = Phase::Idle;
            inner.renewal_proof = None;
            inner.credential_generation = inner.credential_generation.wrapping_add(1);
            // td-811657: a definitive revoke/`invalid_grant` outcome is a
            // reauthentication-required boundary, distinct from an ordinary
            // renewal commit — advance the lifecycle epoch and record the
            // safe end reason carried from the single classification match
            // in `perform_renewal_attempt`.
            inner.lifecycle_epoch = inner.lifecycle_epoch.wrapping_add(1);
            inner.last_end_reason = Some(end_reason.unwrap_or(AccountEndReason::SignedOut));
            drop(inner);
            let _ = self.store.clear_profile(self.profile);
        }
    }

    // ── Revoke / invalidate primitives (not exposed as commands here) ──────

    /// Best-effort remote revoke. A reusable primitive for later tasks
    /// (td-96a620's debug reset, td-cab09a's revoked-credential handling) —
    /// not wired to any IPC command in this task.
    #[allow(dead_code)]
    pub async fn revoke_device(&self, reason: &str) -> Result<(), AccountError> {
        let result = self.revoke_device_impl(reason).await;
        // td-811657: `revoke_device` itself performs no local commit today
        // (it only calls the Engine), but is one of this task's named
        // publish points — re-sending the unchanged current snapshot here
        // is harmless (watch coalesces) and keeps this call-site future-
        // proof against a later task adding a local commit here.
        self.publish_snapshot();
        result
    }

    async fn revoke_device_impl(&self, reason: &str) -> Result<(), AccountError> {
        let (device_id, access_token) = {
            let inner = self.inner.lock().unwrap();
            (
                inner.device_id.clone(),
                inner.access_token.clone().map(|(t, _)| t),
            )
        };
        let Some(device_id) = device_id else {
            return Err(AccountError::NotReady);
        };
        let refresh_token = self
            .store
            .load_profile(self.profile)
            .ok()
            .flatten()
            .and_then(|d| d.refresh_current.map(|r| r.refresh_token));
        let body = json!({
            "device_id": device_id,
            "refresh_token": refresh_token,
            "scope": "device",
            "reason": reason,
        });
        let response = self
            .call("POST", "/revoke", body, access_token.as_deref())
            .await?;
        if response.status == 200 {
            Ok(())
        } else {
            Err(error_from_body(response))
        }
    }

    /// Clears all local state for this profile: cancels any pending login,
    /// disconnects the session (bumping its generation so a stale renewal
    /// response can never commit), drops the memory-only access token, and
    /// wipes the durable profile. Does not touch `install_id` or the other
    /// `ClientProfile` namespace. A reusable primitive for td-96a620.
    /// Advances `credential_generation` (alongside `login_flow`'s own
    /// generation and `session.disconnect`'s) BEFORE wiping the store, all
    /// under `commit_lock` — the SAME boundary `try_refresh` and
    /// `finish_login_exchange` use. Holding the lock across the whole
    /// function (both the generation advance and the store wipe) means no
    /// bootstrap refresh or login exchange in flight can land its commit
    /// between the two: it either already rechecked and lost before this
    /// runs, or it is still waiting for `commit_lock` and will recheck
    /// against the now-advanced generation once it gets it.
    #[allow(dead_code)]
    pub async fn invalidate_local(&self) {
        let _commit = self.commit_lock.lock().await;
        {
            let mut inner = self.inner.lock().unwrap();
            inner.login_flow.cancel();
            inner.session.disconnect();
            inner.account_display = None;
            inner.access_token = None;
            inner.web_lease_expires_at = None;
            inner.renewal_proof = None;
            inner.renewal_failure_logged = false;
            inner.phase = Phase::Idle;
            inner.credential_generation = inner.credential_generation.wrapping_add(1);
            // td-811657: logout/reset is a definitive lifecycle boundary —
            // advance the epoch and record the safe end reason.
            inner.lifecycle_epoch = inner.lifecycle_epoch.wrapping_add(1);
            inner.last_end_reason = Some(AccountEndReason::SignedOut);
        }
        let _ = self.store.clear_profile(self.profile);
        self.publish_snapshot();
    }

    // ── DPoP-signed call with one nonce-retry boundary ──────────────────────

    async fn call(
        &self,
        htm: &str,
        path: &str,
        body: serde_json::Value,
        access_token: Option<&str>,
    ) -> Result<HttpResponse, AccountError> {
        let device_key = self.ensure_device_key().await?;
        let htu = format!("{}{}", super::http::PRODUCTION_BASE_URL, path);
        let response = self
            .transport
            .post_json(
                path,
                self.build_headers(&device_key, htm, &htu, access_token, None),
                body.clone(),
            )
            .await?;
        if response.status == 401 {
            let use_nonce = response
                .header("www-authenticate")
                .is_some_and(|v| v.contains("use_dpop_nonce"));
            if let (true, Some(nonce)) =
                (use_nonce, response.header("dpop-nonce").map(str::to_string))
            {
                // Exactly one retry, with a freshly built proof (new jti/iat).
                return Ok(self
                    .transport
                    .post_json(
                        path,
                        self.build_headers(&device_key, htm, &htu, access_token, Some(&nonce)),
                        body,
                    )
                    .await?);
            }
        }
        Ok(response)
    }

    fn build_headers(
        &self,
        device_key: &DeviceKey,
        htm: &str,
        htu: &str,
        access_token: Option<&str>,
        nonce: Option<&str>,
    ) -> Vec<(String, String)> {
        let mut jti_bytes = [0u8; 16];
        self.rng.fill(&mut jti_bytes);
        let jti = bridge_core::dpop::jti_from_bytes(jti_bytes);
        let iat = self.clock.now_unix().as_secs();
        let ath = access_token.map(bridge_core::dpop::ath_claim);
        let proof =
            bridge_core::dpop::build_proof(device_key, htm, htu, iat, &jti, nonce, ath.as_deref())
                .expect("htu is built from a constant base + path, never a query/fragment");
        // Explicitly set here (not only relying on the shared client's
        // defaults) so both the real transport and a scripted test double
        // carry the mandatory native headers (contract v5 section 4).
        let mut headers = vec![
            ("DPoP".to_string(), proof),
            (
                crate::engine::VERSION_HEADER.to_string(),
                crate::engine::BRIDGE_VERSION.to_string(),
            ),
            (
                "User-Agent".to_string(),
                crate::engine::user_agent(crate::engine::BRIDGE_VERSION),
            ),
        ];
        if let Some(token) = access_token {
            headers.push(("Authorization".to_string(), format!("DPoP {token}")));
        }
        headers
    }
}

fn error_from_body(response: HttpResponse) -> AccountError {
    let error = response
        .body
        .get("error")
        .and_then(|v| v.as_str())
        .unwrap_or("unknown_error")
        .to_string();
    AccountError::Http {
        status: response.status,
        error,
    }
}

fn device_name() -> String {
    std::env::var("COMPUTERNAME")
        .or_else(|_| std::env::var("HOSTNAME"))
        .unwrap_or_else(|_| "TFD-Bridge-Device".to_string())
}

fn platform_name() -> &'static str {
    match std::env::consts::OS {
        "windows" => "windows",
        "macos" => "macos",
        "linux" => "linux",
        other => other,
    }
}

/// Adapter so `Arc<AccountService>` can be handed to the bridge server as an
/// `AuthCallbackSink` trait object without `AccountService` itself needing
/// an `Arc`-receiver method (the trait requires `&self`).
pub struct AccountCallbackSink(pub Arc<AccountService>);

impl AuthCallbackSink for AccountCallbackSink {
    fn handle_callback(&self, callback: AuthCallback) -> AuthCallbackAck {
        AccountService::on_callback(&self.0, callback)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::account::traits::{
        BrowserLauncher, Clock, HttpResponse, SecretStore, SecureRandom, StoreError,
        StoredProfileData, TransportError,
    };
    use async_trait::async_trait;
    use std::collections::{HashMap, VecDeque};
    use std::sync::Mutex as StdMutex;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

    // ── Test doubles ─────────────────────────────────────────────────────────

    struct FixedClock {
        unix: StdMutex<Duration>,
        instant: StdMutex<Instant>,
    }
    #[allow(dead_code)]
    impl FixedClock {
        fn new() -> Arc<Self> {
            Arc::new(Self {
                unix: StdMutex::new(Duration::from_secs(1_700_000_000)),
                instant: StdMutex::new(Instant::now()),
            })
        }
        fn advance(&self, secs: u64) {
            *self.unix.lock().unwrap() += Duration::from_secs(secs);
            *self.instant.lock().unwrap() += Duration::from_secs(secs);
        }
    }
    impl Clock for FixedClock {
        fn now_unix(&self) -> Duration {
            *self.unix.lock().unwrap()
        }
        fn now_instant(&self) -> Instant {
            *self.instant.lock().unwrap()
        }
    }

    /// Deterministic but distinct bytes per call: fills every buffer with an
    /// incrementing counter byte. Good enough to make PKCE/state/jti values
    /// differ across calls without needing a real CSPRNG in tests.
    struct CountingRandom(AtomicUsize);
    impl CountingRandom {
        fn new() -> Arc<Self> {
            Arc::new(Self(AtomicUsize::new(1)))
        }
    }
    impl SecureRandom for CountingRandom {
        fn fill(&self, buf: &mut [u8]) {
            let n = self.0.fetch_add(1, Ordering::SeqCst) as u8;
            buf.fill(n);
        }
    }

    #[derive(Default)]
    struct InMemoryStore {
        install_id: StdMutex<Option<String>>,
        profiles: StdMutex<HashMap<&'static str, StoredProfileData>>,
        save_calls: AtomicUsize,
        fail_save_on_call: StdMutex<Option<usize>>,
        fail_load: AtomicBool,
        unsupported: AtomicBool,
    }
    impl SecretStore for InMemoryStore {
        fn install_id(&self) -> Result<String, StoreError> {
            if self.unsupported.load(Ordering::SeqCst) {
                return Err(StoreError::UnsupportedPlatform);
            }
            let mut guard = self.install_id.lock().unwrap();
            if let Some(id) = guard.clone() {
                return Ok(id);
            }
            let id = "test-install-id".to_string();
            *guard = Some(id.clone());
            Ok(id)
        }
        fn load_profile(
            &self,
            profile: ClientProfile,
        ) -> Result<Option<StoredProfileData>, StoreError> {
            if self.unsupported.load(Ordering::SeqCst) {
                return Err(StoreError::UnsupportedPlatform);
            }
            if self.fail_load.load(Ordering::SeqCst) {
                return Err(StoreError::Backend("forced load failure".into()));
            }
            Ok(self.profiles.lock().unwrap().get(profile.as_str()).cloned())
        }
        fn save_profile(
            &self,
            profile: ClientProfile,
            data: &StoredProfileData,
        ) -> Result<(), StoreError> {
            if self.unsupported.load(Ordering::SeqCst) {
                return Err(StoreError::UnsupportedPlatform);
            }
            let call_number = self.save_calls.fetch_add(1, Ordering::SeqCst) + 1;
            if *self.fail_save_on_call.lock().unwrap() == Some(call_number) {
                return Err(StoreError::Backend("forced save failure".into()));
            }
            self.profiles
                .lock()
                .unwrap()
                .insert(profile.as_str(), data.clone());
            Ok(())
        }
        fn clear_profile(&self, profile: ClientProfile) -> Result<(), StoreError> {
            self.profiles.lock().unwrap().remove(profile.as_str());
            Ok(())
        }
    }

    #[derive(Default)]
    struct RecordingBrowser {
        opened: StdMutex<Vec<String>>,
        succeed: AtomicBool,
    }
    impl RecordingBrowser {
        fn new(succeed: bool) -> Arc<Self> {
            Arc::new(Self {
                opened: StdMutex::new(Vec::new()),
                succeed: AtomicBool::new(succeed),
            })
        }
    }
    impl BrowserLauncher for RecordingBrowser {
        fn open(&self, url: &str) -> bool {
            self.opened.lock().unwrap().push(url.to_string());
            self.succeed.load(Ordering::SeqCst)
        }
    }

    #[derive(Clone)]
    struct RecordedCall {
        path: String,
        headers: Vec<(String, String)>,
        body: serde_json::Value,
    }

    /// FIFO-scripted transport: each `post_json` call pops the next queued
    /// response (or returns `TransportError::Network` if the queue is
    /// empty). Every call is recorded for header/body assertions.
    #[derive(Default)]
    struct ScriptedTransport {
        responses: StdMutex<VecDeque<Result<HttpResponse, TransportError>>>,
        calls: StdMutex<Vec<RecordedCall>>,
    }
    impl ScriptedTransport {
        fn new() -> Arc<Self> {
            Arc::new(Self::default())
        }
        fn push_ok(&self, status: u16, body: serde_json::Value) {
            self.responses.lock().unwrap().push_back(Ok(HttpResponse {
                status,
                headers: Vec::new(),
                body,
            }));
        }
        fn push_ok_with_headers(
            &self,
            status: u16,
            headers: Vec<(String, String)>,
            body: serde_json::Value,
        ) {
            self.responses.lock().unwrap().push_back(Ok(HttpResponse {
                status,
                headers,
                body,
            }));
        }
        fn calls(&self) -> Vec<RecordedCall> {
            self.calls.lock().unwrap().clone()
        }
    }
    #[async_trait]
    impl AccountTransport for ScriptedTransport {
        async fn post_json(
            &self,
            path: &str,
            headers: Vec<(String, String)>,
            body: serde_json::Value,
        ) -> Result<HttpResponse, TransportError> {
            self.calls.lock().unwrap().push(RecordedCall {
                path: path.to_string(),
                headers,
                body,
            });
            self.responses
                .lock()
                .unwrap()
                .pop_front()
                .unwrap_or(Err(TransportError::Network))
        }
    }

    struct Fixture {
        service: Arc<AccountService>,
        transport: Arc<ScriptedTransport>,
        store: Arc<InMemoryStore>,
        browser: Arc<RecordingBrowser>,
        clock: Arc<FixedClock>,
    }

    fn make_fixture(browser_succeeds: bool) -> Fixture {
        let transport = ScriptedTransport::new();
        let store = Arc::new(InMemoryStore::default());
        let browser = RecordingBrowser::new(browser_succeeds);
        let clock = FixedClock::new();
        let rng = CountingRandom::new();
        let service = Arc::new(AccountService::new(
            transport.clone(),
            store.clone(),
            browser.clone(),
            clock.clone(),
            rng,
            ClientProfile::Release,
        ));
        service.set_port(43210);
        Fixture {
            service,
            transport,
            store,
            browser,
            clock,
        }
    }

    fn device_response() -> serde_json::Value {
        json!({"device_id": "dev_test_001", "registered_at": "2026-01-01T00:00:00Z", "client_profile": "release"})
    }

    fn authorize_response() -> serde_json::Value {
        json!({
            "authorization_url": "https://engine.tfd.rocks/auth/bridge?request_id=areq_1",
            "request_id": "areq_1",
            "expires_in": 300,
        })
    }

    fn token_response(refresh_token: &str) -> serde_json::Value {
        json!({
            "access_token": "eyTestAccessToken",
            "token_type": "DPoP",
            "expires_in": 900,
            "access_token_expires_at": "2030-01-01T12:15:00Z",
            "web_lease_expires_at": "2030-01-01T12:15:00Z",
            "refresh_token": refresh_token,
            // `refresh_expires_in` is a real contract field alongside the
            // absolute timestamps, deliberately kept in this fixture to
            // prove the parser ignores it rather than trusting it.
            "refresh_expires_in": 7776000,
            "refresh_expires_at": "2030-03-31T12:00:00Z",
            "refresh_absolute_expires_at": "2031-01-01T12:00:00Z",
            "scope": "bridge.submit bridge.share bridge.profile",
            "account": {"user_id":1,"account_id":591735977,"realm":"eu","nickname":"FrankDrake","clan_tag":"TFX","role":"member","linked_account_verified":true},
        })
    }

    // ── begin_login / open-failure / reopen ─────────────────────────────────

    #[tokio::test]
    async fn begin_login_registers_device_authorizes_and_opens_browser() {
        let f = make_fixture(true);
        f.transport.push_ok(201, device_response());
        f.transport.push_ok(200, authorize_response());

        f.service.begin_login().await.unwrap();

        assert_eq!(f.service.status(), AccountStatus::AwaitingBrowser);
        let opened = f.browser.opened.lock().unwrap();
        assert_eq!(opened.len(), 1);
        assert_eq!(
            opened[0],
            "https://engine.tfd.rocks/auth/bridge?request_id=areq_1"
        );

        let calls = f.transport.calls();
        assert_eq!(calls[0].path, "/device");
        assert_eq!(calls[1].path, "/authorize");
        // Every native call carries the mandatory version headers (contract
        // v5 section 4) and a DPoP proof; neither call carries a bearer
        // token (no access token exists yet).
        for call in &calls {
            assert!(call.headers.iter().any(|(k, _)| k == "DPoP"));
            assert!(
                call.headers
                    .iter()
                    .any(|(k, v)| k == crate::engine::VERSION_HEADER
                        && v == crate::engine::BRIDGE_VERSION)
            );
            assert!(call.headers.iter().any(|(k, _)| k == "User-Agent"));
            assert!(!call.headers.iter().any(|(k, _)| k == "Authorization"));
        }
        assert_eq!(
            calls[1].body["redirect_uri"],
            "http://127.0.0.1:43210/auth/callback"
        );
        assert_eq!(calls[1].body["code_challenge_method"], "S256");
    }

    #[tokio::test]
    async fn begin_login_requires_a_known_bridge_port() {
        let f = make_fixture(true);
        // Fresh service without set_port — simulate calling before the
        // listener is up.
        let transport = ScriptedTransport::new();
        let store = Arc::new(InMemoryStore::default());
        let browser = RecordingBrowser::new(true);
        let service = Arc::new(AccountService::new(
            transport,
            store,
            browser,
            FixedClock::new(),
            CountingRandom::new(),
            ClientProfile::Release,
        ));
        assert_eq!(service.begin_login().await, Err(AccountError::NotReady));
        let _ = f;
    }

    #[tokio::test]
    async fn begin_login_rejects_a_malicious_or_out_of_contract_authorization_url() {
        let malicious_urls = [
            "http://engine.tfd.rocks/auth/bridge?request_id=1", // wrong scheme
            "https://evil.example/auth/bridge?request_id=1",    // wrong host entirely
            "https://engine.tfd.rocks.evil.example/auth/bridge", // host-confusable suffix
            "https://evil.example/?engine.tfd.rocks/auth/bridge", // host-confusable prefix trick
            "https://engine.tfd.rocks:8443/auth/bridge?request_id=1", // non-default port
            "https://engine.tfd.rocks/auth/other?request_id=1", // wrong path
            "https://engine.tfd.rocks/auth/bridge/",            // trailing-slash path variant
            "https://user:pass@engine.tfd.rocks/auth/bridge",   // embedded credentials
            "https://engine.tfd.rocks/auth/bridge#fragment",    // fragment
            "not a url at all",                                 // unparseable
        ];
        for url in malicious_urls {
            let f = make_fixture(true);
            f.transport.push_ok(201, device_response());
            f.transport.push_ok(
                200,
                json!({"authorization_url": url, "request_id": "areq_1", "expires_in": 300}),
            );
            let result = f.service.begin_login().await;
            assert_eq!(result, Err(AccountError::InvalidResponse), "url: {url}");
            // Never opened, and never stored as a pending attempt to reopen.
            assert!(f.browser.opened.lock().unwrap().is_empty(), "url: {url}");
            assert_eq!(
                f.service.reopen_pending(),
                Err(AccountError::NoPendingLogin),
                "url: {url}"
            );
        }
    }

    #[tokio::test]
    async fn browser_open_failure_is_visible_and_reopen_retries_the_same_url_without_a_second_authorize_call()
     {
        let f = make_fixture(false);
        f.transport.push_ok(201, device_response());
        f.transport.push_ok(200, authorize_response());

        f.service.begin_login().await.unwrap();
        assert_eq!(f.service.status(), AccountStatus::OpenFailed);

        // Reopen with the browser now succeeding.
        f.browser.succeed.store(true, Ordering::SeqCst);
        f.service.reopen_pending().unwrap();
        assert_eq!(f.service.status(), AccountStatus::AwaitingBrowser);

        let opened = f.browser.opened.lock().unwrap();
        assert_eq!(opened.len(), 2);
        assert_eq!(
            opened[0], opened[1],
            "reopen must use the exact same stored URL"
        );
        // No second /authorize call was made for the reopen.
        assert_eq!(f.transport.calls().len(), 2);
    }

    #[tokio::test]
    async fn reopen_without_any_pending_login_fails() {
        let f = make_fixture(true);
        assert_eq!(
            f.service.reopen_pending(),
            Err(AccountError::NoPendingLogin)
        );
    }

    #[tokio::test]
    async fn status_expires_a_pending_attempt_with_no_callback_once_its_own_ttl_passes() {
        let f = make_fixture(true);
        f.transport.push_ok(201, device_response());
        f.transport.push_ok(200, authorize_response());
        f.service.begin_login().await.unwrap();
        assert_eq!(f.service.status(), AccountStatus::AwaitingBrowser);

        // No callback ever arrives (browser closed) and nobody calls
        // reopen — `status()` is the only thing still being polled, and it
        // alone must still notice the TTL passing (contract v5 section 4.3:
        // "the Bridge times out after `expires_in`").
        f.clock.advance(301); // authorize_response()'s expires_in is 300
        assert_eq!(f.service.status(), AccountStatus::Expired);
        // Idempotent: polling again does not panic or flip back.
        assert_eq!(f.service.status(), AccountStatus::Expired);
    }

    #[tokio::test]
    async fn reopen_after_expiry_reports_expired_and_never_reopens_the_browser() {
        let f = make_fixture(true);
        f.transport.push_ok(201, device_response());
        f.transport.push_ok(200, authorize_response());
        f.service.begin_login().await.unwrap();
        let opened_before = f.browser.opened.lock().unwrap().len();

        f.clock.advance(301);
        assert_eq!(
            f.service.reopen_pending(),
            Err(AccountError::NoPendingLogin)
        );
        assert_eq!(f.service.status(), AccountStatus::Expired);
        assert_eq!(
            f.browser.opened.lock().unwrap().len(),
            opened_before,
            "an expired attempt must never be reopened in the browser"
        );
    }

    #[tokio::test]
    async fn cancel_login_invalidates_the_pending_attempt_for_reopen_and_callback() {
        let f = make_fixture(true);
        f.transport.push_ok(201, device_response());
        f.transport.push_ok(200, authorize_response());
        f.service.begin_login().await.unwrap();

        f.service.cancel_login();
        assert_eq!(f.service.status(), AccountStatus::Canceled);
        assert_eq!(
            f.service.reopen_pending(),
            Err(AccountError::NoPendingLogin)
        );

        let state = f.transport.calls()[1].body["state"]
            .as_str()
            .unwrap()
            .to_string();
        let ack = AccountService::on_callback(
            &f.service,
            AuthCallback {
                state,
                outcome: AuthCallbackOutcome::Code("late-code".into()),
            },
        );
        assert_eq!(ack, AuthCallbackAck::Rejected);
    }

    // ── Callback handling ────────────────────────────────────────────────────

    async fn begin_and_get_state(f: &Fixture) -> String {
        f.transport.push_ok(201, device_response());
        f.transport.push_ok(200, authorize_response());
        f.service.begin_login().await.unwrap();
        f.transport.calls()[1].body["state"]
            .as_str()
            .unwrap()
            .to_string()
    }

    #[tokio::test]
    async fn on_callback_accepts_matching_code_and_marks_exchanging() {
        let f = make_fixture(true);
        let state = begin_and_get_state(&f).await;
        // No /token response queued: the spawned exchange will hit a
        // transport error and return WITHOUT touching `phase`, so this
        // assertion is race-free regardless of scheduling.
        let ack = AccountService::on_callback(
            &f.service,
            AuthCallback {
                state,
                outcome: AuthCallbackOutcome::Code("test-code".into()),
            },
        );
        // The ack must never claim a connection: the exchange has not even
        // started yet, let alone succeeded and been durably persisted.
        assert_eq!(ack, AuthCallbackAck::Pending);
        assert_eq!(f.service.status(), AccountStatus::Exchanging);
    }

    #[tokio::test]
    async fn duplicate_or_concurrent_callback_exchanges_at_most_once() {
        let f = make_fixture(true);
        let state = begin_and_get_state(&f).await;
        let callback = || AuthCallback {
            state: state.clone(),
            outcome: AuthCallbackOutcome::Code("test-code".into()),
        };
        assert_eq!(
            AccountService::on_callback(&f.service, callback()),
            AuthCallbackAck::Pending
        );
        // A second, "concurrent" callback for the same already-consumed
        // attempt can only ever be Rejected — the exchange it could trigger
        // therefore happens at most once.
        assert_eq!(
            AccountService::on_callback(&f.service, callback()),
            AuthCallbackAck::Rejected
        );
    }

    #[tokio::test]
    async fn unknown_state_is_rejected_and_does_not_disturb_the_real_pending_attempt() {
        let f = make_fixture(true);
        let state = begin_and_get_state(&f).await;
        let ack = AccountService::on_callback(
            &f.service,
            AuthCallback {
                state: "totally-unrelated-guess".into(),
                outcome: AuthCallbackOutcome::Code("x".into()),
            },
        );
        assert_eq!(ack, AuthCallbackAck::Rejected);
        assert_eq!(f.service.status(), AccountStatus::AwaitingBrowser);
        // The real attempt is still reopenable.
        assert!(f.service.reopen_pending().is_ok());
        // ... and still consumable with its real state.
        let ack2 = AccountService::on_callback(
            &f.service,
            AuthCallback {
                state,
                outcome: AuthCallbackOutcome::Code("real-code".into()),
            },
        );
        assert_eq!(ack2, AuthCallbackAck::Pending);
    }

    #[tokio::test]
    async fn distinct_terminal_outcomes_map_to_distinct_statuses_and_acks() {
        for (error, expected_status, expected_ack) in [
            (
                AuthCallbackOutcome::AccessDenied,
                AccountStatus::Denied,
                AuthCallbackAck::Denied,
            ),
            (
                AuthCallbackOutcome::AccountNotAuthorized,
                AccountStatus::AccountNotAuthorized,
                AuthCallbackAck::AccountNotAuthorized,
            ),
            (
                AuthCallbackOutcome::ExpiredRequest,
                AccountStatus::Expired,
                AuthCallbackAck::Expired,
            ),
        ] {
            let f = make_fixture(true);
            let state = begin_and_get_state(&f).await;
            let ack = AccountService::on_callback(
                &f.service,
                AuthCallback {
                    state,
                    outcome: error,
                },
            );
            assert_eq!(ack, expected_ack);
            assert_eq!(f.service.status(), expected_status);
        }
    }

    #[tokio::test]
    async fn a_late_callback_past_its_own_deadline_is_reported_as_expired_not_a_generic_rejection()
    {
        let f = make_fixture(true);
        let state = begin_and_get_state(&f).await;
        f.clock.advance(301); // past the 300s pending-attempt TTL
        let ack = AccountService::on_callback(
            &f.service,
            AuthCallback {
                state,
                outcome: AuthCallbackOutcome::Code("late-code".into()),
            },
        );
        assert_eq!(ack, AuthCallbackAck::Expired);
        assert_eq!(f.service.status(), AccountStatus::Expired);
    }

    // ── Full exchange + two-slot persistence (direct, race-free) ────────────

    /// Drives the exchange directly (bypassing the fire-and-forget spawn in
    /// `on_callback`) so persistence/generation assertions are deterministic.
    async fn consume_and_exchange(f: &Fixture, state: String, code: &str) {
        let now = f.clock.now_instant();
        let consumed = {
            let mut inner = f.service.inner.lock().unwrap();
            inner.login_flow.consume_code(&state, now).unwrap()
        };
        Arc::clone(&f.service)
            .complete_exchange(code.to_string(), consumed)
            .await;
    }

    #[tokio::test]
    async fn full_round_trip_connects_and_persists_two_slot_refresh() {
        let f = make_fixture(true);
        let state = begin_and_get_state(&f).await;
        f.transport.push_ok(200, token_response("refresh-abc"));

        consume_and_exchange(&f, state, "auth-code").await;

        match f.service.status() {
            AccountStatus::Connected {
                nickname,
                clan_tag,
                realm,
                web_lease_expires_at,
            } => {
                assert_eq!(nickname, "FrankDrake");
                assert_eq!(clan_tag.as_deref(), Some("TFX"));
                assert_eq!(realm.as_deref(), Some("eu"));
                // The fixture's `web_lease_expires_at: "2030-01-01T12:15:00Z"`.
                assert_eq!(web_lease_expires_at, Some(1_893_500_100));
            }
            other => panic!("expected Connected, got {other:?}"),
        }

        let stored = f
            .store
            .profiles
            .lock()
            .unwrap()
            .get("release")
            .cloned()
            .unwrap();
        assert_eq!(
            stored.refresh_current.as_ref().unwrap().refresh_token,
            "refresh-abc"
        );
        assert!(
            stored.refresh_pending.is_none(),
            "must be promoted, not left pending"
        );
        assert_eq!(stored.device_id.as_deref(), Some("dev_test_001"));
        // Both v5 expiry fields are parsed as absolute UTC timestamps and
        // persisted verbatim: 2030-03-31T12:00:00Z / 2031-01-01T12:00:00Z,
        // never `now + a locally chosen duration`.
        let current = stored.refresh_current.as_ref().unwrap();
        assert_eq!(current.refresh_expires_at_unix, 1_901_188_800);
        assert_eq!(current.refresh_absolute_expires_at_unix, 1_925_035_200);

        // Access token is never written to the store.
        let stored_json = serde_json::to_string(&stored).unwrap();
        assert!(!stored_json.contains("eyTestAccessToken"));
    }

    #[tokio::test]
    async fn exchange_rejects_missing_or_malformed_v5_expiry_fields_without_persisting_anything() {
        let malformed_bodies: Vec<(&str, serde_json::Value)> = vec![
            ("missing refresh_expires_at", {
                let mut b = token_response("x");
                b.as_object_mut().unwrap().remove("refresh_expires_at");
                b
            }),
            ("missing refresh_absolute_expires_at", {
                let mut b = token_response("x");
                b.as_object_mut()
                    .unwrap()
                    .remove("refresh_absolute_expires_at");
                b
            }),
            ("malformed refresh_expires_at", {
                let mut b = token_response("x");
                b["refresh_expires_at"] = json!("not-a-timestamp");
                b
            }),
            ("malformed refresh_absolute_expires_at", {
                let mut b = token_response("x");
                b["refresh_absolute_expires_at"] = json!("not-a-timestamp");
                b
            }),
            ("missing web_lease_expires_at", {
                let mut b = token_response("x");
                b.as_object_mut().unwrap().remove("web_lease_expires_at");
                b
            }),
            ("malformed web_lease_expires_at", {
                let mut b = token_response("x");
                b["web_lease_expires_at"] = json!("not-a-timestamp");
                b
            }),
        ];
        for (label, body) in malformed_bodies {
            let f = make_fixture(true);
            let state = begin_and_get_state(&f).await;
            f.transport.push_ok(200, body);

            consume_and_exchange(&f, state, "auth-code").await;

            assert!(
                !matches!(f.service.status(), AccountStatus::Connected { .. }),
                "{label}: must never report Connected"
            );
            assert!(
                f.store
                    .profiles
                    .lock()
                    .unwrap()
                    .get("release")
                    .and_then(|d| d.refresh_current.clone())
                    .is_none(),
                "{label}: must never persist a credential"
            );
        }
    }

    #[tokio::test]
    async fn refresh_persists_the_engine_provided_absolute_cap_verbatim_never_recomputed_from_now()
    {
        let f = make_fixture(true);
        f.store.profiles.lock().unwrap().insert(
            "release",
            StoredProfileData {
                schema_version: STORED_PROFILE_SCHEMA_VERSION,
                device_key_scalar_b64: URL_SAFE_NO_PAD.encode([9u8; 32]),
                device_id: Some("dev_restart".into()),
                refresh_current: Some(StoredRefresh {
                    refresh_token: "refresh-current".into(),
                    refresh_expires_at_unix: f.clock.now_unix().as_secs() + 1000,
                    // Deliberately much smaller than the fixture's server
                    // response so a "now + fallback duration" bug would be
                    // trivially distinguishable from the correct, verbatim
                    // server value asserted below.
                    refresh_absolute_expires_at_unix: f.clock.now_unix().as_secs() + 100_000,
                    account: StoredAccount {
                        nickname: "Old".into(),
                        clan_tag: None,
                        realm: None,
                    },
                }),
                refresh_pending: None,
            },
        );
        f.transport.push_ok(200, token_response("refresh-rotated"));
        f.service.bootstrap().await;
        assert!(matches!(
            f.service.status(),
            AccountStatus::Connected { .. }
        ));

        let stored = f
            .store
            .profiles
            .lock()
            .unwrap()
            .get("release")
            .cloned()
            .unwrap();
        let current = stored.refresh_current.unwrap();
        assert_eq!(current.refresh_token, "refresh-rotated");
        // Exactly the `token_response` fixture's contract fields — never a
        // value derived from the clock, and never the stale value it
        // replaced.
        assert_eq!(current.refresh_expires_at_unix, 1_901_188_800);
        assert_eq!(current.refresh_absolute_expires_at_unix, 1_925_035_200);
    }

    #[tokio::test]
    async fn cancel_before_exchange_completes_prevents_a_stale_response_from_committing() {
        let f = make_fixture(true);
        let state = begin_and_get_state(&f).await;
        let now = f.clock.now_instant();
        let consumed = {
            let mut inner = f.service.inner.lock().unwrap();
            inner.login_flow.consume_code(&state, now).unwrap()
        };
        // Cancel (e.g. the user backed out, or logged out) while the network
        // call is conceptually "in flight" — bumps the login_flow generation.
        f.service.cancel_login();
        f.transport
            .push_ok(200, token_response("refresh-should-not-land"));

        Arc::clone(&f.service)
            .complete_exchange("auth-code".into(), consumed)
            .await;

        assert_eq!(f.service.status(), AccountStatus::Canceled);
        // A device-key/device_id record from `begin_login` may already be
        // stored, but the stale exchange must never have written a refresh.
        assert!(
            f.store
                .profiles
                .lock()
                .unwrap()
                .get("release")
                .and_then(|d| d.refresh_current.clone())
                .is_none(),
            "a stale response must never persist credentials"
        );
    }

    #[tokio::test]
    async fn initial_exchange_store_write_failure_stays_disconnected() {
        let f = make_fixture(true);
        let state = begin_and_get_state(&f).await;
        f.transport.push_ok(200, token_response("refresh-xyz"));
        // The 3rd save_profile call in this flow is the one persisting the
        // refresh's `pending` slot (1st = device-key persist during
        // ensure_device_key, 2nd = device_id persist during
        // ensure_registered, 3rd = refresh `pending` write).
        *f.store.fail_save_on_call.lock().unwrap() = Some(3);

        consume_and_exchange(&f, state, "auth-code").await;

        assert_eq!(f.service.status(), AccountStatus::StoreUnavailable);
        assert!(
            f.store
                .profiles
                .lock()
                .unwrap()
                .get("release")
                .and_then(|d| d.refresh_current.clone())
                .is_none(),
            "no credential must be committed on a store failure"
        );
    }

    #[tokio::test]
    async fn secret_fields_never_appear_in_a_debug_or_serialized_status() {
        let f = make_fixture(true);
        let state = begin_and_get_state(&f).await;
        f.transport
            .push_ok(200, token_response("super-secret-refresh-canary"));
        consume_and_exchange(&f, state, "auth-code-canary").await;

        let status = f.service.status();
        let debugged = format!("{status:?}");
        let serialized = serde_json::to_string(&status).unwrap();
        for canary in ["super-secret-refresh-canary", "auth-code-canary"] {
            assert!(!debugged.contains(canary));
            assert!(!serialized.contains(canary));
        }
    }

    // ── Two-slot crash boundary + restart recovery ──────────────────────────

    #[tokio::test]
    async fn crash_between_pending_write_and_promotion_recovers_via_pending_on_restart() {
        let f = make_fixture(true);
        let state = begin_and_get_state(&f).await;
        f.transport
            .push_ok(200, token_response("refresh-crash-recovery"));
        // Fail exactly the promotion write (the 4th save_profile call: 1 key
        // persist + 1 device_id persist + 1 pending write + [fails] promote).
        *f.store.fail_save_on_call.lock().unwrap() = Some(4);

        consume_and_exchange(&f, state, "auth-code").await;
        assert_eq!(f.service.status(), AccountStatus::StoreUnavailable);

        let stored = f
            .store
            .profiles
            .lock()
            .unwrap()
            .get("release")
            .cloned()
            .unwrap();
        assert!(
            stored.refresh_current.is_none(),
            "promotion must not have happened"
        );
        assert_eq!(
            stored.refresh_pending.as_ref().unwrap().refresh_token,
            "refresh-crash-recovery",
            "the pending write itself must have survived the later promotion failure"
        );

        // "Restart": a brand new AccountService sharing the same store.
        let transport2 = ScriptedTransport::new();
        transport2.push_ok(200, token_response("refresh-after-restart"));
        let service2 = Arc::new(AccountService::new(
            transport2.clone(),
            f.store.clone(),
            RecordingBrowser::new(true),
            f.clock.clone(),
            CountingRandom::new(),
            ClientProfile::Release,
        ));
        service2.bootstrap().await;
        assert!(matches!(service2.status(), AccountStatus::Connected { .. }));
        assert_eq!(transport2.calls()[0].body["grant_type"], "refresh_token");
        assert_eq!(
            transport2.calls()[0].body["refresh_token"],
            "refresh-crash-recovery"
        );
    }

    #[tokio::test]
    async fn restart_with_valid_current_refresh_reconnects_without_touching_pending() {
        let f = make_fixture(true);
        f.store.profiles.lock().unwrap().insert(
            "release",
            StoredProfileData {
                schema_version: STORED_PROFILE_SCHEMA_VERSION,
                device_key_scalar_b64: URL_SAFE_NO_PAD.encode([9u8; 32]),
                device_id: Some("dev_restart".into()),
                refresh_current: Some(StoredRefresh {
                    refresh_token: "refresh-current".into(),
                    refresh_expires_at_unix: f.clock.now_unix().as_secs() + 1000,
                    refresh_absolute_expires_at_unix: f.clock.now_unix().as_secs() + 100_000,
                    account: StoredAccount {
                        nickname: "Old".into(),
                        clan_tag: None,
                        realm: None,
                    },
                }),
                refresh_pending: None,
            },
        );
        f.transport.push_ok(200, token_response("refresh-rotated"));
        f.service.bootstrap().await;
        assert!(matches!(
            f.service.status(),
            AccountStatus::Connected { .. }
        ));
        assert_eq!(
            f.transport.calls()[0].body["refresh_token"],
            "refresh-current"
        );
    }

    #[tokio::test]
    async fn restart_never_falls_back_to_pending_after_current_is_rejected() {
        let f = make_fixture(true);
        f.store.profiles.lock().unwrap().insert(
            "release",
            StoredProfileData {
                schema_version: STORED_PROFILE_SCHEMA_VERSION,
                device_key_scalar_b64: URL_SAFE_NO_PAD.encode([9u8; 32]),
                device_id: Some("dev_restart".into()),
                refresh_current: Some(StoredRefresh {
                    refresh_token: "refresh-revoked".into(),
                    refresh_expires_at_unix: f.clock.now_unix().as_secs() + 1000,
                    refresh_absolute_expires_at_unix: f.clock.now_unix().as_secs() + 100_000,
                    account: StoredAccount {
                        nickname: "Old".into(),
                        clan_tag: None,
                        realm: None,
                    },
                }),
                refresh_pending: Some(StoredRefresh {
                    refresh_token: "refresh-pending-but-unsafe".into(),
                    refresh_expires_at_unix: f.clock.now_unix().as_secs() + 1000,
                    refresh_absolute_expires_at_unix: f.clock.now_unix().as_secs() + 100_000,
                    account: StoredAccount {
                        nickname: "Old".into(),
                        clan_tag: None,
                        realm: None,
                    },
                }),
            },
        );
        // The Engine rejects `current` outright (e.g. reuse/theft detected —
        // the whole family, `pending` included, is revoked server-side).
        f.transport.push_ok(
            400,
            json!({"error":"invalid_grant","error_description":"reuse detected","request_id":"r1"}),
        );
        f.service.bootstrap().await;
        assert_eq!(f.service.status(), AccountStatus::SignedOut);
        // `pending` must never have been tried.
        assert_eq!(f.transport.calls().len(), 1);
    }

    #[tokio::test]
    async fn restart_with_absolute_expired_current_and_no_pending_stays_signed_out() {
        let f = make_fixture(true);
        f.store.profiles.lock().unwrap().insert(
            "release",
            StoredProfileData {
                schema_version: STORED_PROFILE_SCHEMA_VERSION,
                device_key_scalar_b64: URL_SAFE_NO_PAD.encode([9u8; 32]),
                device_id: Some("dev_restart".into()),
                refresh_current: Some(StoredRefresh {
                    refresh_token: "refresh-too-old".into(),
                    refresh_expires_at_unix: 1,
                    refresh_absolute_expires_at_unix: 1,
                    account: StoredAccount {
                        nickname: "Old".into(),
                        clan_tag: None,
                        realm: None,
                    },
                }),
                refresh_pending: None,
            },
        );
        f.service.bootstrap().await;
        assert_eq!(f.service.status(), AccountStatus::SignedOut);
        assert!(
            f.transport.calls().is_empty(),
            "an absolutely-expired refresh must never be sent to the Engine"
        );
    }

    #[tokio::test]
    async fn restart_with_no_stored_profile_stays_signed_out_without_any_network_call() {
        let f = make_fixture(true);
        f.service.bootstrap().await;
        assert_eq!(f.service.status(), AccountStatus::SignedOut);
        assert!(f.transport.calls().is_empty());
    }

    #[tokio::test]
    async fn late_response_after_a_fresh_login_replaces_the_old_attempt_is_also_stale() {
        let f = make_fixture(true);
        let state_1 = begin_and_get_state(&f).await;
        let now = f.clock.now_instant();
        let consumed_1 = {
            let mut inner = f.service.inner.lock().unwrap();
            inner.login_flow.consume_code(&state_1, now).unwrap()
        };
        // A brand-new login attempt starts (replacing the old one) before
        // the first exchange's network response arrives. The device is
        // already registered from the first `begin_login`, so only a fresh
        // `/authorize` call is expected this time.
        f.transport.push_ok(200, authorize_response());
        f.service.begin_login().await.unwrap();

        f.transport
            .push_ok(200, token_response("refresh-from-stale-attempt"));
        Arc::clone(&f.service)
            .complete_exchange("stale-code".into(), consumed_1)
            .await;

        // The stale exchange must not have connected anything.
        assert_eq!(f.service.status(), AccountStatus::AwaitingBrowser);
        assert!(
            f.store
                .profiles
                .lock()
                .unwrap()
                .get("release")
                .and_then(|d| d.refresh_current.clone())
                .is_none()
        );
    }

    // ── Credential-mutation serialization boundary (td-27bcc9 rejection
    // cycle 1): bootstrap's one-shot refresh, login-exchange commit,
    // invalidate/logout and device registration all share one commit
    // boundary (`commit_lock` + `credential_generation`). These tests use a
    // deterministic gate on a specific in-flight network call instead of
    // real timing, so the interleaving is exact and repeatable. ───────────

    /// Wraps a `ScriptedTransport` and blocks the FIRST request matching
    /// `predicate(path, body)` until the test calls `release.notify_one()`
    /// — the deterministic stand-in for "this network response is still in
    /// flight". Every other request (and every later request, once the one
    /// match has been gated) passes straight through untouched.
    struct GateOnMatch {
        inner: Arc<ScriptedTransport>,
        predicate: Box<dyn Fn(&str, &serde_json::Value) -> bool + Send + Sync>,
        gated: AtomicBool,
        reached: Arc<tokio::sync::Notify>,
        release: Arc<tokio::sync::Notify>,
    }
    impl GateOnMatch {
        fn new(
            inner: Arc<ScriptedTransport>,
            predicate: impl Fn(&str, &serde_json::Value) -> bool + Send + Sync + 'static,
        ) -> (
            Arc<Self>,
            Arc<tokio::sync::Notify>,
            Arc<tokio::sync::Notify>,
        ) {
            let reached = Arc::new(tokio::sync::Notify::new());
            let release = Arc::new(tokio::sync::Notify::new());
            let gate = Arc::new(Self {
                inner,
                predicate: Box::new(predicate),
                gated: AtomicBool::new(false),
                reached: reached.clone(),
                release: release.clone(),
            });
            (gate, reached, release)
        }
        fn calls(&self) -> Vec<RecordedCall> {
            self.inner.calls()
        }
    }
    #[async_trait]
    impl AccountTransport for GateOnMatch {
        async fn post_json(
            &self,
            path: &str,
            headers: Vec<(String, String)>,
            body: serde_json::Value,
        ) -> Result<HttpResponse, TransportError> {
            // Predicate FIRST: the gate latch must only be spent on a call
            // that actually matches, otherwise an earlier unrelated request
            // (e.g. bootstrap's own refresh) silently disarms the gate and
            // the waiting test hangs forever on `reached`.
            if (self.predicate)(path, &body) && !self.gated.swap(true, Ordering::SeqCst) {
                self.reached.notify_one();
                self.release.notified().await;
            }
            self.inner.post_json(path, headers, body).await
        }
    }

    fn stored_old_account(
        device_id: &str,
        refresh_token: &str,
        now_unix: u64,
    ) -> StoredProfileData {
        StoredProfileData {
            schema_version: STORED_PROFILE_SCHEMA_VERSION,
            device_key_scalar_b64: URL_SAFE_NO_PAD.encode([9u8; 32]),
            device_id: Some(device_id.to_string()),
            refresh_current: Some(StoredRefresh {
                refresh_token: refresh_token.to_string(),
                refresh_expires_at_unix: now_unix + 1_000,
                refresh_absolute_expires_at_unix: now_unix + 100_000,
                account: StoredAccount {
                    nickname: "OldAccount".into(),
                    clan_tag: None,
                    realm: None,
                },
            }),
            refresh_pending: None,
        }
    }

    #[tokio::test]
    async fn deferred_bootstrap_refresh_response_after_a_fresh_login_commits_writes_nothing() {
        let store = Arc::new(InMemoryStore::default());
        let now_unix = 1_700_000_000u64;
        store.profiles.lock().unwrap().insert(
            "release",
            stored_old_account("dev_race", "refresh-old", now_unix),
        );

        let (gate, reached, release) = GateOnMatch::new(ScriptedTransport::new(), |path, body| {
            path == "/token" && body["grant_type"] == "refresh_token"
        });
        // Consumption order matches invocation order, not push order: the
        // gated bootstrap refresh call blocks BEFORE popping a response, so
        // the fresh login's unblocked `/authorize` then `/token` calls pop
        // the first two queued responses; the refresh response is popped
        // last, once released.
        gate.inner.push_ok(200, authorize_response());
        gate.inner.push_ok(200, token_response("refresh-new"));
        gate.inner.push_ok(200, token_response("refresh-old-late"));

        let clock = FixedClock::new();
        let service = Arc::new(AccountService::new(
            gate.clone(),
            store.clone(),
            RecordingBrowser::new(true),
            clock.clone(),
            CountingRandom::new(),
            ClientProfile::Release,
        ));
        service.set_port(43210);

        let bootstrap_service = service.clone();
        let bootstrap_task = tokio::spawn(async move { bootstrap_service.bootstrap().await });

        // Wait until the bootstrap refresh's network call is in flight.
        reached.notified().await;

        // A brand-new interactive login now runs to full completion while
        // the old refresh response is still withheld.
        service.begin_login().await.unwrap();
        let state = gate
            .calls()
            .iter()
            .find(|c| c.path == "/authorize")
            .unwrap()
            .body["state"]
            .as_str()
            .unwrap()
            .to_string();
        let now = clock.now_instant();
        let consumed = {
            let mut inner = service.inner.lock().unwrap();
            inner.login_flow.consume_code(&state, now).unwrap()
        };
        Arc::clone(&service)
            .complete_exchange("auth-code".into(), consumed)
            .await;
        assert!(
            matches!(service.status(), AccountStatus::Connected { .. }),
            "the fresh login must have fully committed before the old refresh is released"
        );

        // Now release the old bootstrap refresh's response.
        release.notify_one();
        bootstrap_task.await.unwrap();

        // The stale refresh must have performed zero writes and zero status
        // changes: the new login's creds/slots/display/status must remain.
        match service.status() {
            AccountStatus::Connected { nickname, .. } => assert_eq!(nickname, "FrankDrake"),
            other => {
                panic!("expected the fresh login's Connected status to survive, got {other:?}")
            }
        }
        let stored = store
            .profiles
            .lock()
            .unwrap()
            .get("release")
            .cloned()
            .unwrap();
        assert_eq!(stored.refresh_current.unwrap().refresh_token, "refresh-new");
    }

    #[tokio::test]
    async fn invalidate_during_a_delayed_bootstrap_refresh_leaves_no_writes() {
        let store = Arc::new(InMemoryStore::default());
        let now_unix = 1_700_000_000u64;
        store.profiles.lock().unwrap().insert(
            "release",
            stored_old_account("dev_inv", "refresh-old", now_unix),
        );

        let (gate, reached, release) = GateOnMatch::new(ScriptedTransport::new(), |path, body| {
            path == "/token" && body["grant_type"] == "refresh_token"
        });
        gate.inner.push_ok(200, token_response("refresh-old-late"));

        let clock = FixedClock::new();
        let service = Arc::new(AccountService::new(
            gate.clone(),
            store.clone(),
            RecordingBrowser::new(true),
            clock.clone(),
            CountingRandom::new(),
            ClientProfile::Release,
        ));
        service.set_port(43210);

        let bootstrap_service = service.clone();
        let bootstrap_task = tokio::spawn(async move { bootstrap_service.bootstrap().await });
        reached.notified().await;

        // Invalidate (logout / account switch) lands while the refresh is
        // still in flight.
        service.invalidate_local().await;
        assert!(
            store.profiles.lock().unwrap().get("release").is_none(),
            "invalidate must have wiped the store"
        );
        let saves_before_release = store.save_calls.load(Ordering::SeqCst);

        release.notify_one();
        bootstrap_task.await.unwrap();

        // The stale refresh must not have resurrected anything.
        assert_eq!(service.status(), AccountStatus::SignedOut);
        assert!(store.profiles.lock().unwrap().get("release").is_none());
        assert_eq!(
            store.save_calls.load(Ordering::SeqCst),
            saves_before_release,
            "the stale refresh must not have written to the store at all"
        );
    }

    #[tokio::test]
    async fn simultaneous_begin_login_registers_the_device_exactly_once() {
        let (gate, reached, release) =
            GateOnMatch::new(ScriptedTransport::new(), |path, _| path == "/device");
        gate.inner.push_ok(201, device_response());
        gate.inner.push_ok(200, authorize_response());
        gate.inner.push_ok(200, authorize_response());

        let store = Arc::new(InMemoryStore::default());
        let service = Arc::new(AccountService::new(
            gate.clone(),
            store.clone(),
            RecordingBrowser::new(true),
            FixedClock::new(),
            CountingRandom::new(),
            ClientProfile::Release,
        ));
        service.set_port(43210);

        let s1 = service.clone();
        let s2 = service.clone();
        let t1 = tokio::spawn(async move { AccountService::begin_login(&s1).await });
        let t2 = tokio::spawn(async move { AccountService::begin_login(&s2).await });

        // Whichever call reaches `/device` first blocks there; the other
        // cannot even start its own `/device` call while `commit_lock` is
        // held, so releasing here is safe regardless of which one it was.
        reached.notified().await;
        release.notify_one();

        t1.await.unwrap().unwrap();
        t2.await.unwrap().unwrap();

        let calls = gate.calls();
        let device_calls: Vec<_> = calls.iter().filter(|c| c.path == "/device").collect();
        assert_eq!(
            device_calls.len(),
            1,
            "concurrent begin_login must register the device exactly once"
        );
        let authorize_device_ids: std::collections::HashSet<_> = calls
            .iter()
            .filter(|c| c.path == "/authorize")
            .map(|c| c.body["device_id"].clone())
            .collect();
        assert_eq!(
            authorize_device_ids.len(),
            1,
            "both concurrent begin_login calls must end up using the same registered device_id"
        );
        // Exactly one key generated + saved, one device_id saved — no
        // duplicate key material ever written to the store.
        assert_eq!(store.save_calls.load(Ordering::SeqCst), 2);
    }

    // ── Nonce retry ──────────────────────────────────────────────────────────

    #[tokio::test]
    async fn a_use_dpop_nonce_challenge_is_retried_exactly_once_with_the_server_nonce() {
        let f = make_fixture(true);
        f.transport.push_ok_with_headers(
            401,
            vec![
                ("www-authenticate".into(), "DPoP error=\"use_dpop_nonce\"".into()),
                ("dpop-nonce".into(), "server-nonce-value".into()),
            ],
            json!({"error":"use_dpop_nonce","error_description":"retry with nonce","request_id":"r1"}),
        );
        f.transport.push_ok(201, device_response());
        f.transport.push_ok(200, authorize_response());

        f.service.begin_login().await.unwrap();

        let calls = f.transport.calls();
        assert_eq!(
            calls.len(),
            3,
            "the 401 attempt plus exactly one retry, then the next real call"
        );
        let first_proof = calls[0]
            .headers
            .iter()
            .find(|(k, _)| k == "DPoP")
            .unwrap()
            .1
            .clone();
        let retry_proof = calls[1]
            .headers
            .iter()
            .find(|(k, _)| k == "DPoP")
            .unwrap()
            .1
            .clone();
        assert_ne!(
            first_proof, retry_proof,
            "the retry must rebuild the proof (fresh jti/iat), never reuse it verbatim"
        );
        // The retried proof embeds the server nonce in its payload.
        let payload_b64 = retry_proof.split('.').nth(1).unwrap();
        let payload = String::from_utf8(URL_SAFE_NO_PAD.decode(payload_b64).unwrap()).unwrap();
        assert!(payload.contains("server-nonce-value"));
    }

    // ── Store-missing / denied / corrupt ────────────────────────────────────

    #[tokio::test]
    async fn unsupported_platform_store_fails_begin_login_explicitly() {
        let f = make_fixture(true);
        f.store.unsupported.store(true, Ordering::SeqCst);
        assert_eq!(
            f.service.begin_login().await,
            Err(AccountError::Store(StoreError::UnsupportedPlatform))
        );
    }

    #[tokio::test]
    async fn corrupt_local_profile_falls_back_to_generating_a_fresh_device_key() {
        let f = make_fixture(true);
        f.store.profiles.lock().unwrap().insert(
            "release",
            StoredProfileData {
                schema_version: 999,
                device_key_scalar_b64: "not-valid-base64!!".into(),
                device_id: None,
                refresh_current: None,
                refresh_pending: None,
            },
        );
        f.transport.push_ok(201, device_response());
        f.transport.push_ok(200, authorize_response());
        // Must not panic or error out — a corrupt local scalar is treated
        // like "no key yet" and a fresh one is generated and persisted.
        f.service.begin_login().await.unwrap();
        assert_eq!(f.service.status(), AccountStatus::AwaitingBrowser);
    }

    // ── invalidate_local / revoke primitive ──────────────────────────────────

    #[tokio::test]
    async fn invalidate_local_disconnects_clears_store_and_bumps_generation_against_stale_work() {
        let f = make_fixture(true);
        let state = begin_and_get_state(&f).await;
        f.transport
            .push_ok(200, token_response("refresh-to-be-cleared"));
        consume_and_exchange(&f, state, "auth-code").await;
        assert!(matches!(
            f.service.status(),
            AccountStatus::Connected { .. }
        ));

        f.service.invalidate_local().await;
        assert_eq!(f.service.status(), AccountStatus::SignedOut);
        assert!(f.store.profiles.lock().unwrap().get("release").is_none());
    }

    #[tokio::test]
    async fn profile_separation_release_and_debug_never_share_credentials() {
        let transport = ScriptedTransport::new();
        let store = Arc::new(InMemoryStore::default());
        let release = Arc::new(AccountService::new(
            transport.clone(),
            store.clone(),
            RecordingBrowser::new(true),
            FixedClock::new(),
            CountingRandom::new(),
            ClientProfile::Release,
        ));
        let debug = Arc::new(AccountService::new(
            transport.clone(),
            store.clone(),
            RecordingBrowser::new(true),
            FixedClock::new(),
            CountingRandom::new(),
            ClientProfile::Debug,
        ));
        release.set_port(43210);
        debug.set_port(43210);
        transport.push_ok(201, json!({"device_id":"dev_release"}));
        transport.push_ok(200, authorize_response());
        release.begin_login().await.unwrap();

        assert!(store.profiles.lock().unwrap().contains_key("release"));
        assert!(!store.profiles.lock().unwrap().contains_key("debug"));
        assert_eq!(debug.status(), AccountStatus::SignedOut);
    }

    // ── Recurring renewal scheduler (td-cab09a) ─────────────────────────────

    /// Connects via the normal interactive-login path (`token_response`'s
    /// `expires_in: 900` → `renew_in` of 450 s) so scheduler tests start
    /// from a plain `Connected` fixture without duplicating login plumbing.
    async fn connected_fixture() -> Fixture {
        let f = make_fixture(true);
        let state = begin_and_get_state(&f).await;
        f.transport.push_ok(200, token_response("refresh-initial"));
        consume_and_exchange(&f, state, "auth-code").await;
        assert!(matches!(
            f.service.status(),
            AccountStatus::Connected { .. }
        ));
        f
    }

    /// Only the scheduler's own refresh-grant `/token` calls (excludes the
    /// login exchange's authorization-code grant).
    fn refresh_calls(f: &Fixture) -> Vec<RecordedCall> {
        f.transport
            .calls()
            .into_iter()
            .filter(|c| c.path == "/token" && c.body["grant_type"] == "refresh_token")
            .collect()
    }

    fn dpop_header(call: &RecordedCall) -> String {
        call.headers
            .iter()
            .find(|(k, _)| k == "DPoP")
            .unwrap()
            .1
            .clone()
    }

    /// Decodes the `jti` claim out of a DPoP proof's payload segment, for
    /// tests that want to assert distinct/identical `jti`s directly rather
    /// than relying on full-proof-string equality alone.
    fn dpop_jti(proof: &str) -> String {
        let payload_b64 = proof.split('.').nth(1).unwrap();
        let payload = String::from_utf8(URL_SAFE_NO_PAD.decode(payload_b64).unwrap()).unwrap();
        let value: serde_json::Value = serde_json::from_str(&payload).unwrap();
        value["jti"].as_str().unwrap().to_string()
    }

    #[tokio::test]
    async fn scheduler_makes_exactly_three_attempts_staggered_5s_then_30s_then_exhausts() {
        let f = connected_fixture().await;
        f.clock.advance(450); // renew_in due

        f.service.renewal_tick().await; // attempt 1 (transport queue empty -> network error)
        assert_eq!(refresh_calls(&f).len(), 1);
        assert!(
            matches!(f.service.status(), AccountStatus::Connected { .. }),
            "a retry in flight stays silent"
        );

        // Not due yet before the 5 s local delay elapses.
        f.service.renewal_tick().await;
        assert_eq!(refresh_calls(&f).len(), 1);
        f.clock.advance(4);
        f.service.renewal_tick().await;
        assert_eq!(
            refresh_calls(&f).len(),
            1,
            "must not fire before its scheduled time"
        );
        f.clock.advance(1); // total 5s elapsed

        f.service.renewal_tick().await; // attempt 2
        assert_eq!(refresh_calls(&f).len(), 2);

        f.service.renewal_tick().await;
        assert_eq!(refresh_calls(&f).len(), 2);
        f.clock.advance(29);
        f.service.renewal_tick().await;
        assert_eq!(
            refresh_calls(&f).len(),
            2,
            "must not fire before the 30s delay elapses"
        );
        f.clock.advance(1); // total 30s elapsed since attempt 2's failure

        f.service.renewal_tick().await; // attempt 3
        assert_eq!(refresh_calls(&f).len(), 3);
        assert!(matches!(
            f.service.status(),
            AccountStatus::RenewalFailed { .. }
        ));

        // Never a 4th attempt, no matter how much later the next tick runs.
        f.clock.advance(3600);
        f.service.renewal_tick().await;
        assert_eq!(refresh_calls(&f).len(), 3);
        assert!(matches!(
            f.service.status(),
            AccountStatus::RenewalFailed { .. }
        ));
    }

    #[tokio::test]
    async fn scheduler_honours_a_longer_retry_after_than_the_local_backoff_capped_at_300s() {
        let f = connected_fixture().await;
        f.clock.advance(450);
        f.transport.push_ok_with_headers(
            503,
            vec![("retry-after".into(), "90".into())],
            json!({"error": "auth_backend_unavailable"}),
        );
        f.service.renewal_tick().await; // attempt 1
        assert_eq!(refresh_calls(&f).len(), 1);

        f.clock.advance(89);
        f.service.renewal_tick().await;
        assert_eq!(
            refresh_calls(&f).len(),
            1,
            "the server's 90s Retry-After beats the local 5s delay"
        );
        f.clock.advance(1); // exactly 90s elapsed
        f.transport
            .push_ok(200, token_response("refresh-after-retry-after"));
        f.service.renewal_tick().await; // attempt 2, now due
        assert_eq!(refresh_calls(&f).len(), 2);
        assert!(matches!(
            f.service.status(),
            AccountStatus::Connected { .. }
        ));
    }

    #[tokio::test]
    async fn renewal_retry_within_60s_reuses_the_exact_same_proof_and_jti() {
        let f = connected_fixture().await;
        f.clock.advance(450);
        f.service.renewal_tick().await; // attempt 1, network error
        let proof1 = dpop_header(&refresh_calls(&f)[0]);

        f.clock.advance(5); // attempt 2, only 5s after attempt 1's iat (<60s)
        f.service.renewal_tick().await;
        let proof2 = dpop_header(&refresh_calls(&f)[1]);

        assert_eq!(
            proof1, proof2,
            "a retry under 60s old must reuse the exact same proof/jti"
        );
        assert_eq!(dpop_jti(&proof1), dpop_jti(&proof2));
    }

    #[tokio::test]
    async fn renewal_retry_after_60s_mints_a_fresh_proof_and_jti() {
        let f = connected_fixture().await;
        f.clock.advance(450);
        // A long server Retry-After simulates a suspend/queue delay that
        // pushes the next attempt's send time well past the proof's 60s
        // freshness window.
        f.transport.push_ok_with_headers(
            429,
            vec![("retry-after".into(), "90".into())],
            json!({"error": "rate_limited"}),
        );
        f.service.renewal_tick().await; // attempt 1
        let proof1 = dpop_header(&refresh_calls(&f)[0]);

        f.clock.advance(90); // 90s later: proof1's iat is now 90s old (>60s)
        f.transport
            .push_ok(200, token_response("refresh-fresh-proof"));
        f.service.renewal_tick().await; // attempt 2
        let proof2 = dpop_header(&refresh_calls(&f)[1]);

        assert_ne!(
            proof1, proof2,
            "a retry sent 90s later must mint a fresh proof"
        );
        assert_ne!(
            dpop_jti(&proof1),
            dpop_jti(&proof2),
            "and therefore a fresh jti"
        );
    }

    #[tokio::test]
    async fn definitive_invalid_grant_reasons_disconnect_without_spending_retry_budget() {
        let reasons = [
            "refresh_expired",
            "refresh_absolute_expired",
            "refresh_reuse_detected",
            "refresh_orphaned",
            "device_revoked",
            "account_changed",
        ];
        for reason in reasons {
            let f = connected_fixture().await;
            f.clock.advance(450);
            f.transport
                .push_ok(400, json!({"error": "invalid_grant", "reason": reason}));

            f.service.renewal_tick().await;

            assert_eq!(
                refresh_calls(&f).len(),
                1,
                "{reason}: must not retry a definitive reason"
            );
            assert_eq!(
                f.service.status(),
                AccountStatus::SignedOut,
                "{reason}: must require sign-in again, not stay Connected/RenewalFailed"
            );

            // No zombie retries even much later.
            f.clock.advance(3600);
            f.service.renewal_tick().await;
            assert_eq!(
                refresh_calls(&f).len(),
                1,
                "{reason}: a signed-out session must never renew again"
            );
        }
    }

    #[tokio::test]
    async fn transient_network_5xx_429_and_dpop_proof_errors_all_retry_not_definitive() {
        let cases: Vec<(u16, serde_json::Value)> = vec![
            (503, json!({"error": "auth_backend_unavailable"})),
            (429, json!({"error": "rate_limited"})),
            (401, json!({"error": "invalid_dpop_proof"})),
            (500, json!({"error": "internal_error"})),
        ];
        for (status, body) in cases {
            let f = connected_fixture().await;
            f.clock.advance(450);
            f.transport.push_ok(status, body.clone());
            f.service.renewal_tick().await;
            assert_eq!(
                refresh_calls(&f).len(),
                1,
                "status {status}: must have attempted once"
            );
            assert!(
                matches!(f.service.status(), AccountStatus::Connected { .. }),
                "status {status}: a transient failure must stay silently Connected, not disconnect"
            );
            // The retry budget is still available: the next attempt fires.
            f.clock.advance(5);
            f.transport
                .push_ok(200, token_response("refresh-recovered"));
            f.service.renewal_tick().await;
            assert_eq!(
                refresh_calls(&f).len(),
                2,
                "status {status}: must still have a retry left"
            );
            assert!(matches!(
                f.service.status(),
                AccountStatus::Connected { .. }
            ));
        }
    }

    #[tokio::test]
    async fn a_store_write_failure_during_scheduled_commit_leaves_a_usable_slot_for_the_next_attempt()
     {
        let f = connected_fixture().await;
        // 4 prior save_profile calls happened during connected_fixture's own
        // login: device-key persist, device_id persist, pending write,
        // promotion write (mirrors `crash_between_pending_write_and_
        // promotion_recovers_via_pending_on_restart`'s own accounting).
        assert_eq!(f.store.save_calls.load(Ordering::SeqCst), 4);
        f.clock.advance(450);
        // Fail exactly the PROMOTION write (save #6) of the scheduler's own
        // persist_new_refresh (save #5 = pending, #6 = promote).
        *f.store.fail_save_on_call.lock().unwrap() = Some(6);
        f.transport
            .push_ok(200, token_response("refresh-crash-recovery"));

        f.service.renewal_tick().await;

        // Treated like a transient failure: still silently Connected on the
        // OLD (still valid) access token, not exhausted and not corrupted.
        assert!(matches!(
            f.service.status(),
            AccountStatus::Connected { .. }
        ));
        let stored = f
            .store
            .profiles
            .lock()
            .unwrap()
            .get("release")
            .cloned()
            .unwrap();
        assert_eq!(
            stored.refresh_current.as_ref().unwrap().refresh_token,
            "refresh-initial",
            "the OLD current must be untouched — the failed promotion never overwrote it"
        );
        assert_eq!(
            stored.refresh_pending.as_ref().unwrap().refresh_token,
            "refresh-crash-recovery",
            "the pending write itself must have survived the later promotion failure"
        );

        // The next scheduled attempt (backoff already applied, save-failure
        // was one-shot, not sticky) succeeds normally and promotes cleanly.
        f.clock.advance(5);
        f.transport
            .push_ok(200, token_response("refresh-crash-recovery"));
        f.service.renewal_tick().await;
        assert!(matches!(
            f.service.status(),
            AccountStatus::Connected { .. }
        ));
        let stored = f
            .store
            .profiles
            .lock()
            .unwrap()
            .get("release")
            .cloned()
            .unwrap();
        assert_eq!(
            stored.refresh_current.as_ref().unwrap().refresh_token,
            "refresh-crash-recovery"
        );
        assert!(stored.refresh_pending.is_none());

        // A fresh restart also still reconnects cleanly from the same store.
        let transport2 = ScriptedTransport::new();
        let service2 = Arc::new(AccountService::new(
            transport2.clone(),
            f.store.clone(),
            RecordingBrowser::new(true),
            f.clock.clone(),
            CountingRandom::new(),
            ClientProfile::Release,
        ));
        service2.set_port(43210);
        transport2.push_ok(200, token_response("refresh-after-restart"));
        service2.bootstrap().await;
        assert!(matches!(service2.status(), AccountStatus::Connected { .. }));
        assert_eq!(
            transport2.calls()[0].body["refresh_token"],
            "refresh-crash-recovery"
        );
    }

    #[tokio::test]
    async fn scheduled_renewal_response_after_invalidate_local_writes_nothing() {
        let store = Arc::new(InMemoryStore::default());
        let now_unix = 1_700_000_000u64;
        store.profiles.lock().unwrap().insert(
            "release",
            stored_old_account("dev_sched_inv", "refresh-seed", now_unix),
        );

        let (gate, reached, release) = GateOnMatch::new(ScriptedTransport::new(), |path, body| {
            path == "/token"
                && body["grant_type"] == "refresh_token"
                && body["refresh_token"] == "refresh-boot"
        });
        gate.inner.push_ok(200, token_response("refresh-boot")); // bootstrap's own refresh, ungated
        gate.inner
            .push_ok(200, token_response("refresh-after-invalidate")); // the scheduler's gated attempt

        let clock = FixedClock::new();
        let service = Arc::new(AccountService::new(
            gate.clone(),
            store.clone(),
            RecordingBrowser::new(true),
            clock.clone(),
            CountingRandom::new(),
            ClientProfile::Release,
        ));
        service.set_port(43210);
        service.bootstrap().await;
        assert!(matches!(service.status(), AccountStatus::Connected { .. }));

        clock.advance(450); // renew_in due, per token_response's expires_in: 900
        let svc = service.clone();
        let tick_task = tokio::spawn(async move { svc.renewal_tick().await });
        reached.notified().await;

        service.invalidate_local().await;
        assert_eq!(service.status(), AccountStatus::SignedOut);
        let saves_before_release = store.save_calls.load(Ordering::SeqCst);

        release.notify_one();
        tick_task.await.unwrap();

        assert_eq!(service.status(), AccountStatus::SignedOut);
        assert_eq!(
            store.save_calls.load(Ordering::SeqCst),
            saves_before_release,
            "the stale scheduler response must not have written to the store at all"
        );
    }

    #[tokio::test]
    async fn a_stale_attempt_ticket_cannot_commit_once_a_newer_attempt_in_the_same_chain_has_begun()
    {
        let f = connected_fixture().await;
        f.clock.advance(450);
        // Attempt 1 of the chain, captured directly so the test still holds
        // its ticket after the chain itself has moved on.
        let stale_ticket = {
            let mut inner = f.service.inner.lock().unwrap();
            inner.session.begin_renewal(f.clock.now_instant()).unwrap()
        };
        // Attempt 1 fails transiently; once its 5s backoff is due the
        // scheduler starts attempt 2 (empty transport script = network
        // error), so `stale_ticket` is no longer the current attempt.
        f.service
            .commit_renewal_failure(
                stale_ticket,
                RenewalFailure::Transient { retry_after: None },
                None,
            )
            .await;
        f.clock.advance(5);
        f.service.renewal_tick().await;
        assert!(matches!(
            f.service.status(),
            AccountStatus::Connected { .. }
        ));

        // A late SUCCESS response for the now-superseded attempt-1 ticket
        // must never commit.
        let parsed = parse_token_response(&token_response("refresh-late-attempt-1")).unwrap();
        f.service.commit_renewal_success(stale_ticket, parsed).await;
        let stored = f
            .store
            .profiles
            .lock()
            .unwrap()
            .get("release")
            .cloned()
            .unwrap();
        assert_eq!(
            stored.refresh_current.as_ref().unwrap().refresh_token,
            "refresh-initial",
            "a stale attempt's success must never overwrite the store"
        );

        // A late DEFINITIVE failure for the same stale ticket must not tear
        // down the still-live (later-attempt) chain either.
        f.service
            .commit_renewal_failure(stale_ticket, RenewalFailure::ReauthenticationRequired, None)
            .await;
        assert!(matches!(
            f.service.status(),
            AccountStatus::Connected { .. }
        ));
        assert!(f.store.profiles.lock().unwrap().get("release").is_some());
    }

    #[tokio::test]
    async fn scheduler_recovers_correctly_after_a_large_clock_jump_simulating_sleep_or_offline() {
        let f = connected_fixture().await;
        f.clock.advance(450);

        f.service.renewal_tick().await; // attempt 1
        assert_eq!(refresh_calls(&f).len(), 1);

        // The whole tick loop itself was "asleep" (system suspend) for an
        // hour, far longer than the 5s scheduled delay.
        f.clock.advance(3600);
        f.service.renewal_tick().await; // attempt 2 (self-heals: due-ness is re-derived from `now`)
        assert_eq!(refresh_calls(&f).len(), 2);

        f.clock.advance(3600);
        f.service.renewal_tick().await; // attempt 3
        assert_eq!(refresh_calls(&f).len(), 3);
        assert!(matches!(
            f.service.status(),
            AccountStatus::RenewalFailed { .. }
        ));

        // A long offline/sleep gap AFTER exhaustion never causes a 4th
        // attempt or a duplicate exhausted-state transition.
        f.clock.advance(86_400);
        f.service.renewal_tick().await;
        assert_eq!(
            refresh_calls(&f).len(),
            3,
            "no duplicate attempts after a long offline/sleep gap"
        );
        assert!(matches!(
            f.service.status(),
            AccountStatus::RenewalFailed { .. }
        ));
    }

    #[tokio::test]
    async fn exhausted_renewal_status_is_stable_and_never_re_signaled_on_repeated_polls() {
        let f = connected_fixture().await;
        f.clock.advance(450);
        f.service.renewal_tick().await;
        f.clock.advance(5);
        f.service.renewal_tick().await;
        f.clock.advance(30);
        f.service.renewal_tick().await;
        assert!(matches!(
            f.service.status(),
            AccountStatus::RenewalFailed { .. }
        ));
        assert!(f.service.inner.lock().unwrap().renewal_failure_logged);

        let calls_before = refresh_calls(&f).len();
        for _ in 0..5 {
            assert!(matches!(
                f.service.status(),
                AccountStatus::RenewalFailed { .. }
            ));
        }
        assert_eq!(
            refresh_calls(&f).len(),
            calls_before,
            "polling status() must never trigger new network activity"
        );
        assert!(
            f.service.inner.lock().unwrap().renewal_failure_logged,
            "still exactly one logged transition"
        );
    }

    #[tokio::test]
    async fn web_lease_expires_at_is_captured_on_scheduled_refresh_and_only_a_success_advances_it()
    {
        let f = connected_fixture().await;
        // Captured on the initial exchange already ("2030-01-01T12:15:00Z" -> 1_893_500_100).
        match f.service.status() {
            AccountStatus::Connected {
                web_lease_expires_at,
                ..
            } => {
                assert_eq!(web_lease_expires_at, Some(1_893_500_100))
            }
            other => panic!("expected Connected, got {other:?}"),
        }

        f.clock.advance(450);
        // A transient failure must not move it at all.
        f.service.renewal_tick().await; // network error
        match f.service.status() {
            AccountStatus::Connected {
                web_lease_expires_at,
                ..
            } => {
                assert_eq!(
                    web_lease_expires_at,
                    Some(1_893_500_100),
                    "a failed attempt must never advance the lease"
                )
            }
            other => panic!("expected Connected, got {other:?}"),
        }

        // A later successful refresh with a NEW web_lease_expires_at does
        // advance it.
        f.clock.advance(5);
        let mut body = token_response("refresh-new-lease");
        body["web_lease_expires_at"] = json!("2030-06-01T00:00:00Z");
        f.transport.push_ok(200, body);
        f.service.renewal_tick().await;
        match f.service.status() {
            AccountStatus::Connected {
                web_lease_expires_at,
                ..
            } => {
                assert_eq!(web_lease_expires_at, Some(1_906_502_400))
            }
            other => panic!("expected Connected, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn account_changed_and_device_revoked_mid_session_disconnect_rather_than_retry() {
        for reason in ["account_changed", "device_revoked"] {
            let f = connected_fixture().await;
            f.clock.advance(450);
            f.transport
                .push_ok(400, json!({"error": "invalid_grant", "reason": reason}));
            f.service.renewal_tick().await;
            assert_eq!(
                refresh_calls(&f).len(),
                1,
                "{reason}: a mid-session scheduled renewal must not retry"
            );
            assert_eq!(
                f.service.status(),
                AccountStatus::SignedOut,
                "{reason}: must require sign-in again"
            );
        }
    }

    // ── Scheduler loop lifecycle (td-cab09a) ────────────────────────────────

    #[tokio::test]
    async fn next_tick_delay_follows_the_session_deadline_and_a_clock_jump_re_checks_at_once() {
        let f = make_fixture(true);
        assert_eq!(
            f.service.next_tick_delay(),
            MAX_RENEWAL_SLEEP,
            "signed out: nothing scheduled, plain re-check cadence"
        );

        let f = connected_fixture().await;
        assert_eq!(
            f.service.next_tick_delay(),
            MAX_RENEWAL_SLEEP,
            "renewal is 450s away, far beyond the capped sleep"
        );
        f.clock.advance(410); // 40s left of the 450s renew_in
        assert_eq!(f.service.next_tick_delay(), Duration::from_secs(40));

        // A huge jump (sleep/resume or a long offline gap) lands past the
        // deadline: the next wake is immediate, not a missed cycle.
        f.clock.advance(86_400);
        assert_eq!(f.service.next_tick_delay(), MIN_RENEWAL_SLEEP);

        // After a transient failure the backoff deadline drives the sleep.
        f.service.renewal_tick().await; // empty script -> network error
        assert_eq!(f.service.next_tick_delay(), Duration::from_secs(5));
    }

    #[tokio::test]
    async fn the_renewal_loop_performs_a_due_renewal_and_stops_promptly_when_cancelled() {
        let f = connected_fixture().await;
        f.clock.advance(450);
        f.transport
            .push_ok(200, token_response("refresh-from-loop"));

        let (stop, rx) = tokio::sync::watch::channel(false);
        let task = tokio::spawn(crate::account::renewal::run_renewal_loop(
            f.service.clone(),
            rx,
        ));

        // Bounded wait on the loop's own first tick — no real backoff sleep
        // is involved, the scripted transport answers immediately.
        let renewed = tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                let rotated = f
                    .store
                    .profiles
                    .lock()
                    .unwrap()
                    .get("release")
                    .and_then(|d| d.refresh_current.clone())
                    .is_some_and(|r| r.refresh_token == "refresh-from-loop");
                if rotated {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await;
        assert!(
            renewed.is_ok(),
            "the loop must run the due renewal on its first tick"
        );
        assert!(matches!(
            f.service.status(),
            AccountStatus::Connected { .. }
        ));

        // The loop is now parked in its (capped) sleep; cancelling must end
        // it without waiting that sleep out.
        stop.send(true).unwrap();
        let joined = tokio::time::timeout(Duration::from_secs(5), task).await;
        assert!(joined.is_ok(), "cancellation must end the loop promptly");
        joined.unwrap().unwrap();
        assert_eq!(
            refresh_calls(&f).len(),
            1,
            "a cancelled loop performs no further attempts"
        );
    }

    #[tokio::test]
    async fn dropping_the_scheduler_handle_ends_the_renewal_loop() {
        let f = connected_fixture().await; // nothing due yet: the first tick is a no-op
        let (stop, rx) = tokio::sync::watch::channel(false);
        let task = tokio::spawn(crate::account::renewal::run_renewal_loop(
            f.service.clone(),
            rx,
        ));
        drop(stop); // what `RenewalScheduler::drop` amounts to for the loop
        let joined = tokio::time::timeout(Duration::from_secs(5), task).await;
        assert!(
            joined.is_ok(),
            "a dropped scheduler must not leave the loop running"
        );
        joined.unwrap().unwrap();
        assert!(
            refresh_calls(&f).is_empty(),
            "no renewal was due, so no attempt may happen"
        );
    }

    // ── Native lifecycle snapshot / subscription / signing seam (td-811657) ─

    #[tokio::test]
    async fn a_subscriber_attaching_after_construction_immediately_observes_the_current_snapshot() {
        let f = make_fixture(true);
        // Attaches before any mutation at all.
        let rx = f.service.subscribe();
        let initial = rx.borrow().clone();
        assert_eq!(initial.status, AccountStatus::SignedOut);
        assert!(!initial.can_use_account);
        assert_eq!(initial.lifecycle_epoch, 0);
        drop(rx);

        let state = begin_and_get_state(&f).await;
        f.transport
            .push_ok(200, token_response("refresh-late-subscriber"));
        consume_and_exchange(&f, state, "auth-code").await;

        // A NEW subscriber attaching mid-flight (after the mutation already
        // happened) must observe the LATEST state, not the initial one, and
        // without any poll loop.
        let rx2 = f.service.subscribe();
        assert!(
            rx2.borrow().can_use_account,
            "a mid-flight subscriber sees the latest snapshot"
        );
        assert_eq!(rx2.borrow().lifecycle_epoch, 1);
    }

    #[tokio::test]
    async fn access_expiry_while_a_renewal_retry_is_pending_makes_the_snapshot_unusable_and_reaches_a_subscriber()
     {
        let f = connected_fixture().await;
        let mut rx = f.service.subscribe();
        assert!(rx.borrow_and_update().can_use_account);

        // Past both `renew_at` (450s) and the 900s access lifetime: a single
        // tick performs one (failing) retry attempt and lands in
        // `Renewal::Waiting`, never `Failed` — the session is still
        // "retrying", not yet exhausted.
        f.clock.advance(1000);
        f.service.renewal_tick().await;

        assert!(
            matches!(f.service.status(), AccountStatus::Connected { .. }),
            "status() must still look Connected-shaped while a retry is pending"
        );
        let snap = f.service.snapshot();
        assert!(
            !snap.can_use_account,
            "the access token itself is long expired"
        );

        rx.changed().await.unwrap();
        assert!(
            !rx.borrow_and_update().can_use_account,
            "a subscriber must observe the unusable transition without polling status()"
        );
    }

    #[tokio::test]
    async fn a_slow_receiver_is_coalesced_to_only_the_latest_snapshot() {
        let f = connected_fixture().await;
        let mut rx = f.service.subscribe();
        rx.borrow_and_update();
        let initial_epoch = f.service.snapshot().lifecycle_epoch;

        // Two separate ordinary-renewal commits happen without the receiver
        // ever being read in between ("a slow receiver").
        f.clock.advance(450);
        f.transport
            .push_ok(200, token_response("refresh-coalesce-1"));
        f.service.renewal_tick().await;
        f.clock.advance(450);
        f.transport
            .push_ok(200, token_response("refresh-coalesce-2"));
        f.service.renewal_tick().await;

        rx.changed().await.unwrap();
        let latest = rx.borrow_and_update().clone();
        assert_eq!(
            latest.lifecycle_epoch, initial_epoch,
            "ordinary renewals never advance the epoch"
        );
        assert!(
            !rx.has_changed().unwrap(),
            "watch must coalesce to the single latest value, not queue the two intermediate sends"
        );
    }

    #[tokio::test]
    async fn an_ordinary_scheduled_refresh_commit_advances_expiry_but_leaves_lifecycle_epoch_unchanged()
     {
        let f = connected_fixture().await;
        let before = f.service.snapshot();

        f.clock.advance(450);
        f.transport.push_ok(200, token_response("refresh-rotated"));
        f.service.renewal_tick().await;

        let after = f.service.snapshot();
        assert_eq!(
            after.lifecycle_epoch, before.lifecycle_epoch,
            "an ordinary renewal must not advance the epoch"
        );
        assert!(after.can_use_account);
        assert_ne!(
            after.access_expires_at_unix, before.access_expires_at_unix,
            "the access expiry itself DOES advance on a successful renewal"
        );
    }

    #[tokio::test]
    async fn bootstrap_refresh_leaves_lifecycle_epoch_unchanged() {
        let f = make_fixture(true);
        f.store.profiles.lock().unwrap().insert(
            "release",
            StoredProfileData {
                schema_version: STORED_PROFILE_SCHEMA_VERSION,
                device_key_scalar_b64: URL_SAFE_NO_PAD.encode([9u8; 32]),
                device_id: Some("dev_restart".into()),
                refresh_current: Some(StoredRefresh {
                    refresh_token: "refresh-current".into(),
                    refresh_expires_at_unix: f.clock.now_unix().as_secs() + 1000,
                    refresh_absolute_expires_at_unix: f.clock.now_unix().as_secs() + 100_000,
                    account: StoredAccount {
                        nickname: "Old".into(),
                        clan_tag: None,
                        realm: None,
                    },
                }),
                refresh_pending: None,
            },
        );
        f.transport.push_ok(200, token_response("refresh-rotated"));

        f.service.bootstrap().await;

        let snap = f.service.snapshot();
        assert!(matches!(snap.status, AccountStatus::Connected { .. }));
        assert!(snap.can_use_account);
        assert_eq!(
            snap.lifecycle_epoch, 0,
            "bootstrap's one-shot refresh must never advance the lifecycle epoch"
        );
    }

    #[tokio::test]
    async fn login_exchange_commit_increments_lifecycle_epoch_and_clears_any_prior_end_reason() {
        let f = make_fixture(true);
        assert_eq!(f.service.snapshot().lifecycle_epoch, 0);

        let state = begin_and_get_state(&f).await;
        f.transport
            .push_ok(200, token_response("refresh-first-login"));
        consume_and_exchange(&f, state, "auth-code-1").await;
        let snap1 = f.service.snapshot();
        assert_eq!(
            snap1.lifecycle_epoch, 1,
            "the first login-exchange commit must advance the epoch"
        );
        assert_eq!(snap1.end_reason, None);

        // A second full login while already connected (an account switch)
        // reuses the already-registered device (no second `/device` call).
        f.transport.push_ok(200, authorize_response());
        f.service.begin_login().await.unwrap();
        let state2 = f.transport.calls().last().unwrap().body["state"]
            .as_str()
            .unwrap()
            .to_string();
        f.transport.push_ok(200, token_response("refresh-switched"));
        consume_and_exchange(&f, state2, "auth-code-2").await;
        let snap2 = f.service.snapshot();
        assert_eq!(
            snap2.lifecycle_epoch, 2,
            "a fresh login-exchange commit (switch) also advances the epoch"
        );
    }

    #[tokio::test]
    async fn invalidate_local_increments_lifecycle_epoch_and_reports_signed_out() {
        let f = connected_fixture().await;
        let before = f.service.snapshot();
        assert_eq!(before.lifecycle_epoch, 1);

        f.service.invalidate_local().await;

        let after = f.service.snapshot();
        assert_eq!(
            after.lifecycle_epoch, 2,
            "logout/reset must advance the epoch"
        );
        assert!(!after.can_use_account);
        assert_eq!(after.end_reason, Some(AccountEndReason::SignedOut));
    }

    #[tokio::test]
    async fn each_definitive_invalid_grant_reason_increments_lifecycle_epoch_and_reports_the_correct_end_reason()
     {
        let cases: [(&str, AccountEndReason); 6] = [
            ("refresh_expired", AccountEndReason::SignedOut),
            ("refresh_absolute_expired", AccountEndReason::SignedOut),
            ("refresh_reuse_detected", AccountEndReason::SignedOut),
            ("refresh_orphaned", AccountEndReason::SignedOut),
            ("device_revoked", AccountEndReason::Revoked),
            ("account_changed", AccountEndReason::AccountChanged),
        ];
        for (reason, expected) in cases {
            let f = connected_fixture().await;
            let before = f.service.snapshot();
            assert_eq!(
                before.lifecycle_epoch, 1,
                "{reason}: baseline after one login-exchange commit"
            );

            f.clock.advance(450);
            f.transport
                .push_ok(400, json!({"error": "invalid_grant", "reason": reason}));
            f.service.renewal_tick().await;

            let after = f.service.snapshot();
            assert_eq!(
                after.lifecycle_epoch, 2,
                "{reason}: a definitive outcome must advance the epoch exactly once"
            );
            assert!(!after.can_use_account, "{reason}: must be unusable");
            assert_eq!(
                after.end_reason,
                Some(expected),
                "{reason}: must report the correct safe end reason"
            );
        }
    }

    #[tokio::test]
    async fn renewal_exhaustion_does_not_change_lifecycle_epoch_but_reports_unusable_and_renewal_exhausted()
     {
        let f = connected_fixture().await;
        let before_epoch = f.service.snapshot().lifecycle_epoch;

        f.clock.advance(450);
        f.service.renewal_tick().await; // attempt 1 (network error)
        f.clock.advance(5);
        f.service.renewal_tick().await; // attempt 2 (network error)
        f.clock.advance(30);
        f.service.renewal_tick().await; // attempt 3 (network error) -> exhausted

        assert!(matches!(
            f.service.status(),
            AccountStatus::RenewalFailed { .. }
        ));
        let snap = f.service.snapshot();
        assert_eq!(
            snap.lifecycle_epoch, before_epoch,
            "exhaustion must not change the identity epoch"
        );
        assert!(!snap.can_use_account);
        assert_eq!(snap.end_reason, Some(AccountEndReason::RenewalExhausted));
    }

    #[tokio::test]
    async fn sign_request_requires_a_currently_usable_account() {
        let f = make_fixture(true);
        assert!(matches!(
            f.service.sign_request(SignedEndpoint::Handoff).await,
            Err(AccountError::NotReady)
        ));

        let f2 = connected_fixture().await;
        f2.service.invalidate_local().await;
        assert!(matches!(
            f2.service.sign_request(SignedEndpoint::Handoff).await,
            Err(AccountError::NotReady)
        ));
    }

    #[tokio::test]
    async fn sign_request_targets_the_exact_allowlisted_method_and_url_with_a_correct_dpop_and_ath()
    {
        let f = connected_fixture().await;
        let material = f
            .service
            .sign_request(SignedEndpoint::Handoff)
            .await
            .unwrap();

        let dpop = material
            .headers
            .iter()
            .find(|(k, _)| k == "DPoP")
            .unwrap()
            .1
            .clone();
        let authorization = material
            .headers
            .iter()
            .find(|(k, _)| k == "Authorization")
            .unwrap()
            .1
            .clone();
        assert!(authorization.starts_with("DPoP "));
        let access_token = authorization.trim_start_matches("DPoP ").to_string();
        assert_eq!(access_token, "eyTestAccessToken");

        let payload_b64 = dpop.split('.').nth(1).unwrap();
        let payload = String::from_utf8(URL_SAFE_NO_PAD.decode(payload_b64).unwrap()).unwrap();
        let value: serde_json::Value = serde_json::from_str(&payload).unwrap();
        assert_eq!(value["htm"], "POST");
        assert_eq!(
            value["htu"],
            "https://engine.tfd.rocks/api/v1/bridge/auth/handoff"
        );
        assert_eq!(
            value["ath"].as_str().unwrap(),
            bridge_core::dpop::ath_claim(&access_token),
            "the `ath` claim must hash the exact access token this material's Authorization header carries"
        );
        assert_eq!(
            material.lifecycle_epoch,
            f.service.snapshot().lifecycle_epoch
        );
    }

    #[tokio::test]
    async fn signed_request_material_epoch_is_stale_after_a_subsequent_invalidate_local() {
        let f = connected_fixture().await;
        let material = f
            .service
            .sign_request(SignedEndpoint::Handoff)
            .await
            .unwrap();
        let captured_epoch = material.lifecycle_epoch;

        f.service.invalidate_local().await;
        // Recovering usability with a fresh login must also move the epoch
        // past the captured one.
        f.transport.push_ok(200, authorize_response());
        f.service.begin_login().await.unwrap();
        let state = f.transport.calls().last().unwrap().body["state"]
            .as_str()
            .unwrap()
            .to_string();
        f.transport
            .push_ok(200, token_response("refresh-after-invalidate"));
        consume_and_exchange(&f, state, "auth-code-after-invalidate").await;

        let fresh = f.service.snapshot();
        assert_ne!(
            fresh.lifecycle_epoch, captured_epoch,
            "a consumer re-checking lifecycle_epoch immediately before applying a response must detect staleness"
        );
    }

    // td-811657: compile-time — the raw access token lives in this type's
    // `Authorization` header, so it must never be `Debug`/`Serialize`.
    static_assertions::assert_not_impl_any!(SignedRequestMaterial: std::fmt::Debug, serde::Serialize);

    #[tokio::test]
    async fn account_snapshot_never_leaks_a_secret_via_debug_or_serialize() {
        let f = make_fixture(true);
        let state = begin_and_get_state(&f).await;
        f.transport
            .push_ok(200, token_response("super-secret-refresh-canary-2"));
        consume_and_exchange(&f, state, "auth-code-canary-2").await;

        let snap = f.service.snapshot();
        let debugged = format!("{snap:?}");
        let serialized = serde_json::to_string(&snap).unwrap();
        for canary in [
            "super-secret-refresh-canary-2",
            "auth-code-canary-2",
            "eyTestAccessToken",
        ] {
            assert!(!debugged.contains(canary));
            assert!(!serialized.contains(canary));
        }
    }

    #[tokio::test]
    async fn subscribing_snapshotting_and_signing_do_not_spawn_a_second_background_scheduler() {
        let f = connected_fixture().await;
        let count_before = Arc::strong_count(&f.service);

        let _rx1 = f.service.subscribe();
        let _rx2 = f.service.subscribe();
        let _snap = f.service.snapshot();
        let _material = f
            .service
            .sign_request(SignedEndpoint::Handoff)
            .await
            .unwrap();
        assert_eq!(
            Arc::strong_count(&f.service),
            count_before,
            "subscribe/snapshot/sign_request must never spawn a task holding its own Arc clone"
        );

        // The ONLY account-related background task remains the existing
        // renewal scheduler (td-cab09a): starting it is the only thing that
        // may add exactly one more live clone of the service.
        let scheduler = crate::account::RenewalScheduler::start(f.service.clone());
        assert_eq!(
            Arc::strong_count(&f.service),
            count_before + 1,
            "exactly one background task holds a clone while the scheduler runs"
        );
        drop(scheduler);
    }
}
