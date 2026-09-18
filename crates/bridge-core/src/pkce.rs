//! PKCE (S256) verifier/challenge and login `state`, built from
//! caller-supplied random bytes (contract v5 section 4.3). Pure, no RNG of
//! its own — the native `AccountService`'s injected `SecureRandom` produces
//! the bytes; this module only turns them into the exact wire encodings.
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};
use sha2::{Digest, Sha256};

/// 32 random bytes -> a 43-character base64url verifier, within RFC 7636's
/// 43-128 character allowed range. Longer buys nothing; this is fixed.
pub const VERIFIER_ENTROPY_BYTES: usize = 32;
/// `state` fixed-length random bytes: 256 bits, compared in constant time.
pub const STATE_ENTROPY_BYTES: usize = 32;

/// PKCE `code_verifier` from raw random bytes (RFC 7636 section 4.1).
pub fn code_verifier_from_bytes(bytes: &[u8; VERIFIER_ENTROPY_BYTES]) -> String {
    URL_SAFE_NO_PAD.encode(bytes)
}

/// PKCE `code_challenge` for `code_challenge_method=S256` (RFC 7636 section
/// 4.2): `BASE64URL-ENCODE(SHA256(ASCII(code_verifier)))`.
pub fn code_challenge_s256(verifier: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(verifier.as_bytes());
    URL_SAFE_NO_PAD.encode(hasher.finalize())
}

/// Login `state` from raw random bytes: base64url of a fixed-length buffer.
pub fn state_from_bytes(bytes: &[u8; STATE_ENTROPY_BYTES]) -> String {
    URL_SAFE_NO_PAD.encode(bytes)
}

/// Constant-time string comparison (contract: "state is compared in constant
/// time; unknown states are ignored"). Not a general-purpose `PartialEq`
/// replacement — deliberately named to be used only where timing matters.
pub fn constant_time_eq(a: &str, b: &str) -> bool {
    let (a, b) = (a.as_bytes(), b.as_bytes());
    if a.len() != b.len() {
        return false;
    }
    let mut diff = 0u8;
    for (x, y) in a.iter().zip(b.iter()) {
        diff |= x ^ y;
    }
    diff == 0
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The exact RFC 7636 Appendix B worked example: an independent,
    /// published test vector, not something this module computed itself.
    #[test]
    fn code_challenge_matches_rfc7636_appendix_b_vector() {
        let verifier = "dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk";
        assert_eq!(
            code_challenge_s256(verifier),
            "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM"
        );
    }

    #[test]
    fn code_verifier_from_bytes_is_deterministic_and_url_safe() {
        let bytes = [7u8; VERIFIER_ENTROPY_BYTES];
        let v1 = code_verifier_from_bytes(&bytes);
        let v2 = code_verifier_from_bytes(&bytes);
        assert_eq!(v1, v2);
        assert_eq!(v1.len(), 43);
        assert!(v1.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_'));
    }

    #[test]
    fn state_from_bytes_is_deterministic_and_differs_by_input() {
        let a = state_from_bytes(&[1u8; STATE_ENTROPY_BYTES]);
        let b = state_from_bytes(&[2u8; STATE_ENTROPY_BYTES]);
        assert_ne!(a, b);
        assert_eq!(a, state_from_bytes(&[1u8; STATE_ENTROPY_BYTES]));
    }

    #[test]
    fn constant_time_eq_matches_regular_equality_semantics() {
        assert!(constant_time_eq("abc", "abc"));
        assert!(!constant_time_eq("abc", "abd"));
        assert!(!constant_time_eq("abc", "ab"));
        assert!(!constant_time_eq("", "a"));
        assert!(constant_time_eq("", ""));
    }
}
