//! In-meeting app context tokens (ADR 0065 I7): a 5-minute Ed25519 JWS the Meeting Service
//! mints for an app's meeting surface (the iframe panel), verifiable with the public JWKS
//! (`GET /api/v2/app-context-keys`).
//!
//! Header `{alg: EdDSA, typ: JWT, kid}`; `kid` is the RFC 7638 thumbprint of the public
//! key. Claims:
//!
//! | Claim | Meaning |
//! |---|---|
//! | `iss` | `cfmeet.meeting-service` |
//! | `aud` | the app's OAuth `client_id` (= app id) |
//! | `iat` / `nbf` / `exp` | `exp = iat + 300` |
//! | `jti` | unique token id |
//! | `typ` | `app-context` |
//! | `conference` | the conference id |
//! | `participant` | pairwise participant id: HMAC(key, app, participant), base64url — stable for the (app, participant) pair, unlinkable across apps |
//! | `role` | the participant's conference role |
//! | `sub` | the user's pairwise OAuth subject for the app — ONLY when that user granted the app |
//!
//! The private key is a PKCS#8 PEM file (`MEETING_APP_CONTEXT_SIGNING_KEY_FILE`); the
//! pairwise key is derived from it (domain-separated), so rotating the key also rotates
//! pairwise participant ids (they are conference-scoped already).

use std::path::Path;

use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
use chrono::Utc;
use ed25519_dalek::{pkcs8::DecodePrivateKey, Signer, SigningKey};
use hmac::{Hmac, Mac};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use uuid::Uuid;
use zeroize::Zeroizing;

/// Lifetime of an app-context token.
pub const CONTEXT_TTL_SECONDS: i64 = 300;
pub const ISSUER: &str = "cfmeet.meeting-service";

/// The Meeting Service's app-context signing key.
pub struct AppContextSigner {
    kid: String,
    signing: SigningKey,
    pairwise: Zeroizing<[u8; 32]>,
}

impl std::fmt::Debug for AppContextSigner {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AppContextSigner")
            .field("kid", &self.kid)
            .field("private_key", &"[REDACTED]")
            .finish()
    }
}

impl AppContextSigner {
    pub fn from_signing_key(signing: SigningKey) -> Self {
        let x = URL_SAFE_NO_PAD.encode(signing.verifying_key().to_bytes());
        // RFC 7638: members in lexicographic order, no whitespace.
        let thumbprint = Sha256::digest(format!(r#"{{"crv":"Ed25519","kty":"OKP","x":"{x}"}}"#));
        let mut derive = Sha256::new();
        derive.update(b"cfmeet-app-context-pairwise-v1");
        derive.update(signing.to_bytes());
        Self {
            kid: URL_SAFE_NO_PAD.encode(thumbprint),
            pairwise: Zeroizing::new(derive.finalize().into()),
            signing,
        }
    }

    pub fn from_pkcs8_pem(pem: &str) -> Result<Self, String> {
        SigningKey::from_pkcs8_pem(pem)
            .map(Self::from_signing_key)
            .map_err(|_| "the app-context key is not an Ed25519 PKCS#8 PEM key".to_string())
    }

    pub fn from_pem_file(path: &Path) -> Result<Self, String> {
        let pem = Zeroizing::new(std::fs::read_to_string(path).map_err(|_| {
            "MEETING_APP_CONTEXT_SIGNING_KEY_FILE cannot be read".to_string()
        })?);
        Self::from_pkcs8_pem(&pem)
    }

    pub fn kid(&self) -> &str {
        &self.kid
    }

    /// The public JWKS (`{keys:[OKP JWK]}`).
    pub fn jwks(&self) -> Value {
        json!({ "keys": [{
            "kty": "OKP",
            "crv": "Ed25519",
            "x": URL_SAFE_NO_PAD.encode(self.signing.verifying_key().to_bytes()),
            "kid": self.kid,
            "use": "sig",
            "alg": "EdDSA",
        }] })
    }

    /// The pairwise participant id of `participant` for `app`.
    pub fn pairwise_participant(&self, app: Uuid, participant: Uuid) -> String {
        let mut mac =
            Hmac::<Sha256>::new_from_slice(self.pairwise.as_slice()).expect("any key length");
        mac.update(app.as_bytes());
        mac.update(participant.as_bytes());
        URL_SAFE_NO_PAD.encode(mac.finalize().into_bytes())
    }

    /// Signs `claims` (a JSON object) as a compact JWS.
    pub fn sign(&self, claims: &Value) -> String {
        let header = json!({ "alg": "EdDSA", "typ": "JWT", "kid": self.kid });
        let input = format!(
            "{}.{}",
            URL_SAFE_NO_PAD.encode(header.to_string()),
            URL_SAFE_NO_PAD.encode(claims.to_string())
        );
        let signature = self.signing.sign(input.as_bytes());
        format!("{input}.{}", URL_SAFE_NO_PAD.encode(signature.to_bytes()))
    }

    /// Mints the context token for one participant (claims per the module table).
    pub fn mint(
        &self,
        app: Uuid,
        conference: Uuid,
        participant: Uuid,
        role: &str,
        sub: Option<&str>,
    ) -> String {
        let now = Utc::now().timestamp();
        let mut claims = json!({
            "iss": ISSUER,
            "aud": app.to_string(),
            "iat": now,
            "nbf": now,
            "exp": now + CONTEXT_TTL_SECONDS,
            "jti": Uuid::new_v4().to_string(),
            "typ": "app-context",
            "conference": conference.to_string(),
            "participant": self.pairwise_participant(app, participant),
            "role": role,
        });
        if let Some(sub) = sub {
            claims["sub"] = json!(sub);
        }
        self.sign(&claims)
    }
}

/// Verifies a compact JWS against a JWKS and returns its claims (tests and tooling).
pub fn verify(token: &str, jwks: &Value) -> Option<Value> {
    use ed25519_dalek::{Signature, Verifier, VerifyingKey};
    let mut parts = token.split('.');
    let (h, c, s) = (parts.next()?, parts.next()?, parts.next()?);
    if parts.next().is_some() {
        return None;
    }
    let header: Value = serde_json::from_slice(&URL_SAFE_NO_PAD.decode(h).ok()?).ok()?;
    if header.get("alg")?.as_str()? != "EdDSA" {
        return None;
    }
    let kid = header.get("kid")?.as_str()?;
    let key = jwks
        .get("keys")?
        .as_array()?
        .iter()
        .find(|k| k.get("kid").and_then(Value::as_str) == Some(kid))?;
    let x: [u8; 32] = URL_SAFE_NO_PAD
        .decode(key.get("x")?.as_str()?)
        .ok()?
        .try_into()
        .ok()?;
    let sig: [u8; 64] = URL_SAFE_NO_PAD.decode(s).ok()?.try_into().ok()?;
    VerifyingKey::from_bytes(&x)
        .ok()?
        .verify(
            format!("{h}.{c}").as_bytes(),
            &Signature::from_bytes(&sig),
        )
        .ok()?;
    serde_json::from_slice(&URL_SAFE_NO_PAD.decode(c).ok()?).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn signer() -> AppContextSigner {
        AppContextSigner::from_signing_key(SigningKey::from_bytes(&[7u8; 32]))
    }

    #[test]
    fn tokens_verify_with_the_jwks_and_carry_the_claims() {
        let s = signer();
        let (app, conf, part) = (Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4());
        let t = s.mint(app, conf, part, "HOST", None);
        let c = verify(&t, &s.jwks()).unwrap();
        assert_eq!(c["aud"], app.to_string());
        assert_eq!(c["conference"], conf.to_string());
        assert_eq!(c["role"], "HOST");
        assert_eq!(c["exp"].as_i64().unwrap() - c["iat"].as_i64().unwrap(), 300);
        assert!(c.get("sub").is_none());
        assert_ne!(c["participant"], part.to_string(), "pairwise, never the raw id");
        let t = s.mint(app, conf, part, "PARTICIPANT", Some("pairwise-sub"));
        assert_eq!(verify(&t, &s.jwks()).unwrap()["sub"], "pairwise-sub");
        // Another key does not verify it; a tampered token does not verify.
        let other = AppContextSigner::from_signing_key(SigningKey::from_bytes(&[8u8; 32]));
        assert!(verify(&t, &other.jwks()).is_none());
        let mut bad = t.clone();
        bad.insert(t.find('.').unwrap() + 2, 'A');
        assert!(verify(&bad, &s.jwks()).is_none());
    }

    #[test]
    fn pairwise_ids_are_stable_per_app_and_unlinkable_across_apps() {
        let s = signer();
        let p = Uuid::new_v4();
        let (a, b) = (Uuid::new_v4(), Uuid::new_v4());
        assert_eq!(s.pairwise_participant(a, p), s.pairwise_participant(a, p));
        assert_ne!(s.pairwise_participant(a, p), s.pairwise_participant(b, p));
        let jwks = s.jwks();
        assert_eq!(jwks["keys"][0]["kid"], s.kid());
        assert_eq!(jwks["keys"][0]["crv"], "Ed25519");
        assert!(jwks.to_string().len() < 400, "public material only");
        assert!(format!("{s:?}").contains("REDACTED"));
    }
}
