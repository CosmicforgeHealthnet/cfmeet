//! A5 operations, the `messaging-admin` command-line tool's operations as bridge commands
//! (design §6.3 "CLI→API"; the tool keeps working unchanged).
//!
//! | Command | Route | Risk | Status | Version |
//! |---|---|---|---|---|
//! | `messaging.switch.change` | `POST /operator/switches/{switchName}` | R3 | `ENGAGED` / `RELEASED` | the switch row's (0: never set) |
//! | `messaging.incident.record` | `POST /operator/incidents/{incidentId}/resolve` | R2 | `RESOLVED` | none |
//! | `messaging.failed-message.retry` | `POST /operator/failed-messages/{eventId}/retry` | R2 | `RETRIED` | none |
//! | `messaging.key-transparency.checkpoint` | `POST /operator/key-transparency/checkpoint` | R3 | `CHECKPOINTED` / `ROLLBACK_DETECTED` / `UNVERIFIABLE` | none |
//!
//! Integrity stays with the server's own operators: the bridge cannot release a switch the
//! key-transparency monitor engaged (`KT_CHECKPOINT_UNVERIFIABLE`) nor resolve a
//! `KT_ROLLBACK_DETECTED` incident (409 `CONFLICT` / `KT_INTEGRITY_CLI_ONLY`): both stop
//! log appends after an integrity failure and are lifted only after evidence is restored
//! and verified with `messaging-admin`. A checkpoint is the monitor's own pass run now: it
//! verifies the database log against the operator checkpoint file and advances the file
//! only when consistent.

use axum::{
    extract::{rejection::JsonRejection, Path, State},
    http::HeaderMap,
    Json,
};
use chrono::{DateTime, Utc};
use sea_orm::{FromQueryResult, TransactionTrait};
use serde::Deserialize;
use serde_json::{json, Value};
use uuid::Uuid;

use super::{begin, body, finish, reason, BridgeCommand, Outcome, Subject, ADMIN_BRIDGE};
use crate::{
    conversations::service::rfc3339,
    db,
    error::{Error, ErrorCode},
    http::{error::ApiError, middleware::InternalCaller},
    operations::{self, KillSwitch, KtVerdict},
    state::AppState,
};

pub const CHANGE_SWITCH: &str = "messaging.switch.change";
pub const RECORD_INCIDENT: &str = "messaging.incident.record";
pub const RETRY_FAILED_MESSAGE: &str = "messaging.failed-message.retry";
pub const KT_CHECKPOINT: &str = "messaging.key-transparency.checkpoint";

/// The reason the key-transparency monitor engages its switches with.
const KT_UNVERIFIABLE: &str = "KT_CHECKPOINT_UNVERIFIABLE";

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ChangeSwitch {
    /// `null` / absent: the GLOBAL switch; otherwise one application environment's.
    #[serde(default)]
    pub environment_id: Option<Uuid>,
    pub engaged: bool,
    /// The switch row's version; 0 when the switch was never set (absent from the list).
    pub expected_version: i64,
    pub reason_code: String,
}

#[derive(Debug, FromQueryResult)]
struct SwitchLock {
    engaged: bool,
    reason: String,
    version: i32,
}

/// `POST /operator/switches/{switchName}` — `messaging.switch.change` (engage stops a
/// capability; release lets it run again).
pub async fn change_switch(
    State(state): State<AppState>,
    caller: InternalCaller,
    headers: HeaderMap,
    Path(name): Path<String>,
    raw: Result<Json<ChangeSwitch>, JsonRejection>,
) -> Result<Json<Value>, ApiError> {
    let cmd = BridgeCommand::of(&caller, &headers, true)?;
    let req = body(raw)?;
    let switch = KillSwitch::parse(&name).ok_or_else(|| {
        Error::validation(format!(
            "switchName must be one of {}",
            KillSwitch::ALL.map(|k| k.as_str()).join(", ")
        ))
    })?;
    if req.expected_version < 0 {
        return Err(Error::validation(
            "expectedVersion must be the switch's version (0 when it was never set)",
        ));
    }
    reason(&req.reason_code)?;
    let scope = req
        .environment_id
        .map(|e| e.to_string())
        .unwrap_or_else(|| "GLOBAL".into());
    let subject = Subject::new("KILL_SWITCH", format!("{}:{scope}", switch.as_str()));
    let now = state.now();
    let txn = state.db.begin().await?;
    if let Some(done) = begin(&txn, &cmd, CHANGE_SWITCH, &subject).await? {
        return Ok(Json(done));
    }
    // One writer per switch and scope (also while no row exists yet).
    db::exec(
        &txn,
        "SELECT pg_advisory_xact_lock(hashtextextended($1, 68))",
        vec![format!("messaging-kill-switch:{}", subject.id).into()],
    )
    .await?;
    let current: Option<SwitchLock> = db::one(
        &txn,
        "SELECT engaged, reason, version FROM messaging_kill_switches WHERE switch_name = $1 AND scope_key = $2 FOR UPDATE",
        vec![switch.as_str().into(), scope.clone().into()],
    )
    .await?;
    let version = current.as_ref().map(|c| i64::from(c.version)).unwrap_or(0);
    if version != req.expected_version {
        return Err(Error::code(ErrorCode::VersionConflict));
    }
    let engaged = current.as_ref().is_some_and(|c| c.engaged);
    if engaged == req.engaged {
        return Err(Error::reason(
            ErrorCode::Conflict,
            "SWITCH_ALREADY_IN_STATE",
        ));
    }
    if !req.engaged
        && current
            .as_ref()
            .is_some_and(|c| c.reason == KT_UNVERIFIABLE)
    {
        return Err(Error::reason(ErrorCode::Conflict, "KT_INTEGRITY_CLI_ONLY"));
    }
    let changed = db::exec(
        &txn,
        "INSERT INTO messaging_kill_switches (switch_name, scope_key, engaged, reason, changed_by, changed_at) \
         VALUES ($1, $2, $3, $4, $5, $6) \
         ON CONFLICT (switch_name, scope_key) DO UPDATE SET engaged = EXCLUDED.engaged, reason = EXCLUDED.reason, \
           changed_by = EXCLUDED.changed_by, changed_at = EXCLUDED.changed_at, version = messaging_kill_switches.version + 1 \
         WHERE messaging_kill_switches.engaged IS DISTINCT FROM EXCLUDED.engaged",
        vec![
            switch.as_str().into(),
            scope.clone().into(),
            req.engaged.into(),
            req.reason_code.clone().into(),
            ADMIN_BRIDGE.into(),
            now.into(),
        ],
    )
    .await?;
    if changed != 1 {
        return Err(Error::code(ErrorCode::VersionConflict));
    }
    operations::audit(
        &txn,
        ADMIN_BRIDGE,
        if req.engaged {
            "KILL_SWITCH_ENGAGED"
        } else {
            "KILL_SWITCH_RELEASED"
        },
        &subject.id,
        json!({ "reason": req.reason_code, "operator_ref": cmd.operator }),
        now,
    )
    .await?;
    let status = if req.engaged { "ENGAGED" } else { "RELEASED" };
    let outcome = Outcome {
        state: status,
        version: Some(version + 1),
        resource: json!({
            "switchName": switch.as_str(),
            "scopeKey": scope,
            "engaged": req.engaged,
            "reason": req.reason_code,
            "changedAt": rfc3339(now),
        }),
    };
    let answer = finish(&txn, &cmd, CHANGE_SWITCH, &subject, outcome, now).await?;
    txn.commit().await?;
    Ok(Json(answer))
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ReasonOnly {
    pub reason_code: String,
}

#[derive(Debug, FromQueryResult)]
struct IncidentLock {
    category: String,
    status: String,
}

/// `POST /operator/incidents/{incidentId}/resolve` — `messaging.incident.record`: records
/// the resolution of an OPEN incident (the tool's `incidents resolve`; incidents are raised
/// by the service itself, never by an operator).
pub async fn resolve_incident(
    State(state): State<AppState>,
    caller: InternalCaller,
    headers: HeaderMap,
    Path(incident_id): Path<Uuid>,
    raw: Result<Json<ReasonOnly>, JsonRejection>,
) -> Result<Json<Value>, ApiError> {
    let cmd = BridgeCommand::of(&caller, &headers, false)?;
    let req = body(raw)?;
    reason(&req.reason_code)?;
    let subject = Subject::new("INCIDENT", incident_id);
    let now = state.now();
    let txn = state.db.begin().await?;
    if let Some(done) = begin(&txn, &cmd, RECORD_INCIDENT, &subject).await? {
        return Ok(Json(done));
    }
    let incident: IncidentLock = db::one(
        &txn,
        "SELECT category, status FROM messaging_incidents WHERE id = $1 FOR UPDATE",
        vec![incident_id.into()],
    )
    .await?
    .ok_or(Error::code(ErrorCode::NotFound))?;
    if incident.status != "OPEN" {
        return Err(Error::reason(ErrorCode::Conflict, "INCIDENT_NOT_OPEN"));
    }
    if incident.category == operations::IncidentCategory::KtRollbackDetected.as_str() {
        return Err(Error::reason(ErrorCode::Conflict, "KT_INTEGRITY_CLI_ONLY"));
    }
    db::exec(
        &txn,
        "UPDATE messaging_incidents SET status = 'RESOLVED', resolved_at = $2, resolved_by = $3 WHERE id = $1 AND status = 'OPEN'",
        vec![incident_id.into(), now.into(), ADMIN_BRIDGE.into()],
    )
    .await?;
    operations::audit(
        &txn,
        ADMIN_BRIDGE,
        "INCIDENT_RESOLVED",
        &incident_id.to_string(),
        json!({ "reason": req.reason_code, "operator_ref": cmd.operator }),
        now,
    )
    .await?;
    let outcome = Outcome {
        state: "RESOLVED",
        version: None,
        resource: json!({
            "incidentId": incident_id,
            "category": incident.category,
            "incidentStatus": "RESOLVED",
            "resolvedAt": rfc3339(now),
        }),
    };
    let answer = finish(&txn, &cmd, RECORD_INCIDENT, &subject, outcome, now).await?;
    txn.commit().await?;
    Ok(Json(answer))
}

#[derive(Debug, FromQueryResult)]
struct DeadLetterLock {
    event_type: String,
    aggregate_type: String,
    aggregate_id: Uuid,
    status: String,
}

/// `POST /operator/failed-messages/{eventId}/retry` — `messaging.failed-message.retry`:
/// re-queues one dead-lettered outbox event for its publisher with a fresh retry budget
/// (the tool's `dead-letters replay --kind outbox --id`; publication stays idempotent by
/// event id). The event payload is never read or returned.
pub async fn retry_failed_message(
    State(state): State<AppState>,
    caller: InternalCaller,
    headers: HeaderMap,
    Path(event_id): Path<Uuid>,
    raw: Result<Json<ReasonOnly>, JsonRejection>,
) -> Result<Json<Value>, ApiError> {
    let cmd = BridgeCommand::of(&caller, &headers, false)?;
    let req = body(raw)?;
    reason(&req.reason_code)?;
    let subject = Subject::new("OUTBOX_EVENT", event_id);
    let now = state.now();
    let txn = state.db.begin().await?;
    if let Some(done) = begin(&txn, &cmd, RETRY_FAILED_MESSAGE, &subject).await? {
        return Ok(Json(done));
    }
    let event: DeadLetterLock = db::one(
        &txn,
        "SELECT event_type, aggregate_type, aggregate_id, status FROM messaging_outbox_events WHERE id = $1 FOR UPDATE",
        vec![event_id.into()],
    )
    .await?
    .ok_or(Error::code(ErrorCode::NotFound))?;
    if event.status != "DEAD_LETTER" {
        return Err(Error::reason(ErrorCode::Conflict, "NOT_DEAD_LETTERED"));
    }
    db::exec(
        &txn,
        "UPDATE messaging_outbox_events SET status = 'PENDING', attempts = 0, next_attempt_at = $2, lease_token = NULL, \
           lease_expires_at = NULL WHERE id = $1 AND status = 'DEAD_LETTER'",
        vec![event_id.into(), now.into()],
    )
    .await?;
    operations::audit(
        &txn,
        ADMIN_BRIDGE,
        "DEAD_LETTERS_REPLAYED",
        "outbox",
        json!({ "count": 1, "id": event_id, "reason": req.reason_code, "operator_ref": cmd.operator }),
        now,
    )
    .await?;
    let outcome = Outcome {
        state: "RETRIED",
        version: None,
        resource: json!({
            "eventId": event_id,
            "eventType": event.event_type,
            "aggregateType": event.aggregate_type,
            "aggregateId": event.aggregate_id,
            "queueState": "PENDING",
            "retriedAt": rfc3339(now),
        }),
    };
    let answer = finish(&txn, &cmd, RETRY_FAILED_MESSAGE, &subject, outcome, now).await?;
    txn.commit().await?;
    crate::metrics::inc(
        "messaging_dead_letters_replayed_total",
        &[("kind", "outbox")],
    );
    state.wake_outbox();
    Ok(Json(answer))
}

/// The resulting state of a checkpoint pass.
fn checkpoint_state(verdict: &Result<KtVerdict, String>) -> &'static str {
    match verdict {
        Ok(KtVerdict::Consistent { .. }) => "CHECKPOINTED",
        Ok(_) => "ROLLBACK_DETECTED",
        Err(_) => "UNVERIFIABLE",
    }
}

/// `POST /operator/key-transparency/checkpoint` — `messaging.key-transparency.checkpoint`:
/// one key-transparency monitor pass now. Consistent: the operator checkpoint file
/// advances to the current tree head (`CHECKPOINTED`). Behind / forked: the CRITICAL
/// incident is raised and the file is untouched (`ROLLBACK_DETECTED`). Unreadable /
/// unwritable: the monitor's protective switches are engaged (`UNVERIFIABLE`). Exactly what
/// the monitor does on its next tick, so recording it after the pass is safe. 409
/// `KT_CHECKPOINT_NOT_CONFIGURED` (nothing done) without a configured checkpoint file.
pub async fn checkpoint(
    State(state): State<AppState>,
    caller: InternalCaller,
    headers: HeaderMap,
    raw: Result<Json<ReasonOnly>, JsonRejection>,
) -> Result<Json<Value>, ApiError> {
    let cmd = BridgeCommand::of(&caller, &headers, true)?;
    let req = body(raw)?;
    reason(&req.reason_code)?;
    let subject = Subject::new("KT_LOG", "transparency-log");
    let now: DateTime<Utc> = state.now();
    let txn = state.db.begin().await?;
    if let Some(done) = begin(&txn, &cmd, KT_CHECKPOINT, &subject).await? {
        return Ok(Json(done));
    }
    let Some(path) = state.kt_checkpoint.clone() else {
        return Err(Error::reason(
            ErrorCode::Conflict,
            "KT_CHECKPOINT_NOT_CONFIGURED",
        ));
    };
    let verdict = operations::kt_monitor_once(&state.db, now, &path).await;
    let head = operations::kt_checkpoint(&txn).await?;
    let outcome = Outcome {
        state: checkpoint_state(&verdict),
        version: None,
        resource: json!({
            "treeSize": head.tree_size,
            "rootHash": head.root_hash,
            "verdict": checkpoint_state(&verdict),
            "checkedAt": rfc3339(now),
        }),
    };
    operations::audit(
        &txn,
        ADMIN_BRIDGE,
        "KT_CHECKPOINT_REQUESTED",
        &subject.id,
        json!({ "reason": req.reason_code, "operator_ref": cmd.operator, "verdict": outcome.state, "treeSize": head.tree_size }),
        now,
    )
    .await?;
    let answer = finish(&txn, &cmd, KT_CHECKPOINT, &subject, outcome, now).await?;
    txn.commit().await?;
    Ok(Json(answer))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn checkpoint_states_name_the_verdict() {
        assert_eq!(
            checkpoint_state(&Ok(KtVerdict::Consistent { tree_size: 3 })),
            "CHECKPOINTED"
        );
        assert_eq!(
            checkpoint_state(&Ok(KtVerdict::Forked { checkpoint_size: 3 })),
            "ROLLBACK_DETECTED"
        );
        assert_eq!(
            checkpoint_state(&Ok(KtVerdict::Behind {
                database_size: 1,
                checkpoint_size: 3
            })),
            "ROLLBACK_DETECTED"
        );
        assert_eq!(checkpoint_state(&Err("x".into())), "UNVERIFIABLE");
    }
}
