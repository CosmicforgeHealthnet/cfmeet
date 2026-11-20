//! A5 chat policy: `messaging.policy.read` (R1) and `messaging.policy.change` (R3).
//!
//! The policy is per application environment, so both name it (`environmentId`, with its
//! `tenantId`; an environment Messaging does not know for that tenant is 404). The read
//! returns the environment's own override document (what a change replaces), its version
//! (0: none stored yet), the code-defined baseline and the effective policy: flags,
//! limits, enums and counts only (the policy holds no personal data and no content).
//!
//! A change replaces the override document at `expectedVersion` through the same path as
//! the application's own `PUT /v1/messaging/policy` (validation, deployment availability,
//! audit entry, `messaging.policy.updated` event). The security posture stays the
//! application's own decision: a change from the bridge that alters any context's
//! `allowedProfiles`, `defaultProfile` or `centralArchiveRequired` is refused (422
//! `SECURITY_PROFILE_NOT_ALLOWED` / `SECURITY_POSTURE_OWNER_ONLY`), so HQ can never move a
//! context away from end-to-end encryption. Attachments on / off, moderation, retention,
//! privacy signals and abuse limits are changeable.

use std::collections::BTreeSet;

use axum::{
    extract::{
        rejection::{JsonRejection, QueryRejection},
        Query, State,
    },
    http::HeaderMap,
    Json,
};
use serde::Deserialize;
use serde_json::{json, Value};
use uuid::Uuid;

use super::{begin, body, environment_scope, finish, reason, BridgeCommand, Outcome, Subject};
use crate::{
    conversations::{
        policy::{self, ApplicationPolicy, ContextOverride},
        service::{self, request_context, rfc3339, PolicyUpdate},
    },
    error::{Error, ErrorCode},
    http::{error::ApiError, middleware::InternalCaller},
    operator_reads::require_bridge,
    state::AppState,
};

pub const CHANGE_POLICY: &str = "messaging.policy.change";

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PolicyQuery {
    pub tenant_id: Uuid,
    pub environment_id: Uuid,
}

/// `GET /operator/policy?tenantId&environmentId`.
pub async fn read(
    State(state): State<AppState>,
    caller: InternalCaller,
    raw: Result<Query<PolicyQuery>, QueryRejection>,
) -> Result<Json<Value>, ApiError> {
    require_bridge(&caller)?;
    let Query(q) = raw.map_err(|_| {
        Error::validation("tenantId and environmentId (UUIDs) are required, nothing else")
    })?;
    let scope = environment_scope(&state.db, q.environment_id, q.tenant_id).await?;
    let (overrides, version) =
        policy::load_application_policy(&state.db, q.environment_id, false).await?;
    let baseline = policy::load_baseline(&state.db, q.environment_id).await?;
    let effective = policy::resolve(&policy::layer(baseline, &overrides), version)
        .map_err(|_| Error::code(ErrorCode::InternalError))?;
    Ok(Json(json!({
        "tenantId": scope.tenant_id,
        "applicationId": scope.application_id,
        "environmentId": scope.environment_id,
        "version": version,
        "policy": overrides,
        "applicationBaseline": baseline.map(|b| b.as_str()),
        "effective": effective.to_value(),
        "digest": effective.digest(),
        "platformPolicyVersion": policy::PLATFORM_POLICY_VERSION,
    })))
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ChangePolicy {
    pub tenant_id: Uuid,
    pub environment_id: Uuid,
    /// The override document's version (0: none stored yet).
    pub expected_version: i64,
    pub reason_code: String,
    /// The complete new override document (what `GET` returned as `policy`, changed).
    pub policy: ApplicationPolicy,
}

/// The security-relevant settings of one context override.
type Posture = (
    Option<Vec<crate::domain::SecurityProfile>>,
    Option<crate::domain::SecurityProfile>,
    Option<bool>,
);

fn posture(o: Option<&ContextOverride>) -> Posture {
    o.map(|o| {
        (
            o.allowed_profiles.clone(),
            o.default_profile,
            o.central_archive_required,
        )
    })
    .unwrap_or((None, None, None))
}

/// Refuses a document that changes any context's security posture.
pub(crate) fn posture_unchanged(
    current: &ApplicationPolicy,
    next: &ApplicationPolicy,
) -> Result<(), ApiError> {
    let classes: BTreeSet<_> = current
        .contexts
        .keys()
        .chain(next.contexts.keys())
        .collect();
    for class in classes {
        if posture(current.contexts.get(class)) != posture(next.contexts.get(class)) {
            return Err(Error::reason(
                ErrorCode::SecurityProfileNotAllowed,
                "SECURITY_POSTURE_OWNER_ONLY",
            ));
        }
    }
    Ok(())
}

/// `POST /operator/policy` — `messaging.policy.change`.
pub async fn change(
    State(state): State<AppState>,
    caller: InternalCaller,
    headers: HeaderMap,
    raw: Result<Json<ChangePolicy>, JsonRejection>,
) -> Result<Json<Value>, ApiError> {
    let cmd = BridgeCommand::of(&caller, &headers, true)?;
    let req = body(raw)?;
    if req.expected_version < 0 {
        return Err(Error::validation(
            "expectedVersion must be the policy's version (0 when none is stored)",
        ));
    }
    reason(&req.reason_code)?;
    let subject = Subject::new("MESSAGING_POLICY", req.environment_id);
    let now = state.now();
    let txn = sea_orm::TransactionTrait::begin(&state.db).await?;
    if let Some(done) = begin(&txn, &cmd, CHANGE_POLICY, &subject).await? {
        return Ok(Json(done));
    }
    let scope = environment_scope(&txn, req.environment_id, req.tenant_id).await?;
    let ctx = scope.context();
    let update = PolicyUpdate {
        expected_version: req.expected_version,
        policy: req.policy,
    };
    let resolved = service::check_policy_update(&state, req.environment_id, &update).await?;
    let (current, _) = policy::load_application_policy(&txn, req.environment_id, true).await?;
    posture_unchanged(&current, &update.policy)?;
    let version =
        service::write_policy_in(&txn, &ctx, &update, &resolved, now, &request_context()).await?;
    let outcome = Outcome {
        state: "UPDATED",
        version: Some(version),
        resource: json!({
            "tenantId": scope.tenant_id,
            "applicationId": scope.application_id,
            "environmentId": scope.environment_id,
            "digest": resolved.digest(),
            "platformPolicyVersion": policy::PLATFORM_POLICY_VERSION,
            "updatedAt": rfc3339(now),
        }),
    };
    let answer = finish(&txn, &cmd, CHANGE_POLICY, &subject, outcome, now).await?;
    txn.commit().await?;
    state.wake_outbox();
    Ok(Json(answer))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn doc(v: Value) -> ApplicationPolicy {
        serde_json::from_value(v).unwrap()
    }

    #[test]
    fn the_bridge_never_changes_the_security_posture() {
        let current = doc(json!({
            "contexts": { "GROUP": { "allowedProfiles": ["PRIVATE_E2EE"], "maxMembers": 50 } },
            "attachments": { "enabled": true }
        }));
        // Attachments off and a member limit: allowed.
        let next = doc(json!({
            "contexts": { "GROUP": { "allowedProfiles": ["PRIVATE_E2EE"], "maxMembers": 20 } },
            "attachments": { "enabled": false }
        }));
        assert!(posture_unchanged(&current, &next).is_ok());
        for bad in [
            json!({ "contexts": { "GROUP": { "allowedProfiles": ["MANAGED", "PRIVATE_E2EE"] } } }),
            json!({ "contexts": { "GROUP": { "allowedProfiles": ["PRIVATE_E2EE"], "defaultProfile": "PRIVATE_E2EE" } } }),
            json!({ "contexts": { "GROUP": { "allowedProfiles": ["PRIVATE_E2EE"] }, "SUPPORT": { "centralArchiveRequired": true } } }),
            json!({}),
        ] {
            let e = posture_unchanged(&current, &doc(bad.clone())).unwrap_err();
            assert_eq!(
                e.error_code(),
                ErrorCode::SecurityProfileNotAllowed,
                "{bad}"
            );
        }
    }
}
