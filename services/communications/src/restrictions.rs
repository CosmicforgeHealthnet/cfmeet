//! X4 `checkCommunicationRestrictions` (contract `communications/notification-owner/v1`,
//! Notification Service only, `communications-restrictions.read`).
//!
//! One batch read of the Hub's own restriction state between an optional actor and up to
//! 100 notification recipients, replacing the per-recipient `policy/check` round trips for
//! notifications (Document 4 §7.2 rules 2, 4 and 6). Read-only; account references go in,
//! three booleans per recipient come out (no profile, privacy value or principal id).
//!
//! The answer reuses the Hub's existing decisions in the delegated first-party scope (the
//! signed context is required and validated with Platform Access, as for every scoped Hub
//! route):
//! * `blocked` — an active block in either direction in `communication_block_bridge`
//!   (Hub blocks and the Calling / Messaging domain blocks they propagate), exactly the
//!   check of [`crate::store::Store::blocked`]. The live per-pair owner re-query of
//!   `policy/check` is not repeated here: it would cost two owner calls per recipient,
//!   and this answer only filters notifications, it authorizes no communication.
//! * `privacyDenied` — the recipient's privacy settings refuse the actor for the action,
//!   through [`crate::policy::decide`] (the `policy/check` rule) with the pair's
//!   CONNECTED state: `CONTACT_REQUEST` → contact requests, `MESSAGE` → direct-message
//!   notifications, `CALL` → direct calls, `GROUP_INVITATION` → group invitations.
//!   `CONTACT_CONNECTED` (a mutual connection) and `OTHER` have no privacy rule (`OTHER`
//!   matches the Hub's group-message notification rule, which needs no personal
//!   contact). A recipient or actor without a profile is evaluated as not connected
//!   with default privacy, as `policy/check` refuses unknown principals. Notification
//!   preferences and mutes are not privacy and are not consulted.
//! * `operatorHold` — an active `communication_operator_holds` row on the recipient
//!   (`/admin/suspend`), whatever the actor.
//!
//! Without an actor, `blocked` and `privacyDenied` are false. An actor that is also a
//! recipient is never blocked or privacy-denied against itself.
use crate::{
    auth,
    error::{Error, Result},
    model::PrivacyRow,
    policy::decide,
    preferences::valid_reference,
    store, State,
};
use axum::{extract::State as HttpState, http::HeaderValue, response::IntoResponse, Extension};
use cf_communications_policy::{Action, Scope};
use cf_service_auth::{DelegatedPlatformContext, InternalServiceContext, ServiceId};
use serde::Deserialize;
use serde_json::{json, Value};
use std::collections::{BTreeMap, BTreeSet};
use uuid::Uuid;

/// Recipients per check (contract `maxItems`).
pub const MAX_RECIPIENTS: usize = 100;

/// The notification's action (contract enum).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum RestrictionAction {
    ContactRequest,
    ContactConnected,
    Message,
    Call,
    GroupInvitation,
    Other,
}

impl RestrictionAction {
    /// The Hub policy action whose privacy rule applies, if any.
    fn policy_action(self) -> Option<Action> {
        match self {
            Self::ContactRequest => Some(Action::ContactRequest),
            Self::Message => Some(Action::MessageNotification),
            Self::Call => Some(Action::DirectCall),
            Self::GroupInvitation => Some(Action::GroupInvitation),
            Self::ContactConnected | Self::Other => None,
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RestrictionCheck {
    #[serde(default)]
    pub actor_ref: Option<String>,
    pub recipient_refs: Vec<String>,
    pub action: RestrictionAction,
}

/// One recipient's restrictions.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Restriction {
    pub recipient_ref: String,
    pub blocked: bool,
    pub privacy_denied: bool,
    pub operator_hold: bool,
}

impl Restriction {
    pub fn to_json(&self) -> Value {
        json!({"recipientRef":self.recipient_ref,"blocked":self.blocked,
            "privacyDenied":self.privacy_denied,"operatorHold":self.operator_hold})
    }
}

/// Bounded, canonical, duplicate-free input.
pub fn validate(check: &RestrictionCheck) -> Result<()> {
    let unique: BTreeSet<&str> = check.recipient_refs.iter().map(String::as_str).collect();
    if unique.is_empty()
        || unique.len() != check.recipient_refs.len()
        || unique.len() > MAX_RECIPIENTS
        || !unique.iter().all(|r| valid_reference(r))
        || check
            .actor_ref
            .as_deref()
            .is_some_and(|r| !valid_reference(r))
    {
        return Err(Error::Validation);
    }
    Ok(())
}

/// The pair facts read for one recipient profile.
#[derive(Debug, Deserialize)]
struct RecipientFacts {
    #[serde(rename = "ref")]
    reference: String,
    id: Uuid,
    held: bool,
    blocked: bool,
    connected: bool,
    privacy: Option<PrivacyRow>,
}

impl State {
    /// X4: per recipient, in request order.
    pub async fn communication_restrictions(
        &self,
        scope: &DelegatedPlatformContext,
        check: &RestrictionCheck,
    ) -> Result<Vec<Restriction>> {
        validate(check)?;
        auth::validate_scope(self, scope).await?;
        let s = Scope::of(scope);
        let actor: Option<Uuid> = match check.actor_ref.as_deref() {
            None => None,
            Some(r) => {
                let mut v = store::scoped(s);
                v.push(r.into());
                let row: Option<Value> = store::one(&self.store.db,
                    "SELECT jsonb_build_object('id',id) doc FROM communication_profiles WHERE tenant_id=$1 AND application_id=$2 AND environment_id=$3 AND account_reference=$4",
                    v).await?;
                row.map(|r| {
                    r["id"]
                        .as_str()
                        .and_then(|id| Uuid::parse_str(id).ok())
                        .ok_or(Error::Unavailable)
                })
                .transpose()?
            }
        };
        let mut v = store::scoped(s);
        v.push(json!(check.recipient_refs).into());
        v.push(actor.into());
        let rows: Vec<RecipientFacts> = store::rows(
            &self.store.db,
            "SELECT jsonb_build_object(\
                'ref', r.account_reference, 'id', r.id,\
                'held', EXISTS (SELECT 1 FROM communication_operator_holds h WHERE h.profile_id = r.id AND h.active),\
                'blocked', $5::uuid IS NOT NULL AND EXISTS (SELECT 1 FROM communication_block_bridge b WHERE b.active \
                    AND ((b.blocker_profile_id = $5 AND b.blocked_profile_id = r.id) OR (b.blocker_profile_id = r.id AND b.blocked_profile_id = $5))),\
                'connected', $5::uuid IS NOT NULL AND EXISTS (SELECT 1 FROM communication_connections c \
                    WHERE c.tenant_id = $1 AND c.application_id = $2 AND c.environment_id = $3 \
                      AND c.left_profile_id = LEAST(r.id, $5) AND c.right_profile_id = GREATEST(r.id, $5) AND c.state = 'CONNECTED'),\
                'privacy', (SELECT to_jsonb(p) FROM communication_privacy p WHERE p.profile_id = r.id)) doc \
               FROM communication_profiles r \
              WHERE r.tenant_id = $1 AND r.application_id = $2 AND r.environment_id = $3 \
                AND r.account_reference IN (SELECT jsonb_array_elements_text($4::jsonb))",
            v,
        )
        .await?;
        let facts: BTreeMap<String, RecipientFacts> =
            rows.into_iter().map(|f| (f.reference.clone(), f)).collect();
        Ok(check
            .recipient_refs
            .iter()
            .map(|reference| {
                let f = facts.get(reference);
                let held = f.is_some_and(|f| f.held);
                let is_self = actor.is_some() && f.is_some_and(|f| Some(f.id) == actor);
                let (blocked, privacy_denied) = if check.actor_ref.is_none() || is_self {
                    (false, false)
                } else {
                    let connected = f.is_some_and(|f| f.connected);
                    let privacy = f
                        .and_then(|f| f.privacy.as_ref())
                        .map(PrivacyRow::settings)
                        .unwrap_or_default();
                    (
                        f.is_some_and(|f| f.blocked),
                        check
                            .action
                            .policy_action()
                            .is_some_and(|a| !decide(a, connected, &privacy)),
                    )
                };
                Restriction {
                    recipient_ref: reference.clone(),
                    blocked,
                    privacy_denied,
                    operator_hold: held,
                }
            })
            .collect())
    }
}

/// `POST /internal/v1/communications/restrictions/check` — X4, Notification Service only.
pub async fn check(
    HttpState(s): HttpState<State>,
    Extension(ctx): Extension<InternalServiceContext>,
    axum::Json(input): axum::Json<RestrictionCheck>,
) -> Result<axum::response::Response> {
    if ctx.authenticated_service() != &ServiceId::notifications() {
        return Err(Error::Forbidden);
    }
    let scope = auth::delegated(&ctx)?;
    let results = s.communication_restrictions(&scope, &input).await?;
    let results: Vec<Value> = results.iter().map(Restriction::to_json).collect();
    let mut response = axum::Json(json!({ "results": results })).into_response();
    response
        .headers_mut()
        .insert("cache-control", HeaderValue::from_static("no-store"));
    Ok(response)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn input(actor: Option<String>, recipients: Vec<String>) -> RestrictionCheck {
        RestrictionCheck {
            actor_ref: actor,
            recipient_refs: recipients,
            action: RestrictionAction::Message,
        }
    }

    fn r() -> String {
        format!("cfmeet-user:{}", Uuid::new_v4())
    }

    #[test]
    fn batches_are_bounded_canonical_and_unique() {
        assert!(validate(&input(None, vec![r()])).is_ok());
        assert!(validate(&input(Some(r()), (0..100).map(|_| r()).collect())).is_ok());
        assert!(validate(&input(None, vec![])).is_err());
        assert!(validate(&input(None, (0..101).map(|_| r()).collect())).is_err());
        let one = r();
        assert!(validate(&input(None, vec![one.clone(), one])).is_err());
        assert!(validate(&input(None, vec!["someone@example.com".into()])).is_err());
        assert!(validate(&input(Some("cfmeet-user:nope".into()), vec![r()])).is_err());
    }

    #[test]
    fn only_privacy_bearing_actions_map_to_a_policy_rule() {
        assert_eq!(
            RestrictionAction::Call.policy_action(),
            Some(Action::DirectCall)
        );
        assert_eq!(
            RestrictionAction::Message.policy_action(),
            Some(Action::MessageNotification)
        );
        assert_eq!(RestrictionAction::ContactConnected.policy_action(), None);
        assert_eq!(RestrictionAction::Other.policy_action(), None);
    }
}
