//! Relay-number assignment: allocation, release, containment and the release / cooldown
//! / drain workers (§34).

use std::collections::BTreeSet;

use chrono::{DateTime, Utc};
use sea_orm::{ConnectionTrait, DatabaseTransaction, TransactionTrait};
use serde::Deserialize;
use serde_json::{json, Value};
use uuid::Uuid;

use super::{AssignmentRow, InventoryRow, ASSIGNMENT_COLUMNS, INVENTORY_COLUMNS};
use crate::{
    audit::{self, AuditDraft},
    contact_vault::verification::contact_use,
    context::{aggregate::ContextAggregate, repository::Lock},
    db,
    error::{ErrorCode, RelayError, RelayResult},
    models::{ContextRow, CONTEXT_COLUMNS},
    relay::{
        authorization::ContactUse,
        session::{session_row, SessionAggregate},
    },
    scope::CommandContext,
    service::RelayService,
    vocabulary::{
        AuditEventType, CommunicationContextStatus, CommunicationRoute, PrivacyRelaySessionStatus,
        RelayAssignmentEndReason, RelayAssignmentStatus, RelayNumberCapability, RelayNumberStatus,
    },
};

/// `POST /internal/v1/sessions/{id}/relay-number-assignment` request body.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AssignRelayNumberRequest {
    /// ISO 3166-1 alpha-2 country of the relay number.
    pub country: String,
    #[serde(default)]
    pub region: Option<String>,
    /// Required provisioned capabilities (VOICE_INBOUND, VOICE_OUTBOUND).
    pub capabilities: Vec<String>,
}

struct Criteria {
    country: String,
    region: Option<String>,
    capabilities: Vec<String>,
}

fn criteria(req: AssignRelayNumberRequest) -> RelayResult<Criteria> {
    if req.country.len() != 2 || !req.country.bytes().all(|b| b.is_ascii_uppercase()) {
        return Err(RelayError::Validation(
            "country must be an ISO 3166-1 alpha-2 country code.",
        ));
    }
    if let Some(region) = &req.region {
        let ok = !region.is_empty()
            && region.len() <= 64
            && region.as_bytes()[0].is_ascii_alphanumeric()
            && region
                .bytes()
                .all(|b| b.is_ascii_uppercase() || b.is_ascii_digit() || b == b'_' || b == b'-');
        if !ok {
            return Err(RelayError::Validation(
                "region must be an upper-case region code of at most 64 characters.",
            ));
        }
    }
    let mut capabilities = BTreeSet::new();
    for c in &req.capabilities {
        let c = RelayNumberCapability::parse(c).ok_or(RelayError::Validation(
            "capabilities may only contain VOICE_INBOUND and VOICE_OUTBOUND.",
        ))?;
        if !capabilities.insert(c.as_str().to_string()) {
            return Err(RelayError::Validation(
                "capabilities must not repeat a value.",
            ));
        }
    }
    if capabilities.is_empty() {
        return Err(RelayError::Validation("capabilities must not be empty."));
    }
    Ok(Criteria {
        country: req.country,
        region: req.region,
        capabilities: capabilities.into_iter().collect(),
    })
}

/// The audit attribution of an assignment change (the caller holds the context and
/// session row locks).
pub(crate) struct AssignmentAudit<'a> {
    pub cmd: &'a CommandContext,
    pub context_id: Uuid,
    pub context_version: i64,
    pub context_status: &'static str,
    pub session_id: Uuid,
    pub session_version: i64,
}

#[derive(Debug, sea_orm::FromQueryResult)]
struct Candidate {
    id: Uuid,
    context_id: Uuid,
}

#[derive(Debug, sea_orm::FromQueryResult)]
struct Generation {
    generation: i64,
}

async fn assignment_of_session<C: ConnectionTrait>(
    conn: &C,
    cmd: &CommandContext,
    session_id: Uuid,
    lock: bool,
) -> RelayResult<Option<AssignmentRow>> {
    let scope = cmd.scope();
    db::one(
        conn,
        &format!(
            "SELECT {ASSIGNMENT_COLUMNS} FROM relay_number_assignments \
             WHERE relay_session_id = $1 AND tenant_id = $2 AND application_id = $3 AND environment_id = $4{}",
            if lock { " FOR UPDATE" } else { "" }
        ),
        vec![
            session_id.into(),
            scope.tenant_id().into(),
            scope.application_id().into(),
            scope.environment_id().into(),
        ],
    )
    .await
}

/// Appends one row of the inventory ledger (no tenant data, no number).
#[allow(clippy::too_many_arguments)]
pub(crate) async fn ledger<C: ConnectionTrait>(
    conn: &C,
    number: Uuid,
    event: &str,
    from: Option<RelayNumberStatus>,
    to: RelayNumberStatus,
    generation: i64,
    assignment: Option<Uuid>,
    reason: Option<&str>,
    actor: (&str, &str),
    now: DateTime<Utc>,
) -> RelayResult<()> {
    db::exec(
        conn,
        "INSERT INTO relay_number_events (id, relay_number_id, event_type, from_status, to_status, generation, \
           assignment_id, reason, actor_kind, actor_ref, occurred_at) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11)",
        vec![
            Uuid::new_v4().into(),
            number.into(),
            event.into(),
            from.map(|s| s.as_str().to_string()).into(),
            to.as_str().into(),
            generation.into(),
            assignment.into(),
            reason.map(str::to_string).into(),
            actor.0.into(),
            actor.1.into(),
            now.into(),
        ],
    )
    .await
    .map(|_| ())
}

async fn inventory_for_update(txn: &DatabaseTransaction, id: Uuid) -> RelayResult<InventoryRow> {
    db::one(
        txn,
        &format!("SELECT {INVENTORY_COLUMNS} FROM relay_number_inventory WHERE id = $1 FOR UPDATE"),
        vec![id.into()],
    )
    .await?
    .ok_or(RelayError::Refused(ErrorCode::InternalError))
}

fn context_refusal(status: CommunicationContextStatus) -> RelayError {
    RelayError::Refused(match status {
        CommunicationContextStatus::Active => ErrorCode::InternalError,
        CommunicationContextStatus::Suspended => ErrorCode::ContextSuspended,
        CommunicationContextStatus::Revoked => ErrorCode::ContextRevoked,
        CommunicationContextStatus::Expired => ErrorCode::ContextExpired,
        CommunicationContextStatus::Completed => ErrorCode::ContextNotActive,
    })
}

impl RelayService {
    pub fn relay_number_pool_enabled(&self) -> bool {
        self.relay_numbers.enabled.enabled()
    }

    /// Lends a relay number to a session: idempotent per session (a replay with the same
    /// criteria answers the same assignment; there is never a second one). Returns the
    /// HTTP status (201 new, 200 existing) and the safe body.
    pub async fn assign_relay_number(
        &self,
        cmd: &CommandContext,
        session_id: Uuid,
        req: AssignRelayNumberRequest,
    ) -> RelayResult<(u16, Value)> {
        let wanted = criteria(req)?;
        let scope = cmd.scope();
        let now = self.now();
        let txn = self.db.begin().await?;
        // Lock order: context, then session (serializes every request for this session).
        let probe = session_row(&txn, scope, session_id, Lock::None)
            .await?
            .ok_or(RelayError::Refused(ErrorCode::SessionNotFound))?;
        let ctx = ContextAggregate::load(&txn, scope, probe.context_id, Lock::Update).await?;
        let session = SessionAggregate::load(&txn, scope, session_id, Lock::Update).await?;
        if session.row.relay_identity_binding_ref.is_some() {
            let existing = assignment_of_session(&txn, cmd, session_id, false)
                .await?
                .ok_or(RelayError::Refused(ErrorCode::InternalError))?;
            txn.rollback().await?;
            let same = existing.country_iso2 == wanted.country
                && existing.region == wanted.region
                && existing.capabilities == wanted.capabilities;
            if !same {
                return Err(RelayError::Coded(
                    ErrorCode::IdempotencyConflict,
                    "This relay session already holds a relay number assignment with different criteria.",
                ));
            }
            if existing.status()? != RelayAssignmentStatus::Active {
                return Err(RelayError::Refused(ErrorCode::RelayMappingNotActive));
            }
            return Ok((200, json!({ "assignment": existing.view()? })));
        }
        // New assignments only: both kill switches.
        self.require_enabled()?;
        if !self.relay_number_pool_enabled() {
            return Err(RelayError::Refused(ErrorCode::RelayNumberPoolDisabled));
        }
        let ctx_status = ctx.status()?;
        if ctx_status != CommunicationContextStatus::Active {
            return Err(context_refusal(ctx_status));
        }
        let row = &session.row;
        if row.status()? != PrivacyRelaySessionStatus::Active
            || now < row.starts_at
            || now >= row.final_expires_at
            || now >= ctx.row.expires_at
        {
            return Err(RelayError::Refused(ErrorCode::SessionNotActive));
        }
        let strategy = row.strategy()?;
        if !(strategy.permits(CommunicationRoute::PublicRelay)
            || strategy.permits(CommunicationRoute::CallbackRelay))
        {
            return Err(RelayError::Coded(
                ErrorCode::StrategyNotAllowed,
                "A relay number is only lent to a session whose strategy allows public or callback relay.",
            ));
        }
        // A "verified" session: every protected contact it names is enabled and
        // effectively VERIFIED now (Contact Vault on).
        let contacts: Vec<Uuid> = session
            .participants
            .iter()
            .filter_map(|p| p.contact_ref)
            .collect();
        if contacts.is_empty() {
            return Err(RelayError::Coded(
                ErrorCode::StrategyNotAllowed,
                "A relay number is only lent to a session with a protected contact participant.",
            ));
        }
        if !self.contact_vault_enabled() {
            return Err(RelayError::Refused(ErrorCode::ContactVaultDisabled));
        }
        for contact in contacts {
            match contact_use(&txn, scope, contact, now).await? {
                ContactUse::Verified => {}
                ContactUse::Expired => {
                    return Err(RelayError::Refused(ErrorCode::ContactVerificationExpired))
                }
                ContactUse::Blocked => return Err(RelayError::Refused(ErrorCode::ContactBlocked)),
                ContactUse::NotVerified => {
                    return Err(RelayError::Refused(ErrorCode::ContactNotVerified))
                }
                ContactUse::Unavailable => {
                    return Err(RelayError::Refused(ErrorCode::ContactNotFound))
                }
            }
        }
        // Deterministic, concurrency-safe selection: least recently changed eligible
        // number; a row another allocation holds is skipped, never shared.
        let candidate: Option<InventoryRow> = db::one(
            &txn,
            &format!(
                "SELECT {INVENTORY_COLUMNS} FROM relay_number_inventory \
                 WHERE operational_status = 'AVAILABLE' AND country_iso2 = $1 \
                   AND ($2::text IS NULL OR region = $2) AND capabilities @> $3::text[] \
                   AND (cooldown_until IS NULL OR cooldown_until <= $4) \
                 ORDER BY updated_at, id LIMIT 1 FOR UPDATE SKIP LOCKED"
            ),
            vec![
                wanted.country.clone().into(),
                wanted.region.clone().into(),
                wanted.capabilities.clone().into(),
                now.into(),
            ],
        )
        .await?;
        let Some(number) = candidate else {
            return Err(RelayError::Refused(ErrorCode::RelayNumberUnavailable));
        };
        let generation: Generation = db::one(
            &txn,
            "UPDATE relay_number_inventory SET operational_status = 'ASSIGNED', generation = generation + 1, \
               cooldown_until = NULL, updated_at = GREATEST(updated_at, $2) \
             WHERE id = $1 AND operational_status = 'AVAILABLE' RETURNING generation",
            vec![number.id.into(), now.into()],
        )
        .await?
        .ok_or(RelayError::Refused(ErrorCode::RelayNumberUnavailable))?;
        let id = Uuid::new_v4();
        db::exec(
            &txn,
            "INSERT INTO relay_number_assignments (id, relay_number_id, tenant_id, application_id, environment_id, \
               context_id, relay_session_id, assignment_generation, country_iso2, region, capabilities, status, \
               assigned_at, expires_at, requested_by_service, request_id, correlation_id, created_at, updated_at) \
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, 'ACTIVE', $12, $13, $14, $15, $16, $12, $12)",
            vec![
                id.into(),
                number.id.into(),
                scope.tenant_id().into(),
                scope.application_id().into(),
                scope.environment_id().into(),
                ctx.id().into(),
                session_id.into(),
                generation.generation.into(),
                wanted.country.clone().into(),
                wanted.region.clone().into(),
                wanted.capabilities.clone().into(),
                now.into(),
                row.final_expires_at.into(),
                cmd.actor_service().as_str().into(),
                cmd.request_id().as_str().into(),
                cmd.correlation_id().as_str().into(),
            ],
        )
        .await?;
        // The session's relay identity binding is the assignment id (never a provider id).
        db::exec(
            &txn,
            "UPDATE privacy_relay_sessions SET relay_identity_binding_ref = $2, updated_at = GREATEST(updated_at, $3) \
             WHERE id = $1",
            vec![session_id.into(), id.into(), now.into()],
        )
        .await?;
        audit::append(
            &txn,
            cmd,
            self.runtime,
            AuditDraft {
                event_type: AuditEventType::RelayNumberAssigned,
                context_id: ctx.id(),
                context_version: ctx.row.version,
                context_status: ctx_status.as_str(),
                session: Some((session_id, row.version)),
                session_status: Some(row.status()?.as_str()),
                enforcement_job_id: None,
                data: audit::data(vec![
                    ("assignment_id", json!(id)),
                    ("assignment_generation", json!(generation.generation)),
                    ("country", json!(wanted.country)),
                ]),
            },
            now,
        )
        .await?;
        ledger(
            &txn,
            number.id,
            "RELAY_NUMBER_ASSIGNED",
            Some(RelayNumberStatus::Available),
            RelayNumberStatus::Assigned,
            generation.generation,
            Some(id),
            None,
            ("SERVICE", cmd.actor_service().as_str()),
            now,
        )
        .await?;
        let assignment = assignment_of_session(&txn, cmd, session_id, false)
            .await?
            .ok_or(RelayError::Refused(ErrorCode::InternalError))?;
        txn.commit().await?;
        tracing::info!(relay_session_id = %session_id, assignment_id = %id, assignment_generation = generation.generation, "relay number assigned");
        Ok((201, json!({ "assignment": assignment.view()? })))
    }

    /// The session's relay-number assignment (any status).
    pub async fn get_relay_assignment(
        &self,
        cmd: &CommandContext,
        session_id: Uuid,
    ) -> RelayResult<Value> {
        session_row(&self.db, cmd.scope(), session_id, Lock::None)
            .await?
            .ok_or(RelayError::Refused(ErrorCode::SessionNotFound))?;
        let row = assignment_of_session(&self.db, cmd, session_id, false)
            .await?
            .ok_or(RelayError::Refused(ErrorCode::RelayAssignmentNotFound))?;
        Ok(json!({ "assignment": row.view()? }))
    }

    /// Explicit release (restrictive: never gated by a kill switch; idempotent — an
    /// already ended assignment answers its current state).
    pub async fn release_relay_assignment(
        &self,
        cmd: &CommandContext,
        session_id: Uuid,
    ) -> RelayResult<Value> {
        let scope = cmd.scope();
        let now = self.now();
        let txn = self.db.begin().await?;
        let probe = session_row(&txn, scope, session_id, Lock::None)
            .await?
            .ok_or(RelayError::Refused(ErrorCode::SessionNotFound))?;
        let ctx = ContextAggregate::load(&txn, scope, probe.context_id, Lock::Update).await?;
        let session = SessionAggregate::load(&txn, scope, session_id, Lock::Update).await?;
        if assignment_of_session(&txn, cmd, session_id, false)
            .await?
            .is_none()
        {
            return Err(RelayError::Refused(ErrorCode::RelayAssignmentNotFound));
        }
        self.end_relay_assignment_in(
            &txn,
            &AssignmentAudit {
                cmd,
                context_id: ctx.id(),
                context_version: ctx.row.version,
                context_status: ctx.status()?.as_str(),
                session_id,
                session_version: session.row.version,
            },
            RelayAssignmentEndReason::Released,
            now,
        )
        .await?;
        let row = assignment_of_session(&txn, cmd, session_id, false)
            .await?
            .ok_or(RelayError::Refused(ErrorCode::InternalError))?;
        txn.commit().await?;
        Ok(json!({ "assignment": row.view()? }))
    }

    /// Ends the ACTIVE assignment of a session (if any) in the caller's transaction:
    /// routing authority stops now, the assignment and its number enter COOLDOWN.
    /// Returns the assignment id it ended.
    pub(crate) async fn end_relay_assignment_in(
        &self,
        txn: &DatabaseTransaction,
        at: &AssignmentAudit<'_>,
        reason: RelayAssignmentEndReason,
        now: DateTime<Utc>,
    ) -> RelayResult<Option<Uuid>> {
        let Some(a) = assignment_of_session(txn, at.cmd, at.session_id, true).await? else {
            return Ok(None);
        };
        if a.status()? != RelayAssignmentStatus::Active {
            return Ok(None);
        }
        let cooldown_until = now + self.relay_numbers.cooldown;
        db::exec(
            txn,
            "UPDATE relay_number_assignments SET status = 'COOLDOWN', end_reason = $2, released_at = $3, \
               cooldown_until = $4, updated_at = GREATEST(updated_at, $3) WHERE id = $1 AND status = 'ACTIVE'",
            vec![a.id.into(), reason.as_str().into(), now.into(), cooldown_until.into()],
        )
        .await?;
        let number = inventory_for_update(txn, a.relay_number_id).await?;
        let from = number.status()?;
        // An ASSIGNED number cools down; a DRAINING / UNAVAILABLE one keeps its status
        // but carries the cooldown (it is never offered before it ends either way).
        let to = if from == RelayNumberStatus::Assigned {
            RelayNumberStatus::Cooldown
        } else {
            from
        };
        db::exec(
            txn,
            "UPDATE relay_number_inventory SET operational_status = $2, \
               cooldown_until = GREATEST(COALESCE(cooldown_until, $3), $3), updated_at = GREATEST(updated_at, $4) \
             WHERE id = $1",
            vec![number.id.into(), to.as_str().into(), cooldown_until.into(), now.into()],
        )
        .await?;
        for (event, data) in [
            (
                AuditEventType::RelayNumberReleased,
                audit::data(vec![
                    ("assignment_id", json!(a.id)),
                    ("assignment_generation", json!(a.assignment_generation)),
                    ("end_reason", json!(reason.as_str())),
                ]),
            ),
            (
                AuditEventType::RelayNumberCooldownStarted,
                audit::data(vec![
                    ("assignment_id", json!(a.id)),
                    ("assignment_generation", json!(a.assignment_generation)),
                    (
                        "cooldown_seconds",
                        json!(self.relay_numbers.cooldown.num_seconds()),
                    ),
                ]),
            ),
        ] {
            audit::append(
                txn,
                at.cmd,
                self.runtime,
                AuditDraft {
                    event_type: event,
                    context_id: at.context_id,
                    context_version: at.context_version,
                    context_status: at.context_status,
                    session: Some((at.session_id, at.session_version)),
                    session_status: None,
                    enforcement_job_id: None,
                    data,
                },
                now,
            )
            .await?;
        }
        let actor = at.cmd.actor_service().as_str().to_string();
        let actor_kind = match at.cmd.actor_kind() {
            crate::scope::ActorKind::Application => "SERVICE",
            crate::scope::ActorKind::System => "SYSTEM",
        };
        ledger(
            txn,
            number.id,
            "RELAY_NUMBER_RELEASED",
            Some(from),
            to,
            number.generation,
            Some(a.id),
            Some(reason.as_str()),
            (actor_kind, &actor),
            now,
        )
        .await?;
        ledger(
            txn,
            number.id,
            "RELAY_NUMBER_COOLDOWN_STARTED",
            Some(to),
            to,
            number.generation,
            Some(a.id),
            None,
            (actor_kind, &actor),
            now,
        )
        .await?;
        tracing::info!(relay_session_id = %at.session_id, assignment_id = %a.id, end_reason = reason.as_str(), "relay number assignment ended; cooldown started");
        Ok(Some(a.id))
    }

    /// Keeps a live session's ACTIVE assignment inside the session's (possibly narrowed)
    /// final authority; an assignment whose window is gone ends now.
    pub(crate) async fn contain_relay_assignment_in(
        &self,
        txn: &DatabaseTransaction,
        at: &AssignmentAudit<'_>,
        final_expires_at: DateTime<Utc>,
        now: DateTime<Utc>,
    ) -> RelayResult<()> {
        let Some(a) = assignment_of_session(txn, at.cmd, at.session_id, true).await? else {
            return Ok(());
        };
        if a.status()? != RelayAssignmentStatus::Active || a.expires_at <= final_expires_at {
            return Ok(());
        }
        if final_expires_at <= now || final_expires_at <= a.assigned_at {
            self.end_relay_assignment_in(txn, at, RelayAssignmentEndReason::AssignmentExpired, now)
                .await?;
            return Ok(());
        }
        db::exec(
            txn,
            "UPDATE relay_number_assignments SET expires_at = $2, updated_at = GREATEST(updated_at, $3) \
             WHERE id = $1 AND status = 'ACTIVE'",
            vec![a.id.into(), final_expires_at.into(), now.into()],
        )
        .await?;
        Ok(())
    }

    /// Worker: ends ACTIVE assignments whose own validity has passed (multi-instance safe:
    /// context rows are claimed with SKIP LOCKED, the assignment is re-checked locked).
    pub async fn expire_relay_assignments(&self, limit: u64) -> RelayResult<usize> {
        let now = self.now();
        let candidates: Vec<Candidate> = db::all(
            &self.db,
            "SELECT relay_session_id AS id, context_id FROM relay_number_assignments \
             WHERE status = 'ACTIVE' AND expires_at <= $1 ORDER BY expires_at, id LIMIT $2",
            vec![now.into(), (limit.clamp(1, 1000) as i64).into()],
        )
        .await?;
        let mut ended = 0;
        for c in candidates {
            let txn = self.db.begin().await?;
            let ctx_row: Option<ContextRow> = db::one(
                &txn,
                &format!("SELECT {CONTEXT_COLUMNS} FROM communication_contexts WHERE id = $1 FOR UPDATE SKIP LOCKED"),
                vec![c.context_id.into()],
            )
            .await?;
            let Some(ctx_row) = ctx_row else {
                txn.rollback().await?;
                continue;
            };
            let cmd = crate::workers::system_cmd(&ctx_row)?;
            let Some(session) = session_row(&txn, cmd.scope(), c.id, Lock::Update).await? else {
                txn.rollback().await?;
                continue;
            };
            let due = assignment_of_session(&txn, &cmd, c.id, true)
                .await?
                .is_some_and(|a| a.status == "ACTIVE" && a.expires_at <= now);
            if !due {
                txn.rollback().await?;
                continue;
            }
            let at = AssignmentAudit {
                cmd: &cmd,
                context_id: ctx_row.id,
                context_version: ctx_row.version,
                context_status: ctx_row.status()?.as_str(),
                session_id: session.id,
                session_version: session.version,
            };
            if self
                .end_relay_assignment_in(
                    &txn,
                    &at,
                    RelayAssignmentEndReason::AssignmentExpired,
                    now,
                )
                .await?
                .is_some()
            {
                ended += 1;
            }
            txn.commit().await?;
        }
        Ok(ended)
    }

    /// Worker: finishes cooldowns — the assignment reaches its terminal state and the
    /// number returns to AVAILABLE (or, when it was draining, becomes UNAVAILABLE).
    pub async fn complete_relay_cooldowns(&self, limit: u64) -> RelayResult<usize> {
        let now = self.now();
        let txn = self.db.begin().await?;
        let due: Vec<AssignmentRow> = db::all(
            &txn,
            &format!(
                "SELECT {ASSIGNMENT_COLUMNS} FROM relay_number_assignments \
                 WHERE status = 'COOLDOWN' AND cooldown_until <= $1 \
                 ORDER BY cooldown_until, id LIMIT $2 FOR UPDATE SKIP LOCKED"
            ),
            vec![now.into(), (limit.clamp(1, 1000) as i64).into()],
        )
        .await?;
        for a in &due {
            let reason = a
                .end_reason()?
                .ok_or(RelayError::Refused(ErrorCode::InternalError))?;
            db::exec(
                &txn,
                "UPDATE relay_number_assignments SET status = $2, updated_at = GREATEST(updated_at, $3) \
                 WHERE id = $1 AND status = 'COOLDOWN'",
                vec![a.id.into(), reason.terminal_status().as_str().into(), now.into()],
            )
            .await?;
            let number = inventory_for_update(&txn, a.relay_number_id).await?;
            let cooled = number.cooldown_until.is_none_or(|u| u <= now);
            match number.status()? {
                RelayNumberStatus::Cooldown if cooled => {
                    db::exec(
                        &txn,
                        "UPDATE relay_number_inventory SET operational_status = 'AVAILABLE', cooldown_until = NULL, \
                           updated_at = GREATEST(updated_at, $2) WHERE id = $1",
                        vec![number.id.into(), now.into()],
                    )
                    .await?;
                    ledger(
                        &txn,
                        number.id,
                        "RELAY_NUMBER_AVAILABLE",
                        Some(RelayNumberStatus::Cooldown),
                        RelayNumberStatus::Available,
                        number.generation,
                        Some(a.id),
                        None,
                        ("SYSTEM", "cfmeet.privacy-relay"),
                        now,
                    )
                    .await?;
                }
                RelayNumberStatus::Draining if cooled => {
                    self.finish_drain_in(&txn, &number, now).await?;
                }
                _ => {}
            }
        }
        txn.commit().await?;
        Ok(due.len())
    }

    /// Worker: DRAINING numbers that nothing holds any more become UNAVAILABLE.
    pub async fn complete_relay_drains(&self, limit: u64) -> RelayResult<usize> {
        let now = self.now();
        let txn = self.db.begin().await?;
        let rows: Vec<InventoryRow> = db::all(
            &txn,
            &format!(
                "SELECT {INVENTORY_COLUMNS} FROM relay_number_inventory n \
                 WHERE operational_status = 'DRAINING' AND (cooldown_until IS NULL OR cooldown_until <= $1) \
                   AND NOT EXISTS (SELECT 1 FROM relay_number_assignments a \
                                   WHERE a.relay_number_id = n.id AND a.status IN ('ACTIVE', 'COOLDOWN')) \
                 ORDER BY draining_at, id LIMIT $2 FOR UPDATE SKIP LOCKED"
            ),
            vec![now.into(), (limit.clamp(1, 1000) as i64).into()],
        )
        .await?;
        for number in &rows {
            self.finish_drain_in(&txn, number, now).await?;
        }
        txn.commit().await?;
        Ok(rows.len())
    }

    async fn finish_drain_in(
        &self,
        txn: &DatabaseTransaction,
        number: &InventoryRow,
        now: DateTime<Utc>,
    ) -> RelayResult<()> {
        db::exec(
            txn,
            "UPDATE relay_number_inventory SET operational_status = 'UNAVAILABLE', unavailable_at = $2, \
               updated_at = GREATEST(updated_at, $2) WHERE id = $1 AND operational_status = 'DRAINING'",
            vec![number.id.into(), now.into()],
        )
        .await?;
        ledger(
            txn,
            number.id,
            "RELAY_NUMBER_DRAINED",
            Some(RelayNumberStatus::Draining),
            RelayNumberStatus::Unavailable,
            number.generation,
            None,
            None,
            ("SYSTEM", "cfmeet.privacy-relay"),
            now,
        )
        .await
    }
}

#[derive(sea_orm::FromQueryResult)]
struct CallerIdentityRow {
    assignment_id: Uuid,
    e164: String,
}

/// The caller identity of a public-network leg of `session` (Part E §44): the relay
/// number lent to THAT session — its active, outbound-capable relay identity binding —
/// as (assignment id, canonical E.164). `None` when the session has none: the leg then
/// cannot present any caller identity, and Telephony refuses to originate.
pub(crate) async fn session_caller_identity<C: ConnectionTrait>(
    conn: &C,
    scope: crate::scope::RelayScope,
    session: Uuid,
    now: DateTime<Utc>,
) -> RelayResult<Option<(Uuid, String)>> {
    let row: Option<CallerIdentityRow> = db::one(
        conn,
        "SELECT a.id AS assignment_id, i.canonical_public_e164 AS e164 \
         FROM privacy_relay_sessions s \
         JOIN relay_number_assignments a ON a.id = s.relay_identity_binding_ref \
         JOIN relay_number_inventory i ON i.id = a.relay_number_id \
         WHERE s.id = $1 AND s.tenant_id = $2 AND s.application_id = $3 AND s.environment_id = $4 \
           AND a.relay_session_id = s.id AND a.status = 'ACTIVE' AND a.expires_at > $5 \
           AND i.operational_status = 'ASSIGNED' AND 'VOICE_OUTBOUND' = ANY(a.capabilities)",
        vec![
            session.into(),
            scope.tenant_id().into(),
            scope.application_id().into(),
            scope.environment_id().into(),
            now.into(),
        ],
    )
    .await?;
    Ok(row.map(|r| (r.assignment_id, r.e164)))
}
