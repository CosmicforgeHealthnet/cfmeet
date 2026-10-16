//! Privacy Relay incidents (§86): conditions the service refuses to guess about.
//!
//! * `OUTBOX_DEAD_LETTER` — a committed domain event could not be published (permanent
//!   refusal or exhausted retries);
//! * `ENFORCEMENT_DEAD_LETTER` — an active-call enforcement job stopped retrying;
//! * `CONTACT_KEY_UNAVAILABLE` — a stored contact names a key version the configured
//!   keyring cannot read, or decryption failed at runtime (the operation fails closed;
//!   there is no plaintext fallback);
//! * `RELAY_MAPPING_AMBIGUOUS` — more than one ACTIVE assignment for one relay number
//!   (inbound resolution refused, never a choice).
//!
//! Deduplicated while OPEN by a stable key (a repeat bumps `occurrences` and
//! `last_seen_at`). The summary is a fixed, safe sentence per category: never a number,
//! a ContactReference, a secret or key material. Raising never fails the caller's
//! operation (best effort, on its own connection); resolving is an operator action that
//! writes `privacy_relay_operator_audit` in the same transaction.

use chrono::{DateTime, Utc};
use sea_orm::{ConnectionTrait, DatabaseConnection, TransactionTrait};
use serde_json::json;
use uuid::Uuid;

use crate::{db, error::RelayResult, metrics};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IncidentCategory {
    OutboxDeadLetter,
    EnforcementDeadLetter,
    ContactKeyUnavailable,
    RelayMappingAmbiguous,
}

impl IncidentCategory {
    pub const ALL: [IncidentCategory; 4] = [
        IncidentCategory::OutboxDeadLetter,
        IncidentCategory::EnforcementDeadLetter,
        IncidentCategory::ContactKeyUnavailable,
        IncidentCategory::RelayMappingAmbiguous,
    ];

    pub fn as_str(&self) -> &'static str {
        match self {
            IncidentCategory::OutboxDeadLetter => "OUTBOX_DEAD_LETTER",
            IncidentCategory::EnforcementDeadLetter => "ENFORCEMENT_DEAD_LETTER",
            IncidentCategory::ContactKeyUnavailable => "CONTACT_KEY_UNAVAILABLE",
            IncidentCategory::RelayMappingAmbiguous => "RELAY_MAPPING_AMBIGUOUS",
        }
    }

    fn severity(&self) -> &'static str {
        match self {
            IncidentCategory::OutboxDeadLetter => "MEDIUM",
            IncidentCategory::EnforcementDeadLetter => "HIGH",
            IncidentCategory::ContactKeyUnavailable => "CRITICAL",
            IncidentCategory::RelayMappingAmbiguous => "CRITICAL",
        }
    }

    /// The fixed, safe summary of the category.
    fn summary(&self) -> &'static str {
        match self {
            IncidentCategory::OutboxDeadLetter => {
                "Committed Privacy Relay events were dead-lettered and are not being published; replay after fixing the cause."
            }
            IncidentCategory::EnforcementDeadLetter => {
                "Active-call enforcement jobs stopped retrying after their attempt cap; protected calls may still be live."
            }
            IncidentCategory::ContactKeyUnavailable => {
                "A stored protected contact names an encryption key version the configured keyring cannot read; resolution fails closed."
            }
            IncidentCategory::RelayMappingAmbiguous => {
                "More than one active relay mapping exists for one relay number; inbound resolution is refused."
            }
        }
    }
}

/// Builds a dedupe key `<CATEGORY>[:<part>]*` from bounded parts (key versions, codes,
/// inventory ids). Parts outside the safe charset are dropped, never escaped.
pub fn dedupe_key(category: IncidentCategory, parts: &[&str]) -> String {
    let mut key = category.as_str().to_string();
    for part in parts.iter().take(3) {
        if !part.is_empty()
            && part.len() <= 64
            && part
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'))
        {
            key.push(':');
            key.push_str(part);
        }
    }
    key
}

/// Opens an incident or records another occurrence of the open one with the same key.
pub async fn raise<C: ConnectionTrait>(
    conn: &C,
    category: IncidentCategory,
    dedupe: &str,
    at: DateTime<Utc>,
) -> RelayResult<Uuid> {
    let row: Option<IdRow> = db::one(
        conn,
        "INSERT INTO privacy_relay_incidents (id, dedupe_key, category, severity, status, summary, occurrences, \
           first_seen_at, last_seen_at) \
         VALUES ($1, $2, $3, $4, 'OPEN', $5, 1, $6, $6) \
         ON CONFLICT (dedupe_key) WHERE status = 'OPEN' DO UPDATE SET \
           occurrences = privacy_relay_incidents.occurrences + 1, \
           last_seen_at = GREATEST(privacy_relay_incidents.last_seen_at, EXCLUDED.last_seen_at) \
         RETURNING id",
        vec![
            Uuid::new_v4().into(),
            dedupe.into(),
            category.as_str().into(),
            category.severity().into(),
            category.summary().into(),
            at.into(),
        ],
    )
    .await?;
    metrics::inc(
        "privacy_relay_incidents_raised_total",
        &[("category", category.as_str())],
    );
    tracing::error!(
        category = category.as_str(),
        severity = category.severity(),
        "INCIDENT: privacy relay incident raised"
    );
    Ok(row.map(|r| r.id).unwrap_or_default())
}

/// [`raise`] on its own connection; a failure is logged, never propagated (an incident
/// must not turn a fail-closed refusal into a different error).
pub async fn raise_best_effort(
    conn: &DatabaseConnection,
    category: IncidentCategory,
    dedupe: &str,
    at: DateTime<Utc>,
) {
    if let Err(error) = raise(conn, category, dedupe, at).await {
        tracing::error!(%error, category = category.as_str(), "privacy relay incident could not be recorded");
    }
}

/// One incident (operator view: safe columns only).
#[derive(Debug, Clone, sea_orm::FromQueryResult)]
pub struct IncidentRow {
    pub id: Uuid,
    pub dedupe_key: String,
    pub category: String,
    pub severity: String,
    pub status: String,
    pub summary: String,
    pub occurrences: i32,
    pub first_seen_at: DateTime<Utc>,
    pub last_seen_at: DateTime<Utc>,
    pub resolved_at: Option<DateTime<Utc>>,
    pub resolved_by: Option<String>,
}

const INCIDENT_COLUMNS: &str = "id, dedupe_key, category, severity, status, summary, occurrences, \
     first_seen_at, last_seen_at, resolved_at, resolved_by";

/// Incidents, newest activity first (OPEN only unless `all`).
pub async fn list<C: ConnectionTrait>(conn: &C, all: bool) -> RelayResult<Vec<IncidentRow>> {
    db::all(
        conn,
        &format!(
            "SELECT {INCIDENT_COLUMNS} FROM privacy_relay_incidents \
             WHERE ($1 OR status = 'OPEN') ORDER BY last_seen_at DESC, id LIMIT 500"
        ),
        vec![all.into()],
    )
    .await
}

/// Operator identity (`--operator` / `CF_OPERATOR_ID`): a short safe token.
pub fn valid_operator(operator: &str) -> bool {
    (2..=64).contains(&operator.len())
        && operator.as_bytes()[0].is_ascii_alphanumeric()
        && operator
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'@' | b':' | b'-'))
}

/// Why an operator resolution did not happen.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ResolveError {
    #[error("an operator identity is required (--operator or CF_OPERATOR_ID)")]
    OperatorRequired,
    #[error("no OPEN incident with this id")]
    NotOpen,
    #[error("database error ({0})")]
    Database(String),
}

/// Resolves an OPEN incident and appends the operator audit row in ONE transaction
/// (an effect without its audit row, or the reverse, cannot exist).
pub async fn resolve(
    conn: &DatabaseConnection,
    id: Uuid,
    operator: &str,
    at: DateTime<Utc>,
) -> Result<IncidentRow, ResolveError> {
    if !valid_operator(operator) {
        return Err(ResolveError::OperatorRequired);
    }
    let db_err = |e: crate::error::RelayError| ResolveError::Database(e.code().as_str().into());
    let txn = conn
        .begin()
        .await
        .map_err(|e| ResolveError::Database(crate::error::summarize_db_error(&e).0))?;
    let row: Option<IncidentRow> = db::one(
        &txn,
        &format!(
            "UPDATE privacy_relay_incidents SET status = 'RESOLVED', resolved_at = GREATEST($2, last_seen_at), \
               resolved_by = $3 WHERE id = $1 AND status = 'OPEN' RETURNING {INCIDENT_COLUMNS}"
        ),
        vec![id.into(), at.into(), operator.into()],
    )
    .await
    .map_err(db_err)?;
    let Some(row) = row else {
        let _ = txn.rollback().await;
        return Err(ResolveError::NotOpen);
    };
    db::exec(
        &txn,
        "INSERT INTO privacy_relay_operator_audit (id, actor, action, target_kind, target_id, detail, occurred_at) \
         VALUES ($1, $2, 'INCIDENT_RESOLVE', 'INCIDENT', $3, $4, $5)",
        vec![
            Uuid::new_v4().into(),
            operator.into(),
            id.into(),
            json!({
                "category": row.category,
                "occurrences": row.occurrences,
                "status": "RESOLVED",
            })
            .into(),
            at.into(),
        ],
    )
    .await
    .map_err(db_err)?;
    txn.commit()
        .await
        .map_err(|e| ResolveError::Database(crate::error::summarize_db_error(&e).0))?;
    tracing::info!(
        incident_id = %id,
        category = %row.category,
        "privacy relay incident resolved by operator"
    );
    Ok(row)
}

#[derive(Debug, sea_orm::FromQueryResult)]
struct IdRow {
    id: Uuid,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dedupe_keys_keep_only_bounded_parts() {
        assert_eq!(
            dedupe_key(
                IncidentCategory::ContactKeyUnavailable,
                &["ENCRYPTION", "ck-2"]
            ),
            "CONTACT_KEY_UNAVAILABLE:ENCRYPTION:ck-2"
        );
        assert_eq!(
            dedupe_key(
                IncidentCategory::OutboxDeadLetter,
                &["+44 20 7946 0123", "MALFORMED"]
            ),
            "OUTBOX_DEAD_LETTER:MALFORMED"
        );
        for c in IncidentCategory::ALL {
            let s = c.summary();
            assert!(s.len() <= 240 && !s.chars().any(|ch| ch.is_ascii_digit() || ch == '@'));
        }
        assert!(valid_operator("ops.alice") && !valid_operator("") && !valid_operator("a b"));
    }
}
