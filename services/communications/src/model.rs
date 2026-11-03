use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum Activation {
    AccountCreated,
    Active,
    Suspended,
    Deactivated,
}
#[derive(Debug, Clone, Deserialize)]
pub struct Profile {
    pub id: Uuid,
    pub tenant_id: Uuid,
    pub application_id: Uuid,
    pub environment_id: Uuid,
    pub account_reference: String,
    pub principal_id: Uuid,
    pub display_name: String,
    pub avatar_reference: Option<String>,
    pub activation_state: Activation,
    pub version: i64,
    pub account_version: i64,
    pub credentials_not_before: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}
impl Profile {
    pub fn scope(&self) -> cf_communications_policy::Scope {
        cf_communications_policy::Scope {
            tenant_id: self.tenant_id,
            application_id: self.application_id,
            environment_id: self.environment_id,
        }
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum Audience {
    Connections,
    Nobody,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum RequestPermission {
    Everyone,
    Nobody,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum InvitationPermission {
    Enabled,
    Disabled,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum Visibility {
    Public,
    Connections,
    Hidden,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Privacy {
    pub public_id_discoverable: bool,
    pub invitation_permissions: InvitationPermission,
    pub contact_request_permissions: RequestPermission,
    pub incoming_call_policy: Audience,
    pub direct_chat_policy: Audience,
    pub public_profile_visibility: Visibility,
    pub presence_visibility: Audience,
}
impl Default for Privacy {
    fn default() -> Self {
        Self {
            public_id_discoverable: true,
            invitation_permissions: InvitationPermission::Enabled,
            contact_request_permissions: RequestPermission::Everyone,
            incoming_call_policy: Audience::Connections,
            direct_chat_policy: Audience::Connections,
            public_profile_visibility: Visibility::Public,
            presence_visibility: Audience::Connections,
        }
    }
}
#[derive(Debug, Clone, Deserialize)]
pub struct PrivacyRow {
    pub version: i64,
    pub public_id_discoverable: bool,
    pub invitation_permissions: InvitationPermission,
    pub contact_request_permissions: RequestPermission,
    pub incoming_call_policy: Audience,
    pub direct_chat_policy: Audience,
    pub public_profile_visibility: Visibility,
    pub presence_visibility: Audience,
}
impl PrivacyRow {
    pub fn settings(&self) -> Privacy {
        Privacy {
            public_id_discoverable: self.public_id_discoverable,
            invitation_permissions: self.invitation_permissions,
            contact_request_permissions: self.contact_request_permissions,
            incoming_call_policy: self.incoming_call_policy,
            direct_chat_policy: self.direct_chat_policy,
            public_profile_visibility: self.public_profile_visibility,
            presence_visibility: self.presence_visibility,
        }
    }
}
#[derive(Debug, Clone, Deserialize)]
pub struct Connection {
    pub id: Uuid,
    pub left_profile_id: Uuid,
    pub right_profile_id: Uuid,
    pub state: String,
    pub version: i64,
}
impl Connection {
    pub fn other(&self, owner: Uuid) -> Option<Uuid> {
        if self.left_profile_id == owner {
            Some(self.right_profile_id)
        } else if self.right_profile_id == owner {
            Some(self.left_profile_id)
        } else {
            None
        }
    }
}
#[derive(Debug, Clone, Deserialize)]
pub struct ContactRequest {
    pub id: Uuid,
    pub connection_id: Uuid,
    pub sender_profile_id: Uuid,
    pub recipient_profile_id: Uuid,
    pub state: String,
    pub version: i64,
    pub expires_at: DateTime<Utc>,
}
#[derive(Debug, Clone, Deserialize)]
pub struct Invite {
    pub id: Uuid,
    pub owner_profile_id: Uuid,
    pub purpose: String,
    pub expires_at: DateTime<Utc>,
    pub revoked_at: Option<DateTime<Utc>>,
    pub max_uses: i32,
    pub uses: i32,
    pub version: i64,
}

pub use cf_communications_policy::{Dispatch, VerifiedAccount};
