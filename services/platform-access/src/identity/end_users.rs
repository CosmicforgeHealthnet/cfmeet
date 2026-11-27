//! Partner users (ADR 0062 §1; contract `docs/platform/build-internal-contracts.md` §2.1).
//!
//! A partner user is an EXTERNAL_APPLICATION_PRINCIPAL principal of one environment,
//! identified by the partner's opaque `externalId` (its external reference, never a
//! telephone number or SIP address), with a profile (display name, image URL) and a
//! session epoch (`tokens_not_before`) in `platform_end_user_profiles`.
//!
//! * Creation is idempotent on `externalId` and bounded by a per-environment cap of
//!   ACTIVE users (serialized per environment by a transaction-scoped advisory lock).
//! * Every mutation writes exactly one operator audit row in its own transaction (the
//!   actor is the forwarded credential's service account, `devplat.s-<id>`).
//! * DELETE is the existing principal tombstone (DEACTIVATED for ever, the external
//!   reference replaced, ids kept) plus erasure of the profile values.
//! * `revoke-sessions` advances the epoch: user tokens issued before it are revoked.

use chrono::{DateTime, Duration, DurationRound, Utc};
use sea_orm::{
    ConnectionTrait, DatabaseConnection, DatabaseTransaction, DbBackend, DbErr, FromQueryResult,
    Statement, TransactionTrait, Value,
};
use serde::Serialize;
use serde_json::json;
use uuid::Uuid;

use super::{
    operator::{self, AuditEntry, AuditTargetKind, OperatorAction, OperatorContext},
    principals::{validate_external_reference, NewPrincipal},
    user_tokens::MINTS_PER_HOUR,
    PlatformError, PlatformService,
};
use crate::domain::{EnvironmentId, EnvironmentKind, PrincipalId, PrincipalKind, PrincipalStatus};

pub const MAX_EXTERNAL_ID_LEN: usize = 128;
pub const MAX_DISPLAY_NAME_CHARS: usize = 64;
pub const MAX_IMAGE_URL_LEN: usize = 512;
pub const DEFAULT_PAGE: u64 = 50;
pub const MAX_PAGE: u64 = 200;

const KIND: &str = "EXTERNAL_APPLICATION_PRINCIPAL";

/// The environment a partner request acts in (always from the forwarded credential).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EndUserScope {
    pub tenant_id: Uuid,
    pub application_id: Uuid,
    pub environment_id: Uuid,
    pub kind: EnvironmentKind,
}

/// A principal with its partner profile.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EndUserRow {
    pub id: Uuid,
    pub tenant_id: Uuid,
    pub application_id: Uuid,
    pub environment_id: Uuid,
    pub kind: PrincipalKind,
    pub external_reference: String,
    pub status: PrincipalStatus,
    pub tombstoned: bool,
    pub display_name: Option<String>,
    pub image_url: Option<String>,
    pub tokens_not_before: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(FromQueryResult)]
struct Row {
    id: Uuid,
    tenant_id: Uuid,
    application_id: Uuid,
    environment_id: Uuid,
    kind: String,
    external_reference: String,
    status: String,
    tombstoned: bool,
    display_name: Option<String>,
    image_url: Option<String>,
    tokens_not_before: Option<DateTime<Utc>>,
    created_at: DateTime<Utc>,
    updated_at: DateTime<Utc>,
}

impl Row {
    fn typed(self) -> Result<EndUserRow, DbErr> {
        let corrupt = || DbErr::Custom("corrupt principal row".to_string());
        Ok(EndUserRow {
            id: self.id,
            tenant_id: self.tenant_id,
            application_id: self.application_id,
            environment_id: self.environment_id,
            kind: PrincipalKind::parse(&self.kind).ok_or_else(corrupt)?,
            external_reference: self.external_reference,
            status: PrincipalStatus::parse(&self.status).ok_or_else(corrupt)?,
            tombstoned: self.tombstoned,
            display_name: self.display_name,
            image_url: self.image_url,
            tokens_not_before: self.tokens_not_before,
            created_at: self.created_at,
            updated_at: self.updated_at,
        })
    }
}

const SELECT: &str = "SELECT p.id, p.tenant_id, p.application_id, p.environment_id, p.kind, \
        p.external_reference, p.status, (p.tombstoned_at IS NOT NULL) AS tombstoned, \
        f.display_name, f.image_url, f.tokens_not_before, p.created_at, \
        GREATEST(p.updated_at, COALESCE(f.updated_at, p.updated_at)) AS updated_at \
     FROM platform_principals p \
     LEFT JOIN platform_end_user_profiles f ON f.principal_id = p.id";

/// The partner user object of the Users API.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EndUserView {
    pub id: Uuid,
    pub external_id: String,
    pub display_name: Option<String>,
    pub image_url: Option<String>,
    pub status: PrincipalStatus,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

impl From<&EndUserRow> for EndUserView {
    fn from(r: &EndUserRow) -> Self {
        Self {
            id: r.id,
            external_id: r.external_reference.clone(),
            display_name: r.display_name.clone(),
            image_url: r.image_url.clone(),
            status: r.status,
            created_at: r.created_at,
            updated_at: r.updated_at,
        }
    }
}

/// Errors of partner-user operations.
#[derive(Debug, thiserror::Error)]
pub enum EndUserError {
    #[error(transparent)]
    Platform(#[from] PlatformError),
    /// The environment's cap of ACTIVE users is reached.
    #[error("environment partner-user limit reached")]
    LimitExceeded,
}

impl From<DbErr> for EndUserError {
    fn from(e: DbErr) -> Self {
        EndUserError::Platform(e.into())
    }
}

pub type EndUserResult<T> = Result<T, EndUserError>;

fn invalid(message: impl Into<String>) -> PlatformError {
    PlatformError::Validation(message.into())
}

/// `externalId`: 1–128 printable ASCII without spaces; never a telephone number, `tel:` /
/// `sip:` URI or the reserved tombstone prefix (`validate_external_reference`).
pub fn validate_external_id(value: &str) -> Result<String, PlatformError> {
    if value.is_empty() || value.len() > MAX_EXTERNAL_ID_LEN {
        return Err(invalid(format!(
            "externalId must be 1-{MAX_EXTERNAL_ID_LEN} printable ASCII characters without spaces"
        )));
    }
    validate_external_reference(PrincipalKind::ExternalApplicationPrincipal, value).map_err(
        |e| match e {
            PlatformError::Validation(_) => invalid(format!(
                "externalId must be 1-{MAX_EXTERNAL_ID_LEN} printable ASCII characters without spaces"
            )),
            _ => invalid(
                "externalId must be an opaque id: telephone numbers, tel: / sip: URIs and the tombstoned: prefix are refused",
            ),
        },
    )
}

/// `displayName`: 1–64 characters (trimmed), no control characters.
pub fn validate_display_name(value: &str) -> Result<String, PlatformError> {
    let v = value.trim();
    let n = v.chars().count();
    if n == 0 || n > MAX_DISPLAY_NAME_CHARS || v.chars().any(char::is_control) {
        return Err(invalid(format!(
            "displayName must be 1-{MAX_DISPLAY_NAME_CHARS} characters without control characters"
        )));
    }
    Ok(v.to_string())
}

/// `imageUrl`: an `https://` URL with a host, at most 512 characters, no whitespace.
pub fn validate_image_url(value: &str) -> Result<String, PlatformError> {
    let ok = value.len() <= MAX_IMAGE_URL_LEN
        && value
            .strip_prefix("https://")
            .is_some_and(|rest| !rest.is_empty() && !rest.starts_with('/'))
        && value.bytes().all(|b| (0x21..=0x7e).contains(&b));
    if !ok {
        return Err(invalid(format!(
            "imageUrl must be an https:// URL of at most {MAX_IMAGE_URL_LEN} characters"
        )));
    }
    Ok(value.to_string())
}

/// Profile fields of a request: `None` = not given (unchanged), `Some(None)` = clear.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ProfileInput {
    pub display_name: Option<Option<String>>,
    pub image_url: Option<Option<String>>,
}

impl ProfileInput {
    pub fn validated(self) -> Result<Self, PlatformError> {
        Ok(Self {
            display_name: match self.display_name {
                Some(Some(v)) => Some(Some(validate_display_name(&v)?)),
                other => other,
            },
            image_url: match self.image_url {
                Some(Some(v)) => Some(Some(validate_image_url(&v)?)),
                other => other,
            },
        })
    }
}

/// Loads any principal with its profile (no scope check).
pub async fn load<C: ConnectionTrait>(db: &C, id: Uuid) -> Result<Option<EndUserRow>, DbErr> {
    Row::find_by_statement(Statement::from_sql_and_values(
        DbBackend::Postgres,
        format!("{SELECT} WHERE p.id = $1"),
        [id.into()],
    ))
    .one(db)
    .await?
    .map(Row::typed)
    .transpose()
}

/// The visible partner user `id` of `scope` (any other: `None`, reported as NOT_FOUND).
pub async fn in_scope<C: ConnectionTrait>(
    db: &C,
    scope: &EndUserScope,
    id: Uuid,
) -> Result<Option<EndUserRow>, DbErr> {
    Ok(load(db, id).await?.filter(|r| {
        r.tenant_id == scope.tenant_id
            && r.application_id == scope.application_id
            && r.environment_id == scope.environment_id
            && r.kind == PrincipalKind::ExternalApplicationPrincipal
            && !r.tombstoned
    }))
}

async fn by_external_id<C: ConnectionTrait>(
    db: &C,
    scope: &EndUserScope,
    external_id: &str,
) -> Result<Option<EndUserRow>, DbErr> {
    Row::find_by_statement(Statement::from_sql_and_values(
        DbBackend::Postgres,
        format!(
            "{SELECT} WHERE p.tenant_id = $1 AND p.application_id = $2 AND p.environment_id = $3 \
               AND p.kind = '{KIND}' AND p.external_reference = $4 AND p.tombstoned_at IS NULL"
        ),
        [
            scope.tenant_id.into(),
            scope.application_id.into(),
            scope.environment_id.into(),
            external_id.into(),
        ],
    ))
    .one(db)
    .await?
    .map(Row::typed)
    .transpose()
}

/// A page of the visible partner users of an environment, in creation order.
#[derive(Debug, Clone)]
pub struct EndUserPage {
    pub items: Vec<EndUserRow>,
    pub next_cursor: Option<Uuid>,
}

/// Lists the visible partner users of `(tenant, application, environment)`. `cursor` is
/// the `nextCursor` of the previous page (a user id of the same environment).
pub async fn list<C: ConnectionTrait>(
    db: &C,
    tenant_id: Uuid,
    application_id: Uuid,
    environment_id: Uuid,
    external_id: Option<&str>,
    cursor: Option<Uuid>,
    limit: Option<u64>,
) -> Result<EndUserPage, PlatformError> {
    let limit = limit.unwrap_or(DEFAULT_PAGE);
    if limit == 0 || limit > MAX_PAGE {
        return Err(invalid(format!("limit must be between 1 and {MAX_PAGE}")));
    }
    let mut sql = format!(
        "{SELECT} WHERE p.tenant_id = $1 AND p.application_id = $2 AND p.environment_id = $3 \
           AND p.kind = '{KIND}' AND p.tombstoned_at IS NULL"
    );
    let mut values: Vec<Value> = vec![
        tenant_id.into(),
        application_id.into(),
        environment_id.into(),
    ];
    if let Some(external_id) = external_id {
        values.push(external_id.into());
        sql.push_str(&format!(" AND p.external_reference = ${}", values.len()));
    }
    if let Some(cursor) = cursor {
        let known = db
            .query_one(Statement::from_sql_and_values(
                DbBackend::Postgres,
                "SELECT 1 FROM platform_principals WHERE id = $1 AND environment_id = $2",
                [cursor.into(), environment_id.into()],
            ))
            .await?
            .is_some();
        if !known {
            return Err(invalid("cursor is not a cursor of this listing"));
        }
        values.push(cursor.into());
        sql.push_str(&format!(
            " AND (p.created_at, p.id) > (SELECT c.created_at, c.id FROM platform_principals c WHERE c.id = ${})",
            values.len()
        ));
    }
    values.push(((limit + 1) as i64).into());
    sql.push_str(&format!(
        " ORDER BY p.created_at, p.id LIMIT ${}",
        values.len()
    ));
    let rows = Row::find_by_statement(Statement::from_sql_and_values(
        DbBackend::Postgres,
        sql,
        values,
    ))
    .all(db)
    .await?;
    let mut items = rows
        .into_iter()
        .map(Row::typed)
        .collect::<Result<Vec<_>, _>>()?;
    let next_cursor = if items.len() as u64 > limit {
        items.truncate(limit as usize);
        items.last().map(|r| r.id)
    } else {
        None
    };
    Ok(EndUserPage { items, next_cursor })
}

/// Serializes partner-user mutations of one environment (caps stay exact under
/// concurrency).
async fn lock_environment(txn: &DatabaseTransaction, environment_id: Uuid) -> Result<(), DbErr> {
    txn.execute(Statement::from_sql_and_values(
        DbBackend::Postgres,
        "SELECT pg_advisory_xact_lock(7239063, hashtext($1::text))",
        [environment_id.to_string().into()],
    ))
    .await?;
    Ok(())
}

async fn count_active<C: ConnectionTrait>(db: &C, scope: &EndUserScope) -> Result<i64, DbErr> {
    Ok(db
        .query_one(Statement::from_sql_and_values(
            DbBackend::Postgres,
            format!(
                "SELECT count(*) AS n FROM platform_principals \
                 WHERE tenant_id = $1 AND application_id = $2 AND environment_id = $3 \
                   AND kind = '{KIND}' AND status = 'ACTIVE' AND tombstoned_at IS NULL"
            ),
            [
                scope.tenant_id.into(),
                scope.application_id.into(),
                scope.environment_id.into(),
            ],
        ))
        .await?
        .map(|r| r.try_get::<i64>("", "n"))
        .transpose()?
        .unwrap_or(0))
}

/// Writes the profile of `row` if `profile` changes it. Returns the changed field names.
async fn apply_profile(
    txn: &DatabaseTransaction,
    row: &EndUserRow,
    profile: &ProfileInput,
    now: DateTime<Utc>,
) -> Result<Vec<&'static str>, DbErr> {
    let display_name = profile
        .display_name
        .clone()
        .unwrap_or_else(|| row.display_name.clone());
    let image_url = profile
        .image_url
        .clone()
        .unwrap_or_else(|| row.image_url.clone());
    let mut changed = Vec::new();
    if display_name != row.display_name {
        changed.push("displayName");
    }
    if image_url != row.image_url {
        changed.push("imageUrl");
    }
    if changed.is_empty() {
        return Ok(changed);
    }
    upsert_profile(txn, row, display_name, image_url, now).await?;
    Ok(changed)
}

async fn upsert_profile(
    txn: &DatabaseTransaction,
    row: &EndUserRow,
    display_name: Option<String>,
    image_url: Option<String>,
    now: DateTime<Utc>,
) -> Result<(), DbErr> {
    txn.execute(Statement::from_sql_and_values(
        DbBackend::Postgres,
        "INSERT INTO platform_end_user_profiles (principal_id, tenant_id, application_id, \
           environment_id, display_name, image_url, created_at, updated_at) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $7) \
         ON CONFLICT (principal_id) DO UPDATE \
           SET display_name = EXCLUDED.display_name, image_url = EXCLUDED.image_url, \
               updated_at = EXCLUDED.updated_at",
        [
            row.id.into(),
            row.tenant_id.into(),
            row.application_id.into(),
            row.environment_id.into(),
            display_name.into(),
            image_url.into(),
            now.into(),
        ],
    ))
    .await?;
    Ok(())
}

fn entry(action: OperatorAction, row: &EndUserRow) -> AuditEntry {
    AuditEntry::new(action, AuditTargetKind::Principal, row.id).scope(
        Some(row.tenant_id),
        Some(row.application_id),
        Some(row.environment_id),
    )
}

async fn reload(db: &DatabaseConnection, id: Uuid) -> EndUserResult<EndUserRow> {
    Ok(load(db, id).await?.ok_or(PlatformError::NotFound("user"))?)
}

impl PlatformService {
    /// Creates the partner user `external_id` of `scope`, or returns the existing one
    /// (applying any given profile field). `true` when created.
    pub async fn upsert_end_user(
        &self,
        op: &OperatorContext,
        scope: &EndUserScope,
        cap: i64,
        external_id: &str,
        profile: ProfileInput,
    ) -> EndUserResult<(EndUserRow, bool)> {
        let external_id = validate_external_id(external_id)?;
        let profile = profile.validated()?;
        let now = self.clock.now();
        let txn = self.db.begin().await?;
        lock_environment(&txn, scope.environment_id).await?;
        if let Some(row) = by_external_id(&txn, scope, &external_id).await? {
            let changed = apply_profile(&txn, &row, &profile, now).await?;
            if !changed.is_empty() {
                operator::record(
                    &txn,
                    op,
                    &entry(OperatorAction::PrincipalProfileSet, &row)
                        .detail(json!({ "fields": changed, "source": "partner-users" })),
                    now,
                )
                .await?;
            }
            txn.commit().await?;
            return Ok((reload(&self.db, row.id).await?, false));
        }
        if count_active(&txn, scope).await? >= cap {
            return Err(EndUserError::LimitExceeded);
        }
        let (principal, created) = self
            .register_principal_in(
                &txn,
                EnvironmentId::from_uuid(scope.environment_id),
                NewPrincipal::External {
                    kind: PrincipalKind::ExternalApplicationPrincipal,
                    external_reference: external_id,
                },
            )
            .await?;
        let row = load(&txn, principal.id.as_uuid())
            .await?
            .ok_or(PlatformError::NotFound("user"))?;
        upsert_profile(
            &txn,
            &row,
            profile.display_name.flatten(),
            profile.image_url.flatten(),
            now,
        )
        .await?;
        operator::record(
            &txn,
            op,
            &entry(OperatorAction::PrincipalRegistered, &row).detail(json!({
                "kind": KIND,
                "created": created,
                "source": "partner-users",
            })),
            now,
        )
        .await?;
        txn.commit().await?;
        crate::metrics::inc("platform_access_end_users_created_total", &[]);
        Ok((reload(&self.db, row.id).await?, true))
    }

    /// PATCH: profile fields and / or status (ACTIVE / SUSPENDED only).
    pub async fn update_end_user(
        &self,
        op: &OperatorContext,
        scope: &EndUserScope,
        cap: i64,
        id: Uuid,
        profile: ProfileInput,
        status: Option<PrincipalStatus>,
    ) -> EndUserResult<EndUserRow> {
        let profile = profile.validated()?;
        if status.is_some_and(|s| s == PrincipalStatus::Deactivated) {
            return Err(
                invalid("status must be ACTIVE or SUSPENDED (DELETE removes a user)").into(),
            );
        }
        let now = self.clock.now();
        let txn = self.db.begin().await?;
        lock_environment(&txn, scope.environment_id).await?;
        let row = in_scope(&txn, scope, id)
            .await?
            .ok_or(PlatformError::NotFound("user"))?;
        let status_change = status.filter(|s| *s != row.status);
        if let Some(new_status) = status_change {
            if row.status == PrincipalStatus::Deactivated {
                return Err(PlatformError::InvalidState(
                    "a deactivated user cannot change status".to_string(),
                )
                .into());
            }
            if new_status == PrincipalStatus::Active && count_active(&txn, scope).await? >= cap {
                return Err(EndUserError::LimitExceeded);
            }
            self.set_principal_status_in(&txn, PrincipalId::from_uuid(id), new_status)
                .await?;
        }
        let changed = apply_profile(&txn, &row, &profile, now).await?;
        let audit = match status_change {
            Some(s) => Some(
                entry(OperatorAction::PrincipalStatusSet, &row).detail(json!({
                    "status": s.as_str(),
                    "fields": changed,
                    "source": "partner-users",
                })),
            ),
            None if !changed.is_empty() => Some(
                entry(OperatorAction::PrincipalProfileSet, &row)
                    .detail(json!({ "fields": changed, "source": "partner-users" })),
            ),
            None => None,
        };
        if let Some(audit) = audit {
            operator::record(&txn, op, &audit, now).await?;
        }
        txn.commit().await?;
        reload(&self.db, id).await
    }

    /// DELETE: the principal tombstone plus erasure of the profile values.
    pub async fn delete_end_user(
        &self,
        op: &OperatorContext,
        scope: &EndUserScope,
        id: Uuid,
    ) -> EndUserResult<()> {
        let now = self.clock.now();
        let txn = self.db.begin().await?;
        lock_environment(&txn, scope.environment_id).await?;
        let row = in_scope(&txn, scope, id)
            .await?
            .ok_or(PlatformError::NotFound("user"))?;
        let tombstoned = self
            .tombstone_principal_in(&txn, PrincipalId::from_uuid(id))
            .await?;
        txn.execute(Statement::from_sql_and_values(
            DbBackend::Postgres,
            "UPDATE platform_end_user_profiles SET display_name = NULL, image_url = NULL, \
               updated_at = $2 WHERE principal_id = $1",
            [id.into(), now.into()],
        ))
        .await?;
        operator::record(
            &txn,
            op,
            &entry(OperatorAction::PrincipalTombstoned, &row).detail(json!({
                "alreadyTombstoned": tombstoned.already_tombstoned,
                "source": "partner-users",
            })),
            now,
        )
        .await?;
        txn.commit().await?;
        Ok(())
    }

    /// Advances the user's session epoch to the next whole second: every user token
    /// issued at or before now is revoked; tokens minted afterwards carry `iat` at or
    /// after the epoch.
    pub async fn revoke_end_user_sessions(
        &self,
        op: &OperatorContext,
        scope: &EndUserScope,
        id: Uuid,
    ) -> EndUserResult<DateTime<Utc>> {
        let now = self.clock.now();
        let epoch = now.duration_trunc(Duration::seconds(1)).unwrap_or(now) + Duration::seconds(1);
        let txn = self.db.begin().await?;
        let row = in_scope(&txn, scope, id)
            .await?
            .ok_or(PlatformError::NotFound("user"))?;
        let stored: DateTime<Utc> = txn
            .query_one(Statement::from_sql_and_values(
                DbBackend::Postgres,
                "INSERT INTO platform_end_user_profiles (principal_id, tenant_id, application_id, \
                   environment_id, tokens_not_before, created_at, updated_at) \
                 VALUES ($1, $2, $3, $4, $5, $6, $6) \
                 ON CONFLICT (principal_id) DO UPDATE \
                   SET tokens_not_before = GREATEST(platform_end_user_profiles.tokens_not_before, \
                                                    EXCLUDED.tokens_not_before), \
                       updated_at = EXCLUDED.updated_at \
                 RETURNING tokens_not_before",
                [
                    row.id.into(),
                    row.tenant_id.into(),
                    row.application_id.into(),
                    row.environment_id.into(),
                    epoch.into(),
                    now.into(),
                ],
            ))
            .await?
            .ok_or(PlatformError::NotFound("user"))?
            .try_get("", "tokens_not_before")?;
        operator::record(
            &txn,
            op,
            &entry(OperatorAction::PrincipalSessionsRevoked, &row)
                .detail(json!({ "source": "partner-users" })),
            now,
        )
        .await?;
        // ADR 0068: the admin bridge's sign-out (`partners.user.sign-out`, unversioned) is
        // recorded in the same transaction.
        if let Some(command) = op.bridge() {
            super::bridge::record(&txn, command, None, Some("SIGNED_OUT"), now).await?;
        }
        txn.commit().await?;
        Ok(stored)
    }
}

/// Counts one mint for `principal_id` in the current clock hour. `false` once more than
/// [`MINTS_PER_HOUR`] were counted in that hour.
pub async fn note_mint<C: ConnectionTrait>(
    db: &C,
    principal_id: Uuid,
    now: DateTime<Utc>,
) -> Result<bool, DbErr> {
    let window_start = now.duration_trunc(Duration::hours(1)).unwrap_or(now);
    let mints: i64 = db
        .query_one(Statement::from_sql_and_values(
            DbBackend::Postgres,
            "INSERT INTO platform_user_token_mint_windows (principal_id, window_start, mints) \
             VALUES ($1, $2, 1) \
             ON CONFLICT (principal_id, window_start) \
             DO UPDATE SET mints = platform_user_token_mint_windows.mints + 1 \
             RETURNING mints",
            [principal_id.into(), window_start.into()],
        ))
        .await?
        .map(|r| r.try_get_by_index::<i64>(0))
        .transpose()?
        .unwrap_or(0);
    Ok(mints <= MINTS_PER_HOUR)
}

/// Deletes mint windows that started before `before`.
pub async fn prune_mint_windows<C: ConnectionTrait>(
    db: &C,
    before: DateTime<Utc>,
) -> Result<u64, DbErr> {
    Ok(db
        .execute(Statement::from_sql_and_values(
            DbBackend::Postgres,
            "DELETE FROM platform_user_token_mint_windows WHERE window_start < $1",
            [before.into()],
        ))
        .await?
        .rows_affected())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn external_ids_are_opaque_bounded_ascii() {
        for ok in [
            "user-1",
            "u_ABC.def",
            "12345",
            "acct:9f8e",
            &"x".repeat(128),
        ] {
            assert_eq!(validate_external_id(ok).unwrap(), ok);
        }
        for bad in [
            "",
            "has space",
            &"x".repeat(129),
            "naïve",
            "+2348012345678",
            "tel:08012345678",
            "sip:alice@example.org",
            "tombstoned:abc",
        ] {
            assert!(
                matches!(validate_external_id(bad), Err(PlatformError::Validation(_))),
                "{bad}"
            );
        }
    }

    #[test]
    fn profile_fields_are_validated() {
        assert_eq!(validate_display_name("  Ada  ").unwrap(), "Ada");
        assert!(validate_display_name("").is_err());
        assert!(validate_display_name(&"é".repeat(65)).is_err());
        assert!(validate_display_name(&"é".repeat(64)).is_ok());
        assert!(validate_display_name("bad\u{7}").is_err());
        assert!(validate_image_url("https://cdn.example.com/a.png").is_ok());
        for bad in [
            "http://cdn.example.com/a.png",
            "https://",
            "https:///x",
            "https://a b",
            "javascript:alert(1)",
            &format!("https://e.com/{}", "a".repeat(510)),
        ] {
            assert!(validate_image_url(bad).is_err(), "{bad}");
        }
        let p = ProfileInput {
            display_name: Some(None),
            image_url: Some(Some("ftp://x".into())),
        };
        assert!(p.validated().is_err());
    }
}
