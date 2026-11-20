//! The command core (design §9, phase A3.0): every HQ command runs through [`execute`].
//!
//! * The operations ledger (`admin_operations`) is keyed by the staff member and HQ's
//!   `Idempotency-Key`. A replay with the same intent returns the stored outcome; a
//!   different intent is `IDEMPOTENCY_CONFLICT`. Only one non-failed operation may hold a
//!   target's expected version (`VERSION_CONFLICT`).
//! * Send once: `PENDING` → `SENT` is recorded BEFORE the owner call; the owner is never
//!   called twice for one row. No clear answer is `UNKNOWN_OUTCOME` (502), settled only
//!   from the owner's own record of the `Idempotency-Key` ([`settle`]), never by resending.
//! * Every outcome is audited (the chain already audited the accepted request before this
//!   runs). An answer HQ may treat as "nothing was done" ([`ErrorCode::nothing_done`]) is
//!   returned only when the owner was never called.
//! * `bridge.operations.read` (`GET /hq/operations/{idempotency_key}`, [`readback`]) is
//!   HQ's generic settlement: by the
//!   `Idempotency-Key` it sent, within the calling staff member, same purpose and scope.

use std::collections::BTreeMap;

use chrono::{DateTime, Duration, Utc};
use serde_json::{json, Value};
use uuid::Uuid;

use crate::chain::{Answer, Authorized, Bridge};
use crate::contract::model::{Approval, Owner};
use crate::errors::ErrorCode;
use crate::owners::{ApprovalHeaders, CommandCall, CommandOutcome};
use crate::store::{
    AuditDraft, BeginOutcome, LocalChange, LocalOutcome, OpStatus, Operation, Settlement,
};

/// A `PENDING` row older than this was never sent (the request that recorded it is gone).
pub const PENDING_GRACE_SECONDS: i64 = 120;
/// A sent command the owner has no record of after this was never applied: owners record
/// the `Idempotency-Key` in the same transaction as the change, and no owner request runs
/// this long.
pub const NOT_RECEIVED_AFTER_SECONDS: i64 = 600;
/// Most open operations settled per maintenance tick.
pub const SWEEP_LIMIT: u32 = 50;

/// What the family handler asks the core to send.
#[derive(Debug, Clone)]
pub struct CommandSpec {
    pub owner: Owner,
    /// Owner path under `/internal/v1/`.
    pub owner_path: String,
    pub owner_body: Value,
    /// Stable target name, e.g. `developer-platform:production-access:<uuid>`.
    pub target: String,
    pub expected_version: String,
    /// HQ's command id (body `operation_id`).
    pub operation_id: Uuid,
}

/// The owner's answer, reduced to what the ledger keeps plus the family's view.
#[derive(Debug, Clone, PartialEq)]
pub struct Applied {
    pub source_status: String,
    pub source_version: Option<String>,
    pub view: Value,
}

/// Fields every command body carries (`CommandBase` in the contract).
#[derive(Debug, Clone, PartialEq)]
pub struct BaseBody {
    pub operation_id: Uuid,
    pub expected_version: String,
    pub reason_code: Option<String>,
    pub note: Option<String>,
}

fn version_ok(v: &str) -> bool {
    !v.is_empty()
        && v.len() <= 64
        && v.bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b':' | b'-'))
}

pub(crate) fn reason_ok(v: &str) -> bool {
    !v.is_empty()
        && v.len() <= 64
        && v.as_bytes()[0].is_ascii_uppercase()
        && v.bytes()
            .all(|b| b.is_ascii_uppercase() || b.is_ascii_digit() || b == b'_')
}

/// Parses a command body: a JSON object with exactly the base fields plus `extra` (the
/// family's own fields). Unknown fields are refused.
pub fn parse_body(raw: &[u8], extra: &[&str]) -> Result<(BaseBody, Value), ErrorCode> {
    parse_command_body(raw, extra, true)
}

/// [`parse_body`]; for an unversioned resource (`versioned == false`) the body carries no
/// `expected_version` and [`BaseBody::expected_version`] is `"0"`.
pub fn parse_command_body(
    raw: &[u8],
    extra: &[&str],
    versioned: bool,
) -> Result<(BaseBody, Value), ErrorCode> {
    let bad = ErrorCode::CommandBodyInvalid;
    let v: Value = serde_json::from_slice(raw).map_err(|_| bad)?;
    let obj = v.as_object().ok_or(bad)?;
    let allowed = ["operation_id", "expected_version", "reason_code", "note"];
    if obj
        .keys()
        .any(|k| !allowed.contains(&k.as_str()) && !extra.contains(&k.as_str()))
    {
        return Err(bad);
    }
    let operation_id = obj
        .get("operation_id")
        .and_then(Value::as_str)
        .filter(|s| crate::chain::assertion::uuid_ok(s))
        .and_then(|s| Uuid::parse_str(s).ok())
        .ok_or(bad)?;
    // A string, or the owner's integer row version (HQ sends `case.version` as is).
    let expected_version = match (versioned, obj.get("expected_version")) {
        (false, None) => "0".to_string(),
        (false, Some(_)) => return Err(bad),
        (true, Some(Value::String(s))) if version_ok(s) => s.clone(),
        (true, Some(Value::Number(n))) if n.as_u64().is_some() => n.to_string(),
        _ => return Err(bad),
    };
    let reason_code = match obj.get("reason_code") {
        None | Some(Value::Null) => None,
        Some(Value::String(s)) if reason_ok(s) => Some(s.clone()),
        _ => return Err(bad),
    };
    let note = match obj.get("note") {
        None | Some(Value::Null) => None,
        Some(Value::String(s))
            if !s.trim().is_empty()
                && s.chars().count() <= 500
                && !s.chars().any(char::is_control) =>
        {
            Some(s.clone())
        }
        _ => return Err(bad),
    };
    Ok((
        BaseBody {
            operation_id,
            expected_version,
            reason_code,
            note,
        },
        v,
    ))
}

/// The intent an `Idempotency-Key` is bound to: capability, path parameters, purpose,
/// scope, the exact body and the approval used.
pub fn intent_sha256(auth: &Authorized) -> String {
    let params: BTreeMap<&str, &str> = auth.params.iter().map(|(k, v)| (*k, v.as_str())).collect();
    let v = json!({
        "capability": auth.capability.id,
        "params": params,
        "purpose": auth.query.purpose.as_str(),
        "scope": scope_of(auth),
        "body_sha256": crate::wire::sha256_hex(&auth.body),
        "approval_id": auth.staff.claims.approval.as_ref().map(|a| a.approval_id.clone()),
    });
    crate::wire::sha256_hex(&serde_json::to_vec(&v).expect("serializable"))
}

pub(crate) fn scope_of(auth: &Authorized) -> Value {
    let q = &auth.query;
    json!({"market": q.market, "organization": q.organization, "zone": q.zone, "tenant": q.tenant})
}

fn fmt(t: DateTime<Utc>) -> String {
    t.to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
}

/// The readback shape (`OperationReadback`).
pub fn readback_json(op: &Operation) -> Value {
    json!({
        "idempotency_key": op.idempotency_key.to_string(),
        "capability_id": op.capability,
        "status": op.status.as_str(),
        "source_status": op.source_status,
        "source_version": op.source_version,
        "settled_at": op.settled_at.map(fmt),
    })
}

pub(crate) fn stored_code(op: &Operation) -> ErrorCode {
    op.error_code
        .as_deref()
        .and_then(|c| ErrorCode::ALL.iter().copied().find(|e| e.code() == c))
        .unwrap_or(ErrorCode::OwnerRefused)
}

/// The answer for an operation as stored now.
fn answer_for(op: &Operation, view: Option<Value>) -> Result<Answer, ErrorCode> {
    match op.status {
        OpStatus::Succeeded => Ok(Answer {
            result: json!({"operation": readback_json(op), "resource": view.unwrap_or(Value::Null)}),
            freshness: "LIVE",
        }),
        OpStatus::Failed => Err(stored_code(op)),
        OpStatus::Pending | OpStatus::Sent | OpStatus::UnknownOutcome => {
            Err(ErrorCode::UnknownOutcome)
        }
    }
}

pub(crate) async fn audit_outcome(
    bridge: &Bridge,
    auth: &Authorized,
    op: &Operation,
    decision: &str,
    status: u16,
) {
    let draft = AuditDraft {
        correlation_id: auth.correlation_id,
        occurred_at: (bridge.clock)(),
        actor: format!("hq:{}", auth.staff.claims.sub),
        session_id: Some(auth.staff.claims.session_id.clone()),
        capability: Some(auth.capability.id.to_string()),
        method: auth.capability.method.as_str().to_string(),
        path: auth.capability.full_path(),
        purpose: Some(auth.query.purpose.as_str().to_string()),
        scope: scope_of(auth),
        decision: decision.to_string(),
        http_status: status,
        request_digest: Some(auth.staff.claims.request_digest.clone()),
        approval_id: op.approval_id.clone(),
        approver: auth
            .staff
            .claims
            .approval
            .as_ref()
            .map(|a| format!("hq:{}", a.approver_ref)),
        detail: json!({
            "operation": "COMMAND_OUTCOME",
            "idempotency_key": op.idempotency_key.to_string(),
            "operation_id": op.operation_id.to_string(),
            "target": op.target,
            "expected_version": op.expected_version,
            "status": op.status.as_str(),
            "source_status": op.source_status,
        }),
    };
    // The owner may already have acted: the ledger keeps the outcome, the answer stays true.
    if bridge.store.append_audit(&draft).await.is_err() {
        tracing::error!(correlation_id = %auth.correlation_id, "command outcome audit failed (ledger kept)");
    }
}

async fn record(
    bridge: &Bridge,
    op: &mut Operation,
    status: OpStatus,
    source_status: Option<String>,
    source_version: Option<String>,
    error: Option<ErrorCode>,
) {
    let at = (bridge.clock)();
    let s = Settlement {
        status,
        source_status: source_status.clone(),
        source_version: source_version.clone(),
        error_code: error.map(|e| e.code().to_string()),
        at,
    };
    match bridge
        .store
        .settle_operation(&op.principal_sub, op.idempotency_key, &s)
        .await
    {
        Ok(true) => {
            op.status = status;
            op.source_status = source_status;
            op.source_version = source_version;
            op.error_code = s.error_code;
            if status.settled() {
                op.settled_at = Some(at);
            }
        }
        // Not recorded: the sweep settles it later from the owner's record.
        Ok(false) | Err(_) => {
            tracing::warn!(target = %op.target, status = status.as_str(), "operation outcome not recorded");
        }
    }
}

/// Runs one command: replay or begin, send once, record and audit the outcome. `applied`
/// reduces the owner's 2xx body (an off-contract body leaves the outcome unknown).
pub async fn execute(
    bridge: &Bridge,
    auth: &Authorized,
    spec: CommandSpec,
    applied: impl Fn(&Value) -> Option<Applied>,
) -> Result<Answer, ErrorCode> {
    let key = auth
        .idempotency_key
        .ok_or(ErrorCode::IdempotencyKeyRequired)?;
    // One UUID per HQ command: Idempotency-Key, operation_id and the readback key.
    if spec.operation_id != key {
        return Err(ErrorCode::CommandBodyInvalid);
    }
    let intent = intent_sha256(auth);
    let sub = auth.staff.claims.sub.clone();

    let replay = |op: Operation| -> Result<Operation, ErrorCode> {
        if op.intent_sha256 != intent {
            Err(ErrorCode::IdempotencyConflict)
        } else {
            Ok(op)
        }
    };
    if let Some(op) = auth.existing.clone() {
        let mut op = replay(op)?;
        if !op.status.settled() {
            settle(bridge, &mut op).await;
        }
        return answer_for(&op, None);
    }

    let approval = auth.staff.claims.approval.as_ref();
    let mut op = Operation {
        principal_sub: sub.clone(),
        idempotency_key: key,
        operation_id: spec.operation_id,
        capability: auth.capability.id.to_string(),
        owner: spec.owner.as_str().to_string(),
        purpose: auth.query.purpose.as_str().to_string(),
        scope: scope_of(auth),
        intent_sha256: intent.clone(),
        target: spec.target.clone(),
        expected_version: spec.expected_version.clone(),
        approval_id: approval.map(|a| a.approval_id.clone()),
        correlation_id: auth.correlation_id,
        status: OpStatus::Pending,
        source_status: None,
        source_version: None,
        error_code: None,
        created_at: auth.now,
        sent_at: None,
        settled_at: None,
    };
    match bridge
        .store
        .begin_operation(&op)
        .await
        .map_err(|_| ErrorCode::BridgeUnavailable)?
    {
        BeginOutcome::Started => {}
        BeginOutcome::Existing(existing) => {
            let existing = replay(*existing)?;
            return answer_for(&existing, None);
        }
        BeginOutcome::VersionTaken => return Err(ErrorCode::VersionConflict),
    }
    // Two-person commands must carry the approval the chain consumed.
    let headers = match (auth.capability.approval(), approval) {
        (Approval::TwoPerson, Some(a)) => Some(ApprovalHeaders {
            approver: format!("hq:{}", a.approver_ref),
            approval_ref: a.approval_id.clone(),
            intent_digest: a.intent_digest.clone(),
        }),
        (Approval::TwoPerson, None) => {
            record(
                bridge,
                &mut op,
                OpStatus::Failed,
                Some("NOT_SENT".into()),
                None,
                Some(ErrorCode::ApprovalRequired),
            )
            .await;
            return Err(ErrorCode::ApprovalRequired);
        }
        (Approval::None, _) => None,
    };
    match bridge.store.mark_sent(&sub, key, (bridge.clock)()).await {
        Ok(true) => op.sent_at = Some((bridge.clock)()),
        // Never sent: the row stays PENDING and the sweep fails it after the grace period.
        Ok(false) => return Err(ErrorCode::UnknownOutcome),
        Err(_) => return Err(ErrorCode::BridgeUnavailable),
    }
    op.status = OpStatus::Sent;
    let operator = format!("hq:{sub}");
    let key_text = key.to_string();
    let outcome = bridge
        .owners
        .command(
            spec.owner,
            CommandCall {
                path: &spec.owner_path,
                body: &spec.owner_body,
                operator: &operator,
                approval: headers.as_ref(),
                idempotency_key: &key_text,
            },
        )
        .await;
    let result = match outcome {
        CommandOutcome::Applied(body) => match applied(&body) {
            Some(a) => {
                record(
                    bridge,
                    &mut op,
                    OpStatus::Succeeded,
                    Some(a.source_status),
                    a.source_version,
                    None,
                )
                .await;
                Ok(Answer {
                    result: json!({"operation": readback_json(&op), "resource": a.view}),
                    freshness: "LIVE",
                })
            }
            None => {
                record(
                    bridge,
                    &mut op,
                    OpStatus::UnknownOutcome,
                    None,
                    None,
                    Some(ErrorCode::UnknownOutcome),
                )
                .await;
                Err(ErrorCode::UnknownOutcome)
            }
        },
        CommandOutcome::Refused { status, code } => {
            let e = match status {
                404 => ErrorCode::ResourceNotFound,
                409 => ErrorCode::VersionConflict,
                _ => ErrorCode::OwnerRefused,
            };
            let source = code
                .filter(|c| reason_ok(c))
                .unwrap_or_else(|| format!("HTTP_{status}"));
            record(
                bridge,
                &mut op,
                OpStatus::Failed,
                Some(source),
                None,
                Some(e),
            )
            .await;
            Err(e)
        }
        CommandOutcome::NotSent { configured } => {
            let e = if configured {
                ErrorCode::Internal
            } else {
                ErrorCode::CapabilityDisabled
            };
            record(
                bridge,
                &mut op,
                OpStatus::Failed,
                Some("NOT_SENT".into()),
                None,
                Some(e),
            )
            .await;
            Err(e)
        }
        CommandOutcome::Unknown => {
            record(
                bridge,
                &mut op,
                OpStatus::UnknownOutcome,
                None,
                None,
                Some(ErrorCode::UnknownOutcome),
            )
            .await;
            Err(ErrorCode::UnknownOutcome)
        }
    };
    let (decision, status) = match &result {
        Ok(_) => ("SUCCEEDED".to_string(), 200),
        Err(e) => (e.code().to_string(), e.status()),
    };
    audit_outcome(bridge, auth, &op, &decision, status).await;
    result
}

/// Settles an open operation without ever resending: a `PENDING` row past its grace was
/// never sent (FAILED); a sent one is settled from the owner's record of its
/// `Idempotency-Key`, or FAILED as never received once the owner has had long enough.
pub async fn settle(bridge: &Bridge, op: &mut Operation) {
    // Release workflow dispatches settle from GitHub's runs list (routes::operations).
    if op.owner == super::operations::DISPATCH_OWNER {
        super::operations::settle_dispatch(bridge, op).await;
        return;
    }
    let now = (bridge.clock)();
    match op.status {
        OpStatus::Pending => {
            if now - op.created_at >= Duration::seconds(PENDING_GRACE_SECONDS) {
                record(
                    bridge,
                    op,
                    OpStatus::Failed,
                    Some("NOT_SENT".into()),
                    None,
                    Some(ErrorCode::BridgeUnavailable),
                )
                .await;
            }
        }
        OpStatus::Sent | OpStatus::UnknownOutcome => {
            let Some(owner) = Owner::parse(&op.owner) else {
                return;
            };
            let operator = format!("hq:{}", op.principal_sub);
            let record_of = bridge
                .owners
                .operation_record(owner, &op.idempotency_key.to_string(), &operator)
                .await;
            match record_of {
                Ok(Some(rec)) => {
                    if let Some((status, source, version)) = owner_record(&rec, op) {
                        let error = (status == OpStatus::Failed).then_some(ErrorCode::OwnerRefused);
                        record(bridge, op, status, Some(source), version, error).await;
                    } else {
                        tracing::error!(target = %op.target, "owner operation record is off-contract");
                    }
                }
                Ok(None) => {
                    let since = op.sent_at.unwrap_or(op.created_at);
                    if now - since >= Duration::seconds(NOT_RECEIVED_AFTER_SECONDS) {
                        record(
                            bridge,
                            op,
                            OpStatus::Failed,
                            Some("NOT_RECEIVED".into()),
                            None,
                            Some(ErrorCode::OwnerUnavailable),
                        )
                        .await;
                    }
                }
                Err(_) => {}
            }
        }
        OpStatus::Succeeded | OpStatus::Failed => {}
    }
}

/// `{"idempotency_key", "outcome": "APPLIED"|"REFUSED", "source_status", "source_version"}`
/// for this very operation, or `None` when off-contract.
fn owner_record(rec: &Value, op: &Operation) -> Option<(OpStatus, String, Option<String>)> {
    if rec.get("idempotency_key")?.as_str()? != op.idempotency_key.to_string() {
        return None;
    }
    let status = match rec.get("outcome")?.as_str()? {
        "APPLIED" => OpStatus::Succeeded,
        "REFUSED" => OpStatus::Failed,
        _ => return None,
    };
    let source = rec
        .get("source_status")?
        .as_str()
        .filter(|s| reason_ok(s))?
        .to_string();
    let version = match rec.get("source_version") {
        None | Some(Value::Null) => None,
        Some(Value::String(v)) if version_ok(v) => Some(v.clone()),
        _ => return None,
    };
    Some((status, source, version))
}

/// `bridge.operations.read`: the operation this staff member sent with that key, after a
/// settlement attempt. Another staff member's key is RESOURCE_NOT_FOUND (never leaked).
pub async fn readback(bridge: &Bridge, auth: &Authorized) -> Result<Answer, ErrorCode> {
    let key = auth
        .params
        .get("idempotency_key")
        .and_then(|k| Uuid::parse_str(k).ok())
        .ok_or(ErrorCode::QueryInvalid)?;
    let mut op = bridge
        .store
        .operation(&auth.staff.claims.sub, key)
        .await
        .map_err(|_| ErrorCode::BridgeUnavailable)?
        .ok_or(ErrorCode::ResourceNotFound)?;
    if op.purpose != auth.query.purpose.as_str() {
        return Err(ErrorCode::PurposeNotAllowed);
    }
    if op.scope != scope_of(auth) {
        return Err(ErrorCode::ScopeDenied);
    }
    if !op.status.settled() {
        settle(bridge, &mut op).await;
    }
    Ok(Answer {
        result: readback_json(&op),
        freshness: "LIVE",
    })
}

/// Maintenance: settles open operations (oldest first).
pub async fn sweep(bridge: &Bridge) -> usize {
    let now = (bridge.clock)();
    let Ok(open) = bridge
        .store
        .open_operations(
            now - Duration::seconds(PENDING_GRACE_SECONDS.min(NOT_RECEIVED_AFTER_SECONDS)),
            SWEEP_LIMIT,
        )
        .await
    else {
        return 0;
    };
    let mut settled = 0;
    for mut op in open {
        settle(bridge, &mut op).await;
        settled += usize::from(op.status.settled());
    }
    settled
}

/// The plainest command: the base body is forwarded as is, the target is the capability
/// plus its path parameters, and the owner answers `{"status", "version"}`.
pub async fn generic(
    bridge: &Bridge,
    auth: &Authorized,
    owner: Owner,
    owner_path: &str,
) -> Result<Answer, ErrorCode> {
    let (base, body) = parse_body(&auth.body, &[])?;
    let mut path = owner_path.to_string();
    let mut target = auth.capability.id.to_string();
    for (name, value) in &auth.params {
        path = path.replace(&format!("{{{name}}}"), value);
        target.push(':');
        target.push_str(value);
    }
    execute(
        bridge,
        auth,
        CommandSpec {
            owner,
            owner_path: path,
            owner_body: body,
            target,
            expected_version: base.expected_version,
            operation_id: base.operation_id,
        },
        status_and_version,
    )
    .await
}

/// `{"status": "<CODE>", "version": "<v>"|null, …}` → the whole body as the view.
pub fn status_and_version(v: &Value) -> Option<Applied> {
    let source_status = v
        .get("status")?
        .as_str()
        .filter(|s| reason_ok(s))?
        .to_string();
    let source_version = match v.get("version") {
        None | Some(Value::Null) => None,
        Some(Value::String(s)) if version_ok(s) => Some(s.clone()),
        _ => return None,
    };
    Some(Applied {
        source_status,
        source_version,
        view: v.clone(),
    })
}

/// A command on the bridge's OWN data ([`LocalChange`]): the operation and the change are
/// written in one transaction, so the outcome is always SUCCEEDED or FAILED, never unknown.
/// Replays and conflicts behave exactly as for owner commands. `view` renders the resource
/// after a success.
pub async fn execute_local(
    bridge: &Bridge,
    auth: &Authorized,
    target: String,
    expected_version: String,
    operation_id: Uuid,
    change: LocalChange,
) -> Result<Operation, ErrorCode> {
    let key = auth
        .idempotency_key
        .ok_or(ErrorCode::IdempotencyKeyRequired)?;
    if operation_id != key {
        return Err(ErrorCode::CommandBodyInvalid);
    }
    let intent = intent_sha256(auth);
    let sub = auth.staff.claims.sub.clone();
    let replay = |op: Operation| -> Result<Operation, ErrorCode> {
        if op.intent_sha256 != intent {
            return Err(ErrorCode::IdempotencyConflict);
        }
        match op.status {
            OpStatus::Succeeded => Ok(op),
            OpStatus::Failed => Err(stored_code(&op)),
            _ => Err(ErrorCode::UnknownOutcome),
        }
    };
    if let Some(op) = auth.existing.clone() {
        return replay(op);
    }
    let op = Operation {
        principal_sub: sub.clone(),
        idempotency_key: key,
        operation_id,
        capability: auth.capability.id.to_string(),
        owner: crate::contract::model::BRIDGE_OWNER.to_string(),
        purpose: auth.query.purpose.as_str().to_string(),
        scope: scope_of(auth),
        intent_sha256: intent.clone(),
        target,
        expected_version,
        approval_id: auth
            .staff
            .claims
            .approval
            .as_ref()
            .map(|a| a.approval_id.clone()),
        correlation_id: auth.correlation_id,
        status: OpStatus::Pending,
        source_status: None,
        source_version: None,
        error_code: None,
        created_at: auth.now,
        sent_at: None,
        settled_at: None,
    };
    let outcome = bridge
        .store
        .local_command(&op, &change)
        .await
        .map_err(|_| ErrorCode::BridgeUnavailable)?;
    let result = match outcome {
        LocalOutcome::Existing(existing) => return replay(*existing),
        LocalOutcome::VersionTaken => return Err(ErrorCode::VersionConflict),
        LocalOutcome::Applied { .. } | LocalOutcome::Refused(_) => bridge
            .store
            .operation(&sub, key)
            .await
            .map_err(|_| ErrorCode::BridgeUnavailable)?
            .ok_or(ErrorCode::Internal)?,
    };
    let (decision, status) = match result.status {
        OpStatus::Succeeded => ("SUCCEEDED".to_string(), 200),
        _ => {
            let e = stored_code(&result);
            (e.code().to_string(), e.status())
        }
    };
    audit_outcome(bridge, auth, &result, &decision, status).await;
    match result.status {
        OpStatus::Succeeded => Ok(result),
        _ => Err(stored_code(&result)),
    }
}
