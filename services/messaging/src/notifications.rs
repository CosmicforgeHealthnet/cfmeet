//! The Notification Service's owner API of Messaging (contract
//! `packages/contracts/messaging/notification-owner/v1.openapi.json`, X3 of Document 4
//! §3.4). Messaging stays the authority for membership, devices and E2EE: it authorizes
//! every message wake-up the Notification Service sends.
//!
//! | Route | Permission |
//! |---|---|
//! | `POST /internal/v1/notifications/wakeups/authorize` (`authorizeWakeups`) | `messaging-wakeups.authorize` (grantable to `cfmeet.notifications` only) |
//!
//! The answer is [`push::authorize_wakeups`], the same decision push job creation and
//! dispatch use (no second copy of the rules), read from the canonical tables at request
//! time. It carries ids, the conversation's profile / kind / state and the mention flag
//! only: never content, ciphertext, keys or message metadata beyond the contract.
//! Read-only; the service token is single-use (POST), as on every internal route.

use std::collections::HashMap;

use axum::{extract::State, routing::post, Json, Router};
use cf_service_auth::InternalPermission;
use serde::Deserialize;
use serde_json::{json, Value};
use uuid::Uuid;

use crate::{
    error::{Error, ErrorCode},
    http::{error::ApiError, middleware::InternalCaller},
    identity::{EnvironmentKind, OwnerScope},
    realtime::push::{self, WakeupAuthorization},
    state::AppState,
};

/// `authorizeWakeups`, nested under `/internal/v1`.
pub const AUTHORIZE_WAKEUPS: &str = "/notifications/wakeups/authorize";

/// Most principals in `recheckPrincipals` (contract `maxItems`).
pub const MAX_RECHECK: usize = 200;

/// Most entries in `entitled` (contract `maxItems`). A larger audience is refused
/// (`CONFLICT` / `WAKEUP_RECIPIENT_LIMIT`), never truncated: a silently shortened list
/// would drop wake-ups. The caller can still recheck known principals in batches.
pub const MAX_ENTITLED: usize = 1000;

/// The first-party account reference prefix (`AccountRef` of the notifications common
/// schema).
pub const ACCOUNT_REF_PREFIX: &str = "cfmeet-user:";

/// Routes nested under `/internal/v1`.
pub fn routes() -> Router<AppState> {
    Router::new().route(AUTHORIZE_WAKEUPS, post(authorize_wakeups))
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AuthorizeWakeups {
    pub conversation_id: Uuid,
    pub event_sequence: i64,
    /// The sender the caller saw on the event; empty: Messaging takes it from the event.
    pub sender_principal: String,
    #[serde(default)]
    pub recheck_principals: Option<Vec<Uuid>>,
}

/// `cfmeet-user:<uuid>` exactly (lower-case, hyphenated), else `None`.
pub fn account_ref(reference: &str) -> Option<String> {
    let id = reference.strip_prefix(ACCOUNT_REF_PREFIX)?;
    let parsed = Uuid::parse_str(id).ok()?;
    (parsed.hyphenated().to_string() == id).then(|| reference.to_owned())
}

/// `conversationKind`: DIRECT, or GROUP for every multi-party context class (the DIRECT
/// block rule applies to DIRECT only).
pub fn conversation_kind(context_class: &str) -> &'static str {
    if context_class == "DIRECT" {
        "DIRECT"
    } else {
        "GROUP"
    }
}

async fn authorize_wakeups(
    State(state): State<AppState>,
    caller: InternalCaller,
    Json(body): Json<AuthorizeWakeups>,
) -> Result<Json<Value>, ApiError> {
    caller.require_permission(InternalPermission::MessagingWakeupsAuthorize)?;
    Ok(Json(authorize(&state, body).await?))
}

/// The `authorizeWakeups` answer.
pub async fn authorize(state: &AppState, request: AuthorizeWakeups) -> Result<Value, Error> {
    if request.event_sequence < 1 {
        return Err(Error::validation("eventSequence must be at least 1"));
    }
    let claimed_sender = match request.sender_principal.as_str() {
        "" => None,
        s => Some(
            Uuid::parse_str(s)
                .map_err(|_| Error::validation("senderPrincipal must be a UUID or empty"))?,
        ),
    };
    if request
        .recheck_principals
        .as_ref()
        .is_some_and(|p| p.len() > MAX_RECHECK)
    {
        return Err(Error::validation(format!(
            "at most {MAX_RECHECK} recheckPrincipals"
        )));
    }
    let decision = push::authorize_wakeups(
        &state.db,
        request.conversation_id,
        request.event_sequence,
        request.recheck_principals.as_deref(),
    )
    .await?
    .ok_or(Error::code(ErrorCode::NotFound))?;
    // The event's recorded sender is authoritative: a different claimed sender names no
    // event Messaging knows.
    if claimed_sender.is_some_and(|claimed| decision.sender != Some(claimed)) {
        return Err(Error::code(ErrorCode::NotFound));
    }
    if decision.entitled.len() > MAX_ENTITLED {
        return Err(Error::reason(ErrorCode::Conflict, "WAKEUP_RECIPIENT_LIMIT"));
    }
    let accounts = account_refs(state, &decision).await?;
    let entitled: Vec<Value> = decision
        .entitled
        .iter()
        .map(|e| {
            json!({
                "principal": e.principal_id,
                "accountRef": accounts.get(&e.principal_id),
                "mentionsRecipient": e.mentions_recipient,
            })
        })
        .collect();
    Ok(json!({
        "conversationActive": decision.conversation_active,
        "securityProfile": decision.security_profile,
        "conversationKind": conversation_kind(&decision.context_class),
        "entitled": entitled,
    }))
}

/// `MESSAGING_ORGANIZATION_WAKEUPS` (gap audit S1): `off` (default: unchanged behaviour)
/// or `on`.
pub const ORGANIZATION_WAKEUPS_ENV: &str = "MESSAGING_ORGANIZATION_WAKEUPS";

pub fn organization_wakeups_from_env(get: impl Fn(&str) -> Option<String>) -> Result<bool, String> {
    match get(ORGANIZATION_WAKEUPS_ENV).as_deref().map(str::trim) {
        None | Some("") | Some("off") => Ok(false),
        Some("on") => Ok(true),
        Some(_) => Err(format!("{ORGANIZATION_WAKEUPS_ENV} must be off or on")),
    }
}

/// Gap audit S1: whether `decision`'s conversation lives in an organization's own
/// first-party scope (the event's authenticated tenant classification was FIRST_PARTY and
/// the scope is not CF Meet's personal scope) while `MESSAGING_ORGANIZATION_WAKEUPS` is on.
/// There, as in the personal scope, a NATIVE_CF_USER principal whose reference is
/// `cfmeet-user:<uuid>` is the member's CF Meet account (the Meeting Service registers
/// organization members that way).
pub(crate) fn organization_scope(state: &AppState, decision: &WakeupAuthorization) -> bool {
    let Some(policy) = state.communications.as_ref() else {
        return false;
    };
    state.organization_wakeups
        && decision.first_party
        && !policy.governs(cf_communications_policy::Scope {
            tenant_id: decision.tenant_id,
            application_id: decision.application_id,
            environment_id: decision.environment_id,
        })
}

/// The first-party account references of the entitled principals. Only in the governed
/// personal scope (CF Meet's own first-party environment, where Messaging already treats
/// NATIVE_CF_USER principals as CF Meet accounts) and only for ACTIVE NATIVE_CF_USER
/// principals whose Platform Access reference is `cfmeet-user:<uuid>` (set by the account
/// authority, Meeting). Platform Access is asked inside the event's own authenticated
/// scope (its authority references, as for delayed policy checks). Anything else: `null`.
/// Platform Access unavailable: 503 (a missing reference would misroute the wake-up).
pub(crate) async fn account_refs(
    state: &AppState,
    decision: &WakeupAuthorization,
) -> Result<HashMap<Uuid, String>, Error> {
    if decision.entitled.is_empty() {
        return Ok(HashMap::new());
    }
    let personal = cf_communications_policy::Scope {
        tenant_id: decision.tenant_id,
        application_id: decision.application_id,
        environment_id: decision.environment_id,
    };
    let governed = state
        .communications
        .as_ref()
        .is_some_and(|p| p.governs(personal));
    if !governed && !organization_scope(state, decision) {
        return Ok(HashMap::new());
    }
    let (Some(service_account_id), Some(environment_kind)) = (
        decision.initiating_service_account_id,
        decision
            .environment_kind
            .as_deref()
            .and_then(EnvironmentKind::parse),
    ) else {
        return Ok(HashMap::new());
    };
    let scope = OwnerScope {
        tenant_id: decision.tenant_id,
        application_id: decision.application_id,
        environment_id: decision.environment_id,
        environment_kind,
        service_account_id,
    };
    let principals: Vec<Uuid> = decision.entitled.iter().map(|e| e.principal_id).collect();
    let references = state
        .principals
        .native_user_references(&scope, &principals)
        .await
        .map_err(|e| {
            tracing::warn!(error = %e, "principal directory unavailable for wake-up authorization");
            Error::code(ErrorCode::ServiceUnavailable)
        })?;
    Ok(references
        .into_iter()
        .filter(|(p, _)| principals.contains(p))
        .filter_map(|(p, r)| account_ref(&r).map(|r| (p, r)))
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn account_refs_are_exactly_cfmeet_user_uuids() {
        let id = Uuid::new_v4();
        assert_eq!(
            account_ref(&format!("cfmeet-user:{id}")),
            Some(format!("cfmeet-user:{id}"))
        );
        for bad in [
            format!("cfmeet-user:{}", id.to_string().to_uppercase()),
            format!("cfmeet-user:{}", id.simple()),
            format!("cfmeet-participant:{id}"),
            format!("{id}"),
            "cfmeet-user:".into(),
            format!("cfmeet-user:{id}x"),
        ] {
            assert_eq!(account_ref(&bad), None, "{bad}");
        }
    }

    #[test]
    fn the_organization_wakeup_switch_is_off_by_default() {
        let env = |v: Option<&'static str>| {
            move |k: &str| {
                (k == ORGANIZATION_WAKEUPS_ENV)
                    .then(|| v.map(str::to_string))
                    .flatten()
            }
        };
        assert_eq!(organization_wakeups_from_env(env(None)), Ok(false));
        assert_eq!(organization_wakeups_from_env(env(Some("off"))), Ok(false));
        assert_eq!(organization_wakeups_from_env(env(Some("on"))), Ok(true));
        assert!(organization_wakeups_from_env(env(Some("true"))).is_err());
    }

    #[test]
    fn every_multi_party_context_is_a_group() {
        assert_eq!(conversation_kind("DIRECT"), "DIRECT");
        for c in ["GROUP", "MEETING", "SUPPORT", "DEVELOPER_DEFINED"] {
            assert_eq!(conversation_kind(c), "GROUP");
        }
    }
}
