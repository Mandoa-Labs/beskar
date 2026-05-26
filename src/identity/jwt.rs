//! Minimal JWT (JWS compact) handling for the identity layer (PRD §6.3 E2.2).
//!
//! All crypto routes through the existing `openssl` crate so the FIPS posture
//! (E1.9) stays coherent — no new dependencies, and a FIPS build verifies tokens
//! with the validated module. Two algorithms are supported:
//!
//! * **HS256** — HMAC-SHA256, used both to *issue* short-lived beskar session
//!   tokens (`beskar login`) and to validate OIDC ID tokens from an IdP that
//!   signs with a shared secret.
//! * **RS256** — RSASSA-PKCS1-v1_5 over SHA-256, used to *verify* OIDC ID tokens
//!   signed by an IdP's RSA key (verification only; beskar never signs RS256).
//!
//! Only the algorithm a caller asks for is accepted: the `alg` header is matched
//! exactly, so `none` and HS/RS confusion attacks are rejected.

use anyhow::{bail, Context, Result};
use openssl::hash::MessageDigest;
use openssl::pkey::PKey;
use openssl::sign::{Signer, Verifier};
use serde_json::Value;

// ---------------------------------------------------------------------------
// base64url (RFC 7515 §2 — URL-safe alphabet, no padding)
// ---------------------------------------------------------------------------

const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";

pub fn b64url_encode(input: &[u8]) -> String {
    let mut out = String::with_capacity(input.len().div_ceil(3) * 4);
    for chunk in input.chunks(3) {
        let b0 = chunk[0] as u32;
        let b1 = *chunk.get(1).unwrap_or(&0) as u32;
        let b2 = *chunk.get(2).unwrap_or(&0) as u32;
        let n = (b0 << 16) | (b1 << 8) | b2;
        out.push(ALPHABET[((n >> 18) & 63) as usize] as char);
        out.push(ALPHABET[((n >> 12) & 63) as usize] as char);
        if chunk.len() > 1 {
            out.push(ALPHABET[((n >> 6) & 63) as usize] as char);
        }
        if chunk.len() > 2 {
            out.push(ALPHABET[(n & 63) as usize] as char);
        }
    }
    out
}

pub fn b64url_decode(input: &str) -> Result<Vec<u8>> {
    fn val(c: u8) -> Option<u32> {
        match c {
            b'A'..=b'Z' => Some((c - b'A') as u32),
            b'a'..=b'z' => Some((c - b'a' + 26) as u32),
            b'0'..=b'9' => Some((c - b'0' + 52) as u32),
            b'-' => Some(62),
            b'_' => Some(63),
            _ => None,
        }
    }
    // Tolerate (but do not require) trailing `=` padding.
    let trimmed = input.trim_end_matches('=');
    let mut out = Vec::with_capacity(trimmed.len() * 3 / 4);
    let mut buf: u32 = 0;
    let mut bits: u32 = 0;
    for &c in trimmed.as_bytes() {
        let v = val(c).context("invalid base64url character")?;
        buf = (buf << 6) | v;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((buf >> bits) as u8);
        }
    }
    Ok(out)
}

// ---------------------------------------------------------------------------
// HMAC-SHA256 (HS256)
// ---------------------------------------------------------------------------

fn hs256_mac(signing_input: &[u8], secret: &[u8]) -> Result<Vec<u8>> {
    let key = PKey::hmac(secret).context("failed to build HMAC key")?;
    let mut signer =
        Signer::new(MessageDigest::sha256(), &key).context("failed to init HMAC signer")?;
    signer.update(signing_input).context("HMAC update failed")?;
    signer.sign_to_vec().context("HMAC finalization failed")
}

/// Issue an HS256-signed JWT carrying `claims`.
pub fn encode_hs256(claims: &Value, secret: &[u8]) -> Result<String> {
    let header = serde_json::json!({ "alg": "HS256", "typ": "JWT" });
    let h = b64url_encode(serde_json::to_vec(&header)?.as_slice());
    let p = b64url_encode(serde_json::to_vec(claims)?.as_slice());
    let signing_input = format!("{h}.{p}");
    let sig = hs256_mac(signing_input.as_bytes(), secret)?;
    Ok(format!("{signing_input}.{}", b64url_encode(&sig)))
}

// ---------------------------------------------------------------------------
// Parsing & verification
// ---------------------------------------------------------------------------

/// Split a compact JWS into `(signing_input, header, claims, signature)`.
fn split(token: &str) -> Result<(String, Value, Value, Vec<u8>)> {
    let mut parts = token.split('.');
    let h = parts.next().filter(|s| !s.is_empty()).context("jwt: missing header")?;
    let p = parts.next().filter(|s| !s.is_empty()).context("jwt: missing payload")?;
    let s = parts.next().filter(|s| !s.is_empty()).context("jwt: missing signature")?;
    if parts.next().is_some() {
        bail!("jwt: too many segments");
    }
    let header: Value = serde_json::from_slice(&b64url_decode(h)?).context("jwt: bad header")?;
    let claims: Value = serde_json::from_slice(&b64url_decode(p)?).context("jwt: bad payload")?;
    let sig = b64url_decode(s)?;
    Ok((format!("{h}.{p}"), header, claims, sig))
}

fn require_alg(header: &Value, expected: &str) -> Result<()> {
    let alg = header.get("alg").and_then(Value::as_str).unwrap_or("");
    if alg != expected {
        bail!("jwt: unexpected alg '{alg}' (expected {expected})");
    }
    Ok(())
}

/// Verify an HS256 token and return its claims. Constant-time MAC comparison.
pub fn verify_hs256(token: &str, secret: &[u8]) -> Result<Value> {
    let (signing_input, header, claims, sig) = split(token)?;
    require_alg(&header, "HS256")?;
    let expected = hs256_mac(signing_input.as_bytes(), secret)?;
    if expected.len() != sig.len() || !openssl::memcmp::eq(&expected, &sig) {
        bail!("jwt: signature verification failed");
    }
    Ok(claims)
}

/// Verify an RS256 token against an RSA public key (PEM) and return its claims.
pub fn verify_rs256(token: &str, public_key_pem: &str) -> Result<Value> {
    let (signing_input, header, claims, sig) = split(token)?;
    require_alg(&header, "RS256")?;
    let pkey = PKey::public_key_from_pem(public_key_pem.as_bytes())
        .context("jwt: invalid RS256 public key (expected PEM SubjectPublicKeyInfo)")?;
    let mut verifier =
        Verifier::new(MessageDigest::sha256(), &pkey).context("failed to init RSA verifier")?;
    verifier.update(signing_input.as_bytes()).context("RSA verify update failed")?;
    if !verifier.verify(&sig).context("RSA verify failed")? {
        bail!("jwt: signature verification failed");
    }
    Ok(claims)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn base64url_roundtrips_without_padding() {
        for input in [&b""[..], b"f", b"fo", b"foo", b"foob", b"fooba", b"foobar"] {
            let encoded = b64url_encode(input);
            assert!(!encoded.contains('='), "no padding: {encoded}");
            assert_eq!(b64url_decode(&encoded).unwrap(), input);
        }
        // URL-safe alphabet: bytes that map to + / in standard base64 use - _.
        let bytes = [0xfb, 0xff, 0xbf];
        assert_eq!(b64url_encode(&bytes), "-_-_");
        assert_eq!(b64url_decode("-_-_").unwrap(), bytes);
    }

    #[test]
    fn hs256_encode_then_verify_roundtrips() {
        let secret = b"super-secret-signing-key";
        let claims = json!({"sub": "alice", "tenant": "acme", "exp": 9_999_999_999u64});
        let token = encode_hs256(&claims, secret).unwrap();
        assert_eq!(token.split('.').count(), 3);
        let decoded = verify_hs256(&token, secret).unwrap();
        assert_eq!(decoded["sub"], "alice");
        assert_eq!(decoded["tenant"], "acme");
    }

    #[test]
    fn hs256_rejects_wrong_secret_and_tampering() {
        let token = encode_hs256(&json!({"sub": "alice"}), b"key-one").unwrap();
        assert!(verify_hs256(&token, b"key-two").is_err());

        // Tamper with the payload but keep the original signature.
        let mut parts: Vec<&str> = token.split('.').collect();
        let forged_payload = b64url_encode(b"{\"sub\":\"admin\"}");
        parts[1] = &forged_payload;
        let forged = parts.join(".");
        assert!(verify_hs256(&forged, b"key-one").is_err());
    }

    #[test]
    fn rejects_alg_confusion_and_none() {
        let secret = b"key";
        let token = encode_hs256(&json!({"sub": "x"}), secret).unwrap();
        // An HS256 token must not verify as RS256, and vice versa.
        assert!(verify_rs256(&token, "not-a-key").is_err());

        // A forged `alg: none` header must be rejected by HS256 verification.
        let header = b64url_encode(br#"{"alg":"none","typ":"JWT"}"#);
        let payload = b64url_encode(br#"{"sub":"admin"}"#);
        let none_token = format!("{header}.{payload}.");
        assert!(verify_hs256(&none_token, secret).is_err());
    }

    #[test]
    fn malformed_tokens_are_rejected() {
        assert!(verify_hs256("only.two", b"k").is_err());
        assert!(verify_hs256("a.b.c.d", b"k").is_err());
        assert!(verify_hs256("..", b"k").is_err());
    }
}
