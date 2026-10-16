//! Operator audit of platform administration (Part H §72.8; table
//! `platform_operator_audit`, migrations 000034 / 000038 / 000042).
//!
//! Identity administration (tenants … credentials, principals) moved to Platform Access
//! with the identity authority (ADR 0032); its vocabulary stays here because the
//! append-only audit keeps the Part III rows. Calling still audits its own policy
//! administration (limits, routing / spend policy, …) and the identity hand-off.
//!
//! Every mutating administration operation takes an explicit [`OperatorContext`] — WHO
//! acts and the correlation id of the command — and writes exactly ONE audit row in the
//! SAME transaction as the mutation: the row exists if and only if the change committed.
//! The audited operations are the `operator_*` methods below (and the operator functions
//! of the modules that own the other administered resources, which call
//! [`record`]); `platform_admin` reaches mutations only through them, so the audit write
//! cannot be skipped by the CLI.
//!
//! `detail` holds opaque ids and bounded codes only — never a credential secret, a
//! verifier, the pepper, a contact value or a number (the table refuses the obvious keys
//! and anything over 2 KiB). The table is append-only (UPDATE / DELETE / TRUNCATE are
//! refused by trigger).

use chrono::{DateTime, Utc};
use futures_util::future::BoxFuture;
use sea_orm::{
    ConnectionTrait, DatabaseConnection, DatabaseTransaction, DbBackend, Statement,
    TransactionTrait,
};
use serde_json::{json, Value};
use uuid::Uuid;

use super::{
    credentials::CredentialView, limits::LimitValues,
    principals::PrincipalView, service_accounts::ServiceAccountView, PlatformError,
    PlatformResult, PlatformService,
};
use crate::domain::{ApplicationId, CredentialId, PrincipalId, ServiceAccountId, TenantId};

/// The environment variable `platform_admin` reads the operator id from when
/// `--operator` is absent.
pub const OPERATOR_ENV: &str = "CF_OPERATOR_ID";

text_enum!(
    /// Closed vocabulary of audited operator actions (the table's CHECK).
    OperatorAction {
        TenantCreated = "TENANT_CREATED",
        TenantStatusSet = "TENANT_STATUS_SET",
        ApplicationCreated = "APPLICATION_CREATED",
        ApplicationStatusSet = "APPLICATION_STATUS_SET",
        EnvironmentCreated = "ENVIRONMENT_CREATED",
        EnvironmentStatusSet = "ENVIRONMENT_STATUS_SET",
        EnvironmentLiveTelecomSet = "ENVIRONMENT_LIVE_TELECOM_SET",
        ServiceAccountCreated = "SERVICE_ACCOUNT_CREATED",
        ServiceAccountStatusSet = "SERVICE_ACCOUNT_STATUS_SET",
        CredentialCreated = "CREDENTIAL_CREATED",
        CredentialRotated = "CREDENTIAL_ROTATED",
        CredentialRevoked = "CREDENTIAL_REVOKED",
        ScopeGranted = "SCOPE_GRANTED",
        ScopeRevoked = "SCOPE_REVOKED",
        LimitsSet = "LIMITS_SET",
        EnterpriseCapabilitiesSet = "ENTERPRISE_CAPABILITIES_SET",
        InboundOwnerSet = "INBOUND_OWNER_SET",
        RoutingPolicySet = "ROUTING_POLICY_SET",
        SpendPolicySet = "SPEND_POLICY_SET",
        ComplianceEligibilitySet = "COMPLIANCE_ELIGIBILITY_SET",
        ComplianceEligibilityRevoked = "COMPLIANCE_ELIGIBILITY_REVOKED",
        AbuseReportStatusSet = "ABUSE_REPORT_STATUS_SET",
        BlockRuleReleased = "BLOCK_RULE_RELEASED",
        PrincipalRegistered = "PRINCIPAL_REGISTERED",
        PrincipalStatusSet = "PRINCIPAL_STATUS_SET",
        RetentionHoldPlaced = "RETENTION_HOLD_PLACED",
        RetentionHoldReleased = "RETENTION_HOLD_RELEASED",
        PrincipalTombstoned = "PRINCIPAL_TOMBSTONED",
        WebhookKeysResealed = "WEBHOOK_KEYS_RESEALED",
        IncidentResolved = "INCIDENT_RESOLVED",
        IdentityExported = "IDENTITY_EXPORTED",
        IdentityHandoffVerified = "IDENTITY_HANDOFF_VERIFIED",
        IdentityHandoffCompleted = "IDENTITY_HANDOFF_COMPLETED",
        IdentityVerifiersRestored = "IDENTITY_VERIFIERS_RESTORED",
    }
);

text_enum!(
    /// What an audited action is about (an opaque id of that kind).
    AuditTargetKind {
        Tenant = "TENANT",
        Application = "APPLICATION",
        Environment = "ENVIRONMENT",
        ServiceAccount = "SERVICE_ACCOUNT",
        Credential = "CREDENTIAL",
        Principal = "PRINCIPAL",
        BlockRule = "BLOCK_RULE",
        AbuseReport = "ABUSE_REPORT",
        ComplianceEligibility = "COMPLIANCE_ELIGIBILITY",
        RetentionHold = "RETENTION_HOLD",
        WebhookKeyRing = "WEBHOOK_KEY_RING",
        Incident = "INCIDENT",
        IdentityHandoff = "IDENTITY_HANDOFF",
    }
);

/// Who performs an administration command, and the correlation id of that command.
/// Constructed only from a validated operator id: there is no default operator.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OperatorContext {
    operator: String,
    correlation_id: String,
}

fn valid_operator(value: &str) -> bool {
    let bytes = value.as_bytes();
    !bytes.is_empty()
        && bytes.len() <= 100
        && bytes[0].is_ascii_alphanumeric()
        && bytes
            .iter()
            .all(|b| b.is_ascii_alphanumeric() || b"._@-".contains(b))
}

impl OperatorContext {
    /// `operator` must match `^[A-Za-z0-9][A-Za-z0-9._@-]{0,99}$` (an opaque operator
    /// id such as an SSO handle); a fresh correlation id is generated.
    pub fn new(operator: &str) -> PlatformResult<Self> {
        let operator = operator.trim();
        if !valid_operator(operator) {
            return Err(PlatformError::Validation(
                "the operator id must match ^[A-Za-z0-9][A-Za-z0-9._@-]{0,99}$".to_string(),
            ));
        }
        Ok(Self {
            operator: operator.to_string(),
            correlation_id: format!("op_{}", Uuid::new_v4().simple()),
        })
    }

    pub fn operator(&self) -> &str {
        &self.operator
    }

    pub fn correlation_id(&self) -> &str {
        &self.correlation_id
    }
}

/// One audited action: what, on which target (scoped), with bounded detail.
#[derive(Debug, Clone)]
pub struct AuditEntry {
    pub action: OperatorAction,
    pub target_kind: AuditTargetKind,
    pub target_id: Option<Uuid>,
    pub tenant_id: Option<Uuid>,
    pub application_id: Option<Uuid>,
    pub environment_id: Option<Uuid>,
    pub detail: Value,
}

impl AuditEntry {
    pub fn new(action: OperatorAction, target_kind: AuditTargetKind, target_id: Uuid) -> Self {
        Self {
            action,
            target_kind,
            target_id: Some(target_id),
            tenant_id: None,
            application_id: None,
            environment_id: None,
            detail: json!({}),
        }
    }

    /// The owning scope (any level may be absent, e.g. a tenant-level action).
    pub fn scope(
        mut self,
        tenant_id: Option<Uuid>,
        application_id: Option<Uuid>,
        environment_id: Option<Uuid>,
    ) -> Self {
        self.tenant_id = tenant_id;
        self.application_id = application_id;
        self.environment_id = environment_id;
        self
    }

    /// Bounded detail: opaque ids and codes only.
    pub fn detail(mut self, detail: Value) -> Self {
        self.detail = detail;
        self
    }
}

/// Writes the audit row of `entry` with `db` — which must be the transaction of the
/// mutation it describes. A failure fails the whole operation (no unaudited change).
pub async fn record<C: ConnectionTrait>(
    db: &C,
    operator: &OperatorContext,
    entry: &AuditEntry,
    at: DateTime<Utc>,
) -> PlatformResult<Uuid> {
    let id = Uuid::new_v4();
    db.execute(Statement::from_sql_and_values(
        DbBackend::Postgres,
        "INSERT INTO platform_operator_audit (id, actor, correlation_id, action, target_kind, \
           target_id, tenant_id, application_id, environment_id, detail, occurred_at) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11)",
        [
            id.into(),
            operator.operator().into(),
            operator.correlation_id().into(),
            entry.action.as_str().into(),
            entry.target_kind.as_str().into(),
            entry.target_id.into(),
            entry.tenant_id.into(),
            entry.application_id.into(),
            entry.environment_id.into(),
            entry.detail.clone().into(),
            at.into(),
        ],
    ))
    .await?;
    crate::metrics::inc(
        "calling_operator_actions_total",
        &[("action", entry.action.as_str())],
    );
    tracing::info!(
        operator = operator.operator(),
        correlation_id = operator.correlation_id(),
        action = entry.action.as_str(),
        target_kind = entry.target_kind.as_str(),
        target_id = ?entry.target_id,
        "operator action audited"
    );
    Ok(id)
}

/// Runs `mutation` in a fresh transaction of `db`, writes its audit entry in that same
/// transaction and commits — all or nothing. For administered resources owned outside
/// `platform` (routing / spend policies, inbound owners, …).
pub async fn audited<T, F>(
    db: &DatabaseConnection,
    operator: &OperatorContext,
    at: DateTime<Utc>,
    mutation: F,
) -> PlatformResult<T>
where
    F: for<'t> FnOnce(&'t DatabaseTransaction) -> BoxFuture<'t, PlatformResult<(T, AuditEntry)>>,
{
    let txn = db.begin().await?;
    let (value, entry) = mutation(&txn).await?;
    record(&txn, operator, &entry, at).await?;
    txn.commit().await?;
    Ok(value)
}

/// Bounded audit detail of a limit policy (numbers of the policy, never identities).
fn limit_detail(level: &str, values: &LimitValues) -> Value {
    json!({
        "level": level,
        "requestsPerMinute": values.requests_per_minute,
        "maxConcurrentCommunications": values.max_concurrent_communications,
        "telecomSpendDailyLimitMinor": values.telecom_spend_daily_limit_minor,
        "telecomSpendMonthlyLimitMinor": values.telecom_spend_monthly_limit_minor,
    })
}

/// Opens the mutation's transaction, runs it, audits it and commits — all or nothing.
macro_rules! audited {
    ($self:ident, $op:ident, |$txn:ident| $body:expr) => {{
        let $txn = $self.db.begin().await?;
        let (value, entry) = $body;
        record(&$txn, $op, &entry, $self.clock.now()).await?;
        $txn.commit().await?;
        Ok(value)
    }};
}

impl PlatformService {
















    pub async fn operator_set_tenant_limits(
        &self,
        op: &OperatorContext,
        tenant_id: TenantId,
        values: LimitValues,
    ) -> PlatformResult<LimitValues> {
        audited!(self, op, |txn| {
            let v = self.set_tenant_limits_in(&txn, tenant_id, values).await?;
            let entry = AuditEntry::new(
                OperatorAction::LimitsSet,
                AuditTargetKind::Tenant,
                tenant_id.as_uuid(),
            )
            .scope(Some(tenant_id.as_uuid()), None, None)
            .detail(limit_detail("TENANT", &v));
            (v, entry)
        })
    }

    pub async fn operator_set_application_limits(
        &self,
        op: &OperatorContext,
        application_id: ApplicationId,
        values: LimitValues,
    ) -> PlatformResult<LimitValues> {
        let application = self.application(application_id).await?;
        audited!(self, op, |txn| {
            let v = self
                .set_application_limits_in(&txn, application_id, values)
                .await?;
            let entry = AuditEntry::new(
                OperatorAction::LimitsSet,
                AuditTargetKind::Application,
                application_id.as_uuid(),
            )
            .scope(
                Some(application.tenant_id.as_uuid()),
                Some(application_id.as_uuid()),
                None,
            )
            .detail(limit_detail("APPLICATION", &v));
            (v, entry)
        })
    }

    pub async fn operator_set_service_account_limits(
        &self,
        op: &OperatorContext,
        service_account_id: ServiceAccountId,
        values: LimitValues,
    ) -> PlatformResult<LimitValues> {
        let sa = self.service_account(service_account_id).await?;
        audited!(self, op, |txn| {
            let v = self
                .set_service_account_limits_in(&txn, service_account_id, values)
                .await?;
            let entry = service_account_entry(OperatorAction::LimitsSet, &sa)
                .detail(limit_detail("SERVICE_ACCOUNT", &v));
            (v, entry)
        })
    }

    pub async fn operator_set_principal_limits(
        &self,
        op: &OperatorContext,
        principal_id: PrincipalId,
        values: LimitValues,
    ) -> PlatformResult<LimitValues> {
        let p = self.principal(principal_id).await?;
        audited!(self, op, |txn| {
            let v = self
                .set_principal_limits_in(&txn, principal_id, values)
                .await?;
            let entry = principal_entry(OperatorAction::LimitsSet, &p)
                .detail(limit_detail("PRINCIPAL", &v));
            (v, entry)
        })
    }

    pub async fn operator_set_credential_limits(
        &self,
        op: &OperatorContext,
        credential_id: CredentialId,
        values: LimitValues,
    ) -> PlatformResult<LimitValues> {
        let c = self.credential(credential_id).await?;
        audited!(self, op, |txn| {
            let v = self
                .set_credential_limits_in(&txn, credential_id, values)
                .await?;
            let entry = credential_entry(OperatorAction::LimitsSet, &c)
                .detail(limit_detail("CREDENTIAL", &v));
            (v, entry)
        })
    }
}


fn service_account_entry(action: OperatorAction, sa: &ServiceAccountView) -> AuditEntry {
    AuditEntry::new(action, AuditTargetKind::ServiceAccount, sa.id.as_uuid()).scope(
        Some(sa.tenant_id.as_uuid()),
        Some(sa.application_id.as_uuid()),
        Some(sa.environment_id.as_uuid()),
    )
}

fn credential_entry(action: OperatorAction, c: &CredentialView) -> AuditEntry {
    AuditEntry::new(action, AuditTargetKind::Credential, c.id.as_uuid())
        .scope(
            Some(c.tenant_id.as_uuid()),
            Some(c.application_id.as_uuid()),
            Some(c.environment_id.as_uuid()),
        )
        .detail(json!({
            "publicId": c.public_id,
            "serviceAccountId": c.service_account_id,
            "scopes": c.scopes,
        }))
}

fn principal_entry(action: OperatorAction, p: &PrincipalView) -> AuditEntry {
    AuditEntry::new(action, AuditTargetKind::Principal, p.id.as_uuid()).scope(
        Some(p.tenant_id.as_uuid()),
        Some(p.application_id.as_uuid()),
        Some(p.environment_id.as_uuid()),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn operator_ids_are_bounded_opaque_handles() {
        for ok in ["ops.alice", "a", "svc-ops@example", "OPS_1"] {
            let op = OperatorContext::new(ok).unwrap();
            assert_eq!(op.operator(), ok);
            let c = op.correlation_id();
            assert!(c.len() >= 8 && c.len() <= 64);
            assert!(c.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_'));
        }
        for bad in [
            "",
            " ",
            "-ops",
            "has space",
            "+2348012345678",
            &"a".repeat(101),
        ] {
            assert!(OperatorContext::new(bad).is_err(), "{bad}");
        }
        assert_ne!(
            OperatorContext::new("ops").unwrap().correlation_id(),
            OperatorContext::new("ops").unwrap().correlation_id()
        );
    }

    #[test]
    fn the_action_vocabulary_is_the_tables() {
        // Kept in step with migrations 000034 + 000038 + 000042 + 000043 + 000044 (an unknown
        // code is refused there).
        assert_eq!(OperatorAction::ALL.len(), 34);
        assert_eq!(AuditTargetKind::ALL.len(), 13);
    }
}
