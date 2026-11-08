//! Legacy `/api/v1` secrets at rest (PLT-002 / SEC-006 / DATA-003).
//!
//! * Legacy API keys are stored as a SHA-256 verifier (`api_keys.key_hash`) plus a short
//!   non-secret lookup prefix (`api_keys.key_prefix`). Authentication looks candidates up
//!   by prefix and compares the verifier in constant time; the usage counter is consumed
//!   with one atomic conditional `UPDATE … SET used_count = used_count + 1`.
//! * New rows keep a non-secret placeholder in the plaintext column (it starts with
//!   U+0001, which no HTTP header value can carry, so no plaintext lookup — ours or the
//!   previous image's — can ever match it).
//! * Rows written by an older image after the hardening migration ran (a rollback window)
//!   have no verifier yet; they are found by the plaintext fallback once and healed. The
//!   fallback and the plaintext column go away in the documented contract step
//!   (`docs/operations/hardening-sections-2-3.md`, "Cleanup").
//! * Legacy webhook secrets are never delivered (DATA-007): only a fingerprint is kept.
//!
//! Domain separation strings are part of the stored format (the migration backfill uses
//! the same ones); never change them.

use chrono::NaiveDateTime;
use sea_orm::{
    ActiveModelTrait, ColumnTrait, ConnectionTrait, DatabaseConnection, DbBackend, EntityTrait,
    QueryFilter, QuerySelect, Set, Statement,
};
use sha2::{Digest, Sha256};
use uuid::Uuid;

use crate::{
    error::{ApiError, ApiResult},
    models::api_keys::{self, ApiKeyStatus, Entity as ApiKeys},
};

const API_KEY_DOMAIN: &str = "cfmeet-legacy-api-key:v1|";
const WEBHOOK_SECRET_DOMAIN: &str = "cfmeet-legacy-webhook-secret:v1|";
/// Non-secret lookup prefix length (the key is 64 random alphanumerics).
pub const API_KEY_PREFIX_LEN: usize = 8;
/// Stored in `webhooks.secret` for rows whose secret is no longer kept.
pub const WEBHOOK_SECRET_PLACEHOLDER: &str = "\u{1}sha256";
/// Bound on prefix candidates examined per request (collisions are astronomically rare).
const MAX_PREFIX_CANDIDATES: u64 = 16;

fn sha256_hex(domain: &str, value: &str) -> String {
    let mut h = Sha256::new();
    h.update(domain.as_bytes());
    h.update(value.as_bytes());
    hex::encode(h.finalize())
}

/// Stored verifier of a legacy API key.
pub fn api_key_hash(key: &str) -> String {
    sha256_hex(API_KEY_DOMAIN, key)
}

/// Non-secret lookup / display prefix; `None` when the value cannot be a key.
pub fn api_key_prefix(key: &str) -> Option<String> {
    (key.len() >= API_KEY_PREFIX_LEN && key.is_ascii())
        .then(|| key[..API_KEY_PREFIX_LEN].to_owned())
}

/// Placeholder written to the plaintext column of a new key (unique, never a header value).
pub fn api_key_placeholder(id: Uuid) -> String {
    format!("\u{1}h:{}", id.simple())
}

/// True for a plaintext column that holds a placeholder, not a key.
pub fn is_placeholder(value: &str) -> bool {
    value.starts_with('\u{1}')
}

/// Fingerprint kept instead of a legacy webhook secret.
pub fn webhook_secret_fingerprint(secret: &str) -> String {
    sha256_hex(WEBHOOK_SECRET_DOMAIN, secret)
}

/// Constant-time equality of two hex verifiers (length is public).
pub fn constant_time_eq(a: &str, b: &str) -> bool {
    let (a, b) = (a.as_bytes(), b.as_bytes());
    if a.len() != b.len() {
        return false;
    }
    a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

/// What the owner sees instead of the key: the lookup prefix, never the secret part.
pub fn masked(key: &api_keys::Model) -> String {
    match key.key_prefix.as_deref() {
        Some(prefix) => format!("{prefix}{}", "*".repeat(8)),
        None if !is_placeholder(&key.api_key) && key.api_key.len() >= API_KEY_PREFIX_LEN => {
            format!("{}{}", &key.api_key[..API_KEY_PREFIX_LEN], "*".repeat(8))
        }
        None => "*".repeat(8),
    }
}

/// Find the stored key matching a presented `Api-Key` value.
pub async fn find_api_key(
    db: &DatabaseConnection,
    presented: &str,
) -> ApiResult<Option<api_keys::Model>> {
    let Some(prefix) = api_key_prefix(presented) else {
        return Ok(None);
    };
    let verifier = api_key_hash(presented);
    let candidates = ApiKeys::find()
        .filter(api_keys::Column::KeyPrefix.eq(prefix.clone()))
        .filter(api_keys::Column::KeyHash.is_not_null())
        .limit(MAX_PREFIX_CANDIDATES)
        .all(db)
        .await?;
    let mut found = None;
    for c in candidates {
        // Examine every candidate: no early exit on a match.
        if c.key_hash
            .as_deref()
            .is_some_and(|h| constant_time_eq(h, &verifier))
        {
            found = Some(c);
        }
    }
    if found.is_some() {
        return Ok(found);
    }
    // Rows written by an older image after the migration (no verifier yet): plaintext
    // fallback, healed on first use. Removed by the documented contract step.
    let legacy = ApiKeys::find()
        .filter(api_keys::Column::KeyHash.is_null())
        .filter(api_keys::Column::ApiKey.eq(presented))
        .one(db)
        .await?;
    let Some(legacy) = legacy else {
        return Ok(None);
    };
    let mut heal: api_keys::ActiveModel = legacy.clone().into();
    heal.key_hash = Set(Some(verifier.clone()));
    heal.key_prefix = Set(Some(prefix.clone()));
    if let Err(e) = heal.update(db).await {
        tracing::warn!(api_key_id = %legacy.id, "legacy API key verifier backfill failed: {e}");
    }
    Ok(Some(api_keys::Model {
        key_hash: Some(verifier),
        key_prefix: Some(prefix),
        ..legacy
    }))
}

/// Why a key cannot be used right now.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeyRefusal {
    Revoked,
    Expired,
    LimitExceeded,
}

impl KeyRefusal {
    pub fn into_error(self) -> ApiError {
        match self {
            KeyRefusal::Revoked => ApiError::Unauthorized("API key has been revoked".into()),
            KeyRefusal::Expired => ApiError::Unauthorized("API key has expired".into()),
            KeyRefusal::LimitExceeded => ApiError::Forbidden("API key usage limit exceeded".into()),
        }
    }
}

fn refusal(key: &api_keys::Model, now: NaiveDateTime) -> Option<KeyRefusal> {
    if key.status != ApiKeyStatus::Active {
        Some(KeyRefusal::Revoked)
    } else if key.expires_at <= now {
        Some(KeyRefusal::Expired)
    } else if key.used_count >= key.usage_limit {
        Some(KeyRefusal::LimitExceeded)
    } else {
        None
    }
}

/// Consume one use atomically: the increment and every condition are one statement, so
/// concurrent requests can never exceed `usage_limit` or lose increments.
pub async fn consume_use<C: ConnectionTrait>(
    db: &C,
    key: &api_keys::Model,
    now: NaiveDateTime,
) -> ApiResult<Result<i32, KeyRefusal>> {
    if let Some(r) = refusal(key, now) {
        return Ok(Err(r));
    }
    let row = db
        .query_one(Statement::from_sql_and_values(
            DbBackend::Postgres,
            "UPDATE api_keys SET used_count = used_count + 1, updated_at = $2 \
             WHERE id = $1 AND status = 'active' AND expires_at > $2 AND used_count < usage_limit \
             RETURNING used_count",
            [key.id.into(), now.into()],
        ))
        .await?;
    if let Some(row) = row {
        return Ok(Ok(row.try_get::<i32>("", "used_count")?));
    }
    // Lost the race (or the key changed meanwhile): report the current reason.
    let current = ApiKeys::find_by_id(key.id).one(db).await?;
    Ok(Err(current
        .as_ref()
        .and_then(|k| refusal(k, now))
        .unwrap_or(KeyRefusal::LimitExceeded)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn verifier_is_domain_separated_hex() {
        let k = "A".repeat(64);
        let h = api_key_hash(&k);
        assert_eq!(h.len(), 64);
        assert!(h.chars().all(|c| c.is_ascii_hexdigit()));
        assert_ne!(h, crate::services::secrets::hash_secret(&k));
        assert_ne!(h, webhook_secret_fingerprint(&k));
        assert!(!h.contains(&k));
    }

    #[test]
    fn verifier_matches_the_migration_backfill_format() {
        // encode(sha256(convert_to('cfmeet-legacy-api-key:v1|' || 'abc', 'UTF8')), 'hex')
        assert_eq!(
            api_key_hash("abc"),
            hex::encode(Sha256::digest(b"cfmeet-legacy-api-key:v1|abc"))
        );
    }

    #[test]
    fn placeholder_can_never_be_presented() {
        let p = api_key_placeholder(Uuid::new_v4());
        assert!(is_placeholder(&p));
        assert!(p.len() <= 64);
        assert!(axum::http::HeaderValue::from_str(&p).is_err());
        assert!(axum::http::HeaderValue::from_str(WEBHOOK_SECRET_PLACEHOLDER).is_err());
        assert!(api_key_prefix("short").is_none());
        assert_eq!(api_key_prefix("abcdefghXYZ").as_deref(), Some("abcdefgh"));
    }

    #[test]
    fn constant_time_eq_is_exact() {
        assert!(constant_time_eq("abcd", "abcd"));
        assert!(!constant_time_eq("abcd", "abce"));
        assert!(!constant_time_eq("abcd", "abc"));
    }
}
