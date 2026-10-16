//! Protected calls (Part D §29–§30): Calling asks Privacy Relay for CURRENT authority
//! before a protected call is created or a new participant is introduced by transfer,
//! records the decision it relied on, and fails CLOSED when no answer can be obtained.
//! A protected call never falls back to APP_DIRECT_V1.

use chrono::Utc;
use sea_orm::{ConnectionTrait, DatabaseTransaction, DbBackend, Statement};
use uuid::Uuid;

use super::AppCallService;
use crate::{
    calls::{
        attributes::{SafeAttributes, SafeValue},
        command::CallScope,
        errors::{CallError, CallResult},
        ids::CallId,
    },
    internal_auth::delegation::delegated_context,
    models::calls,
    platform::{context::PlatformAuthContext, principals::ResolvedPrincipal},
    privacy_relay::{
        enforcement::{lock_context, superseded},
        AuthorizationPurpose, CallAuthorization, PrivacyDecision, PrivacyRelayError,
    },
};

/// Privacy Relay decision evidence on the call's authorization (ids and versions only).
pub(crate) fn privacy_evidence(policy: &mut SafeAttributes, d: &PrivacyDecision) -> CallResult<()> {
    policy.insert(
        "communication_context_id",
        SafeValue::Text(d.context_id.to_string()),
    )?;
    policy.insert(
        "communication_context_version",
        SafeValue::from(d.context_version),
    )?;
    policy.insert(
        "privacy_relay_session_id",
        SafeValue::Text(d.relay_session_id.to_string()),
    )?;
    policy.insert(
        "privacy_relay_session_version",
        SafeValue::from(d.relay_session_version),
    )?;
    policy.insert(
        "privacy_decision_id",
        SafeValue::Text(d.decision_id.to_string()),
    )?;
    policy.insert("relay_strategy", SafeValue::Text(d.strategy.clone()))?;
    policy.insert(
        "privacy_policy_version",
        SafeValue::Text(d.policy_version.clone()),
    )?;
    if let Some(until) = d.valid_until {
        policy.insert(
            "valid_until",
            SafeValue::Text(until.to_rfc3339_opts(chrono::SecondsFormat::Secs, true)),
        )?;
    }
    Ok(())
}

impl AppCallService {
    /// The relay session of a protected call of `scope` (None: ordinary call).
    pub(crate) async fn protected_session(
        &self,
        scope: CallScope,
        call_id: CallId,
    ) -> CallResult<Option<Uuid>> {
        Ok(self.calls_row(scope, call_id).await?.privacy_relay_ref)
    }

    /// Privacy Relay's decision for `source -> targets` under `relay_session`. Only an
    /// AUTHORIZED, consistent decision for that session passes.
    pub(crate) async fn privacy_authority(
        &self,
        ctx: &PlatformAuthContext,
        source: &ResolvedPrincipal,
        relay_session: Uuid,
        targets: Vec<Uuid>,
        purpose: AuthorizationPurpose,
    ) -> CallResult<PrivacyDecision> {
        let delegated = delegated_context(ctx, Some(source)).map_err(|_| CallError::NotFound)?;
        let decision = self
            .privacy
            .authorize_call(
                &delegated,
                ctx.correlation_id(),
                &CallAuthorization {
                    relay_session_id: relay_session,
                    source_principal_id: source.id().as_uuid(),
                    target_principal_ids: targets,
                    purpose,
                    ..Default::default()
                },
            )
            .await
            .map_err(|e| match e {
                PrivacyRelayError::NotFound => CallError::NotFound,
                PrivacyRelayError::Unavailable => CallError::PrivacyAuthorizationUnavailable,
            })?;
        if !decision.is_authorized_for(relay_session) {
            tracing::info!(
                privacy_relay_ref = %relay_session,
                decision_id = %decision.decision_id,
                reason = %decision.reason,
                purpose = purpose.as_str(),
                "protected call refused by privacy relay"
            );
            return Err(CallError::AuthorizationDenied);
        }
        Ok(decision)
    }

    /// Inside the creating / transferring transaction: serializes with enforcement on the
    /// same context and refuses a decision an already-received enforcement superseded.
    pub(crate) async fn guard_privacy_decision(
        &self,
        txn: &DatabaseTransaction,
        scope: CallScope,
        d: &PrivacyDecision,
        principals: &[Uuid],
    ) -> CallResult<()> {
        lock_context(txn, d.context_id).await?;
        let owner = (
            scope.tenant_id().as_uuid(),
            scope.application_id().as_uuid(),
            scope.environment_id().as_uuid(),
        );
        if superseded(
            txn,
            owner,
            d.context_id,
            d.context_version,
            d.relay_session_id,
            d.relay_session_version,
            principals,
        )
        .await?
        {
            tracing::info!(decision_id = %d.decision_id, "privacy decision superseded by enforcement");
            return Err(CallError::AuthorizationDenied);
        }
        Ok(())
    }

    /// Appends the decision a protected call relied on.
    pub(crate) async fn record_privacy_decision(
        &self,
        txn: &DatabaseTransaction,
        call: &calls::Model,
        d: &PrivacyDecision,
        purpose: AuthorizationPurpose,
        target: Option<Uuid>,
    ) -> CallResult<()> {
        let valid_until = d.valid_until.ok_or(CallError::AuthorizationDenied)?;
        txn.execute(Statement::from_sql_and_values(
            DbBackend::Postgres,
            "INSERT INTO call_privacy_authorizations (id, call_id, tenant_id, application_id, environment_id, purpose, \
               communication_context_ref, privacy_relay_ref, context_version, session_version, decision_ref, strategy, \
               policy_version, valid_until, target_principal_id, decided_at, created_at) \
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15, $16, $17)",
            vec![
                Uuid::new_v4().into(),
                call.id.into(),
                call.tenant_id.into(),
                call.application_id.into(),
                call.environment_id.into(),
                purpose.as_str().into(),
                d.context_id.into(),
                d.relay_session_id.into(),
                d.context_version.into(),
                d.relay_session_version.into(),
                d.decision_id.into(),
                d.strategy.clone().into(),
                d.policy_version.clone().into(),
                valid_until.into(),
                target.into(),
                d.decided_at.into(),
                Utc::now().into(),
            ],
        ))
        .await?;
        Ok(())
    }
}
