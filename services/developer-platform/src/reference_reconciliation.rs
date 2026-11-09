//! DATA-017: report-only reconciliation of the foreign references the Developer Platform
//! holds (the meeting-service `reconcile_references` pattern).
//!
//! Every tenant / application / environment / service account / credential id here is an
//! opaque Platform Access id and every workspace member user id a Meeting Service account
//! id: no foreign key can cross the databases. This report never changes data and never
//! reads another service's database. It
//!
//! * inventories every foreign reference (rows and distinct ids, per owning service);
//! * checks the invariants provable locally: a Platform Access environment belongs to
//!   exactly ONE tenant / application wherever it is recorded, an environment's tenant is
//!   its project's tenant, a LIVE production environment is a known environment of the
//!   request's own project;
//! * optionally ([`verify_remote`], with Platform Access configured) asks Platform Access
//!   whether each organization, project and environment still exists and still belongs
//!   to the same parent (read-only delegated reads; `MISSING` / `MISMATCHED` findings).
//!
//! Nothing is repaired or deleted: an anomaly is for an operator to investigate.
//! `cargo run --bin developer-platform-reconcile-references` (exit 1 when anything needs
//! attention), and the worker logs the same report every
//! `DEVELOPER_PLATFORM_REFERENCE_RECONCILIATION_INTERVAL_SECS`.

use cf_service_auth::CorrelationId;
use chrono::{DateTime, Utc};
use sea_orm::{
    AccessMode, ConnectionTrait, DatabaseConnection, DbBackend, DbErr, IsolationLevel, Statement,
    TransactionTrait,
};
use serde::Serialize;
use uuid::Uuid;

use crate::platform_access::{Delegated, DelegationCall, PlatformAccessPort};

/// The worker schedule key of the periodic report.
pub const JOB: &str = "reference-reconciliation";

/// One reference check.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ReferenceCheck {
    pub name: &'static str,
    /// The service that owns the referenced ids.
    pub owner: &'static str,
    /// Where the references live in this service.
    pub held_in: &'static str,
    pub rows: i64,
    pub distinct_foreign_ids: i64,
    /// Rows that break a locally provable invariant (0 = consistent).
    pub anomalies: i64,
    /// `OK` or `ATTENTION` (report only; nothing is repaired).
    pub status: &'static str,
}

/// One remote finding (ids only).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteFinding {
    /// `TENANT` / `APPLICATION` / `ENVIRONMENT`.
    pub kind: &'static str,
    pub id: Uuid,
    /// `MISSING` (Platform Access answers 404) or `MISMATCHED` (another parent).
    pub problem: &'static str,
}

/// The remote existence check at Platform Access.
#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteVerification {
    pub examined: i64,
    pub missing: i64,
    pub mismatched: i64,
    /// Ids Platform Access could not answer for (not counted as anomalies).
    pub unavailable: i64,
    /// At most [`MAX_FINDINGS`] findings.
    pub findings: Vec<RemoteFinding>,
}

/// Findings listed in a report (the counters stay exact).
pub const MAX_FINDINGS: usize = 100;

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ReferenceReport {
    pub service: &'static str,
    pub generated_at: DateTime<Utc>,
    pub checks: Vec<ReferenceCheck>,
    /// Local anomalies plus remote MISSING / MISMATCHED findings.
    pub anomalies: i64,
    pub remote_verification: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub remote: Option<RemoteVerification>,
}

struct Spec {
    name: &'static str,
    owner: &'static str,
    held_in: &'static str,
    /// `SELECT rows, distinct_ids, anomalies` (one row, three BIGINT columns).
    sql: String,
}

/// Every environment the Developer Platform knows with its (tenant, application).
const KNOWN: &str =
    "(SELECT environment_id, tenant_id, application_id FROM devplat_environment_profiles \
     UNION SELECT environment_id, tenant_id, application_id FROM devplat_environment_refs)";

/// Rows of `table` whose environment is known under ANOTHER tenant / application.
fn scoped(name: &'static str, held_in: &'static str, table: &str) -> Spec {
    Spec {
        name,
        owner: "platform-access",
        held_in,
        sql: format!(
            "SELECT count(*), count(DISTINCT t.environment_id), \
                    count(*) FILTER (WHERE EXISTS (SELECT 1 FROM {KNOWN} k WHERE k.environment_id = t.environment_id) \
                        AND NOT EXISTS (SELECT 1 FROM {KNOWN} k WHERE k.environment_id = t.environment_id \
                            AND k.tenant_id = t.tenant_id AND k.application_id = t.application_id)) \
             FROM {table} t"
        ),
    }
}

fn specs() -> Vec<Spec> {
    vec![
        Spec {
            name: "environment_profiles_to_projects",
            owner: "platform-access",
            held_in: "devplat_environment_profiles (tenant_id, application_id, environment_id)",
            sql: "SELECT count(*), count(DISTINCT e.environment_id), \
                         count(*) FILTER (WHERE p.tenant_id IS DISTINCT FROM e.tenant_id) \
                  FROM devplat_environment_profiles e \
                  LEFT JOIN devplat_project_profiles p ON p.application_id = e.application_id"
                .into(),
        },
        Spec {
            name: "environment_single_owner",
            owner: "platform-access",
            held_in: "every (tenant_id, application_id, environment_id) recorded",
            sql: "WITH s AS ( \
                      SELECT environment_id, tenant_id, application_id FROM devplat_environment_profiles \
                      UNION ALL SELECT environment_id, tenant_id, application_id FROM devplat_environment_refs \
                      UNION ALL SELECT environment_id, tenant_id, application_id FROM devplat_webhook_subscriptions \
                      UNION ALL SELECT environment_id, tenant_id, application_id FROM devplat_quota_restrictions \
                      UNION ALL SELECT environment_id, tenant_id, application_id FROM devplat_usage_facts \
                      UNION ALL SELECT environment_id, tenant_id, application_id FROM devplat_credential_lifecycle_notices) \
                  SELECT (SELECT count(*) FROM s), (SELECT count(DISTINCT environment_id) FROM s), \
                         (SELECT count(*) FROM (SELECT environment_id FROM s GROUP BY environment_id \
                             HAVING count(DISTINCT (tenant_id, application_id)) > 1) x)"
                .into(),
        },
        scoped(
            "webhook_subscription_environments",
            "devplat_webhook_subscriptions (tenant_id, application_id, environment_id)",
            "devplat_webhook_subscriptions",
        ),
        scoped(
            "webhook_event_environments",
            "devplat_webhook_events (tenant_id, application_id, environment_id)",
            "devplat_webhook_events",
        ),
        scoped(
            "quota_restriction_environments",
            "devplat_quota_restrictions (tenant_id, application_id, environment_id, subject_id)",
            "devplat_quota_restrictions",
        ),
        scoped(
            "usage_fact_environments",
            "devplat_usage_facts (tenant_id, application_id, environment_id)",
            "devplat_usage_facts",
        ),
        scoped(
            "credential_notice_environments",
            "devplat_credential_lifecycle_notices (credential_id, service_account_id, environment_id)",
            "devplat_credential_lifecycle_notices",
        ),
        Spec {
            name: "production_environments",
            owner: "platform-access",
            held_in: "devplat_production_access_requests (production_environment_id)",
            sql: "SELECT count(*), count(DISTINCT r.production_environment_id), \
                         count(*) FILTER (WHERE r.state IN ('LIVE', 'SUSPENDED') AND NOT EXISTS ( \
                             SELECT 1 FROM devplat_environment_profiles e \
                              WHERE e.environment_id = r.production_environment_id \
                                AND e.application_id = r.application_id AND e.tenant_id = r.tenant_id)) \
                  FROM devplat_production_access_requests r WHERE r.production_environment_id IS NOT NULL"
                .into(),
        },
        Spec {
            name: "workspace_member_accounts",
            owner: "meeting-service",
            held_in: "devplat_workspace_members (user_id)",
            sql: "SELECT count(*), count(DISTINCT user_id), 0::bigint FROM devplat_workspace_members"
                .into(),
        },
    ]
}

/// The local report, read in one READ ONLY transaction (a consistent snapshot).
pub async fn report(db: &DatabaseConnection) -> Result<ReferenceReport, DbErr> {
    let txn = db
        .begin_with_config(
            Some(IsolationLevel::RepeatableRead),
            Some(AccessMode::ReadOnly),
        )
        .await?;
    let mut checks = Vec::new();
    for spec in specs() {
        let row = txn
            .query_one(Statement::from_string(
                DbBackend::Postgres,
                spec.sql.clone(),
            ))
            .await?
            .ok_or_else(|| DbErr::Custom(format!("{}: no result", spec.name)))?;
        let rows: i64 = row.try_get_by_index(0)?;
        let distinct_foreign_ids: i64 = row.try_get_by_index(1)?;
        let anomalies: i64 = row.try_get_by_index(2)?;
        checks.push(ReferenceCheck {
            name: spec.name,
            owner: spec.owner,
            held_in: spec.held_in,
            rows,
            distinct_foreign_ids,
            anomalies,
            status: if anomalies == 0 { "OK" } else { "ATTENTION" },
        });
    }
    txn.commit().await?;
    Ok(ReferenceReport {
        service: "developer-platform",
        generated_at: Utc::now(),
        anomalies: checks.iter().map(|c| c.anomalies).sum(),
        checks,
        remote_verification: "NOT_PERFORMED: run with Platform Access configured \
            (CF_PLATFORM_ACCESS_BASE_URL) to verify existence at the owner",
        remote: None,
    })
}

fn id_of(v: &serde_json::Value, k: &str) -> Option<Uuid> {
    v.get(k)
        .and_then(serde_json::Value::as_str)
        .and_then(|s| Uuid::parse_str(s).ok())
}

fn found(out: &mut RemoteVerification, kind: &'static str, id: Uuid, problem: &'static str) {
    if problem == "MISSING" {
        out.missing += 1;
    } else {
        out.mismatched += 1;
    }
    if out.findings.len() < MAX_FINDINGS {
        out.findings.push(RemoteFinding { kind, id, problem });
    }
}

/// Asks Platform Access whether the organizations, projects and environments of this
/// service still exist under the same parent (at most `limit` ids of each kind; closed
/// organizations are skipped). Read-only; never repairs.
pub async fn verify_remote(
    db: &DatabaseConnection,
    platform: &dyn PlatformAccessPort,
    limit: u64,
) -> Result<RemoteVerification, DbErr> {
    let correlation = CorrelationId::generate();
    let rows = |sql: &str| {
        Statement::from_sql_and_values(DbBackend::Postgres, sql, [(limit as i64).into()])
    };
    let mut out = RemoteVerification::default();
    let tenants = db
        .query_all(rows(
            "SELECT tenant_id FROM devplat_organizations WHERE state <> 'CLOSED' ORDER BY tenant_id LIMIT $1",
        ))
        .await?;
    for r in tenants {
        let id: Uuid = r.try_get("", "tenant_id")?;
        out.examined += 1;
        match platform
            .delegate(DelegationCall::GetTenant { tenant_id: id }, &correlation)
            .await
        {
            Ok(Delegated::Ok(_)) => {}
            Ok(Delegated::Refused { status: 404, .. }) => found(&mut out, "TENANT", id, "MISSING"),
            _ => out.unavailable += 1,
        }
    }
    let projects = db
        .query_all(rows(
            "SELECT application_id, tenant_id FROM devplat_project_profiles ORDER BY application_id LIMIT $1",
        ))
        .await?;
    for r in projects {
        let id: Uuid = r.try_get("", "application_id")?;
        let tenant: Uuid = r.try_get("", "tenant_id")?;
        out.examined += 1;
        match platform
            .delegate(
                DelegationCall::GetApplication { application_id: id },
                &correlation,
            )
            .await
        {
            Ok(Delegated::Ok(v)) if id_of(&v, "tenantId") == Some(tenant) => {}
            Ok(Delegated::Ok(_)) => found(&mut out, "APPLICATION", id, "MISMATCHED"),
            Ok(Delegated::Refused { status: 404, .. }) => {
                found(&mut out, "APPLICATION", id, "MISSING")
            }
            _ => out.unavailable += 1,
        }
    }
    let environments = db
        .query_all(rows(
            "SELECT environment_id, application_id, tenant_id FROM devplat_environment_profiles \
             ORDER BY environment_id LIMIT $1",
        ))
        .await?;
    for r in environments {
        let id: Uuid = r.try_get("", "environment_id")?;
        let app: Uuid = r.try_get("", "application_id")?;
        let tenant: Uuid = r.try_get("", "tenant_id")?;
        out.examined += 1;
        match platform
            .delegate(
                DelegationCall::GetEnvironment { environment_id: id },
                &correlation,
            )
            .await
        {
            Ok(Delegated::Ok(v))
                if id_of(&v, "tenantId") == Some(tenant)
                    && id_of(&v, "applicationId") == Some(app) => {}
            Ok(Delegated::Ok(_)) => found(&mut out, "ENVIRONMENT", id, "MISMATCHED"),
            Ok(Delegated::Refused { status: 404, .. }) => {
                found(&mut out, "ENVIRONMENT", id, "MISSING")
            }
            _ => out.unavailable += 1,
        }
    }
    Ok(out)
}

/// [`report`] plus, with Platform Access, [`verify_remote`].
pub async fn full_report(
    db: &DatabaseConnection,
    platform: Option<&dyn PlatformAccessPort>,
    remote_limit: u64,
) -> Result<ReferenceReport, DbErr> {
    let mut r = report(db).await?;
    if let Some(p) = platform {
        let remote = verify_remote(db, p, remote_limit).await?;
        r.anomalies += remote.missing + remote.mismatched;
        r.remote_verification = if remote.unavailable > 0 {
            "PARTIAL: Platform Access did not answer for some ids"
        } else {
            "PERFORMED: Platform Access delegated reads"
        };
        r.remote = Some(remote);
    }
    Ok(r)
}
