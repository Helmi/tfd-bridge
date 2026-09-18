//! ES256 device key, RFC 7638 JWK thumbprint and DPoP proof construction.
//!
//! Pure crypto: no I/O, no clock, no randomness source of its own. The native
//! `AccountService` (src-tauri) owns the OS credential store, the injected
//! clock and the injected RNG; this module only turns already-produced key
//! material / random bytes into the exact wire formats the frozen v5 Engine
//! contract requires (`docs/account-authorization.md`,
//! `private-sync/notes/engine-bridge-auth-contract-v5.md`, sections 1 and 3).
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};
use p256::ecdsa::signature::{Signer, Verifier};
use p256::ecdsa::{Signature, SigningKey, VerifyingKey};
use std::fmt;

/// Length of a raw P-256 private scalar / public coordinate, in bytes.
pub const FIELD_LEN: usize = 32;
/// Length of a fixed r||s ES256 signature, in bytes.
pub const SIGNATURE_LEN: usize = 64;

#[derive(Debug, PartialEq, Eq)]
pub enum DeviceKeyError {
    /// The 32 bytes do not encode a valid P-256 private scalar (probability
    /// astronomically small for real random input; still handled).
    InvalidScalar,
}

impl fmt::Display for DeviceKeyError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidScalar => write!(f, "invalid P-256 private scalar"),
        }
    }
}

impl std::error::Error for DeviceKeyError {}

/// A software EC P-256 device key (ES256). No plaintext fallback and no
/// hardware-backing claim: this is deliberately "just" a software key,
/// matching v5 section 1 until a non-exportable CNG/TPM backend is investigated.
#[derive(Clone)]
pub struct DeviceKey {
    signing_key: SigningKey,
}

/// Deliberately redacted: never print the private scalar, even in a test
/// failure message or a debug log line.
impl fmt::Debug for DeviceKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("DeviceKey")
            .field("thumbprint", &self.thumbprint())
            .finish()
    }
}

impl DeviceKey {
    /// Build a device key from a caller-supplied 32-byte scalar. The caller
    /// (the native `SecureRandom` in `AccountService`, or a deterministic
    /// test double) is the only source of randomness this module accepts.
    pub fn from_scalar_bytes(bytes: &[u8; FIELD_LEN]) -> Result<Self, DeviceKeyError> {
        let signing_key =
            SigningKey::from_bytes(bytes.into()).map_err(|_| DeviceKeyError::InvalidScalar)?;
        Ok(Self { signing_key })
    }

    /// Raw 32-byte private scalar, for durable storage in the OS credential
    /// store. Never logged, never sent over the wire.
    pub fn to_scalar_bytes(&self) -> [u8; FIELD_LEN] {
        self.signing_key.to_bytes().into()
    }

    /// The uncompressed public point as `(x, y)`, each `FIELD_LEN` bytes.
    fn public_point(&self) -> ([u8; FIELD_LEN], [u8; FIELD_LEN]) {
        let verifying_key = VerifyingKey::from(&self.signing_key);
        let point = verifying_key.to_sec1_point(false);
        let x = point.x().expect("uncompressed point has x");
        let y = point.y().expect("uncompressed point has y");
        let mut xb = [0u8; FIELD_LEN];
        let mut yb = [0u8; FIELD_LEN];
        xb.copy_from_slice(x.as_slice());
        yb.copy_from_slice(y.as_slice());
        (xb, yb)
    }

    /// Public JWK coordinates, base64url (unpadded).
    pub fn public_jwk(&self) -> PublicJwk {
        let (x, y) = self.public_point();
        PublicJwk {
            x: URL_SAFE_NO_PAD.encode(x),
            y: URL_SAFE_NO_PAD.encode(y),
        }
    }

    /// RFC 7638 JWK thumbprint: SHA-256 over the canonical member ordering
    /// `crv,kty,x,y` with no insignificant whitespace, base64url (unpadded).
    pub fn thumbprint(&self) -> String {
        self.public_jwk().thumbprint()
    }

    /// Sign `message`, returning the fixed 64-byte `r || s` form (not DER).
    pub fn sign(&self, message: &[u8]) -> [u8; SIGNATURE_LEN] {
        let signature: Signature = self.signing_key.sign(message);
        let mut out = [0u8; SIGNATURE_LEN];
        out.copy_from_slice(&signature.to_bytes());
        out
    }

    /// Verify a fixed 64-byte `r || s` signature against this key's public
    /// half. Used only by tests and by a defensive self-check before signing
    /// is trusted at startup; the Engine is the real verifier.
    pub fn verify(&self, message: &[u8], signature: &[u8; SIGNATURE_LEN]) -> bool {
        let Ok(signature) = Signature::from_slice(signature) else {
            return false;
        };
        VerifyingKey::from(&self.signing_key)
            .verify(message, &signature)
            .is_ok()
    }
}

/// EC P-256 public key coordinates, base64url (unpadded), no private
/// material. `kty`/`crv` are implied constants (`"EC"` / `"P-256"`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PublicJwk {
    pub x: String,
    pub y: String,
}

impl PublicJwk {
    /// JSON object in the exact member order the frozen contract's DPoP
    /// header example uses: `kty, crv, x, y`. This is a JOSE header, not the
    /// RFC 7638 thumbprint input -- the two have different required
    /// orderings and must not be confused.
    pub fn to_header_json(&self) -> String {
        format!(
            r#"{{"kty":"EC","crv":"P-256","x":"{}","y":"{}"}}"#,
            self.x, self.y
        )
    }

    /// RFC 7638 canonical form: lexicographic member order `crv,kty,x,y`,
    /// no insignificant whitespace.
    fn canonical_json(&self) -> String {
        format!(
            r#"{{"crv":"P-256","kty":"EC","x":"{}","y":"{}"}}"#,
            self.x, self.y
        )
    }

    /// RFC 7638 thumbprint of this public key: base64url (unpadded) SHA-256
    /// of `canonical_json()`. This is the DPoP `jkt` value.
    pub fn thumbprint(&self) -> String {
        let digest = sha256_bytes(self.canonical_json().as_bytes());
        URL_SAFE_NO_PAD.encode(digest)
    }
}

fn sha256_bytes(bytes: &[u8]) -> [u8; 32] {
    use sha2::Digest;
    let mut hasher = sha2::Sha256::new();
    hasher.update(bytes);
    hasher.finalize().into()
}

/// `ath` claim: base64url (unpadded) SHA-256 of the ASCII access-token
/// string, required whenever an access token is presented (contract
/// section 3).
pub fn ath_claim(access_token: &str) -> String {
    URL_SAFE_NO_PAD.encode(sha256_bytes(access_token.as_bytes()))
}

/// Deterministic UUIDv4-shaped `jti` built from 16 caller-supplied bytes
/// (the injected RNG in production; fixed bytes in tests). Sets the
/// version/variant bits per RFC 4122 the same way a real random UUIDv4 would.
pub fn jti_from_bytes(bytes: [u8; 16]) -> String {
    uuid::Builder::from_random_bytes(bytes)
        .into_uuid()
        .to_string()
}

#[derive(Debug, PartialEq, Eq)]
pub enum ProofError {
    /// `htu` must exclude query and fragment (contract section 3).
    HtuHasQueryOrFragment,
}

/// Build one DPoP proof JWS: `base64url(header).base64url(payload).base64url(signature)`.
///
/// - `htm` is upper-cased defensively (contract: "Uppercase htm").
/// - `htu` must already exclude query/fragment; this is checked, not fixed
///   up, so a caller bug is a hard error rather than a silently wrong proof.
/// - `nonce`/`ath` are included only when `Some` (contract: `ath` forbidden
///   when no access token is presented).
pub fn build_proof(
    key: &DeviceKey,
    htm: &str,
    htu: &str,
    iat_unix: u64,
    jti: &str,
    nonce: Option<&str>,
    ath: Option<&str>,
) -> Result<String, ProofError> {
    if htu.contains('?') || htu.contains('#') {
        return Err(ProofError::HtuHasQueryOrFragment);
    }
    let jwk = key.public_jwk();
    let header = format!(
        r#"{{"typ":"dpop+jwt","alg":"ES256","jwk":{}}}"#,
        jwk.to_header_json()
    );
    let mut payload = format!(
        r#"{{"htm":"{}","htu":{},"iat":{iat_unix},"jti":{}"#,
        htm.to_ascii_uppercase(),
        json_string(htu),
        json_string(jti),
    );
    if let Some(nonce) = nonce {
        payload.push_str(&format!(r#","nonce":{}"#, json_string(nonce)));
    }
    if let Some(ath) = ath {
        payload.push_str(&format!(r#","ath":{}"#, json_string(ath)));
    }
    payload.push('}');

    let header_b64 = URL_SAFE_NO_PAD.encode(header.as_bytes());
    let payload_b64 = URL_SAFE_NO_PAD.encode(payload.as_bytes());
    let signing_input = format!("{header_b64}.{payload_b64}");
    let signature = key.sign(signing_input.as_bytes());
    let signature_b64 = URL_SAFE_NO_PAD.encode(signature);
    Ok(format!("{signing_input}.{signature_b64}"))
}

/// Minimal JSON string escaping -- sufficient for the constrained alphabets
/// (base64url, UUIDs, HTTPS URLs, server-issued nonces) this module handles,
/// implemented locally to avoid pulling `serde_json` into a proof built by
/// hand for exact member ordering.
fn json_string(value: &str) -> String {
    let mut out = String::with_capacity(value.len() + 2);
    out.push('"');
    for c in value.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Private scalar = 1: the public key is exactly the NIST P-256 base
    /// point `G`, whose coordinates are a published constant. This lets the
    /// test assert exact byte values independent of this module's own logic
    /// (cross-checked against an independent Python computation of the same
    /// vector).
    fn base_point_key() -> DeviceKey {
        let mut scalar = [0u8; FIELD_LEN];
        scalar[FIELD_LEN - 1] = 1;
        DeviceKey::from_scalar_bytes(&scalar).unwrap()
    }

    #[test]
    fn public_jwk_matches_known_base_point_coordinates() {
        let key = base_point_key();
        let jwk = key.public_jwk();
        assert_eq!(jwk.x, "axfR8uEsQkf4vOblY6RA8ncDfYEt6zOg9KE5RdiYwpY");
        assert_eq!(jwk.y, "T-NC4v4af5uO5-tKfA-eFivOM1drMV7Oy7ZAaDe_UfU");
    }

    #[test]
    fn thumbprint_matches_independently_computed_rfc7638_value() {
        let key = base_point_key();
        assert_eq!(
            key.thumbprint(),
            "xx0BcA-wMohw8atYDJOe6peGModklG2wRHBlXHMvl0M"
        );
    }

    #[test]
    fn thumbprint_uses_canonical_crv_kty_x_y_ordering() {
        let jwk = PublicJwk {
            x: "x-value".into(),
            y: "y-value".into(),
        };
        assert_eq!(
            jwk.canonical_json(),
            r#"{"crv":"P-256","kty":"EC","x":"x-value","y":"y-value"}"#
        );
    }

    #[test]
    fn header_json_uses_contract_order_kty_crv_x_y() {
        let jwk = PublicJwk {
            x: "x-value".into(),
            y: "y-value".into(),
        };
        assert_eq!(
            jwk.to_header_json(),
            r#"{"kty":"EC","crv":"P-256","x":"x-value","y":"y-value"}"#
        );
    }

    #[test]
    fn scalar_round_trips_through_storage_bytes() {
        let key = base_point_key();
        let bytes = key.to_scalar_bytes();
        let restored = DeviceKey::from_scalar_bytes(&bytes).unwrap();
        assert_eq!(restored.public_jwk(), key.public_jwk());
    }

    #[test]
    fn zero_scalar_is_rejected() {
        let scalar = [0u8; FIELD_LEN];
        assert_eq!(
            DeviceKey::from_scalar_bytes(&scalar).unwrap_err(),
            DeviceKeyError::InvalidScalar
        );
    }

    #[test]
    fn sign_produces_fixed_64_byte_r_s_and_verifies() {
        let key = base_point_key();
        let sig = key.sign(b"hello dpop");
        assert_eq!(sig.len(), SIGNATURE_LEN);
        assert!(key.verify(b"hello dpop", &sig));
        assert!(!key.verify(b"tampered", &sig));
    }

    #[test]
    fn ath_claim_is_base64url_sha256_of_access_token() {
        let claim = ath_claim("example-access-token");
        // Cross-checked independently: base64url(sha256("example-access-token")).
        assert_eq!(claim, "Z1P3Ll-e0JrOBqzfbrTXjd9Z_l-iiW1obnZMWdV1w1s");
    }

    #[test]
    fn jti_from_bytes_is_deterministic_and_v4_shaped() {
        let jti = jti_from_bytes([0x11; 16]);
        let jti2 = jti_from_bytes([0x11; 16]);
        assert_eq!(jti, jti2, "same bytes must give the same jti");
        let jti3 = jti_from_bytes([0x22; 16]);
        assert_ne!(jti, jti3);
        // UUIDv4 shape: version nibble '4', variant nibble in 8-b.
        let parts: Vec<&str> = jti.split('-').collect();
        assert_eq!(parts.len(), 5);
        assert_eq!(&parts[2][0..1], "4");
        assert!(matches!(parts[3].chars().next().unwrap(), '8'..='b'));
    }

    #[test]
    fn build_proof_matches_contract_field_order_and_verifies() {
        let key = base_point_key();
        let jws = build_proof(
            &key,
            "post",
            "https://engine.tfd.rocks/api/v1/bridge/auth/token",
            1758196800,
            "11111111-1111-4111-8111-111111111111",
            None,
            None,
        )
        .unwrap();
        let parts: Vec<&str> = jws.split('.').collect();
        assert_eq!(parts.len(), 3);
        let header = String::from_utf8(URL_SAFE_NO_PAD.decode(parts[0]).unwrap()).unwrap();
        assert!(header.starts_with(r#"{"typ":"dpop+jwt","alg":"ES256","jwk":{"kty":"EC""#));
        let payload = String::from_utf8(URL_SAFE_NO_PAD.decode(parts[1]).unwrap()).unwrap();
        assert_eq!(
            payload,
            r#"{"htm":"POST","htu":"https://engine.tfd.rocks/api/v1/bridge/auth/token","iat":1758196800,"jti":"11111111-1111-4111-8111-111111111111"}"#
        );
        let signing_input = format!("{}.{}", parts[0], parts[1]);
        let signature_bytes = URL_SAFE_NO_PAD.decode(parts[2]).unwrap();
        let mut sig = [0u8; SIGNATURE_LEN];
        sig.copy_from_slice(&signature_bytes);
        assert!(key.verify(signing_input.as_bytes(), &sig));
    }

    #[test]
    fn build_proof_includes_nonce_and_ath_only_when_present() {
        let key = base_point_key();
        let jws = build_proof(
            &key,
            "GET",
            "https://engine.tfd.rocks/api/v1/bridge/auth/me",
            1,
            "jti-value",
            Some("server-nonce"),
            Some("ath-value"),
        )
        .unwrap();
        let payload =
            String::from_utf8(URL_SAFE_NO_PAD.decode(jws.split('.').nth(1).unwrap()).unwrap())
                .unwrap();
        assert_eq!(
            payload,
            r#"{"htm":"GET","htu":"https://engine.tfd.rocks/api/v1/bridge/auth/me","iat":1,"jti":"jti-value","nonce":"server-nonce","ath":"ath-value"}"#
        );
    }

    #[test]
    fn build_proof_rejects_htu_with_query_or_fragment() {
        let key = base_point_key();
        assert_eq!(
            build_proof(&key, "GET", "https://x/y?z=1", 1, "j", None, None),
            Err(ProofError::HtuHasQueryOrFragment)
        );
        assert_eq!(
            build_proof(&key, "GET", "https://x/y#z", 1, "j", None, None),
            Err(ProofError::HtuHasQueryOrFragment)
        );
    }
}
