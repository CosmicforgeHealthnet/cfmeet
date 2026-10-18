//! Effective messaging policy (Part V D.4 / D.6).
//!
//! Effective policy = explicit PLATFORM defaults ([`platform_defaults`], versioned by
//! [`PLATFORM_POLICY_VERSION`]) merged with the application environment's validated,
//! versioned overrides ([`ApplicationPolicy`]). Resolution is a pure function; its result
//! is canonicalized (sorted keys) and stored as an immutable, content-addressed snapshot
//! that every conversation references, so a conversation's policy can be reproduced.
//!
//! Platform defaults are deliberately conservative and assume nothing about features that
//! do not exist yet: PRIVATE_E2EE is the default for every private context except
//! SUPPORT (support conversations commonly involve server-side agents / bots, so the
//! application must choose); MANAGED is NEVER available unless the application's policy
//! enables it for a context; attachments, moderation, retention, presence and read
//! receipts are unset / disabled until their workstreams; legal hold is eligible only
//! where MANAGED is allowed; the SMS Bridge is never eligible.

use std::collections::BTreeMap;

use chrono::{DateTime, Utc};
use sea_orm::{ConnectionTrait, FromQueryResult};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use uuid::Uuid;

use super::ContextClass;
use crate::{
    db,
    domain::SecurityProfile,
    error::{Error, ErrorCode, Result},
};

/// Version of the platform defaults below.
pub const PLATFORM_POLICY_VERSION: i32 = 1;
/// Version of the policy document schema.
pub const POLICY_SCHEMA_VERSION: i32 = 1;
/// Version of the security policy persisted with every conversation (profile rules).
pub const SECURITY_POLICY_VERSION: i32 = 1;

/// The effective policy of one context class.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ContextPolicy {
    pub enabled: bool,
    pub allowed_profiles: Vec<SecurityProfile>,
    /// `None`: the creator must name a profile (and it must be allowed).
    pub default_profile: Option<SecurityProfile>,
    /// `None`: unset (the membership workstream's platform bound applies).
    pub max_members: Option<i64>,
    /// `None`: no context expiry.
    pub max_lifetime_seconds: Option<i64>,
    pub reopen_allowed: bool,
    pub legal_hold_eligible: bool,
    pub sms_bridge_eligible: bool,
}

/// The effective policy of an application environment.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EffectivePolicy {
    pub schema_version: i32,
    pub platform_policy_version: i32,
    pub application_policy_version: i64,
    pub contexts: BTreeMap<ContextClass, ContextPolicy>,
    pub sending_enabled: bool,
    /// Features of later workstreams: present, explicit, and off / unset.
    pub attachments: Value,
    pub editing: Value,
    pub privacy: Value,
    pub retention: Value,
    pub moderation: Value,
}

/// The platform defaults (explicit; see the module documentation).
pub fn platform_defaults() -> EffectivePolicy {
    let contexts = ContextClass::ALL
        .iter()
        .map(|c| {
            let (allowed, default) = match c {
                ContextClass::Support => (vec![], None),
                _ => (
                    vec![SecurityProfile::PrivateE2ee],
                    Some(SecurityProfile::PrivateE2ee),
                ),
            };
            (
                *c,
                ContextPolicy {
                    enabled: true,
                    allowed_profiles: allowed,
                    default_profile: default,
                    max_members: if *c == ContextClass::Direct {
                        Some(2)
                    } else {
                        None
                    },
                    max_lifetime_seconds: None,
                    reopen_allowed: true,
                    legal_hold_eligible: false,
                    sms_bridge_eligible: false,
                },
            )
        })
        .collect();
    EffectivePolicy {
        schema_version: POLICY_SCHEMA_VERSION,
        platform_policy_version: PLATFORM_POLICY_VERSION,
        application_policy_version: 0,
        contexts,
        sending_enabled: true,
        attachments: json!({ "enabled": false, "maxBytes": null, "allowedMediaTypes": [] }),
        editing: json!({ "editWindowSeconds": null, "deleteWindowSeconds": null }),
        privacy: json!({ "presence": null, "readReceipts": null }),
        retention: json!({ "days": null }),
        moderation: json!({ "enabled": false }),
    }
}

/// An application's override of one context class.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ContextOverride {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub enabled: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub allowed_profiles: Option<Vec<SecurityProfile>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_profile: Option<SecurityProfile>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_members: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_lifetime_seconds: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reopen_allowed: Option<bool>,
}

/// An application environment's policy overrides (what `PUT /v1/messaging/policy` sets).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ApplicationPolicy {
    #[serde(default)]
    pub contexts: BTreeMap<ContextClass, ContextOverride>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sending_enabled: Option<bool>,
}

const MAX_LIFETIME: i64 = 10 * 365 * 86_400;

impl ApplicationPolicy {
    /// Validation independent of the defaults (bounds, consistency).
    pub fn validate(&self) -> std::result::Result<(), String> {
        for (class, o) in &self.contexts {
            if let Some(profiles) = &o.allowed_profiles {
                let mut seen = profiles.clone();
                seen.sort_by_key(|p| p.as_str());
                seen.dedup();
                if seen.len() != profiles.len() {
                    return Err(format!(
                        "{}: allowedProfiles has duplicates",
                        class.as_str()
                    ));
                }
            }
            if let Some(m) = o.max_members {
                if *class == ContextClass::Direct && m != 2 {
                    return Err("DIRECT conversations always have exactly 2 members".into());
                }
                if !(2..=100_000).contains(&m) {
                    return Err(format!(
                        "{}: maxMembers must be between 2 and 100000",
                        class.as_str()
                    ));
                }
            }
            if let Some(l) = o.max_lifetime_seconds {
                if !(60..=MAX_LIFETIME).contains(&l) {
                    return Err(format!(
                        "{}: maxLifetimeSeconds must be between 60 and {MAX_LIFETIME}",
                        class.as_str()
                    ));
                }
            }
        }
        Ok(())
    }
}

/// Deterministic resolution of the effective policy.
pub fn resolve(
    overrides: &ApplicationPolicy,
    application_version: i64,
) -> std::result::Result<EffectivePolicy, String> {
    overrides.validate()?;
    let mut p = platform_defaults();
    p.application_policy_version = application_version;
    if let Some(s) = overrides.sending_enabled {
        p.sending_enabled = s;
    }
    for (class, o) in &overrides.contexts {
        let c = p.contexts.get_mut(class).expect("every class has defaults");
        if let Some(e) = o.enabled {
            c.enabled = e;
        }
        if let Some(allowed) = &o.allowed_profiles {
            let mut allowed = allowed.clone();
            allowed.sort_by_key(|p| p.as_str());
            c.allowed_profiles = allowed;
            // A default that is no longer allowed is dropped (never a silent fallback).
            if c.default_profile
                .is_some_and(|d| !c.allowed_profiles.contains(&d))
            {
                c.default_profile = None;
            }
        }
        if let Some(d) = o.default_profile {
            if !c.allowed_profiles.contains(&d) {
                return Err(format!(
                    "{}: defaultProfile must be one of allowedProfiles",
                    class.as_str()
                ));
            }
            c.default_profile = Some(d);
        }
        if o.max_members.is_some() {
            c.max_members = o.max_members;
        }
        if o.max_lifetime_seconds.is_some() {
            c.max_lifetime_seconds = o.max_lifetime_seconds;
        }
        if let Some(r) = o.reopen_allowed {
            c.reopen_allowed = r;
        }
        c.legal_hold_eligible = c.allowed_profiles.contains(&SecurityProfile::Managed);
        c.sms_bridge_eligible = false;
    }
    Ok(p)
}

/// JSON with recursively sorted object keys (independent of serde_json feature flags).
pub fn canonical_json(value: &Value) -> String {
    fn write(v: &Value, out: &mut String) {
        match v {
            Value::Object(map) => {
                let mut keys: Vec<_> = map.keys().collect();
                keys.sort();
                out.push('{');
                for (i, k) in keys.iter().enumerate() {
                    if i > 0 {
                        out.push(',');
                    }
                    out.push_str(&serde_json::to_string(k).expect("string"));
                    out.push(':');
                    write(&map[*k], out);
                }
                out.push('}');
            }
            Value::Array(items) => {
                out.push('[');
                for (i, item) in items.iter().enumerate() {
                    if i > 0 {
                        out.push(',');
                    }
                    write(item, out);
                }
                out.push(']');
            }
            other => out.push_str(&serde_json::to_string(other).expect("scalar")),
        }
    }
    let mut out = String::new();
    write(value, &mut out);
    out
}

impl EffectivePolicy {
    pub fn to_value(&self) -> Value {
        serde_json::to_value(self).expect("policy serializes")
    }

    /// SHA-256 (hex) of the canonical document.
    pub fn digest(&self) -> String {
        hex::encode(Sha256::digest(canonical_json(&self.to_value()).as_bytes()))
    }

    pub fn context(&self, class: ContextClass) -> &ContextPolicy {
        &self.contexts[&class]
    }
}

#[derive(Debug, Clone, FromQueryResult)]
struct PolicyRow {
    version: i64,
    document: Value,
}

/// The application environment's stored overrides and their version (0 = none).
pub async fn load_application_policy<C: ConnectionTrait>(
    db: &C,
    environment_id: Uuid,
    lock: bool,
) -> Result<(ApplicationPolicy, i64)> {
    let row: Option<PolicyRow> = db::one(
        db,
        &format!(
            "SELECT version, document FROM messaging_application_policies WHERE environment_id = $1{}",
            if lock { " FOR UPDATE" } else { "" }
        ),
        vec![environment_id.into()],
    )
    .await?;
    match row {
        None => Ok((ApplicationPolicy::default(), 0)),
        Some(r) => {
            let policy = serde_json::from_value(r.document)
                .map_err(|_| Error::code(ErrorCode::InternalError))?;
            Ok((policy, r.version))
        }
    }
}

/// A stored snapshot.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Snapshot {
    pub id: Uuid,
    pub digest: String,
    pub policy: EffectivePolicy,
}

#[derive(FromQueryResult)]
struct SnapshotId {
    id: Uuid,
}

/// The current effective policy of `environment_id`, stored as an immutable snapshot
/// (created once per distinct document; concurrent callers converge on one row).
pub async fn current_snapshot<C: ConnectionTrait>(
    db: &C,
    environment_id: Uuid,
    now: DateTime<Utc>,
) -> Result<Snapshot> {
    let (overrides, version) = load_application_policy(db, environment_id, false).await?;
    let policy = resolve(&overrides, version).map_err(|_| Error::code(ErrorCode::InternalError))?;
    let digest = policy.digest();
    db::exec(
        db,
        "INSERT INTO messaging_policy_snapshots (id, environment_id, digest, platform_policy_version, \
           application_policy_version, document, created_at) VALUES ($1, $2, $3, $4, $5, $6, $7) \
         ON CONFLICT (environment_id, digest) DO NOTHING",
        vec![
            Uuid::new_v4().into(),
            environment_id.into(),
            digest.clone().into(),
            PLATFORM_POLICY_VERSION.into(),
            version.into(),
            policy.to_value().into(),
            now.into(),
        ],
    )
    .await?;
    let row: SnapshotId = db::one(
        db,
        "SELECT id FROM messaging_policy_snapshots WHERE environment_id = $1 AND digest = $2",
        vec![environment_id.into(), digest.clone().into()],
    )
    .await?
    .ok_or(Error::code(ErrorCode::InternalError))?;
    Ok(Snapshot {
        id: row.id,
        digest,
        policy,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn platform_defaults_are_e2ee_where_eligible_and_never_managed() {
        let p = platform_defaults();
        assert_eq!(p.contexts.len(), 8);
        for (class, c) in &p.contexts {
            assert!(
                !c.allowed_profiles.contains(&SecurityProfile::Managed),
                "{class:?}"
            );
            assert!(!c.sms_bridge_eligible && !c.legal_hold_eligible);
            if *class == ContextClass::Support {
                assert_eq!(
                    c.default_profile, None,
                    "support needs an explicit application choice"
                );
            } else {
                assert_eq!(c.default_profile, Some(SecurityProfile::PrivateE2ee));
            }
        }
        assert_eq!(p.context(ContextClass::Direct).max_members, Some(2));
    }

    #[test]
    fn resolution_is_deterministic_and_validated() {
        let mut o = ApplicationPolicy::default();
        o.contexts.insert(
            ContextClass::Enterprise,
            ContextOverride {
                allowed_profiles: Some(vec![
                    SecurityProfile::Managed,
                    SecurityProfile::PrivateE2ee,
                ]),
                default_profile: Some(SecurityProfile::Managed),
                ..Default::default()
            },
        );
        let a = resolve(&o, 3).unwrap();
        let b = resolve(&o, 3).unwrap();
        assert_eq!(a.digest(), b.digest());
        assert_ne!(a.digest(), resolve(&o, 4).unwrap().digest());
        assert_ne!(a.digest(), platform_defaults().digest());
        let e = a.context(ContextClass::Enterprise);
        assert_eq!(e.default_profile, Some(SecurityProfile::Managed));
        assert!(e.legal_hold_eligible && !e.sms_bridge_eligible);
        // Profile order in the override does not change the digest.
        o.contexts
            .get_mut(&ContextClass::Enterprise)
            .unwrap()
            .allowed_profiles = Some(vec![SecurityProfile::PrivateE2ee, SecurityProfile::Managed]);
        assert_eq!(resolve(&o, 3).unwrap().digest(), a.digest());
        // Removing E2EE from a context drops its default instead of falling back.
        let mut only_managed = ApplicationPolicy::default();
        only_managed.contexts.insert(
            ContextClass::Group,
            ContextOverride {
                allowed_profiles: Some(vec![SecurityProfile::Managed]),
                ..Default::default()
            },
        );
        assert_eq!(
            resolve(&only_managed, 1)
                .unwrap()
                .context(ContextClass::Group)
                .default_profile,
            None
        );
        // Invalid overrides.
        for bad in [
            ContextOverride {
                default_profile: Some(SecurityProfile::Managed),
                ..Default::default()
            },
            ContextOverride {
                max_members: Some(1),
                ..Default::default()
            },
            ContextOverride {
                max_lifetime_seconds: Some(5),
                ..Default::default()
            },
            ContextOverride {
                allowed_profiles: Some(vec![SecurityProfile::Managed, SecurityProfile::Managed]),
                ..Default::default()
            },
        ] {
            let mut p = ApplicationPolicy::default();
            p.contexts.insert(ContextClass::Group, bad.clone());
            assert!(resolve(&p, 1).is_err(), "{bad:?}");
        }
        let mut direct = ApplicationPolicy::default();
        direct.contexts.insert(
            ContextClass::Direct,
            ContextOverride {
                max_members: Some(10),
                ..Default::default()
            },
        );
        assert!(resolve(&direct, 1).is_err());
    }

    #[test]
    fn canonical_json_sorts_keys_recursively() {
        let a: Value =
            serde_json::from_str(r#"{"b":1,"a":{"d":[{"z":1,"y":2}],"c":null}}"#).unwrap();
        assert_eq!(
            canonical_json(&a),
            r#"{"a":{"c":null,"d":[{"y":2,"z":1}]},"b":1}"#
        );
    }
}
