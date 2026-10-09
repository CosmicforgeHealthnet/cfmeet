//! Enterprise SIP devices (Part F §54, §179): binds an opaque Telephony device reference
//! (`dev-…`, minted by the operator where the device's edge endpoint lives) to a principal
//! of the scope. A dialog from the device then calls as that principal. Calling never sees
//! the device's SIP identity, address or credentials; Telephony never learns the principal.

use chrono::{DateTime, Utc};
use sea_orm::{ConnectionTrait, FromQueryResult};
use serde::Serialize;
use uuid::Uuid;

use super::{
    audit::{self, AuditDetails, AuditFact, AuditOperation, AuditResource},
    check_version, clean_name,
    departments::active_principal,
    exec,
    policy::{self, AdminCapability, ResourceStatus},
    scope_values, stmt, time, AdminActor, EnterpriseError, EnterpriseResult, EnterpriseService,
    PageRequest,
};
use crate::calls::command::CallScope;

#[derive(Debug, Clone, FromQueryResult)]
pub(crate) struct DeviceRow {
    pub id: Uuid,
    pub principal_id: Uuid,
    pub device_ref: String,
    pub display_label: String,
    pub status: String,
    pub version: i64,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub disabled_at: Option<DateTime<Utc>>,
}

const COLUMNS: &str =
    "id, principal_id, device_ref, display_label, status, version, created_at, updated_at, disabled_at";

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SipDeviceView {
    pub sip_device_id: Uuid,
    pub device_ref: String,
    pub principal_id: Uuid,
    pub display_label: String,
    pub status: String,
    pub version: i64,
    pub created_at: String,
    pub updated_at: String,
    pub disabled_at: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SipDevicePage {
    pub sip_devices: Vec<SipDeviceView>,
    pub next_cursor: Option<String>,
}

fn view(row: DeviceRow) -> SipDeviceView {
    SipDeviceView {
        sip_device_id: row.id,
        device_ref: row.device_ref,
        principal_id: row.principal_id,
        display_label: row.display_label,
        status: row.status,
        version: row.version,
        created_at: time(row.created_at),
        updated_at: time(row.updated_at),
        disabled_at: row.disabled_at.map(time),
    }
}

/// `dev-` + 4-60 lower-case letters, digits or dashes (mirrors the database CHECK).
pub fn valid_device_ref(r: &str) -> bool {
    r.len() >= 8
        && r.len() <= 64
        && r.starts_with("dev-")
        && r[4..]
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
}

async fn device_row<C: ConnectionTrait>(
    db: &C,
    scope: CallScope,
    id: Uuid,
    lock: bool,
) -> EnterpriseResult<DeviceRow> {
    let mut values = scope_values(scope);
    values.push(id.into());
    DeviceRow::find_by_statement(stmt(
        &format!(
            "SELECT {COLUMNS} FROM enterprise_sip_devices \
             WHERE tenant_id = $1 AND application_id = $2 AND environment_id = $3 AND id = $4{}",
            if lock { " FOR UPDATE" } else { "" }
        ),
        values,
    ))
    .one(db)
    .await?
    .ok_or(EnterpriseError::NotFound)
}

/// The ACTIVE binding of `device_ref` in `scope` (None: unknown here — including a device
/// bound in another scope).
pub(crate) async fn active_binding<C: ConnectionTrait>(
    db: &C,
    scope: CallScope,
    device_ref: &str,
) -> EnterpriseResult<Option<DeviceRow>> {
    let mut values = scope_values(scope);
    values.push(device_ref.to_string().into());
    Ok(DeviceRow::find_by_statement(stmt(
        &format!(
            "SELECT {COLUMNS} FROM enterprise_sip_devices \
             WHERE tenant_id = $1 AND application_id = $2 AND environment_id = $3 AND device_ref = $4 \
               AND status = 'ACTIVE'"
        ),
        values,
    ))
    .one(db)
    .await?)
}

impl EnterpriseService {
    pub async fn bind_sip_device(
        &self,
        actor: &AdminActor,
        device_ref: String,
        principal_id: Uuid,
        display_label: String,
    ) -> EnterpriseResult<SipDeviceView> {
        if !valid_device_ref(&device_ref) {
            return Err(EnterpriseError::Validation(
                "deviceRef must be dev- followed by 4-60 lower-case letters, digits or dashes.",
            ));
        }
        let label = clean_name(
            &display_label,
            100,
            "displayLabel must be 1-100 characters.",
        )?;
        let txn = self.begin().await?;
        policy::require_capability(&txn, actor.scope, AdminCapability::ManageRouting).await?;
        if !active_principal(&txn, actor.scope, principal_id).await? {
            return Err(EnterpriseError::NotFound);
        }
        let id = Uuid::new_v4();
        let now = self.now();
        let mut values = vec![id.into()];
        values.extend(scope_values(actor.scope));
        values.extend([
            principal_id.into(),
            device_ref.into(),
            label.into(),
            now.into(),
        ]);
        exec(
            &txn,
            "INSERT INTO enterprise_sip_devices (id, tenant_id, application_id, environment_id, \
             principal_id, device_ref, display_label, status, version, created_at, updated_at) \
             VALUES ($1, $2, $3, $4, $5, $6, $7, 'ACTIVE', 1, $8, $8)",
            values,
        )
        .await?;
        audit::record(
            &txn,
            actor,
            AuditFact {
                resource: AuditResource::SipDevice,
                resource_id: id,
                operation: AuditOperation::Create,
                old_version: None,
                new_version: Some(1),
                details: AuditDetails::new(),
            },
            now,
        )
        .await?;
        let out = view(device_row(&txn, actor.scope, id, false).await?);
        txn.commit().await?;
        Ok(out)
    }

    pub async fn get_sip_device(
        &self,
        scope: CallScope,
        id: Uuid,
    ) -> EnterpriseResult<SipDeviceView> {
        Ok(view(device_row(self.db(), scope, id, false).await?))
    }

    pub async fn list_sip_devices(
        &self,
        scope: CallScope,
        status: Option<ResourceStatus>,
        page: &PageRequest,
    ) -> EnterpriseResult<SipDevicePage> {
        let limit = page.limit()?;
        let mut values = scope_values(scope);
        let mut sql = format!(
            "SELECT {COLUMNS} FROM enterprise_sip_devices \
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
        let mut rows = DeviceRow::find_by_statement(stmt(&sql, values))
            .all(self.db())
            .await?;
        let next_cursor = if rows.len() as u64 > limit {
            rows.truncate(limit as usize);
            rows.last()
                .map(|r| super::cursor::encode(r.created_at, r.id))
        } else {
            None
        };
        Ok(SipDevicePage {
            sip_devices: rows.into_iter().map(view).collect(),
            next_cursor,
        })
    }

    /// Unbinds a device (its dialogs are refused from now on). Never blocked.
    pub async fn disable_sip_device(
        &self,
        actor: &AdminActor,
        id: Uuid,
        expected_version: i64,
    ) -> EnterpriseResult<SipDeviceView> {
        let txn = self.begin().await?;
        let row = device_row(&txn, actor.scope, id, true).await?;
        check_version(expected_version, row.version)?;
        if row.status == "DISABLED" {
            return Err(EnterpriseError::Conflict("the device is already disabled"));
        }
        let now = self.now();
        exec(
            &txn,
            "UPDATE enterprise_sip_devices SET status = 'DISABLED', disabled_at = $2, \
             version = version + 1, updated_at = GREATEST(updated_at, $2) WHERE id = $1",
            vec![id.into(), now.into()],
        )
        .await?;
        audit::record(
            &txn,
            actor,
            AuditFact {
                resource: AuditResource::SipDevice,
                resource_id: id,
                operation: AuditOperation::Disable,
                old_version: Some(row.version),
                new_version: Some(row.version + 1),
                details: AuditDetails::new(),
            },
            now,
        )
        .await?;
        let out = view(device_row(&txn, actor.scope, id, false).await?);
        txn.commit().await?;
        Ok(out)
    }
}
