//! The app manifest a developer registers on a project (integration contract §5) and its
//! validation. A stored manifest is always the normalized form below (camelCase), so two
//! equal manifests compare equal and every version is self-describing:
//!
//! ```json
//! { "name", "description", "logoUrl", "privacyUrl", "termsUrl", "redirectUris": [],
//!   "userScopes": [], "botScopes": [], "eventTypes": [],
//!   "meetingSurface": { "url", "origins": [] } | null, "distribution": "PRIVATE|PUBLIC" }
//! ```

use serde::Deserialize;
use serde_json::{json, Value};

use crate::{
    http::error::{ApiError, ErrorCode},
    webhooks::catalog,
};

/// The closed OAuth scope catalogue (Platform Access `identity/oauth_scopes.rs`) a user
/// grant may request (`events:meetings` is an install scope of either kind of grant).
pub const USER_SCOPES: &[&str] = &[
    "profile:read",
    "profile:email",
    "meetings:read",
    "meetings:write",
    "chat:read",
    "chat:write",
    "events:meetings",
];

/// The OAuth scopes of an organization install's bot.
pub const BOT_SCOPES: &[&str] = &["chat:read:bot", "chat:write:bot"];

/// Every scope `/v1/oauth/metadata` advertises.
pub fn scopes_supported() -> Vec<&'static str> {
    USER_SCOPES.iter().chain(BOT_SCOPES).copied().collect()
}

/// At most this many redirect URIs (contract §2).
pub const MAX_REDIRECT_URIS: usize = 10;
/// At most this many meeting-surface origins.
pub const MAX_ORIGINS: usize = 10;
/// Draft test accounts (contract §2: at most 25).
pub const MAX_TEST_ACCOUNTS: usize = 25;

/// The public event families an installed app may receive (the install routing pass).
/// Anything else (calls, SMS, numbers, relay, credentials, usage, …) never crosses an
/// install.
pub fn install_routable(event_type: &str) -> bool {
    matches!(
        catalog::family(event_type),
        "meeting" | "participant" | "artifact" | "message" | "conversation" | "bot" | "app"
    ) && catalog::definition(event_type).is_some()
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct MeetingSurfaceInput {
    pub url: String,
    #[serde(default)]
    pub origins: Vec<String>,
}

/// The `PUT /v1/projects/{projectId}/app` body.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ManifestInput {
    pub name: String,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub logo_url: Option<String>,
    #[serde(default)]
    pub privacy_url: Option<String>,
    #[serde(default)]
    pub terms_url: Option<String>,
    #[serde(default)]
    pub redirect_uris: Vec<String>,
    #[serde(default)]
    pub user_scopes: Vec<String>,
    #[serde(default)]
    pub bot_scopes: Vec<String>,
    #[serde(default)]
    pub event_types: Vec<String>,
    #[serde(default)]
    pub meeting_surface: Option<MeetingSurfaceInput>,
    #[serde(default)]
    pub distribution: Option<String>,
}

fn invalid() -> ApiError {
    ApiError(ErrorCode::ValidationError)
}

fn visible(s: &str) -> bool {
    !s.is_empty() && s.bytes().all(|b| (0x21..=0x7e).contains(&b))
}

/// An absolute `https://` URL (no credentials, no whitespace, at most 2048 bytes).
pub fn https_url(value: &str) -> Result<String, ApiError> {
    let v = value.trim();
    let rest = v.strip_prefix("https://").ok_or_else(invalid)?;
    let host = rest.split(['/', '?', '#']).next().unwrap_or("");
    if v.len() > 2048 || !visible(v) || host.is_empty() || host.contains('@') {
        return Err(invalid());
    }
    Ok(v.to_string())
}

/// Whether `uri` is a loopback redirect (`http://localhost[:port]/…`, `127.0.0.1`).
pub fn is_loopback(uri: &str) -> bool {
    let Some(rest) = uri.strip_prefix("http://") else {
        return false;
    };
    let authority = rest.split('/').next().unwrap_or("");
    let host = authority.rsplit_once(':').map_or(authority, |(h, port)| {
        if !port.is_empty() && port.len() <= 5 && port.bytes().all(|b| b.is_ascii_digit()) {
            h
        } else {
            "\u{0}"
        }
    });
    matches!(host, "localhost" | "127.0.0.1" | "[::1]")
}

/// A redirect URI: exact (no fragment, no wildcard), `https://` or a loopback `http://`.
pub fn redirect_uri(value: &str) -> Result<String, ApiError> {
    let v = value.trim();
    if v.contains('#') || v.contains('*') || v.len() > 2048 || !visible(v) {
        return Err(invalid());
    }
    if is_loopback(v) {
        return Ok(v.to_string());
    }
    https_url(v)
}

/// An https origin (`https://host[:port]`, no path).
pub fn origin(value: &str) -> Result<String, ApiError> {
    let v = https_url(value)?.to_ascii_lowercase();
    let rest = &v["https://".len()..];
    if rest.contains(['/', '?', '#']) || rest.is_empty() {
        return Err(invalid());
    }
    Ok(v)
}

fn origin_of(url: &str) -> String {
    let rest = &url["https://".len()..];
    let host = rest.split(['/', '?', '#']).next().unwrap_or("");
    format!("https://{}", host.to_ascii_lowercase())
}

fn optional_url(v: &Option<String>) -> Result<Value, ApiError> {
    match v.as_deref().map(str::trim) {
        None | Some("") => Ok(Value::Null),
        Some(u) => Ok(Value::String(https_url(u)?)),
    }
}

fn dedup<T: PartialEq + Clone>(items: impl IntoIterator<Item = T>) -> Vec<T> {
    let mut out: Vec<T> = Vec::new();
    for i in items {
        if !out.contains(&i) {
            out.push(i);
        }
    }
    out
}

fn scopes(requested: &[String], allowed: &[&str]) -> Result<Vec<String>, ApiError> {
    if requested.iter().any(|s| !allowed.contains(&s.as_str())) {
        return Err(invalid());
    }
    // Catalogue order: stable and comparable.
    Ok(allowed
        .iter()
        .filter(|a| requested.iter().any(|r| r == *a))
        .map(|s| s.to_string())
        .collect())
}

/// Validates and normalizes a manifest.
pub fn normalize(input: ManifestInput) -> Result<Value, ApiError> {
    let name = crate::control::display_name(&input.name)?;
    let description = match input.description.as_deref().map(str::trim) {
        None | Some("") => Value::Null,
        Some(d)
            if d.chars().count() <= 1000
                && !d.chars().any(|c| c.is_control() && c != '\n') =>
        {
            Value::String(d.to_string())
        }
        Some(_) => return Err(invalid()),
    };
    let redirect_uris = dedup(
        input
            .redirect_uris
            .iter()
            .map(|u| redirect_uri(u))
            .collect::<Result<Vec<_>, _>>()?,
    );
    if redirect_uris.is_empty() || redirect_uris.len() > MAX_REDIRECT_URIS {
        return Err(invalid());
    }
    let user_scopes = scopes(&input.user_scopes, USER_SCOPES)?;
    let bot_scopes = scopes(&input.bot_scopes, BOT_SCOPES)?;
    if user_scopes.is_empty() && bot_scopes.is_empty() {
        return Err(invalid());
    }
    if input.event_types.len() > 64 || input.event_types.iter().any(|t| !install_routable(t)) {
        return Err(invalid());
    }
    let mut event_types = dedup(input.event_types.iter().cloned());
    event_types.sort();
    let meeting_surface = match input.meeting_surface {
        None => Value::Null,
        Some(m) => {
            let url = https_url(&m.url)?;
            let mut origins = dedup(
                m.origins
                    .iter()
                    .map(|o| origin(o))
                    .collect::<Result<Vec<_>, _>>()?,
            );
            let own = origin_of(&url);
            if !origins.contains(&own) {
                origins.insert(0, own);
            }
            if origins.len() > MAX_ORIGINS {
                return Err(invalid());
            }
            json!({ "url": url, "origins": origins })
        }
    };
    let distribution = match input.distribution.as_deref() {
        None | Some("PRIVATE") => "PRIVATE",
        Some("PUBLIC") => "PUBLIC",
        Some(_) => return Err(invalid()),
    };
    Ok(json!({
        "name": name,
        "description": description,
        "logoUrl": optional_url(&input.logo_url)?,
        "privacyUrl": optional_url(&input.privacy_url)?,
        "termsUrl": optional_url(&input.terms_url)?,
        "redirectUris": redirect_uris,
        "userScopes": user_scopes,
        "botScopes": bot_scopes,
        "eventTypes": event_types,
        "meetingSurface": meeting_surface,
        "distribution": distribution,
    }))
}

/// Whether a stored manifest may go to review: no loopback redirect (Platform Access
/// accepts `http://localhost` only while the app is DRAFT), and a PUBLIC app names its
/// privacy policy and terms.
pub fn reviewable(manifest: &Value) -> bool {
    let loopback = strings(&manifest["redirectUris"])
        .iter()
        .any(|u| is_loopback(u));
    let public = manifest["distribution"] == "PUBLIC";
    !loopback
        && (!public || (manifest["privacyUrl"].is_string() && manifest["termsUrl"].is_string()))
}

/// The strings of a JSON array (others ignored).
pub fn strings(v: &Value) -> Vec<String> {
    v.as_array()
        .map(|a| {
            a.iter()
                .filter_map(Value::as_str)
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn input(v: Value) -> Result<Value, ApiError> {
        normalize(serde_json::from_value(v).map_err(|_| invalid())?)
    }

    fn base() -> Value {
        json!({ "name": " Calendar Sync ", "redirectUris": ["https://app.example.com/cb"],
                "userScopes": ["meetings:write", "profile:read"] })
    }

    #[test]
    fn manifests_are_normalized() {
        let m = input(base()).unwrap();
        assert_eq!(m["name"], "Calendar Sync");
        assert_eq!(m["userScopes"], json!(["profile:read", "meetings:write"]));
        assert_eq!(m["distribution"], "PRIVATE");
        assert_eq!(m["meetingSurface"], Value::Null);
        let mut v = base();
        v["meetingSurface"] = json!({ "url": "https://panel.example.com/app", "origins": [] });
        v["eventTypes"] = json!(["meeting.started", "bot.mentioned", "meeting.started"]);
        let m = input(v).unwrap();
        assert_eq!(
            m["meetingSurface"]["origins"],
            json!(["https://panel.example.com"])
        );
        assert_eq!(m["eventTypes"], json!(["bot.mentioned", "meeting.started"]));
        assert!(reviewable(&m));
    }

    #[test]
    fn redirect_uris_are_exact_and_bounded() {
        for bad in [
            "https://app.example.com/cb#frag",
            "https://*.example.com/cb",
            "http://app.example.com/cb",
            "https://user@app.example.com/cb",
            "ftp://x/y",
            "https://",
        ] {
            let mut v = base();
            v["redirectUris"] = json!([bad]);
            assert!(input(v).is_err(), "{bad}");
        }
        let mut v = base();
        v["redirectUris"] = json!(["http://localhost:8080/cb"]);
        let m = input(v).unwrap();
        assert!(!reviewable(&m), "loopback only while DRAFT");
        let mut v = base();
        v["redirectUris"] = json!((0..11).map(|i| format!("https://a.example.com/{i}")).collect::<Vec<_>>());
        assert!(input(v).is_err());
        let mut v = base();
        v["redirectUris"] = json!([]);
        assert!(input(v).is_err());
        assert!(is_loopback("http://127.0.0.1/cb") && !is_loopback("http://localhost.evil.com/cb"));
    }

    #[test]
    fn scopes_and_events_come_from_the_closed_catalogues() {
        for (k, v) in [
            ("userScopes", json!(["admin:all"])),
            ("userScopes", json!(["chat:read:bot"])),
            ("botScopes", json!(["chat:read"])),
            ("eventTypes", json!(["call.ended"])),
            ("eventTypes", json!(["sms.received"])),
            ("distribution", json!("UNLISTED")),
        ] {
            let mut b = base();
            b[k] = v.clone();
            assert!(input(b).is_err(), "{k} {v}");
        }
        let mut b = base();
        b["userScopes"] = json!([]);
        assert!(input(b).is_err(), "at least one scope");
        let mut b = base();
        b["userScopes"] = json!([]);
        b["botScopes"] = json!(["chat:write:bot"]);
        assert!(input(b).is_ok());
        let mut b = base();
        b["distribution"] = json!("PUBLIC");
        let m = input(b).unwrap();
        assert!(!reviewable(&m), "a public app names privacy and terms");
        assert!(scopes_supported().contains(&"chat:write:bot"));
    }
}
