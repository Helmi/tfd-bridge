//! Real `AccountTransport`: reqwest against the frozen v5 bridge-auth base
//! (`https://engine.tfd.rocks/api/v1/bridge/auth`). Reuses `engine.rs`'s
//! version/User-Agent header helpers verbatim, but deliberately does NOT
//! share `engine::shared_client()` — native auth calls need a redirect
//! policy and connect timeout the other (config-refresh/donation) engine
//! traffic does not, and this keeps that hardening scoped to auth alone.
use super::traits::{AccountTransport, HttpResponse, TransportError};
use async_trait::async_trait;
use std::sync::OnceLock;

/// Production base for every native bridge-auth call.
pub const PRODUCTION_BASE_URL: &str = "https://engine.tfd.rocks/api/v1/bridge/auth";

/// Contract v5 section 2: "HTTP connect 10 s, read 30 s".
const CONNECT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);
const REQUEST_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);

/// Defensive cap on a response body. The real Engine responses are small
/// JSON objects (well under 4 KiB); this exists so a misbehaving or
/// compromised endpoint cannot make the Bridge buffer an unbounded amount of
/// memory (contract: "bounded response body").
const MAX_RESPONSE_BYTES: usize = 64 * 1024;

/// The auth-only HTTP client: version headers, a bounded connect/read
/// timeout, and — critically — no redirect following. A compromised or
/// misrouted Engine answering `POST /token` with a redirect must never have
/// this client replay a refresh-token request body at another origin.
fn auth_client() -> &'static reqwest::Client {
    static CLIENT: OnceLock<reqwest::Client> = OnceLock::new();
    CLIENT.get_or_init(|| {
        // Mirrors `engine::build_client`'s one-time crypto-provider install;
        // idempotent (first caller wins) so it is safe regardless of
        // whether `engine::shared_client()` has already run.
        if rustls::crypto::CryptoProvider::get_default().is_none() {
            let _ = rustls::crypto::ring::default_provider().install_default();
        }
        reqwest::Client::builder()
            .default_headers(crate::engine::default_headers(crate::engine::BRIDGE_VERSION))
            .user_agent(crate::engine::user_agent(crate::engine::BRIDGE_VERSION))
            .redirect(reqwest::redirect::Policy::none())
            .connect_timeout(CONNECT_TIMEOUT)
            .timeout(REQUEST_TIMEOUT)
            .build()
            .expect("static auth client config must be valid")
    })
}

pub struct ReqwestTransport {
    base_url: String,
}

impl ReqwestTransport {
    pub fn production() -> Self {
        Self {
            base_url: PRODUCTION_BASE_URL.to_string(),
        }
    }

    /// Test-only: point at a local mock server instead of the real Engine.
    #[cfg(test)]
    pub fn with_base_url(base_url: String) -> Self {
        Self { base_url }
    }
}

#[async_trait]
impl AccountTransport for ReqwestTransport {
    async fn post_json(
        &self,
        path: &str,
        headers: Vec<(String, String)>,
        body: serde_json::Value,
    ) -> Result<HttpResponse, TransportError> {
        let url = format!("{}{}", self.base_url, path);
        let mut builder = auth_client().post(&url).json(&body);
        for (name, value) in &headers {
            builder = builder.header(name.as_str(), value.as_str());
        }
        let response = builder.send().await.map_err(|e| {
            if e.is_timeout() {
                TransportError::Timeout
            } else {
                TransportError::Network
            }
        })?;
        let status = response.status().as_u16();
        let response_headers: Vec<(String, String)> = response
            .headers()
            .iter()
            .filter_map(|(name, value)| {
                value
                    .to_str()
                    .ok()
                    .map(|v| (name.as_str().to_ascii_lowercase(), v.to_string()))
            })
            .collect();

        // Bounded read: accumulate chunks and abort as soon as the cap would
        // be exceeded, rather than trusting a (spoofable, or simply absent)
        // Content-Length header.
        let mut response = response;
        let mut bytes = Vec::new();
        while let Some(chunk) = response
            .chunk()
            .await
            .map_err(|_| TransportError::Network)?
        {
            if bytes.len() + chunk.len() > MAX_RESPONSE_BYTES {
                return Err(TransportError::ResponseTooLarge);
            }
            bytes.extend_from_slice(&chunk);
        }

        let body = if bytes.is_empty() {
            serde_json::Value::Null
        } else {
            serde_json::from_slice(&bytes).map_err(|_| TransportError::InvalidResponse)?
        };

        Ok(HttpResponse {
            status,
            headers: response_headers,
            body,
        })
    }
}

// ── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    /// Contract v5 section 2's connect/read timeouts and (critically) the
    /// no-redirect policy: a compromised/misrouted Engine answering `POST
    /// /token` (or any auth call) with a redirect must never have this
    /// client chase it and replay the request body — including a
    /// refresh/authorization-code grant — at another origin. Mirrors the
    /// existing `video_share.rs::redirects_do_not_forward_capability`
    /// pattern (a local `tiny_http` mock, no live network).
    #[tokio::test]
    async fn post_json_does_not_follow_a_redirect_response() {
        let server = tiny_http::Server::http("127.0.0.1:0").expect("bind mock engine");
        let port = server
            .server_addr()
            .to_ip()
            .expect("mock engine binds a TCP socket")
            .port();
        let worker = std::thread::spawn(move || {
            let request = server.recv().expect("mock engine receives one request");
            request
                .respond(
                    tiny_http::Response::empty(307).with_header(
                        tiny_http::Header::from_bytes("Location", "https://attacker.example/steal")
                            .unwrap(),
                    ),
                )
                .unwrap();
        });

        let transport = ReqwestTransport::with_base_url(format!("http://127.0.0.1:{port}"));
        let result = transport
            .post_json("/token", Vec::new(), serde_json::json!({"grant_type": "refresh_token"}))
            .await
            .expect("the redirect response itself is a normal, well-formed HTTP response");

        // The 307 is surfaced as-is — never followed to the attacker's
        // Location, and no body is replayed there.
        assert_eq!(result.status, 307);
        worker.join().unwrap();
    }
}
