//! The signed runtime access policy (role -> permission -> purposes).
//!
//! CF Meet signs the exact JSON bytes with its policy key (Ed25519, offline); the bridge
//! loads only the revision pinned in configuration (`ADMIN_BRIDGE_POLICY_REVISION`),
//! verifies the signature with the configured public key, and refuses any grant outside
//! the contract ceiling (`docs/contract/access-policy.v1.json`, derived from the catalog).
//!
//! ```json
//! {"policy": "cfmeet-admin-bridge.access-policy.v1", "revision": 1,
//!  "issued_at": "2026-11-19T00:00:00Z",
//!  "roles": {"cfmeet.viewer": {"grants": {"bridge.datasets.read": ["metrics-read"]}}}}
//! ```

use std::collections::{BTreeMap, HashMap, HashSet};

use ed25519_dalek::{Signature, Signer, SigningKey, Verifier, VerifyingKey};
use serde::Deserialize;

use crate::contract::{
    catalog::catalog,
    model::{Purpose, Role},
};
use crate::store::PolicyRow;

pub const POLICY_KIND: &str = "cfmeet-admin-bridge.access-policy.v1";

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum PolicyError {
    #[error("no policy row at the pinned revision")]
    Missing,
    #[error("policy signature invalid")]
    Signature,
    #[error("policy key id does not match the configured key")]
    KeyId,
    #[error("policy document malformed: {0}")]
    Malformed(String),
    #[error("policy revision does not match the pinned revision")]
    Revision,
    #[error("policy grants outside the contract ceiling: {0}")]
    OutsideCeiling(String),
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Document {
    policy: String,
    revision: i64,
    issued_at: String,
    roles: BTreeMap<String, RoleDoc>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RoleDoc {
    grants: BTreeMap<String, Vec<String>>,
}

/// A verified policy: for each role, permission -> allowed purposes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AccessPolicy {
    pub revision: i64,
    grants: HashMap<Role, HashMap<String, HashSet<Purpose>>>,
}

impl AccessPolicy {
    /// True when `role` is granted `permission` for `purpose`.
    pub fn allows(&self, role: Role, permission: &str, purpose: Purpose) -> bool {
        self.grants
            .get(&role)
            .and_then(|g| g.get(permission))
            .is_some_and(|p| p.contains(&purpose))
    }
}

/// (role, permission) -> purposes the contract allows: the ceiling.
pub fn ceiling() -> HashMap<(Role, String), HashSet<Purpose>> {
    let mut m: HashMap<(Role, String), HashSet<Purpose>> = HashMap::new();
    for c in catalog() {
        for role in &c.roles {
            let entry = m.entry((*role, c.permission.clone())).or_default();
            entry.extend(c.purposes.iter().filter(|p| role.allows_purpose(**p)));
        }
    }
    m
}

pub fn sign(key: &SigningKey, document: &[u8]) -> String {
    hex::encode(key.sign(document).to_bytes())
}

/// Verifies and parses the stored row against the configured key and pinned revision.
pub fn verify(
    row: &PolicyRow,
    key: &VerifyingKey,
    key_id: &str,
    pinned_revision: i64,
) -> Result<AccessPolicy, PolicyError> {
    if row.key_id != key_id {
        return Err(PolicyError::KeyId);
    }
    let sig_bytes: [u8; 64] = hex::decode(&row.signature)
        .ok()
        .and_then(|b| b.try_into().ok())
        .ok_or(PolicyError::Signature)?;
    key.verify(&row.document, &Signature::from_bytes(&sig_bytes))
        .map_err(|_| PolicyError::Signature)?;
    let doc: Document =
        serde_json::from_slice(&row.document).map_err(|e| PolicyError::Malformed(e.to_string()))?;
    if doc.policy != POLICY_KIND {
        return Err(PolicyError::Malformed("policy kind".into()));
    }
    if chrono::DateTime::parse_from_rfc3339(&doc.issued_at).is_err() {
        return Err(PolicyError::Malformed("issued_at".into()));
    }
    if doc.revision < 1 || doc.revision != row.revision || doc.revision != pinned_revision {
        return Err(PolicyError::Revision);
    }
    let ceiling = ceiling();
    let mut grants: HashMap<Role, HashMap<String, HashSet<Purpose>>> = HashMap::new();
    for (role_name, role_doc) in doc.roles {
        let role = Role::parse(&role_name)
            .ok_or_else(|| PolicyError::Malformed(format!("unknown role {role_name}")))?;
        for (permission, purposes) in role_doc.grants {
            let allowed = ceiling.get(&(role, permission.clone())).ok_or_else(|| {
                PolicyError::OutsideCeiling(format!("{role_name} / {permission}"))
            })?;
            let mut set = HashSet::new();
            for p in purposes {
                let purpose = Purpose::parse(&p)
                    .ok_or_else(|| PolicyError::Malformed(format!("unknown purpose {p}")))?;
                if !allowed.contains(&purpose) {
                    return Err(PolicyError::OutsideCeiling(format!(
                        "{role_name} / {permission} / {p}"
                    )));
                }
                set.insert(purpose);
            }
            grants.entry(role).or_default().insert(permission, set);
        }
    }
    Ok(AccessPolicy {
        revision: doc.revision,
        grants,
    })
}

/// The ceiling as a policy document body (every role granted everything the contract
/// allows), for operators who start from the full ceiling and remove grants.
pub fn ceiling_document(revision: i64, issued_at: &str) -> serde_json::Value {
    let mut roles: BTreeMap<String, BTreeMap<String, Vec<String>>> = BTreeMap::new();
    for ((role, permission), purposes) in ceiling() {
        let mut p: Vec<String> = purposes.iter().map(|p| p.as_str().to_string()).collect();
        p.sort();
        roles
            .entry(role.as_str().into())
            .or_default()
            .insert(permission, p);
    }
    serde_json::json!({
        "policy": POLICY_KIND,
        "revision": revision,
        "issued_at": issued_at,
        "roles": roles.into_iter().map(|(r, g)| (r, serde_json::json!({"grants": g}))).collect::<serde_json::Map<_, _>>(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key() -> SigningKey {
        SigningKey::from_bytes(&[7u8; 32])
    }

    fn row(doc: &serde_json::Value, revision: i64) -> PolicyRow {
        let bytes = serde_json::to_vec(doc).unwrap();
        PolicyRow {
            revision,
            signature: sign(&key(), &bytes),
            document: bytes,
            key_id: "policy-1".into(),
        }
    }

    fn doc(roles: serde_json::Value) -> serde_json::Value {
        serde_json::json!({"policy": POLICY_KIND, "revision": 3, "issued_at": "2026-11-19T00:00:00Z", "roles": roles})
    }

    #[test]
    fn verifies_and_answers_grants() {
        let r = row(
            &doc(
                serde_json::json!({"cfmeet.viewer": {"grants": {"bridge.datasets.read": ["metrics-read"]}}}),
            ),
            3,
        );
        let p = verify(&r, &key().verifying_key(), "policy-1", 3).unwrap();
        assert!(p.allows(Role::Viewer, "bridge.datasets.read", Purpose::MetricsRead));
        assert!(!p.allows(Role::Viewer, "bridge.datasets.read", Purpose::OpsIncident));
        assert!(!p.allows(Role::Support, "bridge.datasets.read", Purpose::MetricsRead));
    }

    #[test]
    fn the_full_ceiling_is_a_valid_policy() {
        let r = row(&ceiling_document(3, "2026-11-19T00:00:00Z"), 3);
        assert!(verify(&r, &key().verifying_key(), "policy-1", 3).is_ok());
    }

    #[test]
    fn refuses_tampering_wrong_revision_and_grants_outside_the_ceiling() {
        let good = doc(
            serde_json::json!({"cfmeet.viewer": {"grants": {"bridge.datasets.read": ["metrics-read"]}}}),
        );
        let mut r = row(&good, 3);
        r.document[10] ^= 1;
        assert_eq!(
            verify(&r, &key().verifying_key(), "policy-1", 3),
            Err(PolicyError::Signature)
        );
        assert_eq!(
            verify(&row(&good, 3), &key().verifying_key(), "policy-1", 4),
            Err(PolicyError::Revision)
        );
        assert_eq!(
            verify(&row(&good, 3), &key().verifying_key(), "other", 3),
            Err(PolicyError::KeyId)
        );
        let other_key = SigningKey::from_bytes(&[8u8; 32]).verifying_key();
        assert_eq!(
            verify(&row(&good, 3), &other_key, "policy-1", 3),
            Err(PolicyError::Signature)
        );
        // viewer may never hold an accounts permission
        let wide = doc(
            serde_json::json!({"cfmeet.viewer": {"grants": {"accounts.users.read": ["support-review"]}}}),
        );
        assert!(matches!(
            verify(&row(&wide, 3), &key().verifying_key(), "policy-1", 3),
            Err(PolicyError::OutsideCeiling(_))
        ));
        // source_sync may never use ops-incident, even on projection status
        let leak = doc(
            serde_json::json!({"cfmeet.source_sync": {"grants": {"hq.projections.read": ["ops-incident"]}}}),
        );
        assert!(matches!(
            verify(&row(&leak, 3), &key().verifying_key(), "policy-1", 3),
            Err(PolicyError::OutsideCeiling(_))
        ));
        let unknown = doc(serde_json::json!({"cfmeet.root": {"grants": {}}}));
        assert!(matches!(
            verify(&row(&unknown, 3), &key().verifying_key(), "policy-1", 3),
            Err(PolicyError::Malformed(_))
        ));
    }
}
