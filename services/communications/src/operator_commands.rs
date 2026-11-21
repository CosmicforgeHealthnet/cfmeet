//! The admin bridge's operator COMMANDS (ADR 0068 phase A4, design §6.5 and §17.6).
//!
//! * `POST /internal/v1/communications/operator/profiles/{cfMeetId}/suspend` and `/restore`
//!   (R3): place or release the operator hold on a profile (the same hold the operator
//!   console's `admin/suspend` sets: new personal communications are denied by the policy
//!   decision; meetings admission and the Account lifecycle are unaffected);
//! * `POST /internal/v1/communications/operator/reports/{reportId}/review` (R3): the decision
//!   on one profile report, optionally holding the reported profile;
//! * `POST /internal/v1/communications/operator/backfill` (R2): one page of the Account's
//!   native accounts into profiles;
//! * `POST /internal/v1/communications/operator/replay` (R2): re-queue a DEAD outbox event;
//! * `GET /internal/v1/operator/operations/{idempotencyKey}`: the record the bridge settles an
//!   unknown outcome from.
//!
//! Only `cfmeet.admin-bridge` holds `communications.administer`, and every command names
//! the HQ staff member it acts for (`X-CF-Operator`) and carries HQ's `Idempotency-Key`; the
//! R3 commands also carry the second person (`X-CF-Approver`, a different person, verified
//! by the shared attribution parser). Versioned subjects change only at the
//! `expectedVersion` the bridge read (the row is locked in the transaction). Every applied
//! command is recorded in `communication_admin_operations` in the SAME transaction as the
//! change; a repeated key for the same command and subject answers the current resource
//! without applying again, any other reuse is `IDEMPOTENCY_CONFLICT`. Refusals are definite
//! 4xx with nothing done. Answers carry ids, states, versions and times only: never a name,
//! an avatar, an About line or contact data.

use crate::{
    error::{Error, Result},
    events::{backfill_account, backfill_page},
    operator::Operator,
    relationships::uuid_field,
    store::{execute, one, rows},
    State,
};
use axum::{
    extract::{Path, State as HttpState},
    http::HeaderMap,
    Extension, Json,
};
use cf_communications_policy::Scope;
use cf_service_auth::{ApplicationEnvironmentKind, DelegatedPlatformContext};
use sea_orm::ConnectionTrait;
use serde::Deserialize;
use serde_json::{json, Value};
use uuid::Uuid;

/// The commands the bridge can send (the ledger's `command`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Command {
    ProfileSuspend,
    ProfileRestore,
    ReportReview,
    BackfillRun,
    EventsReplay,
}

impl Command {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::ProfileSuspend => "PROFILE_SUSPEND",
            Self::ProfileRestore => "PROFILE_RESTORE",
            Self::ReportReview => "REPORT_REVIEW",
            Self::BackfillRun => "BACKFILL_RUN",
            Self::EventsReplay => "EVENTS_REPLAY",
        }
    }
    pub const fn subject(self) -> &'static str {
        match self {
            Self::ProfileSuspend | Self::ProfileRestore => "PROFILE",
            Self::ReportReview => "REPORT",
            Self::BackfillRun => "ENVIRONMENT",
            Self::EventsReplay => "OUTBOX_EVENT",
        }
    }
    /// R3: a second person is required.
    pub const fn two_person(self) -> bool {
        matches!(
            self,
            Self::ProfileSuspend | Self::ProfileRestore | Self::ReportReview
        )
    }
}

/// One verified bridge command.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Bridge {
    pub key: Uuid,
    /// `hq:<staff>`.
    pub operator: String,
    /// `(hq:<approver>, approval ref)`.
    pub approval: Option<(String, String)>,
}

impl Bridge {
    /// The bridge's attribution is required (403 otherwise; only the bridge can reach these
    /// routes), the second person for R3 commands (403), and the `Idempotency-Key` (a UUID,
    /// 400 otherwise).
    pub fn of(op: &Operator, headers: &HeaderMap, command: Command) -> Result<Self> {
        let attribution = op.0.as_ref().ok_or(Error::Forbidden)?;
        let approval = attribution
            .approval
            .as_ref()
            .map(|a| (a.approver.as_str().to_owned(), a.approval_ref.clone()));
        if command.two_person() && approval.is_none() {
            return Err(Error::Forbidden);
        }
        let key = idempotency_key(headers).ok_or(Error::Validation)?;
        Ok(Self {
            key,
            operator: attribution.operator.as_str().to_owned(),
            approval,
        })
    }

    /// Who acted, for the Hub's audit row (the person, never only the service).
    fn audit(&self, mut details: Value) -> Value {
        if let Some(map) = details.as_object_mut() {
            map.insert("operator".into(), json!(self.operator));
            map.insert(
                "approver".into(),
                json!(self.approval.as_ref().map(|a| &a.0)),
            );
            map.insert(
                "approvalRef".into(),
                json!(self.approval.as_ref().map(|a| &a.1)),
            );
            map.insert("idempotencyKey".into(), json!(self.key));
            map.insert("via".into(), json!("cfmeet.admin-bridge"));
        }
        details
    }
}

fn idempotency_key(headers: &HeaderMap) -> Option<Uuid> {
    let mut values = headers.get_all("idempotency-key").iter();
    let value = values.next()?;
    if values.next().is_some() {
        return None;
    }
    Uuid::parse_str(value.to_str().ok()?.trim()).ok()
}

fn expected(version: i64) -> Result<i64> {
    if version >= 1 {
        Ok(version)
    } else {
        Err(Error::Validation)
    }
}

/// `{"status", "version", "resource"}`: the version as the string of the integer.
fn answer(status: &str, version: Option<i64>, resource: Value) -> Json<Value> {
    Json(json!({
        "status": status,
        "version": version.map(|v| v.to_string()),
        "resource": resource,
    }))
}

/// The record of an earlier command under the same key: `Some` for the same command and
/// subject, `IDEMPOTENCY_CONFLICT` for anything else.
async fn prior<C: ConnectionTrait>(
    db: &C,
    bridge: &Bridge,
    command: Command,
    subject: Uuid,
) -> Result<Option<Value>> {
    let row: Option<Value> = one(
        db,
        "SELECT jsonb_build_object('command',command,'subjectKind',subject_kind,'subjectId',subject_id, \
           'state',resulting_state,'version',resulting_version,'resource',resulting_resource) doc \
         FROM communication_admin_operations WHERE idempotency_key=$1",
        vec![bridge.key.into()],
    )
    .await?;
    match row {
        None => Ok(None),
        Some(r)
            if r["command"] == command.as_str()
                && r["subjectKind"] == command.subject()
                && r["subjectId"] == subject.to_string() =>
        {
            Ok(Some(r))
        }
        Some(_) => Err(Error::Idempotency),
    }
}

/// The stored answer of an unversioned command (backfill, replay).
fn stored(row: &Value) -> Json<Value> {
    answer(
        row["state"].as_str().unwrap_or("UNKNOWN"),
        row["version"].as_i64(),
        row["resource"].clone(),
    )
}

/// Records the applied command in the change's transaction. `false`: the key was taken
/// meanwhile (the caller rolls back).
async fn record<C: ConnectionTrait>(
    txn: &C,
    bridge: &Bridge,
    command: Command,
    subject: Uuid,
    state: &str,
    version: Option<i64>,
    resource: &Value,
) -> Result<bool> {
    let (approver, approval_ref) = match &bridge.approval {
        Some((a, r)) => (Some(a.clone()), Some(r.clone())),
        None => (None, None),
    };
    let inserted = execute(
        txn,
        "INSERT INTO communication_admin_operations(idempotency_key,command,subject_kind,subject_id, \
           operator_ref,approver_ref,approval_ref,resulting_state,resulting_version,resulting_resource) \
         VALUES($1,$2,$3,$4,$5,$6,$7,$8,$9,$10) ON CONFLICT (idempotency_key) DO NOTHING",
        vec![
            bridge.key.into(),
            command.as_str().into(),
            command.subject().into(),
            subject.into(),
            bridge.operator.clone().into(),
            approver.into(),
            approval_ref.into(),
            state.into(),
            version.into(),
            resource.clone().into(),
        ],
    )
    .await?;
    Ok(inserted == 1)
}

fn scope_of(v: &Value) -> Result<Scope> {
    Ok(Scope {
        tenant_id: uuid_field(v, "tenantId")?,
        application_id: uuid_field(v, "applicationId")?,
        environment_id: uuid_field(v, "environmentId")?,
    })
}

// ---- Profiles: suspend / restore (the operator hold) ------------------------------------

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SuspendProfile {
    /// The profile's version (`version` of the operator profile read).
    expected_version: i64,
    /// ABUSE, SECURITY, LEGAL or OTHER.
    reason: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RestoreProfile {
    expected_version: i64,
}

/// A profile's operator state: scope ids (never answered), and the answer's resource.
async fn profile_state<C: ConnectionTrait>(db: &C, id: Uuid, lock: bool) -> Result<Value> {
    one(
        db,
        &format!(
            "SELECT jsonb_build_object('tenantId',p.tenant_id,'applicationId',p.application_id,'environmentId',p.environment_id, \
               'cfMeetId',(SELECT public_id FROM communication_public_ids WHERE profile_id=p.id AND current), \
               'activationState',p.activation_state,'version',p.version,'updatedAt',p.updated_at, \
               'held',COALESCE(h.active,false), \
               'hold',CASE WHEN h.profile_id IS NULL THEN NULL ELSE jsonb_build_object('reason',h.reason,'active',h.active,'updatedAt',h.updated_at) END) doc \
             FROM communication_profiles p LEFT JOIN communication_operator_holds h ON h.profile_id=p.id \
             WHERE p.id=$1{}",
            if lock { " FOR UPDATE OF p" } else { "" }
        ),
        vec![id.into()],
    )
    .await?
    .ok_or(Error::NotFound)
}

/// `SUSPENDED` while the operator hold is active, else `RESTORED`; the resource.
fn profile_answer(state: &Value) -> (&'static str, Option<i64>, Value) {
    let status = if state["held"] == true {
        "SUSPENDED"
    } else {
        "RESTORED"
    };
    let resource = json!({
        "cfMeetId": state["cfMeetId"],
        "activationState": state["activationState"],
        "held": state["held"],
        "hold": state["hold"],
        "version": state["version"],
        "updatedAt": state["updatedAt"],
    });
    (status, state["version"].as_i64(), resource)
}

async fn profile_by_cf_meet_id(s: &State, cf_meet_id: &str) -> Result<Uuid> {
    let public_id = crate::crypto::normalize_public_id(cf_meet_id);
    if !crate::crypto::valid_public_id(&public_id) {
        return Err(Error::NotFound);
    }
    let row: Value = one(
        &s.store.db,
        "SELECT jsonb_build_object('id',profile_id) doc FROM communication_public_ids WHERE public_id=$1 AND current",
        vec![public_id.into()],
    )
    .await?
    .ok_or(Error::NotFound)?;
    uuid_field(&row, "id")
}

/// Bumps the profile's version (the version HQ reads and sends back) and writes the
/// profile's change-feed entry; returns the new version.
async fn bump_profile<C: ConnectionTrait>(s: &State, txn: &C, id: Uuid) -> Result<i64> {
    let row: Value = one(
        txn,
        "UPDATE communication_profiles SET version=version+1,updated_at=now() WHERE id=$1 \
         RETURNING jsonb_build_object('version',version) doc",
        vec![id.into()],
    )
    .await?
    .ok_or(Error::NotFound)?;
    let version = row["version"].as_i64().ok_or(Error::Unavailable)?;
    s.store
        .change(txn, id, "PROFILE", Some(id), Some(version))
        .await?;
    Ok(version)
}

/// Places (`reason`) or releases (`None`) the operator hold at the profile's expected
/// version.
async fn hold_command(
    s: &State,
    bridge: Bridge,
    cf_meet_id: &str,
    command: Command,
    expected_version: i64,
    reason: Option<&str>,
) -> Result<Json<Value>> {
    let expected_version = expected(expected_version)?;
    let id = profile_by_cf_meet_id(s, cf_meet_id).await?;
    let txn = s.store.begin().await?;
    let current = profile_state(&txn, id, true).await?;
    if prior(&txn, &bridge, command, id).await?.is_some() {
        let (status, version, resource) = profile_answer(&current);
        return Ok(answer(status, version, resource));
    }
    if current["version"].as_i64() != Some(expected_version) {
        return Err(Error::Version);
    }
    let operation = match reason {
        Some(reason) => {
            execute(
                &txn,
                "INSERT INTO communication_operator_holds AS h (profile_id,reason,active) VALUES($1,$2,true) \
                 ON CONFLICT(profile_id) DO UPDATE SET reason=EXCLUDED.reason,active=true,version=h.version+1,updated_at=now()",
                vec![id.into(), reason.into()],
            )
            .await?;
            "OPERATOR_HOLD_APPLIED"
        }
        None => {
            // Nothing to restore: a definite refusal, nothing done.
            if current["held"] != true {
                return Err(Error::Version);
            }
            execute(
                &txn,
                "UPDATE communication_operator_holds SET active=false,version=version+1,updated_at=now() WHERE profile_id=$1",
                vec![id.into()],
            )
            .await?;
            "OPERATOR_HOLD_RELEASED"
        }
    };
    bump_profile(s, &txn, id).await?;
    s.store
        .audit_system(
            &txn,
            scope_of(&current)?,
            operation,
            Some(id),
            bridge.audit(json!({"reason": reason.or(current["hold"]["reason"].as_str())})),
        )
        .await?;
    let after = profile_state(&txn, id, false).await?;
    let (status, version, resource) = profile_answer(&after);
    if !record(&txn, &bridge, command, id, status, version, &resource).await? {
        return Err(Error::Idempotency);
    }
    txn.commit().await?;
    Ok(answer(status, version, resource))
}

/// `POST /internal/v1/communications/operator/profiles/{cfMeetId}/suspend` (R3).
pub async fn suspend_profile(
    HttpState(s): HttpState<State>,
    Extension(op): Extension<Operator>,
    Path(cf_meet_id): Path<String>,
    headers: HeaderMap,
    Json(input): Json<SuspendProfile>,
) -> Result<Json<Value>> {
    let bridge = Bridge::of(&op, &headers, Command::ProfileSuspend)?;
    if !matches!(
        input.reason.as_str(),
        "ABUSE" | "SECURITY" | "LEGAL" | "OTHER"
    ) {
        return Err(Error::Validation);
    }
    hold_command(
        &s,
        bridge,
        &cf_meet_id,
        Command::ProfileSuspend,
        input.expected_version,
        Some(&input.reason),
    )
    .await
}

/// `POST /internal/v1/communications/operator/profiles/{cfMeetId}/restore` (R3).
pub async fn restore_profile(
    HttpState(s): HttpState<State>,
    Extension(op): Extension<Operator>,
    Path(cf_meet_id): Path<String>,
    headers: HeaderMap,
    Json(input): Json<RestoreProfile>,
) -> Result<Json<Value>> {
    let bridge = Bridge::of(&op, &headers, Command::ProfileRestore)?;
    hold_command(
        &s,
        bridge,
        &cf_meet_id,
        Command::ProfileRestore,
        input.expected_version,
        None,
    )
    .await
}

// ---- Reports: review ----------------------------------------------------------------------

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ReviewReport {
    /// The report's version: 1 while open, 2 once reviewed (`version` of the report read).
    expected_version: i64,
    /// DISMISSED or ACTIONED.
    decision: String,
    /// NO_VIOLATION (dismissals), ABUSE, SECURITY, LEGAL or OTHER.
    reason: String,
    /// ACTIONED only: also place the operator hold on the reported profile.
    #[serde(default)]
    hold_target: bool,
}

/// The decision / reason / hold combinations a review may record (shared with the
/// operator console's `admin/reports/review`).
pub fn valid_review(decision: &str, reason: &str, hold_target: bool) -> bool {
    match decision {
        "DISMISSED" => reason == "NO_VIOLATION" && !hold_target,
        "ACTIONED" => matches!(reason, "ABUSE" | "SECURITY" | "LEGAL" | "OTHER"),
        _ => false,
    }
}

async fn report_state<C: ConnectionTrait>(db: &C, id: Uuid, lock: bool) -> Result<Value> {
    one(
        db,
        &format!(
            "SELECT jsonb_build_object('tenantId',t.tenant_id,'applicationId',t.application_id,'environmentId',t.environment_id, \
               'reportId',r.id,'targetProfileId',r.target_profile_id, \
               'targetCfMeetId',(SELECT public_id FROM communication_public_ids WHERE profile_id=r.target_profile_id AND current), \
               'decision',v.decision,'reason',v.reason,'held',COALESCE(v.held,false),'reviewedAt',v.reviewed_at, \
               'version',CASE WHEN v.report_id IS NULL THEN 1 ELSE 2 END) doc \
             FROM communication_profile_reports r JOIN communication_profiles t ON t.id=r.target_profile_id \
             LEFT JOIN communication_profile_report_reviews v ON v.report_id=r.id WHERE r.id=$1{}",
            if lock { " FOR UPDATE OF r" } else { "" }
        ),
        vec![id.into()],
    )
    .await?
    .ok_or(Error::NotFound)
}

/// The decision (`DISMISSED` / `ACTIONED`, `OPEN` before a review); the resource.
fn report_answer(state: &Value) -> (String, Option<i64>, Value) {
    let status = state["decision"].as_str().unwrap_or("OPEN").to_owned();
    let resource = json!({
        "reportId": state["reportId"],
        "targetCfMeetId": state["targetCfMeetId"],
        "decision": state["decision"],
        "reason": state["reason"],
        "held": state["held"],
        "reviewedAt": state["reviewedAt"],
        "version": state["version"],
    });
    (status, state["version"].as_i64(), resource)
}

/// `POST /internal/v1/communications/operator/reports/{reportId}/review` (R3): the decision
/// on one report, once (a reviewed report is at version 2: any further review is a version
/// conflict), recording the HQ staff member as the reviewer; with `holdTarget` (ACTIONED
/// only) the reported profile is held in the same transaction.
pub async fn review_report(
    HttpState(s): HttpState<State>,
    Extension(op): Extension<Operator>,
    Path(report_id): Path<Uuid>,
    headers: HeaderMap,
    Json(input): Json<ReviewReport>,
) -> Result<Json<Value>> {
    let command = Command::ReportReview;
    let bridge = Bridge::of(&op, &headers, command)?;
    let expected_version = expected(input.expected_version)?;
    // `reviewed_by` holds at most 128 characters.
    if !valid_review(&input.decision, &input.reason, input.hold_target)
        || bridge.operator.len() > 128
    {
        return Err(Error::Validation);
    }
    let txn = s.store.begin().await?;
    let current = report_state(&txn, report_id, true).await?;
    if prior(&txn, &bridge, command, report_id).await?.is_some() {
        let (status, version, resource) = report_answer(&current);
        return Ok(answer(&status, version, resource));
    }
    if current["version"].as_i64() != Some(expected_version) {
        return Err(Error::Version);
    }
    let inserted = execute(
        &txn,
        "INSERT INTO communication_profile_report_reviews(report_id,decision,reason,held,reviewed_by) VALUES($1,$2,$3,$4,$5) ON CONFLICT DO NOTHING",
        vec![
            report_id.into(),
            input.decision.clone().into(),
            input.reason.clone().into(),
            input.hold_target.into(),
            bridge.operator.clone().into(),
        ],
    )
    .await?;
    if inserted == 0 {
        return Err(Error::Version);
    }
    let target = uuid_field(&current, "targetProfileId")?;
    if input.hold_target {
        let hold_reason = if input.reason == "NO_VIOLATION" {
            "OTHER"
        } else {
            input.reason.as_str()
        };
        execute(
            &txn,
            "INSERT INTO communication_operator_holds AS h (profile_id,reason,active) VALUES($1,$2,true) \
             ON CONFLICT(profile_id) DO UPDATE SET reason=EXCLUDED.reason,active=true,version=h.version+1,updated_at=now()",
            vec![target.into(), hold_reason.into()],
        )
        .await?;
        bump_profile(&s, &txn, target).await?;
    }
    s.store
        .audit_system(
            &txn,
            scope_of(&current)?,
            "PROFILE_REPORT_REVIEWED",
            Some(target),
            bridge.audit(json!({"reportId":report_id,"decision":input.decision,"reason":input.reason,"held":input.hold_target})),
        )
        .await?;
    let after = report_state(&txn, report_id, false).await?;
    let (status, version, resource) = report_answer(&after);
    // ADR 0068 A7: the reviewed report to the admin bridge's event feed (when configured).
    crate::admin_events::enqueue_report(
        &s,
        &txn,
        report_id,
        after["decision"].as_str(),
        version.unwrap_or(2),
    )
    .await?;
    if !record(
        &txn, &bridge, command, report_id, &status, version, &resource,
    )
    .await?
    {
        return Err(Error::Idempotency);
    }
    txn.commit().await?;
    Ok(answer(&status, version, resource))
}

// ---- Backfill and replay (R2, unversioned) ------------------------------------------------

/// A command whose subject has no version: when another request took the key meanwhile,
/// its stored answer (same command and subject) or `IDEMPOTENCY_CONFLICT`.
async fn taken(s: &State, bridge: &Bridge, command: Command, subject: Uuid) -> Result<Json<Value>> {
    match prior(&s.store.db, bridge, command, subject).await? {
        Some(row) => Ok(stored(&row)),
        None => Err(Error::Idempotency),
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RunBackfill {
    /// 1–100 accounts.
    limit: u64,
    /// The previous page's `nextCursor`.
    #[serde(default)]
    cursor: Option<Uuid>,
    /// The first-party environment to backfill; optional while the Hub knows exactly one.
    #[serde(default)]
    environment_id: Option<Uuid>,
}

/// The delegated authority the Hub stored for one environment (the same reference the
/// outbox delivers with): the named one, or the only one the Hub knows.
async fn environment_authority(
    s: &State,
    environment: Option<Uuid>,
) -> Result<DelegatedPlatformContext> {
    let known: Vec<Value> = rows(
        &s.store.db,
        "SELECT jsonb_build_object('tenantId',tenant_id,'applicationId',application_id,'environmentId',environment_id, \
           'serviceAccountId',service_account_id,'environmentKind',environment_kind) doc \
         FROM communication_scope_authorities WHERE ($1::uuid IS NULL OR environment_id=$1) \
         ORDER BY tenant_id,application_id,environment_id LIMIT 2",
        vec![environment.into()],
    )
    .await?;
    let authority = match known.as_slice() {
        [] => return Err(Error::NotFound),
        [one] => one,
        // Several environments: the bridge must name one.
        _ => return Err(Error::Validation),
    };
    let kind: ApplicationEnvironmentKind =
        serde_json::from_value(authority["environmentKind"].clone())
            .map_err(|_| Error::Unavailable)?;
    Ok(DelegatedPlatformContext::new(
        uuid_field(authority, "tenantId")?,
        uuid_field(authority, "applicationId")?,
        uuid_field(authority, "environmentId")?,
        kind,
        uuid_field(authority, "serviceAccountId")?,
    ))
}

/// `POST /internal/v1/communications/operator/backfill` (R2): one page of the Account's
/// native accounts into profiles (existing profiles are kept), in one transaction with its
/// record. Answers `COMPLETED` with `{environmentId, created, nextCursor}`; a repeated key
/// returns that answer without asking the Account again.
pub async fn run_backfill(
    HttpState(s): HttpState<State>,
    Extension(op): Extension<Operator>,
    headers: HeaderMap,
    Json(input): Json<RunBackfill>,
) -> Result<Json<Value>> {
    let command = Command::BackfillRun;
    let bridge = Bridge::of(&op, &headers, command)?;
    if !(1..=100).contains(&input.limit) {
        return Err(Error::Validation);
    }
    let authority = environment_authority(&s, input.environment_id).await?;
    let scope = Scope::of(&authority);
    if let Some(row) = prior(&s.store.db, &bridge, command, scope.environment_id).await? {
        return Ok(stored(&row));
    }
    let page = backfill_page(&s, &authority, input.limit, input.cursor).await?;
    let txn = s.store.begin().await?;
    let mut created = 0;
    for account in &page.accounts {
        if backfill_account(
            &s,
            &txn,
            scope,
            account,
            bridge.audit(json!({"source":"ACCOUNT"})),
        )
        .await?
        {
            created += 1;
        }
    }
    s.store
        .audit_system(
            &txn,
            scope,
            "OPERATOR_BACKFILL_RUN",
            None,
            bridge.audit(json!({"created":created,"limit":input.limit})),
        )
        .await?;
    let status = "COMPLETED";
    let resource = json!({
        "environmentId": scope.environment_id,
        "created": created,
        "nextCursor": page.next_cursor,
    });
    if !record(
        &txn,
        &bridge,
        command,
        scope.environment_id,
        status,
        None,
        &resource,
    )
    .await?
    {
        drop(txn);
        return taken(&s, &bridge, command, scope.environment_id).await;
    }
    txn.commit().await?;
    Ok(answer(status, None, resource))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ReplayEvent {
    event_id: Uuid,
}

/// `POST /internal/v1/communications/operator/replay` (R2): re-queues the DEAD deliveries of
/// one outbox event (any scope). Unknown event: 404; nothing DEAD: 409, nothing done.
/// Answers `REQUEUED` with `{eventId, requeued}`.
pub async fn replay_event(
    HttpState(s): HttpState<State>,
    Extension(op): Extension<Operator>,
    headers: HeaderMap,
    Json(input): Json<ReplayEvent>,
) -> Result<Json<Value>> {
    let command = Command::EventsReplay;
    let bridge = Bridge::of(&op, &headers, command)?;
    if let Some(row) = prior(&s.store.db, &bridge, command, input.event_id).await? {
        return Ok(stored(&row));
    }
    let txn = s.store.begin().await?;
    let deliveries: Vec<Value> = rows(
        &txn,
        "SELECT jsonb_build_object('tenantId',tenant_id,'applicationId',application_id,'environmentId',environment_id,'state',state) doc \
         FROM communication_outbox WHERE event_id=$1 ORDER BY seq FOR UPDATE",
        vec![input.event_id.into()],
    )
    .await?;
    let first = deliveries.first().ok_or(Error::NotFound)?;
    if !deliveries.iter().any(|d| d["state"] == "DEAD") {
        return Err(Error::Version);
    }
    let requeued = execute(
        &txn,
        "UPDATE communication_outbox SET state='PENDING',attempts=0,next_attempt_at=now(),lease_id=NULL,lease_until=NULL \
         WHERE event_id=$1 AND state='DEAD'",
        vec![input.event_id.into()],
    )
    .await?;
    s.store
        .audit_system(
            &txn,
            scope_of(first)?,
            "OUTBOX_REPLAY_REQUESTED",
            Some(input.event_id),
            bridge.audit(json!({"requeued":requeued})),
        )
        .await?;
    let status = "REQUEUED";
    let resource = json!({"eventId": input.event_id, "requeued": requeued});
    if !record(
        &txn,
        &bridge,
        command,
        input.event_id,
        status,
        None,
        &resource,
    )
    .await?
    {
        drop(txn);
        return taken(&s, &bridge, command, input.event_id).await;
    }
    txn.commit().await?;
    Ok(answer(status, None, resource))
}

// ---- The settlement record --------------------------------------------------------------

/// `GET /internal/v1/operator/operations/{idempotencyKey}`: the bridge's settlement read.
/// 404 for any caller without the bridge's attribution and for an unknown key.
pub async fn operation_record(
    HttpState(s): HttpState<State>,
    Extension(op): Extension<Operator>,
    Path(key): Path<String>,
) -> Result<Json<Value>> {
    if op.0.is_none() {
        return Err(Error::NotFound);
    }
    let key = Uuid::parse_str(&key).map_err(|_| Error::NotFound)?;
    let row: Value = one(
        &s.store.db,
        "SELECT jsonb_build_object('state',resulting_state,'version',resulting_version) doc \
         FROM communication_admin_operations WHERE idempotency_key=$1",
        vec![key.into()],
    )
    .await?
    .ok_or(Error::NotFound)?;
    Ok(Json(json!({
        "idempotency_key": key,
        "outcome": "APPLIED",
        "source_status": row["state"],
        "source_version": row["version"].as_i64().map(|v| v.to_string()),
    })))
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::HeaderValue;
    use cf_service_auth::{OperatorAttribution, ServiceId};

    fn headers(pairs: &[(&'static str, &str)]) -> HeaderMap {
        let mut h = HeaderMap::new();
        for (k, v) in pairs {
            h.append(*k, HeaderValue::from_str(v).unwrap());
        }
        h
    }

    fn operator(pairs: &[(&'static str, &str)]) -> Operator {
        Operator(
            OperatorAttribution::from_headers(&ServiceId::admin_bridge(), &headers(pairs)).unwrap(),
        )
    }

    const DIGEST: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";

    #[test]
    fn bridge_commands_need_attribution_key_and_for_r3_a_second_person() {
        let key = Uuid::new_v4().to_string();
        let alone = operator(&[("x-cf-operator", "hq:staff-1")]);
        let approved = operator(&[
            ("x-cf-operator", "hq:staff-1"),
            ("x-cf-approver", "hq:staff-2"),
            ("x-cf-approval-ref", "appr-1"),
            ("x-cf-intent-digest", DIGEST),
        ]);
        let with_key = headers(&[("idempotency-key", &key)]);
        // No attribution (the operator console): refused.
        assert!(matches!(
            Bridge::of(&Operator(None), &with_key, Command::BackfillRun),
            Err(Error::Forbidden)
        ));
        // R3 without the second person: refused; R2 is fine.
        for c in [
            Command::ProfileSuspend,
            Command::ProfileRestore,
            Command::ReportReview,
        ] {
            assert!(matches!(
                Bridge::of(&alone, &with_key, c),
                Err(Error::Forbidden)
            ));
            let b = Bridge::of(&approved, &with_key, c).unwrap();
            assert_eq!(
                b.approval,
                Some(("hq:staff-2".to_owned(), "appr-1".to_owned()))
            );
        }
        for c in [Command::BackfillRun, Command::EventsReplay] {
            let b = Bridge::of(&alone, &with_key, c).unwrap();
            assert_eq!(b.operator, "hq:staff-1");
            assert_eq!(b.key.to_string(), key);
        }
        // The key is required, a UUID, sent once.
        for bad in [
            headers(&[]),
            headers(&[("idempotency-key", "not-a-uuid")]),
            headers(&[("idempotency-key", &key), ("idempotency-key", &key)]),
        ] {
            assert!(matches!(
                Bridge::of(&alone, &bad, Command::EventsReplay),
                Err(Error::Validation)
            ));
        }
    }

    #[test]
    fn answers_carry_the_version_as_a_string_and_states_in_upper_snake() {
        let Json(a) = answer("SUSPENDED", Some(7), json!({}));
        assert_eq!(a["version"], "7");
        assert_eq!(a["status"], "SUSPENDED");
        let Json(a) = answer("COMPLETED", None, json!({}));
        assert!(a["version"].is_null());
        let held = json!({"held": true, "version": 3, "cfMeetId": "CF-ABCDEF", "hold": {"reason": "ABUSE"}});
        let (status, version, resource) = profile_answer(&held);
        assert_eq!((status, version), ("SUSPENDED", Some(3)));
        assert!(resource.get("displayName").is_none());
        let (status, _, _) = profile_answer(&json!({"held": false, "version": 4}));
        assert_eq!(status, "RESTORED");
        let (status, version, _) = report_answer(&json!({"decision": null, "version": 1}));
        assert_eq!((status.as_str(), version), ("OPEN", Some(1)));
    }

    #[test]
    fn reviews_and_versions_are_validated() {
        assert!(valid_review("DISMISSED", "NO_VIOLATION", false));
        assert!(!valid_review("DISMISSED", "NO_VIOLATION", true));
        assert!(!valid_review("DISMISSED", "ABUSE", false));
        assert!(valid_review("ACTIONED", "ABUSE", true));
        assert!(!valid_review("ACTIONED", "NO_VIOLATION", false));
        assert!(!valid_review("MAYBE", "OTHER", false));
        assert!(expected(0).is_err() && expected(-1).is_err());
        assert_eq!(expected(1).unwrap(), 1);
    }

    #[test]
    fn commands_name_their_subjects() {
        assert_eq!(Command::ProfileSuspend.subject(), "PROFILE");
        assert_eq!(Command::ReportReview.subject(), "REPORT");
        assert_eq!(Command::BackfillRun.subject(), "ENVIRONMENT");
        assert_eq!(Command::EventsReplay.subject(), "OUTBOX_EVENT");
        assert!(!Command::BackfillRun.two_person() && !Command::EventsReplay.two_person());
    }
}
