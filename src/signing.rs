//! Signed image URLs.
//!
//! A signature is `HMAC-SHA256(key, "{id}\n{canonical query}\n{exp}")`,
//! hex-encoded. Unless `ANSEL_SIGNING_KEY` is set, the key is derived from the
//! API token, so a deployment only has to manage one secret.

use hmac::{Hmac, KeyInit, Mac};
use sha2::{Digest, Sha256};

type HmacSha256 = Hmac<Sha256>;

/// Label mixed into the derived key, so the signing key is never the API
/// token itself.
const DERIVATION_LABEL: &[u8] = b"ansel/url-signing/v1";

/// The signing key: `ANSEL_SIGNING_KEY` when given, otherwise derived from
/// the API token.
pub fn signing_key(api_token: &str, explicit: Option<&str>) -> Vec<u8> {
    match explicit {
        Some(key) => key.as_bytes().to_vec(),
        None => {
            let mut mac = new_mac(api_token.as_bytes());
            mac.update(DERIVATION_LABEL);
            mac.finalize().into_bytes().to_vec()
        }
    }
}

/// Signs an image ID, canonical transform query, and expiry (Unix seconds).
pub fn sign(key: &[u8], id: &str, query: &str, exp: u64) -> String {
    let mut mac = new_mac(key);
    mac.update(payload(id, query, exp).as_bytes());
    hex(&mac.finalize().into_bytes())
}

/// Checks a hex signature in constant time.
pub fn verify(key: &[u8], id: &str, query: &str, exp: u64, sig: &str) -> bool {
    let Some(sig) = unhex(sig) else {
        return false;
    };
    let mut mac = new_mac(key);
    mac.update(payload(id, query, exp).as_bytes());
    mac.verify_slice(&sig).is_ok()
}

/// SHA-256 of `data`.
pub fn sha256(data: &[u8]) -> [u8; 32] {
    Sha256::digest(data).into()
}

/// Compares two byte strings without leaking where they differ.
pub fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

/// Lowercase hex encoding.
pub fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write;
    bytes
        .iter()
        .fold(String::with_capacity(bytes.len() * 2), |mut s, b| {
            let _ = write!(s, "{b:02x}");
            s
        })
}

fn unhex(s: &str) -> Option<Vec<u8>> {
    if s.len() != 64 {
        return None;
    }
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(s.get(i..i + 2)?, 16).ok())
        .collect()
}

fn payload(id: &str, query: &str, exp: u64) -> String {
    format!("{id}\n{query}\n{exp}")
}

fn new_mac(key: &[u8]) -> HmacSha256 {
    // HMAC accepts keys of any length, so this can't fail.
    HmacSha256::new_from_slice(key).expect("HMAC accepts any key length")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sign_and_verify() {
        let key = signing_key("token", None);
        let sig = sign(&key, "01JD8Z6Q4M3V9K2T7XW5R8N0PB", "w=10", 100);
        assert_eq!(sig.len(), 64);
        assert!(verify(
            &key,
            "01JD8Z6Q4M3V9K2T7XW5R8N0PB",
            "w=10",
            100,
            &sig
        ));
        assert!(!verify(
            &key,
            "01JD8Z6Q4M3V9K2T7XW5R8N0PB",
            "w=11",
            100,
            &sig
        ));
        assert!(!verify(
            &key,
            "01JD8Z6Q4M3V9K2T7XW5R8N0PB",
            "w=10",
            101,
            &sig
        ));
        assert!(!verify(
            &key,
            "01JD8Z6Q4M3V9K2T7XW5R8N0PC",
            "w=10",
            100,
            &sig
        ));
        assert!(!verify(
            &key,
            "01JD8Z6Q4M3V9K2T7XW5R8N0PB",
            "w=10",
            100,
            "zz"
        ));
        let other = signing_key("other-token", None);
        assert!(!verify(
            &other,
            "01JD8Z6Q4M3V9K2T7XW5R8N0PB",
            "w=10",
            100,
            &sig
        ));
    }

    #[test]
    fn derived_key_differs_from_token() {
        assert_ne!(signing_key("token", None), b"token".to_vec());
        assert_eq!(signing_key("token", Some("explicit")), b"explicit".to_vec());
    }

    #[test]
    fn constant_time_eq_works() {
        assert!(constant_time_eq(b"abc", b"abc"));
        assert!(!constant_time_eq(b"abc", b"abd"));
        assert!(!constant_time_eq(b"abc", b"ab"));
    }
}
