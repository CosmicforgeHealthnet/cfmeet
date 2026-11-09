//! DATA-017: report-only reconciliation of the foreign references Numbering holds (the
//! meeting-service `reconcile_references` pattern).
//!
//! Tenant / application / environment / principal ids are opaque Platform Access ids,
//! relay-lease holders are Privacy Relay references: no foreign key can cross the
//! databases. This report never changes data and never reads another service's
//! database. It inventories every foreign reference (rows and distinct ids, per owning
//! service) and checks what is provable locally:
//!
//! * a Platform Access environment belongs to exactly ONE tenant / application wherever
//!   Numbering recorded it;
//! * an ORGANIZATION-owned number belongs to the authenticated organization, i.e. its
//!   owner reference IS the tenant id (`acquisition::assignments::resolve_owner`), in
//!   assignments, their history and release requests;
//! * a PRINCIPAL owner reference is never a tenant id;
//! * an assignment's history names the assignment's own (immutable) legal owner.
//!
//! Nothing is repaired or deleted. Verifying existence at Platform Access is reported as
//! `remoteVerification: NOT_PERFORMED`. Run it with
//! `cargo run --bin numbering-reconcile-references` (read-only transaction; exit 1 when
//! anomalies exist) or call [`report`].

use chrono::{DateTime, Utc};
use sea_orm::{
    AccessMode, ConnectionTrait, DatabaseConnection, DbBackend, DbErr, IsolationLevel, Statement,
    TransactionTrait,
};
use serde::Serialize;

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

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ReferenceReport {
    pub service: &'static str,
    pub generated_at: DateTime<Utc>,
    pub checks: Vec<ReferenceCheck>,
    pub anomalies: i64,
    pub remote_verification: &'static str,
}

struct Spec {
    name: &'static str,
    owner: &'static str,
    held_in: &'static str,
    /// `SELECT rows, distinct_ids, anomalies` (one row, three BIGINT columns).
    sql: &'static str,
}

const SPECS: &[Spec] = &[
    Spec {
        name: "environment_single_owner",
        owner: "platform-access",
        held_in: "every (tenant_id, application_id, environment_id) recorded",
        sql: "WITH s AS ( \
                SELECT tenant_id, application_id, environment_id FROM numbering_outbox_events \
                UNION ALL SELECT tenant_id, application_id, environment_id FROM number_routing_profiles \
                UNION ALL SELECT tenant_id, application_id, environment_id FROM number_reservations \
                UNION ALL SELECT tenant_id, application_id, environment_id FROM dcn_assignments \
                UNION ALL SELECT tenant_id, application_id, environment_id FROM number_kyc_cases \
                UNION ALL SELECT tenant_id, application_id, environment_id FROM number_eligibility_decisions \
                UNION ALL SELECT tenant_id, application_id, environment_id FROM number_acquisitions \
                UNION ALL SELECT tenant_id, application_id, environment_id FROM number_pools \
                UNION ALL SELECT tenant_id, application_id, environment_id FROM number_routing_grants \
                UNION ALL SELECT tenant_id, application_id, environment_id FROM number_suspensions \
                UNION ALL SELECT tenant_id, application_id, environment_id FROM number_release_requests \
                UNION ALL SELECT tenant_id, application_id, environment_id FROM number_usage_records \
                UNION ALL SELECT tenant_id, application_id, environment_id FROM number_risk_decisions \
                UNION ALL SELECT tenant_id, application_id, environment_id FROM number_port_requests), \
              k AS (SELECT * FROM s WHERE tenant_id IS NOT NULL AND application_id IS NOT NULL \
                    AND environment_id IS NOT NULL) \
              SELECT (SELECT count(*) FROM k), (SELECT count(DISTINCT environment_id) FROM k), \
                     (SELECT count(*) FROM (SELECT environment_id FROM k GROUP BY environment_id \
                         HAVING count(DISTINCT (tenant_id, application_id)) > 1) x)",
    },
    Spec {
        name: "organization_owner_refs",
        owner: "platform-access",
        held_in: "dcn_assignments / number_assignment_history / number_release_requests \
                  (owner_kind = ORGANIZATION, owner_ref)",
        sql: "WITH o AS ( \
                SELECT tenant_id, owner_ref FROM dcn_assignments WHERE owner_kind = 'ORGANIZATION' \
                UNION ALL SELECT tenant_id, owner_ref FROM number_assignment_history WHERE owner_kind = 'ORGANIZATION' \
                UNION ALL SELECT tenant_id, owner_ref FROM number_release_requests WHERE owner_kind = 'ORGANIZATION') \
              SELECT count(*), count(DISTINCT owner_ref), count(*) FILTER (WHERE owner_ref <> tenant_id) FROM o",
    },
    Spec {
        name: "principal_owner_refs",
        owner: "platform-access",
        held_in: "dcn_assignments (owner_kind = PRINCIPAL, owner_ref)",
        sql: "SELECT count(*), count(DISTINCT owner_ref), \
                     count(*) FILTER (WHERE owner_ref = tenant_id OR owner_ref = application_id \
                         OR owner_ref = environment_id) \
              FROM dcn_assignments WHERE owner_kind = 'PRINCIPAL'",
    },
    Spec {
        name: "assignment_history_owners",
        owner: "platform-access",
        held_in: "number_assignment_history (owner_kind, owner_ref) against its assignment",
        sql: "SELECT count(*), count(DISTINCT h.owner_ref), \
                     count(*) FILTER (WHERE a.id IS NULL OR a.owner_kind <> h.owner_kind \
                         OR a.owner_ref <> h.owner_ref) \
              FROM number_assignment_history h LEFT JOIN dcn_assignments a ON a.id = h.assignment_id",
    },
    Spec {
        name: "reservation_principals",
        owner: "platform-access",
        held_in: "number_reservations (requested_for_principal_id)",
        sql: "SELECT count(*), count(DISTINCT requested_for_principal_id), 0::bigint \
              FROM number_reservations WHERE requested_for_principal_id IS NOT NULL",
    },
    Spec {
        name: "routing_grant_principals",
        owner: "platform-access",
        held_in: "number_routing_grants (principal_id)",
        sql: "SELECT count(*), count(DISTINCT principal_id), 0::bigint \
              FROM number_routing_grants WHERE principal_id IS NOT NULL",
    },
    Spec {
        name: "kyc_subjects",
        owner: "platform-access",
        held_in: "number_kyc_cases (subject_ref)",
        sql: "SELECT count(*), count(DISTINCT subject_ref), 0::bigint FROM number_kyc_cases",
    },
    Spec {
        name: "relay_lease_holders",
        owner: "privacy-relay",
        held_in: "number_relay_leases (holder_service, holder_ref; status = ACTIVE)",
        sql: "SELECT count(*), count(DISTINCT holder_ref), 0::bigint \
              FROM number_relay_leases WHERE status = 'ACTIVE'",
    },
];

/// The report, read in one READ ONLY transaction (a consistent snapshot).
pub async fn report(db: &DatabaseConnection) -> Result<ReferenceReport, DbErr> {
    let txn = db
        .begin_with_config(
            Some(IsolationLevel::RepeatableRead),
            Some(AccessMode::ReadOnly),
        )
        .await?;
    let mut checks = Vec::with_capacity(SPECS.len());
    for spec in SPECS {
        let row = txn
            .query_one(Statement::from_string(DbBackend::Postgres, spec.sql))
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
        service: "numbering",
        generated_at: Utc::now(),
        anomalies: checks.iter().map(|c| c.anomalies).sum(),
        checks,
        remote_verification: "NOT_PERFORMED: existence at Platform Access (principal resolution, \
            identity change feed) and Privacy Relay is the documented next step",
    })
}
