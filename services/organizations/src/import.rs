//! Meeting Service → Organizations migration import (contract §5).
//!
//! * The export is refused on a different `schemaVersion` or `source`, a count mismatch, a
//!   checksum mismatch, invalid values, duplicate ids, two non-removed memberships of one
//!   organization + user, or a membership whose organization is neither in the export nor
//!   already imported.
//! * Every id and timestamp is preserved; `creationSource=IMPORTED`,
//!   `provisioningState=PENDING`, `joinedVia=IMPORTED`; source `status` → `lifecycleStatus`
//!   unchanged; legacy roles OWNER / ADMIN / MEMBER unchanged.
//! * Re-importing identical rows is a no-op. A row that differs from the stored one is
//!   reported in `conflicts` and never overwritten (the cutover gate, Section 4 step 4, is
//!   the only place a re-sync may ever be allowed).
//! * One transaction per import (serialized by an advisory lock); every import writes an
//!   audit event and an `organization_import_runs` receipt; imported rows emit
//!   `organization.created` / `membership.activated` to the outbox.
//!
//! The same code backs `POST /internal/v1/migration/import` and the
//! `organizations-import` CLI.

use std::collections::{BTreeMap, BTreeSet, HashSet};

use chrono::{DateTime, Utc};
use sea_orm::{ConnectionTrait, DatabaseConnection, DbErr, FromQueryResult, TransactionTrait};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use uuid::Uuid;

use crate::{
    db,
    store::{self, Event, MembershipRow, OpContext, MEMBERSHIP_COLUMNS},
    validation::{valid_legacy_slug, ProfileValues},
};

pub const SOURCE: &str = "cfmeet.meeting-service";
pub const SCHEMA_VERSION: i64 = 1;

fn empty_object() -> Value {
    json!({})
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Export {
    pub schema_version: i64,
    pub source: String,
    pub exported_at: DateTime<Utc>,
    pub organizations: Vec<ExportOrganization>,
    pub memberships: Vec<ExportMembership>,
    pub counts: ExportCounts,
    pub checksum: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExportCounts {
    pub organizations: u64,
    pub memberships: u64,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExportOrganization {
    pub id: Uuid,
    pub name: String,
    pub slug: String,
    pub status: String,
    #[serde(default)]
    pub created_by: Option<Uuid>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    #[serde(default = "empty_object")]
    pub profile_policy: Value,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExportMembership {
    pub id: Uuid,
    pub organization_id: Uuid,
    pub user_id: Uuid,
    pub role: String,
    pub status: String,
    #[serde(default)]
    pub created_by: Option<Uuid>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    #[serde(default)]
    pub removed_at: Option<DateTime<Utc>>,
}

// ---------------------------------------------------------------------------------------
// Checksum (both sides compute it identically)
// ---------------------------------------------------------------------------------------

/// One checksummed organization.
pub struct OrgLine<'a> {
    pub id: Uuid,
    pub name: &'a str,
    pub slug: &'a str,
    pub status: &'a str,
    pub created_by: Option<Uuid>,
    pub created_at: DateTime<Utc>,
}

/// One checksummed membership.
pub struct MemLine<'a> {
    pub id: Uuid,
    pub organization_id: Uuid,
    pub user_id: Uuid,
    pub role: &'a str,
    pub status: &'a str,
    pub created_at: DateTime<Utc>,
}

/// SHA-256 (lower-case hex) over UTF-8 lines, each followed by `\n`:
/// `org|{id}|{sha256hex(name)}|{slug}|{status}|{createdBy or -}|{createdAt unix µs}` sorted
/// by id, then `mem|{id}|{organizationId}|{userId}|{role}|{status}|{createdAt unix µs}`
/// sorted by id. Ids are lower-case hyphenated UUIDs.
pub fn checksum(orgs: &[OrgLine<'_>], mems: &[MemLine<'_>]) -> String {
    let mut org_lines: Vec<(Uuid, String)> = orgs
        .iter()
        .map(|o| {
            (
                o.id,
                format!(
                    "org|{}|{}|{}|{}|{}|{}\n",
                    o.id,
                    hex::encode(Sha256::digest(o.name.as_bytes())),
                    o.slug,
                    o.status,
                    o.created_by
                        .map(|u| u.to_string())
                        .unwrap_or_else(|| "-".into()),
                    o.created_at.timestamp_micros()
                ),
            )
        })
        .collect();
    let mut mem_lines: Vec<(Uuid, String)> = mems
        .iter()
        .map(|m| {
            (
                m.id,
                format!(
                    "mem|{}|{}|{}|{}|{}|{}\n",
                    m.id,
                    m.organization_id,
                    m.user_id,
                    m.role,
                    m.status,
                    m.created_at.timestamp_micros()
                ),
            )
        })
        .collect();
    // Uuid ordering is byte ordering = ordering of the lower-case hyphenated string.
    org_lines.sort_by_key(|l| l.0);
    mem_lines.sort_by_key(|l| l.0);
    let mut hasher = Sha256::new();
    for (_, line) in org_lines.iter().chain(mem_lines.iter()) {
        hasher.update(line.as_bytes());
    }
    hex::encode(hasher.finalize())
}

impl Export {
    pub fn computed_checksum(&self) -> String {
        let orgs: Vec<OrgLine<'_>> = self
            .organizations
            .iter()
            .map(|o| OrgLine {
                id: o.id,
                name: &o.name,
                slug: &o.slug,
                status: &o.status,
                created_by: o.created_by,
                created_at: o.created_at,
            })
            .collect();
        let mems: Vec<MemLine<'_>> = self
            .memberships
            .iter()
            .map(|m| MemLine {
                id: m.id,
                organization_id: m.organization_id,
                user_id: m.user_id,
                role: &m.role,
                status: &m.status,
                created_at: m.created_at,
            })
            .collect();
        checksum(&orgs, &mems)
    }

    /// Document-level validation (no database). Messages carry counts, never values.
    pub fn validate(&self) -> Result<(), String> {
        if self.schema_version != SCHEMA_VERSION {
            return Err("unsupported schemaVersion (expected 1)".into());
        }
        if self.source != SOURCE {
            return Err("unsupported source (expected cfmeet.meeting-service)".into());
        }
        if self.counts.organizations != self.organizations.len() as u64
            || self.counts.memberships != self.memberships.len() as u64
        {
            return Err("counts do not match the exported rows".into());
        }
        let expected = self.checksum.trim().to_ascii_lowercase();
        if expected.len() != 64 || !expected.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Err("checksum must be a lower-case hex SHA-256".into());
        }
        if self.computed_checksum() != expected {
            return Err("checksum mismatch".into());
        }
        let mut ids = HashSet::new();
        let mut slugs = HashSet::new();
        let mut bad = 0usize;
        for o in &self.organizations {
            let trimmed = o.name.trim();
            if !ids.insert(o.id) || !slugs.insert(o.slug.as_str()) {
                return Err("duplicate organization id or slug".into());
            }
            if trimmed.is_empty()
                || o.name.chars().count() > 200
                || !valid_legacy_slug(&o.slug)
                || !matches!(o.status.as_str(), "ACTIVE" | "SUSPENDED")
                || !o.profile_policy.is_object()
            {
                bad += 1;
            }
        }
        if bad > 0 {
            return Err(format!("{bad} organization(s) have invalid values"));
        }
        let mut mids = HashSet::new();
        let mut current = HashSet::new();
        for m in &self.memberships {
            if !mids.insert(m.id) {
                return Err("duplicate membership id".into());
            }
            if !matches!(m.role.as_str(), "OWNER" | "ADMIN" | "MEMBER")
                || !matches!(m.status.as_str(), "ACTIVE" | "SUSPENDED" | "REMOVED")
                || (m.status == "REMOVED" && m.removed_at.is_none())
            {
                bad += 1;
            }
            if m.status != "REMOVED" && !current.insert((m.organization_id, m.user_id)) {
                return Err("two non-removed memberships of one organization and user".into());
            }
        }
        if bad > 0 {
            return Err(format!("{bad} membership(s) have invalid values"));
        }
        Ok(())
    }
}

// ---------------------------------------------------------------------------------------
// Import
// ---------------------------------------------------------------------------------------

#[derive(Debug, thiserror::Error)]
pub enum ImportError {
    #[error("export refused: {0}")]
    Invalid(String),
    #[error("database failure: {}", crate::db::summarize_db_error(.0))]
    Db(#[from] DbErr),
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct Conflict {
    /// `ORGANIZATION_DIFFERS`, `MEMBERSHIP_DIFFERS`, `SLUG_TAKEN`, `MEMBERSHIP_DUPLICATE`
    /// (another non-removed membership of the same organization + user exists) or
    /// `ORGANIZATION_NOT_IMPORTED` (its organization was itself a conflict).
    pub kind: &'static str,
    pub entity: &'static str,
    pub id: Uuid,
    pub fields: Vec<&'static str>,
}

#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Tally {
    pub imported: u64,
    pub unchanged: u64,
    pub conflicts: u64,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ImportReport {
    pub run_id: Uuid,
    pub imported: u64,
    pub unchanged: u64,
    pub conflicts: Vec<Conflict>,
    pub checksum: String,
    pub organizations: Tally,
    pub memberships: Tally,
}

#[derive(Debug, FromQueryResult)]
struct StoredOrg {
    id: Uuid,
    name: String,
    slug: String,
    lifecycle_status: String,
    creation_source: String,
    created_by: Option<Uuid>,
    created_at: DateTime<Utc>,
    updated_at: DateTime<Utc>,
    profile_policy: Value,
}

fn org_diff(stored: &StoredOrg, o: &ExportOrganization) -> Vec<&'static str> {
    let mut f = Vec::new();
    if stored.creation_source != "IMPORTED" {
        f.push("creationSource");
    }
    if stored.name != o.name {
        f.push("name");
    }
    if stored.slug != o.slug {
        f.push("slug");
    }
    if stored.lifecycle_status != o.status {
        f.push("status");
    }
    if stored.created_by != o.created_by {
        f.push("createdBy");
    }
    if stored.created_at.timestamp_micros() != o.created_at.timestamp_micros() {
        f.push("createdAt");
    }
    if stored.updated_at.timestamp_micros() != o.updated_at.timestamp_micros() {
        f.push("updatedAt");
    }
    if stored.profile_policy != o.profile_policy {
        f.push("profilePolicy");
    }
    f
}

fn micros(t: Option<DateTime<Utc>>) -> Option<i64> {
    t.map(|t| t.timestamp_micros())
}

fn membership_diff(stored: &MembershipRow, m: &ExportMembership) -> Vec<&'static str> {
    let mut f = Vec::new();
    if stored.joined_via != "IMPORTED" {
        f.push("joinedVia");
    }
    if stored.organization_id != m.organization_id {
        f.push("organizationId");
    }
    if stored.user_id != m.user_id {
        f.push("userId");
    }
    if stored.role != m.role {
        f.push("role");
    }
    if stored.status != m.status {
        f.push("status");
    }
    if stored.created_by != m.created_by {
        f.push("createdBy");
    }
    if stored.created_at.timestamp_micros() != m.created_at.timestamp_micros() {
        f.push("createdAt");
    }
    if stored.updated_at.timestamp_micros() != m.updated_at.timestamp_micros() {
        f.push("updatedAt");
    }
    if micros(stored.removed_at) != micros(m.removed_at) {
        f.push("removedAt");
    }
    f
}

/// Imports `export` (validated here) in one transaction.
pub async fn import(
    db: &DatabaseConnection,
    ctx: &OpContext,
    export: &Export,
) -> Result<ImportReport, ImportError> {
    export.validate().map_err(ImportError::Invalid)?;
    let txn = db.begin().await?;
    db::exec(
        &txn,
        "SELECT pg_advisory_xact_lock(hashtextextended('organizations-import', 0))",
        vec![],
    )
    .await?;
    let now = db::now(&txn).await?;

    // Memberships whose organization is neither exported nor already stored are refused.
    let exported: BTreeSet<Uuid> = export.organizations.iter().map(|o| o.id).collect();
    let foreign: BTreeSet<Uuid> = export
        .memberships
        .iter()
        .map(|m| m.organization_id)
        .filter(|id| !exported.contains(id))
        .collect();
    let mut orphans = 0usize;
    for id in &foreign {
        if store::org(&txn, *id, false).await?.is_none() {
            orphans += export
                .memberships
                .iter()
                .filter(|m| m.organization_id == *id)
                .count();
        }
    }
    if orphans > 0 {
        return Err(ImportError::Invalid(format!(
            "{orphans} membership(s) reference an organization that is absent"
        )));
    }

    let mut conflicts = Vec::new();
    let mut orgs = Tally::default();
    let mut mems = Tally::default();
    let mut not_imported: BTreeSet<Uuid> = BTreeSet::new();
    let mut sorted_orgs: Vec<&ExportOrganization> = export.organizations.iter().collect();
    sorted_orgs.sort_by_key(|o| o.id);
    for o in sorted_orgs {
        let stored: Option<StoredOrg> = db::one(
            &txn,
            "SELECT id, name, slug, lifecycle_status, creation_source, created_by, created_at, updated_at, profile_policy FROM organizations WHERE id = $1 FOR UPDATE",
            vec![o.id.into()],
        )
        .await?;
        if let Some(stored) = stored {
            let fields = org_diff(&stored, o);
            if fields.is_empty() {
                orgs.unchanged += 1;
            } else {
                orgs.conflicts += 1;
                conflicts.push(Conflict {
                    kind: "ORGANIZATION_DIFFERS",
                    entity: "organization",
                    id: stored.id,
                    fields,
                });
            }
            continue;
        }
        let slug_owner = db::scalar_i64(
            &txn,
            "SELECT count(*)::bigint AS n FROM organizations WHERE slug = $1",
            vec![o.slug.clone().into()],
        )
        .await?;
        if slug_owner > 0 {
            orgs.conflicts += 1;
            not_imported.insert(o.id);
            conflicts.push(Conflict {
                kind: "SLUG_TAKEN",
                entity: "organization",
                id: o.id,
                fields: vec!["slug"],
            });
            continue;
        }
        db::exec(
            &txn,
            "INSERT INTO organizations (id, name, slug, lifecycle_status, lifecycle_reason, provisioning_state, creation_source, created_by, created_at, updated_at, version, profile_policy) \
             VALUES ($1, $2, $3, $4, $5, 'PENDING', 'IMPORTED', $6, $7, $8, 1, $9)",
            vec![
                o.id.into(),
                o.name.clone().into(),
                o.slug.clone().into(),
                o.status.clone().into(),
                (if o.status == "SUSPENDED" { Some("IMPORTED") } else { None }).into(),
                o.created_by.into(),
                o.created_at.into(),
                o.updated_at.into(),
                o.profile_policy.clone().into(),
            ],
        )
        .await?;
        store::save_profile(&txn, o.id, &ProfileValues::default(), now).await?;
        store::emit(
            &txn,
            ctx,
            now,
            Event {
                organization_id: o.id,
                aggregate_type: "organization",
                aggregate_id: o.id,
                aggregate_version: 1,
                event_type: "organization.created",
                payload: json!({ "slug": o.slug, "lifecycleStatus": o.status, "provisioningState": "PENDING", "creationSource": "IMPORTED" }),
            },
        )
        .await?;
        orgs.imported += 1;
    }

    let mut sorted_mems: Vec<&ExportMembership> = export.memberships.iter().collect();
    sorted_mems.sort_by_key(|m| m.id);
    for m in sorted_mems {
        let stored: Option<MembershipRow> = db::one(
            &txn,
            &format!("SELECT {MEMBERSHIP_COLUMNS} FROM organization_memberships WHERE id = $1 FOR UPDATE"),
            vec![m.id.into()],
        )
        .await?;
        if let Some(stored) = stored {
            let fields = membership_diff(&stored, m);
            if fields.is_empty() {
                mems.unchanged += 1;
            } else {
                mems.conflicts += 1;
                conflicts.push(Conflict {
                    kind: "MEMBERSHIP_DIFFERS",
                    entity: "membership",
                    id: m.id,
                    fields,
                });
            }
            continue;
        }
        if not_imported.contains(&m.organization_id) {
            mems.conflicts += 1;
            conflicts.push(Conflict {
                kind: "ORGANIZATION_NOT_IMPORTED",
                entity: "membership",
                id: m.id,
                fields: vec!["organizationId"],
            });
            continue;
        }
        if m.status != "REMOVED"
            && store::current_membership(&txn, m.organization_id, m.user_id, false)
                .await?
                .is_some()
        {
            mems.conflicts += 1;
            conflicts.push(Conflict {
                kind: "MEMBERSHIP_DUPLICATE",
                entity: "membership",
                id: m.id,
                fields: vec!["userId"],
            });
            continue;
        }
        let row: MembershipRow = db::one(
            &txn,
            &format!(
                "INSERT INTO organization_memberships (id, organization_id, user_id, role, status, joined_via, created_by, created_at, updated_at, removed_at, version) \
                 VALUES ($1, $2, $3, $4, $5, 'IMPORTED', $6, $7, $8, $9, 1) RETURNING {MEMBERSHIP_COLUMNS}"
            ),
            vec![
                m.id.into(),
                m.organization_id.into(),
                m.user_id.into(),
                m.role.clone().into(),
                m.status.clone().into(),
                m.created_by.into(),
                m.created_at.into(),
                m.updated_at.into(),
                m.removed_at.into(),
            ],
        )
        .await?
        .ok_or_else(|| DbErr::Custom("membership insert returned no row".into()))?;
        store::history(&txn, ctx, now, None, &row, "IMPORTED").await?;
        if row.status == "ACTIVE" {
            store::emit_membership_activated(&txn, ctx, now, &row).await?;
        }
        mems.imported += 1;
    }

    let run_id = Uuid::new_v4();
    let checksum = export.checksum.trim().to_ascii_lowercase();
    let imported = orgs.imported + mems.imported;
    let unchanged = orgs.unchanged + mems.unchanged;
    db::exec(
        &txn,
        "INSERT INTO organization_import_runs (id, source, exported_at, checksum, organizations_count, memberships_count, imported, unchanged, conflicts, actor_kind, actor_service, created_at) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12)",
        vec![
            run_id.into(),
            export.source.clone().into(),
            export.exported_at.into(),
            checksum.clone().into(),
            (export.organizations.len() as i32).into(),
            (export.memberships.len() as i32).into(),
            (imported as i32).into(),
            (unchanged as i32).into(),
            serde_json::to_value(&conflicts).unwrap_or_else(|_| json!([])).into(),
            ctx.kind.as_str().into(),
            ctx.service.clone().into(),
            now.into(),
        ],
    )
    .await?;
    store::audit(
        &txn,
        ctx,
        now,
        None,
        "migration.imported",
        "import_run",
        Some(run_id),
        json!({
            "checksum": checksum,
            "imported": imported,
            "unchanged": unchanged,
            "conflicts": conflicts.len(),
            "organizations": orgs.imported,
            "memberships": mems.imported,
        }),
    )
    .await?;
    txn.commit().await?;
    Ok(ImportReport {
        run_id,
        imported,
        unchanged,
        conflicts,
        checksum,
        organizations: orgs,
        memberships: mems,
    })
}

// ---------------------------------------------------------------------------------------
// Reconciliation
// ---------------------------------------------------------------------------------------

#[derive(Debug, FromQueryResult)]
struct CheckOrg {
    id: Uuid,
    name: String,
    slug: String,
    lifecycle_status: String,
    created_by: Option<Uuid>,
    created_at: DateTime<Utc>,
}

#[derive(Debug, FromQueryResult)]
struct CheckMem {
    id: Uuid,
    organization_id: Uuid,
    user_id: Uuid,
    role: String,
    status: String,
    created_at: DateTime<Utc>,
}

#[derive(Debug, FromQueryResult)]
struct LastRun {
    id: Uuid,
    exported_at: DateTime<Utc>,
    checksum: String,
    organizations_count: i32,
    memberships_count: i32,
    imported: i32,
    unchanged: i32,
    conflicts: Value,
    created_at: DateTime<Utc>,
}

/// Counts, the checksum of the imported rows in their CURRENT state (same algorithm as
/// the export, so it is directly comparable with a fresh Meeting Service export), the last
/// import and the mismatches between them.
pub async fn reconciliation<C: ConnectionTrait>(c: &C) -> Result<Value, DbErr> {
    let orgs: Vec<CheckOrg> = db::all(
        c,
        "SELECT id, name, slug, lifecycle_status, created_by, created_at FROM organizations WHERE creation_source = 'IMPORTED'",
        vec![],
    )
    .await?;
    let mems: Vec<CheckMem> = db::all(
        c,
        "SELECT id, organization_id, user_id, role, status, created_at FROM organization_memberships WHERE joined_via = 'IMPORTED'",
        vec![],
    )
    .await?;
    let current = checksum(
        &orgs
            .iter()
            .map(|o| OrgLine {
                id: o.id,
                name: &o.name,
                slug: &o.slug,
                status: &o.lifecycle_status,
                created_by: o.created_by,
                created_at: o.created_at,
            })
            .collect::<Vec<_>>(),
        &mems
            .iter()
            .map(|m| MemLine {
                id: m.id,
                organization_id: m.organization_id,
                user_id: m.user_id,
                role: &m.role,
                status: &m.status,
                created_at: m.created_at,
            })
            .collect::<Vec<_>>(),
    );
    let total_orgs =
        db::scalar_i64(c, "SELECT count(*)::bigint AS n FROM organizations", vec![]).await?;
    let total_mems = db::scalar_i64(
        c,
        "SELECT count(*)::bigint AS n FROM organization_memberships",
        vec![],
    )
    .await?;
    let mut by_status: BTreeMap<String, u64> = BTreeMap::new();
    for m in &mems {
        *by_status.entry(m.status.clone()).or_default() += 1;
    }
    let last: Option<LastRun> = db::one(
        c,
        "SELECT id, exported_at, checksum, organizations_count, memberships_count, imported, unchanged, conflicts, created_at FROM organization_import_runs ORDER BY created_at DESC, id DESC LIMIT 1",
        vec![],
    )
    .await?;
    let mut mismatches = Vec::new();
    if let Some(run) = &last {
        if run.checksum.trim() != current {
            mismatches.push(json!({ "kind": "CHECKSUM_MISMATCH", "expected": run.checksum.trim(), "actual": current }));
        }
        if run.organizations_count as usize != orgs.len() {
            mismatches.push(json!({ "kind": "ORGANIZATION_COUNT_MISMATCH", "expected": run.organizations_count, "actual": orgs.len() }));
        }
        if run.memberships_count as usize != mems.len() {
            mismatches.push(json!({ "kind": "MEMBERSHIP_COUNT_MISMATCH", "expected": run.memberships_count, "actual": mems.len() }));
        }
        let n = run.conflicts.as_array().map(|a| a.len()).unwrap_or(0);
        if n > 0 {
            mismatches.push(json!({ "kind": "IMPORT_CONFLICTS", "count": n }));
        }
    }
    Ok(json!({
        "organizations": { "total": total_orgs, "imported": orgs.len() },
        "memberships": { "total": total_mems, "imported": mems.len(), "importedByStatus": by_status },
        "checksum": current,
        "lastImport": last.as_ref().map(|r| json!({
            "runId": r.id,
            "importedAt": store::rfc3339(r.created_at),
            "exportedAt": store::rfc3339(r.exported_at),
            "checksum": r.checksum.trim(),
            "counts": { "organizations": r.organizations_count, "memberships": r.memberships_count },
            "imported": r.imported,
            "unchanged": r.unchanged,
            "conflicts": r.conflicts,
        })),
        "mismatches": mismatches,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The shared Meeting Service ↔ Organizations test vector (contract §5).
    pub const VECTOR_CHECKSUM: &str =
        "1a212f81add323e1e9bed715699c0ba8c97b2dc22e211c84a05bca42d0908e78";

    pub fn vector() -> Value {
        json!({
            "schemaVersion": 1,
            "source": "cfmeet.meeting-service",
            "exportedAt": "2026-11-06T00:00:00Z",
            "organizations": [{
                "id": "00000000-0000-4000-8000-000000000001", "name": "Acme Health", "slug": "acme-health",
                "status": "ACTIVE", "createdBy": null, "createdAt": "2026-10-04T12:00:00Z",
                "updatedAt": "2026-10-04T12:00:00Z", "profilePolicy": {}
            }],
            "memberships": [{
                "id": "00000000-0000-4000-8000-0000000000b1", "organizationId": "00000000-0000-4000-8000-000000000001",
                "userId": "00000000-0000-4000-8000-0000000000a1", "role": "OWNER", "status": "ACTIVE",
                "createdBy": null, "createdAt": "2026-10-04T12:00:00Z", "updatedAt": "2026-10-04T12:00:00Z", "removedAt": null
            }],
            "counts": { "organizations": 1, "memberships": 1 },
            "checksum": VECTOR_CHECKSUM
        })
    }

    #[test]
    fn the_shared_checksum_vector_matches() {
        let export: Export = serde_json::from_value(vector()).unwrap();
        assert_eq!(
            export.organizations[0].created_at.timestamp_micros(),
            1_791_115_200_000_000
        );
        assert_eq!(export.computed_checksum(), VECTOR_CHECKSUM);
        export.validate().unwrap();
    }

    #[test]
    fn checksum_is_order_independent_and_covers_every_line_field() {
        let mut doc = vector();
        let base: Export = serde_json::from_value(doc.clone()).unwrap();
        doc["organizations"][0]["name"] = json!("Acme Health ");
        let changed: Export = serde_json::from_value(doc).unwrap();
        assert_ne!(base.computed_checksum(), changed.computed_checksum());
        assert!(changed.validate().unwrap_err().contains("checksum"));
        // Fields outside the lines (updatedAt, profilePolicy) do not change the checksum.
        let mut doc = vector();
        doc["organizations"][0]["updatedAt"] = json!("2026-11-01T00:00:00Z");
        let same: Export = serde_json::from_value(doc).unwrap();
        assert_eq!(same.computed_checksum(), VECTOR_CHECKSUM);
    }

    #[test]
    #[allow(clippy::type_complexity)]
    fn invalid_exports_are_refused() {
        let cases: Vec<(&str, Box<dyn Fn(&mut Value)>)> = vec![
            ("schemaVersion", Box::new(|d| d["schemaVersion"] = json!(2))),
            (
                "source",
                Box::new(|d| d["source"] = json!("cfmeet.calling")),
            ),
            (
                "counts",
                Box::new(|d| d["counts"]["memberships"] = json!(2)),
            ),
            (
                "checksum",
                Box::new(|d| d["checksum"] = json!("0".repeat(64))),
            ),
        ];
        for (what, mutate) in cases {
            let mut doc = vector();
            mutate(&mut doc);
            let export: Export = serde_json::from_value(doc).unwrap();
            assert!(export.validate().is_err(), "{what}");
        }
    }
}
