//! Short-lived media join credentials (Part C §41, §54).
//!
//! A media credential is minted per request, for one authorized session generation, and
//! is never stored: the database records only the issuance (generation, expiry, applied
//! permissions) as CALL_MEDIA_CREDENTIAL_ISSUED. Its lifetime defaults to five minutes and
//! is capped at fifteen; a credential is a key to join, not session authority — the
//! server admits each connection against the database before granting media.

use chrono::{DateTime, Duration, SecondsFormat, Utc};
use serde::Serialize;

use super::{permissions::MediaPermissions, provider::IssuedMediaCredential};
use crate::config::ConfigError;

pub const DEFAULT_TTL_SECONDS: i64 = 300;
pub const MIN_TTL_SECONDS: i64 = 30;
/// Strict maximum (`CALLING_MEDIA_CREDENTIAL_TTL_SECONDS` cannot exceed it).
pub const MAX_TTL_SECONDS: i64 = 900;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MediaCredentialPolicy {
    pub ttl: Duration,
}

impl Default for MediaCredentialPolicy {
    fn default() -> Self {
        Self {
            ttl: Duration::seconds(DEFAULT_TTL_SECONDS),
        }
    }
}

impl MediaCredentialPolicy {
    pub fn from_lookup(get: &impl Fn(&str) -> Option<String>) -> Result<Self, ConfigError> {
        let ttl = match get("CALLING_MEDIA_CREDENTIAL_TTL_SECONDS") {
            None => DEFAULT_TTL_SECONDS,
            Some(v) => v
                .trim()
                .parse::<i64>()
                .ok()
                .filter(|s| (MIN_TTL_SECONDS..=MAX_TTL_SECONDS).contains(s))
                .ok_or_else(|| {
                    ConfigError::invalid(
                        "CALLING_MEDIA_CREDENTIAL_TTL_SECONDS",
                        format!("must be between {MIN_TTL_SECONDS} and {MAX_TTL_SECONDS}"),
                    )
                })?,
        };
        Ok(Self {
            ttl: Duration::seconds(ttl),
        })
    }
}

/// The public media credential response: connection URL, join token, expiry and the
/// server-authorized capabilities the connection receives once admitted. Nothing else —
/// no provider key, secret, resource id or administrative field.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MediaCredentialView {
    pub session_id: uuid::Uuid,
    pub url: String,
    pub token: String,
    pub expires_at: String,
    pub capabilities: MediaPermissions,
}

impl MediaCredentialView {
    pub fn new(
        session_id: uuid::Uuid,
        credential: &IssuedMediaCredential,
        capabilities: MediaPermissions,
    ) -> Self {
        Self {
            session_id,
            url: credential.url.clone(),
            token: credential.token.expose().to_string(),
            expires_at: format_time(credential.expires_at),
            capabilities,
        }
    }
}

fn format_time(at: DateTime<Utc>) -> String {
    at.to_rfc3339_opts(SecondsFormat::Secs, true)
}

impl std::fmt::Debug for MediaCredentialView {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MediaCredentialView")
            .field("session_id", &self.session_id)
            .field("url", &self.url)
            .field("token", &"[REDACTED]")
            .field("expires_at", &self.expires_at)
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ttl_is_short_and_capped() {
        let none = |_: &str| None;
        assert_eq!(
            MediaCredentialPolicy::from_lookup(&none).unwrap().ttl,
            Duration::minutes(5)
        );
        for bad in ["0", "29", "901", "3600", "x"] {
            let get =
                |k: &str| (k == "CALLING_MEDIA_CREDENTIAL_TTL_SECONDS").then(|| bad.to_string());
            assert!(MediaCredentialPolicy::from_lookup(&get).is_err(), "{bad}");
        }
        let get =
            |k: &str| (k == "CALLING_MEDIA_CREDENTIAL_TTL_SECONDS").then(|| "900".to_string());
        assert_eq!(
            MediaCredentialPolicy::from_lookup(&get).unwrap().ttl,
            Duration::minutes(15)
        );
    }
}
