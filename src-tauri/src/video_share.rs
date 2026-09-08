//! Native client for Engine's minimap sharing v2 contract.
//! Credentials stay native; no browser-supplied API or storage base URLs.
use crate::video_upload::{PreparedVideo, UploadError, UploadTicket};
use reqwest::{Client, Method, Url};
use serde::{de::DeserializeOwned, Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    time::{Duration, SystemTime},
};

const HOSTS: [&str; 2] = [
    "tfd-internal-backups.nbg1.your-objectstorage.com",
    "nbg1.your-objectstorage.com",
];

#[derive(Debug, PartialEq, Eq)]
pub enum ShareError {
    InvalidAuthorization,
    Expired,
    InvalidResponse,
    Transport,
    Http(u16),
    Upload(UploadError),
}

/// Deliberately not Debug or Serialize: contains the scoped Engine credential.
pub struct AuthorizedShare {
    id: String,
    capability: String,
    expires_at: SystemTime,
    max_bytes: u64,
}

impl AuthorizedShare {
    pub fn new(
        id: String,
        capability: String,
        expires_at: SystemTime,
        max_bytes: u64,
    ) -> Result<Self, ShareError> {
        if id.is_empty()
            || id.len() > 128
            || !id
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
            || capability.is_empty()
            || capability.len() > 4096
            || !capability.bytes().all(|b| (33..=126).contains(&b))
            || max_bytes == 0
            || max_bytes > 512 * 1024 * 1024
        {
            return Err(ShareError::InvalidAuthorization);
        }
        let share = Self {
            id,
            capability,
            expires_at,
            max_bytes,
        };
        share.check_expiry()?;
        Ok(share)
    }

    fn check_expiry(&self) -> Result<(), ShareError> {
        if SystemTime::now() >= self.expires_at {
            Err(ShareError::Expired)
        } else {
            Ok(())
        }
    }
}

#[derive(Deserialize)]
struct TicketResponse {
    upload: TicketFields,
    attempt: u8,
    max_upload_attempts: u8,
}
#[derive(Deserialize)]
struct TicketFields {
    method: String,
    url: String,
    host: String,
    headers: BTreeMap<String, serde_json::Value>,
    expires_at: String,
}

impl TicketResponse {
    fn validate(self, bytes: u64, max_bytes: u64) -> Result<UploadTicket, ShareError> {
        let u = self.upload;
        let url = Url::parse(&u.url).map_err(|_| ShareError::InvalidResponse)?;
        let content_length = u.headers.get("Content-Length").and_then(|v| {
            v.as_u64()
                .or_else(|| v.as_str().and_then(|s| s.parse().ok()))
        });
        if bytes > max_bytes
            || self.attempt == 0
            || self.attempt > self.max_upload_attempts
            || self.max_upload_attempts > 5
            || u.method != "PUT"
            || !HOSTS.contains(&u.host.as_str())
            || url.host_str() != Some(u.host.as_str())
            || u.headers.len() != 2
            || content_length != Some(bytes)
            || u.headers.get("Content-Type").and_then(|v| v.as_str()) != Some("video/mp4")
        {
            return Err(ShareError::InvalidResponse);
        }
        let expiry = chrono::DateTime::parse_from_rfc3339(&u.expires_at)
            .map_err(|_| ShareError::InvalidResponse)?;
        UploadTicket::validate(&u.url, max_bytes, expiry.into(), &HOSTS.map(str::to_owned))
            .map_err(ShareError::Upload)
    }
}

#[derive(Clone, Copy, Serialize)]
pub struct VideoMetadata {
    pub duration_s: f64,
    pub width: u32,
    pub height: u32,
}

#[derive(Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum DeliveryState {
    Authorized,
    Uploading,
    Queued,
    Posting,
    Posted,
    Failed,
    Expired,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct DeliveryStatus {
    pub status: DeliveryState,
    pub attempt: u8,
    pub message_url: Option<String>,
    pub error: Option<String>,
    pub capability_expires_at: Option<String>,
}

pub struct VideoShareClient {
    client: Client,
    base: Url,
}

impl VideoShareClient {
    pub fn new() -> Result<Self, ShareError> {
        let _ = rustls::crypto::ring::default_provider().install_default();
        let client = Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .timeout(Duration::from_secs(30))
            .user_agent(concat!("TFD-Bridge/", env!("CARGO_PKG_VERSION")))
            .build()
            .map_err(|_| ShareError::Transport)?;
        Ok(Self {
            client,
            base: Url::parse("https://engine.tfd.rocks/").unwrap(),
        })
    }

    async fn request(
        &self,
        share: &AuthorizedShare,
        method: Method,
        suffix: &str,
        body: Option<serde_json::Value>,
    ) -> Result<reqwest::Response, ShareError> {
        // Engine decides whether a terminal token still has status scope.
        // Allow the expiry grace period for reads only; never for mutations.
        if method == Method::GET && suffix.is_empty() {
            if SystemTime::now() >= share.expires_at + Duration::from_secs(15 * 60) {
                return Err(ShareError::Expired);
            }
        } else {
            share.check_expiry()?;
        }
        let url = self
            .base
            .join(&format!("api/discord/share/video/{}{suffix}", share.id))
            .map_err(|_| ShareError::InvalidAuthorization)?;
        let mut request = self
            .client
            .request(method, url)
            .bearer_auth(&share.capability)
            .header("X-TFD-Bridge-Version", env!("CARGO_PKG_VERSION"));
        if let Some(body) = body {
            request = request.json(&body);
        }
        let response = request.send().await.map_err(|_| ShareError::Transport)?;
        if !response.status().is_success() {
            return Err(ShareError::Http(response.status().as_u16()));
        }
        Ok(response)
    }

    async fn json<T: DeserializeOwned>(mut response: reqwest::Response) -> Result<T, ShareError> {
        // Bound even chunked replies; never retain arbitrary remote bodies in errors.
        let mut bytes = Vec::new();
        while let Some(chunk) = response.chunk().await.map_err(|_| ShareError::Transport)? {
            if bytes.len() + chunk.len() > 16_384 {
                return Err(ShareError::InvalidResponse);
            }
            bytes.extend_from_slice(&chunk);
        }
        serde_json::from_slice(&bytes).map_err(|_| ShareError::InvalidResponse)
    }

    /// Call only after local rendering completes. Each call consumes one Engine
    /// ticket attempt; failures are returned rather than retried invisibly.
    pub async fn upload(
        &self,
        share: &AuthorizedShare,
        video: PreparedVideo,
        metadata: VideoMetadata,
    ) -> Result<(), ShareError> {
        if !metadata.duration_s.is_finite()
            || metadata.duration_s <= 0.0
            || metadata.width == 0
            || metadata.height == 0
            || video.receipt().bytes > share.max_bytes
        {
            return Err(ShareError::InvalidResponse);
        }
        let response = self
            .request(
                share,
                Method::POST,
                "/ticket",
                Some(serde_json::json!({
                    "bytes": video.receipt().bytes, "sha256": video.receipt().sha256
                })),
            )
            .await?;
        let ticket = Self::json::<TicketResponse>(response)
            .await?
            .validate(video.receipt().bytes, share.max_bytes)?;
        let receipt = video.upload(ticket).await.map_err(ShareError::Upload)?;
        self.complete(share, &receipt, metadata).await
    }

    /// Separately callable after an uncertain completion response, without
    /// requesting another ticket or uploading the file again.
    pub async fn complete(
        &self,
        share: &AuthorizedShare,
        receipt: &crate::video_upload::UploadReceipt,
        metadata: VideoMetadata,
    ) -> Result<(), ShareError> {
        if receipt.bytes == 0
            || receipt.bytes > share.max_bytes
            || receipt.sha256.len() != 64
            || !receipt.sha256.bytes().all(|b| b.is_ascii_hexdigit())
            || !metadata.duration_s.is_finite()
            || metadata.duration_s <= 0.0
            || metadata.width == 0
            || metadata.height == 0
        {
            return Err(ShareError::InvalidResponse);
        }
        self.request(
            share,
            Method::POST,
            "/complete",
            Some(serde_json::json!({
                "bytes":receipt.bytes,"sha256":receipt.sha256,
                "duration_s":metadata.duration_s,"width":metadata.width,"height":metadata.height
            })),
        )
        .await?;
        Ok(())
    }

    pub async fn status(&self, share: &AuthorizedShare) -> Result<DeliveryStatus, ShareError> {
        Self::json(self.request(share, Method::GET, "", None).await?).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn share() -> AuthorizedShare {
        AuthorizedShare::new(
            "share-123".into(),
            "private-token".into(),
            SystemTime::now() + Duration::from_secs(60),
            1024,
        )
        .unwrap()
    }
    fn ticket() -> serde_json::Value {
        serde_json::json!({"upload":{"method":"PUT","url":format!("https://{}/video?signature=secret",HOSTS[0]),"host":HOSTS[0],"headers":{"Content-Type":"video/mp4","Content-Length":24},"expires_at":"2099-01-01T00:00:00Z"},"attempt":1,"max_upload_attempts":5})
    }
    #[test]
    fn reject_credential_injection_and_path_escape() {
        for id in ["../other", "x?token=y", "x/y", ""] {
            assert!(AuthorizedShare::new(
                id.into(),
                "ok".into(),
                SystemTime::now() + Duration::from_secs(60),
                1024
            )
            .is_err());
        }
        assert!(AuthorizedShare::new(
            "ok".into(),
            "a\r\nb".into(),
            SystemTime::now() + Duration::from_secs(60),
            1024
        )
        .is_err());
    }
    #[test]
    fn validate_ticket_against_actual_file_and_fixed_hosts() {
        assert!(serde_json::from_value::<TicketResponse>(ticket())
            .unwrap()
            .validate(24, 1024)
            .is_ok());
        for (field, value) in [
            ("host", serde_json::json!(HOSTS[1])),
            ("method", serde_json::json!("POST")),
            ("url", serde_json::json!("https://evil.test/video")),
        ] {
            let mut t = ticket();
            t["upload"][field] = value;
            assert!(serde_json::from_value::<TicketResponse>(t)
                .unwrap()
                .validate(24, 1024)
                .is_err());
        }
        assert!(serde_json::from_value::<TicketResponse>(ticket())
            .unwrap()
            .validate(25, 1024)
            .is_err());
        let mut t = ticket();
        t["upload"]["headers"]["x-amz-acl"] = serde_json::json!("public-read");
        assert!(serde_json::from_value::<TicketResponse>(t)
            .unwrap()
            .validate(24, 1024)
            .is_err());
    }
    #[tokio::test]
    async fn completion_and_status_use_scoped_native_auth() {
        let server = tiny_http::Server::http("127.0.0.1:0").unwrap();
        let mut client = VideoShareClient::new().unwrap();
        client.base = Url::parse(&format!("http://{}/", server.server_addr())).unwrap();
        let worker = std::thread::spawn(move || {
            for path in [
                "/api/discord/share/video/share-123/complete",
                "/api/discord/share/video/share-123",
            ] {
                let mut req = server.recv().unwrap();
                assert_eq!(req.url(), path);
                assert!(req.headers().iter().any(|h| h.field.equiv("Authorization")
                    && h.value.as_str() == "Bearer private-token"));
                assert!(!req.headers().iter().any(|h| h.field.equiv("Cookie")));
                if path.ends_with("complete") {
                    let body: serde_json::Value = serde_json::from_reader(req.as_reader()).unwrap();
                    assert_eq!(body["bytes"], 24);
                    assert_eq!(body["duration_s"], 1.5);
                    req.respond(tiny_http::Response::empty(204)).unwrap();
                } else {
                    req.respond(tiny_http::Response::from_string(
                        r#"{"status":"queued","attempt":1}"#,
                    ))
                    .unwrap();
                }
            }
        });
        client
            .complete(
                &share(),
                &crate::video_upload::UploadReceipt {
                    bytes: 24,
                    sha256: "a".repeat(64),
                },
                VideoMetadata {
                    duration_s: 1.5,
                    width: 1920,
                    height: 1080,
                },
            )
            .await
            .unwrap();
        let mut expired = share();
        expired.expires_at = SystemTime::now() - Duration::from_secs(1);
        assert!(matches!(
            client
                .request(&expired, Method::POST, "/ticket", None)
                .await,
            Err(ShareError::Expired)
        ));
        assert_eq!(
            client.status(&expired).await.unwrap().status,
            DeliveryState::Queued
        );
        expired.expires_at = SystemTime::now() - Duration::from_secs(901);
        assert!(matches!(
            client.status(&expired).await,
            Err(ShareError::Expired)
        ));
        worker.join().unwrap();
    }
    #[tokio::test]
    async fn redirects_do_not_forward_capability() {
        let server = tiny_http::Server::http("127.0.0.1:0").unwrap();
        let mut client = VideoShareClient::new().unwrap();
        client.base = Url::parse(&format!("http://{}/", server.server_addr())).unwrap();
        let worker = std::thread::spawn(move || {
            let req = server.recv().unwrap();
            req.respond(tiny_http::Response::empty(302).with_header(
                tiny_http::Header::from_bytes("Location", "https://example.com/").unwrap(),
            ))
            .unwrap();
        });
        assert!(matches!(
            client.status(&share()).await,
            Err(ShareError::Http(302))
        ));
        worker.join().unwrap();
    }
}
