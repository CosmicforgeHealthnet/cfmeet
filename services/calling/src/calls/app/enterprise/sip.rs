//! Enterprise SIP devices (Part F §54, §57, §179): internal SIP dialogs on CF Meet's own
//! edge, offered by Telephony, routed by Calling on ONE canonical Call.
//!
//! Calling commands the device's leg through the durable Telephony command outbox
//! (ANSWER, PROMPT with a BUILT-IN prompt id, TERMINATE, bridges) and receives its
//! normalized events and DTMF. No SIP URI, edge endpoint, channel id or digit sequence is
//! stored here.

use cf_service_auth::CorrelationId;
use sea_orm::{ConnectionTrait, DatabaseTransaction, FromQueryResult, TransactionTrait};
use serde::Deserialize;
use serde_json::{json, Value};
use uuid::Uuid;

use super::{insert_context, record_step, stmt, IvrInputSource};
use crate::{
    calls::{
        app::{
            public::{enqueue_in, CommandKind},
            AppCallService,
        },
        command::{CallCommandContext, CallScope},
        errors::{CallError, CallResult},
        ids::{CallParticipantId, CallRouteId},
        kinds::{CallKind, LegDirection, LegKind},
        legs::NewLeg,
        service::{CallCreator, NewCall},
    },
    domain::{ApplicationId, CredentialId, EnvironmentId, PrincipalId, ServiceAccountId, TenantId},
    enterprise::{
        destinations::{Destination, DestinationType},
        extensions,
        ivr::BuiltinPrompt,
        policy::effective_policy,
        sip_devices,
    },
    models::{call_legs, calls},
    platform::principals::PrincipalReference,
};

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct OfferScope {
    pub tenant_id: Uuid,
    pub application_id: Uuid,
    pub environment_id: Uuid,
}

/// Telephony's offer of an internal enterprise SIP dialog (contract
/// `cfmeet.telephony.enterprise`): references only.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct EnterpriseDialogOffer {
    pub schema_version: String,
    pub telephony_leg_id: Uuid,
    pub correlation_id: String,
    pub scope: OfferScope,
    pub device_ref: String,
    pub extension: String,
    pub max_duration_seconds: i32,
}

/// One key of an enterprise SIP leg's caller (transient; never stored or logged).
#[derive(Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct EnterpriseKeypad {
    pub schema_version: String,
    pub telephony_leg_id: Uuid,
    pub call_id: Uuid,
    pub call_leg_id: Uuid,
    pub digit: String,
}

impl std::fmt::Debug for EnterpriseKeypad {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EnterpriseKeypad")
            .field("call_id", &self.call_id)
            .finish_non_exhaustive()
    }
}

#[derive(Debug, FromQueryResult)]
struct Owner {
    service_account_id: Uuid,
    credential_id: Uuid,
}

#[derive(Debug, FromQueryResult)]
struct Existing {
    call_id: Uuid,
    id: Uuid,
}

fn answer(call_id: Uuid, call_leg_id: Uuid) -> Value {
    json!({ "callId": call_id, "callLegId": call_leg_id, "routeClass": "ENTERPRISE_INTERNAL" })
}

impl AppCallService {
    /// Accepts (or refuses) an enterprise device's dialog: ONE canonical Call (origin
    /// ENTERPRISE_DEVICE) over ENTERPRISE_INTERNAL, the dialog a bound PRIVATE_NETWORK
    /// leg, routed by enterprise configuration exactly like an application-created call.
    /// Idempotent per Telephony leg. Refusals reveal nothing to the device.
    pub async fn accept_enterprise_offer(
        &self,
        offer: &EnterpriseDialogOffer,
    ) -> CallResult<Value> {
        if offer.schema_version != "1"
            || !(30..=86_400).contains(&offer.max_duration_seconds)
            || CorrelationId::parse(&offer.correlation_id).is_err()
            || !sip_devices::valid_device_ref(&offer.device_ref)
        {
            return Err(CallError::Validation("malformed enterprise offer"));
        }
        if let Some(e) = Existing::find_by_statement(stmt(
            "SELECT call_id, id FROM call_legs WHERE execution_binding_ref = $1",
            vec![offer.telephony_leg_id.into()],
        ))
        .one(self.db())
        .await?
        {
            return Ok(answer(e.call_id, e.id));
        }
        if !self.enterprise_flags.enterprise_calling {
            return Err(CallError::CapabilityUnavailable(
                "enterprise calling is not enabled",
            ));
        }
        let provider = self.provider()?;
        if !provider.capabilities().supports_app_calls() {
            return Err(CallError::CapabilityUnavailable(
                "the media provider cannot execute application calls",
            ));
        }
        let s = &offer.scope;
        let scope = CallScope::from_parts(
            TenantId::from_uuid(s.tenant_id),
            ApplicationId::from_uuid(s.application_id),
            EnvironmentId::from_uuid(s.environment_id),
        );
        // The device must be bound to an ACTIVE principal of THIS scope.
        let device = sip_devices::active_binding(self.db(), scope, &offer.device_ref)
            .await
            .map_err(super::enterprise_error)?
            .ok_or(CallError::NotFound)?;
        let owner = Owner::find_by_statement(stmt(
            "SELECT service_account_id, credential_id FROM call_inbound_owners \
             WHERE environment_id = $1 AND tenant_id = $2 AND application_id = $3 AND enabled",
            vec![
                s.environment_id.into(),
                s.tenant_id.into(),
                s.application_id.into(),
            ],
        ))
        .one(self.db())
        .await?
        .ok_or(CallError::CapabilityUnavailable(
            "no owner is configured for calls of this environment",
        ))?;
        let caller = self
            .principals
            .resolve_in(
                scope.tenant_id(),
                scope.application_id(),
                scope.environment_id(),
                &PrincipalReference::Id(PrincipalId::from_uuid(device.principal_id)),
            )
            .await
            .map_err(|_| CallError::NotFound)?;
        let Some(extension) = extensions::resolve_number(self.db(), scope, &offer.extension)
            .await
            .map_err(super::enterprise_error)?
        else {
            crate::metrics::inc(
                "calling_enterprise_extension_resolution_failures_total",
                &[("reason", "NOT_FOUND")],
            );
            return Err(CallError::NotFound);
        };
        let policy = effective_policy(self.db(), scope, extension.policy_id)
            .await
            .map_err(super::enterprise_error)?;
        if !policy.rules.internal_calling_enabled {
            crate::metrics::inc(
                "calling_enterprise_extension_resolution_failures_total",
                &[("reason", "POLICY_DENIED")],
            );
            return Err(CallError::AuthorizationDenied);
        }
        let destination = extension.destination().map_err(super::enterprise_error)?;
        if destination.kind.is_principal() && destination.id == device.principal_id {
            return Err(CallError::Validation(
                "the extension routes to the calling device's own principal.",
            ));
        }
        let cmd = CallCommandContext::system(
            scope,
            ServiceAccountId::from_uuid(owner.service_account_id),
        );
        let creator = CallCreator {
            cmd: cmd.clone(),
            service_account_id: ServiceAccountId::from_uuid(owner.service_account_id),
            credential_id: CredentialId::from_uuid(owner.credential_id),
            origin: "ENTERPRISE_DEVICE",
        };
        let txn = self.db().begin().await?;
        let (call, caller_participant, _) = self
            .calls
            .create_call_as_in(
                &txn,
                &creator,
                &caller,
                NewCall {
                    kind: CallKind::Voice,
                    communication_context_ref: None,
                    privacy_relay_ref: None,
                    initiator_display_name: None,
                },
            )
            .await?;
        let call = self
            .authorize_enterprise_in(
                &txn,
                &cmd,
                &call,
                CallKind::Voice,
                &extension,
                destination,
                &policy,
            )
            .await?;
        let (call, route_id) = match self
            .route_enterprise_in(&txn, &cmd, &call, destination, "ENTERPRISE_DEVICE")
            .await
        {
            Ok(v) => v,
            Err(error) => {
                txn.rollback().await?;
                return Err(error);
            }
        };
        // The device's dialog: the caller's PRIVATE_NETWORK leg, bound to its execution.
        let device_leg = self
            .calls
            .record_leg_in(
                &txn,
                &cmd,
                &call,
                NewLeg {
                    route_id: Some(CallRouteId::from_uuid(route_id)),
                    participant_id: Some(CallParticipantId::from_uuid(caller_participant.id)),
                    direction: LegDirection::Inbound,
                    kind: LegKind::PrivateNetwork,
                },
            )
            .await?;
        super::exec(
            &txn,
            "UPDATE call_legs SET execution_binding_ref = $2 WHERE id = $1 AND execution_binding_ref IS NULL",
            vec![device_leg.id.into(), offer.telephony_leg_id.into()],
        )
        .await?;
        insert_context(
            &txn,
            &call,
            &extension,
            &policy,
            destination,
            self.calls.now(),
        )
        .await?;
        let call = self.locked(&txn, &call).await?;
        record_step(
            &txn,
            &call,
            "ENTER",
            Some(Destination::new(DestinationType::Extension, extension.id)),
            "DEVICE_DIALLED",
            None,
            self.calls.now(),
        )
        .await?;
        txn.commit().await?;
        crate::metrics::inc(
            "calling_enterprise_calls_total",
            &[("route_class", "ENTERPRISE_INTERNAL")],
        );
        tracing::info!(call_id = %call.id, call_leg_id = %device_leg.id, "enterprise device dialog accepted");
        // Routing runs now; the leg's ANSWER / PROMPT commands are sent by the dispatcher
        // once Telephony has persisted the leg it is offering (after this answer).
        self.advance_routing(call.id).await?;
        Ok(answer(call.id, device_leg.id))
    }

    /// One key from an enterprise SIP leg (Telephony). The leg must be the Call's bound
    /// device leg. Returns whether the IVR took it.
    pub async fn accept_enterprise_keypad(&self, keypad: &EnterpriseKeypad) -> CallResult<bool> {
        if keypad.schema_version != "1" {
            return Err(CallError::Validation("malformed keypad input"));
        }
        let bound = self
            .db()
            .query_one(stmt(
                "SELECT 1 FROM call_legs WHERE id = $1 AND call_id = $2 AND execution_binding_ref = $3 \
                 AND leg_kind = 'PRIVATE_NETWORK'",
                vec![
                    keypad.call_leg_id.into(),
                    keypad.call_id.into(),
                    keypad.telephony_leg_id.into(),
                ],
            ))
            .await?
            .is_some();
        if !bound {
            return Err(CallError::NotFound);
        }
        let accepted = self
            .ivr_input(keypad.call_id, &keypad.digit, IvrInputSource::Sip)
            .await?;
        Ok(accepted)
    }

    /// Answers the device's dialog (idempotent per leg).
    pub(crate) async fn enqueue_sip_answer_in(
        &self,
        txn: &DatabaseTransaction,
        call: &calls::Model,
        leg: &call_legs::Model,
    ) -> CallResult<()> {
        enqueue_in(
            txn,
            call,
            CommandKind::Answer,
            Some(leg.id),
            None,
            None,
            &format!("answer:{}", leg.id),
            json!({
                "correlationId": cf_service_auth::CorrelationId::generate().as_str(),
            }),
            self.calls.now(),
        )
        .await?;
        Ok(())
    }

    /// Plays one built-in prompt on the device's leg. `step` makes the command unique per
    /// IVR step (a replayed step never plays twice).
    pub(crate) async fn enqueue_sip_prompt_in(
        &self,
        txn: &DatabaseTransaction,
        call: &calls::Model,
        leg: &call_legs::Model,
        prompt: BuiltinPrompt,
        step: i16,
    ) -> CallResult<()> {
        let n: i64 = {
            use sea_orm::ConnectionTrait;
            txn.query_one(super::stmt(
                "SELECT count(*) FROM call_telephony_commands WHERE call_leg_id = $1 AND kind = 'PROMPT'",
                vec![leg.id.into()],
            ))
            .await?
            .map(|r| r.try_get_by_index::<i64>(0))
            .transpose()?
            .unwrap_or(0)
        };
        enqueue_in(
            txn,
            call,
            CommandKind::Prompt,
            Some(leg.id),
            None,
            None,
            &format!("prompt:{}:{n:05}:{step}", leg.id),
            json!({
                "correlationId": cf_service_auth::CorrelationId::generate().as_str(),
                "prompt": prompt.as_str(),
            }),
            self.calls.now(),
        )
        .await?;
        Ok(())
    }
}
