//! Operations: feature switch-on requests and release history (design §10, §6.15, A8).
//!
//! * `operations.activation.request` (R4, TWO_PERSON): starts the release workflow
//!   (`cfmeet-coordinated-release.yml` on `main`) through GitHub's dispatch API. The dispatch
//!   is an external side effect, so it runs like an owner command, never like a local one:
//!   the operation (PENDING) and its activation request (REQUESTED) are recorded in one
//!   transaction, `PENDING` → `SENT` is recorded BEFORE the dispatch, and the dispatch is
//!   sent once. GitHub's 2xx is SUCCEEDED (DISPATCHED); a 3xx/4xx is FAILED (nothing ran); a
//!   connection never established is FAILED (`NOT_SENT`); anything else is UNKNOWN_OUTCOME,
//!   settled only from the workflow's runs list (a `workflow_dispatch` run on `main` of the
//!   requested commit created around the dispatch and not linked to another request), never
//!   by dispatching again. No run after 600 s: FAILED (`NOT_RECEIVED`). Without a GitHub
//!   token the capability answers CAPABILITY_DISABLED before anything is recorded.
//! * `operations.activations.list`: the recorded requests, newest first, with their run's
//!   latest state from GitHub when configured (otherwise the recorded state, PARTIAL).
//! * `operations.releases.list`: the workflow's runs in the window, newest first (ids, event,
//!   the operation when the bridge requested it, commit, status, conclusion, times), never
//!   logs or anything the workflow printed. Without GitHub: an empty list, UNAVAILABLE.

use std::collections::HashMap;

use chrono::{DateTime, Duration, SecondsFormat, Utc};
use serde_json::{json, Value};
use uuid::Uuid;

use super::commands;
use crate::chain::{Answer, Authorized, Bridge};
use crate::errors::ErrorCode;
use crate::releases::{
    confirm_ok, github_time, sha_ok, DispatchOutcome, ReleaseWorkflow, WorkflowRun,
    ANY_SHA_OPERATIONS, RELEASE_OPERATIONS, WORKFLOW_FILE, WORKFLOW_REF,
};
use crate::store::{
    ActivationRequest, ActivationUpdate, BeginOutcome, OpStatus, Operation, Settlement,
};

/// `admin_operations.owner` of a release workflow dispatch.
pub const DISPATCH_OWNER: &str = "github-actions";
/// A run counts as this request's when created this long before the dispatch was sent
/// (clock skew) ...
pub const RUN_EARLY_SECONDS: i64 = 60;
/// ... up to the settlement deadline after it.
pub const RUN_LATE_SECONDS: i64 = commands::NOT_RECEIVED_AFTER_SECONDS;
/// GitHub lists at most this many runs for a filtered query.
pub const MAX_LISTED_RUNS: u32 = 1000;

pub async fn dispatch(bridge: &Bridge, auth: &Authorized) -> Option<Result<Answer, ErrorCode>> {
    Some(match auth.capability.id {
        "operations.activation.request" => request(bridge, auth).await,
        "operations.activations.list" => list(bridge, auth).await,
        "operations.releases.list" => releases(bridge, auth).await,
        _ => return None,
    })
}

fn ts(t: DateTime<Utc>) -> String {
    t.to_rfc3339_opts(SecondsFormat::Secs, true)
}

fn activation_json(a: &ActivationRequest, run: Option<&WorkflowRun>) -> Value {
    json!({
        "request_id": a.request_id.to_string(),
        "operation": a.operation,
        "release_sha": a.release_sha,
        "confirm": a.confirm,
        "requested_by": a.requested_by,
        "approver": a.approver,
        "state": a.state,
        "created_at": ts(a.created_at),
        "dispatched_at": a.dispatched_at.map(ts),
        "run_id": a.run_id,
        "run_status": run.map(|r| r.status.clone()),
        "run_conclusion": run.and_then(|r| r.conclusion.clone()),
        "run_updated_at": run.map(|r| ts(r.updated_at)),
    })
}

/// Records an outcome on the operation and its activation request (one transaction).
async fn finish(
    bridge: &Bridge,
    op: &mut Operation,
    status: OpStatus,
    source_status: &str,
    source_version: Option<String>,
    error: Option<ErrorCode>,
    update: ActivationUpdate,
) {
    let at = (bridge.clock)();
    let s = Settlement {
        status,
        source_status: Some(source_status.to_string()),
        source_version: source_version.clone(),
        error_code: error.map(|e| e.code().to_string()),
        at,
    };
    match bridge
        .store
        .settle_activation(&op.principal_sub, op.idempotency_key, &s, &update)
        .await
    {
        Ok(true) => {
            op.status = status;
            op.source_status = s.source_status;
            op.source_version = source_version;
            op.error_code = s.error_code;
            if status.settled() {
                op.settled_at = Some(at);
            }
        }
        // Not recorded: the sweep settles it later from the runs list.
        Ok(false) | Err(_) => {
            tracing::warn!(status = status.as_str(), "activation outcome not recorded");
        }
    }
}

/// The answer for an activation operation as it stands. Once a dispatch may have been
/// sent, no "nothing was done" code is ever returned.
async fn answer(
    bridge: &Bridge,
    op: &Operation,
    known: Option<ActivationRequest>,
) -> Result<Answer, ErrorCode> {
    match op.status {
        OpStatus::Succeeded => {
            let rec = match known {
                Some(r) => r,
                None => bridge
                    .store
                    .activation_request(op.operation_id)
                    .await
                    .ok()
                    .flatten()
                    .ok_or(ErrorCode::UnknownOutcome)?,
            };
            Ok(Answer {
                result: json!({
                    "operation": commands::readback_json(op),
                    "resource": activation_json(&rec, None),
                }),
                freshness: "LIVE",
            })
        }
        OpStatus::Failed => Err(commands::stored_code(op)),
        OpStatus::Pending | OpStatus::Sent | OpStatus::UnknownOutcome => {
            Err(ErrorCode::UnknownOutcome)
        }
    }
}

async fn request(bridge: &Bridge, auth: &Authorized) -> Result<Answer, ErrorCode> {
    let bad = ErrorCode::CommandBodyInvalid;
    let (base, body) = commands::parse_body(&auth.body, &["operation", "release_sha", "confirm"])?;
    if base.expected_version != "0" {
        return Err(bad);
    }
    let operation = body
        .get("operation")
        .and_then(Value::as_str)
        .filter(|o| RELEASE_OPERATIONS.contains(o))
        .ok_or(bad)?
        .to_string();
    let release_sha = body
        .get("release_sha")
        .and_then(Value::as_str)
        .filter(|s| sha_ok(s))
        .ok_or(bad)?
        .to_string();
    let confirm = match body.get("confirm") {
        None | Some(Value::Null) => None,
        Some(Value::String(c)) if confirm_ok(c) => Some(c.clone()),
        _ => return Err(bad),
    };
    let key = auth
        .idempotency_key
        .ok_or(ErrorCode::IdempotencyKeyRequired)?;
    if base.operation_id != key {
        return Err(bad);
    }
    let intent = commands::intent_sha256(auth);
    // A replay returns the stored outcome (after a settlement attempt); never a resend.
    if let Some(mut op) = auth.existing.clone() {
        if op.intent_sha256 != intent {
            return Err(ErrorCode::IdempotencyConflict);
        }
        if !op.status.settled() {
            settle_dispatch(bridge, &mut op).await;
        }
        return answer(bridge, &op, None).await;
    }
    // Not configured (or no readable token): refused before anything is recorded.
    let Some(github) = bridge.releases.as_ref().filter(|g| g.usable()) else {
        return Err(ErrorCode::CapabilityDisabled);
    };
    let approval = auth
        .staff
        .claims
        .approval
        .as_ref()
        .ok_or(ErrorCode::ApprovalRequired)?;
    let sub = auth.staff.claims.sub.clone();
    let mut op = Operation {
        principal_sub: sub.clone(),
        idempotency_key: key,
        operation_id: key,
        capability: auth.capability.id.to_string(),
        owner: DISPATCH_OWNER.to_string(),
        purpose: auth.query.purpose.as_str().to_string(),
        scope: commands::scope_of(auth),
        intent_sha256: intent.clone(),
        target: format!("github:{WORKFLOW_FILE}:activation:{key}"),
        expected_version: "0".into(),
        approval_id: Some(approval.approval_id.clone()),
        correlation_id: auth.correlation_id,
        status: OpStatus::Pending,
        source_status: None,
        source_version: None,
        error_code: None,
        created_at: auth.now,
        sent_at: None,
        settled_at: None,
    };
    let mut rec = ActivationRequest {
        request_id: key,
        operation: operation.clone(),
        release_sha: release_sha.clone(),
        confirm: confirm.clone(),
        requested_by: format!("hq:{sub}"),
        approver: format!("hq:{}", approval.approver_ref),
        approval_id: approval.approval_id.clone(),
        state: "REQUESTED".into(),
        created_at: auth.now,
        dispatched_at: None,
        run_id: None,
    };
    match bridge
        .store
        .begin_activation(&op, &rec)
        .await
        .map_err(|_| ErrorCode::BridgeUnavailable)?
    {
        BeginOutcome::Started => {}
        BeginOutcome::Existing(existing) => {
            if existing.intent_sha256 != intent {
                return Err(ErrorCode::IdempotencyConflict);
            }
            return answer(bridge, &existing, None).await;
        }
        BeginOutcome::VersionTaken => return Err(ErrorCode::VersionConflict),
    }
    let sent_at = (bridge.clock)();
    match bridge.store.mark_sent(&sub, key, sent_at).await {
        Ok(true) => op.sent_at = Some(sent_at),
        // Never sent: the row stays PENDING and the sweep fails it after the grace period.
        Ok(false) => return Err(ErrorCode::UnknownOutcome),
        Err(_) => return Err(ErrorCode::BridgeUnavailable),
    }
    op.status = OpStatus::Sent;
    let outcome = github
        .dispatch(&operation, &release_sha, confirm.as_deref())
        .await;
    let failed = || ActivationUpdate {
        state: Some("FAILED"),
        dispatched_at: None,
        run_id: None,
    };
    let result = match outcome {
        DispatchOutcome::Dispatched => {
            let update = ActivationUpdate {
                state: Some("DISPATCHED"),
                dispatched_at: Some(sent_at),
                run_id: None,
            };
            finish(
                bridge,
                &mut op,
                OpStatus::Succeeded,
                "DISPATCHED",
                None,
                None,
                update,
            )
            .await;
            rec.state = "DISPATCHED".into();
            rec.dispatched_at = Some(sent_at);
            answer(bridge, &op, Some(rec)).await
        }
        DispatchOutcome::Refused { status } => {
            finish(
                bridge,
                &mut op,
                OpStatus::Failed,
                &format!("HTTP_{status}"),
                None,
                Some(ErrorCode::OwnerRefused),
                failed(),
            )
            .await;
            answer(bridge, &op, None).await
        }
        DispatchOutcome::NotSent => {
            finish(
                bridge,
                &mut op,
                OpStatus::Failed,
                "NOT_SENT",
                None,
                Some(ErrorCode::OwnerUnavailable),
                failed(),
            )
            .await;
            answer(bridge, &op, None).await
        }
        DispatchOutcome::Unknown => {
            let update = ActivationUpdate {
                state: None,
                dispatched_at: None,
                run_id: None,
            };
            finish(
                bridge,
                &mut op,
                OpStatus::UnknownOutcome,
                "NO_ANSWER",
                None,
                Some(ErrorCode::UnknownOutcome),
                update,
            )
            .await;
            Err(ErrorCode::UnknownOutcome)
        }
    };
    let (decision, status) = match &result {
        Ok(_) => ("SUCCEEDED".to_string(), 200),
        Err(e) => (e.code().to_string(), e.status()),
    };
    commands::audit_outcome(bridge, auth, &op, &decision, status).await;
    result
}

/// True when `run` may be the run `rec`'s dispatch (sent at `since`) started: a
/// `workflow_dispatch` run on `main`, created around the dispatch, of the requested commit
/// (the workflow's gate runs every other operation on exactly that commit).
fn may_be(run: &WorkflowRun, rec: &ActivationRequest, since: DateTime<Utc>) -> bool {
    run.event == "workflow_dispatch"
        && run.head_branch.as_deref().is_none_or(|b| b == WORKFLOW_REF)
        && run.created_at >= since - Duration::seconds(RUN_EARLY_SECONDS)
        && run.created_at <= since + Duration::seconds(RUN_LATE_SECONDS)
        && (run.head_sha == rec.release_sha || ANY_SHA_OPERATIONS.contains(&rec.operation.as_str()))
}

/// The earliest candidate run not linked to another request.
fn pick<'a>(
    runs: &'a [WorkflowRun],
    rec: &ActivationRequest,
    since: DateTime<Utc>,
    linked: &HashMap<i64, Uuid>,
) -> Option<&'a WorkflowRun> {
    runs.iter()
        .filter(|r| may_be(r, rec, since))
        .filter(|r| linked.get(&r.id).is_none_or(|id| *id == rec.request_id))
        .min_by_key(|r| (r.created_at, r.id))
}

async fn linked_runs(bridge: &Bridge, runs: &[WorkflowRun]) -> Result<HashMap<i64, Uuid>, ()> {
    let ids: Vec<i64> = runs.iter().map(|r| r.id).collect();
    Ok(bridge
        .store
        .activations_for_runs(&ids)
        .await
        .map_err(|_| ())?
        .into_iter()
        .filter_map(|a| a.run_id.map(|r| (r, a.request_id)))
        .collect())
}

/// The run `rec`'s dispatch started, from the workflow's runs list.
async fn find_run(
    bridge: &Bridge,
    github: &ReleaseWorkflow,
    rec: &ActivationRequest,
    since: DateTime<Utc>,
) -> Result<Option<WorkflowRun>, ()> {
    let from = github_time(since - Duration::seconds(RUN_EARLY_SECONDS));
    let page = github
        .runs(&format!(">={from}"), 100, 1)
        .await
        .map_err(|_| ())?;
    let linked = linked_runs(bridge, &page.runs).await?;
    Ok(pick(&page.runs, rec, since, &linked).cloned())
}

/// Settles an open dispatch without ever dispatching again: a `PENDING` row past its grace
/// was never sent (FAILED); a sent one is SUCCEEDED once its run is listed, or FAILED as
/// never received once GitHub has had long enough.
pub async fn settle_dispatch(bridge: &Bridge, op: &mut Operation) {
    let now = (bridge.clock)();
    let failed = ActivationUpdate {
        state: Some("FAILED"),
        dispatched_at: None,
        run_id: None,
    };
    match op.status {
        OpStatus::Pending => {
            if now - op.created_at >= Duration::seconds(commands::PENDING_GRACE_SECONDS) {
                finish(
                    bridge,
                    op,
                    OpStatus::Failed,
                    "NOT_SENT",
                    None,
                    Some(ErrorCode::BridgeUnavailable),
                    failed,
                )
                .await;
            }
        }
        OpStatus::Sent | OpStatus::UnknownOutcome => {
            let Some(github) = bridge.releases.as_ref() else {
                return;
            };
            let Ok(Some(rec)) = bridge.store.activation_request(op.operation_id).await else {
                return;
            };
            let since = op.sent_at.unwrap_or(op.created_at);
            match find_run(bridge, github, &rec, since).await {
                Ok(Some(run)) => {
                    let update = ActivationUpdate {
                        state: Some("DISPATCHED"),
                        dispatched_at: Some(since),
                        run_id: Some(run.id),
                    };
                    finish(
                        bridge,
                        op,
                        OpStatus::Succeeded,
                        "DISPATCHED",
                        Some(run.id.to_string()),
                        None,
                        update,
                    )
                    .await;
                }
                Ok(None) => {
                    if now - since >= Duration::seconds(commands::NOT_RECEIVED_AFTER_SECONDS) {
                        finish(
                            bridge,
                            op,
                            OpStatus::Failed,
                            "NOT_RECEIVED",
                            None,
                            Some(ErrorCode::OwnerUnavailable),
                            failed,
                        )
                        .await;
                    }
                }
                Err(()) => {}
            }
        }
        OpStatus::Succeeded | OpStatus::Failed => {}
    }
}

/// `<created_at micros>.<request id>`.
fn page_cursor(c: &str) -> Option<(DateTime<Utc>, Uuid)> {
    let (m, id) = c.split_once('.')?;
    let t = DateTime::from_timestamp_micros(m.parse().ok()?)?;
    let parsed = Uuid::parse_str(id).ok()?;
    (parsed.hyphenated().to_string() == id).then_some((t, parsed))
}

async fn list(bridge: &Bridge, auth: &Authorized) -> Result<Answer, ErrorCode> {
    let q = &auth.query;
    let before = match &q.cursor {
        None => None,
        Some(c) => Some(page_cursor(c).ok_or(ErrorCode::QueryInvalid)?),
    };
    let limit = q.limit.unwrap_or(50);
    let rows = bridge
        .store
        .activation_requests(before, limit + 1)
        .await
        .map_err(|_| ErrorCode::BridgeUnavailable)?;
    let more = rows.len() > limit as usize;
    let mut page: Vec<ActivationRequest> = rows.into_iter().take(limit as usize).collect();
    let needs_runs = page.iter().any(|a| a.state == "DISPATCHED");
    // One runs-list call covers the page (from its oldest request on).
    let runs: Option<Vec<WorkflowRun>> = match (bridge.releases.as_ref(), page.last()) {
        (Some(github), Some(oldest)) if needs_runs => {
            let from = github_time(oldest.created_at - Duration::seconds(RUN_EARLY_SECONDS));
            github
                .runs(&format!(">={from}"), 100, 1)
                .await
                .ok()
                .map(|p| p.runs)
        }
        _ => None,
    };
    if let Some(runs) = &runs {
        // Link dispatched requests to their runs once they are listed.
        let mut linked = linked_runs(bridge, runs)
            .await
            .map_err(|_| ErrorCode::BridgeUnavailable)?;
        for a in page
            .iter_mut()
            .filter(|a| a.state == "DISPATCHED" && a.run_id.is_none())
        {
            let since = a.dispatched_at.unwrap_or(a.created_at);
            if let Some(run) = pick(runs, a, since, &linked) {
                if bridge
                    .store
                    .link_activation_run(a.request_id, run.id)
                    .await
                    .unwrap_or(false)
                {
                    linked.insert(run.id, a.request_id);
                    a.run_id = Some(run.id);
                }
            }
        }
    }
    let items: Vec<Value> = page
        .iter()
        .map(|a| {
            let run = a
                .run_id
                .and_then(|id| runs.as_ref()?.iter().find(|r| r.id == id));
            activation_json(a, run)
        })
        .collect();
    let next = more
        .then(|| page.last())
        .flatten()
        .map(|a| format!("{}.{}", a.created_at.timestamp_micros(), a.request_id));
    let freshness = if needs_runs && runs.is_none() {
        "PARTIAL"
    } else {
        "LIVE"
    };
    Ok(Answer {
        result: json!({"items": items, "next_cursor": next}),
        freshness,
    })
}

async fn releases(bridge: &Bridge, auth: &Authorized) -> Result<Answer, ErrorCode> {
    let q = &auth.query;
    let (start, end) = (
        q.start.ok_or(ErrorCode::QueryInvalid)?,
        q.end.ok_or(ErrorCode::QueryInvalid)?,
    );
    let limit = q.limit.unwrap_or(50);
    let page: u32 = match &q.cursor {
        None => 1,
        Some(c) => c
            .parse::<u32>()
            .ok()
            .filter(|p| *p >= 1 && c.bytes().all(|b| b.is_ascii_digit()))
            .filter(|p| (p - 1).saturating_mul(limit) < MAX_LISTED_RUNS)
            .ok_or(ErrorCode::QueryInvalid)?,
    };
    // No release source configured: honest emptiness, never "no releases".
    let Some(github) = bridge.releases.as_ref() else {
        return Ok(Answer {
            result: json!({"items": [], "next_cursor": null}),
            freshness: "UNAVAILABLE",
        });
    };
    let created = format!("{}..{}", github_time(start), github_time(end));
    let listed = github
        .runs(&created, limit, page)
        .await
        .map_err(|_| ErrorCode::OwnerUnavailable)?;
    let full_page = listed.runs.len() as u32 >= limit;
    // GitHub's range is inclusive; the window is [start, end).
    let runs: Vec<&WorkflowRun> = listed
        .runs
        .iter()
        .filter(|r| r.created_at >= start && r.created_at < end)
        .collect();
    let ids: Vec<i64> = runs.iter().map(|r| r.id).collect();
    let requested: HashMap<i64, String> = bridge
        .store
        .activations_for_runs(&ids)
        .await
        .map_err(|_| ErrorCode::BridgeUnavailable)?
        .into_iter()
        .filter_map(|a| a.run_id.map(|r| (r, a.operation)))
        .collect();
    let items: Vec<Value> = runs
        .iter()
        .map(|r| {
            json!({
                "run_id": r.id,
                "event": r.event,
                "operation": requested.get(&r.id),
                "head_sha": r.head_sha,
                "status": r.status,
                "conclusion": r.conclusion,
                "created_at": ts(r.created_at),
                "updated_at": ts(r.updated_at),
            })
        })
        .collect();
    let seen = u64::from(page) * u64::from(limit);
    let next = (full_page && seen < listed.total_count && seen < u64::from(MAX_LISTED_RUNS))
        .then(|| (page + 1).to_string());
    Ok(Answer {
        result: json!({"items": items, "next_cursor": next}),
        freshness: "LIVE",
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(id: i64, sha: &str, created: DateTime<Utc>) -> WorkflowRun {
        WorkflowRun {
            id,
            event: "workflow_dispatch".into(),
            head_sha: sha.into(),
            head_branch: Some("main".into()),
            status: "QUEUED".into(),
            conclusion: None,
            created_at: created,
            updated_at: created,
        }
    }

    fn rec(operation: &str, sha: &str) -> ActivationRequest {
        ActivationRequest {
            request_id: Uuid::new_v4(),
            operation: operation.into(),
            release_sha: sha.into(),
            confirm: None,
            requested_by: "hq:a".into(),
            approver: "hq:b".into(),
            approval_id: "ap-1".into(),
            state: "REQUESTED".into(),
            created_at: Utc::now(),
            dispatched_at: None,
            run_id: None,
        }
    }

    #[test]
    fn a_run_is_matched_by_commit_time_and_claims() {
        let (a, b) = ("a".repeat(40), "b".repeat(40));
        let now = Utc::now();
        let runs = vec![
            run(1, &b, now + Duration::seconds(2)),
            run(2, &a, now + Duration::seconds(5)),
            run(3, &a, now + Duration::seconds(3)),
            run(4, &a, now - Duration::seconds(3600)),
        ];
        let r = rec("activate-oauth", &a);
        let none = HashMap::new();
        assert_eq!(pick(&runs, &r, now, &none).map(|r| r.id), Some(3));
        // Run 3 belongs to another request: the next candidate.
        let taken = HashMap::from([(3, Uuid::new_v4())]);
        assert_eq!(pick(&runs, &r, now, &taken).map(|r| r.id), Some(2));
        // inspect runs on the main head whatever release_sha says.
        assert_eq!(
            pick(&runs, &rec("inspect", &"c".repeat(40)), now, &none).map(|r| r.id),
            Some(1)
        );
        assert!(pick(&runs, &rec("release", &"c".repeat(40)), now, &none).is_none());
    }
}
