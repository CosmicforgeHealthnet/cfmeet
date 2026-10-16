//! The authenticated internal caller.
//!
//! [`InternalServiceContext`] answers *which CF Meet backend service sent this request,
//! for whom it was addressed, in which runtime environment, and what this receiver allows
//! it to do*. Permissions come only from the receiver's trust policy. It can be created
//! only by [`crate::ServiceTokenVerifier`], cannot be deserialized, and is not a customer
//! platform context, a communication principal or a delegated context: those are
//! separate trust statements with separate types and no conversions between them.

use crate::{
    delegated::DelegatedPlatformContext,
    identity::{RuntimeEnvironment, ServiceId},
    ids::{CorrelationId, KeyId, RequestId, TokenId},
    permissions::{InternalPermission, PermissionSet},
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InternalServiceContext {
    pub(crate) authenticated_service: ServiceId,
    pub(crate) audience: ServiceId,
    pub(crate) runtime_environment: RuntimeEnvironment,
    pub(crate) key_id: KeyId,
    pub(crate) token_id: TokenId,
    pub(crate) issued_at: i64,
    pub(crate) expires_at: i64,
    pub(crate) request_id: RequestId,
    pub(crate) correlation_id: CorrelationId,
    pub(crate) permissions: PermissionSet,
    pub(crate) delegated_platform_context: Option<DelegatedPlatformContext>,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{service} is not granted {permission}")]
pub struct PermissionDenied {
    pub service: ServiceId,
    pub permission: InternalPermission,
}

impl InternalServiceContext {
    pub fn authenticated_service(&self) -> &ServiceId {
        &self.authenticated_service
    }

    pub fn audience(&self) -> &ServiceId {
        &self.audience
    }

    pub fn runtime_environment(&self) -> RuntimeEnvironment {
        self.runtime_environment
    }

    pub fn key_id(&self) -> &KeyId {
        &self.key_id
    }

    pub fn token_id(&self) -> &TokenId {
        &self.token_id
    }

    pub fn issued_at(&self) -> i64 {
        self.issued_at
    }

    pub fn expires_at(&self) -> i64 {
        self.expires_at
    }

    pub fn request_id(&self) -> &RequestId {
        &self.request_id
    }

    pub fn correlation_id(&self) -> &CorrelationId {
        &self.correlation_id
    }

    /// Permissions granted by THIS receiver's policy.
    pub fn permissions(&self) -> &PermissionSet {
        &self.permissions
    }

    pub fn delegated_platform_context(&self) -> Option<&DelegatedPlatformContext> {
        self.delegated_platform_context.as_ref()
    }

    pub fn has_permission(&self, permission: InternalPermission) -> bool {
        self.permissions.contains(permission)
    }

    pub fn require_permission(
        &self,
        permission: InternalPermission,
    ) -> Result<(), PermissionDenied> {
        if self.has_permission(permission) {
            Ok(())
        } else {
            Err(PermissionDenied {
                service: self.authenticated_service.clone(),
                permission,
            })
        }
    }
}
