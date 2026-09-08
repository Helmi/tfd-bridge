//! Native MP4 upload transport. This does not authorize a Discord share.
//! The job coordinator must obtain an Engine-authorized ticket after rendering,
//! then submit the returned receipt to Engine's completion endpoint.
//! Engine endpoint orchestration lives in `video_share`; this module owns PUT only.

use reqwest::{Client, Url};
use serde::Serialize;
use sha2::{Digest, Sha256};
use std::io::Read;
use std::path::Path;
use std::time::{Duration, SystemTime};

const LOCAL_MAX_BYTES: u64 = 512 * 1024 * 1024;

#[derive(Debug, PartialEq, Eq)]
pub enum UploadError {
    ReadFailed,
    InvalidMp4,
    ExceedsLimit,
    InvalidTicket,
    TicketExpired,
    TransportFailed,
    HttpRejected(u16),
}

impl std::fmt::Display for UploadError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Never expose presigned URLs/query credentials or remote error bodies.
        write!(f, "video upload: {self:?}")
    }
}
impl std::error::Error for UploadError {}

/// Not Debug/Serialize: the URL contains a short-lived storage credential.
pub struct UploadTicket {
    url: Url,
    max_bytes: u64,
    expires_at: SystemTime,
}

impl UploadTicket {
    /// `trusted_hosts` MUST come from the trusted Engine config, never from
    /// the browser's render-start request. Exact hosts only; no suffix matches.
    pub fn validate(
        url: &str,
        max_bytes: u64,
        expires_at: SystemTime,
        trusted_hosts: &[String],
    ) -> Result<Self, UploadError> {
        let url = Url::parse(url).map_err(|_| UploadError::InvalidTicket)?;
        if url.scheme() != "https"
            || !url.username().is_empty()
            || url.password().is_some()
            || url.fragment().is_some()
            || url.port_or_known_default() != Some(443)
            || max_bytes == 0
            || max_bytes > LOCAL_MAX_BYTES
            || !url.host_str().is_some_and(|host| {
                trusted_hosts
                    .iter()
                    .any(|trusted| host.eq_ignore_ascii_case(trusted))
            })
        {
            return Err(UploadError::InvalidTicket);
        }
        let ticket = Self {
            url,
            max_bytes,
            expires_at,
        };
        ticket.check_expiry()?;
        Ok(ticket)
    }

    fn check_expiry(&self) -> Result<(), UploadError> {
        if SystemTime::now() >= self.expires_at {
            Err(UploadError::TicketExpired)
        } else {
            Ok(())
        }
    }
}

#[derive(Debug, Serialize, PartialEq, Eq)]
pub struct UploadReceipt {
    pub bytes: u64,
    pub sha256: String,
}

/// Bounded encoded bytes, loaded from a job-owned output file. The caller must
/// resolve a job/file handle, not accept an arbitrary path from an Engine page.
pub struct PreparedVideo {
    bytes: Vec<u8>,
    receipt: UploadReceipt,
}

impl PreparedVideo {
    pub fn receipt(&self) -> &UploadReceipt {
        &self.receipt
    }
    pub fn read(path: &Path, max_bytes: u64) -> Result<Self, UploadError> {
        if max_bytes == 0 || max_bytes > LOCAL_MAX_BYTES {
            return Err(UploadError::ExceedsLimit);
        }
        let file = std::fs::File::open(path).map_err(|_| UploadError::ReadFailed)?;
        let metadata = file.metadata().map_err(|_| UploadError::ReadFailed)?;
        if !metadata.is_file() {
            return Err(UploadError::ReadFailed);
        }
        if metadata.len() > max_bytes {
            return Err(UploadError::ExceedsLimit);
        }
        let mut bytes = Vec::new();
        file.take(max_bytes + 1)
            .read_to_end(&mut bytes)
            .map_err(|_| UploadError::ReadFailed)?;
        if bytes.len() as u64 > max_bytes {
            return Err(UploadError::ExceedsLimit);
        }
        if bytes.get(4..8) != Some(b"ftyp") {
            return Err(UploadError::InvalidMp4);
        }
        let receipt = UploadReceipt {
            bytes: bytes.len() as u64,
            sha256: hex::encode(Sha256::digest(&bytes)),
        };
        Ok(Self { bytes, receipt })
    }

    /// One PUT attempt; the coordinator decides retries/ticket renewal.
    /// Dropping this future cancels an in-flight request. No cookies, bearer
    /// tokens or automatic redirects are used for storage requests.
    pub async fn upload(self, ticket: UploadTicket) -> Result<UploadReceipt, UploadError> {
        ticket.check_expiry()?;
        if self.receipt.bytes > ticket.max_bytes {
            return Err(UploadError::ExceedsLimit);
        }
        let _ = rustls::crypto::ring::default_provider().install_default();
        let client = Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .timeout(Duration::from_secs(5 * 60))
            .build()
            .map_err(|_| UploadError::TransportFailed)?;
        let response = client
            .put(ticket.url)
            .header(reqwest::header::CONTENT_TYPE, "video/mp4")
            .header(reqwest::header::CONTENT_LENGTH, self.receipt.bytes)
            .body(self.bytes)
            .send()
            .await
            .map_err(|_| UploadError::TransportFailed)?;
        if !response.status().is_success() {
            return Err(UploadError::HttpRejected(response.status().as_u16()));
        }
        Ok(self.receipt)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tiny_http::{Header, Response, Server, StatusCode};

    fn future() -> SystemTime {
        SystemTime::now() + Duration::from_secs(60)
    }
    fn sample() -> Vec<u8> {
        b"\0\0\0\x18ftypisom\0\0\0\0isomavc1".to_vec()
    }
    fn prepared() -> PreparedVideo {
        let file = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(file.path(), sample()).unwrap();
        PreparedVideo::read(file.path(), 1024).unwrap()
    }

    #[test]
    fn only_exact_trusted_https_storage_hosts_are_accepted() {
        let hosts = vec!["storage.example.com".to_string()];
        assert!(UploadTicket::validate(
            "https://storage.example.com/video?signature=secret",
            1024,
            future(),
            &hosts
        )
        .is_ok());
        for url in [
            "http://storage.example.com/video",
            "https://storage.example.com.evil.test/video",
            "https://user:pass@storage.example.com/video",
            "https://storage.example.com:444/video",
            "https://storage.example.com/video#fragment",
            "https://127.0.0.1/video",
        ] {
            assert!(matches!(
                UploadTicket::validate(url, 1024, future(), &hosts),
                Err(UploadError::InvalidTicket)
            ));
        }
        assert!(matches!(
            UploadTicket::validate(
                "https://storage.example.com/video",
                1024,
                SystemTime::UNIX_EPOCH,
                &hosts
            ),
            Err(UploadError::TicketExpired)
        ));
    }

    #[test]
    fn hashes_actual_bytes_and_refuses_oversized_or_non_mp4_files() {
        let file = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(file.path(), sample()).unwrap();
        assert_eq!(
            prepared().receipt.sha256,
            hex::encode(Sha256::digest(sample()))
        );
        assert!(matches!(
            PreparedVideo::read(file.path(), 8),
            Err(UploadError::ExceedsLimit)
        ));
        std::fs::write(file.path(), b"not a video").unwrap();
        assert!(matches!(
            PreparedVideo::read(file.path(), 1024),
            Err(UploadError::InvalidMp4)
        ));
    }

    // Tests construct a private ticket for loopback HTTP; production can only
    // obtain one through validate(), which requires trusted HTTPS storage.
    #[tokio::test]
    async fn sends_exact_bytes_without_credentials_and_returns_receipt() {
        let server = Server::http("127.0.0.1:0").unwrap();
        let url = Url::parse(&format!("http://{}/video", server.server_addr())).unwrap();
        let worker = std::thread::spawn(move || {
            let mut request = server
                .recv_timeout(Duration::from_secs(5))
                .unwrap()
                .unwrap();
            assert_eq!(request.method().as_str(), "PUT");
            assert!(request
                .headers()
                .iter()
                .any(|h| h.field.equiv("Content-Type") && h.value.as_str() == "video/mp4"));
            assert!(!request
                .headers()
                .iter()
                .any(|h| h.field.equiv("Cookie") || h.field.equiv("Authorization")));
            let mut bytes = Vec::new();
            request.as_reader().read_to_end(&mut bytes).unwrap();
            assert_eq!(bytes, sample());
            request.respond(Response::empty(StatusCode(200))).unwrap();
        });
        let receipt = prepared()
            .upload(UploadTicket {
                url,
                max_bytes: 1024,
                expires_at: future(),
            })
            .await
            .unwrap();
        assert_eq!(receipt.bytes, sample().len() as u64);
        worker.join().unwrap();
    }

    #[tokio::test]
    async fn refuses_redirects_and_rechecks_cap_and_expiry_before_network() {
        let server = Server::http("127.0.0.1:0").unwrap();
        let url = Url::parse(&format!(
            "http://{}/video?secret=not-for-errors",
            server.server_addr()
        ))
        .unwrap();
        assert_eq!(
            prepared()
                .upload(UploadTicket {
                    url: url.clone(),
                    max_bytes: 8,
                    expires_at: future()
                })
                .await,
            Err(UploadError::ExceedsLimit)
        );
        assert_eq!(
            prepared()
                .upload(UploadTicket {
                    url: url.clone(),
                    max_bytes: 1024,
                    expires_at: SystemTime::UNIX_EPOCH
                })
                .await,
            Err(UploadError::TicketExpired)
        );
        let worker = std::thread::spawn(move || {
            let request = server
                .recv_timeout(Duration::from_secs(5))
                .unwrap()
                .unwrap();
            request
                .respond(
                    Response::empty(StatusCode(307))
                        .with_header(Header::from_bytes("Location", "/unexpected").unwrap()),
                )
                .unwrap();
            assert!(server
                .recv_timeout(Duration::from_millis(300))
                .unwrap()
                .is_none());
        });
        let error = prepared()
            .upload(UploadTicket {
                url,
                max_bytes: 1024,
                expires_at: future(),
            })
            .await
            .unwrap_err();
        assert_eq!(error, UploadError::HttpRejected(307));
        assert!(!error.to_string().contains("secret"));
        worker.join().unwrap();
    }
}
