//! DATA-017: report-only reconciliation of the other services' ids this service holds.
//!
//! Cross-service references are opaque by design (no foreign keys across databases):
//! Platform Access principals / environments / tenants and Messaging conversations. This
//! report never changes data and never reads another service's database. It
//!
//! * inventories every foreign reference (rows and distinct ids, per owning service), and
//! * checks the invariants this service can prove locally: a principal link must belong
//!   to an existing account, every principal used by the chat integration must be one this
//!   service mapped, and a Platform Access environment must belong to exactly one tenant /
//!   application.
//!
//! Verifying that each id still exists at its owner (Platform Access identity change feed,
//! Messaging conversation reads) is the documented next step; the report says so in
//! `remoteVerification`. Run it with `cargo run --bin reconcile_references` (read-only
//! transaction) or call [`report`].

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
        name: "principal_links_to_accounts",
        owner: "platform-access",
        held_in: "communication_principal_links (subject_kind = USER)",
        sql: "SELECT count(*), count(DISTINCT l.principal_id), \
                     count(*) FILTER (WHERE u.id IS NULL) \
              FROM communication_principal_links l LEFT JOIN users u ON u.id = l.subject_id \
              WHERE l.subject_kind = 'USER'",
    },
    Spec {
        name: "principal_links_to_participants",
        owner: "platform-access",
        held_in: "communication_principal_links (subject_kind = PARTICIPANT)",
        sql: "SELECT count(*), count(DISTINCT principal_id), 0::bigint \
              FROM communication_principal_links WHERE subject_kind = 'PARTICIPANT'",
    },
    Spec {
        name: "chat_conversation_owners",
        owner: "messaging",
        held_in: "conference_chat_links (conversation_id, owner_principal_id)",
        sql: "SELECT count(*), count(DISTINCT c.conversation_id), \
                     count(*) FILTER (WHERE NOT EXISTS (SELECT 1 FROM communication_principal_links l \
                         WHERE l.principal_id = c.owner_principal_id AND l.environment_id = c.environment_id)) \
              FROM conference_chat_links c",
    },
    Spec {
        name: "chat_member_principals",
        owner: "platform-access",
        held_in: "conference_chat_member_sync (principal_id)",
        sql: "SELECT count(*), count(DISTINCT m.principal_id), \
                     count(*) FILTER (WHERE NOT EXISTS (SELECT 1 FROM communication_principal_links l \
                         WHERE l.principal_id = m.principal_id)) \
              FROM conference_chat_member_sync m",
    },
    Spec {
        name: "pending_session_revocations",
        owner: "messaging",
        held_in: "communication_session_revocations (status = PENDING)",
        sql: "SELECT count(*), count(DISTINCT r.principal_id), \
                     count(*) FILTER (WHERE NOT EXISTS (SELECT 1 FROM communication_principal_links l \
                         WHERE l.principal_id = r.principal_id AND l.environment_id = r.environment_id)) \
              FROM communication_session_revocations r WHERE r.status = 'PENDING'",
    },
    Spec {
        name: "platform_environment_meetings",
        owner: "platform-access",
        held_in: "meeting_spaces (platform_tenant_id, platform_application_id, platform_environment_id)",
        sql: "SELECT (SELECT count(*) FROM meeting_spaces WHERE platform_environment_id IS NOT NULL), \
                     (SELECT count(DISTINCT platform_environment_id) FROM meeting_spaces), \
                     (SELECT count(*) FROM (SELECT platform_environment_id FROM meeting_spaces \
                         WHERE platform_environment_id IS NOT NULL GROUP BY platform_environment_id \
                         HAVING count(DISTINCT (platform_tenant_id, platform_application_id)) > 1) x)",
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
    let anomalies = checks.iter().map(|c| c.anomalies).sum();
    Ok(ReferenceReport {
        generated_at: Utc::now(),
        checks,
        anomalies,
        remote_verification: "NOT_PERFORMED: existence at the owning service (Platform Access \
            identity change feed, Messaging conversation reads) is the documented next step",
    })
}
