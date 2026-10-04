//! RecurrenceService — RFC 5545 recurrence interpreted in the meeting-space timezone,
//! with bounded, idempotent, multi-worker-safe occurrence materialization (spec §C15–§C17).
//!
//! Exceptions are authoritative rows in `meeting_recurrence_exceptions`, keyed by the
//! occurrence's ORIGINAL wall-clock start in the series timezone. They can target any
//! future occurrence — materialized or not. The materializer applies them when it
//! generates the ConferenceSession (row marked `is_exception = true`); if the row
//! already exists it is updated in the same transaction. The materializer never
//! overwrites existing rows (`ON CONFLICT (meeting_space_id, occurrence_key) DO NOTHING`).

use chrono::{DateTime, Duration, NaiveDateTime, Utc};
use chrono_tz::Tz;
use rrule::RRuleSet;
use sea_orm::{
    sea_query::{Expr, LockBehavior, LockType, OnConflict},
    ActiveModelTrait, ColumnTrait, ConnectionTrait, DbBackend, EntityTrait, QueryFilter,
    QueryOrder, QuerySelect, Set, Statement, TransactionTrait,
};
use serde_json::json;
use uuid::Uuid;

use crate::{
    config::MeetingV2Config,
    domain::{
        ConferenceKind, ConferenceStatus, RecordOrigin, RecurrenceExceptionKind, RecurrenceStatus,
        SpaceStatus, TerminationReason,
    },
    dto::v2::{RecurrenceExceptionAction, RecurrenceExceptionRequest, SetRecurrenceRequest},
    error::ApiResult,
    models::{
        conference_sessions, meeting_recurrence_exceptions, meeting_recurrence_rules,
        meeting_spaces,
    },
    services::{
        actor::ActorContext,
        meeting_core::{
            find_space, lock_space, permission_denied, space_role, state_conflict, validation, Core,
        },
        meeting_events::{action, audit, emit, AuditActor, AuditRecord, OutboxRecord},
        meeting_permissions::{space_allows, PermissionContext, SpacePermission},
    },
};

const MAX_OCCURRENCES_PER_QUERY: u16 = 500;

/// Validate an RRULE body and build the rule set anchored at `dtstart_local` in `timezone`.
pub fn build_rule_set(
    rrule_body: &str,
    timezone: &str,
    dtstart_local: NaiveDateTime,
) -> Result<RRuleSet, String> {
    let body = rrule_body.trim();
    let body = body.strip_prefix("RRULE:").unwrap_or(body);
    if body.is_empty() || body.len() > 512 {
        return Err("rrule must be a non-empty RRULE body".into());
    }
    if !body.chars().all(|c| {
        c.is_ascii_uppercase()
            || c.is_ascii_digit()
            || matches!(c, '=' | ';' | ',' | '-' | '+' | 'T' | 'Z')
    }) {
        return Err("rrule contains unsupported characters (provide only the RRULE body)".into());
    }
    let freq = body
        .split(';')
        .find_map(|p| p.strip_prefix("FREQ="))
        .ok_or("rrule requires FREQ")?;
    if !matches!(freq, "DAILY" | "WEEKLY" | "MONTHLY" | "YEARLY") {
        return Err("FREQ must be DAILY, WEEKLY, MONTHLY or YEARLY".into());
    }
    if body
        .split(';')
        .any(|p| p.starts_with("BYHOUR") || p.starts_with("BYMINUTE") || p.starts_with("BYSECOND"))
    {
        return Err(
            "BYHOUR/BYMINUTE/BYSECOND are not supported; the start time defines the time of day"
                .into(),
        );
    }
    let tz: Tz = timezone
        .parse()
        .map_err(|_| "invalid timezone".to_string())?;
    let source = format!(
        "DTSTART;TZID={}:{}\nRRULE:{}",
        tz.name(),
        dtstart_local.format("%Y%m%dT%H%M%S"),
        body
    );
    source
        .parse::<RRuleSet>()
        .map_err(|e| format!("invalid rrule: {e}"))
}

/// Occurrence starts in `[from, to)` (UTC), at most `limit`.
pub fn occurrences_between(
    set: RRuleSet,
    tz: Tz,
    from: DateTime<Utc>,
    to: DateTime<Utc>,
    limit: u16,
) -> Vec<DateTime<Utc>> {
    let rtz = rrule::Tz::Tz(tz);
    let after = (from - Duration::seconds(1)).with_timezone(&rtz);
    let before = to.with_timezone(&rtz);
    set.after(after)
        .before(before)
        .all(limit.min(MAX_OCCURRENCES_PER_QUERY))
        .dates
        .into_iter()
        .map(|d| d.with_timezone(&Utc))
        .filter(|d| *d >= from && *d < to)
        .collect()
}

pub fn occurrence_key(rule_id: Uuid, start: DateTime<Utc>) -> String {
    format!(
        "rrule:{}:{}",
        rule_id.simple(),
        start.format("%Y-%m-%dT%H:%M:%SZ")
    )
}

/// Idempotently materialize the rule's occurrences within the horizon.
/// Returns the number of newly inserted occurrences (0 on rerun).
pub async fn materialize<C: ConnectionTrait>(
    conn: &C,
    space: &meeting_spaces::Model,
    rule: &meeting_recurrence_rules::Model,
    now: DateTime<Utc>,
    config: &MeetingV2Config,
) -> ApiResult<u64> {
    if space.status != SpaceStatus::Active || rule.status != RecurrenceStatus::Active {
        return Ok(0);
    }
    let tz: Tz = rule
        .timezone
        .parse()
        .map_err(|_| validation("invalid recurrence timezone"))?;
    let set =
        build_rule_set(&rule.rrule, &rule.timezone, rule.dtstart_local).map_err(validation)?;
    let horizon_end = now + Duration::days(config.occurrence_horizon_days);
    let starts = occurrences_between(
        set,
        tz,
        now,
        horizon_end,
        config.occurrence_max_per_run as u16,
    );
    let duration = Duration::minutes(rule.duration_minutes as i64);
    let exceptions: std::collections::HashMap<NaiveDateTime, meeting_recurrence_exceptions::Model> =
        meeting_recurrence_exceptions::Entity::find()
            .filter(meeting_recurrence_exceptions::Column::RecurrenceRuleId.eq(rule.id))
            .all(conn)
            .await?
            .into_iter()
            .map(|e| (e.original_start_local, e))
            .collect();
    let mut inserted = 0u64;
    let ts = Utc::now();
    for start in &starts {
        let id = Uuid::new_v4();
        let exception = exceptions.get(&start.with_timezone(&tz).naive_local());
        let (status, scheduled_start, scheduled_end, cancelled) = match exception {
            Some(e) if e.kind == RecurrenceExceptionKind::Cancelled => {
                (ConferenceStatus::Cancelled, *start, *start + duration, true)
            }
            Some(e) => {
                let new_start = e.new_start_at.unwrap_or(*start);
                (
                    ConferenceStatus::Created,
                    new_start,
                    e.new_end_at.unwrap_or(new_start + duration),
                    false,
                )
            }
            None => (ConferenceStatus::Created, *start, *start + duration, false),
        };
        let model = conference_sessions::ActiveModel {
            id: Set(id),
            meeting_space_id: Set(space.id),
            occurrence_key: Set(occurrence_key(rule.id, *start)),
            kind: Set(ConferenceKind::Recurring),
            origin: Set(RecordOrigin::V2),
            legacy_meeting_id: Set(None),
            recurrence_rule_id: Set(Some(rule.id)),
            is_exception: Set(exception.is_some()),
            original_start_at: Set(Some(*start)),
            status: Set(status),
            scheduled_start_at: Set(Some(scheduled_start)),
            scheduled_end_at: Set(Some(scheduled_end)),
            started_at: Set(None),
            started_by: Set(None),
            ending_requested_at: Set(None),
            ended_at: Set(None),
            ended_by: Set(None),
            cancelled_at: Set(cancelled.then_some(ts)),
            cancelled_by: Set(cancelled.then(|| "system:recurrence-exception".to_string())),
            termination_reason: Set(cancelled.then_some(TerminationReason::Cancelled)),
            failure_reason: Set(None),
            is_locked: Set(false),
            locked_at: Set(None),
            locked_by: Set(None),
            rtc_provider: Set("LIVEKIT".into()),
            rtc_room_name: Set(Some(format!("cf-{}", id.simple()))),
            rtc_room_id: Set(None),
            region: Set(None),
            recording_state: Set("OFF".into()),
            transcription_state: Set("OFF".into()),
            created_at: Set(ts),
            updated_at: Set(ts),
            version: Set(1),
            event_seq: Set(0),
            rtc_room_ready_at: Set(None),
            rtc_room_terminated_at: Set(None),
        };
        inserted += conference_sessions::Entity::insert(model)
            .on_conflict(
                OnConflict::columns([
                    conference_sessions::Column::MeetingSpaceId,
                    conference_sessions::Column::OccurrenceKey,
                ])
                .do_nothing()
                .to_owned(),
            )
            .exec_without_returning(conn)
            .await?;
    }
    meeting_recurrence_rules::Entity::update_many()
        .col_expr(
            meeting_recurrence_rules::Column::MaterializedUntil,
            Expr::value(horizon_end),
        )
        .col_expr(
            meeting_recurrence_rules::Column::UpdatedAt,
            Expr::value(Utc::now()),
        )
        .filter(meeting_recurrence_rules::Column::Id.eq(rule.id))
        .exec(conn)
        .await?;
    if inserted > 0 {
        emit(
            conn,
            OutboxRecord {
                aggregate_type: "meeting_space",
                aggregate_id: space.id,
                event_type: "conference.occurrences_materialized",
                meeting_space_id: Some(space.id),
                conference_id: None,
                payload: json!({ "meetingSpaceId": space.id, "recurrenceRuleId": rule.id, "inserted": inserted }),
            },
        )
        .await?;
    }
    Ok(inserted)
}

/// Remove future, unstarted, non-exception occurrences of a rule (no history attached);
/// anything that cannot be removed is cancelled instead. Exceptions are preserved.
async fn retire_future_occurrences<C: ConnectionTrait>(
    conn: &C,
    rule_id: Uuid,
    now: DateTime<Utc>,
    include_exceptions: bool,
) -> ApiResult<u64> {
    // With `include_exceptions` (series re-anchored, same rule) exception rows are
    // regenerated from `meeting_recurrence_exceptions`, so their old rows are retired too.
    let deleted = conn
        .execute(Statement::from_sql_and_values(
            DbBackend::Postgres,
            r#"DELETE FROM conference_sessions c
                WHERE c.recurrence_rule_id = $1
                  AND (c.status = 'CREATED' OR ($3 AND c.status = 'CANCELLED'))
                  AND (c.is_exception = false OR $3)
                  AND c.scheduled_start_at > $2
                  AND NOT EXISTS (SELECT 1 FROM conference_participants p WHERE p.conference_id = c.id)
                  AND NOT EXISTS (SELECT 1 FROM meeting_invitations i WHERE i.conference_id = c.id)
                  AND NOT EXISTS (SELECT 1 FROM conference_audit_events a WHERE a.conference_id = c.id)
                  AND NOT EXISTS (SELECT 1 FROM conference_bans b WHERE b.conference_id = c.id)"#,
            [rule_id.into(), now.into(), include_exceptions.into()],
        ))
        .await?
        .rows_affected();
    let mut cancel = conference_sessions::Entity::update_many()
        .col_expr(
            conference_sessions::Column::Status,
            Expr::value(ConferenceStatus::Cancelled.as_str()),
        )
        .col_expr(
            conference_sessions::Column::TerminationReason,
            Expr::value(TerminationReason::Cancelled.as_str()),
        )
        .col_expr(conference_sessions::Column::CancelledAt, Expr::value(now))
        .col_expr(
            conference_sessions::Column::CancelledBy,
            Expr::value("system:recurrence"),
        )
        .col_expr(conference_sessions::Column::UpdatedAt, Expr::value(now))
        .filter(conference_sessions::Column::RecurrenceRuleId.eq(rule_id))
        .filter(conference_sessions::Column::Status.eq(ConferenceStatus::Created))
        .filter(conference_sessions::Column::ScheduledStartAt.gt(now));
    if !include_exceptions {
        cancel = cancel.filter(conference_sessions::Column::IsException.eq(false));
    }
    let cancelled = cancel.exec(conn).await?.rows_affected;
    Ok(deleted + cancelled)
}

async fn active_rule<C: ConnectionTrait>(
    conn: &C,
    space_id: Uuid,
    for_update: bool,
) -> ApiResult<Option<meeting_recurrence_rules::Model>> {
    let mut q = meeting_recurrence_rules::Entity::find()
        .filter(meeting_recurrence_rules::Column::MeetingSpaceId.eq(space_id))
        .filter(meeting_recurrence_rules::Column::Status.eq(RecurrenceStatus::Active));
    if for_update {
        q = q.lock(LockType::Update);
    }
    Ok(q.one(conn).await?)
}

/// Re-anchor the active series when the space timezone changes (same wall-clock time).
pub async fn follow_space_timezone<C: ConnectionTrait>(
    conn: &C,
    space: &meeting_spaces::Model,
    config: &MeetingV2Config,
) -> ApiResult<()> {
    if let Some(rule) = active_rule(conn, space.id, true).await? {
        let now = Utc::now();
        // Exceptions are keyed by wall-clock time, so they stay aligned after re-anchoring.
        retire_future_occurrences(conn, rule.id, now, true).await?;
        let mut am: meeting_recurrence_rules::ActiveModel = rule.clone().into();
        am.timezone = Set(space.timezone.clone());
        am.materialized_until = Set(None);
        am.version = Set(rule.version + 1);
        am.updated_at = Set(now);
        let rule = am.update(conn).await?;
        materialize(conn, space, &rule, now, config).await?;
    }
    Ok(())
}

/// Archive: end the series and cancel unstarted future occurrences.
pub async fn stop_series_for_archive<C: ConnectionTrait>(
    conn: &C,
    space_id: Uuid,
    now: DateTime<Utc>,
) -> ApiResult<u64> {
    meeting_recurrence_rules::Entity::update_many()
        .col_expr(
            meeting_recurrence_rules::Column::Status,
            Expr::value(RecurrenceStatus::Ended.as_str()),
        )
        .col_expr(meeting_recurrence_rules::Column::EndedAt, Expr::value(now))
        .col_expr(
            meeting_recurrence_rules::Column::UpdatedAt,
            Expr::value(now),
        )
        .filter(meeting_recurrence_rules::Column::MeetingSpaceId.eq(space_id))
        .filter(meeting_recurrence_rules::Column::Status.eq(RecurrenceStatus::Active))
        .exec(conn)
        .await?;
    Ok(conference_sessions::Entity::update_many()
        .col_expr(
            conference_sessions::Column::Status,
            Expr::value(ConferenceStatus::Cancelled.as_str()),
        )
        .col_expr(
            conference_sessions::Column::TerminationReason,
            Expr::value(TerminationReason::Cancelled.as_str()),
        )
        .col_expr(conference_sessions::Column::CancelledAt, Expr::value(now))
        .col_expr(
            conference_sessions::Column::CancelledBy,
            Expr::value("system:archive"),
        )
        .col_expr(conference_sessions::Column::UpdatedAt, Expr::value(now))
        .filter(conference_sessions::Column::MeetingSpaceId.eq(space_id))
        .filter(conference_sessions::Column::Status.eq(ConferenceStatus::Created))
        .filter(conference_sessions::Column::Origin.eq(RecordOrigin::V2))
        .filter(
            sea_orm::Condition::any()
                .add(conference_sessions::Column::ScheduledStartAt.gt(now))
                .add(conference_sessions::Column::ScheduledStartAt.is_null()),
        )
        .exec(conn)
        .await?
        .rows_affected)
}

/// Original wall-clock start of an occurrence in the series timezone (exception key).
pub fn occurrence_local(
    rule: &meeting_recurrence_rules::Model,
    start: DateTime<Utc>,
) -> ApiResult<NaiveDateTime> {
    let tz: Tz = rule
        .timezone
        .parse()
        .map_err(|_| validation("invalid recurrence timezone"))?;
    Ok(start.with_timezone(&tz).naive_local())
}

/// Whether `start` is exactly an occurrence of `rule` (any distance in the future).
pub fn is_occurrence(
    rule: &meeting_recurrence_rules::Model,
    start: DateTime<Utc>,
) -> ApiResult<bool> {
    let tz: Tz = rule
        .timezone
        .parse()
        .map_err(|_| validation("invalid recurrence timezone"))?;
    let set =
        build_rule_set(&rule.rrule, &rule.timezone, rule.dtstart_local).map_err(validation)?;
    Ok(
        occurrences_between(set, tz, start, start + Duration::seconds(1), 1)
            .first()
            .map(|d| *d == start)
            .unwrap_or(false),
    )
}

/// Idempotently create/update the authoritative exception row. Returns
/// `(exception, changed)`; an identical request changes nothing.
pub async fn upsert_exception_row<C: ConnectionTrait>(
    conn: &C,
    rule: &meeting_recurrence_rules::Model,
    original_local: NaiveDateTime,
    kind: RecurrenceExceptionKind,
    new_start: Option<DateTime<Utc>>,
    new_end: Option<DateTime<Utc>>,
    actor_ref: String,
) -> ApiResult<(meeting_recurrence_exceptions::Model, bool)> {
    let existing = meeting_recurrence_exceptions::Entity::find()
        .filter(meeting_recurrence_exceptions::Column::RecurrenceRuleId.eq(rule.id))
        .filter(meeting_recurrence_exceptions::Column::OriginalStartLocal.eq(original_local))
        .lock(LockType::Update)
        .one(conn)
        .await?;
    let now = Utc::now();
    match existing {
        Some(e) if e.kind == kind && e.new_start_at == new_start && e.new_end_at == new_end => {
            Ok((e, false))
        }
        Some(e) => {
            let version = e.version + 1;
            let mut am: meeting_recurrence_exceptions::ActiveModel = e.into();
            am.kind = Set(kind);
            am.new_start_at = Set(new_start);
            am.new_end_at = Set(new_end);
            am.created_by = Set(Some(actor_ref));
            am.updated_at = Set(now);
            am.version = Set(version);
            Ok((am.update(conn).await?, true))
        }
        None => {
            let model = meeting_recurrence_exceptions::ActiveModel {
                id: Set(Uuid::new_v4()),
                recurrence_rule_id: Set(rule.id),
                meeting_space_id: Set(rule.meeting_space_id),
                original_start_local: Set(original_local),
                kind: Set(kind),
                new_start_at: Set(new_start),
                new_end_at: Set(new_end),
                created_by: Set(Some(actor_ref)),
                created_at: Set(now),
                updated_at: Set(now),
                version: Set(1),
            }
            .insert(conn)
            .await?;
            Ok((model, true))
        }
    }
}

/// Record the authoritative exception for an already-materialized recurring occurrence
/// (used by the conference cancel/reschedule endpoints).
pub async fn record_exception_for_conference<C: ConnectionTrait>(
    conn: &C,
    conference: &conference_sessions::Model,
    kind: RecurrenceExceptionKind,
    new_start: Option<DateTime<Utc>>,
    new_end: Option<DateTime<Utc>>,
    actor_ref: String,
) -> ApiResult<()> {
    let (Some(rule_id), Some(original)) =
        (conference.recurrence_rule_id, conference.original_start_at)
    else {
        return Ok(());
    };
    let Some(rule) = meeting_recurrence_rules::Entity::find_by_id(rule_id)
        .one(conn)
        .await?
    else {
        return Ok(());
    };
    let local = occurrence_local(&rule, original)?;
    upsert_exception_row(conn, &rule, local, kind, new_start, new_end, actor_ref).await?;
    Ok(())
}

pub struct RecurrenceService {
    core: Core,
}

impl RecurrenceService {
    pub fn new(core: Core) -> Self {
        Self { core }
    }

    async fn require_scheduler<C: ConnectionTrait>(
        &self,
        conn: &C,
        ctx: &ActorContext,
        space: &meeting_spaces::Model,
    ) -> ApiResult<()> {
        let actor = ctx.require_registered()?;
        let role = space_role(conn, space, actor).await?.map(|(r, _)| r);
        let pctx = PermissionContext::for_space(space);
        if role
            .map(|r| space_allows(r, SpacePermission::ScheduleConferences, &pctx))
            .unwrap_or(false)
        {
            Ok(())
        } else {
            Err(permission_denied(
                "You are not permitted to schedule conferences",
            ))
        }
    }

    pub async fn get(
        &self,
        ctx: &ActorContext,
        space_id: Uuid,
    ) -> ApiResult<Option<meeting_recurrence_rules::Model>> {
        let actor = ctx.require_registered()?;
        let space = find_space(&self.core.db, space_id).await?;
        if space_role(&self.core.db, &space, actor).await?.is_none() {
            return Err(permission_denied("You are not a member of this meeting"));
        }
        active_rule(&self.core.db, space_id, false).await
    }

    /// Create or replace the space's recurrence rule.
    pub async fn set(
        &self,
        ctx: &ActorContext,
        space_id: Uuid,
        req: SetRecurrenceRequest,
    ) -> ApiResult<meeting_recurrence_rules::Model> {
        let actor = ctx.require_registered()?;
        self.core.require_v2_write(actor)?;
        if !(1..=1440).contains(&req.duration_minutes) {
            return Err(validation("durationMinutes must be between 1 and 1440"));
        }
        let txn = self.core.db.begin().await?;
        let space = lock_space(&txn, space_id).await?;
        self.require_scheduler(&txn, ctx, &space).await?;
        if space.status != SpaceStatus::Active {
            return Err(state_conflict("Archived meeting spaces cannot recur"));
        }
        build_rule_set(&req.rrule, &space.timezone, req.start_local).map_err(validation)?;
        let now = Utc::now();
        let mut replaced = None;
        if let Some(old) = active_rule(&txn, space_id, true).await? {
            retire_future_occurrences(&txn, old.id, now, false).await?;
            let mut am: meeting_recurrence_rules::ActiveModel = old.clone().into();
            am.status = Set(RecurrenceStatus::Ended);
            am.ended_at = Set(Some(now));
            am.updated_at = Set(now);
            am.version = Set(old.version + 1);
            am.update(&txn).await?;
            replaced = Some(old.id);
        }
        let body = req.rrule.trim();
        let rule = meeting_recurrence_rules::ActiveModel {
            id: Set(Uuid::new_v4()),
            meeting_space_id: Set(space_id),
            rrule: Set(body.strip_prefix("RRULE:").unwrap_or(body).to_string()),
            timezone: Set(space.timezone.clone()),
            dtstart_local: Set(req.start_local),
            duration_minutes: Set(req.duration_minutes),
            status: Set(RecurrenceStatus::Active),
            materialized_until: Set(None),
            created_by: Set(Some(actor.user_id)),
            created_at: Set(now),
            updated_at: Set(now),
            ended_at: Set(None),
            version: Set(1),
        }
        .insert(&txn)
        .await?;
        let inserted = materialize(&txn, &space, &rule, now, &self.core.config).await?;
        audit(
            &txn,
            &AuditActor::from(ctx),
            AuditRecord {
                meeting_space_id: space_id,
                conference_id: None,
                action: action::RECURRENCE_SET,
                target_type: Some("RECURRENCE_RULE"),
                target_ref: Some(rule.id.to_string()),
                metadata: json!({ "rrule": rule.rrule, "timezone": rule.timezone, "replaced": replaced, "materialized": inserted }),
            },
        )
        .await?;
        let rule = meeting_recurrence_rules::Entity::find_by_id(rule.id)
            .one(&txn)
            .await?
            .unwrap_or(rule);
        txn.commit().await?;
        Ok(rule)
    }

    /// Cancel or reschedule ONE future occurrence, materialized or not. Idempotent.
    /// Returns the exception and the materialized conference it was applied to, if any.
    pub async fn set_exception(
        &self,
        ctx: &ActorContext,
        space_id: Uuid,
        req: RecurrenceExceptionRequest,
    ) -> ApiResult<(meeting_recurrence_exceptions::Model, Option<Uuid>)> {
        let actor = ctx.require_registered()?;
        self.core.require_v2_write(actor)?;
        let now = Utc::now();
        if req.original_start_at <= now {
            return Err(validation("originalStartAt must be a future occurrence"));
        }
        let txn = self.core.db.begin().await?;
        let space = lock_space(&txn, space_id).await?;
        self.require_scheduler(&txn, ctx, &space).await?;
        let rule = active_rule(&txn, space_id, true)
            .await?
            .ok_or_else(|| validation("This meeting has no active recurrence"))?;
        if !is_occurrence(&rule, req.original_start_at)? {
            return Err(validation(
                "originalStartAt is not an occurrence of this series",
            ));
        }
        let duration = Duration::minutes(rule.duration_minutes as i64);
        let (kind, new_start, new_end) = match req.action {
            RecurrenceExceptionAction::Cancel => {
                if req.scheduled_start_at.is_some() || req.scheduled_end_at.is_some() {
                    return Err(validation("CANCEL takes no schedule"));
                }
                (RecurrenceExceptionKind::Cancelled, None, None)
            }
            RecurrenceExceptionAction::Reschedule => {
                let start = req
                    .scheduled_start_at
                    .ok_or_else(|| validation("RESCHEDULE requires scheduledStartAt"))?;
                let end = req.scheduled_end_at.unwrap_or(start + duration);
                if start <= now || end <= start || end - start > Duration::hours(24) {
                    return Err(validation("Invalid rescheduled time window"));
                }
                (RecurrenceExceptionKind::Rescheduled, Some(start), Some(end))
            }
        };
        let audit_actor = AuditActor::from(ctx);
        let local = occurrence_local(&rule, req.original_start_at)?;
        let (exception, changed) = upsert_exception_row(
            &txn,
            &rule,
            local,
            kind,
            new_start,
            new_end,
            audit_actor.reference(),
        )
        .await?;

        // Already materialized? Apply to the row now (same transaction).
        let key = occurrence_key(rule.id, req.original_start_at);
        let materialized = conference_sessions::Entity::find()
            .filter(conference_sessions::Column::MeetingSpaceId.eq(space_id))
            .filter(conference_sessions::Column::OccurrenceKey.eq(key))
            .lock(LockType::Update)
            .one(&txn)
            .await?;
        let mut applied_to = None;
        if let Some(c) = materialized {
            let target_state_reached = match kind {
                RecurrenceExceptionKind::Cancelled => c.status == ConferenceStatus::Cancelled,
                RecurrenceExceptionKind::Rescheduled => {
                    c.status == ConferenceStatus::Created
                        && c.scheduled_start_at == new_start
                        && c.scheduled_end_at == new_end
                }
            };
            if !target_state_reached {
                if c.status != ConferenceStatus::Created {
                    return Err(state_conflict(
                        "This occurrence has already started or ended",
                    ));
                }
                let version = c.version + 1;
                let mut am: conference_sessions::ActiveModel = c.clone().into();
                am.is_exception = Set(true);
                match kind {
                    RecurrenceExceptionKind::Cancelled => {
                        am.status = Set(ConferenceStatus::Cancelled);
                        am.cancelled_at = Set(Some(now));
                        am.cancelled_by = Set(Some(audit_actor.reference()));
                        am.termination_reason = Set(Some(TerminationReason::Cancelled));
                    }
                    RecurrenceExceptionKind::Rescheduled => {
                        am.scheduled_start_at = Set(new_start);
                        am.scheduled_end_at = Set(new_end);
                    }
                }
                am.version = Set(version);
                am.updated_at = Set(now);
                let updated = am.update(&txn).await?;
                let event = match kind {
                    RecurrenceExceptionKind::Cancelled => {
                        crate::services::meeting_events::event::CONFERENCE_CANCELLED
                    }
                    RecurrenceExceptionKind::Rescheduled => {
                        crate::services::meeting_events::event::CONFERENCE_RESCHEDULED
                    }
                };
                emit(
                    &txn,
                    crate::services::conference::conference_event(&updated, event),
                )
                .await?;
            }
            applied_to = Some(c.id);
        }
        if changed {
            audit(
                &txn,
                &audit_actor,
                AuditRecord {
                    meeting_space_id: space_id,
                    conference_id: None,
                    action: action::RECURRENCE_EXCEPTION_SET,
                    target_type: Some("RECURRENCE_EXCEPTION"),
                    target_ref: Some(exception.id.to_string()),
                    metadata: json!({
                        "originalStartLocal": exception.original_start_local,
                        "kind": exception.kind,
                        "newStartAt": exception.new_start_at,
                        "materialized": applied_to.is_some(),
                    }),
                },
            )
            .await?;
        }
        txn.commit().await?;
        Ok((exception, applied_to))
    }

    pub async fn list_exceptions(
        &self,
        ctx: &ActorContext,
        space_id: Uuid,
    ) -> ApiResult<Vec<(meeting_recurrence_exceptions::Model, String)>> {
        let actor = ctx.require_registered()?;
        let space = find_space(&self.core.db, space_id).await?;
        if space_role(&self.core.db, &space, actor).await?.is_none() {
            return Err(permission_denied("You are not a member of this meeting"));
        }
        let Some(rule) = active_rule(&self.core.db, space_id, false).await? else {
            return Ok(vec![]);
        };
        let list = meeting_recurrence_exceptions::Entity::find()
            .filter(meeting_recurrence_exceptions::Column::RecurrenceRuleId.eq(rule.id))
            .order_by_asc(meeting_recurrence_exceptions::Column::OriginalStartLocal)
            .all(&self.core.db)
            .await?;
        Ok(list
            .into_iter()
            .map(|e| (e, rule.timezone.clone()))
            .collect())
    }

    /// End the series. Started/past occurrences and exceptions are preserved.
    pub async fn end(&self, ctx: &ActorContext, space_id: Uuid) -> ApiResult<()> {
        let txn = self.core.db.begin().await?;
        let space = lock_space(&txn, space_id).await?;
        self.require_scheduler(&txn, ctx, &space).await?;
        let Some(rule) = active_rule(&txn, space_id, true).await? else {
            txn.commit().await?;
            return Ok(());
        };
        let now = Utc::now();
        let retired = retire_future_occurrences(&txn, rule.id, now, false).await?;
        let mut am: meeting_recurrence_rules::ActiveModel = rule.clone().into();
        am.status = Set(RecurrenceStatus::Ended);
        am.ended_at = Set(Some(now));
        am.updated_at = Set(now);
        am.version = Set(rule.version + 1);
        am.update(&txn).await?;
        audit(
            &txn,
            &AuditActor::from(ctx),
            AuditRecord {
                meeting_space_id: space_id,
                conference_id: None,
                action: action::RECURRENCE_END,
                target_type: Some("RECURRENCE_RULE"),
                target_ref: Some(rule.id.to_string()),
                metadata: json!({ "retiredFutureOccurrences": retired }),
            },
        )
        .await?;
        txn.commit().await?;
        Ok(())
    }

    /// Scheduler entry point: materialize every due series. Rows are claimed with
    /// `FOR UPDATE SKIP LOCKED` so concurrent workers never process the same series,
    /// and inserts are conflict-free anyway.
    pub async fn materialize_due(&self, now: DateTime<Utc>, batch: u64) -> ApiResult<u64> {
        let refresh_before =
            now + Duration::days(self.core.config.occurrence_horizon_days) - Duration::hours(12);
        let txn = self.core.db.begin().await?;
        let rules = meeting_recurrence_rules::Entity::find()
            .filter(meeting_recurrence_rules::Column::Status.eq(RecurrenceStatus::Active))
            .filter(
                sea_orm::Condition::any()
                    .add(meeting_recurrence_rules::Column::MaterializedUntil.is_null())
                    .add(meeting_recurrence_rules::Column::MaterializedUntil.lt(refresh_before)),
            )
            .limit(batch)
            .lock_with_behavior(LockType::Update, LockBehavior::SkipLocked)
            .all(&txn)
            .await?;
        let mut total = 0;
        for rule in rules {
            let Some(space) = meeting_spaces::Entity::find_by_id(rule.meeting_space_id)
                .one(&txn)
                .await?
            else {
                continue;
            };
            total += materialize(&txn, &space, &rule, now, &self.core.config).await?;
        }
        txn.commit().await?;
        Ok(total)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{NaiveDate, TimeZone, Timelike};

    fn local(y: i32, m: u32, d: u32, h: u32) -> NaiveDateTime {
        NaiveDate::from_ymd_opt(y, m, d)
            .unwrap()
            .and_hms_opt(h, 0, 0)
            .unwrap()
    }

    #[test]
    fn rejects_unsafe_or_unsupported_rules() {
        let start = local(2026, 10, 5, 9);
        assert!(build_rule_set("FREQ=WEEKLY;BYDAY=MO", "UTC", start).is_ok());
        assert!(build_rule_set("RRULE:FREQ=DAILY;COUNT=3", "UTC", start).is_ok());
        assert!(build_rule_set("FREQ=SECONDLY", "UTC", start).is_err());
        assert!(build_rule_set("FREQ=DAILY\nDTSTART:20200101T000000Z", "UTC", start).is_err());
        assert!(build_rule_set("FREQ=DAILY;BYHOUR=3", "UTC", start).is_err());
        assert!(build_rule_set("COUNT=3", "UTC", start).is_err());
        assert!(build_rule_set("FREQ=DAILY", "Nowhere/City", start).is_err());
    }

    #[test]
    fn weekly_rule_keeps_local_time_across_dst() {
        // Europe/London leaves BST on 25 Oct 2026: 09:00 local = 08:00Z before, 09:00Z after.
        let set = build_rule_set(
            "FREQ=WEEKLY;BYDAY=MO;COUNT=4",
            "Europe/London",
            local(2026, 10, 12, 9),
        )
        .unwrap();
        let from = Utc.with_ymd_and_hms(2026, 10, 1, 0, 0, 0).unwrap();
        let to = Utc.with_ymd_and_hms(2026, 12, 1, 0, 0, 0).unwrap();
        let occ = occurrences_between(set, chrono_tz::Europe::London, from, to, 50);
        assert_eq!(occ.len(), 4);
        assert_eq!(occ[0].hour(), 8);
        assert_eq!(occ[1].hour(), 8);
        assert_eq!(occ[2].hour(), 9);
        assert_eq!(occ[3].hour(), 9);
    }

    #[test]
    fn materialization_window_is_bounded() {
        let set = build_rule_set("FREQ=DAILY", "Africa/Lagos", local(2026, 1, 1, 10)).unwrap();
        let from = Utc.with_ymd_and_hms(2026, 10, 3, 0, 0, 0).unwrap();
        let occ = occurrences_between(
            set,
            chrono_tz::Africa::Lagos,
            from,
            from + Duration::days(30),
            50,
        );
        assert_eq!(occ.len(), 30, "unbounded rule yields only the horizon");
        assert!(occ.iter().all(|d| d.hour() == 9), "10:00 WAT = 09:00Z");
        let set = build_rule_set("FREQ=DAILY", "UTC", local(2026, 1, 1, 10)).unwrap();
        assert_eq!(
            occurrences_between(set, chrono_tz::UTC, from, from + Duration::days(365), 50).len(),
            50
        );
    }

    #[test]
    fn occurrence_keys_are_deterministic() {
        let id = Uuid::new_v4();
        let t = Utc.with_ymd_and_hms(2026, 10, 5, 8, 0, 0).unwrap();
        assert_eq!(occurrence_key(id, t), occurrence_key(id, t));
        assert_ne!(occurrence_key(id, t), occurrence_key(Uuid::new_v4(), t));
    }
}
