//! Block / opt-out rules (Part H §70, §71, §83).
//!
//! A principal (or the whole application) refuses communication with another principal
//! or with an opaque Privacy Relay ContactReference. Rules are consulted BEFORE any
//! protected side effect — before Privacy Relay is asked, before spend is reserved,
//! before a route is selected and before anything rings:
//!
//! * app-to-app: an ACTIVE rule between the initiator and any target, in either
//!   direction (or an APPLICATION rule on either side) → `TARGET_UNAVAILABLE`, which never
//!   discloses which side blocked;
//! * enterprise ring groups, queues and transfers skip blocked members silently;
//! * public outbound / callback: the initiator's (or the application's) rule on the
//!   destination contact → `PUBLIC_ROUTE_DENIED` (`DESTINATION_BLOCKED`);
//! * public inbound: a target that blocked the caller contact is dropped; an application
//!   rule on the caller refuses the offer (`CALLER_BLOCKED`).
//!
//! Creating a rule never ends an existing Call (policy choice: the parties may end it).
//! Repeated refusals of one initiator are counted in a bounded window
//! (`calling_abuse_windows`, UNLOGGED) and raise a `REPEATED_BLOCKED_ATTEMPTS` /
//! `REPEATED_ROUTE_DENIALS` incident once per window (counts only as evidence).
//! Nothing here ever holds a telephone number.

use std::collections::HashSet;

use chrono::{DateTime, Duration, Utc};
use sea_orm::{ConnectionTrait, FromQueryResult, TransactionTrait};
use serde::Serialize;
use uuid::Uuid;

use super::{
    enterprise::stmt,
    incidents::{Incident, IncidentCategory, IncidentResource, IncidentSeverity},
    AppCallService,
};
use crate::{
    calls::{
        command::CallScope,
        errors::{CallError, CallResult},
    },
    domain::PrincipalId,
    platform::{
        context::PlatformAuthContext,
        principals::{PrincipalReference, PrincipalResolutionError},
        scopes::names,
    },
};

text_enum!(
    /// Who blocks.
    BlockerKind {
        Principal = "PRINCIPAL",
        Application = "APPLICATION",
    }
);

text_enum!(
    /// What is blocked: a principal of the same environment or an opaque contact.
    BlockedKind {
        Principal = "PRINCIPAL",
        ContactReference = "CONTACT_REFERENCE",
    }
);

text_enum!(
    /// Bounded reason vocabulary (never free text).
    BlockReason {
        Unwanted = "UNWANTED",
        Harassment = "HARASSMENT",
        Spam = "SPAM",
        Other = "OTHER",
    }
);

text_enum!(BlockStatus {
    Active = "ACTIVE",
    Released = "RELEASED",
});

/// A refusal counted towards an abuse incident.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RefusalKind {
    /// An ACTIVE block rule refused the attempt.
    BlockedAttempt,
    /// The destination policy (routing engine) denied the attempt.
    RouteDenial,
}

impl RefusalKind {
    fn subject_kind(&self) -> &'static str {
        match self {
            Self::BlockedAttempt => "BLOCKED_ATTEMPT",
            Self::RouteDenial => "ROUTE_DENIAL",
        }
    }

    fn category(&self) -> IncidentCategory {
        match self {
            Self::BlockedAttempt => IncidentCategory::RepeatedBlockedAttempts,
            Self::RouteDenial => IncidentCategory::RepeatedRouteDenials,
        }
    }
}

/// Window of the repeated-refusal counters.
pub const REFUSAL_WINDOW: Duration = Duration::minutes(10);

/// Tunables of the abuse controls (Part H §69, §83): incident thresholds.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AbuseControls {
    /// `CALLING_BLOCKED_ATTEMPTS_INCIDENT_THRESHOLD` (1–1000, default 5): refusals of one
    /// initiator within [`REFUSAL_WINDOW`] that raise an incident.
    pub blocked_attempts_incident_threshold: i64,
    /// `CALLING_ABUSE_REPORTS_INCIDENT_THRESHOLD` (1–1000, default 3): reports against one
    /// principal within 24 h that raise an incident.
    pub abuse_reports_incident_threshold: i64,
}

impl Default for AbuseControls {
    fn default() -> Self {
        Self {
            blocked_attempts_incident_threshold: 5,
            abuse_reports_incident_threshold: 3,
        }
    }
}

impl AbuseControls {
    pub fn from_lookup(
        get: &impl Fn(&str) -> Option<String>,
    ) -> Result<Self, crate::config::ConfigError> {
        let mut c = Self::default();
        for (key, slot) in [
            (
                "CALLING_BLOCKED_ATTEMPTS_INCIDENT_THRESHOLD",
                &mut c.blocked_attempts_incident_threshold,
            ),
            (
                "CALLING_ABUSE_REPORTS_INCIDENT_THRESHOLD",
                &mut c.abuse_reports_incident_threshold,
            ),
        ] {
            if let Some(v) = get(key) {
                let parsed: Option<i64> = v.trim().parse().ok();
                let Some(parsed) = parsed.filter(|p| (1..=1000).contains(p)) else {
                    return Err(crate::config::ConfigError::invalid(
                        key,
                        "must be an integer in 1..=1000".to_string(),
                    ));
                };
                *slot = parsed;
            }
        }
        Ok(c)
    }
}

/// The customer view of a rule. Never a number: a blocked contact is its opaque id.
#[derive(Debug, Clone, Serialize, FromQueryResult)]
#[serde(rename_all = "camelCase")]
pub struct BlockRuleView {
    #[serde(rename = "blockId")]
    pub id: Uuid,
    pub blocker_kind: String,
    #[serde(rename = "principalId")]
    pub blocker_principal_id: Option<Uuid>,
    pub blocked_kind: String,
    pub blocked_principal_id: Option<Uuid>,
    #[serde(rename = "blockedContactReferenceId")]
    pub blocked_contact_reference: Option<Uuid>,
    pub reason_code: String,
    pub status: String,
    pub created_at: DateTime<Utc>,
    pub released_at: Option<DateTime<Utc>>,
    pub version: i64,
}

const VIEW_COLUMNS: &str = "id, blocker_kind, blocker_principal_id, blocked_kind, \
    blocked_principal_id, blocked_contact_reference, reason_code, status, created_at, \
    released_at, version";

/// `POST /v1/blocks` input.
#[derive(Debug, Clone)]
pub struct CreateBlockRule {
    /// The blocking principal (must resolve in the caller's scope); absent = the
    /// application itself blocks (requires `enterprise.config.write`).
    pub principal_id: Option<Uuid>,
    pub blocked_principal_id: Option<Uuid>,
    pub blocked_contact_reference_id: Option<Uuid>,
    pub reason_code: String,
}

/// Scope rule of block administration: PRINCIPAL rules need `calls.write` and the
/// principal resolved in the caller's scope; APPLICATION rules need
/// `enterprise.config.write`.
fn require_block_scope(
    ctx: &PlatformAuthContext,
    blocker: BlockerKind,
) -> Result<(), crate::error::CallingError> {
    match blocker {
        BlockerKind::Principal => ctx.require_scope(names::CALLS_WRITE),
        BlockerKind::Application => ctx.require_scope(names::ENTERPRISE_CONFIG_WRITE),
    }
}

impl AppCallService {
    /// Creates (or returns, idempotently) the ACTIVE rule of the pair.
    pub async fn create_block_rule(
        &self,
        ctx: &PlatformAuthContext,
        request: CreateBlockRule,
    ) -> Result<(BlockRuleView, bool), crate::error::CallingError> {
        let blocker = match request.principal_id {
            Some(_) => BlockerKind::Principal,
            None => BlockerKind::Application,
        };
        require_block_scope(ctx, blocker)?;
        let reason = BlockReason::parse(&request.reason_code).ok_or(
            crate::error::CallingError::Validation(
                "reasonCode must be UNWANTED, HARASSMENT, SPAM or OTHER.",
            ),
        )?;
        let (blocked_kind, blocked_principal, blocked_contact) =
            match (
                request.blocked_principal_id,
                request.blocked_contact_reference_id,
            ) {
                (Some(p), None) => (BlockedKind::Principal, Some(p), None),
                (None, Some(c)) => (BlockedKind::ContactReference, None, Some(c)),
                _ => return Err(crate::error::CallingError::Validation(
                    "Exactly one of blockedPrincipalId and blockedContactReferenceId is required.",
                )),
            };
        if request.principal_id.is_some() && request.principal_id == blocked_principal {
            return Err(crate::error::CallingError::Validation(
                "A principal cannot block itself.",
            ));
        }
        // Every principal must resolve in the caller's own scope (foreign = 404). A
        // suspended blocker may still block (a safety operation); a blocked principal may
        // be in any status.
        for id in request.principal_id.into_iter().chain(blocked_principal) {
            self.principals
                .resolve(ctx, &PrincipalReference::Id(PrincipalId::from_uuid(id)))
                .await
                .map(|_| ())
                .or_else(|e| match e {
                    PrincipalResolutionError::Inactive => Ok(()),
                    other => Err(super::create::resolution(other, false)),
                })?;
        }
        let (view, created) = self
            .store_rule(
                CallScope::of_platform(ctx),
                ctx.credential_id().as_uuid(),
                blocker,
                request.principal_id,
                blocked_kind,
                blocked_principal,
                blocked_contact,
                reason,
            )
            .await?;
        if created {
            crate::metrics::inc(
                "calling_block_rules_total",
                &[
                    ("blocker", blocker.as_str()),
                    ("blocked", blocked_kind.as_str()),
                ],
            );
            tracing::info!(
                block_id = %view.id,
                blocker = blocker.as_str(),
                blocked = blocked_kind.as_str(),
                "block rule created"
            );
        }
        Ok((view, created))
    }

    #[allow(clippy::too_many_arguments)]
    async fn store_rule(
        &self,
        scope: CallScope,
        credential_id: Uuid,
        blocker: BlockerKind,
        blocker_principal: Option<Uuid>,
        blocked_kind: BlockedKind,
        blocked_principal: Option<Uuid>,
        blocked_contact: Option<Uuid>,
        reason: BlockReason,
    ) -> CallResult<(BlockRuleView, bool)> {
        let now = self.calls.now();
        let txn = self.db().begin().await?;
        if let Some(existing) = BlockRuleView::find_by_statement(stmt(
            &format!(
                "SELECT {VIEW_COLUMNS} FROM calling_block_rules WHERE environment_id = $1 \
                 AND status = 'ACTIVE' AND blocker_kind = $2 \
                 AND blocker_principal_id IS NOT DISTINCT FROM $3 AND blocked_kind = $4 \
                 AND blocked_principal_id IS NOT DISTINCT FROM $5 \
                 AND blocked_contact_reference IS NOT DISTINCT FROM $6 FOR UPDATE"
            ),
            vec![
                scope.environment_id().as_uuid().into(),
                blocker.as_str().into(),
                blocker_principal.into(),
                blocked_kind.as_str().into(),
                blocked_principal.into(),
                blocked_contact.into(),
            ],
        ))
        .one(&txn)
        .await?
        {
            txn.rollback().await?;
            return Ok((existing, false));
        }
        let view = BlockRuleView::find_by_statement(stmt(
            &format!(
                "INSERT INTO calling_block_rules (id, tenant_id, application_id, environment_id, \
                 blocker_kind, blocker_principal_id, blocked_kind, blocked_principal_id, \
                 blocked_contact_reference, reason_code, status, created_by_credential_id, \
                 created_at, version) \
                 VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, 'ACTIVE', $11, $12, 1) \
                 RETURNING {VIEW_COLUMNS}"
            ),
            vec![
                Uuid::new_v4().into(),
                scope.tenant_id().as_uuid().into(),
                scope.application_id().as_uuid().into(),
                scope.environment_id().as_uuid().into(),
                blocker.as_str().into(),
                blocker_principal.into(),
                blocked_kind.as_str().into(),
                blocked_principal.into(),
                blocked_contact.into(),
                reason.as_str().into(),
                credential_id.into(),
                now.into(),
            ],
        ))
        .one(&txn)
        .await?
        .ok_or(CallError::Conflict("the block rule was not recorded"))?;
        txn.commit().await?;
        Ok((view, true))
    }

    /// `GET /v1/blocks?principalId=`: the rules of one principal (as blocker), or the
    /// application-level rules when no principal is given. Newest first, bounded.
    pub async fn list_block_rules(
        &self,
        ctx: &PlatformAuthContext,
        principal_id: Option<Uuid>,
    ) -> Result<Vec<BlockRuleView>, crate::error::CallingError> {
        ctx.require_scope(names::CALLS_READ)?;
        let scope = CallScope::of_platform(ctx);
        let (blocker, filter): (&str, Option<Uuid>) = match principal_id {
            Some(p) => {
                self.principals
                    .resolve(ctx, &PrincipalReference::Id(PrincipalId::from_uuid(p)))
                    .await
                    .map(|_| ())
                    .or_else(|e| match e {
                        PrincipalResolutionError::Inactive => Ok(()),
                        other => Err(super::create::resolution(other, false)),
                    })?;
                ("PRINCIPAL", Some(p))
            }
            None => ("APPLICATION", None),
        };
        Ok(self.rules_of(scope, blocker, filter).await?)
    }

    async fn rules_of(
        &self,
        scope: CallScope,
        blocker: &str,
        filter: Option<Uuid>,
    ) -> CallResult<Vec<BlockRuleView>> {
        Ok(BlockRuleView::find_by_statement(stmt(
            &format!(
                "SELECT {VIEW_COLUMNS} FROM calling_block_rules WHERE environment_id = $1 \
                 AND tenant_id = $2 AND application_id = $3 AND blocker_kind = $4 \
                 AND blocker_principal_id IS NOT DISTINCT FROM $5 \
                 ORDER BY created_at DESC, id LIMIT 200"
            ),
            vec![
                scope.environment_id().as_uuid().into(),
                scope.tenant_id().as_uuid().into(),
                scope.application_id().as_uuid().into(),
                blocker.into(),
                filter.into(),
            ],
        ))
        .all(self.db())
        .await?)
    }

    /// `DELETE /v1/blocks/:id`: releases the rule (idempotent; the row stays as
    /// evidence). A rule of another scope is 404.
    pub async fn release_block_rule(
        &self,
        ctx: &PlatformAuthContext,
        id: Uuid,
    ) -> Result<BlockRuleView, crate::error::CallingError> {
        let db = |e: sea_orm::DbErr| crate::error::CallingError::from(CallError::from(e));
        let scope = CallScope::of_platform(ctx);
        let txn = self.db().begin().await.map_err(db)?;
        let current = BlockRuleView::find_by_statement(stmt(
            &format!(
                "SELECT {VIEW_COLUMNS} FROM calling_block_rules WHERE id = $1 \
                 AND environment_id = $2 AND tenant_id = $3 AND application_id = $4 FOR UPDATE"
            ),
            vec![
                id.into(),
                scope.environment_id().as_uuid().into(),
                scope.tenant_id().as_uuid().into(),
                scope.application_id().as_uuid().into(),
            ],
        ))
        .one(&txn)
        .await
        .map_err(db)?
        .ok_or(CallError::NotFound)?;
        let blocker = BlockerKind::parse(&current.blocker_kind).ok_or(CallError::NotFound)?;
        // The scope rule is checked only after the rule proved to be the caller's own,
        // so a missing scope never reveals whether a foreign id exists.
        require_block_scope(ctx, blocker)?;
        if current.status == "RELEASED" {
            txn.rollback().await.map_err(db)?;
            return Ok(current);
        }
        let released = BlockRuleView::find_by_statement(stmt(
            &format!(
                "UPDATE calling_block_rules SET status = 'RELEASED', released_at = $2, \
                 version = version + 1 WHERE id = $1 RETURNING {VIEW_COLUMNS}"
            ),
            vec![id.into(), self.calls.now().into()],
        ))
        .one(&txn)
        .await
        .map_err(db)?
        .ok_or(CallError::NotFound)?;
        txn.commit().await.map_err(db)?;
        tracing::info!(block_id = %id, "block rule released");
        Ok(released)
    }

    /// Counts a refusal of `initiator` and raises the window's incident at the
    /// threshold. Best effort: never fails the refusal it observes.
    pub(crate) async fn note_refusal(
        &self,
        scope: CallScope,
        initiator: Uuid,
        kind: RefusalKind,
        now: DateTime<Utc>,
    ) {
        let threshold = self.abuse_controls.blocked_attempts_incident_threshold;
        let result = count_refusal(self.db(), scope, initiator, kind, now).await;
        match result {
            Ok(hits) if hits == threshold => {
                let window = window_start(now).timestamp();
                let incident = Incident::new(
                    kind.category(),
                    IncidentSeverity::Medium,
                    scope,
                    IncidentResource::Principal,
                    initiator,
                )
                .evidence(serde_json::json!({
                    "refusals": hits,
                    "windowStart": window,
                    "windowSeconds": REFUSAL_WINDOW.num_seconds(),
                }));
                super::incidents::raise(self.db(), &incident, now).await;
            }
            Ok(_) => {}
            Err(err) => tracing::warn!(error = %err, "refusal window could not be counted"),
        }
    }
}

fn window_start(now: DateTime<Utc>) -> DateTime<Utc> {
    let secs = REFUSAL_WINDOW.num_seconds();
    DateTime::from_timestamp(now.timestamp() / secs * secs, 0).unwrap_or(now)
}

/// Increments the refusal counter of `(environment, initiator)` in the current window
/// and prunes expired windows (bounded, best effort). Returns the new count.
async fn count_refusal<C: ConnectionTrait>(
    db: &C,
    scope: CallScope,
    initiator: Uuid,
    kind: RefusalKind,
    now: DateTime<Utc>,
) -> CallResult<i64> {
    let window = window_start(now);
    let row = db
        .query_one(stmt(
            "INSERT INTO calling_abuse_windows (environment_id, subject_kind, subject_id, window_start, hits) \
             VALUES ($1, $2, $3, $4, 1) \
             ON CONFLICT (environment_id, subject_kind, subject_id, window_start) \
             DO UPDATE SET hits = calling_abuse_windows.hits + 1 RETURNING hits",
            vec![
                scope.environment_id().as_uuid().into(),
                kind.subject_kind().into(),
                initiator.into(),
                window.into(),
            ],
        ))
        .await?
        .ok_or(CallError::Conflict("refusal window"))?;
    let hits: i64 = row.try_get_by_index(0)?;
    let _ = db
        .execute(stmt(
            "DELETE FROM calling_abuse_windows WHERE window_start < $1",
            vec![(window - REFUSAL_WINDOW * 2).into()],
        ))
        .await;
    Ok(hits)
}

/// Whether an ACTIVE rule separates `a` and `b` in either direction, or the application
/// blocks either of them.
pub(crate) async fn principal_pair_blocked<C: ConnectionTrait>(
    db: &C,
    environment_id: Uuid,
    a: Uuid,
    b: Uuid,
) -> CallResult<bool> {
    Ok(!blocked_targets(db, environment_id, a, &[b])
        .await?
        .is_empty())
}

/// The members of `candidates` that may not communicate with `initiator`: a rule of the
/// initiator on the candidate, of the candidate on the initiator, or an application rule
/// on either side (an application-blocked initiator blocks every candidate).
pub(crate) async fn blocked_targets<C: ConnectionTrait>(
    db: &C,
    environment_id: Uuid,
    initiator: Uuid,
    candidates: &[Uuid],
) -> CallResult<HashSet<Uuid>> {
    if candidates.is_empty() {
        return Ok(HashSet::new());
    }
    let rows = db
        .query_all(stmt(
            "SELECT c.id FROM unnest($3::uuid[]) AS c(id) \
             WHERE EXISTS (SELECT 1 FROM calling_block_rules r \
                WHERE r.environment_id = $1 AND r.status = 'ACTIVE' AND r.blocked_kind = 'PRINCIPAL' \
                  AND ((r.blocker_kind = 'PRINCIPAL' AND r.blocker_principal_id = $2 AND r.blocked_principal_id = c.id) \
                    OR (r.blocker_kind = 'PRINCIPAL' AND r.blocker_principal_id = c.id AND r.blocked_principal_id = $2) \
                    OR (r.blocker_kind = 'APPLICATION' AND r.blocked_principal_id IN ($2, c.id))))",
            vec![
                environment_id.into(),
                initiator.into(),
                candidates.to_vec().into(),
            ],
        ))
        .await?;
    rows.into_iter()
        .map(|r| r.try_get_by_index::<Uuid>(0).map_err(Into::into))
        .collect()
}

/// Whether `initiator` (or the application) blocks the public contact `contact`.
pub(crate) async fn contact_blocked_for_initiator<C: ConnectionTrait>(
    db: &C,
    environment_id: Uuid,
    initiator: Uuid,
    contact: Uuid,
) -> CallResult<bool> {
    let row = db
        .query_one(stmt(
            "SELECT EXISTS (SELECT 1 FROM calling_block_rules r \
                WHERE r.environment_id = $1 AND r.status = 'ACTIVE' \
                  AND r.blocked_kind = 'CONTACT_REFERENCE' AND r.blocked_contact_reference = $3 \
                  AND (r.blocker_kind = 'APPLICATION' \
                    OR (r.blocker_kind = 'PRINCIPAL' AND r.blocker_principal_id = $2)))",
            vec![environment_id.into(), initiator.into(), contact.into()],
        ))
        .await?;
    Ok(row
        .map(|r| r.try_get_by_index::<bool>(0))
        .transpose()?
        .unwrap_or(false))
}

/// Whether the application blocks the public contact `contact` (any direction).
pub(crate) async fn contact_blocked_by_application<C: ConnectionTrait>(
    db: &C,
    environment_id: Uuid,
    contact: Uuid,
) -> CallResult<bool> {
    let row = db
        .query_one(stmt(
            "SELECT EXISTS (SELECT 1 FROM calling_block_rules r \
                WHERE r.environment_id = $1 AND r.status = 'ACTIVE' AND r.blocker_kind = 'APPLICATION' \
                  AND r.blocked_kind = 'CONTACT_REFERENCE' AND r.blocked_contact_reference = $2)",
            vec![environment_id.into(), contact.into()],
        ))
        .await?;
    Ok(row
        .map(|r| r.try_get_by_index::<bool>(0))
        .transpose()?
        .unwrap_or(false))
}

/// The members of `candidates` that block the public contact `contact`.
pub(crate) async fn principals_blocking_contact<C: ConnectionTrait>(
    db: &C,
    environment_id: Uuid,
    contact: Uuid,
    candidates: &[Uuid],
) -> CallResult<HashSet<Uuid>> {
    if candidates.is_empty() {
        return Ok(HashSet::new());
    }
    let rows = db
        .query_all(stmt(
            "SELECT r.blocker_principal_id FROM calling_block_rules r \
             WHERE r.environment_id = $1 AND r.status = 'ACTIVE' AND r.blocker_kind = 'PRINCIPAL' \
               AND r.blocked_kind = 'CONTACT_REFERENCE' AND r.blocked_contact_reference = $2 \
               AND r.blocker_principal_id = ANY($3::uuid[])",
            vec![
                environment_id.into(),
                contact.into(),
                candidates.to_vec().into(),
            ],
        ))
        .await?;
    rows.into_iter()
        .map(|r| r.try_get_by_index::<Uuid>(0).map_err(Into::into))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn refusal_windows_are_aligned_and_bounded() {
        let now = DateTime::parse_from_rfc3339("2026-10-15T10:17:42Z")
            .unwrap()
            .with_timezone(&Utc);
        assert_eq!(window_start(now).to_rfc3339(), "2026-10-15T10:10:00+00:00");
        let controls = AbuseControls::from_lookup(&|k| match k {
            "CALLING_BLOCKED_ATTEMPTS_INCIDENT_THRESHOLD" => Some("7".into()),
            _ => None,
        })
        .unwrap();
        assert_eq!(controls.blocked_attempts_incident_threshold, 7);
        assert_eq!(controls.abuse_reports_incident_threshold, 3);
        assert!(AbuseControls::from_lookup(&|_| Some("0".into())).is_err());
    }
}
