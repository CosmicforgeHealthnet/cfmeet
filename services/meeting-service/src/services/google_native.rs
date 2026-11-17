//! Gap audit A4: native (mobile) Google sign-in. A Google ID token obtained by the
//! platform SDK is presented to the API, so unlike the web code flow its signature IS the
//! proof: it is verified (RS256) against Google's published keys
//! (`https://www.googleapis.com/oauth2/v3/certs`, cached for an hour; `GOOGLE_JWKS_URL`
//! overrides it for tests), then every claim is validated like the web flow. Accepted
//! audiences: the web client (`GOOGLE_CLIENT_ID`) and the native clients listed in
//! `GOOGLE_NATIVE_CLIENT_IDS` (comma separated); an Android token's `azp` is its own
//! client id.
use std::sync::Mutex;
use std::time::{Duration, Instant};

use jsonwebtoken::{decode, decode_header, Algorithm, DecodingKey, Validation};
use serde_json::Value;

use crate::{
    error::{ApiError, ApiResult},
    handlers::auth::{validate_google_claims, GoogleIdentity},
};

static JWKS: Mutex<Option<(Instant, Value)>> = Mutex::new(None);
const JWKS_TTL: Duration = Duration::from_secs(3600);

fn jwks_url() -> String {
    std::env::var("GOOGLE_JWKS_URL")
        .unwrap_or_else(|_| "https://www.googleapis.com/oauth2/v3/certs".into())
}

async fn jwks(refresh: bool) -> ApiResult<Value> {
    if !refresh {
        if let Some((at, keys)) = JWKS.lock().unwrap_or_else(|p| p.into_inner()).as_ref() {
            if at.elapsed() < JWKS_TTL {
                return Ok(keys.clone());
            }
        }
    }
    let keys: Value = reqwest::Client::new()
        .get(jwks_url())
        .timeout(Duration::from_secs(5))
        .send()
        .await
        .map_err(|_| {
            ApiError::domain(
                crate::error::ErrorCode::ServiceUnavailable,
                "Google keys unavailable",
            )
        })?
        .json()
        .await
        .map_err(|_| {
            ApiError::domain(
                crate::error::ErrorCode::ServiceUnavailable,
                "Google keys unavailable",
            )
        })?;
    *JWKS.lock().unwrap_or_else(|p| p.into_inner()) = Some((Instant::now(), keys.clone()));
    Ok(keys)
}

/// The accepted audiences: the web client and the configured native clients.
pub fn allowed_clients(web_client_id: &str) -> Vec<String> {
    let mut ids = vec![web_client_id.to_string()];
    if let Ok(list) = std::env::var("GOOGLE_NATIVE_CLIENT_IDS") {
        ids.extend(
            list.split(',')
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(str::to_string),
        );
    }
    ids
}

/// Verifies a Google ID token from a native SDK and returns the identity.
pub async fn verify(
    id_token: &str,
    web_client_id: &str,
    nonce: Option<&str>,
) -> ApiResult<GoogleIdentity> {
    let rejected = |r: &str| ApiError::Unauthorized(format!("ID token rejected: {r}"));
    let header = decode_header(id_token).map_err(|_| rejected("format"))?;
    if header.alg != Algorithm::RS256 {
        return Err(rejected("algorithm"));
    }
    let kid = header.kid.ok_or_else(|| rejected("key"))?;
    let mut keys = jwks(false).await?;
    let find = |keys: &Value| {
        keys["keys"].as_array().and_then(|l| {
            l.iter()
                .find(|k| k["kid"].as_str() == Some(kid.as_str()))
                .cloned()
        })
    };
    let key = match find(&keys) {
        Some(k) => k,
        None => {
            // Google rotates keys: refresh once before refusing.
            keys = jwks(true).await?;
            find(&keys).ok_or_else(|| rejected("key"))?
        }
    };
    let (Some(n), Some(e)) = (key["n"].as_str(), key["e"].as_str()) else {
        return Err(rejected("key"));
    };
    let decoding = DecodingKey::from_rsa_components(n, e).map_err(|_| rejected("key"))?;
    let mut validation = Validation::new(Algorithm::RS256);
    // Claims are validated below exactly like the web flow (issuer, audience, expiry...).
    validation.validate_exp = false;
    validation.validate_aud = false;
    validation.required_spec_claims.clear();
    let mut claims = decode::<Value>(id_token, &decoding, &validation)
        .map_err(|_| rejected("signature"))?
        .claims;
    let now = chrono::Utc::now().timestamp();
    let allowed = allowed_clients(web_client_id);
    // An Android token names its own client as `azp` (the audience is the web client).
    if claims["azp"]
        .as_str()
        .is_some_and(|a| allowed.iter().any(|c| c == a))
    {
        if let Some(map) = claims.as_object_mut() {
            map.remove("azp");
        }
    }
    let mut last = "audience";
    for client in &allowed {
        match validate_google_claims(&claims, client, nonce, now) {
            Ok(identity) => return Ok(identity),
            Err(reason) => last = reason,
        }
    }
    Err(rejected(last))
}
