//! Communication authorization (Part D §29–§30): may `source` communicate with every
//! `target`, on `channel`, over `route`, under relay session S — NOW?
//!
//! Every decision evaluates BOTH the current Communication Context AND the current Relay
//! Session (effective authority is their intersection; a stale session snapshot can never
//! override a newer restrictive context), against the server clock, under shared row
//! locks so a concurrent context change is either fully visible or not yet committed.
//! Every decision — AUTHORIZED or DENIED — is recorded in `relay_authorization_decisions`.
//!
//! Scope comes only from the signed delegated context; the request carries no tenant,
//! application, environment or client-asserted result. ContactReference subjects can be
//! authorized by policy but cannot be EXECUTED yet (no Contact Vault / telephony): such
//! requests are CAPABILITY_UNAVAILABLE, never a fabricated destination.

use chrono::{DateTime, Utc};
use sea_orm::TransactionTrait;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use uuid::Uuid;

use super::session::{session_row, SessionAggregate};
use crate::{
    context::{
        aggregate::{ts, ContextAggregate},
        participants::{SubjectBody, SubjectRef},
        repository::Lock,
    },
    db,
    error::{ErrorCode, RelayError, RelayResult},
    scope::CommandContext,
    service::RelayService,
    vocabulary::{
        AuthorizationDecision, AuthorizationPurpose, AuthorizationReason, CommunicationChannel,
        CommunicationRoute,
    },
};

pub const MAX_TARGETS: usize = 64;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AuthorizeRequest {
    pub relay_session_id: Uuid,
    pub source: SubjectBody,
    pub targets: Vec<SubjectBody>,
    pub channel: CommunicationChannel,
    #[serde(default = "application_route")]
    pub route: CommunicationRoute,
    #[serde(default = "check_purpose")]
    pub purpose: AuthorizationPurpose,
}

fn application_route() -> CommunicationRoute {
    CommunicationRoute::Application
}

fn check_purpose() -> AuthorizationPurpose {
    AuthorizationPurpose::Check
}

/// Pure evaluation result.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Evaluation {
    pub reason: AuthorizationReason,
    pub valid_until: Option<DateTime<Utc>>,
}

/// What is being asked, in subject terms.
#[derive(Debug, Clone)]
pub struct Ask<'a> {
    pub source: SubjectRef,
    pub targets: &'a [SubjectRef],
    pub channel: CommunicationChannel,
    pub route: CommunicationRoute,
}

/// Evaluates `ask` against the loaded context and session at `now`.
pub fn evaluate(
    enabled: bool,
    ctx: &ContextAggregate,
    session: &SessionAggregate,
    ask: &Ask<'_>,
    now: DateTime<Utc>,
) -> RelayResult<Evaluation> {
    let (source, targets, channel, route) = (ask.source, ask.targets, ask.channel, ask.route);
    let deny = |reason| {
        Ok(Evaluation {
            reason,
            valid_until: None,
        })
    };
    if !enabled {
        return deny(AuthorizationReason::FeatureDisabled);
    }
    let ctx_until = match ctx.window()?.authority(&ctx.policy()?, now) {
        Ok(until) => until,
        Err(denial) => return deny(denial.context_reason()),
    };
    let session_until = match session.window()?.authority(&session.row.policy()?, now) {
        Ok(until) => until,
        Err(denial) => return deny(denial.session_reason()),
    };
    let strategy = session.row.strategy()?;
    if !ctx.row.strategies()?.contains(&strategy) || !strategy.permits(route) {
        return deny(AuthorizationReason::StrategyNotAllowed);
    }
    let ctx_channels = ctx.enabled_channels()?;
    if !ctx_channels.contains(&channel) || !session.enabled_channels()?.contains(&channel) {
        return deny(AuthorizationReason::ChannelNotAllowed);
    }
    // Participants: active in the context AND in the session snapshot.
    let subjects = ctx.subjects()?;
    let session_ids = session.participant_ids();
    let member = |s: &SubjectRef| {
        subjects
            .get(s)
            .copied()
            .filter(|id| session_ids.contains(id))
    };
    let Some(source_id) = member(&source) else {
        return deny(AuthorizationReason::ParticipantNotAuthorized);
    };
    let mut target_ids = Vec::with_capacity(targets.len());
    for target in targets {
        match member(target) {
            Some(id) if id != source_id => target_ids.push(id),
            _ => return deny(AuthorizationReason::ParticipantNotAuthorized),
        }
    }
    for target in &target_ids {
        if !ctx.allows_direction(source_id, *target, channel)?
            || !session.allows_direction(source_id, *target, channel)?
        {
            return deny(AuthorizationReason::DirectionNotAllowed);
        }
    }
    // Policy allows it; can CF Meet execute it today?
    let needs_contact = matches!(source, SubjectRef::Contact(_))
        || targets.iter().any(|t| matches!(t, SubjectRef::Contact(_)));
    if !route.is_executable() || !channel.is_executable() || needs_contact {
        return deny(AuthorizationReason::CapabilityUnavailable);
    }
    Ok(Evaluation {
        reason: AuthorizationReason::Authorized,
        valid_until: Some(ctx_until.min(session_until)),
    })
}

impl RelayService {
    /// Decides and records one communication authorization.
    pub async fn authorize(
        &self,
        cmd: &CommandContext,
        req: AuthorizeRequest,
    ) -> RelayResult<Value> {
        let source = req.source.subject()?;
        if req.targets.is_empty() || req.targets.len() > MAX_TARGETS {
            return Err(RelayError::Validation(
                "targets must contain 1-64 subjects.",
            ));
        }
        let targets = req
            .targets
            .iter()
            .map(SubjectBody::subject)
            .collect::<RelayResult<Vec<_>>>()?;
        let mut distinct = targets.clone();
        distinct.sort();
        distinct.dedup();
        if distinct.len() != targets.len() {
            return Err(RelayError::Validation("targets must be distinct."));
        }
        let scope = cmd.scope();
        let txn = self.db.begin().await?;
        let probe = session_row(&txn, scope, req.relay_session_id, Lock::None)
            .await?
            .ok_or(RelayError::Refused(ErrorCode::SessionNotFound))?;
        // Shared locks: a concurrent authority change (FOR UPDATE) is either committed and
        // visible here, or waits until this decision is recorded.
        let ctx = ContextAggregate::load(&txn, scope, probe.context_id, Lock::Share).await?;
        let session =
            SessionAggregate::load(&txn, scope, req.relay_session_id, Lock::Share).await?;
        let now = self.now();
        let evaluation = evaluate(
            self.feature.enabled(),
            &ctx,
            &session,
            &Ask {
                source,
                targets: &targets,
                channel: req.channel,
                route: req.route,
            },
            now,
        )?;
        let decision = if evaluation.reason == AuthorizationReason::Authorized {
            AuthorizationDecision::Authorized
        } else {
            AuthorizationDecision::Denied
        };
        let id = Uuid::new_v4();
        let principals: Vec<Uuid> = targets.iter().filter_map(SubjectRef::principal).collect();
        let contacts: Vec<Uuid> = targets.iter().filter_map(SubjectRef::contact).collect();
        db::exec(
            &txn,
            "INSERT INTO relay_authorization_decisions (id, tenant_id, application_id, environment_id, \
               context_id, session_id, context_version, session_version, source_subject_kind, source_ref, \
               target_principal_refs, target_contact_refs, channel, route, purpose, decision, reason, \
               strategy, policy_version, valid_until, requester_service, request_id, correlation_id, decided_at) \
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15, $16, $17, $18, $19, \
                     $20, $21, $22, $23, $24)",
            vec![
                id.into(),
                scope.tenant_id().into(),
                scope.application_id().into(),
                scope.environment_id().into(),
                ctx.id().into(),
                session.row.id.into(),
                ctx.row.version.into(),
                session.row.version.into(),
                source.kind().as_str().into(),
                source.id().into(),
                principals.into(),
                contacts.into(),
                req.channel.as_str().into(),
                req.route.as_str().into(),
                req.purpose.as_str().into(),
                decision.as_str().into(),
                evaluation.reason.as_str().into(),
                session.row.strategy.clone().into(),
                session.row.policy_version.clone().into(),
                evaluation.valid_until.into(),
                cmd.actor_service().as_str().into(),
                cmd.request_id().as_str().into(),
                cmd.correlation_id().as_str().into(),
                now.into(),
            ],
        )
        .await?;
        txn.commit().await?;
        tracing::info!(
            decision_id = %id,
            context_id = %ctx.id(),
            context_version = ctx.row.version,
            relay_session_id = %session.row.id,
            session_version = session.row.version,
            decision = decision.as_str(),
            reason = evaluation.reason.as_str(),
            "communication authorization decided"
        );
        let mut body = json!({
            "decisionId": id,
            "decision": decision.as_str(),
            "reason": evaluation.reason.as_str(),
            "contextId": ctx.id(),
            "contextVersion": ctx.row.version,
            "relaySessionId": session.row.id,
            "relaySessionVersion": session.row.version,
            "strategy": session.row.strategy,
            "policyVersion": session.row.policy_version,
            "channel": req.channel.as_str(),
            "route": req.route.as_str(),
            "purpose": req.purpose.as_str(),
            "decidedAt": ts(now),
        });
        if let Some(until) = evaluation.valid_until {
            body["validUntil"] = json!(ts(until));
        }
        Ok(body)
    }
}
