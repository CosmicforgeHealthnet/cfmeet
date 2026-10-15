//! Abuse reports (Part H §69): a participant reports the Call it took part in.
//!
//! * `POST /v1/calls/:call_id/abuse-reports` (`calls.write`): the reporting principal
//!   must be a participant of that Call (a foreign Call is 404, like every foreign
//!   resource); the reported principal — the other party of a two-party Call — is
//!   derived by the server and never supplied. One report per (Call, reporter): a repeat
//!   returns the existing report.
//! * `GET /v1/abuse-reports?principalId=` (`calls.read`): the reports a principal filed.
//! * Operators review with `platform_admin abuse-report set-status` (audited).
//! * ≥ `CALLING_ABUSE_REPORTS_INCIDENT_THRESHOLD` reports against one principal within
//!   24 h raise an `ABUSE_REPORTS_BURST` incident (one per principal and day).
//!
//! A report carries a bounded category and an optional bounded comment CODE: never free
//! text, a number, a provider payload or media.

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
    error::CallingError,
    platform::{
        context::PlatformAuthContext,
        principals::{PrincipalReference, PrincipalResolutionError},
        scopes::names,
    },
};

text_enum!(AbuseCategory {
    Unwanted = "UNWANTED",
    Harassment = "HARASSMENT",
    Spam = "SPAM",
    FraudSuspected = "FRAUD_SUSPECTED",
    Other = "OTHER",
});

text_enum!(AbuseReportStatus {
    Open = "OPEN",
    Reviewed = "REVIEWED",
    Dismissed = "DISMISSED",
});

/// Reports against one principal are counted over this window.
pub const BURST_WINDOW: Duration = Duration::hours(24);

#[derive(Debug, Clone, Serialize, FromQueryResult)]
#[serde(rename_all = "camelCase")]
pub struct AbuseReportView {
    #[serde(rename = "abuseReportId")]
    pub id: Uuid,
    pub call_id: Uuid,
    #[serde(rename = "principalId")]
    pub reporting_principal_id: Uuid,
    pub reported_principal_id: Option<Uuid>,
    pub category: String,
    pub comment_code: Option<String>,
    pub status: String,
    pub created_at: DateTime<Utc>,
    pub reviewed_at: Option<DateTime<Utc>>,
    pub version: i64,
}

const VIEW_COLUMNS: &str = "id, call_id, reporting_principal_id, reported_principal_id, \
    category, comment_code, status, created_at, reviewed_at, version";

#[derive(Debug, Clone)]
pub struct CreateAbuseReport {
    pub principal_id: Uuid,
    pub category: String,
    pub comment_code: Option<String>,
}

fn valid_comment_code(code: &str) -> bool {
    !code.is_empty()
        && code.len() <= 64
        && code
            .bytes()
            .all(|b| b.is_ascii_uppercase() || b.is_ascii_digit() || b == b'_')
}

impl AppCallService {
    /// Files (or returns, idempotently) the reporter's report of `call_id`.
    pub async fn create_abuse_report(
        &self,
        ctx: &PlatformAuthContext,
        call_id: Uuid,
        request: CreateAbuseReport,
    ) -> Result<(AbuseReportView, bool), CallingError> {
        ctx.require_scope(names::CALLS_WRITE)?;
        let category = AbuseCategory::parse(&request.category).ok_or(CallingError::Validation(
            "category must be UNWANTED, HARASSMENT, SPAM, FRAUD_SUSPECTED or OTHER.",
        ))?;
        if request
            .comment_code
            .as_deref()
            .is_some_and(|c| !valid_comment_code(c))
        {
            return Err(CallingError::Validation(
                "commentCode must match ^[A-Z0-9_]{1,64}$.",
            ));
        }
        // The reporter must be a principal of the caller's scope (any status).
        self.principals
            .resolve(
                ctx,
                &PrincipalReference::Id(PrincipalId::from_uuid(request.principal_id)),
            )
            .await
            .map(|_| ())
            .or_else(|e| match e {
                PrincipalResolutionError::Inactive => Ok(()),
                other => Err(super::create::resolution(other, false)),
            })?;
        let (view, created) = self
            .file_report(
                CallScope::of_platform(ctx),
                call_id,
                request.principal_id,
                category,
                request.comment_code.clone(),
            )
            .await?;
        if created {
            crate::metrics::inc(
                "calling_abuse_reports_total",
                &[("category", category.as_str())],
            );
            tracing::info!(abuse_report_id = %view.id, call_id = %call_id, category = category.as_str(), "abuse report filed");
        }
        Ok((view, created))
    }

    async fn file_report(
        &self,
        scope: CallScope,
        call_id: Uuid,
        principal_id: Uuid,
        category: AbuseCategory,
        comment_code: Option<String>,
    ) -> CallResult<(AbuseReportView, bool)> {
        let now = self.calls.now();
        let txn = self.db().begin().await?;
        // The Call must be the caller's own and the reporter one of its participants.
        let participants = txn
            .query_all(stmt(
                "SELECT DISTINCT p.principal_id FROM call_participants p \
                 JOIN calls c ON c.id = p.call_id \
                 WHERE c.id = $1 AND c.tenant_id = $2 AND c.application_id = $3 AND c.environment_id = $4",
                vec![
                    call_id.into(),
                    scope.tenant_id().as_uuid().into(),
                    scope.application_id().as_uuid().into(),
                    scope.environment_id().as_uuid().into(),
                ],
            ))
            .await?;
        let participants: Vec<Uuid> = participants
            .into_iter()
            .map(|r| r.try_get_by_index::<Uuid>(0))
            .collect::<Result<_, _>>()?;
        if participants.is_empty() {
            return Err(CallError::NotFound);
        }
        if !participants.contains(&principal_id) {
            return Err(CallError::Validation(
                "principalId must be a participant of the call.",
            ));
        }
        let others: Vec<Uuid> = participants
            .into_iter()
            .filter(|p| *p != principal_id)
            .collect();
        let reported = match others[..] {
            [only] => Some(only),
            _ => None,
        };
        if let Some(existing) = AbuseReportView::find_by_statement(stmt(
            &format!(
                "SELECT {VIEW_COLUMNS} FROM calling_abuse_reports \
                 WHERE call_id = $1 AND reporting_principal_id = $2 FOR UPDATE"
            ),
            vec![call_id.into(), principal_id.into()],
        ))
        .one(&txn)
        .await?
        {
            txn.rollback().await?;
            return Ok((existing, false));
        }
        let view = AbuseReportView::find_by_statement(stmt(
            &format!(
                "INSERT INTO calling_abuse_reports (id, tenant_id, application_id, environment_id, \
                 reporting_principal_id, call_id, reported_principal_id, category, comment_code, \
                 status, created_at, version) \
                 VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, 'OPEN', $10, 1) RETURNING {VIEW_COLUMNS}"
            ),
            vec![
                Uuid::new_v4().into(),
                scope.tenant_id().as_uuid().into(),
                scope.application_id().as_uuid().into(),
                scope.environment_id().as_uuid().into(),
                principal_id.into(),
                call_id.into(),
                reported.into(),
                category.as_str().into(),
                comment_code.into(),
                now.into(),
            ],
        ))
        .one(&txn)
        .await?
        .ok_or(CallError::Conflict("the abuse report was not recorded"))?;
        if let Some(reported) = reported {
            self.note_burst(&txn, scope, reported, now).await?;
        }
        txn.commit().await?;
        Ok((view, true))
    }

    /// Raises `ABUSE_REPORTS_BURST` once per principal and day when the reports against
    /// `reported` within [`BURST_WINDOW`] reach the threshold (counts only as evidence).
    async fn note_burst<C: ConnectionTrait + TransactionTrait>(
        &self,
        db: &C,
        scope: CallScope,
        reported: Uuid,
        now: DateTime<Utc>,
    ) -> CallResult<()> {
        let threshold = self.abuse_controls.abuse_reports_incident_threshold;
        let row = db
            .query_one(stmt(
                "SELECT count(*) FROM calling_abuse_reports \
                 WHERE environment_id = $1 AND reported_principal_id = $2 AND created_at > $3",
                vec![
                    scope.environment_id().as_uuid().into(),
                    reported.into(),
                    (now - BURST_WINDOW).into(),
                ],
            ))
            .await?;
        let count: i64 = row.map(|r| r.try_get_by_index(0)).transpose()?.unwrap_or(0);
        if count >= threshold {
            let day = now.format("%Y%m%d").to_string();
            let incident = Incident::new(
                IncidentCategory::AbuseReportsBurst,
                IncidentSeverity::Medium,
                scope,
                IncidentResource::Principal,
                reported,
            )
            .bucket(day)
            .evidence(serde_json::json!({
                "reports": count,
                "windowHours": BURST_WINDOW.num_hours(),
            }));
            super::incidents::raise(db, &incident, now).await;
        }
        Ok(())
    }

    /// `GET /v1/abuse-reports?principalId=`: the reports `principal_id` filed (newest
    /// first, bounded). The principal must be the caller's own.
    pub async fn list_abuse_reports(
        &self,
        ctx: &PlatformAuthContext,
        principal_id: Uuid,
    ) -> Result<Vec<AbuseReportView>, CallingError> {
        ctx.require_scope(names::CALLS_READ)?;
        self.principals
            .resolve(
                ctx,
                &PrincipalReference::Id(PrincipalId::from_uuid(principal_id)),
            )
            .await
            .map(|_| ())
            .or_else(|e| match e {
                PrincipalResolutionError::Inactive => Ok(()),
                other => Err(super::create::resolution(other, false)),
            })?;
        Ok(self
            .reports_of(CallScope::of_platform(ctx), principal_id)
            .await?)
    }

    async fn reports_of(
        &self,
        scope: CallScope,
        principal_id: Uuid,
    ) -> CallResult<Vec<AbuseReportView>> {
        Ok(AbuseReportView::find_by_statement(stmt(
            &format!(
                "SELECT {VIEW_COLUMNS} FROM calling_abuse_reports WHERE environment_id = $1 \
                 AND tenant_id = $2 AND application_id = $3 AND reporting_principal_id = $4 \
                 ORDER BY created_at DESC, id LIMIT 200"
            ),
            vec![
                scope.environment_id().as_uuid().into(),
                scope.tenant_id().as_uuid().into(),
                scope.application_id().as_uuid().into(),
                principal_id.into(),
            ],
        ))
        .all(self.db())
        .await?)
    }
}

/// Operator review (`platform_admin abuse-report set-status`): moves the report to
/// `status`, inside the caller's transaction (audited alongside). `None` = unknown id.
pub async fn set_status<C: ConnectionTrait>(
    db: &C,
    id: Uuid,
    status: AbuseReportStatus,
    now: DateTime<Utc>,
) -> CallResult<Option<AbuseReportView>> {
    let reviewed_at: Option<DateTime<Utc>> = match status {
        AbuseReportStatus::Open => None,
        _ => Some(now),
    };
    Ok(AbuseReportView::find_by_statement(stmt(
        &format!(
            "UPDATE calling_abuse_reports SET status = $2, reviewed_at = $3, version = version + 1 \
             WHERE id = $1 RETURNING {VIEW_COLUMNS}"
        ),
        vec![id.into(), status.as_str().into(), reviewed_at.into()],
    ))
    .one(db)
    .await?)
}

/// `platform_admin abuse-report show`.
pub async fn report<C: ConnectionTrait>(db: &C, id: Uuid) -> CallResult<Option<AbuseReportView>> {
    Ok(AbuseReportView::find_by_statement(stmt(
        &format!("SELECT {VIEW_COLUMNS} FROM calling_abuse_reports WHERE id = $1"),
        vec![id.into()],
    ))
    .one(db)
    .await?)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn comment_codes_are_bounded_codes_never_text() {
        assert!(valid_comment_code("THREATS_MADE"));
        assert!(valid_comment_code("C1"));
        assert!(!valid_comment_code(""));
        assert!(!valid_comment_code("free text"));
        assert!(!valid_comment_code("lower"));
        assert!(!valid_comment_code("+2348012345678"));
        assert!(!valid_comment_code(&"A".repeat(65)));
    }
}
