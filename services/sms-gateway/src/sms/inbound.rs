//! Inbound SMS (Part IV Workstream N).
//!
//! A provider callback has already been authenticated (signature, timestamp window) and
//! parsed by the adapter. Here:
//!
//! 1. A callback whose provider event id was already recorded is a DUPLICATE (answered
//!    with the original disposition; nothing changes).
//! 2. The destination is resolved by Numbering (`sms-destination-resolutions`) from its
//!    canonical state: only an ACTIVE assignment whose SMS family routes somewhere is
//!    ROUTED. Numbering unreachable fails closed: nothing is stored, the provider retries.
//! 3. ROUTED: the message is stored for the environment Numbering named (never one the
//!    callback claims): sealed content and sender, fingerprint, masked view, the
//!    assignment / revision / grant facts, the thread, an estimated usage record, STOP /
//!    START keywords, and `sms-gateway.message.received` in the same transaction.
//!    Immediately before commit (the final delivery boundary) Numbering re-verifies the
//!    INBOUND_SMS grant against its CURRENT state: a reassignment, routing change,
//!    suspension or release in between quarantines the message (ROUTING_CHANGED) instead.
//! 4. Anything else (unknown number, not active, released, not routed): QUARANTINED —
//!    sealed, attributed to no customer, purged after its retention, never delivered.
//!
//! The provider event record and the transport commit together, so concurrent duplicate
//! callbacks store the message once (the unique event reference decides).

use chrono::{DateTime, Utc};
use sea_orm::{ConnectionTrait, DbErr, TransactionTrait};
use serde::Serialize;
use sha2::{Digest, Sha256};
use uuid::Uuid;

use super::{
    controls::{self, OptKeyword},
    e164,
    events::{self, EventScope, MessageEvent},
    protection::{mask, Purpose},
    segments, store, threads, Direction, SmsError, SmsResult, Status,
};
use crate::{
    db,
    domain::RequestContext,
    numbering::{NumberingError, SmsDestination},
    providers::InboundMessage,
    state::AppState,
};

/// The acknowledgement returned to the provider (contract `CallbackAck`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CallbackAck {
    /// `ACCEPTED` (first delivery) or `DUPLICATE` (already recorded).
    pub status: &'static str,
    /// What the callback did: `APPLIED`, `STALE`, `UNMATCHED`, `QUARANTINED`, `REFUSED`.
    pub disposition: String,
}

impl CallbackAck {
    fn accepted(disposition: &str) -> Self {
        Self {
            status: "ACCEPTED",
            disposition: disposition.to_string(),
        }
    }
}

pub fn body_digest(body: &[u8]) -> String {
    hex::encode(Sha256::digest(body))
}

/// The disposition of an already-recorded provider event.
pub async fn recorded<C: ConnectionTrait>(
    db: &C,
    provider_id: &str,
    event_ref: &str,
) -> Result<Option<String>, DbErr> {
    #[derive(sea_orm::FromQueryResult)]
    struct Row {
        disposition: String,
    }
    Ok(db::one::<Row, _>(
        db,
        "SELECT disposition FROM sms_delivery_events WHERE provider_id = $1 AND provider_event_ref = $2",
        vec![provider_id.into(), event_ref.into()],
    )
    .await?
    .map(|r| r.disposition))
}

/// Records a provider event; `false` when its reference was already recorded.
#[allow(clippy::too_many_arguments)]
pub async fn record_event<C: ConnectionTrait>(
    db: &C,
    provider_id: &str,
    event_ref: &str,
    kind: &str,
    transport_id: Option<Uuid>,
    normalized_status: Option<&str>,
    disposition: &str,
    reason: Option<&str>,
    occurred_at: Option<DateTime<Utc>>,
    digest: &str,
    now: DateTime<Utc>,
) -> Result<Option<Uuid>, DbErr> {
    let id = Uuid::new_v4();
    let inserted = db::exec(
        db,
        "INSERT INTO sms_delivery_events (id, provider_id, provider_event_ref, kind, transport_id, \
           normalized_status, disposition, reason, occurred_at, received_at, body_digest) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11) \
         ON CONFLICT (provider_id, provider_event_ref) DO NOTHING",
        vec![
            id.into(),
            provider_id.into(),
            event_ref.into(),
            kind.into(),
            transport_id.into(),
            normalized_status.map(str::to_string).into(),
            disposition.into(),
            reason.map(str::to_string).into(),
            occurred_at.into(),
            now.into(),
            digest.into(),
        ],
    )
    .await?;
    Ok((inserted == 1).then_some(id))
}

/// Why the delivery transaction is rolled back.
enum Abort {
    /// A concurrent duplicate recorded the event first: the original is answered.
    Duplicate,
    /// Numbering's routing changed since the resolution: quarantined instead.
    RoutingChanged,
    /// Numbering could not confirm the routing: nothing stored, the provider retries.
    NumberingUnavailable,
}

enum Stored {
    Applied,
    Quarantined,
    Refused,
}

/// Handles one authenticated inbound message.
pub async fn receive(
    state: &AppState,
    provider_id: &str,
    message: InboundMessage,
    digest: &str,
    ctx: &RequestContext,
) -> SmsResult<CallbackAck> {
    if let Some(d) = recorded(&state.db, provider_id, &message.event_ref).await? {
        crate::metrics::inc(
            "sms_callbacks_total",
            &[("kind", "inbound"), ("outcome", "duplicate")],
        );
        return Ok(CallbackAck {
            status: "DUPLICATE",
            disposition: d,
        });
    }
    let now = state.now();
    // Invalid numbers or content: recorded REFUSED (the provider stops retrying).
    let (event_ref, provider_at) = (message.event_ref.as_str(), message.received_at);
    let refused = |reason: &'static str| async move {
        let txn = state.db.begin().await?;
        let stored = record_event(
            &txn,
            provider_id,
            event_ref,
            "INBOUND_MESSAGE",
            None,
            None,
            "REFUSED",
            Some(reason),
            Some(provider_at),
            digest,
            now,
        )
        .await?;
        txn.commit().await?;
        Ok::<_, SmsError>(stored.is_some())
    };
    let (Some(to), Some(from)) = (e164::normalize(&message.to), e164::normalize(&message.from))
    else {
        let fresh = refused("INVALID_NUMBER").await?;
        return finish(
            state,
            provider_id,
            &message.event_ref,
            fresh,
            Stored::Refused,
        )
        .await;
    };
    let seg = match segments::analyze(&message.text) {
        Ok(s) if s.characters <= 1_600 => s,
        Ok(_) | Err(segments::SegmentError::TooLong) => {
            let fresh = refused("CONTENT_TOO_LONG").await?;
            return finish(
                state,
                provider_id,
                &message.event_ref,
                fresh,
                Stored::Refused,
            )
            .await;
        }
        Err(_) => {
            let fresh = refused("CONTENT_INVALID").await?;
            return finish(
                state,
                provider_id,
                &message.event_ref,
                fresh,
                Stored::Refused,
            )
            .await;
        }
    };
    let destination = match state
        .numbering
        .resolve_destination(&to, &ctx.correlation_id)
        .await
    {
        Ok(d) => d,
        Err(NumberingError::Unavailable) => {
            crate::metrics::inc(
                "sms_callbacks_total",
                &[("kind", "inbound"), ("outcome", "numbering_unavailable")],
            );
            return Err(SmsError::NumberingUnavailable);
        }
        // A refusal of the resolution itself (contract / permission): fail closed too —
        // never guess an owner.
        Err(NumberingError::Refused(_)) => return Err(SmsError::NumberingUnavailable),
    };
    let p = &state.protection;
    let txn = state.db.begin().await?;
    let outcome: Result<Stored, Abort> = match destination {
        SmsDestination::Routed {
            number_resource_id,
            assignment_id,
            routing_revision,
            scope,
            target_kind,
            target_ref,
            grant_id,
            grant,
        } => {
            let resolved_scope = scope;
            let scope = EventScope {
                tenant_id: scope.tenant_id,
                application_id: scope.application_id,
                environment_id: scope.environment_id,
            };
            let id = Uuid::new_v4();
            let fp = p.number_fingerprint(&from);
            let masked = mask(&from);
            let thread = threads::touch(&txn, scope, number_resource_id, &fp, &masked, now).await?;
            let row = store::insert(
                &txn,
                store::NewTransport {
                    id,
                    scope,
                    direction: Direction::Inbound,
                    status: Status::Received,
                    number_resource_id,
                    assignment_id: Some(assignment_id),
                    routing_revision: Some(routing_revision),
                    principal_id: (target_kind == "PRINCIPAL").then_some(target_ref),
                    target: Some((&target_kind, target_ref)),
                    remote_sealed: p.seal(Purpose::Remote, id, &from),
                    remote_fingerprint: &fp,
                    remote_masked: &masked,
                    content_sealed: p.seal(Purpose::Content, id, &message.text),
                    content_fingerprint: p.content_fingerprint(&message.text),
                    encoding: seg.encoding,
                    characters: seg.characters,
                    segments: seg.segments,
                    thread_id: thread.id,
                    service_account_id: None,
                    credential_id: None,
                    idempotency: None,
                    inbound_grant_id: Some(grant_id),
                    provider_id: Some(provider_id),
                    max_attempts: 1,
                    expires_at: None,
                    received_at: Some(now),
                    correlation_id: ctx.correlation_id.as_str(),
                    environment_kind: None,
                },
                now,
            )
            .await?;
            match record_event(
                &txn,
                provider_id,
                &message.event_ref,
                "INBOUND_MESSAGE",
                Some(id),
                Some("RECEIVED"),
                "APPLIED",
                None,
                Some(message.received_at),
                digest,
                now,
            )
            .await?
            {
                None => Err(Abort::Duplicate),
                Some(_) => {
                    db::exec(
                        &txn,
                        "INSERT INTO sms_usage_records (id, transport_id, tenant_id, application_id, environment_id, \
                           direction, segments, estimated_cost_minor, currency, status, created_at, updated_at) \
                         VALUES ($1, $2, $3, $4, $5, 'INBOUND', $6, 0, $7, 'ESTIMATED', $8, $8)",
                        vec![
                            Uuid::new_v4().into(),
                            id.into(),
                            scope.tenant_id.into(),
                            scope.application_id.into(),
                            scope.environment_id.into(),
                            (seg.segments as i16).into(),
                            state.controls.currency.clone().into(),
                            now.into(),
                        ],
                    )
                    .await?;
                    // The recipient's opt-out / opt-in keywords (for this number).
                    match controls::keyword(&message.text) {
                        Some(OptKeyword::Stop) => {
                            controls::block(
                                &txn,
                                scope,
                                Some(number_resource_id),
                                &fp,
                                "STOP_KEYWORD",
                                Some(id),
                                now,
                            )
                            .await?;
                        }
                        Some(OptKeyword::Start) => {
                            controls::lift_stop(
                                &txn,
                                scope.environment_id,
                                number_resource_id,
                                &fp,
                                now,
                            )
                            .await?;
                        }
                        None => {}
                    }
                    events::record(&txn, &row, MessageEvent::Received, ctx, now).await?;
                    // Part V H: a bridged party's message is queued for Messaging in the
                    // same transaction (content stays sealed here until delivery).
                    super::bridge::enqueue_inbound(
                        &txn,
                        scope.environment_id,
                        number_resource_id,
                        &fp,
                        id,
                        now,
                    )
                    .await?;
                    // The final delivery boundary: the routing decision must still be
                    // Numbering's CURRENT one (a reassignment, routing change, suspension or
                    // release since the resolution refuses it — the content then goes to
                    // quarantine, never to the previous or the next owner).
                    match state
                        .numbering
                        .verify_inbound_sms(&grant, &ctx.correlation_id)
                        .await
                    {
                        Ok(v)
                            if v.grant_id == grant_id
                                && v.number_resource_id == number_resource_id
                                && v.assignment_id == assignment_id
                                && v.routing_revision == routing_revision
                                && v.scope == resolved_scope
                                && v.target == Some((target_kind.clone(), target_ref)) =>
                        {
                            Ok(Stored::Applied)
                        }
                        Ok(_) | Err(NumberingError::Refused(_)) => Err(Abort::RoutingChanged),
                        Err(NumberingError::Unavailable) => Err(Abort::NumberingUnavailable),
                    }
                }
            }
        }
        SmsDestination::NotRouted { reason } | SmsDestination::Unavailable { reason } => {
            match quarantine(
                state,
                &txn,
                provider_id,
                &message,
                &to,
                &reason,
                digest,
                now,
            )
            .await?
            {
                true => Ok(Stored::Quarantined),
                false => Err(Abort::Duplicate),
            }
        }
    };
    match outcome {
        Ok(stored) => {
            txn.commit().await?;
            finish(state, provider_id, &message.event_ref, true, stored).await
        }
        Err(Abort::Duplicate) => {
            txn.rollback().await?;
            finish(
                state,
                provider_id,
                &message.event_ref,
                false,
                Stored::Refused,
            )
            .await
        }
        Err(Abort::NumberingUnavailable) => {
            txn.rollback().await?;
            Err(SmsError::NumberingUnavailable)
        }
        Err(Abort::RoutingChanged) => {
            txn.rollback().await?;
            crate::metrics::inc("sms_inbound_routing_changed_total", &[]);
            let txn = state.db.begin().await?;
            let fresh = quarantine(
                state,
                &txn,
                provider_id,
                &message,
                &to,
                "ROUTING_CHANGED",
                digest,
                now,
            )
            .await?;
            txn.commit().await?;
            finish(
                state,
                provider_id,
                &message.event_ref,
                fresh,
                Stored::Quarantined,
            )
            .await
        }
    }
}

/// Records the message as quarantined: sealed, attributed to no customer, purged after
/// its retention. `false` when the provider event was already recorded.
#[allow(clippy::too_many_arguments)]
async fn quarantine<C: ConnectionTrait>(
    state: &AppState,
    txn: &C,
    provider_id: &str,
    message: &InboundMessage,
    to: &str,
    reason: &str,
    digest: &str,
    now: DateTime<Utc>,
) -> Result<bool, DbErr> {
    let reason = if !reason.is_empty()
        && reason.len() <= 64
        && reason.as_bytes()[0].is_ascii_uppercase()
        && reason
            .bytes()
            .all(|b| b.is_ascii_uppercase() || b.is_ascii_digit() || b == b'_')
    {
        reason
    } else {
        "UNAVAILABLE"
    };
    let Some(event_id) = record_event(
        txn,
        provider_id,
        &message.event_ref,
        "INBOUND_MESSAGE",
        None,
        None,
        "QUARANTINED",
        Some(reason),
        Some(message.received_at),
        digest,
        now,
    )
    .await?
    else {
        return Ok(false);
    };
    let p = &state.protection;
    let qid = Uuid::new_v4();
    db::exec(
        txn,
        "INSERT INTO sms_quarantined_inbound (id, provider_id, delivery_event_id, number_fingerprint, \
           reason, content_sealed, received_at, purge_after) VALUES ($1, $2, $3, $4, $5, $6, $7, $8)",
        vec![
            qid.into(),
            provider_id.into(),
            event_id.into(),
            p.number_fingerprint(to).into(),
            reason.into(),
            p.seal(Purpose::Quarantine, qid, &message.text).into(),
            now.into(),
            (now + state.transport.quarantine_retention).into(),
        ],
    )
    .await?;
    Ok(true)
}

async fn finish(
    state: &AppState,
    provider_id: &str,
    event_ref: &str,
    fresh: bool,
    stored: Stored,
) -> SmsResult<CallbackAck> {
    if !fresh {
        let d = recorded(&state.db, provider_id, event_ref)
            .await?
            .unwrap_or_else(|| "REFUSED".into());
        crate::metrics::inc(
            "sms_callbacks_total",
            &[("kind", "inbound"), ("outcome", "duplicate")],
        );
        return Ok(CallbackAck {
            status: "DUPLICATE",
            disposition: d,
        });
    }
    let (label, disposition) = match stored {
        Stored::Applied => ("applied", "APPLIED"),
        Stored::Quarantined => ("quarantined", "QUARANTINED"),
        Stored::Refused => ("refused", "REFUSED"),
    };
    crate::metrics::inc(
        "sms_callbacks_total",
        &[("kind", "inbound"), ("outcome", label)],
    );
    tracing::info!(outcome = label, "inbound sms callback processed");
    Ok(CallbackAck::accepted(disposition))
}
