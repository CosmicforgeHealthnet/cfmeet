//! Inbound relay resolution (§35): the Privacy Relay half of a future inbound
//! public-network call. Telephony (only) presents the CALLED relay number and safe
//! provider metadata; Privacy Relay answers the CURRENT safe mapping — context /
//! session / assignment references, versions, the protected contacts that may call in
//! and whom each may reach (CALLBACK_RELAY, VOICE), and the validity boundary — or a
//! generic NO_ACTIVE_RELAY_MAPPING. No destination number is ever decrypted or returned,
//! no carrier callback exists and nothing is executed.
//!
//! Algorithm (fail closed at every step; every attempt is recorded):
//! 1. Telephony is authenticated and holds `relay-number.resolve-inbound`;
//! 2. the called relay identity is canonicalized (same parser as contacts);
//! 3. exactly one inventory record;
//! 4. exactly one ACTIVE assignment — more is an incident (ledger + error log), never a
//!    choice; none (cooldown, released, revoked, expired, unknown) is generic;
//! 5. a presented assignment id / generation must match;
//! 6. the event must not predate the assignment (trustworthy provider start time, else
//!    the provider's observation time), must not be stale or from the future, and the
//!    assignment must still be valid;
//! 7. – 10. the session (bound to THIS assignment) and its context authorize the
//!    CALLBACK_RELAY route now (same evaluation as every authorization decision:
//!    lifecycle, windows, grace policy, strategy, channels, participants, directions,
//!    contact verification);
//! 11. – 12. only the allowed (source contact → target subject) pairs are returned.
//!
//! Delayed callbacks are defeated twice: the number is quarantined for the cooldown
//! after every assignment, and an event carrying an older generation / assignment, or
//! originating before the current assignment, is refused. Providers that cannot carry
//! generation metadata rely on the cooldown alone, which must therefore exceed the
//! provider's certified delayed-callback / retry window (Part E certification).

use std::collections::BTreeMap;

use chrono::{DateTime, Duration, Utc};
use sea_orm::{ConnectionTrait, TransactionTrait};
use serde::Deserialize;
use serde_json::{json, Value};
use uuid::Uuid;

use super::{assignment::ledger, AssignmentRow, ASSIGNMENT_COLUMNS};
use crate::{
    contact_vault::{normalization::canonical_public_number, verification::contact_use},
    context::{
        aggregate::{ts, ContextAggregate},
        participants::SubjectRef,
        repository::Lock,
        state::validate_external_reference,
    },
    db,
    error::{ErrorCode, RelayError, RelayResult},
    relay::{
        authorization::{evaluate, Ask, ContactUse},
        session::SessionAggregate,
    },
    scope::RelayScope,
    service::RelayService,
    vocabulary::{
        AuthorizationReason, CommunicationChannel, CommunicationRoute, RelayAssignmentStatus,
        RelayNumberStatus,
    },
};
use cf_service_auth::{CorrelationId, RequestId, ServiceId};

/// Oldest provider observation accepted (a carrier retry carries a fresh observation).
const MAX_EVENT_AGE_MINUTES: i64 = 15;
/// Clock skew tolerated for provider timestamps.
const MAX_SKEW_SECONDS: i64 = 120;
/// How long a RESOLVED resolution may scope an inbound caller protection.
pub const PROTECTION_WINDOW_SECONDS: i64 = 120;

/// `POST /internal/v1/relay-routing/resolve-inbound`.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ResolveInboundRequest {
    /// The called relay number (platform-owned public identity; never logged).
    pub called_relay_identity: String,
    /// Opaque provider event reference (never a number or URI).
    #[serde(default)]
    pub provider_event_reference: Option<String>,
    /// When the provider observed the inbound event.
    pub provider_observed_at: DateTime<Utc>,
    /// When the call originated, where the provider reports it trustworthily.
    #[serde(default)]
    pub provider_started_at: Option<DateTime<Utc>>,
    /// Assignment correlation, where the provider path carries it.
    #[serde(default)]
    pub assignment_generation: Option<i64>,
    #[serde(default)]
    pub assignment_id: Option<Uuid>,
}

impl std::fmt::Debug for ResolveInboundRequest {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ResolveInboundRequest")
            .field("provider_observed_at", &self.provider_observed_at)
            .field("assignment_generation", &self.assignment_generation)
            .finish_non_exhaustive()
    }
}

/// The authenticated inbound requester (Telephony; no delegated scope — the mapping
/// is the scope).
#[derive(Debug, Clone)]
pub struct InboundRequester {
    pub service: ServiceId,
    pub request_id: RequestId,
    pub correlation_id: CorrelationId,
}

/// What the record of one attempt knows.
#[derive(Default)]
struct Trace {
    relay_number_id: Option<Uuid>,
    assignment: Option<(Uuid, i64)>,
    scope: Option<(RelayScope, Uuid, Uuid)>,
    versions: Option<(i64, i64)>,
}

struct Refusal {
    reason: &'static str,
    ambiguous: bool,
}

fn refuse(reason: &'static str) -> Refusal {
    Refusal {
        reason,
        ambiguous: false,
    }
}

/// Exactly one ACTIVE mapping, or a refusal (none: generic; several: incident).
pub(crate) fn single_mapping(active: Vec<AssignmentRow>) -> Result<AssignmentRow, &'static str> {
    let mut it = active.into_iter();
    match (it.next(), it.next()) {
        (None, _) => Err("NO_ACTIVE_ASSIGNMENT"),
        (Some(a), None) => Ok(a),
        (Some(_), Some(_)) => Err("AMBIGUOUS_MAPPING"),
    }
}

fn subject_json(s: &SubjectRef) -> Value {
    match s {
        SubjectRef::Principal(id) => json!({ "principalId": id }),
        SubjectRef::Contact(id) => json!({ "contactReferenceId": id }),
    }
}

/// Inbound authority of the session's protected contacts: (source contact, allowed
/// targets) pairs, the earliest validity, and the first denial reason.
pub(crate) struct InboundAuthority {
    pub sources: Vec<(Uuid, Vec<SubjectRef>)>,
    pub valid_until: Option<DateTime<Utc>>,
    pub denial: Option<AuthorizationReason>,
}

pub(crate) async fn inbound_authority<C: ConnectionTrait>(
    relay: &RelayService,
    conn: &C,
    ctx: &ContextAggregate,
    session: &SessionAggregate,
    only_source: Option<Uuid>,
    now: DateTime<Utc>,
) -> RelayResult<InboundAuthority> {
    let scope = RelayScope::from_row(
        ctx.row.tenant_id,
        ctx.row.application_id,
        ctx.row.environment_id,
    );
    let subjects: Vec<SubjectRef> = ctx
        .subjects()?
        .into_iter()
        .filter(|(_, id)| session.participant_ids().contains(id))
        .map(|(s, _)| s)
        .collect();
    let mut uses = BTreeMap::new();
    for s in &subjects {
        if let SubjectRef::Contact(c) = s {
            uses.insert(*c, contact_use(conn, scope, *c, now).await?);
        }
    }
    if let Some(c) = only_source {
        uses.entry(c).or_insert(ContactUse::Unavailable);
    }
    let mut out = InboundAuthority {
        sources: Vec::new(),
        valid_until: None,
        denial: None,
    };
    let sources: Vec<Uuid> = match only_source {
        Some(c) => vec![c],
        None => subjects.iter().filter_map(SubjectRef::contact).collect(),
    };
    for source in sources {
        let mut targets = Vec::new();
        for target in subjects
            .iter()
            .filter(|t| **t != SubjectRef::Contact(source))
        {
            let eval = evaluate(
                relay.feature().enabled(),
                ctx,
                session,
                &Ask {
                    source: SubjectRef::Contact(source),
                    targets: std::slice::from_ref(target),
                    channel: CommunicationChannel::Voice,
                    route: CommunicationRoute::CallbackRelay,
                    contacts: &uses,
                    contact_vault_enabled: relay.contact_vault_enabled(),
                },
                now,
            )?;
            if eval.reason == AuthorizationReason::Authorized {
                targets.push(*target);
                out.valid_until = match (out.valid_until, eval.valid_until) {
                    (Some(a), Some(b)) => Some(a.min(b)),
                    (a, b) => a.or(b),
                };
            } else if out.denial.is_none() {
                out.denial = Some(eval.reason);
            }
        }
        if !targets.is_empty() {
            out.sources.push((source, targets));
        }
    }
    if subjects.iter().all(|s| s.contact().is_none()) && out.denial.is_none() {
        out.denial = Some(AuthorizationReason::CapabilityUnavailable);
    }
    Ok(out)
}

impl RelayService {
    /// Resolves an inbound call's called relay number to its current safe mapping.
    pub async fn resolve_inbound(
        &self,
        requester: &InboundRequester,
        request: ResolveInboundRequest,
    ) -> RelayResult<Value> {
        if let Some(reference) = &request.provider_event_reference {
            validate_external_reference(reference).map_err(|_| {
                RelayError::Validation(
                    "providerEventReference must be an opaque, safe reference (never a number, address or URI).",
                )
            })?;
        }
        if request.assignment_generation.is_some_and(|g| g < 1) {
            return Err(RelayError::Validation(
                "assignmentGeneration must be a positive integer.",
            ));
        }
        let now = self.now();
        let mut trace = Trace::default();
        let outcome = self.resolve_inner(&request, &mut trace, now).await;
        let decision = Uuid::new_v4();
        let (outcome_code, reason, valid_until, body) = match &outcome {
            Ok((body, until)) => ("RESOLVED", "RESOLVED", Some(*until), Some(body.clone())),
            Err(Ok(refusal)) if refusal.ambiguous => ("AMBIGUOUS", refusal.reason, None, None),
            Err(Ok(refusal)) => ("NO_ACTIVE_RELAY_MAPPING", refusal.reason, None, None),
            Err(Err(_)) => ("NO_ACTIVE_RELAY_MAPPING", "INTERNAL_FAILURE", None, None),
        };
        let (scope, context, session) = match trace.scope {
            Some((s, c, se)) => (Some(s), Some(c), Some(se)),
            None => (None, None, None),
        };
        db::exec(
            &self.db,
            "INSERT INTO relay_inbound_resolutions (id, relay_number_id, assignment_id, assignment_generation, \
               tenant_id, application_id, environment_id, context_id, relay_session_id, context_version, \
               session_version, outcome, reason, presented_generation, provider_event_reference, \
               provider_observed_at, provider_started_at, valid_until, requester_service, request_id, \
               correlation_id, decided_at) \
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15, $16, $17, $18, $19, $20, $21, $22)",
            vec![
                decision.into(),
                trace.relay_number_id.into(),
                trace.assignment.map(|a| a.0).into(),
                trace.assignment.map(|a| a.1).into(),
                scope.map(|s| s.tenant_id()).into(),
                scope.map(|s| s.application_id()).into(),
                scope.map(|s| s.environment_id()).into(),
                context.into(),
                session.into(),
                trace.versions.map(|v| v.0).into(),
                trace.versions.map(|v| v.1).into(),
                outcome_code.into(),
                reason.into(),
                request.assignment_generation.into(),
                request.provider_event_reference.clone().into(),
                request.provider_observed_at.into(),
                request.provider_started_at.into(),
                valid_until.into(),
                requester.service.as_str().into(),
                requester.request_id.as_str().into(),
                requester.correlation_id.as_str().into(),
                now.into(),
            ],
        )
        .await?;
        crate::metrics::inc(
            "privacy_relay_inbound_resolutions_total",
            &[("outcome", outcome_code), ("reason", reason)],
        );
        match outcome {
            Ok(_) => {
                let mut body = body.unwrap_or(Value::Null);
                body["decisionId"] = json!(decision);
                tracing::info!(inbound_resolution_id = %decision, relay_session_id = ?session, "inbound relay mapping resolved");
                Ok(json!({ "resolution": body }))
            }
            Err(Ok(refusal)) if refusal.ambiguous => {
                Err(RelayError::Refused(ErrorCode::RelayMappingAmbiguous))
            }
            Err(Ok(refusal)) => {
                tracing::info!(inbound_resolution_id = %decision, reason = refusal.reason, "no active relay mapping");
                Err(RelayError::Refused(ErrorCode::RelayMappingNotActive))
            }
            Err(Err(e)) => Err(e),
        }
    }

    async fn resolve_inner(
        &self,
        req: &ResolveInboundRequest,
        trace: &mut Trace,
        now: DateTime<Utc>,
    ) -> Result<(Value, DateTime<Utc>), Result<Refusal, RelayError>> {
        let skew = Duration::seconds(MAX_SKEW_SECONDS);
        if req.provider_observed_at > now + skew {
            return Err(Ok(refuse("EVENT_FROM_FUTURE")));
        }
        if req.provider_observed_at < now - Duration::minutes(MAX_EVENT_AGE_MINUTES) {
            return Err(Ok(refuse("STALE_EVENT")));
        }
        if req
            .provider_started_at
            .is_some_and(|s| s > req.provider_observed_at + skew)
        {
            return Err(Ok(refuse("INCONSISTENT_TIMESTAMPS")));
        }
        let Ok(called) = canonical_public_number(&req.called_relay_identity) else {
            return Err(Ok(refuse("UNKNOWN_RELAY_IDENTITY")));
        };
        let number: Option<NumberRef> = db::one(
            &self.db,
            "SELECT id, generation FROM relay_number_inventory WHERE canonical_public_e164 = $1",
            vec![called.e164.clone().into()],
        )
        .await
        .map_err(Err)?;
        drop(called);
        let Some(number) = number else {
            return Err(Ok(refuse("UNKNOWN_RELAY_IDENTITY")));
        };
        trace.relay_number_id = Some(number.id);
        // Unlocked probe to learn the scope (lock order: context, session, assignment).
        let probe: Vec<AssignmentRow> = db::all(
            &self.db,
            &format!(
                "SELECT {ASSIGNMENT_COLUMNS} FROM relay_number_assignments \
                 WHERE relay_number_id = $1 AND status = 'ACTIVE' ORDER BY assigned_at, id"
            ),
            vec![number.id.into()],
        )
        .await
        .map_err(Err)?;
        let a = match single_mapping(probe) {
            Ok(a) => a,
            Err("AMBIGUOUS_MAPPING") => {
                self.ambiguity_incident(number.id, number.generation, now)
                    .await;
                return Err(Ok(Refusal {
                    reason: "AMBIGUOUS_MAPPING",
                    ambiguous: true,
                }));
            }
            Err(reason) => return Err(Ok(refuse(reason))),
        };
        trace.assignment = Some((a.id, a.assignment_generation));
        if req.assignment_id.is_some_and(|id| id != a.id) {
            return Err(Ok(refuse("ASSIGNMENT_MISMATCH")));
        }
        if req
            .assignment_generation
            .is_some_and(|g| g != a.assignment_generation)
        {
            return Err(Ok(refuse("GENERATION_MISMATCH")));
        }
        // A delayed event of an earlier assignment originated before this one.
        let origin = req.provider_started_at.unwrap_or(req.provider_observed_at);
        if origin < a.assigned_at {
            return Err(Ok(refuse("PREDATES_ASSIGNMENT")));
        }
        if a.expires_at <= now {
            return Err(Ok(refuse("ASSIGNMENT_EXPIRED")));
        }
        let scope = RelayScope::from_row(a.tenant_id, a.application_id, a.environment_id);
        trace.scope = Some((scope, a.context_id, a.relay_session_id));
        let txn = self.db.begin().await.map_err(|e| Err(e.into()))?;
        let ctx = ContextAggregate::load(&txn, scope, a.context_id, Lock::Share)
            .await
            .map_err(Err)?;
        let session = SessionAggregate::load(&txn, scope, a.relay_session_id, Lock::Share)
            .await
            .map_err(Err)?;
        trace.versions = Some((ctx.row.version, session.row.version));
        // Re-read the mapping under the locks: still the one ACTIVE holder, still bound.
        let locked: Vec<AssignmentRow> = db::all(
            &txn,
            &format!(
                "SELECT {ASSIGNMENT_COLUMNS} FROM relay_number_assignments \
                 WHERE relay_number_id = $1 AND status = 'ACTIVE' FOR SHARE"
            ),
            vec![number.id.into()],
        )
        .await
        .map_err(Err)?;
        match single_mapping(locked) {
            Ok(l) if l.id == a.id && l.status().map_err(Err)? == RelayAssignmentStatus::Active => {}
            Err("AMBIGUOUS_MAPPING") => {
                drop(txn);
                self.ambiguity_incident(number.id, number.generation, now)
                    .await;
                return Err(Ok(Refusal {
                    reason: "AMBIGUOUS_MAPPING",
                    ambiguous: true,
                }));
            }
            _ => return Err(Ok(refuse("NO_ACTIVE_ASSIGNMENT"))),
        }
        if session.row.relay_identity_binding_ref != Some(a.id) {
            return Err(Ok(refuse("BINDING_MISMATCH")));
        }
        let authority = inbound_authority(self, &txn, &ctx, &session, None, now)
            .await
            .map_err(Err)?;
        txn.commit().await.map_err(|e| Err(e.into()))?;
        let Some(until) = authority.valid_until else {
            return Err(Ok(refuse(
                authority
                    .denial
                    .map(|r| r.as_str())
                    .unwrap_or("NO_INBOUND_AUTHORITY"),
            )));
        };
        let valid_until = until.min(a.expires_at);
        let sources: Vec<Value> = authority
            .sources
            .iter()
            .map(|(c, targets)| {
                json!({
                    "contactReferenceId": c,
                    "allowedTargets": targets.iter().map(subject_json).collect::<Vec<_>>(),
                })
            })
            .collect();
        Ok((
            json!({
                "outcome": "RESOLVED",
                // The inbound call's owner scope: the relay number itself carries no
                // tenant, so Telephony learns it only from a RESOLVED mapping.
                "tenantId": ctx.row.tenant_id,
                "applicationId": ctx.row.application_id,
                "environmentId": ctx.row.environment_id,
                "contextId": ctx.id(),
                "contextVersion": ctx.row.version,
                "relaySessionId": session.row.id,
                "relaySessionVersion": session.row.version,
                "assignmentId": a.id,
                "assignmentGeneration": a.assignment_generation,
                "strategy": session.row.strategy().map_err(Err)?.as_str(),
                "channel": CommunicationChannel::Voice.as_str(),
                "route": CommunicationRoute::CallbackRelay.as_str(),
                "inboundSources": sources,
                "validUntil": ts(valid_until),
            }),
            valid_until,
        ))
    }

    /// Fail-closed incident signal: the ledger records the ambiguity (never a choice) and
    /// a deduplicated RELAY_MAPPING_AMBIGUOUS incident is raised for the operator.
    async fn ambiguity_incident(&self, number: Uuid, generation: i64, now: DateTime<Utc>) {
        tracing::error!(relay_number_id = %number, "INCIDENT: more than one active relay mapping; inbound resolution refused");
        let inventory = number.simple().to_string();
        crate::incidents::raise_best_effort(
            &self.db,
            crate::incidents::IncidentCategory::RelayMappingAmbiguous,
            &crate::incidents::dedupe_key(
                crate::incidents::IncidentCategory::RelayMappingAmbiguous,
                &[&inventory],
            ),
            now,
        )
        .await;
        let _ = ledger(
            &self.db,
            number,
            "RELAY_MAPPING_AMBIGUOUS",
            None,
            RelayNumberStatus::Assigned,
            generation,
            None,
            Some("AMBIGUOUS_MAPPING"),
            ("SYSTEM", "cfmeet.privacy-relay"),
            now,
        )
        .await;
    }

    /// The scope of a RESOLVED inbound resolution of `service`, still within its
    /// protection window and with its assignment still ACTIVE.
    pub(crate) async fn inbound_protection_scope(
        &self,
        service: &ServiceId,
        resolution: Uuid,
    ) -> RelayResult<InboundScope> {
        let now = self.now();
        let row: Option<ResolvedRow> = db::one(
            &self.db,
            "SELECT r.tenant_id, r.application_id, r.environment_id, r.context_id, r.relay_session_id, \
                    a.country_iso2 \
             FROM relay_inbound_resolutions r JOIN relay_number_assignments a ON a.id = r.assignment_id \
             WHERE r.id = $1 AND r.outcome = 'RESOLVED' AND r.requester_service = $2 \
               AND r.decided_at > $3 AND r.decided_at <= $4 AND a.status = 'ACTIVE' AND a.expires_at > $4",
            vec![
                resolution.into(),
                service.as_str().into(),
                (now - Duration::seconds(PROTECTION_WINDOW_SECONDS)).into(),
                now.into(),
            ],
        )
        .await?;
        let row = row.ok_or(RelayError::Refused(ErrorCode::RelayMappingNotActive))?;
        Ok(InboundScope {
            scope: RelayScope::from_row(
                row.tenant_id
                    .ok_or(RelayError::Refused(ErrorCode::InternalError))?,
                row.application_id
                    .ok_or(RelayError::Refused(ErrorCode::InternalError))?,
                row.environment_id
                    .ok_or(RelayError::Refused(ErrorCode::InternalError))?,
            ),
            context_id: row
                .context_id
                .ok_or(RelayError::Refused(ErrorCode::InternalError))?,
            relay_session_id: row
                .relay_session_id
                .ok_or(RelayError::Refused(ErrorCode::InternalError))?,
            country: row.country_iso2,
        })
    }

    /// Whom an inbound caller (now a ContactReference) may reach in the mapped session,
    /// evaluated now.
    pub(crate) async fn inbound_caller_targets(
        &self,
        scope: &InboundScope,
        caller: Uuid,
    ) -> RelayResult<Vec<Value>> {
        let now = self.now();
        let txn = self.db.begin().await?;
        let ctx = ContextAggregate::load(&txn, scope.scope, scope.context_id, Lock::Share).await?;
        let session =
            SessionAggregate::load(&txn, scope.scope, scope.relay_session_id, Lock::Share).await?;
        let authority = inbound_authority(self, &txn, &ctx, &session, Some(caller), now).await?;
        txn.commit().await?;
        Ok(authority
            .sources
            .into_iter()
            .flat_map(|(_, t)| t)
            .map(|t| subject_json(&t))
            .collect())
    }
}

/// Scope fixed by a resolved inbound mapping.
#[derive(Debug, Clone)]
pub struct InboundScope {
    pub scope: RelayScope,
    pub context_id: Uuid,
    pub relay_session_id: Uuid,
    pub country: String,
}

#[derive(Debug, sea_orm::FromQueryResult)]
struct ResolvedRow {
    tenant_id: Option<Uuid>,
    application_id: Option<Uuid>,
    environment_id: Option<Uuid>,
    context_id: Option<Uuid>,
    relay_session_id: Option<Uuid>,
    country_iso2: String,
}

#[derive(Debug, sea_orm::FromQueryResult)]
struct NumberRef {
    id: Uuid,
    generation: i64,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(id: u128) -> AssignmentRow {
        AssignmentRow {
            id: Uuid::from_u128(id),
            relay_number_id: Uuid::from_u128(9),
            tenant_id: Uuid::from_u128(1),
            application_id: Uuid::from_u128(2),
            environment_id: Uuid::from_u128(3),
            context_id: Uuid::from_u128(4),
            relay_session_id: Uuid::from_u128(5),
            assignment_generation: 1,
            country_iso2: "GB".into(),
            region: None,
            capabilities: vec!["VOICE_INBOUND".into()],
            status: "ACTIVE".into(),
            end_reason: None,
            assigned_at: Utc::now(),
            expires_at: Utc::now(),
            released_at: None,
            cooldown_until: None,
        }
    }

    #[test]
    fn ambiguity_fails_closed_and_never_picks_one() {
        assert_eq!(single_mapping(vec![]).unwrap_err(), "NO_ACTIVE_ASSIGNMENT");
        assert_eq!(single_mapping(vec![row(1)]).unwrap().id, Uuid::from_u128(1));
        assert_eq!(
            single_mapping(vec![row(1), row(2)]).unwrap_err(),
            "AMBIGUOUS_MAPPING"
        );
    }
}
