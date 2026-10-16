//! Receiver-controlled trust bundle (contract `cfmeet.platform.service-trust-bundle` v1).
//!
//! The bundle is the receiving service's own policy: which issuers it accepts in its
//! runtime environment, with which public keys, and which internal permissions it grants
//! each of them. It is validated completely before a service starts; any problem is fatal.
//!
//! Policy rules beyond the JSON Schema:
//! * `receiver` / `runtime_environment` equal the service's own configuration;
//! * every grant names `audience = receiver` and the same runtime environment;
//! * no issuer twice, no issuer equal to the receiver (no self-trust);
//! * every `kid` is unique across the whole bundle and bound to exactly one issuer;
//! * keys are valid, non-weak Ed25519 points;
//! * permissions are registry permissions received by THIS service and grantable to THAT
//!   issuer — so e.g. `protected-contact.resolve-for-execution` can only ever be granted
//!   to Telephony by Privacy Relay, never to Calling;
//! * no empty, wildcard or super-permissions.

use std::{
    collections::{BTreeMap, BTreeSet},
    path::Path,
};

use jsonwebtoken::DecodingKey;
use serde::Deserialize;

use crate::{
    identity::{bounded, RuntimeEnvironment, ServiceId},
    ids::KeyId,
    keys::{decoding_key, KeyError},
    permissions::{InternalPermission, PermissionSet},
};

pub const BUNDLE_VERSION: u32 = 1;
pub const MAX_KEYS_PER_ISSUER: usize = 4;
pub const MAX_ISSUERS: usize = 64;

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum TrustBundleError {
    #[error("trust bundle file {path} cannot be read")]
    Unreadable { path: String },
    #[error("trust bundle is malformed: {0}")]
    Malformed(String),
    #[error("unsupported trust bundle version {0} (expected {BUNDLE_VERSION})")]
    UnsupportedVersion(u32),
    #[error("{field} `{value}` is not a valid service identity (wildcards are not allowed)")]
    InvalidServiceId { field: &'static str, value: String },
    #[error("{field} `{value}` is not a valid runtime environment")]
    InvalidRuntimeEnvironment { field: &'static str, value: String },
    #[error("bundle receiver {bundle} does not match this service ({configured})")]
    ReceiverMismatch {
        configured: ServiceId,
        bundle: ServiceId,
    },
    #[error("bundle runtime environment {bundle} does not match this service ({configured})")]
    RuntimeEnvironmentMismatch {
        configured: RuntimeEnvironment,
        bundle: RuntimeEnvironment,
    },
    #[error(
        "grant for {issuer} has audience {audience}; it must be exactly the receiver {receiver}"
    )]
    AudienceMismatch {
        issuer: ServiceId,
        audience: ServiceId,
        receiver: ServiceId,
    },
    #[error("grant for {issuer} is for runtime environment {grant}; the bundle is for {bundle}")]
    GrantRuntimeEnvironmentMismatch {
        issuer: ServiceId,
        grant: RuntimeEnvironment,
        bundle: RuntimeEnvironment,
    },
    #[error("issuer {0} is listed more than once")]
    DuplicateIssuer(ServiceId),
    #[error("a service cannot trust itself ({0})")]
    SelfTrust(ServiceId),
    #[error("too many trusted issuers (maximum {MAX_ISSUERS})")]
    TooManyIssuers,
    #[error("issuer {0} has no keys")]
    NoKeys(ServiceId),
    #[error("issuer {issuer} has more than {MAX_KEYS_PER_ISSUER} keys")]
    TooManyKeys { issuer: ServiceId },
    #[error("key id `{0}` is invalid")]
    InvalidKeyId(String),
    #[error("key id {0} appears more than once")]
    DuplicateKeyId(KeyId),
    #[error("key {kid} must be an OKP / Ed25519 key")]
    UnsupportedKeyType { kid: KeyId },
    #[error("{0}")]
    InvalidPublicKey(KeyError),
    #[error("issuer {0} is granted no permissions")]
    EmptyPermissions(ServiceId),
    #[error("issuer {issuer} has an empty permission")]
    EmptyPermission { issuer: ServiceId },
    #[error("issuer {issuer} is granted wildcard / super-permission `{value}`")]
    WildcardPermission { issuer: ServiceId, value: String },
    #[error("issuer {issuer} is granted unknown permission `{value}`")]
    UnknownPermission { issuer: ServiceId, value: String },
    #[error("issuer {issuer} is granted {permission} twice")]
    DuplicatePermission {
        issuer: ServiceId,
        permission: InternalPermission,
    },
    #[error("{permission} is received by {owner}, not by {receiver}")]
    PermissionNotReceivedHere {
        permission: InternalPermission,
        owner: &'static str,
        receiver: ServiceId,
    },
    #[error("{permission} may never be granted to {issuer}")]
    ForbiddenGrant {
        permission: InternalPermission,
        issuer: ServiceId,
    },
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawBundle {
    bundle_version: u32,
    receiver: String,
    runtime_environment: String,
    trusted_issuers: Vec<RawIssuer>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawIssuer {
    issuer: String,
    audience: String,
    runtime_environment: String,
    keys: Vec<RawKey>,
    permissions: Vec<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawKey {
    kid: String,
    kty: String,
    crv: String,
    x: String,
}

/// Grant of one trusted issuer.
#[derive(Debug, Clone)]
pub struct IssuerGrant {
    issuer: ServiceId,
    key_ids: Vec<KeyId>,
    permissions: PermissionSet,
}

impl IssuerGrant {
    pub fn issuer(&self) -> &ServiceId {
        &self.issuer
    }

    pub fn key_ids(&self) -> &[KeyId] {
        &self.key_ids
    }

    pub fn permissions(&self) -> &PermissionSet {
        &self.permissions
    }
}

pub(crate) struct TrustedKey {
    pub issuer: ServiceId,
    pub key: DecodingKey,
    /// Public key (base64url `x`), for cross-checking key separation between token kinds.
    pub x: String,
}

/// A fully validated trust bundle.
pub struct TrustPolicy {
    receiver: ServiceId,
    runtime_environment: RuntimeEnvironment,
    grants: BTreeMap<ServiceId, IssuerGrant>,
    keys: BTreeMap<KeyId, TrustedKey>,
}

impl std::fmt::Debug for TrustPolicy {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TrustPolicy")
            .field("receiver", &self.receiver)
            .field("runtime_environment", &self.runtime_environment)
            .field("grants", &self.grants)
            .finish()
    }
}

fn service_id(field: &'static str, value: &str) -> Result<ServiceId, TrustBundleError> {
    ServiceId::parse(value).map_err(|_| TrustBundleError::InvalidServiceId {
        field,
        value: bounded(value),
    })
}

fn runtime_environment(
    field: &'static str,
    value: &str,
) -> Result<RuntimeEnvironment, TrustBundleError> {
    RuntimeEnvironment::parse(value).map_err(|_| TrustBundleError::InvalidRuntimeEnvironment {
        field,
        value: bounded(value),
    })
}

impl TrustPolicy {
    /// Parses and validates a bundle for the service `receiver` running in
    /// `runtime_environment`.
    pub fn from_json(
        json: &str,
        receiver: &ServiceId,
        environment: RuntimeEnvironment,
    ) -> Result<Self, TrustBundleError> {
        let raw: RawBundle =
            serde_json::from_str(json).map_err(|e| TrustBundleError::Malformed(e.to_string()))?;
        if raw.bundle_version != BUNDLE_VERSION {
            return Err(TrustBundleError::UnsupportedVersion(raw.bundle_version));
        }
        let bundle_receiver = service_id("receiver", &raw.receiver)?;
        let bundle_environment =
            runtime_environment("runtime_environment", &raw.runtime_environment)?;
        if &bundle_receiver != receiver {
            return Err(TrustBundleError::ReceiverMismatch {
                configured: receiver.clone(),
                bundle: bundle_receiver,
            });
        }
        if bundle_environment != environment {
            return Err(TrustBundleError::RuntimeEnvironmentMismatch {
                configured: environment,
                bundle: bundle_environment,
            });
        }
        if raw.trusted_issuers.len() > MAX_ISSUERS {
            return Err(TrustBundleError::TooManyIssuers);
        }

        let mut grants = BTreeMap::new();
        let mut keys = BTreeMap::new();
        for entry in raw.trusted_issuers {
            let issuer = service_id("issuer", &entry.issuer)?;
            let audience = service_id("audience", &entry.audience)?;
            let grant_environment =
                runtime_environment("runtime_environment", &entry.runtime_environment)?;
            if &audience != receiver {
                return Err(TrustBundleError::AudienceMismatch {
                    issuer,
                    audience,
                    receiver: receiver.clone(),
                });
            }
            if grant_environment != environment {
                return Err(TrustBundleError::GrantRuntimeEnvironmentMismatch {
                    issuer,
                    grant: grant_environment,
                    bundle: environment,
                });
            }
            if &issuer == receiver {
                return Err(TrustBundleError::SelfTrust(issuer));
            }
            if grants.contains_key(&issuer) {
                return Err(TrustBundleError::DuplicateIssuer(issuer));
            }

            if entry.keys.is_empty() {
                return Err(TrustBundleError::NoKeys(issuer));
            }
            if entry.keys.len() > MAX_KEYS_PER_ISSUER {
                return Err(TrustBundleError::TooManyKeys { issuer });
            }
            let mut key_ids = Vec::new();
            for key in entry.keys {
                let kid = KeyId::parse(&key.kid)
                    .map_err(|_| TrustBundleError::InvalidKeyId(bounded(&key.kid)))?;
                if keys.contains_key(&kid) {
                    return Err(TrustBundleError::DuplicateKeyId(kid));
                }
                if key.kty != "OKP" || key.crv != "Ed25519" {
                    return Err(TrustBundleError::UnsupportedKeyType { kid });
                }
                let decoding = decoding_key(kid.as_str(), &key.x)
                    .map_err(TrustBundleError::InvalidPublicKey)?;
                keys.insert(
                    kid.clone(),
                    TrustedKey {
                        issuer: issuer.clone(),
                        key: decoding,
                        x: key.x.clone(),
                    },
                );
                key_ids.push(kid);
            }

            if entry.permissions.is_empty() {
                return Err(TrustBundleError::EmptyPermissions(issuer));
            }
            let mut permissions = BTreeSet::new();
            for value in &entry.permissions {
                if value.trim().is_empty() {
                    return Err(TrustBundleError::EmptyPermission { issuer });
                }
                if InternalPermission::is_wildcard_like(value) {
                    return Err(TrustBundleError::WildcardPermission {
                        issuer,
                        value: bounded(value),
                    });
                }
                let permission = InternalPermission::parse(value).ok_or_else(|| {
                    TrustBundleError::UnknownPermission {
                        issuer: issuer.clone(),
                        value: bounded(value),
                    }
                })?;
                let definition = permission.definition();
                if definition.receiver != receiver.as_str() {
                    return Err(TrustBundleError::PermissionNotReceivedHere {
                        permission,
                        owner: definition.receiver,
                        receiver: receiver.clone(),
                    });
                }
                if !definition.grantable_to.contains(&issuer.as_str()) {
                    return Err(TrustBundleError::ForbiddenGrant { permission, issuer });
                }
                if !permissions.insert(permission) {
                    return Err(TrustBundleError::DuplicatePermission { issuer, permission });
                }
            }

            grants.insert(
                issuer.clone(),
                IssuerGrant {
                    issuer,
                    key_ids,
                    permissions: PermissionSet::new(permissions),
                },
            );
        }

        Ok(Self {
            receiver: receiver.clone(),
            runtime_environment: environment,
            grants,
            keys,
        })
    }

    pub fn from_file(
        path: &Path,
        receiver: &ServiceId,
        environment: RuntimeEnvironment,
    ) -> Result<Self, TrustBundleError> {
        let json = std::fs::read_to_string(path).map_err(|_| TrustBundleError::Unreadable {
            path: path.display().to_string(),
        })?;
        Self::from_json(&json, receiver, environment)
    }

    pub fn receiver(&self) -> &ServiceId {
        &self.receiver
    }

    pub fn runtime_environment(&self) -> RuntimeEnvironment {
        self.runtime_environment
    }

    pub fn grants(&self) -> impl Iterator<Item = &IssuerGrant> {
        self.grants.values()
    }

    pub fn grant_for(&self, issuer: &ServiceId) -> Option<&IssuerGrant> {
        self.grants.get(issuer)
    }

    /// Public keys (base64url Ed25519 `x`) of every trusted issuer. Used to refuse a key
    /// trusted for service tokens being trusted for another token kind as well.
    pub fn public_keys(&self) -> impl Iterator<Item = &str> {
        self.keys.values().map(|k| k.x.as_str())
    }

    pub(crate) fn key(&self, kid: &KeyId) -> Option<&TrustedKey> {
        self.keys.get(kid)
    }
}
