//! Outbound SMS authorization (Part IV Workstream O, Steps 15 / 18).
//!
//! `POST /v1/sms/messages` (scope `sms.send`, `Idempotency-Key` required):
//!
//! 1. The tenant / application / environment come from Platform Access (never the body).
//! 2. The content is classified (GSM-7 / UCS-2) and segmented (≤ the segment limit).
//! 3. The recipient is `to` (E.164) or the remote party of one of the environment's
//!    threads of the SAME number; the principal (if any) must be ACTIVE (Platform Access).
//! 4. The same key replays the same message (a different request is refused); a
//!    concurrent duplicate loses on the unique key and replays the winner.
//! 5. Send-time controls run in the gateway itself, even though Numbering will issue a
//!    grant: premium / disallowed destinations, opt-outs, provider anomaly and cost-spike
//!    circuit breakers, environment and tenant spend ceilings, identical-content fan-out
//!    and repeats, then the rate windows (tenant, environment, sender number, principal,
//!    recipient). A refusal is recorded FAILED with its reason code (content sealed) and
//!    `sms.blocked`; a rate window is a 429 and not recorded.
//! 6. Numbering issues a single-use SMS_SENDER grant for the delegated scope (and
//!    principal) bound to THIS message's operation reference, and the gateway consumes it
//!    at once: Numbering verifies ownership, ACTIVE assignment, SMS_OUTBOUND capability and
//!    the current revision. A refusal is `SENDER_NOT_AUTHORIZED` (recorded); Numbering
//!    unreachable is 503 and nothing is recorded. The released sender is stored sealed.
//! 7. The message is AUTHORIZED with its estimated cost (`sms.requested`); the dispatcher
//!    submits it ([`super::dispatch`]).

use sea_orm::{ConnectionTrait, TransactionTrait};
use sha2::{Digest, Sha256};
use uuid::Uuid;

use super::{
    controls, e164,
    events::{self, EventScope, MessageEvent},
    protection::{mask, Purpose},
    segments::{self, SegmentError},
    store, threads, Direction, SmsError, SmsResult, Status, TransportRow, TRANSPORT_COLUMNS,
};
use crate::{
    db,
    domain::RequestContext,
    identity::{PrincipalStatus, TrustedContext},
    numbering::NumberingError,
    state::AppState,
};

/// A validated send request (the HTTP body, normalized).
#[derive(Debug, Clone)]
pub struct SendRequest {
    pub from_number: Uuid,
    pub to: Option<String>,
    pub thread_id: Option<Uuid>,
    pub text: String,
    pub principal_id: Option<Uuid>,
    pub validity_seconds: Option<i64>,
}

impl SendRequest {
    /// The digest an idempotency key is bound to (never stored with the content).
    pub fn digest(&self) -> String {
        let canonical = serde_json::json!({
            "from": self.from_number,
            "to": self.to.as_deref().and_then(e164::normalize),
            "threadId": self.thread_id,
            "text": self.text,
            "principalId": self.principal_id,
            "validitySeconds": self.validity_seconds,
        });
        hex::encode(Sha256::digest(canonical.to_string().as_bytes()))
    }
}

/// What a send produced.
#[derive(Debug)]
pub enum SendOutcome {
    /// A new message (AUTHORIZED).
    Accepted(TransportRow),
    /// The same key and request: the existing message.
    Replayed(TransportRow),
}

pub fn valid_key(key: &str) -> bool {
    (8..=128).contains(&key.len())
        && key
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b':' | b'-'))
}

/// The operation reference the sender grant is bound to: exactly this message.
pub fn operation_ref(transport_id: Uuid) -> String {
    format!("sms:{}", transport_id.simple())
}

async fn by_key<C: ConnectionTrait>(
    db: &C,
    ctx: &TrustedContext,
    key: &str,
) -> Result<Option<TransportRow>, sea_orm::DbErr> {
    db::one::<TransportRow, _>(
        db,
        &format!(
            "SELECT {TRANSPORT_COLUMNS} FROM sms_transports \
              WHERE environment_id = $1 AND service_account_id = $2 AND idempotency_key = $3"
        ),
        vec![
            ctx.environment_id.into(),
            ctx.service_account_id.into(),
            key.into(),
        ],
    )
    .await
}

/// The answer to a repeated key: the same message, or the same refusal.
fn replay(row: TransportRow, digest: &str) -> SmsResult<SendOutcome> {
    if row.request_digest.as_deref() != Some(digest) {
        return Err(SmsError::IdempotencyKeyReused);
    }
    if let Some(reason) = row.blocked_reason.as_deref() {
        return Err(SmsError::Blocked {
            reason: controls::reason_code(reason),
            message_id: Some(row.id),
        });
    }
    Ok(SendOutcome::Replayed(row))
}

struct Recipient {
    e164: zeroize::Zeroizing<String>,
}

async fn recipient(
    state: &AppState,
    ctx: &TrustedContext,
    req: &SendRequest,
) -> SmsResult<Recipient> {
    match (&req.to, req.thread_id) {
        (Some(to), None) => Ok(Recipient {
            e164: zeroize::Zeroizing::new(
                e164::normalize(to).ok_or(SmsError::Validation("to must be an E.164 number."))?,
            ),
        }),
        (None, Some(thread_id)) => {
            let thread = threads::load(&state.db, ctx.environment_id, thread_id)
                .await?
                .ok_or(SmsError::NotFound)?;
            if thread.number_resource_id != req.from_number {
                return Err(SmsError::Validation(
                    "The thread belongs to another number.",
                ));
            }
            #[derive(sea_orm::FromQueryResult)]
            struct Last {
                id: Uuid,
                remote_sealed: String,
            }
            let last = db::one::<Last, _>(
                &state.db,
                "SELECT id, remote_sealed FROM sms_transports WHERE thread_id = $1 AND environment_id = $2 \
                 ORDER BY created_at DESC, id DESC LIMIT 1",
                vec![thread.id.into(), ctx.environment_id.into()],
            )
            .await?
            .ok_or(SmsError::NotFound)?;
            let e164 = state
                .protection
                .open(Purpose::Remote, last.id, &last.remote_sealed)
                .map_err(|_| SmsError::Conflict("THREAD_UNAVAILABLE"))?;
            Ok(Recipient {
                e164: zeroize::Zeroizing::new(e164),
            })
        }
        _ => Err(SmsError::Validation(
            "Exactly one of to or threadId is required.",
        )),
    }
}

/// Authorizes one outbound message.
pub async fn send(
    state: &AppState,
    ctx: &TrustedContext,
    key: &str,
    req: SendRequest,
    rc: &RequestContext,
) -> SmsResult<SendOutcome> {
    if !valid_key(key) {
        return Err(SmsError::Validation(
            "Idempotency-Key must be 8-128 characters of [A-Za-z0-9._:-].",
        ));
    }
    let digest = req.digest();
    if let Some(existing) = by_key(&state.db, ctx, key).await? {
        return replay(existing, &digest);
    }
    let seg = match segments::analyze(&req.text) {
        Ok(s) if s.segments <= state.controls.max_segments => s,
        Ok(_) | Err(SegmentError::TooLong) => return Err(SmsError::ContentTooLong),
        Err(SegmentError::Empty) => return Err(SmsError::Validation("text must not be empty.")),
        Err(SegmentError::ControlCharacter) => {
            return Err(SmsError::Validation("text contains a control character."))
        }
    };
    let validity = match req.validity_seconds {
        None => state.transport.default_validity,
        Some(s) if (60..=86_400).contains(&s) => chrono::Duration::seconds(s),
        Some(_) => return Err(SmsError::Validation("validitySeconds must be 60-86400.")),
    };
    let to = recipient(state, ctx, &req).await?;
    if let Some(p) = req.principal_id {
        match state
            .principals
            .resolve(&ctx.owner_scope(), p)
            .await
            .map_err(|_| SmsError::AuthorityUnavailable)?
        {
            PrincipalStatus::Active => {}
            PrincipalStatus::Inactive | PrincipalStatus::NotFound => {
                return Err(SmsError::Validation(
                    "principalId is not an active principal of the environment.",
                ))
            }
        }
    }
    let provider = state
        .provider
        .clone()
        .ok_or(SmsError::ProviderUnavailable)?;
    let now = state.now();
    let p = &state.protection;
    let c = &state.controls;
    let scope = EventScope {
        tenant_id: ctx.tenant_id,
        application_id: ctx.application_id,
        environment_id: ctx.environment_id,
    };
    let fp = p.number_fingerprint(&to.e164);
    let content_fp = p.content_fingerprint(&req.text);
    let estimate = c.segment_cost_minor.saturating_mul(seg.segments as i64);

    // Send-time controls (recorded refusals), cheapest and most specific first.
    let blocked: Option<&'static str> = if controls::premium(&to.e164, &c.blocked_prefixes) {
        Some(controls::PREMIUM_DESTINATION_BLOCKED)
    } else if !e164::allowed(&to.e164, &c.allowed_prefixes) {
        Some(controls::DESTINATION_NOT_ALLOWED)
    } else if controls::opted_out(&state.db, ctx.environment_id, req.from_number, &fp).await? {
        Some(controls::RECIPIENT_OPTED_OUT)
    } else if controls::provider_anomaly(&state.db, provider.id(), c, now).await? {
        Some(controls::PROVIDER_ANOMALY)
    } else if controls::cost_spike(&state.db, ctx.environment_id, c, now).await? {
        Some(controls::COST_SPIKE_CIRCUIT_OPEN)
    } else if controls::spent_today_by(&state.db, "environment_id", ctx.environment_id, now).await?
        + estimate
        > c.daily_spend_limit_minor
    {
        Some(controls::SPEND_LIMIT_EXCEEDED)
    } else if controls::spent_today_by(&state.db, "tenant_id", ctx.tenant_id, now).await? + estimate
        > c.tenant_daily_spend_limit_minor
    {
        Some(controls::TENANT_SPEND_LIMIT_EXCEEDED)
    } else if controls::identical_recipients(&state.db, ctx.environment_id, &content_fp, &fp, now)
        .await?
        >= c.abuse_identical_recipients
    {
        Some(controls::ABUSE_SUSPECTED)
    } else if controls::repeated_to_recipient(&state.db, ctx.environment_id, &content_fp, &fp, now)
        .await?
        >= c.repeated_destination_limit
    {
        Some(controls::REPEATED_DESTINATION)
    } else {
        None
    };
    let id = Uuid::new_v4();
    if let Some(reason) = blocked {
        return record_blocked(state, ctx, key, &digest, &req, &to, &seg, id, reason, rc).await;
    }
    // Rate windows (429, not recorded): a read-only pre-check here (Numbering is not asked
    // for a grant over the limit); the authoritative count is taken in the same
    // transaction that records the message, after its idempotency key won.
    let windows = rate_windows(ctx, &req, &fp, c);
    for w in &windows {
        if controls::window_count(&state.db, w.kind, &w.subject, w.width, now).await? >= w.limit {
            return Err(rate_limited(w, now));
        }
    }

    // Numbering: the single-use sender grant of THIS message, consumed now.
    let delegated = ctx.owner_scope().delegated(req.principal_id);
    let released = match state
        .numbering
        .release_sender(
            &delegated,
            &rc.correlation_id,
            req.from_number,
            &operation_ref(id),
        )
        .await
    {
        Ok(r)
            if r.number_resource_id == req.from_number
                && r.scope.tenant_id == ctx.tenant_id
                && r.scope.application_id == ctx.application_id
                && r.scope.environment_id == ctx.environment_id =>
        {
            r
        }
        Ok(_) | Err(NumberingError::Refused(_)) => {
            return record_blocked(
                state,
                ctx,
                key,
                &digest,
                &req,
                &to,
                &seg,
                id,
                controls::SENDER_NOT_AUTHORIZED,
                rc,
            )
            .await
        }
        Err(NumberingError::Unavailable) => return Err(SmsError::NumberingUnavailable),
    };

    let txn = state.db.begin().await?;
    let masked = mask(&to.e164);
    let thread = threads::touch(&txn, scope, req.from_number, &fp, &masked, now).await?;
    let inserted = store::insert(
        &txn,
        store::NewTransport {
            id,
            scope,
            direction: Direction::Outbound,
            status: Status::Requested,
            number_resource_id: req.from_number,
            assignment_id: Some(released.assignment_id),
            routing_revision: Some(released.routing_revision),
            principal_id: req.principal_id,
            target: None,
            remote_sealed: p.seal(Purpose::Remote, id, &to.e164),
            remote_fingerprint: &fp,
            remote_masked: &masked,
            content_sealed: p.seal(Purpose::Content, id, &req.text),
            content_fingerprint: content_fp.clone(),
            encoding: seg.encoding,
            characters: seg.characters,
            segments: seg.segments,
            thread_id: thread.id,
            service_account_id: Some(ctx.service_account_id),
            credential_id: Some(ctx.credential_id),
            idempotency: Some((key, &digest)),
            inbound_grant_id: None,
            provider_id: Some(provider.id()),
            max_attempts: state.transport.max_submit_attempts,
            expires_at: Some(now + validity),
            received_at: None,
            correlation_id: rc.correlation_id.as_str(),
            environment_kind: Some(ctx.environment_kind),
        },
        now,
    )
    .await;
    let row = match inserted {
        Ok(row) => row,
        Err(e) if db::sqlstate(&e).as_deref() == Some("23505") => {
            // A concurrent request with the same key won; the consumed grant of this one
            // authorized nothing.
            txn.rollback().await?;
            let winner = by_key(&state.db, ctx, key)
                .await?
                .ok_or(SmsError::Conflict("IDEMPOTENCY_RACE"))?;
            return replay(winner, &digest);
        }
        Err(e) => return Err(e.into()),
    };
    for w in &windows {
        if !controls::hit(&txn, w.kind, &w.subject, w.width, w.limit, now).await? {
            txn.rollback().await?;
            return Err(rate_limited(w, now));
        }
    }
    let row = store::update_in(
        &txn,
        &row,
        Some(Status::Authorized),
        vec![
            (
                "sender_sealed",
                p.seal(Purpose::Sender, id, &released.e164).into(),
            ),
            ("sender_grant_id", released.grant_id.into()),
            ("estimated_cost_minor", estimate.into()),
            ("cost_currency", c.currency.clone().into()),
            ("next_attempt_at", now.into()),
        ],
        now,
    )
    .await?;
    usage(&txn, &row, seg.segments, estimate, &c.currency, now).await?;
    events::record(&txn, &row, MessageEvent::Requested, rc, now).await?;
    txn.commit().await?;
    crate::metrics::inc("sms_outbound_total", &[("outcome", "authorized")]);
    Ok(SendOutcome::Accepted(row))
}

/// One rate window a message counts against.
struct Window {
    kind: &'static str,
    subject: String,
    width: chrono::Duration,
    limit: i64,
    reason: &'static str,
}

fn rate_windows(
    ctx: &TrustedContext,
    req: &SendRequest,
    remote_fingerprint: &str,
    c: &crate::config::ControlSettings,
) -> Vec<Window> {
    let minute = chrono::Duration::minutes(1);
    let mut w = vec![
        Window {
            kind: "TENANT",
            subject: ctx.tenant_id.simple().to_string(),
            width: minute,
            limit: c.tenant_rate_per_minute,
            reason: controls::TENANT_RATE_LIMITED,
        },
        Window {
            kind: "ENVIRONMENT",
            subject: ctx.environment_id.simple().to_string(),
            width: minute,
            limit: c.rate_per_minute,
            reason: controls::RATE_LIMITED,
        },
        Window {
            kind: "NUMBER",
            subject: req.from_number.simple().to_string(),
            width: minute,
            limit: c.number_rate_per_minute,
            reason: controls::NUMBER_RATE_LIMITED,
        },
        Window {
            kind: "DESTINATION",
            subject: format!(
                "{}.{}",
                ctx.environment_id.simple(),
                &remote_fingerprint[..32]
            ),
            width: chrono::Duration::hours(1),
            limit: c.destination_rate_per_hour,
            reason: controls::DESTINATION_RATE_LIMITED,
        },
    ];
    if let Some(p) = req.principal_id {
        w.push(Window {
            kind: "PRINCIPAL",
            subject: p.simple().to_string(),
            width: minute,
            limit: c.principal_rate_per_minute,
            reason: controls::PRINCIPAL_RATE_LIMITED,
        });
    }
    w
}

fn rate_limited(w: &Window, now: chrono::DateTime<chrono::Utc>) -> SmsError {
    crate::metrics::inc("sms_rate_limited_total", &[("window", w.kind)]);
    let width = w.width.num_seconds().max(1);
    let retry = width - now.timestamp().rem_euclid(width);
    SmsError::RateLimited {
        reason: w.reason,
        retry_after_seconds: retry.max(1) as u64,
    }
}

async fn usage<C: ConnectionTrait>(
    db: &C,
    row: &TransportRow,
    segments: usize,
    estimate: i64,
    currency: &str,
    now: chrono::DateTime<chrono::Utc>,
) -> Result<(), sea_orm::DbErr> {
    db::exec(
        db,
        "INSERT INTO sms_usage_records (id, transport_id, tenant_id, application_id, environment_id, \
           direction, segments, estimated_cost_minor, currency, status, created_at, updated_at) \
         VALUES ($1, $2, $3, $4, $5, 'OUTBOUND', $6, $7, $8, 'ESTIMATED', $9, $9)",
        vec![
            Uuid::new_v4().into(),
            row.id.into(),
            row.tenant_id.into(),
            row.application_id.into(),
            row.environment_id.into(),
            (segments as i16).into(),
            estimate.into(),
            currency.into(),
            now.into(),
        ],
    )
    .await
    .map(|_| ())
}

/// Records a refused message: FAILED with its reason (content sealed), `sms.blocked`.
#[allow(clippy::too_many_arguments)]
async fn record_blocked(
    state: &AppState,
    ctx: &TrustedContext,
    key: &str,
    digest: &str,
    req: &SendRequest,
    to: &Recipient,
    seg: &segments::Segmentation,
    id: Uuid,
    reason: &'static str,
    rc: &RequestContext,
) -> SmsResult<SendOutcome> {
    let now = state.now();
    let p = &state.protection;
    let scope = EventScope {
        tenant_id: ctx.tenant_id,
        application_id: ctx.application_id,
        environment_id: ctx.environment_id,
    };
    let fp = p.number_fingerprint(&to.e164);
    let masked = mask(&to.e164);
    let txn = state.db.begin().await?;
    let thread = threads::touch(&txn, scope, req.from_number, &fp, &masked, now).await?;
    let inserted = store::insert(
        &txn,
        store::NewTransport {
            id,
            scope,
            direction: Direction::Outbound,
            status: Status::Requested,
            number_resource_id: req.from_number,
            assignment_id: None,
            routing_revision: None,
            principal_id: req.principal_id,
            target: None,
            remote_sealed: p.seal(Purpose::Remote, id, &to.e164),
            remote_fingerprint: &fp,
            remote_masked: &masked,
            content_sealed: p.seal(Purpose::Content, id, &req.text),
            content_fingerprint: p.content_fingerprint(&req.text),
            encoding: seg.encoding,
            characters: seg.characters,
            segments: seg.segments,
            thread_id: thread.id,
            service_account_id: Some(ctx.service_account_id),
            credential_id: Some(ctx.credential_id),
            idempotency: Some((key, digest)),
            inbound_grant_id: None,
            provider_id: None,
            max_attempts: state.transport.max_submit_attempts,
            expires_at: None,
            received_at: None,
            correlation_id: rc.correlation_id.as_str(),
            environment_kind: Some(ctx.environment_kind),
        },
        now,
    )
    .await;
    let row = match inserted {
        Ok(row) => row,
        Err(e) if db::sqlstate(&e).as_deref() == Some("23505") => {
            txn.rollback().await?;
            let winner = by_key(&state.db, ctx, key)
                .await?
                .ok_or(SmsError::Conflict("IDEMPOTENCY_RACE"))?;
            return replay(winner, digest);
        }
        Err(e) => return Err(e.into()),
    };
    let row = store::update_in(
        &txn,
        &row,
        Some(Status::Failed),
        vec![
            ("failure_code", "BLOCKED".into()),
            ("blocked_reason", reason.into()),
            ("failed_at", now.into()),
        ],
        now,
    )
    .await?;
    events::record(&txn, &row, MessageEvent::Blocked, rc, now).await?;
    txn.commit().await?;
    crate::metrics::inc("sms_blocked_total", &[("reason", reason)]);
    tracing::info!(message_id = %row.id, reason, "outbound sms blocked");
    Err(SmsError::Blocked {
        reason,
        message_id: Some(row.id),
    })
}
