//! Real `AccountTransport`: reqwest against the frozen v5 bridge-auth base
//! (`https://engine.tfd.rocks/api/v1/bridge/auth`). Reuses `engine.rs`'s
//! shared client and version/User-Agent header helpers verbatim — this
//! module only adds the per-request DPoP/Authorization headers and the
//! bounded-response-body contract requirement (v5 section 2).
use super::traits::{AccountTransport, HttpResponse, TransportError};
use async_trait::async_trait;

/// Production base for every native bridge-auth call.
pub const PRODUCTION_BASE_URL: &str = "https://engine.tfd.rocks/api/v1/bridge/auth";

/// Client read timeout ceiling from the contract (v5 section 2): "HTTP
/// connect 10 s, read 30 s". `engine::shared_client()` already applies a
/// single overall request timeout via reqwest's `.timeout(...)`; 30 s here
/// matches the contract's read bound (connect is a strict subset of it).
const REQUEST_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);

/// Defensive cap on a response body. The real Engine responses are small
/// JSON objects (well under 4 KiB); this exists so a misbehaving or
/// compromised endpoint cannot make the Bridge buffer an unbounded amount of
/// memory (contract: "bounded response body").
const MAX_RESPONSE_BYTES: usize = 64 * 1024;

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
        let mut builder = crate::engine::shared_client()
            .post(&url)
            .timeout(REQUEST_TIMEOUT)
            .json(&body);
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
