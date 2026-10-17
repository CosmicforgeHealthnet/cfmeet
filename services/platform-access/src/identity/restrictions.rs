//! Authorized-principal restrictions of credentials.
//!
//! A credential with a restriction may act only for the listed principals of its own
//! environment. Platform Access returns the restriction in the authority chain; the
//! consuming service enforces it on every principal-scoped operation (Numbering: number
//! ownership and assignment; Calling: future use). A credential without rows is
//! unrestricted (Part III behaviour). The database pins every row to the credential's
//! own tenant / application / environment.

use std::collections::BTreeSet;

use sea_orm::{ConnectionTrait, DbBackend, Statement};

use super::{credentials::CredentialView, PlatformError, PlatformResult, PlatformService};
use crate::domain::{CredentialId, PrincipalId};

/// At most this many principals per credential restriction.
pub const MAX_RESTRICTED_PRINCIPALS: usize = 100;

impl PlatformService {
    /// Replaces the restriction of `credential_id` with `principals` (empty = remove the
    /// restriction). Every principal must belong to the credential's environment.
    pub(crate) async fn restrict_credential_principals_in<C: ConnectionTrait>(
        &self,
        db: &C,
        credential_id: CredentialId,
        principals: &[PrincipalId],
    ) -> PlatformResult<(CredentialView, Vec<PrincipalId>)> {
        let unique: BTreeSet<_> = principals.iter().copied().collect();
        if unique.len() > MAX_RESTRICTED_PRINCIPALS {
            return Err(PlatformError::Validation(format!(
                "a credential may be restricted to at most {MAX_RESTRICTED_PRINCIPALS} principals"
            )));
        }
        let credential = self.credential_in(db, credential_id).await?;
        db.execute(Statement::from_sql_and_values(
            DbBackend::Postgres,
            "DELETE FROM platform_credential_principal_restrictions WHERE credential_id = $1",
            [credential_id.as_uuid().into()],
        ))
        .await?;
        let now = self.clock.now();
        for principal in &unique {
            let inserted = db
                .execute(Statement::from_sql_and_values(
                    DbBackend::Postgres,
                    "INSERT INTO platform_credential_principal_restrictions \
                       (credential_id, principal_id, tenant_id, application_id, environment_id, created_at) \
                     SELECT $1, p.id, p.tenant_id, p.application_id, p.environment_id, $3 \
                     FROM platform_principals p \
                     WHERE p.id = $2 AND p.tenant_id = $4 AND p.application_id = $5 AND p.environment_id = $6",
                    [
                        credential_id.as_uuid().into(),
                        principal.as_uuid().into(),
                        now.into(),
                        credential.tenant_id.as_uuid().into(),
                        credential.application_id.as_uuid().into(),
                        credential.environment_id.as_uuid().into(),
                    ],
                ))
                .await?
                .rows_affected();
            if inserted == 0 {
                // Another environment's principal is reported exactly like a missing one.
                return Err(PlatformError::NotFound("principal"));
            }
        }
        tracing::info!(
            credential_id = %credential_id,
            principals = unique.len(),
            "credential principal restriction set"
        );
        Ok((credential, unique.into_iter().collect()))
    }

    /// The restriction of a credential (`None` = unrestricted).
    pub async fn credential_principal_restriction(
        &self,
        credential_id: CredentialId,
    ) -> PlatformResult<Option<Vec<PrincipalId>>> {
        let rows = self
            .db
            .query_all(Statement::from_sql_and_values(
                DbBackend::Postgres,
                "SELECT principal_id FROM platform_credential_principal_restrictions \
                 WHERE credential_id = $1 ORDER BY principal_id",
                [credential_id.as_uuid().into()],
            ))
            .await?;
        if rows.is_empty() {
            return Ok(None);
        }
        rows.iter()
            .map(|r| {
                r.try_get_by_index::<uuid::Uuid>(0)
                    .map(PrincipalId::from_uuid)
                    .map_err(PlatformError::from)
            })
            .collect::<PlatformResult<Vec<_>>>()
            .map(Some)
    }
}
