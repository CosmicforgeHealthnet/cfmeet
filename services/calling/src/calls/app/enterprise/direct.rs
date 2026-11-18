//! Forwarding and voicemail of ordinary person-to-person application calls.
//!
//! A one-to-one application call (`POST /v1/calls` with ONE `targetPrincipalIds`, not
//! protected by Privacy Relay, not public-network) to a callee that has an ACTIVE
//! forwarding rule or an ACTIVE voicemail mailbox of its own is routed by the SAME
//! enterprise machinery as an extension call, from the callee as its entry:
//!
//! * the Call gets an `enterprise_call_contexts` row WITHOUT an entry extension
//!   (`entry_extension_id` NULL, number / label `direct`, gap-closure migration 000054)
//!   and the callee as its destination; the caller is in the visited set, so a rule
//!   forwarding back to the caller is a loop (LOOP_BLOCKED);
//! * ALWAYS: forwarded before ringing (the router, before the callee rings);
//! * UNAVAILABLE (no eligible endpoint): the call is routed anyway and the callee's
//!   UNAVAILABLE rule (else its mailbox) takes it instead of failing;
//! * BUSY (every offer declined) / NO_ANSWER (offers expired): the callee's rule for that
//!   condition, else its mailbox, takes the SAME Call once the ring round ended;
//! * without a rule, the fallback is the callee's own mailbox (owner PRINCIPAL), when
//!   voicemail is allowed by the effective policy, enabled (`VOICEMAIL_ENABLED`) and a
//!   capture path exists; otherwise the ordinary outcome (DECLINED / NO_ANSWER / FAILED)
//!   applies exactly as before.
//!
//! Every hop is hop-budgeted, loop-safe and recorded as routing evidence, the effective
//! enterprise policy of the scope applies (`forwardingAllowed`, `voicemailAllowed`,
//! public forwarding still behind every Part E gate) and the kill switch
//! `ENTERPRISE_CALLING_ENABLED` turns it off. In a scope governed by the Communications
//! personal policy only the voicemail fallback applies: a forward would ring a principal
//! the caller's relationship policy never authorized. A direct context is never an
//! enterprise call anywhere else (representation, recording, personal policy,
//! invitations, history labels): those keep checking `entry_extension_id`. Calls to an
//! extension keep their unchanged behaviour.

use sea_orm::{ConnectionTrait, DatabaseTransaction};
use uuid::Uuid;

use super::{
    context_of, enterprise_error, exec, record_step, router::RouteAction, strings_in, ContextRow,
    EnterprisePhase,
};
use crate::{
    calls::{
        app::{scope_of, AppCallService},
        command::CallScope,
        errors::CallResult,
    },
    enterprise::{
        destinations::{Destination, DestinationType},
        forwarding::ForwardingCondition,
        policy::{effective_policy, EffectivePolicy},
        voicemail::{self, MailboxRow},
    },
    models::calls,
};

/// Entry number / label of a direct call's context.
pub(crate) const DIRECT_ENTRY: &str = "direct";

/// What can take a direct call when its callee does not answer it.
#[derive(Debug, Default, Clone, Copy)]
pub(crate) struct DirectHandlers {
    pub always: bool,
    pub rules: bool,
    pub mailbox: bool,
}

impl DirectHandlers {
    pub(crate) fn any(&self) -> bool {
        self.rules || self.mailbox
    }
}

impl AppCallService {
    fn direct_enabled(&self) -> bool {
        self.enterprise_flags.enterprise_calling
    }

    fn voicemail_usable(&self, policy: &EffectivePolicy) -> bool {
        policy.rules.voicemail_allowed
            && self.enterprise_flags.voicemail
            && self.capture.available()
    }

    /// Whether `scope` is governed by the Communications personal policy.
    fn personal_scope(&self, scope: CallScope) -> bool {
        self.communications
            .as_ref()
            .is_some_and(|c| c.governs(crate::communications::scope(scope)))
    }

    /// The callee's own ACTIVE mailbox (owner PRINCIPAL), if any.
    async fn principal_mailbox_in<C: ConnectionTrait>(
        &self,
        db: &C,
        scope: CallScope,
        principal: Uuid,
    ) -> CallResult<Option<MailboxRow>> {
        let ids = strings_in(
            db,
            "SELECT id::text FROM voicemail_mailboxes WHERE tenant_id = $1 AND application_id = $2 \
               AND environment_id = $3 AND owner_type = 'PRINCIPAL' AND owner_id = $4 \
               AND status = 'ACTIVE' ORDER BY created_at, id LIMIT 1",
            vec![
                scope.tenant_id().as_uuid().into(),
                scope.application_id().as_uuid().into(),
                scope.environment_id().as_uuid().into(),
                principal.into(),
            ],
        )
        .await?;
        let Some(id) = ids.first().and_then(|i| Uuid::parse_str(i).ok()) else {
            return Ok(None);
        };
        voicemail::mailbox_row(db, scope, id, false)
            .await
            .map(Some)
            .map_err(enterprise_error)
    }

    /// What can take a direct call to `callee` in `scope` (nothing when the feature is off).
    pub(crate) async fn direct_handlers_in<C: ConnectionTrait>(
        &self,
        db: &C,
        scope: CallScope,
        callee: Uuid,
    ) -> CallResult<DirectHandlers> {
        if !self.direct_enabled() {
            return Ok(DirectHandlers::default());
        }
        let policy = effective_policy(db, scope, None)
            .await
            .map_err(enterprise_error)?;
        let mut out = DirectHandlers::default();
        if policy.rules.forwarding_allowed && !self.personal_scope(scope) {
            let conditions = strings_in(
                db,
                "SELECT condition FROM enterprise_forwarding_rules WHERE tenant_id = $1 \
                   AND application_id = $2 AND environment_id = $3 AND principal_id = $4 \
                   AND status = 'ACTIVE'",
                vec![
                    scope.tenant_id().as_uuid().into(),
                    scope.application_id().as_uuid().into(),
                    scope.environment_id().as_uuid().into(),
                    callee.into(),
                ],
            )
            .await?;
            out.rules = !conditions.is_empty();
            out.always = conditions.iter().any(|c| c == "ALWAYS");
        }
        if self.voicemail_usable(&policy) {
            out.mailbox = self
                .principal_mailbox_in(db, scope, callee)
                .await?
                .is_some();
        }
        Ok(out)
    }

    /// The callee of a direct one-to-one application call that has no routing context
    /// yet (`None` for every other Call).
    async fn direct_callee_in<C: ConnectionTrait>(
        &self,
        db: &C,
        call: &calls::Model,
    ) -> CallResult<Option<Uuid>> {
        if !self.direct_enabled()
            || call.origin != "APPLICATION"
            || call.privacy_relay_ref.is_some()
            || context_of(db, call.id).await?.is_some()
            || crate::calls::app::public::is_public_outbound(db, call.id).await?
        {
            return Ok(None);
        }
        let recipients = strings_in(
            db,
            "SELECT principal_id::text FROM call_participants WHERE call_id = $1 \
               AND role = 'RECIPIENT' \
               AND (SELECT count(*) FROM call_participants a WHERE a.call_id = $1) = 2",
            vec![call.id.into()],
        )
        .await?;
        Ok(match recipients.as_slice() {
            [one] => Uuid::parse_str(one).ok(),
            _ => None,
        })
    }

    /// Opens the direct routing context of `call` (callee as destination).
    async fn begin_direct_routing_in(
        &self,
        txn: &DatabaseTransaction,
        call: &calls::Model,
        callee: Uuid,
        phase: EnterprisePhase,
    ) -> CallResult<ContextRow> {
        let scope = scope_of(call);
        let policy = effective_policy(txn, scope, None)
            .await
            .map_err(enterprise_error)?;
        let now = self.calls.now();
        // The caller can never be forwarded to; a callee already rung is visited too.
        let mut visited = vec![call.initiator_principal_id];
        if phase == EnterprisePhase::Ringing {
            visited.push(callee);
        }
        exec(
            txn,
            "INSERT INTO enterprise_call_contexts (call_id, tenant_id, application_id, environment_id, \
             entry_extension_id, entry_extension_number, entry_label, policy_id, policy_version, phase, \
             current_destination_type, current_destination_id, hop_count, max_hops, visited, version, \
             created_at, updated_at) VALUES ($1, $2, $3, $4, NULL, $5, $5, $6, $7, $8, 'PRINCIPAL', $9, \
             0, $10, $11, 1, $12, $12)",
            vec![
                call.id.into(),
                call.tenant_id.into(),
                call.application_id.into(),
                call.environment_id.into(),
                DIRECT_ENTRY.into(),
                policy.policy_id.into(),
                policy.version.into(),
                phase.as_str().into(),
                callee.into(),
                (policy.rules.max_routing_hops as i16).into(),
                visited.into(),
                now.into(),
            ],
        )
        .await?;
        record_step(
            &self.calls,
            txn,
            call,
            "ENTER",
            Some(Destination::new(DestinationType::Principal, callee)),
            "DIRECT_CALL",
            None,
            now,
        )
        .await?;
        crate::metrics::inc(
            "calling_direct_routing_total",
            &[("status", phase.as_str())],
        );
        context_of(txn, call.id)
            .await?
            .ok_or(crate::calls::errors::CallError::NotFound)
    }

    /// Before a direct call rings (DIALING, media ready): an ALWAYS rule, or a callee
    /// without an eligible endpoint whose UNAVAILABLE rule / mailbox can take the call,
    /// hands the Call to enterprise routing. Returns whether it did.
    pub(crate) async fn direct_routing_before_ring_in(
        &self,
        txn: &DatabaseTransaction,
        call: &calls::Model,
    ) -> CallResult<bool> {
        let Some(callee) = self.direct_callee_in(txn, call).await? else {
            return Ok(false);
        };
        let scope = scope_of(call);
        let handlers = self.direct_handlers_in(txn, scope, callee).await?;
        if !handlers.any() {
            return Ok(false);
        }
        let unreachable = self
            .endpoints
            .eligible_in(
                txn,
                scope,
                callee,
                self.config.max_endpoints_per_participant,
            )
            .await?
            .is_empty();
        if !(handlers.always || unreachable) {
            return Ok(false);
        }
        self.begin_direct_routing_in(txn, call, callee, EnterprisePhase::Routing)
            .await?;
        Ok(true)
    }

    /// A direct call's ring round ended unanswered: when the callee has a rule or a
    /// mailbox, the Call gets its routing context (RINGING the callee) so the BUSY /
    /// NO_ANSWER continuation applies to it.
    pub(crate) async fn direct_routing_at_ring_end_in(
        &self,
        txn: &DatabaseTransaction,
        call: &calls::Model,
    ) -> CallResult<()> {
        let Some(callee) = self.direct_callee_in(txn, call).await? else {
            return Ok(());
        };
        if self
            .direct_handlers_in(txn, scope_of(call), callee)
            .await?
            .any()
        {
            self.begin_direct_routing_in(txn, call, callee, EnterprisePhase::Ringing)
                .await?;
        }
        Ok(())
    }

    /// After ringing `principal` failed with `condition`: its forwarding rule
    /// ([`super::router::resolve_forward`]) and, for a direct call only, the principal's
    /// own mailbox as the fallback. `None`: the ordinary outcome applies.
    #[allow(clippy::too_many_arguments)]
    pub(crate) async fn forward_or_voicemail_in(
        &self,
        txn: &DatabaseTransaction,
        call: &calls::Model,
        policy: &EffectivePolicy,
        context: &ContextRow,
        principal: Uuid,
        condition: ForwardingCondition,
        at: chrono::DateTime<chrono::Utc>,
    ) -> CallResult<Option<RouteAction>> {
        let scope = scope_of(call);
        let direct = context.entry_extension_id.is_none();
        if !(direct && self.personal_scope(scope)) {
            if let Some(action) = super::router::resolve_forward(
                &self.calls,
                txn,
                &self.principals,
                scope,
                policy,
                context,
                principal,
                condition,
                at,
            )
            .await?
            {
                return Ok(Some(action));
            }
        }
        if !direct || !self.voicemail_usable(policy) {
            return Ok(None);
        }
        let Some(mailbox) = self.principal_mailbox_in(txn, scope, principal).await? else {
            return Ok(None);
        };
        let destination = Destination::new(DestinationType::Voicemail, mailbox.id);
        record_step(
            &self.calls,
            txn,
            call,
            "VOICEMAIL",
            Some(destination),
            match condition {
                ForwardingCondition::Busy => "VOICEMAIL_BUSY",
                ForwardingCondition::NoAnswer => "VOICEMAIL_NO_ANSWER",
                _ => "VOICEMAIL_UNAVAILABLE",
            },
            None,
            at,
        )
        .await?;
        Ok(Some(RouteAction::EnterVoicemail {
            mailbox,
            destination,
        }))
    }
}
