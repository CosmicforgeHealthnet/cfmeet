//! Blueprint Section 4 — legacy organization export for the Organizations Service.
//!
//! Format and checksum are frozen in `docs/api/organizations-internal-v1.md` §5; the
//! Organizations Service recomputes the same checksum on import and refuses a mismatch.
use chrono::{DateTime, Utc};
use sea_orm::{ConnectionTrait, DatabaseConnection, DbBackend, Statement, TransactionTrait};
use serde::Serialize;
use serde_json::Value;
use sha2::{Digest, Sha256};
use uuid::Uuid;

pub const EXPORT_SCHEMA_VERSION: u32 = 1;
pub const EXPORT_SOURCE: &str = "cfmeet.meeting-service";

#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ExportedOrganization {
    pub id: Uuid,
    pub name: String,
    pub slug: String,
    pub status: String,
    pub created_by: Option<Uuid>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub profile_policy: Value,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ExportedMembership {
    pub id: Uuid,
    pub organization_id: Uuid,
    pub user_id: Uuid,
    pub role: String,
    pub status: String,
    pub created_by: Option<Uuid>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub removed_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct ExportCounts {
    pub organizations: usize,
    pub memberships: usize,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OrganizationExport {
    pub schema_version: u32,
    pub source: &'static str,
    pub exported_at: DateTime<Utc>,
    pub organizations: Vec<ExportedOrganization>,
    pub memberships: Vec<ExportedMembership>,
    pub counts: ExportCounts,
    pub checksum: String,
}

#[derive(Debug, thiserror::Error)]
pub enum ExportError {
    #[error("database: {0}")]
    Database(#[from] sea_orm::DbErr),
}

fn sha256_hex(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}

/// The frozen two-sided checksum (contract §5): one line per organization sorted by id,
/// then one line per membership sorted by id, each terminated by `\n`.
pub fn checksum(
    organizations: &[ExportedOrganization],
    memberships: &[ExportedMembership],
) -> String {
    let mut orgs: Vec<&ExportedOrganization> = organizations.iter().collect();
    orgs.sort_by_key(|o| o.id);
    let mut mems: Vec<&ExportedMembership> = memberships.iter().collect();
    mems.sort_by_key(|m| m.id);
    let mut text = String::new();
    for o in orgs {
        text.push_str(&format!(
            "org|{}|{}|{}|{}|{}|{}\n",
            o.id,
            sha256_hex(o.name.as_bytes()),
            o.slug,
            o.status,
            o.created_by
                .map_or_else(|| "-".to_string(), |u| u.to_string()),
            o.created_at.timestamp_micros(),
        ));
    }
    for m in mems {
        text.push_str(&format!(
            "mem|{}|{}|{}|{}|{}|{}\n",
            m.id,
            m.organization_id,
            m.user_id,
            m.role,
            m.status,
            m.created_at.timestamp_micros(),
        ));
    }
    sha256_hex(text.as_bytes())
}

/// Read every legacy organization and membership in one consistent, read-only snapshot.
pub async fn export(db: &DatabaseConnection) -> Result<OrganizationExport, ExportError> {
    let txn = db.begin().await?;
    txn.execute_unprepared("SET TRANSACTION ISOLATION LEVEL REPEATABLE READ, READ ONLY")
        .await?;
    let org_rows = txn
        .query_all(Statement::from_string(
            DbBackend::Postgres,
            "SELECT id, name, slug, status::text AS status, created_by, created_at, updated_at, \
                    profile_policy \
             FROM organizations ORDER BY id"
                .to_owned(),
        ))
        .await?;
    let mem_rows = txn
        .query_all(Statement::from_string(
            DbBackend::Postgres,
            "SELECT id, organization_id, user_id, role::text AS role, status::text AS status, \
                    created_by, created_at, updated_at, removed_at \
             FROM organization_memberships ORDER BY id"
                .to_owned(),
        ))
        .await?;
    let exported_at: DateTime<Utc> = txn
        .query_one(Statement::from_string(
            DbBackend::Postgres,
            "SELECT now() AS at".to_owned(),
        ))
        .await?
        .ok_or_else(|| sea_orm::DbErr::Custom("clock unavailable".into()))?
        .try_get("", "at")?;
    txn.commit().await?;

    let organizations = org_rows
        .iter()
        .map(|r| {
            Ok(ExportedOrganization {
                id: r.try_get("", "id")?,
                name: r.try_get("", "name")?,
                slug: r.try_get("", "slug")?,
                status: r.try_get("", "status")?,
                created_by: r.try_get("", "created_by")?,
                created_at: r.try_get("", "created_at")?,
                updated_at: r.try_get("", "updated_at")?,
                profile_policy: r.try_get("", "profile_policy")?,
            })
        })
        .collect::<Result<Vec<_>, sea_orm::DbErr>>()?;
    let memberships = mem_rows
        .iter()
        .map(|r| {
            Ok(ExportedMembership {
                id: r.try_get("", "id")?,
                organization_id: r.try_get("", "organization_id")?,
                user_id: r.try_get("", "user_id")?,
                role: r.try_get("", "role")?,
                status: r.try_get("", "status")?,
                created_by: r.try_get("", "created_by")?,
                created_at: r.try_get("", "created_at")?,
                updated_at: r.try_get("", "updated_at")?,
                removed_at: r.try_get("", "removed_at")?,
            })
        })
        .collect::<Result<Vec<_>, sea_orm::DbErr>>()?;
    let checksum = checksum(&organizations, &memberships);
    Ok(OrganizationExport {
        schema_version: EXPORT_SCHEMA_VERSION,
        source: EXPORT_SOURCE,
        exported_at,
        counts: ExportCounts {
            organizations: organizations.len(),
            memberships: memberships.len(),
        },
        organizations,
        memberships,
        checksum,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    /// The shared test vector (contract §5): the Organizations Service import must compute
    /// the same value for the same rows.
    fn vector() -> (Vec<ExportedOrganization>, Vec<ExportedMembership>) {
        let at = Utc.with_ymd_and_hms(2026, 10, 4, 12, 0, 0).unwrap();
        let org = Uuid::parse_str("00000000-0000-4000-8000-000000000001").unwrap();
        let owner = Uuid::parse_str("00000000-0000-4000-8000-0000000000a1").unwrap();
        (
            vec![ExportedOrganization {
                id: org,
                name: "Acme Health".into(),
                slug: "acme-health".into(),
                status: "ACTIVE".into(),
                created_by: None,
                created_at: at,
                updated_at: at,
                profile_policy: serde_json::json!({}),
            }],
            vec![ExportedMembership {
                id: Uuid::parse_str("00000000-0000-4000-8000-0000000000b1").unwrap(),
                organization_id: org,
                user_id: owner,
                role: "OWNER".into(),
                status: "ACTIVE".into(),
                created_by: None,
                created_at: at,
                updated_at: at,
                removed_at: None,
            }],
        )
    }

    #[test]
    fn checksum_matches_the_frozen_line_format_and_is_order_independent() {
        let (orgs, mems) = vector();
        let expected_text = format!(
            "org|00000000-0000-4000-8000-000000000001|{}|acme-health|ACTIVE|-|1791115200000000\n\
             mem|00000000-0000-4000-8000-0000000000b1|00000000-0000-4000-8000-000000000001|00000000-0000-4000-8000-0000000000a1|OWNER|ACTIVE|1791115200000000\n",
            sha256_hex(b"Acme Health")
        );
        assert_eq!(checksum(&orgs, &mems), sha256_hex(expected_text.as_bytes()));
        assert_eq!(
            checksum(&orgs, &mems),
            "1a212f81add323e1e9bed715699c0ba8c97b2dc22e211c84a05bca42d0908e78"
        );
        let mut reversed = mems.clone();
        reversed.reverse();
        assert_eq!(checksum(&orgs, &reversed), checksum(&orgs, &mems));
        let mut renamed = orgs.clone();
        renamed[0].name = "Acme Health Ltd".into();
        assert_ne!(checksum(&renamed, &mems), checksum(&orgs, &mems));
    }
}
