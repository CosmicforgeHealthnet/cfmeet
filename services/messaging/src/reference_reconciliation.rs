//! DATA-017: report-only reconciliation of the foreign references Messaging holds (the
//! meeting-service `reconcile_references` pattern).
//!
//! Tenant / application / environment / principal / service account ids are opaque
//! Platform Access ids, SMS bridges name Numbering number resources and controlled
//! conversation contexts are Privacy Relay references: no foreign key can cross the
//! databases. This report never changes data and never reads another service's
//! database. It inventories every foreign reference (rows and distinct ids, per owning
//! service) and checks what is provable locally:
//!
//! * a Platform Access environment belongs to exactly ONE tenant / application wherever
//!   Messaging recorded it;
//! * a conversation member, an SMS bridge and a client session carry the same Platform
//!   Access scope (environment, and principal for sessions) as the conversation / device
//!   they hang off;
//! * a conversation whose Privacy Relay context authority is REVOKED is not open, and one
//!   whose authority is SUSPENDED is not ACTIVE (`integrations::context_authority`).
//!
//! Nothing is repaired or deleted. Verifying existence at Platform Access, Numbering and
//! Privacy Relay is reported as `remoteVerification: NOT_PERFORMED`. Run it with
//! `cargo run --bin messaging-reconcile-references` (read-only transaction; exit 1 when
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
                SELECT tenant_id, application_id, environment_id FROM messaging_conversations \
                UNION ALL SELECT tenant_id, application_id, environment_id FROM messaging_devices \
                UNION ALL SELECT tenant_id, application_id, environment_id FROM messaging_client_sessions \
                UNION ALL SELECT tenant_id, application_id, environment_id FROM messaging_session_devices \
                UNION ALL SELECT tenant_id, application_id, environment_id FROM messaging_sms_bridges \
                UNION ALL SELECT tenant_id, application_id, environment_id FROM messaging_context_authorities \
                UNION ALL SELECT tenant_id, application_id, environment_id FROM messaging_principal_restrictions) \
              SELECT (SELECT count(*) FROM s), (SELECT count(DISTINCT environment_id) FROM s), \
                     (SELECT count(*) FROM (SELECT environment_id FROM s GROUP BY environment_id \
                         HAVING count(DISTINCT (tenant_id, application_id)) > 1) x)",
    },
    Spec {
        name: "conversation_member_principals",
        owner: "platform-access",
        held_in: "messaging_conversation_members (principal_id, environment_id)",
        sql: "SELECT count(*), count(DISTINCT m.principal_id), \
                     count(*) FILTER (WHERE m.environment_id <> c.environment_id) \
              FROM messaging_conversation_members m \
              JOIN messaging_conversations c ON c.id = m.conversation_id",
    },
    Spec {
        name: "conversation_owner_principals",
        owner: "platform-access",
        held_in: "messaging_conversations (owner_principal_id, created_by_principal_id)",
        sql: "SELECT count(*), count(DISTINCT owner_principal_id), 0::bigint \
              FROM messaging_conversations",
    },
    Spec {
        name: "device_principals",
        owner: "platform-access",
        held_in: "messaging_devices (principal_id)",
        sql: "SELECT count(*), count(DISTINCT principal_id), 0::bigint FROM messaging_devices",
    },
    Spec {
        name: "client_session_devices",
        owner: "platform-access",
        held_in: "messaging_client_sessions (principal_id, service_account_id, device_id)",
        sql: "SELECT count(*), count(DISTINCT s.principal_id), \
                     count(*) FILTER (WHERE s.environment_id <> d.environment_id \
                         OR s.principal_id <> d.principal_id OR s.tenant_id <> d.tenant_id \
                         OR s.application_id <> d.application_id) \
              FROM messaging_client_sessions s JOIN messaging_devices d ON d.id = s.device_id",
    },
    Spec {
        name: "sms_bridge_numbers",
        owner: "numbering",
        held_in: "messaging_sms_bridges (number_resource_id, conversation scope)",
        sql: "SELECT count(*), count(DISTINCT b.number_resource_id), \
                     count(*) FILTER (WHERE (b.tenant_id, b.application_id, b.environment_id) \
                         IS DISTINCT FROM (c.tenant_id, c.application_id, c.environment_id)) \
              FROM messaging_sms_bridges b JOIN messaging_conversations c ON c.id = b.conversation_id",
    },
    Spec {
        name: "controlled_context_authorities",
        owner: "privacy-relay",
        held_in: "messaging_conversations (context_namespace = privacy-relay, context_ref)",
        sql: "SELECT count(*), count(DISTINCT (c.environment_id, c.context_ref)), \
                     count(*) FILTER (WHERE (a.state = 'REVOKED' AND c.lifecycle_state IN ('ACTIVE', 'LOCKED')) \
                         OR (a.state = 'SUSPENDED' AND c.lifecycle_state = 'ACTIVE')) \
              FROM messaging_conversations c \
              LEFT JOIN messaging_context_authorities a ON a.environment_id = c.environment_id \
                   AND a.namespace = c.context_namespace AND a.reference = c.context_ref \
              WHERE c.context_namespace = 'privacy-relay'",
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
        service: "messaging",
        generated_at: Utc::now(),
        anomalies: checks.iter().map(|c| c.anomalies).sum(),
        checks,
        remote_verification: "NOT_PERFORMED: existence at Platform Access (identity change \
            feed), Numbering and Privacy Relay is the documented next step",
    })
}
