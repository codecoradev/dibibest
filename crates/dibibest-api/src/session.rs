//! base64url, HMAC-signed cookie payloads, PKCE and random tokens.

use hmac::{Hmac, Mac};
use rand::RngCore;
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::time::{SystemTime, UNIX_EPOCH};
use subtle::ConstantTimeEq;

const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";

pub fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

/// RFC 4648 §5 base64url, no padding.
pub fn b64url(data: &[u8]) -> String {
    let mut out = String::with_capacity(data.len().div_ceil(3) * 4);
    for c in data.chunks(3) {
        let n = (c[0] as u32) << 16
            | (*c.get(1).unwrap_or(&0) as u32) << 8
            | *c.get(2).unwrap_or(&0) as u32;
        for i in 0..=c.len() {
            out.push(ALPHABET[(n >> (18 - 6 * i) & 63) as usize] as char);
        }
    }
    out
}

pub fn b64url_decode(s: &str) -> Option<Vec<u8>> {
    let mut out = Vec::with_capacity(s.len() * 3 / 4);
    for c in s.as_bytes().chunks(4) {
        if c.len() == 1 {
            return None;
        }
        let mut n = 0u32;
        for i in 0..4 {
            let v = match c.get(i) {
                Some(b) => ALPHABET.iter().position(|a| a == b)? as u32,
                None => 0,
            };
            n = n << 6 | v;
        }
        for i in 0..c.len() - 1 {
            out.push((n >> (16 - 8 * i)) as u8);
        }
    }
    Some(out)
}

/// Hash an arbitrary-length secret to exactly 32 bytes.
pub fn derive_key(secret: &str) -> [u8; 32] {
    Sha256::digest(secret.as_bytes()).into()
}

fn mac(key: &[u8; 32], msg: &[u8]) -> Vec<u8> {
    let mut m = Hmac::<Sha256>::new_from_slice(key).expect("hmac takes any key length");
    m.update(msg);
    m.finalize().into_bytes().to_vec()
}

/// `b64(json).b64(hmac)`. The payload must carry a numeric `exp` (unix seconds).
pub fn sign(key: &[u8; 32], payload: &Value) -> String {
    let body = b64url(payload.to_string().as_bytes());
    let sig = b64url(&mac(key, body.as_bytes()));
    format!("{body}.{sig}")
}

/// Returns the payload only if the signature is valid (constant-time) and `exp` is in the future.
pub fn verify(key: &[u8; 32], token: &str) -> Option<Value> {
    let (body, sig) = token.split_once('.')?;
    let sig = b64url_decode(sig)?;
    if !bool::from(sig.ct_eq(&mac(key, body.as_bytes()))) {
        return None;
    }
    let v: Value = serde_json::from_slice(&b64url_decode(body)?).ok()?;
    (v.get("exp")?.as_u64()? > now()).then_some(v)
}

/// `n` random bytes, base64url.
pub fn random_token(n: usize) -> String {
    let mut b = vec![0u8; n];
    rand::rngs::OsRng.fill_bytes(&mut b);
    b64url(&b)
}

pub fn pkce_challenge(verifier: &str) -> String {
    b64url(&Sha256::digest(verifier.as_bytes()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn b64url_rfc4648_vectors() {
        for (i, o) in [
            ("", ""),
            ("f", "Zg"),
            ("fo", "Zm8"),
            ("foo", "Zm9v"),
            ("foob", "Zm9vYg"),
            ("fooba", "Zm9vYmE"),
            ("foobar", "Zm9vYmFy"),
        ] {
            assert_eq!(b64url(i.as_bytes()), o);
            assert_eq!(b64url_decode(o).unwrap(), i.as_bytes());
        }
        assert_eq!(b64url(&[0xfb, 0xff, 0xfe]), "-__-");
        assert!(b64url_decode("a").is_none());
        assert!(b64url_decode("a+b/").is_none());
    }

    #[test]
    fn sign_verify_tamper_secret_expiry() {
        let k = derive_key("s3cret");
        let t = sign(&k, &json!({"email":"a@b.c","exp":now()+60}));
        assert_eq!(verify(&k, &t).unwrap()["email"], "a@b.c");
        assert!(verify(&derive_key("other"), &t).is_none());
        let (b, s) = t.split_once('.').unwrap();
        let forged = b64url(
            json!({"email":"evil@b.c","exp":now()+60})
                .to_string()
                .as_bytes(),
        );
        assert!(verify(&k, &format!("{forged}.{s}")).is_none());
        assert!(verify(&k, &format!("{b}.AAAA")).is_none());
        assert!(verify(&k, "garbage").is_none());
        let old = sign(&k, &json!({"email":"a@b.c","exp":now()-1}));
        assert!(verify(&k, &old).is_none());
    }

    #[test]
    fn pkce_matches_rfc7636_appendix_b() {
        assert_eq!(
            pkce_challenge("dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk"),
            "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM"
        );
    }

    #[test]
    fn tokens_are_random() {
        assert_ne!(random_token(32), random_token(32));
        assert_eq!(random_token(32).len(), 43);
    }
}
