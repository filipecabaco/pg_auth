//! Bearer token verification for `authz.configure(subject => 'jwt_signed')`.
//!
//! Pure Rust with no access to Postgres, so it can forbid `unsafe` outright. Supports
//! HS256 (`kty: oct`) and ES256 (`kty: EC`, `crv: P-256`) keys from a JSON Web Key Set. Every
//! other algorithm, including `none`, is rejected.

#![forbid(unsafe_code)]

use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use serde_json::Value;

/// Verify `token` against `jwks` at time `now` (unix seconds) and return its claims.
/// The error says why the token was rejected; it is shown by `authz.current_subject()`.
pub fn verify(token: &str, jwks: &Value, now: i64, skew: i64) -> Result<Value, String> {
    let mut parts = token.split('.');
    let (Some(header_b64), Some(claims_b64), Some(sig_b64), None) = (parts.next(), parts.next(), parts.next(), parts.next()) else {
        return Err("token is not a JWS compact serialization (header.claims.signature)".into());
    };

    let header: Value = decode_json(header_b64).map_err(|e| format!("header: {e}"))?;
    let alg = header.get("alg").and_then(Value::as_str).unwrap_or("");
    let kid = header.get("kid").and_then(Value::as_str);
    let signature = URL_SAFE_NO_PAD.decode(sig_b64).map_err(|_| "signature is not base64url".to_string())?;
    let signed = format!("{header_b64}.{claims_b64}");

    let keys = jwks.get("keys").and_then(Value::as_array).ok_or("jwks has no keys")?;
    let candidates: Vec<&Value> = keys.iter().filter(|k| kid.is_none() || k.get("kid").and_then(Value::as_str) == kid).collect();
    if candidates.is_empty() {
        return Err(format!("no key in jwks matches kid {:?}", kid.unwrap_or("")));
    }

    let verified = match alg {
        "HS256" => candidates.iter().any(|k| verify_hs256(k, signed.as_bytes(), &signature)),
        "ES256" => candidates.iter().any(|k| verify_es256(k, signed.as_bytes(), &signature)),
        other => return Err(format!("algorithm {other:?} is not accepted (use HS256 or ES256)")),
    };
    if !verified {
        return Err("signature does not match any key".into());
    }

    let claims: Value = decode_json(claims_b64).map_err(|e| format!("claims: {e}"))?;
    match claims.get("exp").and_then(Value::as_i64) {
        None => return Err("token has no exp claim".into()),
        Some(exp) if now > exp + skew => return Err(format!("token expired {}s ago", now - exp)),
        Some(_) => {}
    }
    if let Some(nbf) = claims.get("nbf").and_then(Value::as_i64)
        && now + skew < nbf
    {
        return Err("token is not valid yet (nbf)".into());
    }
    Ok(claims)
}

fn decode_json(part: &str) -> Result<Value, String> {
    let bytes = URL_SAFE_NO_PAD.decode(part).map_err(|_| "not base64url".to_string())?;
    serde_json::from_slice(&bytes).map_err(|_| "not JSON".to_string())
}

fn key_bytes(key: &Value, field: &str) -> Option<Vec<u8>> {
    URL_SAFE_NO_PAD.decode(key.get(field)?.as_str()?).ok()
}

fn verify_hs256(key: &Value, signed: &[u8], signature: &[u8]) -> bool {
    use hmac::{KeyInit, Mac};
    if key.get("kty").and_then(Value::as_str) != Some("oct") {
        return false;
    }
    let Some(secret) = key_bytes(key, "k") else { return false };
    let Ok(mut mac) = hmac::Hmac::<sha2::Sha256>::new_from_slice(&secret) else { return false };
    mac.update(signed);
    // Constant-time comparison.
    mac.verify_slice(signature).is_ok()
}

fn verify_es256(key: &Value, signed: &[u8], signature: &[u8]) -> bool {
    use p256::ecdsa::signature::Verifier;
    use p256::ecdsa::{Signature, VerifyingKey};
    if key.get("kty").and_then(Value::as_str) != Some("EC") || key.get("crv").and_then(Value::as_str) != Some("P-256") {
        return false;
    }
    let (Some(x), Some(y)) = (key_bytes(key, "x"), key_bytes(key, "y")) else { return false };
    if x.len() != 32 || y.len() != 32 {
        return false;
    }
    let mut sec1 = Vec::with_capacity(65);
    sec1.push(0x04);
    sec1.extend_from_slice(&x);
    sec1.extend_from_slice(&y);
    let Ok(vk) = VerifyingKey::from_sec1_bytes(&sec1) else { return false };
    let Ok(sig) = Signature::from_slice(signature) else { return false };
    vk.verify(signed, &sig).is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn b64(v: &[u8]) -> String {
        URL_SAFE_NO_PAD.encode(v)
    }

    pub fn hs256_token(secret: &[u8], header: &Value, claims: &Value) -> String {
        use hmac::{KeyInit, Mac};
        let signed = format!("{}.{}", b64(header.to_string().as_bytes()), b64(claims.to_string().as_bytes()));
        let mut mac = hmac::Hmac::<sha2::Sha256>::new_from_slice(secret).unwrap();
        mac.update(signed.as_bytes());
        format!("{signed}.{}", b64(&mac.finalize().into_bytes()))
    }

    fn jwks(secret: &[u8]) -> Value {
        json!({"keys": [{"kty": "oct", "kid": "k1", "k": b64(secret)}]})
    }

    #[test]
    fn accepts_valid_hs256() {
        let t = hs256_token(b"secret", &json!({"alg": "HS256", "kid": "k1"}), &json!({"sub": "alice", "exp": 2000}));
        assert_eq!(verify(&t, &jwks(b"secret"), 1000, 30).unwrap()["sub"], "alice");
    }

    #[test]
    fn rejects_wrong_key_expired_none_and_missing_exp() {
        let ok_claims = json!({"sub": "alice", "exp": 2000});
        let t = hs256_token(b"other", &json!({"alg": "HS256"}), &ok_claims);
        assert!(verify(&t, &jwks(b"secret"), 1000, 30).unwrap_err().contains("signature"));

        let t = hs256_token(b"secret", &json!({"alg": "HS256"}), &json!({"sub": "alice", "exp": 900}));
        assert!(verify(&t, &jwks(b"secret"), 1000, 30).unwrap_err().contains("expired"));

        let t = hs256_token(b"secret", &json!({"alg": "none"}), &ok_claims);
        assert!(verify(&t, &jwks(b"secret"), 1000, 30).unwrap_err().contains("not accepted"));

        let t = hs256_token(b"secret", &json!({"alg": "HS256"}), &json!({"sub": "alice"}));
        assert!(verify(&t, &jwks(b"secret"), 1000, 30).unwrap_err().contains("no exp"));
    }

    #[test]
    fn rejects_tampered_claims() {
        let t = hs256_token(b"secret", &json!({"alg": "HS256"}), &json!({"sub": "alice", "exp": 2000}));
        let parts: Vec<&str> = t.split('.').collect();
        let forged = format!("{}.{}.{}", parts[0], b64(json!({"sub": "admin", "exp": 2000}).to_string().as_bytes()), parts[2]);
        assert!(verify(&forged, &jwks(b"secret"), 1000, 30).is_err());
    }

    #[test]
    fn accepts_valid_es256_and_rejects_hs256_confusion() {
        use p256::ecdsa::signature::Signer;
        use p256::ecdsa::{Signature, SigningKey};
        let sk = SigningKey::from_slice(&[7u8; 32]).unwrap();
        let point = sk.verifying_key().to_sec1_point(false);
        let key = json!({"kty": "EC", "crv": "P-256", "x": b64(point.x().unwrap()), "y": b64(point.y().unwrap())});
        let set = json!({"keys": [key]});

        let signed = format!(
            "{}.{}",
            b64(json!({"alg": "ES256"}).to_string().as_bytes()),
            b64(json!({"sub": "bob", "exp": 2000}).to_string().as_bytes())
        );
        let sig: Signature = sk.sign(signed.as_bytes());
        let token = format!("{signed}.{}", b64(&sig.to_bytes()));
        assert_eq!(verify(&token, &set, 1000, 30).unwrap()["sub"], "bob");

        // An HS256 token must not verify against an EC key (algorithm confusion).
        let t = hs256_token(b"x", &json!({"alg": "HS256"}), &json!({"sub": "bob", "exp": 2000}));
        assert!(verify(&t, &set, 1000, 30).is_err());
    }
}
