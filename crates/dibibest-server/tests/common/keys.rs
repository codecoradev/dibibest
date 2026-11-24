//! Throwaway ES256 keys for tests: made on the spot with `ring`, so no private key is kept anywhere.
#![allow(dead_code)]
use dibibest_server::auth::codec::b64url;
use ring::rand::SystemRandom;
use ring::signature::{EcdsaKeyPair, KeyPair, ECDSA_P256_SHA256_FIXED_SIGNING};
use serde_json::{json, Value};

pub struct Keys {
    pair: EcdsaKeyPair,
    rng: SystemRandom,
}

impl Keys {
    pub fn new() -> Self {
        let rng = SystemRandom::new();
        let pkcs8 = EcdsaKeyPair::generate_pkcs8(&ECDSA_P256_SHA256_FIXED_SIGNING, &rng).unwrap();
        let pair = EcdsaKeyPair::from_pkcs8(&ECDSA_P256_SHA256_FIXED_SIGNING, pkcs8.as_ref(), &rng)
            .unwrap();
        Self { pair, rng }
    }

    /// The public key as a JWK; `alg` is left out when `None`, as many providers do.
    pub fn jwk(&self, kid: &str, alg: Option<&str>) -> Value {
        let point = self.pair.public_key().as_ref(); // 0x04, x (32 bytes), y (32 bytes)
        let mut k = json!({"kty": "EC", "crv": "P-256", "kid": kid, "use": "sig",
            "x": b64url(&point[1..33]), "y": b64url(&point[33..65])});
        if let Some(a) = alg {
            k["alg"] = json!(a);
        }
        k
    }

    /// `header.payload.signature` with `header` and `claims` written exactly as given.
    pub fn sign(&self, header: &Value, claims: &Value) -> String {
        let head = b64url(header.to_string().as_bytes());
        let body = b64url(claims.to_string().as_bytes());
        let sig = self
            .pair
            .sign(&self.rng, format!("{head}.{body}").as_bytes())
            .unwrap();
        format!("{head}.{body}.{}", b64url(sig.as_ref()))
    }

    /// A valid header and signature around a payload this key never signed.
    pub fn sign_then_swap_payload(&self, header: &Value, claims: &Value, forged: &Value) -> String {
        let good = self.sign(header, claims);
        let parts: Vec<&str> = good.split('.').collect();
        format!(
            "{}.{}.{}",
            parts[0],
            b64url(forged.to_string().as_bytes()),
            parts[2]
        )
    }
}
