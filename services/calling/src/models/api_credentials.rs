//! API credential row. There is no column for the raw secret: only the keyed verifier.

use std::fmt;

use sea_orm::entity::prelude::*;

use crate::domain::CredentialStatus;

/// HMAC-SHA-256 verifier bytes. Not secret by itself, but never logged or serialized:
/// `Debug` is redacted and there is deliberately no `Serialize`.
#[derive(Clone, PartialEq, Eq, DeriveValueType)]
pub struct Verifier(pub Vec<u8>);

impl fmt::Debug for Verifier {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Verifier([REDACTED])")
    }
}

#[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel)]
#[sea_orm(table_name = "platform_api_credentials")]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub id: Uuid,
    pub public_id: String,
    pub tenant_id: Uuid,
    pub application_id: Uuid,
    pub environment_id: Uuid,
    pub service_account_id: Uuid,
    pub label: Option<String>,
    pub verifier: Verifier,
    pub verifier_algorithm: String,
    pub verifier_key_version: i32,
    pub last_four: String,
    pub status: CredentialStatus,
    pub not_before: DateTimeUtc,
    pub expires_at: DateTimeUtc,
    pub revoked_at: Option<DateTimeUtc>,
    pub rotated_from_credential_id: Option<Uuid>,
    pub superseded_by_credential_id: Option<Uuid>,
    pub superseded_at: Option<DateTimeUtc>,
    pub supersession_valid_until: Option<DateTimeUtc>,
    pub created_at: DateTimeUtc,
    pub updated_at: DateTimeUtc,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
