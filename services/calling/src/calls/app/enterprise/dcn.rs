//! Part IV Workstream L: an inbound DCN call whose verified voice target is an enterprise
//! extension, ring group or queue enters the enterprise router through an extension —
//! the extension itself, or the scope's ACTIVE extension that publishes the group /
//! queue (lowest extension number). The caller is the public party (opaque
//! ContactReference); its public leg is bridged to the answering member exactly like a
//! PSTN_TO_APP call (origin PUBLIC_INBOUND).
//!
//! Entry destinations that need prompts or media capture on the caller's leg (IVR,
//! voicemail, schedules) are refused for DCN calls (`DCN_TARGET_UNSUPPORTED`): Telephony
//! plays prompts on enterprise device legs only.

use sea_orm::{FromQueryResult, TransactionTrait};
use serde_json::Value;
use uuid::Uuid;

use super::{insert_context, record_step};
use crate::{
    calls::{
        app::{
            public::dcn::{answer, bind_and_record_public_leg, DcnInboundOffer, DcnOwner},
            AppCallService,
        },
        attempts::NewAttempt,
        command::{CallCommandContext, CallScope},
        errors::{CallError, CallResult},
        ids::{CallLegId, CallParticipantId, CallRouteId},
        kinds::{CallKind, LegDirection, LegKind},
        legs::NewLeg,
        service::{CallCreator, NewCall},
    },
    domain::{CredentialId, ServiceAccountId},
    enterprise::{
        destinations::{Destination, DestinationType},
        extensions::ExtensionRow,
        policy::effective_policy,
    },
    numbering::VerifiedInboundGrant,
    platform::principals::ResolvedPrincipal,
    telephony::spend,
};

const EXTENSION_COLUMNS: &str =
    "id, extension_number, display_label, destination_type, destination_id, \
    policy_id, status, version, created_at, updated_at, disabled_at";

fn refuse(reason: &'static str) -> CallError {
    crate::metrics::inc("calling_dcn_refusals_total", &[("reason", reason)]);
    CallError::PublicRouteDenied(reason)
}

impl AppCallService {
    /// The entry extension of a DCN's enterprise voice target in `scope`.
    async fn dcn_entry_extension(
        &self,
        scope: CallScope,
        kind: DestinationType,
        target: Uuid,
    ) -> CallResult<Option<ExtensionRow>> {
        let scope_values = || -> Vec<sea_orm::Value> {
            vec![
                scope.tenant_id().as_uuid().into(),
                scope.application_id().as_uuid().into(),
                scope.environment_id().as_uuid().into(),
                target.into(),
            ]
        };
        let sql = if kind == DestinationType::Extension {
            format!(
                "SELECT {EXTENSION_COLUMNS} FROM enterprise_extensions WHERE tenant_id = $1 \
                 AND application_id = $2 AND environment_id = $3 AND id = $4 AND status = 'ACTIVE'"
            )
        } else {
            format!(
                "SELECT {EXTENSION_COLUMNS} FROM enterprise_extensions WHERE tenant_id = $1 \
                 AND application_id = $2 AND environment_id = $3 AND destination_id = $4 \
                 AND destination_type = '{}' AND status = 'ACTIVE' ORDER BY extension_number LIMIT 1",
                kind.as_str()
            )
        };
        Ok(
            ExtensionRow::find_by_statement(super::stmt(&sql, scope_values()))
                .one(self.db())
                .await?,
        )
    }

    pub(crate) async fn accept_dcn_enterprise(
        &self,
        offer: &DcnInboundOffer,
        verified: &VerifiedInboundGrant,
        scope: CallScope,
        owner: &DcnOwner,
        caller: &ResolvedPrincipal,
        kind: DestinationType,
    ) -> CallResult<Value> {
        if !self.enterprise_flags.enterprise_calling {
            return Err(refuse("ENTERPRISE_CALLING_DISABLED"));
        }
        let provider = self.provider()?;
        if !provider.capabilities().supports_app_calls() {
            return Err(CallError::CapabilityUnavailable(
                "the media provider cannot execute application calls",
            ));
        }
        let extension = self
            .dcn_entry_extension(scope, kind, verified.target.target_ref)
            .await?
            .ok_or_else(|| refuse("DCN_TARGET_NO_ENTRY"))?;
        let policy = effective_policy(self.db(), scope, extension.policy_id)
            .await
            .map_err(super::enterprise_error)?;
        let destination = extension.destination().map_err(super::enterprise_error)?;
        if !matches!(
            destination.kind,
            DestinationType::Principal
                | DestinationType::EnterpriseMember
                | DestinationType::RingGroup
                | DestinationType::Queue
        ) {
            return Err(refuse("DCN_TARGET_UNSUPPORTED"));
        }
        let now = self.calls.now();
        let cmd = CallCommandContext::system_correlated(
            scope,
            ServiceAccountId::from_uuid(owner.service_account_id),
            Some(&offer.correlation_id),
        );
        let creator = CallCreator {
            cmd: cmd.clone(),
            service_account_id: ServiceAccountId::from_uuid(owner.service_account_id),
            credential_id: CredentialId::from_uuid(owner.credential_id),
            origin: "PUBLIC_INBOUND",
        };
        let txn = self.db().begin().await?;
        let (call, caller_participant, _) = self
            .calls
            .create_call_as_in(
                &txn,
                &creator,
                caller,
                NewCall {
                    kind: CallKind::Voice,
                    communication_context_ref: None,
                    privacy_relay_ref: None,
                    initiator_display_name: None,
                },
            )
            .await?;
        let call = self
            .authorize_enterprise_with_in(
                &txn,
                &cmd,
                &call,
                CallKind::Voice,
                &extension,
                destination,
                &policy,
                Some(verified),
            )
            .await?;
        let (call, route_id) = match self
            .route_enterprise_in(&txn, &cmd, &call, destination, "DCN_INBOUND")
            .await
        {
            Ok(v) => v,
            Err(error) => {
                txn.rollback().await?;
                return Err(error);
            }
        };
        let public_leg = self
            .calls
            .record_leg_in(
                &txn,
                &cmd,
                &call,
                NewLeg {
                    route_id: Some(CallRouteId::from_uuid(route_id)),
                    participant_id: Some(CallParticipantId::from_uuid(caller_participant.id)),
                    direction: LegDirection::Inbound,
                    kind: LegKind::PublicNetwork,
                },
            )
            .await?;
        bind_and_record_public_leg(&txn, &call, public_leg.id, offer, now).await?;
        self.calls
            .record_attempt_in(
                &txn,
                &cmd,
                &call,
                NewAttempt {
                    route_id: Some(CallRouteId::from_uuid(route_id)),
                    leg_id: Some(CallLegId::from_uuid(public_leg.id)),
                    previous_attempt_id: None,
                },
            )
            .await?;
        if let Err(denial) = spend::reserve_in(
            &txn,
            &call,
            public_leg.id,
            Some(offer.caller_contact_reference),
            now,
        )
        .await?
        {
            txn.rollback().await?;
            return Err(CallError::TelecomSpendLimitExceeded(denial.as_str()));
        }
        insert_context(&txn, &call, &extension, &policy, destination, now).await?;
        let call = self.locked(&txn, &call).await?;
        record_step(
            &self.calls,
            &txn,
            &call,
            "ENTER",
            Some(Destination::new(DestinationType::Extension, extension.id)),
            "DCN_DIALLED",
            None,
            now,
        )
        .await?;
        txn.commit().await?;
        crate::metrics::inc("calling_dcn_inbound_calls_total", &[("kind", "ENTERPRISE")]);
        tracing::info!(call_id = %call.id, call_leg_id = %public_leg.id, telephony_leg_id = %offer.telephony_leg_id, "DCN inbound call accepted; entering the enterprise router");
        self.advance_routing(call.id).await?;
        Ok(answer(call.id, public_leg.id))
    }
}
