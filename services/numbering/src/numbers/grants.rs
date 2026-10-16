//! Routing grant records (Part IV Workstream K): what Numbering signed — never the token.
//!
//! A record binds a grant to the number, the ACTIVE assignment and the number's routing
//! revision it was issued at. A routing change (new revision), the assignment leaving
//! ACTIVE or a transfer REVOKES the number's outstanding grants in the same transaction
//! (`revoke_grants_in`); verification also compares the CURRENT state, so a grant is
//! never honoured after its basis changed, whatever its expiry.

use chrono::{DateTime, Utc};
use sea_orm::{ConnectionTrait, DatabaseTransaction, FromQueryResult};
use serde::Serialize;
use uuid::Uuid;

use super::{stmt, DomainError, DomainResult, NumberDomain};

/// Why outstanding grants were revoked.
pub mod revoke_reason {
    pub const ROUTING_CHANGED: &str = "ROUTING_CHANGED";
    pub const ASSIGNMENT_NOT_ACTIVE: &str = "ASSIGNMENT_NOT_ACTIVE";
    pub const TRANSFERRED: &str = "TRANSFERRED";
    pub const STALE_ROUTING_REVISION: &str = "STALE_ROUTING_REVISION";
}

#[derive(Debug, Clone, PartialEq, FromQueryResult, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GrantRow {
    pub id: Uuid,
    pub token_id: String,
    pub grant_kind: String,
    pub number_resource_id: Uuid,
    pub assignment_id: Uuid,
    pub tenant_id: Uuid,
    pub application_id: Uuid,
    pub environment_id: Uuid,
    pub routing_revision: i64,
    pub audience: String,
    pub requested_by: String,
    pub principal_id: Option<Uuid>,
    pub target_kind: Option<String>,
    pub target_ref: Option<Uuid>,
    pub operation_ref: Option<String>,
    pub issued_at: DateTime<Utc>,
    pub expires_at: DateTime<Utc>,
    pub consumed_at: Option<DateTime<Utc>>,
    pub consumed_by: Option<String>,
    pub revoked_at: Option<DateTime<Utc>>,
    pub revoke_reason: Option<String>,
    pub correlation_id: String,
}

/// A grant to record (filled from Numbering's own records).
#[derive(Debug, Clone)]
pub struct NewGrant {
    pub token_id: String,
    pub grant_kind: &'static str,
    pub number_resource_id: Uuid,
    pub assignment_id: Uuid,
    pub tenant_id: Uuid,
    pub application_id: Uuid,
    pub environment_id: Uuid,
    pub routing_revision: i64,
    pub audience: String,
    pub requested_by: String,
    pub principal_id: Option<Uuid>,
    pub target_kind: Option<String>,
    pub target_ref: Option<Uuid>,
    pub operation_ref: Option<String>,
    pub issued_at: DateTime<Utc>,
    pub expires_at: DateTime<Utc>,
    pub correlation_id: String,
}

/// Revokes the number's outstanding (unexpired, unrevoked) grants inside `txn`.
pub(crate) async fn revoke_grants_in<C: ConnectionTrait>(
    db: &C,
    number_resource_id: Uuid,
    reason: &'static str,
    now: DateTime<Utc>,
) -> DomainResult<u64> {
    let r = db
        .execute(stmt(
            "UPDATE number_routing_grants SET revoked_at = $2, revoke_reason = $3 \
             WHERE number_resource_id = $1 AND revoked_at IS NULL AND expires_at > $2",
            vec![number_resource_id.into(), now.into(), reason.into()],
        ))
        .await?;
    if r.rows_affected() > 0 {
        crate::metrics::inc(
            "numbering_routing_grants_revoked_total",
            &[("reason", reason)],
        );
    }
    Ok(r.rows_affected())
}

impl NumberDomain {
    pub(crate) async fn record_grant_in(
        &self,
        txn: &DatabaseTransaction,
        g: NewGrant,
    ) -> DomainResult<GrantRow> {
        GrantRow::find_by_statement(stmt(
            "INSERT INTO number_routing_grants (id, token_id, grant_kind, number_resource_id, assignment_id, tenant_id, \
               application_id, environment_id, routing_revision, audience, requested_by, principal_id, target_kind, \
               target_ref, operation_ref, issued_at, expires_at, correlation_id) \
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15, $16, $17, $18) RETURNING *",
            vec![
                Uuid::new_v4().into(),
                g.token_id.into(),
                g.grant_kind.into(),
                g.number_resource_id.into(),
                g.assignment_id.into(),
                g.tenant_id.into(),
                g.application_id.into(),
                g.environment_id.into(),
                g.routing_revision.into(),
                g.audience.into(),
                g.requested_by.into(),
                g.principal_id.into(),
                g.target_kind.into(),
                g.target_ref.into(),
                g.operation_ref.into(),
                g.issued_at.into(),
                g.expires_at.into(),
                g.correlation_id.into(),
            ],
        ))
        .one(txn)
        .await
        .map_err(|e| {
            let e: DomainError = e.into();
            match e {
                DomainError::Conflict { .. } => DomainError::conflict(
                    "OPERATION_ALREADY_AUTHORIZED",
                    "this operation already has a grant",
                ),
                other => other,
            }
        })?
        .ok_or(DomainError::NotFound("grant"))
    }

    pub async fn grant_by_token(&self, token_id: &str) -> DomainResult<Option<GrantRow>> {
        Ok(GrantRow::find_by_statement(stmt(
            "SELECT * FROM number_routing_grants WHERE token_id = $1",
            vec![token_id.into()],
        ))
        .one(&self.db)
        .await?)
    }

    /// Consumes an outbound grant once (`false`: already consumed or revoked).
    pub async fn consume_grant(&self, id: Uuid, by: &str) -> DomainResult<bool> {
        let r = self
            .db
            .execute(stmt(
                "UPDATE number_routing_grants SET consumed_at = $2, consumed_by = $3 \
                 WHERE id = $1 AND consumed_at IS NULL AND revoked_at IS NULL \
                   AND grant_kind IN ('CALLER_IDENTITY', 'SMS_SENDER')",
                vec![id.into(), self.clock.now().into(), by.into()],
            ))
            .await?;
        Ok(r.rows_affected() == 1)
    }

    /// Revokes one grant (verification found its basis changed).
    pub async fn revoke_grant(&self, id: Uuid, reason: &str) -> DomainResult<()> {
        self.db
            .execute(stmt(
                "UPDATE number_routing_grants SET revoked_at = $2, revoke_reason = $3 WHERE id = $1 AND revoked_at IS NULL",
                vec![id.into(), self.clock.now().into(), reason.into()],
            ))
            .await?;
        Ok(())
    }

    /// Deletes grants expired more than a day ago (bounded batch).
    pub async fn purge_expired_grants(&self) -> DomainResult<u64> {
        let r = self
            .db
            .execute(stmt(
                "DELETE FROM number_routing_grants WHERE id IN (SELECT id FROM number_routing_grants \
                   WHERE expires_at < now() - interval '1 day' LIMIT 1000)",
                vec![],
            ))
            .await?;
        Ok(r.rows_affected())
    }
}
