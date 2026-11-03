use crate::{
    auth,
    error::{Error, Result},
    model::*,
    store::{self},
    State,
};
use cf_communications_policy::{Action, Decision, DecisionReason, PolicyCheck};
use cf_service_auth::{DelegatedPlatformContext, OutboundServiceRequest, ServiceId};
use serde_json::{json, Value};

impl State {
    pub async fn domain_blocks(
        &self,
        scope: &DelegatedPlatformContext,
        a: &Profile,
        b: &Profile,
    ) -> Result<bool> {
        // Fresh owner queries preserve legacy domain blocks and close propagation races.
        // These read-only adapters never call this policy endpoint recursively.
        for service in [ServiceId::calling(), ServiceId::messaging()] {
            let req = OutboundServiceRequest::new_operation(service.clone())
                .with_delegated_context(scope.clone());
            let r = self
                .config
                .client
                .post_json(
                    &req,
                    "/internal/v1/communications/blocks/check",
                    &json!({"actorPrincipalId":a.principal_id,"targetPrincipalId":b.principal_id}),
                )
                .await
                .map_err(|_| Error::Unavailable)?;
            if r.status() != 200 {
                return Err(Error::Unavailable);
            }
            let v: Value = r.json().map_err(|_| Error::Unavailable)?;
            match v["blocked"].as_bool() {
                Some(true) => return Ok(true),
                Some(false) => {}
                None => return Err(Error::Unavailable),
            }
        }
        Ok(false)
    }
    pub async fn active_account(
        &self,
        scope: &DelegatedPlatformContext,
        p: &Profile,
    ) -> Result<bool> {
        let status = auth::account_status(self, scope, &p.account_reference).await?;
        Ok(status["active"] == true
            && status["version"]
                .as_i64()
                .is_some_and(|v| v <= p.account_version))
    }
    pub async fn check_policy(
        &self,
        scope: &DelegatedPlatformContext,
        check: &PolicyCheck,
    ) -> Result<Decision> {
        auth::validate_scope(self, scope).await?;
        let s = cf_communications_policy::Scope::of(scope);
        let actor = self
            .store
            .principal(&self.store.db, s, check.actor_principal_id)
            .await;
        let target = self
            .store
            .principal(&self.store.db, s, check.target_principal_id)
            .await;
        let deny = |reason, version| Decision {
            schema_version: 1,
            allowed: false,
            reason,
            policy_version: version,
        };
        let (actor, target) = match (actor, target) {
            (Ok(a), Ok(b)) if a.id != b.id => (a, b),
            (Err(Error::NotFound), _) | (_, Err(Error::NotFound)) => {
                return Ok(deny(DecisionReason::NotPermitted, 0))
            }
            (Err(e), _) | (_, Err(e)) => return Err(e),
            _ => return Ok(deny(DecisionReason::NotPermitted, 0)),
        };
        let privacy = self.store.privacy(&self.store.db, target.id).await?;
        let version = actor.version.max(target.version).max(privacy.version);
        if actor.activation_state != Activation::Active
            || target.activation_state != Activation::Active
            || !self.active_account(scope, &actor).await?
            || !self.active_account(scope, &target).await?
        {
            return Ok(deny(DecisionReason::AccountInactive, version));
        }
        if let Some(issued) = check.credential_issued_at {
            let credential_owner = if check.credential_principal_id == Some(actor.principal_id) {
                &actor
            } else if check.credential_principal_id == Some(target.principal_id) {
                &target
            } else {
                return Ok(deny(DecisionReason::NotPermitted, version));
            };
            if credential_owner
                .credentials_not_before
                .is_some_and(|at| issued <= at.timestamp())
            {
                return Ok(deny(DecisionReason::CredentialRevoked, version));
            }
        }
        if self
            .store
            .blocked(&self.store.db, actor.id, target.id)
            .await?
            || self.domain_blocks(scope, &actor, &target).await?
        {
            return Ok(deny(DecisionReason::NotPermitted, version));
        }
        let relationship = self
            .store
            .relationship(&self.store.db, &actor, &target)
            .await?;
        let connected = relationship == "CONNECTED";
        let allowed = decide(check.action, connected, &privacy.settings());
        Ok(Decision {
            schema_version: 1,
            allowed,
            reason: if allowed {
                DecisionReason::Allowed
            } else {
                DecisionReason::NotPermitted
            },
            policy_version: version,
        })
    }
    pub async fn capabilities(&self, scope: &DelegatedPlatformContext, p: &Profile) -> Value {
        let mut calling = json!({"available":false,"reason":"BACKEND_UNAVAILABLE"});
        let mut messaging =
            json!({"available":false,"reason":"BACKEND_UNAVAILABLE","securityReady":false});
        for (service, value) in [
            (ServiceId::calling(), &mut calling),
            (ServiceId::messaging(), &mut messaging),
        ] {
            let request = OutboundServiceRequest::new_operation(service.clone())
                .with_delegated_context(scope.clone());
            if let Ok(r) = self
                .config
                .client
                .post_json(
                    &request,
                    "/internal/v1/communications/capabilities",
                    &json!({"principalId":p.principal_id}),
                )
                .await
            {
                if r.status() == 200 {
                    if let Ok(v) = r.json::<Value>() {
                        let reason = v["reason"]
                            .as_str()
                            .filter(|s| {
                                [
                                    "AVAILABLE",
                                    "DISABLED",
                                    "DEVICE_SECURITY_SETUP_REQUIRED",
                                    "BACKEND_UNAVAILABLE",
                                ]
                                .contains(s)
                            })
                            .unwrap_or("BACKEND_UNAVAILABLE");
                        let available =
                            v["available"].as_bool().unwrap_or(false) && reason == "AVAILABLE";
                        *value = json!({"available":available,"reason":reason});
                        if service == ServiceId::messaging() {
                            let security = v["securityReady"] == true;
                            value["securityReady"] = json!(security);
                            if !security {
                                value["available"] = json!(false);
                            }
                        }
                    }
                }
            }
        }
        if p.activation_state != Activation::Active {
            calling = json!({"available":false,"reason":"ACCOUNT_INACTIVE"});
            messaging =
                json!({"available":false,"reason":"ACCOUNT_INACTIVE","securityReady":false});
        }
        json!({"schemaVersion":1,"activationState":p.activation_state,"personalCalling":calling,"messaging":messaging,
            "contacts":{"available":p.activation_state==Activation::Active},"phoneVerification":{"status":"DEFERRED","required":false},
            "meetings":{"independent":true,"authorization":"EXISTING_MEETING_RULES"}})
    }
    pub async fn visible_profile(
        &self,
        scope: &DelegatedPlatformContext,
        actor: &Profile,
        target: &Profile,
        by_invite: bool,
    ) -> Result<Value> {
        if actor.id == target.id {
            return Ok(store::Store::public_profile(
                self.store
                    .current_public_id(&self.store.db, target.id)
                    .await?,
                target,
                "SELF".into(),
            ));
        }
        if target.activation_state != Activation::Active
            || !self.active_account(scope, target).await?
        {
            return Err(Error::NotFound);
        }
        let privacy = self.store.privacy(&self.store.db, target.id).await?;
        let relationship = self
            .store
            .relationship(&self.store.db, actor, target)
            .await?;
        if relationship == "BLOCKED" || self.domain_blocks(scope, actor, target).await? {
            return Err(Error::NotFound);
        }
        if (!by_invite && !privacy.public_id_discoverable)
            || privacy.public_profile_visibility == Visibility::Hidden
            || (privacy.public_profile_visibility == Visibility::Connections
                && relationship != "CONNECTED"
                && !by_invite)
        {
            return Err(Error::NotFound);
        }
        let mut result = store::Store::public_profile(
            self.store
                .current_public_id(&self.store.db, target.id)
                .await?,
            target,
            relationship.clone(),
        );
        result["permittedActions"] = json!({"requestConnection":relationship!="CONNECTED" && relationship!="REQUEST_PENDING" && privacy.contact_request_permissions==RequestPermission::Everyone,
            "directCallEligible":relationship=="CONNECTED" && privacy.incoming_call_policy==Audience::Connections,
            "directChatEligible":relationship=="CONNECTED" && privacy.direct_chat_policy==Audience::Connections});
        Ok(result)
    }
}
pub fn decide(action: Action, connected: bool, p: &Privacy) -> bool {
    match action {
        Action::DirectCall => connected && p.incoming_call_policy == Audience::Connections,
        Action::DirectConversation | Action::MessageSend => {
            connected && p.direct_chat_policy == Audience::Connections
        }
        Action::ContactRequest => p.contact_request_permissions == RequestPermission::Everyone,
        Action::GroupInvitation => {
            connected && p.contact_request_permissions == RequestPermission::Everyone
        }
        Action::Presence => connected && p.presence_visibility == Audience::Connections,
        Action::ProfileDiscovery => {
            p.public_id_discoverable
                && (p.public_profile_visibility == Visibility::Public
                    || connected && p.public_profile_visibility == Visibility::Connections)
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn pending_and_revoked_connections_never_authorize_personal_calls_or_messages() {
        let p = Privacy::default();
        for action in [
            Action::DirectCall,
            Action::DirectConversation,
            Action::MessageSend,
            Action::Presence,
            Action::GroupInvitation,
        ] {
            assert!(!decide(action, false, &p));
            assert!(decide(action, true, &p));
        }
    }
    #[test]
    fn privacy_restrictions_stay_effective_after_connection_acceptance() {
        let p = Privacy {
            incoming_call_policy: Audience::Nobody,
            direct_chat_policy: Audience::Nobody,
            presence_visibility: Audience::Nobody,
            ..Privacy::default()
        };
        for a in [
            Action::DirectCall,
            Action::MessageSend,
            Action::DirectConversation,
            Action::Presence,
        ] {
            assert!(!decide(a, true, &p));
        }
    }
}
