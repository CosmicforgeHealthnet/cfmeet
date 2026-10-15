//! Contact Resolution Grants and execution-time plaintext resolution (§33).
//!
//! Plaintext needs more than a service identity:
//!
//! 1. Calling (`protected-contact.grant-for-execution`) requests a grant for ONE call leg
//!    after Privacy Relay AUTHORIZED a public-network relay communication involving the
//!    contact. The grant binds scope, contact, context, session, decision and the
//!    versions it was decided at, Calling's call and call-leg ids and the purpose; it
//!    lives at most 120 s (never past the decision's validity) and carries a random
//!    secret of which only the SHA-256 is stored. Calling never sees the number.
//! 2. Only Telephony (`protected-contact.resolve-for-execution`, grantable to Telephony
//!    alone) can exchange the grant — same scope, contact, call and leg, unchanged
//!    context / session versions, an enabled and effectively VERIFIED contact — for the
//!    canonical E.164 value. The grant is single-execution: a retry of the SAME
//!    execution attempt by the SAME service is idempotent, any other use is refused.
//!
//! Every attempt, successful or not, is in `contact_access_audit` (who, which contact,
//! call / leg / context / session / decision / grant, purpose, outcome) — never the
//! number. The resolution response is built only here and never logged or persisted.

use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
use chrono::{DateTime, Duration, Utc};
use sea_orm::{FromQueryResult, TransactionTrait};
use serde::Deserialize;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use uuid::Uuid;

use super::{
    audit::{record, record_refusal_required, AccessEntry, ExecutionRefs},
    model::iso,
    repository,
    verification::require_verified,
    ContactActor, ContactOperation, ContactVault,
};
use crate::{
    db,
    error::{ErrorCode, RelayError, RelayResult},
};

/// The only purpose a grant can have today.
pub const PURPOSE_PUBLIC_NETWORK_CALL_LEG: &str = "PUBLIC_NETWORK_CALL_LEG";
/// Grant lifetime (the schema caps it at 300 s).
pub const GRANT_TTL_SECONDS: i64 = 120;

/// `POST /internal/v1/contacts/{ref}/resolution-grants` (Calling).
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct GrantRequest {
    pub relay_session_id: Uuid,
    pub decision_id: Uuid,
    pub call_id: Uuid,
    pub call_leg_id: Uuid,
    pub purpose: String,
}

/// `POST /internal/v1/contacts/{ref}/resolve-for-execution` (Telephony).
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ResolveRequest {
    pub grant_id: Uuid,
    pub grant_secret: GrantSecret,
    pub call_id: Uuid,
    pub call_leg_id: Uuid,
    pub execution_attempt_id: Uuid,
}

/// The grant secret as presented (zeroized, never printed).
pub struct GrantSecret(zeroize::Zeroizing<String>);

impl<'de> Deserialize<'de> for GrantSecret {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let s = String::deserialize(d)
            .map_err(|_| serde::de::Error::custom("grantSecret must be a string"))?;
        Ok(Self(zeroize::Zeroizing::new(s)))
    }
}

impl std::fmt::Debug for ResolveRequest {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ResolveRequest")
            .field("grant_id", &self.grant_id)
            .field("grant_secret", &"[REDACTED]")
            .finish_non_exhaustive()
    }
}

#[derive(Debug, FromQueryResult)]
struct DecisionRow {
    context_id: Uuid,
    session_id: Uuid,
    context_version: i64,
    session_version: i64,
    source_subject_kind: String,
    source_ref: Uuid,
    target_contact_refs: Vec<Uuid>,
    route: String,
    decision: String,
    valid_until: Option<DateTime<Utc>>,
    requester_service: String,
}

#[derive(Debug, FromQueryResult)]
struct GrantRow {
    contact_reference: Uuid,
    context_ref: Uuid,
    relay_session_ref: Uuid,
    decision_ref: Uuid,
    context_version: i64,
    session_version: i64,
    call_ref: Uuid,
    call_leg_ref: Uuid,
    secret_hash: String,
    status: String,
    expires_at: DateTime<Utc>,
    consumed_by_service: Option<String>,
    consumed_attempt_ref: Option<Uuid>,
}

#[derive(Debug, FromQueryResult)]
struct Versions {
    context_version: i64,
    session_version: i64,
}

fn denied() -> RelayError {
    RelayError::Refused(ErrorCode::ContactResolutionDenied)
}

fn hash(secret: &str) -> String {
    hex::encode(Sha256::digest(secret.as_bytes()))
}

/// Constant-time comparison of two equal-length hex digests.
fn same_digest(a: &str, b: &str) -> bool {
    a.len() == b.len()
        && a.bytes()
            .zip(b.bytes())
            .fold(0u8, |acc, (x, y)| acc | (x ^ y))
            == 0
}

async fn current_versions<C: sea_orm::ConnectionTrait>(
    conn: &C,
    actor: &ContactActor,
    context: Uuid,
    session: Uuid,
) -> RelayResult<Option<Versions>> {
    let s = actor.scope;
    db::one(
        conn,
        "SELECT c.version AS context_version, s.version AS session_version \
         FROM communication_contexts c JOIN privacy_relay_sessions s ON s.context_id = c.id \
         WHERE c.id = $1 AND s.id = $2 AND c.tenant_id = $3 AND c.application_id = $4 AND c.environment_id = $5 \
           AND s.tenant_id = $3 AND s.application_id = $4 AND s.environment_id = $5 \
           AND c.status = 'ACTIVE' AND s.status = 'ACTIVE' \
         FOR SHARE OF c, s",
        vec![
            context.into(),
            session.into(),
            s.tenant_id().into(),
            s.application_id().into(),
            s.environment_id().into(),
        ],
    )
    .await
}

impl ContactVault {
    /// Issues a Contact Resolution Grant for one authorized call leg (Calling).
    pub async fn grant_for_execution(
        &self,
        actor: &ContactActor,
        contact: Uuid,
        request: GrantRequest,
    ) -> RelayResult<Value> {
        let entry = AccessEntry::new(ContactOperation::GrantForExecution, Some(contact))
            .purpose(PURPOSE_PUBLIC_NETWORK_CALL_LEG)
            .refs(ExecutionRefs {
                relay_session: Some(request.relay_session_id),
                decision: Some(request.decision_id),
                call: Some(request.call_id),
                call_leg: Some(request.call_leg_id),
                ..Default::default()
            });
        match self.grant_inner(actor, contact, &request).await {
            Ok(v) => Ok(v),
            Err(e) => {
                // No refusal without its audit row (fail closed on an audit failure).
                record_refusal_required(&self.db, actor, entry, &e, self.now()).await?;
                Err(e)
            }
        }
    }

    async fn grant_inner(
        &self,
        actor: &ContactActor,
        contact: Uuid,
        request: &GrantRequest,
    ) -> RelayResult<Value> {
        if request.purpose != PURPOSE_PUBLIC_NETWORK_CALL_LEG {
            return Err(RelayError::Validation(
                "purpose must be PUBLIC_NETWORK_CALL_LEG.",
            ));
        }
        self.keys_for_write()?;
        let now = self.now();
        let scope = actor.scope;
        let txn = self.db.begin().await?;
        let row = repository::find(&txn, scope, contact, false)
            .await?
            .ok_or(RelayError::Refused(ErrorCode::ContactNotFound))?;
        require_verified(&row, now)?;
        let decision: DecisionRow = db::one(
            &txn,
            "SELECT context_id, session_id, context_version, session_version, source_subject_kind, source_ref, \
               target_contact_refs, route, decision, valid_until, requester_service \
             FROM relay_authorization_decisions \
             WHERE id = $1 AND tenant_id = $2 AND application_id = $3 AND environment_id = $4",
            vec![
                request.decision_id.into(),
                scope.tenant_id().into(),
                scope.application_id().into(),
                scope.environment_id().into(),
            ],
        )
        .await?
        .ok_or_else(denied)?;
        let involves = (decision.source_subject_kind == "CONTACT_REFERENCE"
            && decision.source_ref == contact)
            || decision.target_contact_refs.contains(&contact);
        let valid_until = decision
            .valid_until
            .filter(|u| *u > now)
            .ok_or_else(denied)?;
        if decision.decision != "AUTHORIZED"
            || decision.session_id != request.relay_session_id
            || !matches!(decision.route.as_str(), "PUBLIC_RELAY" | "CALLBACK_RELAY")
            || !involves
            || decision.requester_service != actor.service.as_str()
        {
            return Err(denied());
        }
        // The authority the decision was made under must still be the current one.
        let current = current_versions(&txn, actor, decision.context_id, decision.session_id)
            .await?
            .ok_or_else(denied)?;
        if current.context_version != decision.context_version
            || current.session_version != decision.session_version
        {
            return Err(denied());
        }
        // One live grant per call leg and contact: a re-request revokes the previous one.
        db::exec(
            &txn,
            "UPDATE contact_resolution_grants SET status = 'REVOKED', revoked_at = $3 \
             WHERE call_leg_ref = $1 AND contact_reference = $2 AND status = 'ISSUED'",
            vec![request.call_leg_id.into(), contact.into(), now.into()],
        )
        .await?;
        let id = Uuid::new_v4();
        let secret =
            zeroize::Zeroizing::new(URL_SAFE_NO_PAD.encode(*super::crypto::random_secret()));
        let expires_at = (now + Duration::seconds(GRANT_TTL_SECONDS)).min(valid_until);
        db::exec(
            &txn,
            "INSERT INTO contact_resolution_grants (id, tenant_id, application_id, environment_id, contact_reference, \
               context_ref, relay_session_ref, decision_ref, context_version, session_version, call_ref, call_leg_ref, \
               purpose, secret_hash, status, requested_by_service, issued_at, expires_at, request_id, correlation_id) \
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, 'ISSUED', $15, $16, $17, $18, $19)",
            vec![
                id.into(),
                scope.tenant_id().into(),
                scope.application_id().into(),
                scope.environment_id().into(),
                contact.into(),
                decision.context_id.into(),
                decision.session_id.into(),
                request.decision_id.into(),
                decision.context_version.into(),
                decision.session_version.into(),
                request.call_id.into(),
                request.call_leg_id.into(),
                PURPOSE_PUBLIC_NETWORK_CALL_LEG.into(),
                hash(&secret).into(),
                actor.service.as_str().into(),
                now.into(),
                expires_at.into(),
                actor.request_id.as_str().into(),
                actor.correlation_id.as_str().into(),
            ],
        )
        .await?;
        let refs = ExecutionRefs {
            context: Some(decision.context_id),
            relay_session: Some(decision.session_id),
            decision: Some(request.decision_id),
            call: Some(request.call_id),
            call_leg: Some(request.call_leg_id),
            grant: Some(id),
        };
        record(
            &txn,
            actor,
            &AccessEntry::new(ContactOperation::GrantForExecution, Some(contact))
                .purpose(PURPOSE_PUBLIC_NETWORK_CALL_LEG)
                .refs(refs),
            now,
        )
        .await?;
        txn.commit().await?;
        tracing::info!(grant_id = %id, contact_reference = %contact, call_leg_id = %request.call_leg_id, "contact resolution grant issued");
        Ok(json!({
            "grant": {
                "grantId": id,
                "grantSecret": secret.as_str(),
                "contactReference": contact,
                "contextId": decision.context_id,
                "relaySessionId": decision.session_id,
                "decisionId": request.decision_id,
                "callId": request.call_id,
                "callLegId": request.call_leg_id,
                "purpose": PURPOSE_PUBLIC_NETWORK_CALL_LEG,
                "issuedAt": iso(now),
                "expiresAt": iso(expires_at),
            }
        }))
    }

    /// Exchanges a grant for the canonical number (Telephony only). Every attempt is
    /// audited; the value never leaves this function except in the returned body.
    pub async fn resolve_for_execution(
        &self,
        actor: &ContactActor,
        contact: Uuid,
        request: ResolveRequest,
    ) -> RelayResult<Value> {
        let entry = AccessEntry::new(ContactOperation::ResolveForExecution, Some(contact))
            .purpose(PURPOSE_PUBLIC_NETWORK_CALL_LEG)
            .refs(ExecutionRefs {
                call: Some(request.call_id),
                call_leg: Some(request.call_leg_id),
                grant: Some(request.grant_id),
                ..Default::default()
            });
        match self.resolve_inner(actor, contact, &request).await {
            Ok(v) => Ok(v),
            Err(e) => {
                // No refusal without its audit row (fail closed on an audit failure); a
                // success is audited inside its own transaction.
                record_refusal_required(&self.db, actor, entry, &e, self.now()).await?;
                Err(e)
            }
        }
    }

    async fn resolve_inner(
        &self,
        actor: &ContactActor,
        contact: Uuid,
        request: &ResolveRequest,
    ) -> RelayResult<Value> {
        let keys = self.keys_for_write()?;
        let now = self.now();
        let scope = actor.scope;
        let txn = self.db.begin().await?;
        let grant: GrantRow = db::one(
            &txn,
            "SELECT contact_reference, context_ref, relay_session_ref, decision_ref, context_version, session_version, \
               call_ref, call_leg_ref, secret_hash, status, expires_at, consumed_by_service, consumed_attempt_ref \
             FROM contact_resolution_grants \
             WHERE id = $1 AND tenant_id = $2 AND application_id = $3 AND environment_id = $4 FOR UPDATE",
            vec![
                request.grant_id.into(),
                scope.tenant_id().into(),
                scope.application_id().into(),
                scope.environment_id().into(),
            ],
        )
        .await?
        .ok_or_else(denied)?;
        if !same_digest(&hash(&request.grant_secret.0), &grant.secret_hash)
            || grant.contact_reference != contact
            || grant.call_ref != request.call_id
            || grant.call_leg_ref != request.call_leg_id
        {
            return Err(denied());
        }
        if now >= grant.expires_at {
            return Err(RelayError::Refused(
                ErrorCode::ContactResolutionGrantExpired,
            ));
        }
        let retry = match grant.status.as_str() {
            "ISSUED" => false,
            "CONSUMED"
                if grant.consumed_by_service.as_deref() == Some(actor.service.as_str())
                    && grant.consumed_attempt_ref == Some(request.execution_attempt_id) =>
            {
                true
            }
            _ => return Err(denied()),
        };
        let current = current_versions(&txn, actor, grant.context_ref, grant.relay_session_ref)
            .await?
            .ok_or_else(denied)?;
        if current.context_version != grant.context_version
            || current.session_version != grant.session_version
        {
            return Err(denied());
        }
        let row = repository::find(&txn, scope, contact, true)
            .await?
            .ok_or_else(denied)?;
        require_verified(&row, now)?;
        let number = self.open_row(keys, &row)?;
        // The caller identity this leg presents: the relay number lent to THIS session
        // (its active relay identity binding, outbound-capable). Selected here, from the
        // session's own assignment — never by a client, never a customer number.
        let caller_identity = crate::relay_numbers::assignment::session_caller_identity(
            &txn,
            scope,
            grant.relay_session_ref,
            now,
        )
        .await?;
        if !retry {
            db::exec(
                &txn,
                "UPDATE contact_resolution_grants SET status = 'CONSUMED', consumed_at = $2, consumed_by_service = $3, \
                   consumed_attempt_ref = $4 WHERE id = $1",
                vec![
                    request.grant_id.into(),
                    now.into(),
                    actor.service.as_str().into(),
                    request.execution_attempt_id.into(),
                ],
            )
            .await?;
        }
        record(
            &txn,
            actor,
            &AccessEntry::new(ContactOperation::ResolveForExecution, Some(contact))
                .purpose(PURPOSE_PUBLIC_NETWORK_CALL_LEG)
                .refs(ExecutionRefs {
                    context: Some(grant.context_ref),
                    relay_session: Some(grant.relay_session_ref),
                    decision: Some(grant.decision_ref),
                    call: Some(grant.call_ref),
                    call_leg: Some(grant.call_leg_ref),
                    grant: Some(request.grant_id),
                }),
            now,
        )
        .await?;
        txn.commit().await?;
        tracing::info!(grant_id = %request.grant_id, contact_reference = %contact, call_leg_id = %grant.call_leg_ref, retry, "contact resolved for execution");
        let mut body = json!({
            "resolution": {
                "contactReference": contact,
                "e164": number.expose_e164(),
                "country": number.country_iso2(),
                "callId": grant.call_ref,
                "callLegId": grant.call_leg_ref,
                "grantId": request.grant_id,
                "expiresAt": iso(grant.expires_at),
            }
        });
        if let Some((assignment_id, e164)) = caller_identity {
            body["resolution"]["callerIdentity"] = json!({
                "assignmentId": assignment_id,
                "e164": e164,
            });
        }
        Ok(body)
    }

    /// Marks issued grants past their expiry EXPIRED (they are refused at read time
    /// already). Bounded.
    pub async fn expire_grants(&self, limit: u64) -> RelayResult<u64> {
        db::exec(
            &self.db,
            "UPDATE contact_resolution_grants SET status = 'EXPIRED' WHERE id IN ( \
               SELECT id FROM contact_resolution_grants WHERE status = 'ISSUED' AND expires_at <= $1 \
               ORDER BY expires_at LIMIT $2 FOR UPDATE SKIP LOCKED)",
            vec![self.now().into(), (limit.clamp(1, 1000) as i64).into()],
        )
        .await
    }
}
