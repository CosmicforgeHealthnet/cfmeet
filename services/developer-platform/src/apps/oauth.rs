//! The public OAuth endpoints (integration contract §5; RFC 6749 / 7009 / 8414 wire format).
//!
//! Anonymous through the API Gateway: the client authenticates with its client secret
//! (`client_secret_basic` or `client_secret_post`) or presents an access token, and
//! Platform Access — the token authority — verifies both live. The Developer Platform only
//! relays: it never verifies a secret or a token and stores neither.
//!
//! * `POST /v1/oauth/token` — `application/x-www-form-urlencoded`,
//!   `grant_type=authorization_code|refresh_token` → Platform Access
//!   `/internal/v1/oauth/token`; snake_case JSON out.
//! * `POST /v1/oauth/revoke` — always 200 unless the client fails authentication.
//! * `GET /v1/oauth/userinfo` — `Bearer cfoa_…` → Platform Access introspection → Meeting
//!   Service profile → `{sub, name, picture, email?}`.
//! * `GET /v1/oauth/metadata` — RFC 8414-style metadata.
//!
//! Every answer carries `Cache-Control: no-store` and `Pragma: no-cache`; errors are
//! `{error, error_description}` with fixed descriptions (nothing from the request or an
//! authority is echoed).

use axum::{
    body::Bytes,
    http::{header, HeaderMap, HeaderValue, StatusCode},
    response::{IntoResponse, Response},
    Json,
};
use base64::{engine::general_purpose::STANDARD, Engine};
use serde_json::{json, Map, Value};
use uuid::Uuid;

use super::{manifest, port::OAuthReply};
use crate::{domain::current_context, state::AppState};

/// RFC 6749 §5.2 / RFC 6750 §3.1 error codes this endpoint answers with.
const RFC_ERRORS: &[&str] = &[
    "invalid_request",
    "invalid_client",
    "invalid_grant",
    "unauthorized_client",
    "unsupported_grant_type",
    "invalid_scope",
    "invalid_token",
    "insufficient_scope",
    "temporarily_unavailable",
    "slow_down",
];

fn description(code: &str) -> &'static str {
    match code {
        "invalid_request" => "The request is missing a parameter or is malformed.",
        "invalid_client" => "Client authentication failed.",
        "invalid_grant" => {
            "The authorization code or refresh token is invalid, expired or revoked."
        }
        "unauthorized_client" => "The client is not authorized to use this grant type.",
        "unsupported_grant_type" => "The grant type is not supported.",
        "invalid_scope" => "The requested scope is invalid.",
        "invalid_token" => "The access token is invalid, expired or revoked.",
        "insufficient_scope" => "The access token lacks the required scope.",
        "slow_down" => "Too many requests; retry later.",
        _ => "The service is temporarily unavailable.",
    }
}

fn no_store(mut r: Response) -> Response {
    let h = r.headers_mut();
    h.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    h.insert(header::PRAGMA, HeaderValue::from_static("no-cache"));
    if let Some(c) = current_context() {
        if let Ok(v) = HeaderValue::from_str(c.request_id.as_str()) {
            h.insert("x-request-id", v);
        }
    }
    r
}

/// An RFC error response.
pub fn oauth_error(code: &str) -> Response {
    let code = if RFC_ERRORS.contains(&code) {
        code
    } else {
        "invalid_request"
    };
    let status = match code {
        "invalid_client" | "invalid_token" => StatusCode::UNAUTHORIZED,
        "insufficient_scope" => StatusCode::FORBIDDEN,
        "slow_down" => StatusCode::TOO_MANY_REQUESTS,
        "temporarily_unavailable" => StatusCode::SERVICE_UNAVAILABLE,
        _ => StatusCode::BAD_REQUEST,
    };
    let mut r = (
        status,
        Json(json!({ "error": code, "error_description": description(code) })),
    )
        .into_response();
    match code {
        "invalid_client" => {
            r.headers_mut().insert(
                header::WWW_AUTHENTICATE,
                HeaderValue::from_static("Basic realm=\"cfmeet\""),
            );
        }
        "invalid_token" => {
            r.headers_mut().insert(
                header::WWW_AUTHENTICATE,
                HeaderValue::from_static("Bearer realm=\"cfmeet\", error=\"invalid_token\""),
            );
        }
        "insufficient_scope" => {
            r.headers_mut().insert(
                header::WWW_AUTHENTICATE,
                HeaderValue::from_static("Bearer realm=\"cfmeet\", error=\"insufficient_scope\""),
            );
        }
        _ => {}
    }
    no_store(r)
}

fn ok(body: Value) -> Response {
    no_store((StatusCode::OK, Json(body)).into_response())
}

/// Percent-decodes one `application/x-www-form-urlencoded` component (`+` is a space).
pub fn form_decode(s: &str) -> Option<String> {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        match b[i] {
            b'+' => out.push(b' '),
            b'%' => {
                let hex = b.get(i + 1..i + 3)?;
                out.push(u8::from_str_radix(std::str::from_utf8(hex).ok()?, 16).ok()?);
                i += 2;
            }
            c => out.push(c),
        }
        i += 1;
    }
    String::from_utf8(out).ok()
}

/// Parses a form body. Repeated parameters are refused (RFC 6749 §3.2: a parameter MUST
/// NOT be included more than once); empty values count as absent.
pub fn parse_form(body: &[u8]) -> Option<Map<String, Value>> {
    let text = std::str::from_utf8(body).ok()?;
    let mut out = Map::new();
    for pair in text.split('&').filter(|p| !p.is_empty()) {
        let (k, v) = pair.split_once('=').unwrap_or((pair, ""));
        let (k, v) = (form_decode(k)?, form_decode(v)?);
        if out.contains_key(&k) {
            return None;
        }
        if !v.is_empty() {
            out.insert(k, Value::String(v));
        }
    }
    Some(out)
}

fn form_content(headers: &HeaderMap) -> bool {
    headers
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.split(';').next())
        .is_some_and(|v| {
            v.trim()
                .eq_ignore_ascii_case("application/x-www-form-urlencoded")
        })
}

/// The `Authorization` header (a name, so the gateway hop never needs to know it).
fn authorization(headers: &HeaderMap) -> Option<&str> {
    headers.get("authorization").and_then(|v| v.to_str().ok())
}

/// `scheme token` of an `Authorization` value (scheme case-insensitive).
fn scheme<'a>(value: &'a str, name: &str) -> Option<&'a str> {
    let (s, rest) = value.split_once(' ')?;
    s.eq_ignore_ascii_case(name).then(|| rest.trim())
}

fn bounded(v: &str, max: usize) -> bool {
    !v.is_empty() && v.len() <= max && v.bytes().all(|b| (0x20..=0x7e).contains(&b))
}

/// The authenticated-by-Platform-Access client of a request: `(client id, secret)`.
/// `Err(code)`: an RFC error.
pub fn client_credentials(
    headers: &HeaderMap,
    form: &Map<String, Value>,
) -> Result<(Uuid, String), &'static str> {
    let text = |k: &str| form.get(k).and_then(Value::as_str);
    let basic = match authorization(headers) {
        None => None,
        Some(v) => {
            let encoded = scheme(v, "basic").ok_or("invalid_client")?;
            let decoded = STANDARD
                .decode(encoded)
                .ok()
                .and_then(|b| String::from_utf8(b).ok())
                .ok_or("invalid_client")?;
            let (id, secret) = decoded.split_once(':').ok_or("invalid_client")?;
            // RFC 6749 §2.3.1: both parts are form-encoded before Base64.
            Some((
                form_decode(id).ok_or("invalid_client")?,
                form_decode(secret).ok_or("invalid_client")?,
            ))
        }
    };
    let (id, secret) = match (basic, text("client_id"), text("client_secret")) {
        // One authentication method per request.
        (Some(_), _, Some(_)) => return Err("invalid_request"),
        (Some((id, _)), Some(body_id), None) if body_id != id => return Err("invalid_request"),
        (Some((id, secret)), _, None) => (id, secret),
        (None, Some(id), Some(secret)) => (id.to_string(), secret.to_string()),
        (None, _, _) => return Err("invalid_client"),
    };
    let id = Uuid::parse_str(&id).map_err(|_| "invalid_client")?;
    if !bounded(&secret, 512) {
        return Err("invalid_client");
    }
    Ok((id, secret))
}

fn token_refusal(status: u16, code: &str) -> &'static str {
    match code {
        "invalid_grant" => "invalid_grant",
        "invalid_client" => "invalid_client",
        "unsupported_grant_type" => "unsupported_grant_type",
        "invalid_scope" => "invalid_scope",
        "unauthorized_client" => "unauthorized_client",
        "invalid_request" => "invalid_request",
        _ if matches!(status, 401 | 403 | 404) => "invalid_client",
        _ => "invalid_request",
    }
}

/// Platform Access's token answer in the RFC wire format.
fn token_body(pa: &Value) -> Option<Value> {
    let text = |k: &str| pa.get(k).and_then(Value::as_str);
    let mut out = json!({
        "access_token": text("accessToken")?,
        "token_type": "Bearer",
        "expires_in": pa.get("expiresIn").and_then(Value::as_u64).unwrap_or(3600),
    });
    if let Some(r) = text("refreshToken") {
        out["refresh_token"] = json!(r);
    }
    match pa.get("scope") {
        Some(Value::String(s)) => out["scope"] = json!(s),
        Some(Value::Array(a)) => {
            out["scope"] = json!(a
                .iter()
                .filter_map(Value::as_str)
                .collect::<Vec<_>>()
                .join(" "))
        }
        _ => {}
    }
    if let Some(g) = pa.get("grant").filter(|g| g.is_object()) {
        let mut installation = json!({
            "id": g.get("id").and_then(Value::as_str)?,
            "target": g.get("target").and_then(Value::as_str)?,
        });
        if let Some(o) = g.get("organizationId").and_then(Value::as_str) {
            installation["organization_id"] = json!(o);
        }
        out["installation"] = installation;
    }
    if let Some(b) = text("botToken") {
        out["bot_token"] = json!(b);
    }
    Some(out)
}

/// `POST /v1/oauth/token`.
pub async fn token(state: &AppState, headers: &HeaderMap, body: &Bytes) -> Response {
    if !form_content(headers) {
        return oauth_error("invalid_request");
    }
    let Some(form) = parse_form(body) else {
        return oauth_error("invalid_request");
    };
    let (client_id, client_secret) = match client_credentials(headers, &form) {
        Ok(c) => c,
        Err(code) => return oauth_error(code),
    };
    if !state.apps.limiter.allow(&format!("token:{client_id}")) {
        return oauth_error("slow_down");
    }
    let text = |k: &str| {
        form.get(k)
            .and_then(Value::as_str)
            .filter(|v| bounded(v, 4096))
    };
    let mut request = json!({ "clientId": client_id, "clientSecret": client_secret });
    match form.get("grant_type").and_then(Value::as_str) {
        Some("authorization_code") => {
            // PKCE is mandatory (S256 only, checked by Platform Access against the code).
            let (Some(code), Some(redirect), Some(verifier)) =
                (text("code"), text("redirect_uri"), text("code_verifier"))
            else {
                return oauth_error("invalid_request");
            };
            request["grantType"] = json!("authorization_code");
            request["code"] = json!(code);
            request["redirectUri"] = json!(redirect);
            request["codeVerifier"] = json!(verifier);
        }
        Some("refresh_token") => {
            let Some(refresh) = text("refresh_token") else {
                return oauth_error("invalid_request");
            };
            request["grantType"] = json!("refresh_token");
            request["refreshToken"] = json!(refresh);
        }
        Some(_) => return oauth_error("unsupported_grant_type"),
        None => return oauth_error("invalid_request"),
    }
    let correlation = current_context()
        .map(|c| c.correlation_id)
        .unwrap_or_else(cf_service_auth::CorrelationId::generate);
    match state.oauth.token(&request, &correlation).await {
        Ok(OAuthReply::Ok { body, .. }) => match token_body(&body) {
            Some(b) => ok(b),
            None => {
                tracing::warn!("malformed token authority answer");
                oauth_error("temporarily_unavailable")
            }
        },
        Ok(OAuthReply::Refused { status, code }) => oauth_error(token_refusal(status, &code)),
        Err(e) => {
            tracing::warn!(error = %e, "token authority unavailable");
            oauth_error("temporarily_unavailable")
        }
    }
}

/// `POST /v1/oauth/revoke` (RFC 7009: 200 whatever the token's state).
pub async fn revoke(state: &AppState, headers: &HeaderMap, body: &Bytes) -> Response {
    if !form_content(headers) {
        return oauth_error("invalid_request");
    }
    let Some(form) = parse_form(body) else {
        return oauth_error("invalid_request");
    };
    let (client_id, client_secret) = match client_credentials(headers, &form) {
        Ok(c) => c,
        Err(code) => return oauth_error(code),
    };
    if !state.apps.limiter.allow(&format!("revoke:{client_id}")) {
        return oauth_error("slow_down");
    }
    let Some(presented) = form
        .get("token")
        .and_then(Value::as_str)
        .filter(|v| bounded(v, 4096))
    else {
        return oauth_error("invalid_request");
    };
    let correlation = current_context()
        .map(|c| c.correlation_id)
        .unwrap_or_else(cf_service_auth::CorrelationId::generate);
    let request =
        json!({ "clientId": client_id, "clientSecret": client_secret, "token": presented });
    match state.oauth.revoke(&request, &correlation).await {
        Ok(OAuthReply::Refused { status, code })
            if code == "invalid_client" || matches!(status, 401) =>
        {
            oauth_error("invalid_client")
        }
        Ok(_) => no_store(StatusCode::OK.into_response()),
        Err(e) => {
            tracing::warn!(error = %e, "token authority unavailable");
            oauth_error("temporarily_unavailable")
        }
    }
}

fn scope_list(v: Option<&Value>) -> Vec<String> {
    match v {
        Some(Value::Array(a)) => a
            .iter()
            .filter_map(Value::as_str)
            .map(str::to_string)
            .collect(),
        Some(Value::String(s)) => s.split_whitespace().map(str::to_string).collect(),
        _ => vec![],
    }
}

/// `GET /v1/oauth/userinfo`.
pub async fn userinfo(state: &AppState, headers: &HeaderMap) -> Response {
    if !state.apps.limiter.allow_route("userinfo") {
        return oauth_error("slow_down");
    }
    let Some(access) = authorization(headers)
        .and_then(|v| scheme(v, "bearer"))
        .filter(|t| t.starts_with("cfoa_") && bounded(t, 4096))
    else {
        return oauth_error("invalid_token");
    };
    let correlation = current_context()
        .map(|c| c.correlation_id)
        .unwrap_or_else(cf_service_auth::CorrelationId::generate);
    let info = match state.oauth.introspect(access, &correlation).await {
        Ok(OAuthReply::Ok { body, .. }) => body,
        Ok(OAuthReply::Refused { .. }) => return oauth_error("invalid_token"),
        Err(e) => {
            tracing::warn!(error = %e, "token authority unavailable");
            return oauth_error("temporarily_unavailable");
        }
    };
    if info.get("active").and_then(Value::as_bool) != Some(true) {
        return oauth_error("invalid_token");
    }
    let scopes = scope_list(info.get("scopes").or_else(|| info.get("scope")));
    let (Some(sub), Some(account)) = (
        info.get("sub").and_then(Value::as_str),
        info.get("accountId")
            .and_then(Value::as_str)
            .and_then(|a| Uuid::parse_str(a).ok()),
    ) else {
        // A bot / organization credential has no user profile.
        return oauth_error("invalid_token");
    };
    if !scopes.iter().any(|s| s == "profile:read") {
        return oauth_error("insufficient_scope");
    }
    let granted: Vec<String> = scopes
        .iter()
        .filter(|s| matches!(s.as_str(), "profile:read" | "profile:email"))
        .cloned()
        .collect();
    let profile = match state
        .account_profiles
        .userinfo(account, &granted, &correlation)
        .await
    {
        Ok(Some(p)) => p,
        Ok(None) => return oauth_error("invalid_token"),
        Err(e) => {
            tracing::warn!(error = %e, "account authority unavailable");
            return oauth_error("temporarily_unavailable");
        }
    };
    let text = |k: &str| {
        profile
            .get(k)
            .filter(|v| v.is_string())
            .cloned()
            .unwrap_or(Value::Null)
    };
    let mut out = json!({ "sub": sub, "name": text("displayName"), "picture": text("avatarUrl") });
    if granted.iter().any(|s| s == "profile:email") {
        if let Some(e) = profile.get("email").and_then(Value::as_str) {
            out["email"] = json!(e);
        }
    }
    ok(out)
}

/// `GET /v1/oauth/metadata` (RFC 8414 shape).
pub fn metadata(state: &AppState) -> Response {
    let base = &state.apps.endpoints.issuer;
    ok(json!({
        "issuer": base,
        "authorization_endpoint": state.apps.endpoints.authorization_endpoint,
        "token_endpoint": format!("{base}/v1/oauth/token"),
        "revocation_endpoint": format!("{base}/v1/oauth/revoke"),
        "userinfo_endpoint": format!("{base}/v1/oauth/userinfo"),
        "scopes_supported": manifest::scopes_supported(),
        "response_types_supported": ["code"],
        "grant_types_supported": ["authorization_code", "refresh_token"],
        "code_challenge_methods_supported": ["S256"],
        "token_endpoint_auth_methods_supported": ["client_secret_basic", "client_secret_post"],
        "revocation_endpoint_auth_methods_supported": ["client_secret_basic", "client_secret_post"],
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn forms_are_decoded_and_repeats_refused() {
        let f =
            parse_form(b"grant_type=authorization_code&code=a%2Bb+c&redirect_uri=https%3A%2F%2Fx")
                .unwrap();
        assert_eq!(f["code"], "a+b c");
        assert_eq!(f["redirect_uri"], "https://x");
        assert!(parse_form(b"code=a&code=b").is_none());
        assert!(parse_form(b"code=%zz").is_none());
        assert!(parse_form(b"").unwrap().is_empty());
    }

    #[test]
    fn client_authentication_is_one_method() {
        let id = Uuid::new_v4();
        let mut h = HeaderMap::new();
        let basic = STANDARD.encode(format!("{id}:cfcs_sbx_s%3Ag"));
        h.insert("authorization", format!("Basic {basic}").parse().unwrap());
        let (got, secret) = client_credentials(&h, &Map::new()).unwrap();
        assert_eq!((got, secret.as_str()), (id, "cfcs_sbx_s:g"));
        let both = parse_form(format!("client_secret=x&client_id={id}").as_bytes()).unwrap();
        assert_eq!(client_credentials(&h, &both), Err("invalid_request"));
        let post = parse_form(format!("client_secret=cfcs_x&client_id={id}").as_bytes()).unwrap();
        assert!(client_credentials(&HeaderMap::new(), &post).is_ok());
        assert_eq!(
            client_credentials(&HeaderMap::new(), &Map::new()),
            Err("invalid_client")
        );
        let bad = parse_form(b"client_secret=x&client_id=not-a-uuid").unwrap();
        assert_eq!(
            client_credentials(&HeaderMap::new(), &bad),
            Err("invalid_client")
        );
    }

    #[test]
    fn token_answers_are_snake_case() {
        let pa = json!({ "accessToken": "cfoa_sbx_a", "tokenType": "Bearer", "expiresIn": 3600,
            "refreshToken": "cfor_sbx_r", "scope": ["profile:read", "meetings:write"],
            "grant": { "id": "g", "target": "ORGANIZATION", "organizationId": "o" },
            "botToken": "cfob_sbx_b" });
        let b = token_body(&pa).unwrap();
        assert_eq!(b["scope"], "profile:read meetings:write");
        assert_eq!(b["installation"]["organization_id"], "o");
        assert_eq!(b["bot_token"], "cfob_sbx_b");
        assert!(token_body(&json!({})).is_none());
        assert_eq!(token_refusal(400, "invalid_grant"), "invalid_grant");
        assert_eq!(token_refusal(404, "NOT_FOUND"), "invalid_client");
        assert_eq!(token_refusal(400, "VALIDATION_ERROR"), "invalid_request");
    }
}
