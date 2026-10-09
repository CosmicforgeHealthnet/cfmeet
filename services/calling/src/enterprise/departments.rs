//! Departments (Part F §53, §145): administrative and routing groups of principals of
//! the same tenant / application / environment. A department is NOT an authorization
//! system — membership grants no scope and no capability; it is organizational data that
//! routing and directories may use.

use chrono::{DateTime, Utc};
use sea_orm::{ConnectionTrait, FromQueryResult};
use serde::Serialize;
use uuid::Uuid;

use super::{
    audit::{self, AuditDetails, AuditFact, AuditOperation, AuditResource},
    check_version, clean_name, exec,
    policy::ResourceStatus,
    scope_values, stmt, time, AdminActor, EnterpriseError, EnterpriseResult, EnterpriseService,
    PageRequest,
};
use crate::calls::command::CallScope;

text_enum!(
    /// A member's role in a department (organizational only; grants nothing).
    DepartmentRole {
        Member = "MEMBER",
        Manager = "MANAGER",
    }
);

/// Maximum members of one department (bounded replacement payloads).
pub const MAX_DEPARTMENT_MEMBERS: usize = 500;

#[derive(Debug, Clone, FromQueryResult)]
struct DepartmentRow {
    id: Uuid,
    name: String,
    description: Option<String>,
    status: String,
    version: i64,
    created_at: DateTime<Utc>,
    updated_at: DateTime<Utc>,
    disabled_at: Option<DateTime<Utc>>,
}

const COLUMNS: &str = "id, name, description, status, version, created_at, updated_at, disabled_at";

#[derive(Debug, Clone, FromQueryResult)]
struct MemberRow {
    principal_id: Uuid,
    role: String,
    added_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DepartmentMemberView {
    pub principal_id: Uuid,
    pub role: String,
    pub added_at: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DepartmentView {
    pub department_id: Uuid,
    pub name: String,
    pub description: Option<String>,
    pub status: String,
    pub member_count: usize,
    pub members: Vec<DepartmentMemberView>,
    pub version: i64,
    pub created_at: String,
    pub updated_at: String,
    pub disabled_at: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DepartmentPage {
    pub departments: Vec<DepartmentView>,
    pub next_cursor: Option<String>,
}

#[derive(Debug, Clone)]
pub struct NewDepartment {
    pub name: String,
    pub description: Option<String>,
}

#[derive(Debug, Clone, Default)]
pub struct DepartmentChange {
    pub name: Option<String>,
    /// `Some(None)` clears the description.
    pub description: Option<Option<String>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DepartmentMember {
    pub principal_id: Uuid,
    pub role: DepartmentRole,
}

fn description(value: Option<&str>) -> EnterpriseResult<Option<String>> {
    value
        .map(|d| clean_name(d, 500, "description must be 1-500 characters."))
        .transpose()
}

async fn department_row<C: ConnectionTrait>(
    db: &C,
    scope: CallScope,
    id: Uuid,
    lock: bool,
) -> EnterpriseResult<DepartmentRow> {
    let mut values = scope_values(scope);
    values.push(id.into());
    DepartmentRow::find_by_statement(stmt(
        &format!(
            "SELECT {COLUMNS} FROM enterprise_departments \
             WHERE tenant_id = $1 AND application_id = $2 AND environment_id = $3 AND id = $4{}",
            if lock { " FOR UPDATE" } else { "" }
        ),
        values,
    ))
    .one(db)
    .await?
    .ok_or(EnterpriseError::NotFound)
}

async fn view<C: ConnectionTrait>(db: &C, row: DepartmentRow) -> EnterpriseResult<DepartmentView> {
    let members = MemberRow::find_by_statement(stmt(
        "SELECT principal_id, role, added_at FROM enterprise_department_members \
         WHERE department_id = $1 ORDER BY added_at, principal_id",
        vec![row.id.into()],
    ))
    .all(db)
    .await?;
    Ok(DepartmentView {
        department_id: row.id,
        name: row.name,
        description: row.description,
        status: row.status,
        member_count: members.len(),
        members: members
            .into_iter()
            .map(|m| DepartmentMemberView {
                principal_id: m.principal_id,
                role: m.role,
                added_at: time(m.added_at),
            })
            .collect(),
        version: row.version,
        created_at: time(row.created_at),
        updated_at: time(row.updated_at),
        disabled_at: row.disabled_at.map(time),
    })
}

/// Whether `principal_id` is an ACTIVE principal of `scope`.
pub(crate) async fn active_principal<C: ConnectionTrait>(
    db: &C,
    scope: CallScope,
    principal_id: Uuid,
) -> EnterpriseResult<bool> {
    let mut values = scope_values(scope);
    values.push(principal_id.into());
    Ok(db
        .query_one(stmt(
            "SELECT 1 FROM platform_principals WHERE tenant_id = $1 AND application_id = $2 \
             AND environment_id = $3 AND id = $4 AND status = 'ACTIVE'",
            values,
        ))
        .await?
        .is_some())
}

impl EnterpriseService {
    pub async fn create_department(
        &self,
        actor: &AdminActor,
        new: NewDepartment,
    ) -> EnterpriseResult<DepartmentView> {
        let name = clean_name(&new.name, 100, "name must be 1-100 characters.")?;
        let description = description(new.description.as_deref())?;
        let txn = self.begin().await?;
        let id = Uuid::new_v4();
        let now = self.now();
        let mut values = vec![id.into()];
        values.extend(scope_values(actor.scope));
        values.extend([name.into(), description.into(), now.into()]);
        exec(
            &txn,
            "INSERT INTO enterprise_departments (id, tenant_id, application_id, environment_id, \
             name, description, status, version, created_at, updated_at) \
             VALUES ($1, $2, $3, $4, $5, $6, 'ACTIVE', 1, $7, $7)",
            values,
        )
        .await?;
        audit::record(
            &txn,
            actor,
            AuditFact {
                resource: AuditResource::Department,
                resource_id: id,
                operation: AuditOperation::Create,
                old_version: None,
                new_version: Some(1),
                details: AuditDetails::new(),
            },
            now,
        )
        .await?;
        let row = department_row(&txn, actor.scope, id, false).await?;
        let out = view(&txn, row).await?;
        txn.commit().await?;
        Ok(out)
    }

    pub async fn get_department(
        &self,
        scope: CallScope,
        id: Uuid,
    ) -> EnterpriseResult<DepartmentView> {
        let row = department_row(self.db(), scope, id, false).await?;
        view(self.db(), row).await
    }

    pub async fn list_departments(
        &self,
        scope: CallScope,
        status: Option<ResourceStatus>,
        page: &PageRequest,
    ) -> EnterpriseResult<DepartmentPage> {
        let limit = page.limit()?;
        let mut values = scope_values(scope);
        let mut sql = format!(
            "SELECT {COLUMNS} FROM enterprise_departments \
             WHERE tenant_id = $1 AND application_id = $2 AND environment_id = $3"
        );
        if let Some(status) = status {
            values.push(status.as_str().into());
            sql.push_str(&format!(" AND status = ${}", values.len()));
        }
        if let Some((at, id)) = page.after()? {
            values.push(at.into());
            values.push(id.into());
            sql.push_str(&format!(
                " AND (created_at, id) > (${}, ${})",
                values.len() - 1,
                values.len()
            ));
        }
        sql.push_str(&format!(" ORDER BY created_at, id LIMIT {}", limit + 1));
        let mut rows = DepartmentRow::find_by_statement(stmt(&sql, values))
            .all(self.db())
            .await?;
        let next_cursor = if rows.len() as u64 > limit {
            rows.truncate(limit as usize);
            rows.last()
                .map(|r| super::cursor::encode(r.created_at, r.id))
        } else {
            None
        };
        let mut departments = Vec::with_capacity(rows.len());
        for row in rows {
            departments.push(view(self.db(), row).await?);
        }
        Ok(DepartmentPage {
            departments,
            next_cursor,
        })
    }

    pub async fn update_department(
        &self,
        actor: &AdminActor,
        id: Uuid,
        expected_version: i64,
        change: DepartmentChange,
    ) -> EnterpriseResult<DepartmentView> {
        let name = change
            .name
            .as_deref()
            .map(|n| clean_name(n, 100, "name must be 1-100 characters."))
            .transpose()?;
        let new_description = change
            .description
            .as_ref()
            .map(|d| description(d.as_deref()))
            .transpose()?;
        let txn = self.begin().await?;
        let row = department_row(&txn, actor.scope, id, true).await?;
        check_version(expected_version, row.version)?;
        if row.status != "ACTIVE" {
            return Err(EnterpriseError::Conflict(
                "a disabled department cannot be changed",
            ));
        }
        let now = self.now();
        exec(
            &txn,
            "UPDATE enterprise_departments SET name = $2, description = $3, version = version + 1, \
             updated_at = GREATEST(updated_at, $4) WHERE id = $1",
            vec![
                id.into(),
                name.unwrap_or(row.name.clone()).into(),
                new_description
                    .unwrap_or(row.description.clone())
                    .into(),
                now.into(),
            ],
        )
        .await?;
        audit::record(
            &txn,
            actor,
            AuditFact {
                resource: AuditResource::Department,
                resource_id: id,
                operation: AuditOperation::Update,
                old_version: Some(row.version),
                new_version: Some(row.version + 1),
                details: AuditDetails::new(),
            },
            now,
        )
        .await?;
        let row = department_row(&txn, actor.scope, id, false).await?;
        let out = view(&txn, row).await?;
        txn.commit().await?;
        Ok(out)
    }

    /// Replaces the whole member list atomically (bounded, distinct, ACTIVE principals of
    /// the same scope; a foreign principal is reported as not found).
    pub async fn replace_department_members(
        &self,
        actor: &AdminActor,
        id: Uuid,
        expected_version: i64,
        members: Vec<DepartmentMember>,
    ) -> EnterpriseResult<DepartmentView> {
        if members.len() > MAX_DEPARTMENT_MEMBERS {
            return Err(EnterpriseError::Validation(
                "a department has at most 500 members.",
            ));
        }
        let mut ids: Vec<Uuid> = members.iter().map(|m| m.principal_id).collect();
        ids.sort();
        ids.dedup();
        if ids.len() != members.len() {
            return Err(EnterpriseError::Validation("members must be distinct."));
        }
        let txn = self.begin().await?;
        let row = department_row(&txn, actor.scope, id, true).await?;
        check_version(expected_version, row.version)?;
        if row.status != "ACTIVE" {
            return Err(EnterpriseError::Conflict(
                "a disabled department cannot be changed",
            ));
        }
        for member in &members {
            if !active_principal(&txn, actor.scope, member.principal_id).await? {
                return Err(EnterpriseError::NotFound);
            }
        }
        let now = self.now();
        exec(
            &txn,
            "DELETE FROM enterprise_department_members WHERE department_id = $1",
            vec![id.into()],
        )
        .await?;
        for member in &members {
            let mut values = scope_values(actor.scope);
            values.extend([
                id.into(),
                member.principal_id.into(),
                member.role.as_str().into(),
                now.into(),
            ]);
            exec(
                &txn,
                "INSERT INTO enterprise_department_members (tenant_id, application_id, \
                 environment_id, department_id, principal_id, role, added_at) \
                 VALUES ($1, $2, $3, $4, $5, $6, $7)",
                values,
            )
            .await?;
        }
        exec(
            &txn,
            "UPDATE enterprise_departments SET version = version + 1, \
             updated_at = GREATEST(updated_at, $2) WHERE id = $1",
            vec![id.into(), now.into()],
        )
        .await?;
        audit::record(
            &txn,
            actor,
            AuditFact {
                resource: AuditResource::Department,
                resource_id: id,
                operation: AuditOperation::ReplaceMembers,
                old_version: Some(row.version),
                new_version: Some(row.version + 1),
                details: AuditDetails::new().with("member_count", members.len()),
            },
            now,
        )
        .await?;
        let row = department_row(&txn, actor.scope, id, false).await?;
        let out = view(&txn, row).await?;
        txn.commit().await?;
        Ok(out)
    }

    /// Disables a department (membership rows are kept as history).
    pub async fn disable_department(
        &self,
        actor: &AdminActor,
        id: Uuid,
        expected_version: i64,
    ) -> EnterpriseResult<DepartmentView> {
        let txn = self.begin().await?;
        let row = department_row(&txn, actor.scope, id, true).await?;
        check_version(expected_version, row.version)?;
        if row.status == "DISABLED" {
            return Err(EnterpriseError::Conflict(
                "the department is already disabled",
            ));
        }
        let now = self.now();
        exec(
            &txn,
            "UPDATE enterprise_departments SET status = 'DISABLED', disabled_at = $2, \
             version = version + 1, updated_at = GREATEST(updated_at, $2) WHERE id = $1",
            vec![id.into(), now.into()],
        )
        .await?;
        audit::record(
            &txn,
            actor,
            AuditFact {
                resource: AuditResource::Department,
                resource_id: id,
                operation: AuditOperation::Disable,
                old_version: Some(row.version),
                new_version: Some(row.version + 1),
                details: AuditDetails::new(),
            },
            now,
        )
        .await?;
        let row = department_row(&txn, actor.scope, id, false).await?;
        let out = view(&txn, row).await?;
        txn.commit().await?;
        Ok(out)
    }
}
