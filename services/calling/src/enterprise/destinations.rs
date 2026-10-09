//! Typed enterprise destinations (Part F §29, §44, §59, §83–§85).
//!
//! Every place a configuration routes a call to (an extension's target, a fallback, an
//! overflow, an IVR branch, an open / closed-hours route, a forwarding target) is a typed
//! `(type, id)` pair. The service validates that the target exists, is ACTIVE and belongs
//! to the caller's tenant / application / environment; the database independently
//! refuses a dangling or cross-scope destination (`enterprise_destination_exists`).
//! A public telephone destination exists only as a registered public contact
//! (`PUBLIC_CONTACT`: an opaque Privacy Relay ContactReference, forwarding / closed-hours
//! routes only) — never a number.

use sea_orm::ConnectionTrait;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use super::{scope_values, stmt, EnterpriseError, EnterpriseResult};
use crate::calls::command::CallScope;

text_enum!(
    /// Kinds of enterprise destination.
    DestinationType {
        EnterpriseMember = "ENTERPRISE_MEMBER",
        Principal = "PRINCIPAL",
        Extension = "EXTENSION",
        RingGroup = "RING_GROUP",
        Queue = "QUEUE",
        Ivr = "IVR",
        Voicemail = "VOICEMAIL",
        Schedule = "SCHEDULE",
        PublicContact = "PUBLIC_CONTACT",
    }
);

impl DestinationType {
    /// A human (or the application acting for one) answers it directly.
    pub fn is_principal(&self) -> bool {
        matches!(
            self,
            DestinationType::Principal | DestinationType::EnterpriseMember
        )
    }
}

/// A typed destination reference.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Destination {
    #[serde(rename = "type")]
    pub kind: DestinationType,
    pub id: Uuid,
}

impl Destination {
    pub fn new(kind: DestinationType, id: Uuid) -> Self {
        Self { kind, id }
    }
}

/// Destination kinds an extension may route to (§29). Raw public numbers never.
pub const EXTENSION_TARGETS: &[DestinationType] = &[
    DestinationType::EnterpriseMember,
    DestinationType::Principal,
    DestinationType::RingGroup,
    DestinationType::Queue,
    DestinationType::Ivr,
    DestinationType::Schedule,
    DestinationType::Voicemail,
];

/// Whether `dest` exists and is usable (ACTIVE) in `scope`.
pub(crate) async fn destination_active<C: ConnectionTrait>(
    db: &C,
    scope: CallScope,
    dest: Destination,
) -> EnterpriseResult<bool> {
    let sql = match dest.kind {
        DestinationType::Principal => {
            "SELECT 1 FROM platform_principals WHERE tenant_id = $1 AND application_id = $2 \
             AND environment_id = $3 AND id = $4 AND status = 'ACTIVE'"
        }
        DestinationType::EnterpriseMember => {
            "SELECT 1 FROM platform_principals WHERE tenant_id = $1 AND application_id = $2 \
             AND environment_id = $3 AND id = $4 AND status = 'ACTIVE' AND kind = 'ENTERPRISE_MEMBER'"
        }
        DestinationType::Extension => {
            "SELECT 1 FROM enterprise_extensions WHERE tenant_id = $1 AND application_id = $2 \
             AND environment_id = $3 AND id = $4 AND status = 'ACTIVE'"
        }
        DestinationType::RingGroup => {
            "SELECT 1 FROM enterprise_ring_groups WHERE tenant_id = $1 AND application_id = $2 \
             AND environment_id = $3 AND id = $4 AND status = 'ACTIVE'"
        }
        DestinationType::Queue => {
            "SELECT 1 FROM enterprise_call_queues WHERE tenant_id = $1 AND application_id = $2 \
             AND environment_id = $3 AND id = $4 AND status = 'ACTIVE'"
        }
        DestinationType::Ivr => {
            "SELECT 1 FROM enterprise_ivr_flows WHERE tenant_id = $1 AND application_id = $2 \
             AND environment_id = $3 AND id = $4 AND status = 'ACTIVE' AND active_version_id IS NOT NULL"
        }
        DestinationType::Schedule => {
            "SELECT 1 FROM enterprise_business_hours WHERE tenant_id = $1 AND application_id = $2 \
             AND environment_id = $3 AND id = $4 AND status = 'ACTIVE'"
        }
        DestinationType::Voicemail => {
            "SELECT 1 FROM voicemail_mailboxes WHERE tenant_id = $1 AND application_id = $2 \
             AND environment_id = $3 AND id = $4 AND status = 'ACTIVE'"
        }
        DestinationType::PublicContact => {
            "SELECT 1 FROM enterprise_public_contacts WHERE tenant_id = $1 AND application_id = $2 \
             AND environment_id = $3 AND id = $4 AND status = 'ACTIVE'"
        }
    };
    let mut values = scope_values(scope);
    values.push(dest.id.into());
    Ok(db.query_one(stmt(sql, values)).await?.is_some())
}

/// Validates a configured destination: an allowed kind, ACTIVE, in `scope`. Anything
/// else of another scope is reported exactly like a missing one.
pub(crate) async fn validate<C: ConnectionTrait>(
    db: &C,
    scope: CallScope,
    dest: Destination,
    allowed: &[DestinationType],
) -> EnterpriseResult<()> {
    if !allowed.contains(&dest.kind) {
        return Err(EnterpriseError::Validation(
            "the destination type is not allowed here.",
        ));
    }
    if destination_active(db, scope, dest).await? {
        Ok(())
    } else {
        Err(EnterpriseError::NotFound)
    }
}
