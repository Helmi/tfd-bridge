//! Injected boundaries for [`super::AccountService`]: HTTP transport, secure
//! storage, the OS browser opener, the clock and randomness. Every real
//! implementation lives next to its trait; tests use small deterministic
//! doubles instead of touching the network, the OS credential store or a
//! real browser.
use std::time::{Duration, SystemTime};

// ── Clock / randomness ───────────────────────────────────────────────────────

/// Injected wall-clock and monotonic time source.
pub trait Clock: Send + Sync {
    fn now_unix(&self) -> Duration;
    fn now_instant(&self) -> std::time::Instant;
}

pub struct SystemClock;
impl Clock for SystemClock {
    fn now_unix(&self) -> Duration {
        SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap_or_default()
    }
    fn now_instant(&self) -> std::time::Instant {
        std::time::Instant::now()
    }
}

/// Injected randomness. Production fills from the OS CSPRNG; tests supply
/// fixed or scripted bytes so PKCE/state/device-key generation is
/// deterministic and reproducible.
pub trait SecureRandom: Send + Sync {
    fn fill(&self, buf: &mut [u8]);
}

pub struct OsRandom;
impl SecureRandom for OsRandom {
    fn fill(&self, buf: &mut [u8]) {
        use rand::RngCore;
        rand::rngs::OsRng.fill_bytes(buf);
    }
}

// ── HTTP transport ───────────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TransportError {
    Network,
    Timeout,
    ResponseTooLarge,
    InvalidResponse,
}

/// One already-parsed HTTP response: status, header name/value pairs
/// (lower-cased names), and a best-effort JSON body (`Value::Null` for an
/// empty body). `AccountService` builds the DPoP-signed request and
/// interprets the typed contract schema on top of this; the transport itself
/// only needs to move bytes reliably and enforce the bounded-response-body
/// contract requirement (v5 section 2).
#[derive(Clone)]
pub struct HttpResponse {
    pub status: u16,
    pub headers: Vec<(String, String)>,
    pub body: serde_json::Value,
}

impl HttpResponse {
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
    }
}

/// Typed enough for one job: POST a JSON body with caller-built headers
/// (including the DPoP proof and, when present, the bearer access token) to
/// a path relative to the bridge-auth base, and get back a parsed response.
/// `AccountService` owns every endpoint's exact request/response schema, the
/// nonce-retry boundary and error mapping — kept here so those are testable
/// against a scripted `AccountTransport` double without any network or TLS.
#[async_trait::async_trait]
pub trait AccountTransport: Send + Sync {
    async fn post_json(
        &self,
        path: &str,
        headers: Vec<(String, String)>,
        body: serde_json::Value,
    ) -> Result<HttpResponse, TransportError>;
}

// ── Browser launcher ─────────────────────────────────────────────────────────

/// Opens a URL in the OS default browser, bypassing any in-app Engine-link
/// preference. Never accepts a caller-supplied URL from IPC — `AccountService`
/// only ever passes the exact `authorization_url` it just received from
/// `/authorize`, or the one already stored in the pending login attempt.
pub trait BrowserLauncher: Send + Sync {
    fn open(&self, url: &str) -> bool;
}

// ── Secure credential store ──────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StoreError {
    /// No real backend exists on this platform/build (explicit failure, no
    /// mock, no plaintext fallback).
    UnsupportedPlatform,
    /// Read/write/delete against the OS store failed.
    Backend(String),
    /// Stored bytes did not parse as the expected schema (corrupt entry).
    Corrupt,
    /// A value exceeds the store backend's credential-size limit
    /// (contract: "Validate Windows credential size").
    ValueTooLarge,
}

/// `release` and `debug` are separate credential namespaces: a debug reset
/// can never revoke or overwrite release credentials, and the reverse is
/// forbidden (contract v5 section 4.2/4.5). Chosen once per process from
/// `cfg!(debug_assertions)` — never selectable from JS/IPC.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ClientProfile {
    Release,
    Debug,
}

impl ClientProfile {
    pub fn current() -> Self {
        if cfg!(debug_assertions) {
            Self::Debug
        } else {
            Self::Release
        }
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Release => "release",
            Self::Debug => "debug",
        }
    }
}

/// One refresh-chain slot (contract v5 section 5.8's recommended
/// `current`/`pending` shape). Never holds an access token.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct StoredRefresh {
    pub refresh_token: String,
    pub refresh_expires_at_unix: u64,
    pub refresh_absolute_expires_at_unix: u64,
    pub account: StoredAccount,
}

/// Non-secret account display fields, cached so a restart can show "safe
/// status" before any network round trip.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct StoredAccount {
    pub nickname: String,
    pub clan_tag: Option<String>,
    pub realm: Option<String>,
}

/// The versioned, per-`ClientProfile` durable record: device key, the
/// Engine-assigned `device_id` once registered, and the two refresh slots.
/// Never an access token (contract: "Access tokens remain memory-only").
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct StoredProfileData {
    pub schema_version: u32,
    pub device_key_scalar_b64: String,
    pub device_id: Option<String>,
    pub refresh_current: Option<StoredRefresh>,
    pub refresh_pending: Option<StoredRefresh>,
}

pub const STORED_PROFILE_SCHEMA_VERSION: u32 = 1;

/// Injected secure storage. Real implementation: the OS credential store
/// (`store.rs`, `keyring` windows-native). No mock production backend and no
/// plaintext fallback — an unsupported platform is a hard `StoreError`.
pub trait SecretStore: Send + Sync {
    /// Stable per-install identifier, created on first use and independent
    /// of `ClientProfile` (contract: "stable install_id survives reset").
    fn install_id(&self) -> Result<String, StoreError>;
    fn load_profile(&self, profile: ClientProfile) -> Result<Option<StoredProfileData>, StoreError>;
    fn save_profile(&self, profile: ClientProfile, data: &StoredProfileData) -> Result<(), StoreError>;
    fn clear_profile(&self, profile: ClientProfile) -> Result<(), StoreError>;
}
