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
    AccountTransport, BrowserLauncher, Clock, ClientProfile, HttpResponse, SecretStore,
    SecureRandom, StoreError, StoredAccount, StoredProfileData, StoredRefresh, TransportError,
    STORED_PROFILE_SCHEMA_VERSION,
};
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};
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
    },
    Canceled,
    Denied,
    AccountNotAuthorized,
    Expired,
    OpenFailed,
    StoreUnavailable,
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
        }
    }
}

pub struct AccountService {
    transport: Arc<dyn AccountTransport>,
    store: Arc<dyn SecretStore>,
    browser: Arc<dyn BrowserLauncher>,
    clock: Arc<dyn Clock>,
    rng: Arc<dyn SecureRandom>,
    profile: ClientProfile,
    port: AtomicU16,
    inner: Mutex<Inner>,
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
        Self {
            transport,
            store,
            browser,
            clock,
            rng,
            profile,
            port: AtomicU16::new(0),
            inner: Mutex::new(Inner::default()),
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
        }
        if matches!(
            inner.session.status(),
            bridge_core::account::AccountStatus::Connected
                | bridge_core::account::AccountStatus::Renewing
                | bridge_core::account::AccountStatus::RenewalFailed
        ) {
            let account = inner.account_display.clone().unwrap_or(StoredAccount {
                nickname: String::new(),
                clan_tag: None,
                realm: None,
            });
            return AccountStatus::Connected {
                nickname: account.nickname,
                clan_tag: account.clan_tag,
                realm: account.realm,
            };
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

    // ── Device key / registration ────────────────────────────────────────────

    fn ensure_device_key(&self) -> Result<DeviceKey, AccountError> {
        if let Some(key) = self.inner.lock().unwrap().device_key.clone() {
            return Ok(key);
        }
        let existing = self.store.load_profile(self.profile).map_err(AccountError::Store)?;
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
        self.store.save_profile(self.profile, &data).map_err(AccountError::Store)?;
        self.inner.lock().unwrap().device_key = Some(key.clone());
        Ok(key)
    }

    async fn ensure_registered(&self) -> Result<String, AccountError> {
        if let Some(id) = self.inner.lock().unwrap().device_id.clone() {
            return Ok(id);
        }
        let device_key = self.ensure_device_key()?;
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
        self.store.save_profile(self.profile, &data).map_err(AccountError::Store)?;
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
        validate_authorization_url(&parsed.authorization_url).map_err(|_| AccountError::InvalidResponse)?;
        let ttl = Duration::from_secs(parsed.expires_in.min(bridge_core::login_flow::MAX_TTL.as_secs()));
        let now = self.clock.now_instant();
        {
            let mut inner = self.inner.lock().unwrap();
            inner
                .login_flow
                .begin(state, verifier, redirect_uri, parsed.authorization_url.clone(), now, ttl)
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
                AuthCallbackOutcome::Code(code) => match inner.login_flow.consume_code(&callback.state, now) {
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
                },
                AuthCallbackOutcome::AccessDenied => {
                    match inner.login_flow.consume_terminal(&callback.state, now, TerminalReason::Denied) {
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
                    match inner
                        .login_flow
                        .consume_terminal(&callback.state, now, TerminalReason::AccountNotAuthorized)
                    {
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
                    match inner.login_flow.consume_terminal(&callback.state, now, TerminalReason::Expired) {
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
        self.finish_login_exchange(consumed.generation, result);
    }

    /// Generation recheck + durable persistence happen under ONE lock
    /// acquisition, so a `cancel_login`/`invalidate_local` that lands while
    /// the network call was in flight cannot be raced by a response that
    /// commits credentials afterwards.
    fn finish_login_exchange(&self, generation: u64, result: Result<HttpResponse, AccountError>) {
        let response = match result {
            Ok(r) if r.status == 200 => r,
            _ => return,
        };
        let parsed: TokenResponse = match serde_json::from_value(response.body) {
            Ok(v) => v,
            Err(_) => return,
        };
        // Both fields are required, absolute UTC timestamps (v5 section
        // 4.4). A malformed/missing value is rejected outright rather than
        // guessed at — never commit a credential whose expiry we could not
        // establish, and never log the response that carried it.
        let Ok(refresh_expires_at_unix) = parse_rfc3339_unix(&parsed.refresh_expires_at) else {
            return;
        };
        let Ok(refresh_absolute_expires_at_unix) =
            parse_rfc3339_unix(&parsed.refresh_absolute_expires_at)
        else {
            return;
        };
        let now_instant = self.clock.now_instant();
        let refresh = StoredRefresh {
            refresh_token: parsed.refresh_token,
            refresh_expires_at_unix,
            refresh_absolute_expires_at_unix,
            account: StoredAccount {
                nickname: parsed.account.nickname,
                clan_tag: parsed.account.clan_tag,
                realm: parsed.account.realm,
            },
        };
        let lifetime = Duration::from_secs(parsed.expires_in.max(1));
        let renew_in = lifetime.mul_f32(0.5);

        let mut inner = self.inner.lock().unwrap();
        if inner.login_flow.current_generation() != generation {
            // Stale: cancelled, replaced by a new attempt, or logged out
            // while the network call was outstanding. Never commit it.
            return;
        }
        let Some(device_id) = inner.device_id.clone() else { return };
        let Some(device_key) = inner.device_key.clone() else { return };
        let scalar_b64 = URL_SAFE_NO_PAD.encode(device_key.to_scalar_bytes());
        if self
            .persist_new_refresh(refresh.clone(), &device_id, &scalar_b64)
            .is_err()
        {
            // "Initial exchange store failure stays disconnected and
            // requires fresh login" — never show Connected here.
            inner.phase = Phase::StoreUnavailable;
            return;
        }
        if inner.session.connect(now_instant, lifetime, renew_in) {
            inner.access_token = Some((parsed.access_token, now_instant + lifetime));
            inner.account_display = Some(refresh.account);
            inner.phase = Phase::Idle;
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
        if let Ok(bytes) = base64_decode_32(&existing.device_key_scalar_b64) {
            if let Ok(key) = DeviceKey::from_scalar_bytes(&bytes) {
                let mut inner = self.inner.lock().unwrap();
                inner.device_key = Some(key);
                inner.device_id = existing.device_id.clone();
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

    async fn try_refresh(&self, refresh: StoredRefresh) -> bool {
        let Some(device_id) = self.inner.lock().unwrap().device_id.clone() else {
            return false;
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
        let parsed: TokenResponse = match serde_json::from_value(response.body) {
            Ok(v) => v,
            Err(_) => return false,
        };
        // Same required, absolute-UTC parsing as the initial exchange (v5
        // section 4.4) — never re-derive either expiry from "now" here,
        // which is exactly what let a refresh silently reset the 365-day
        // absolute cap.
        let Ok(refresh_expires_at_unix) = parse_rfc3339_unix(&parsed.refresh_expires_at) else {
            return false;
        };
        let Ok(refresh_absolute_expires_at_unix) =
            parse_rfc3339_unix(&parsed.refresh_absolute_expires_at)
        else {
            return false;
        };
        let now_instant = self.clock.now_instant();
        let new_refresh = StoredRefresh {
            refresh_token: parsed.refresh_token,
            refresh_expires_at_unix,
            refresh_absolute_expires_at_unix,
            account: StoredAccount {
                nickname: parsed.account.nickname,
                clan_tag: parsed.account.clan_tag,
                realm: parsed.account.realm,
            },
        };
        let lifetime = Duration::from_secs(parsed.expires_in.max(1));
        let renew_in = lifetime.mul_f32(0.5);
        let device_key = match self.inner.lock().unwrap().device_key.clone() {
            Some(k) => k,
            None => return false,
        };
        let scalar_b64 = URL_SAFE_NO_PAD.encode(device_key.to_scalar_bytes());
        if self
            .persist_new_refresh(new_refresh.clone(), &device_id, &scalar_b64)
            .is_err()
        {
            return false;
        }
        let mut inner = self.inner.lock().unwrap();
        if inner.session.connect(now_instant, lifetime, renew_in) {
            inner.access_token = Some((parsed.access_token, now_instant + lifetime));
            inner.account_display = Some(new_refresh.account);
            inner.phase = Phase::Idle;
        }
        true
    }

    // ── Revoke / invalidate primitives (not exposed as commands here) ──────

    /// Best-effort remote revoke. A reusable primitive for later tasks
    /// (td-96a620's debug reset, td-cab09a's revoked-credential handling) —
    /// not wired to any IPC command in this task.
    #[allow(dead_code)]
    pub async fn revoke_device(&self, reason: &str) -> Result<(), AccountError> {
        let (device_id, access_token) = {
            let inner = self.inner.lock().unwrap();
            (inner.device_id.clone(), inner.access_token.clone().map(|(t, _)| t))
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
        let response = self.call("POST", "/revoke", body, access_token.as_deref()).await?;
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
    #[allow(dead_code)]
    pub fn invalidate_local(&self) {
        let mut inner = self.inner.lock().unwrap();
        inner.login_flow.cancel();
        inner.session.disconnect();
        inner.account_display = None;
        inner.access_token = None;
        inner.phase = Phase::Idle;
        drop(inner);
        let _ = self.store.clear_profile(self.profile);
    }

    // ── DPoP-signed call with one nonce-retry boundary ──────────────────────

    async fn call(
        &self,
        htm: &str,
        path: &str,
        body: serde_json::Value,
        access_token: Option<&str>,
    ) -> Result<HttpResponse, AccountError> {
        let device_key = self.ensure_device_key()?;
        let htu = format!("{}{}", super::http::PRODUCTION_BASE_URL, path);
        let response = self
            .transport
            .post_json(path, self.build_headers(&device_key, htm, &htu, access_token, None), body.clone())
            .await?;
        if response.status == 401 {
            let use_nonce = response
                .header("www-authenticate")
                .is_some_and(|v| v.contains("use_dpop_nonce"));
            if let (true, Some(nonce)) = (use_nonce, response.header("dpop-nonce").map(str::to_string)) {
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
        let proof = bridge_core::dpop::build_proof(device_key, htm, htu, iat, &jti, nonce, ath.as_deref())
            .expect("htu is built from a constant base + path, never a query/fragment");
        // Explicitly set here (not only relying on the shared client's
        // defaults) so both the real transport and a scripted test double
        // carry the mandatory native headers (contract v5 section 4).
        let mut headers = vec![
            ("DPoP".to_string(), proof),
            (crate::engine::VERSION_HEADER.to_string(), crate::engine::BRIDGE_VERSION.to_string()),
            ("User-Agent".to_string(), crate::engine::user_agent(crate::engine::BRIDGE_VERSION)),
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
    AccountError::Http { status: response.status, error }
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
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    use std::sync::Mutex as StdMutex;

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
        fn load_profile(&self, profile: ClientProfile) -> Result<Option<StoredProfileData>, StoreError> {
            if self.unsupported.load(Ordering::SeqCst) {
                return Err(StoreError::UnsupportedPlatform);
            }
            if self.fail_load.load(Ordering::SeqCst) {
                return Err(StoreError::Backend("forced load failure".into()));
            }
            Ok(self.profiles.lock().unwrap().get(profile.as_str()).cloned())
        }
        fn save_profile(&self, profile: ClientProfile, data: &StoredProfileData) -> Result<(), StoreError> {
            if self.unsupported.load(Ordering::SeqCst) {
                return Err(StoreError::UnsupportedPlatform);
            }
            let call_number = self.save_calls.fetch_add(1, Ordering::SeqCst) + 1;
            if *self.fail_save_on_call.lock().unwrap() == Some(call_number) {
                return Err(StoreError::Backend("forced save failure".into()));
            }
            self.profiles.lock().unwrap().insert(profile.as_str(), data.clone());
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
        fn push_ok_with_headers(&self, status: u16, headers: Vec<(String, String)>, body: serde_json::Value) {
            self.responses.lock().unwrap().push_back(Ok(HttpResponse { status, headers, body }));
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
        Fixture { service, transport, store, browser, clock }
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
        assert_eq!(opened[0], "https://engine.tfd.rocks/auth/bridge?request_id=areq_1");

        let calls = f.transport.calls();
        assert_eq!(calls[0].path, "/device");
        assert_eq!(calls[1].path, "/authorize");
        // Every native call carries the mandatory version headers (contract
        // v5 section 4) and a DPoP proof; neither call carries a bearer
        // token (no access token exists yet).
        for call in &calls {
            assert!(call.headers.iter().any(|(k, _)| k == "DPoP"));
            assert!(call
                .headers
                .iter()
                .any(|(k, v)| k == crate::engine::VERSION_HEADER && v == crate::engine::BRIDGE_VERSION));
            assert!(call.headers.iter().any(|(k, _)| k == "User-Agent"));
            assert!(!call.headers.iter().any(|(k, _)| k == "Authorization"));
        }
        assert_eq!(calls[1].body["redirect_uri"], "http://127.0.0.1:43210/auth/callback");
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
            transport, store, browser, FixedClock::new(), CountingRandom::new(), ClientProfile::Release,
        ));
        assert_eq!(service.begin_login().await, Err(AccountError::NotReady));
        let _ = f;
    }

    #[tokio::test]
    async fn begin_login_rejects_a_malicious_or_out_of_contract_authorization_url() {
        let malicious_urls = [
            "http://engine.tfd.rocks/auth/bridge?request_id=1",       // wrong scheme
            "https://evil.example/auth/bridge?request_id=1",          // wrong host entirely
            "https://engine.tfd.rocks.evil.example/auth/bridge",      // host-confusable suffix
            "https://evil.example/?engine.tfd.rocks/auth/bridge",     // host-confusable prefix trick
            "https://engine.tfd.rocks:8443/auth/bridge?request_id=1", // non-default port
            "https://engine.tfd.rocks/auth/other?request_id=1",       // wrong path
            "https://engine.tfd.rocks/auth/bridge/",                  // trailing-slash path variant
            "https://user:pass@engine.tfd.rocks/auth/bridge",         // embedded credentials
            "https://engine.tfd.rocks/auth/bridge#fragment",          // fragment
            "not a url at all",                                       // unparseable
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
            assert_eq!(f.service.reopen_pending(), Err(AccountError::NoPendingLogin), "url: {url}");
        }
    }

    #[tokio::test]
    async fn browser_open_failure_is_visible_and_reopen_retries_the_same_url_without_a_second_authorize_call() {
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
        assert_eq!(opened[0], opened[1], "reopen must use the exact same stored URL");
        // No second /authorize call was made for the reopen.
        assert_eq!(f.transport.calls().len(), 2);
    }

    #[tokio::test]
    async fn reopen_without_any_pending_login_fails() {
        let f = make_fixture(true);
        assert_eq!(f.service.reopen_pending(), Err(AccountError::NoPendingLogin));
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
        assert_eq!(f.service.reopen_pending(), Err(AccountError::NoPendingLogin));
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
        assert_eq!(f.service.reopen_pending(), Err(AccountError::NoPendingLogin));

        let state = f.transport.calls()[1].body["state"].as_str().unwrap().to_string();
        let ack = AccountService::on_callback(
            &f.service,
            AuthCallback { state, outcome: AuthCallbackOutcome::Code("late-code".into()) },
        );
        assert_eq!(ack, AuthCallbackAck::Rejected);
    }

    // ── Callback handling ────────────────────────────────────────────────────

    async fn begin_and_get_state(f: &Fixture) -> String {
        f.transport.push_ok(201, device_response());
        f.transport.push_ok(200, authorize_response());
        f.service.begin_login().await.unwrap();
        f.transport.calls()[1].body["state"].as_str().unwrap().to_string()
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
            AuthCallback { state, outcome: AuthCallbackOutcome::Code("test-code".into()) },
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
        assert_eq!(AccountService::on_callback(&f.service, callback()), AuthCallbackAck::Pending);
        // A second, "concurrent" callback for the same already-consumed
        // attempt can only ever be Rejected — the exchange it could trigger
        // therefore happens at most once.
        assert_eq!(AccountService::on_callback(&f.service, callback()), AuthCallbackAck::Rejected);
    }

    #[tokio::test]
    async fn unknown_state_is_rejected_and_does_not_disturb_the_real_pending_attempt() {
        let f = make_fixture(true);
        let state = begin_and_get_state(&f).await;
        let ack = AccountService::on_callback(
            &f.service,
            AuthCallback { state: "totally-unrelated-guess".into(), outcome: AuthCallbackOutcome::Code("x".into()) },
        );
        assert_eq!(ack, AuthCallbackAck::Rejected);
        assert_eq!(f.service.status(), AccountStatus::AwaitingBrowser);
        // The real attempt is still reopenable.
        assert!(f.service.reopen_pending().is_ok());
        // ... and still consumable with its real state.
        let ack2 = AccountService::on_callback(
            &f.service,
            AuthCallback { state, outcome: AuthCallbackOutcome::Code("real-code".into()) },
        );
        assert_eq!(ack2, AuthCallbackAck::Pending);
    }

    #[tokio::test]
    async fn distinct_terminal_outcomes_map_to_distinct_statuses_and_acks() {
        for (error, expected_status, expected_ack) in [
            (AuthCallbackOutcome::AccessDenied, AccountStatus::Denied, AuthCallbackAck::Denied),
            (
                AuthCallbackOutcome::AccountNotAuthorized,
                AccountStatus::AccountNotAuthorized,
                AuthCallbackAck::AccountNotAuthorized,
            ),
            (AuthCallbackOutcome::ExpiredRequest, AccountStatus::Expired, AuthCallbackAck::Expired),
        ] {
            let f = make_fixture(true);
            let state = begin_and_get_state(&f).await;
            let ack = AccountService::on_callback(&f.service, AuthCallback { state, outcome: error });
            assert_eq!(ack, expected_ack);
            assert_eq!(f.service.status(), expected_status);
        }
    }

    #[tokio::test]
    async fn a_late_callback_past_its_own_deadline_is_reported_as_expired_not_a_generic_rejection() {
        let f = make_fixture(true);
        let state = begin_and_get_state(&f).await;
        f.clock.advance(301); // past the 300s pending-attempt TTL
        let ack = AccountService::on_callback(
            &f.service,
            AuthCallback { state, outcome: AuthCallbackOutcome::Code("late-code".into()) },
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
        Arc::clone(&f.service).complete_exchange(code.to_string(), consumed).await;
    }

    #[tokio::test]
    async fn full_round_trip_connects_and_persists_two_slot_refresh() {
        let f = make_fixture(true);
        let state = begin_and_get_state(&f).await;
        f.transport.push_ok(200, token_response("refresh-abc"));

        consume_and_exchange(&f, state, "auth-code").await;

        match f.service.status() {
            AccountStatus::Connected { nickname, clan_tag, realm } => {
                assert_eq!(nickname, "FrankDrake");
                assert_eq!(clan_tag.as_deref(), Some("TFX"));
                assert_eq!(realm.as_deref(), Some("eu"));
            }
            other => panic!("expected Connected, got {other:?}"),
        }

        let stored = f.store.profiles.lock().unwrap().get("release").cloned().unwrap();
        assert_eq!(stored.refresh_current.as_ref().unwrap().refresh_token, "refresh-abc");
        assert!(stored.refresh_pending.is_none(), "must be promoted, not left pending");
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
                b.as_object_mut().unwrap().remove("refresh_absolute_expires_at");
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
    async fn refresh_persists_the_engine_provided_absolute_cap_verbatim_never_recomputed_from_now() {
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
                    account: StoredAccount { nickname: "Old".into(), clan_tag: None, realm: None },
                }),
                refresh_pending: None,
            },
        );
        f.transport.push_ok(200, token_response("refresh-rotated"));
        f.service.bootstrap().await;
        assert!(matches!(f.service.status(), AccountStatus::Connected { .. }));

        let stored = f.store.profiles.lock().unwrap().get("release").cloned().unwrap();
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
        f.transport.push_ok(200, token_response("refresh-should-not-land"));

        Arc::clone(&f.service).complete_exchange("auth-code".into(), consumed).await;

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
            f.store.profiles.lock().unwrap().get("release").and_then(|d| d.refresh_current.clone()).is_none(),
            "no credential must be committed on a store failure"
        );
    }

    #[tokio::test]
    async fn secret_fields_never_appear_in_a_debug_or_serialized_status() {
        let f = make_fixture(true);
        let state = begin_and_get_state(&f).await;
        f.transport.push_ok(200, token_response("super-secret-refresh-canary"));
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
        f.transport.push_ok(200, token_response("refresh-crash-recovery"));
        // Fail exactly the promotion write (the 4th save_profile call: 1 key
        // persist + 1 device_id persist + 1 pending write + [fails] promote).
        *f.store.fail_save_on_call.lock().unwrap() = Some(4);

        consume_and_exchange(&f, state, "auth-code").await;
        assert_eq!(f.service.status(), AccountStatus::StoreUnavailable);

        let stored = f.store.profiles.lock().unwrap().get("release").cloned().unwrap();
        assert!(stored.refresh_current.is_none(), "promotion must not have happened");
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
        assert_eq!(transport2.calls()[0].body["refresh_token"], "refresh-crash-recovery");
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
                    account: StoredAccount { nickname: "Old".into(), clan_tag: None, realm: None },
                }),
                refresh_pending: None,
            },
        );
        f.transport.push_ok(200, token_response("refresh-rotated"));
        f.service.bootstrap().await;
        assert!(matches!(f.service.status(), AccountStatus::Connected { .. }));
        assert_eq!(f.transport.calls()[0].body["refresh_token"], "refresh-current");
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
                    account: StoredAccount { nickname: "Old".into(), clan_tag: None, realm: None },
                }),
                refresh_pending: Some(StoredRefresh {
                    refresh_token: "refresh-pending-but-unsafe".into(),
                    refresh_expires_at_unix: f.clock.now_unix().as_secs() + 1000,
                    refresh_absolute_expires_at_unix: f.clock.now_unix().as_secs() + 100_000,
                    account: StoredAccount { nickname: "Old".into(), clan_tag: None, realm: None },
                }),
            },
        );
        // The Engine rejects `current` outright (e.g. reuse/theft detected —
        // the whole family, `pending` included, is revoked server-side).
        f.transport.push_ok(400, json!({"error":"invalid_grant","error_description":"reuse detected","request_id":"r1"}));
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
                    account: StoredAccount { nickname: "Old".into(), clan_tag: None, realm: None },
                }),
                refresh_pending: None,
            },
        );
        f.service.bootstrap().await;
        assert_eq!(f.service.status(), AccountStatus::SignedOut);
        assert!(f.transport.calls().is_empty(), "an absolutely-expired refresh must never be sent to the Engine");
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

        f.transport.push_ok(200, token_response("refresh-from-stale-attempt"));
        Arc::clone(&f.service).complete_exchange("stale-code".into(), consumed_1).await;

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
        assert_eq!(calls.len(), 3, "the 401 attempt plus exactly one retry, then the next real call");
        let first_proof = calls[0].headers.iter().find(|(k, _)| k == "DPoP").unwrap().1.clone();
        let retry_proof = calls[1].headers.iter().find(|(k, _)| k == "DPoP").unwrap().1.clone();
        assert_ne!(first_proof, retry_proof, "the retry must rebuild the proof (fresh jti/iat), never reuse it verbatim");
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
        f.transport.push_ok(200, token_response("refresh-to-be-cleared"));
        consume_and_exchange(&f, state, "auth-code").await;
        assert!(matches!(f.service.status(), AccountStatus::Connected { .. }));

        f.service.invalidate_local();
        assert_eq!(f.service.status(), AccountStatus::SignedOut);
        assert!(f.store.profiles.lock().unwrap().get("release").is_none());
    }

    #[tokio::test]
    async fn profile_separation_release_and_debug_never_share_credentials() {
        let transport = ScriptedTransport::new();
        let store = Arc::new(InMemoryStore::default());
        let release = Arc::new(AccountService::new(
            transport.clone(), store.clone(), RecordingBrowser::new(true), FixedClock::new(), CountingRandom::new(), ClientProfile::Release,
        ));
        let debug = Arc::new(AccountService::new(
            transport.clone(), store.clone(), RecordingBrowser::new(true), FixedClock::new(), CountingRandom::new(), ClientProfile::Debug,
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

}
