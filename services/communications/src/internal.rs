//! Section 16: private owner-event, readiness, reconciliation and operator routes. All
//! sit behind signed service authentication with one explicit permission each (see
//! `auth.rs`), durable replay protection, and never appear in public Swagger.
use crate::{
    auth,
    error::{Error, Result},
    model::*,
    relationships::uuid_field,
    store::{self, execute, one},
    State,
};
use axum::{extract::State as HttpState, Extension, Json};
use cf_communications_policy::Scope;
use cf_service_auth::{InternalServiceContext, ServiceId};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use uuid::Uuid;

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct OwnerEvent {
    pub event_id: Uuid,
    pub event_type: String,
    pub resource_id: Uuid,
    pub source_version: i64,
    #[serde(default)]
    pub principal_id: Option<Uuid>,
    #[serde(default)]
    pub state: Option<String>,
}

async fn dedupe(
    s: &State,
    txn: &sea_orm::DatabaseTransaction,
    source: &str,
    event: &OwnerEvent,
) -> Result<bool> {
    let body = serde_json::to_value(event).map_err(|_| Error::Validation)?;
    let hash = crate::crypto::digest(&body);
    let inserted=execute(txn,"INSERT INTO communication_inbox_dedupe(source,event_id,request_digest) VALUES($1,$2,$3) ON CONFLICT DO NOTHING",vec![source.into(),event.event_id.into(),hash.clone().into()]).await?;
    if inserted == 0 {
        let r:Value=one(txn,"SELECT jsonb_build_object('d',encode(request_digest,'hex')) doc FROM communication_inbox_dedupe WHERE source=$1 AND event_id=$2",vec![source.into(),event.event_id.into()]).await?.ok_or(Error::Unavailable)?;
        if r["d"].as_str() != Some(hex::encode(&hash).as_str()) {
            return Err(Error::Idempotency);
        }
    }
    let _ = s;
    Ok(inserted > 0)
}

/// `/events/{calling|messaging|meetings|devices}`: owner lifecycle references. Each
/// family accepts only its owner and a closed set of event types; payloads carry ids and
/// states, never content. Events only invalidate projections; reads re-ask the owner.
pub async fn owner_event(
    HttpState(s): HttpState<State>,
    Extension(ctx): Extension<InternalServiceContext>,
    axum::extract::Path(family): axum::extract::Path<String>,
    Json(event): Json<OwnerEvent>,
) -> Result<Json<Value>> {
    let scope = auth::delegated(&ctx)?;
    auth::validate_scope(&s, &scope).await?;
    let caller = ctx.authenticated_service();
    let allowed: &[&str] = match family.as_str() {
        "calling" if caller == &ServiceId::calling() => &["call.state_changed", "call.ended"],
        "messaging" if caller == &ServiceId::messaging() => {
            &["conversation.member_removed", "device.revoked"]
        }
        "meetings" if caller == &ServiceId::meeting_service() => &["meeting.ended"],
        "devices" if caller == &ServiceId::calling() || caller == &ServiceId::messaging() => {
            &["device.revoked", "endpoint.revoked"]
        }
        "calling" | "messaging" | "meetings" | "devices" => return Err(Error::Forbidden),
        _ => return Err(Error::NotFound),
    };
    if !allowed.contains(&event.event_type.as_str())
        || event.source_version < 0
        || event.state.as_deref().is_some_and(|v| {
            v.len() > 40 || !v.bytes().all(|b| b.is_ascii_uppercase() || b == b'_')
        })
    {
        return Err(Error::Validation);
    }
    let txn = s.store.begin().await?;
    let source = format!("{}/{}", caller.as_str(), family);
    if !dedupe(&s, &txn, &source, &event).await? {
        txn.commit().await?;
        return Ok(Json(json!({"applied":false,"duplicate":true})));
    }
    let scope_ids = Scope::of(&scope);
    let applied = match event.event_type.as_str() {
        // Mark cards for refresh; the next read asks Calling for the canonical outcome.
        "call.state_changed" | "call.ended" | "meeting.ended" => {
            execute(&txn,"UPDATE communication_timeline_entries e SET refreshed_at='epoch' FROM communication_profiles p WHERE p.id=e.viewer_profile_id AND p.tenant_id=$1 AND p.application_id=$2 AND p.environment_id=$3 AND e.source_id=$4 AND e.source_version<=$5",
                vec![scope_ids.tenant_id.into(),scope_ids.application_id.into(),scope_ids.environment_id.into(),event.resource_id.into(),event.source_version.into()]).await?
        }
        "conversation.member_removed" => 0, // membership is always re-read live (MS-1)
        "device.revoked" | "endpoint.revoked" => {
            let column = if event.event_type == "device.revoked" { "chat_device_ref" } else { "call_endpoint_ref" };
            let principal = event.principal_id.ok_or(Error::Validation)?;
            let mut v = store::scoped(scope_ids);
            v.extend([principal.into(), event.resource_id.into()]);
            let rows:Vec<Value>=store::rows(&txn,&format!("UPDATE communication_devices d SET state='REVOKED',revoked_at=now(),revocation_reason='OWNER_SERVICE',push_platform=NULL,push_registered_at=NULL,version=d.version+1,updated_at=now() FROM communication_profiles p WHERE p.id=d.profile_id AND p.tenant_id=$1 AND p.application_id=$2 AND p.environment_id=$3 AND p.principal_id=$4 AND d.{column}=$5 AND d.state='ACTIVE' RETURNING jsonb_build_object('id',d.id,'profile',d.profile_id,'version',d.version) doc"),v).await?;
            for r in &rows {
                s.store.change(&txn, uuid_field(r, "profile")?, "DEVICE", Some(uuid_field(r, "id")?), r["version"].as_i64()).await?;
            }
            rows.len() as u64
        }
        _ => return Err(Error::Validation),
    };
    s.store
        .audit_system(
            &txn,
            scope_ids,
            "OWNER_EVENT_APPLIED",
            Some(event.event_id),
            json!({"family":family,"type":event.event_type,"rows":applied}),
        )
        .await?;
    txn.commit().await?;
    Ok(Json(
        json!({"applied":true,"duplicate":false,"rows":applied}),
    ))
}

/// Detailed readiness for operators (no personal data).
pub async fn readiness(HttpState(s): HttpState<State>) -> Result<Json<Value>> {
    let migrations = communications_migration::ready(&s.store.db)
        .await
        .unwrap_or(false);
    let outbox:Value=one(&s.store.db,"SELECT jsonb_build_object('pending',count(*) FILTER (WHERE state IN ('PENDING','INFLIGHT')),'dead',count(*) FILTER (WHERE state='DEAD'),'oldestPendingSeconds',COALESCE(EXTRACT(EPOCH FROM now()-min(created_at) FILTER (WHERE state IN ('PENDING','INFLIGHT'))),0)) doc FROM communication_outbox",vec![]).await?.ok_or(Error::Unavailable)?;
    Ok(Json(
        json!({"schemaVersion":1,"ready":migrations,"databaseMigrationsCurrent":migrations,
        "owners":{"platformAccess":"CONFIGURED","account":"CONFIGURED",
            "calling":if s.config.calling_configured {"CONFIGURED"} else {"NOT_CONFIGURED"},
            "messaging":if s.config.messaging_configured {"CONFIGURED"} else {"NOT_CONFIGURED"},
            "meetings":"CONFIGURED"},
        "phoneVerification":"PROVIDER_BLOCKED","outbox":outbox}),
    ))
}

/// Operator metrics: process counters plus bounded aggregate table counts.
pub async fn admin_metrics(HttpState(s): HttpState<State>) -> Result<Json<Value>> {
    let counts: Value = one(
        &s.store.db,
        "SELECT jsonb_build_object('profiles',(SELECT count(*) FROM communication_profiles),
        'connections',(SELECT count(*) FROM communication_connections WHERE state='CONNECTED'),
        'devicesActive',(SELECT count(*) FROM communication_devices WHERE state='ACTIVE'),
        'timelineEntries',(SELECT count(*) FROM communication_timeline_entries),
        'changeFeedEntries',(SELECT count(*) FROM communication_changes),
        'outboxDead',(SELECT count(*) FROM communication_outbox WHERE state='DEAD'),
        'operatorHolds',(SELECT count(*) FROM communication_operator_holds WHERE active)) doc",
        vec![],
    )
    .await?
    .ok_or(Error::Unavailable)?;
    let mut m = s.metrics.snapshot();
    m["tables"] = counts;
    Ok(Json(m))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Reconcile {
    from: chrono::DateTime<chrono::Utc>,
    to: chrono::DateTime<chrono::Utc>,
    #[serde(default)]
    requeue_dead_events: bool,
}
/// Resumable, bounded reconciliation for a time range: non-terminal cards are marked
/// for refresh from their owners, and DEAD outbox events can be re-queued.
pub async fn reconcile(
    HttpState(s): HttpState<State>,
    Extension(ctx): Extension<InternalServiceContext>,
    Json(input): Json<Reconcile>,
) -> Result<Json<Value>> {
    if input.to <= input.from || input.to - input.from > chrono::Duration::days(31) {
        return Err(Error::Validation);
    }
    let scope = auth::delegated(&ctx)?;
    auth::validate_scope(&s, &scope).await?;
    let sc = Scope::of(&scope);
    let txn = s.store.begin().await?;
    let cards=execute(&txn,"UPDATE communication_timeline_entries e SET refreshed_at='epoch' FROM communication_profiles p WHERE p.id=e.viewer_profile_id AND p.tenant_id=$1 AND p.application_id=$2 AND p.environment_id=$3 AND e.occurred_at>=$4 AND e.occurred_at<$5 AND COALESCE(e.data->>'terminal','false')<>'true'",
        vec![sc.tenant_id.into(),sc.application_id.into(),sc.environment_id.into(),input.from.into(),input.to.into()]).await?;
    let events = if input.requeue_dead_events {
        execute(&txn,"UPDATE communication_outbox SET state='PENDING',attempts=0,next_attempt_at=now(),lease_id=NULL,lease_until=NULL WHERE tenant_id=$1 AND application_id=$2 AND environment_id=$3 AND state='DEAD' AND created_at>=$4 AND created_at<$5",
            vec![sc.tenant_id.into(),sc.application_id.into(),sc.environment_id.into(),input.from.into(),input.to.into()]).await?
    } else {
        0
    };
    s.store
        .audit_system(
            &txn,
            sc,
            "RECONCILIATION_REQUESTED",
            None,
            json!({"cards":cards,"requeuedEvents":events}),
        )
        .await?;
    txn.commit().await?;
    Ok(Json(
        json!({"schemaVersion":1,"cardsMarkedForRefresh":cards,"requeuedEvents":events}),
    ))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Suspend {
    cf_meet_id: String,
    active: bool,
    reason: String,
}
/// Operator hold: denies new personal communications through the policy decision.
/// Meetings admission and Account lifecycle are unaffected.
pub async fn suspend(
    HttpState(s): HttpState<State>,
    Extension(ctx): Extension<InternalServiceContext>,
    Json(input): Json<Suspend>,
) -> Result<Json<Value>> {
    if !matches!(
        input.reason.as_str(),
        "ABUSE" | "SECURITY" | "LEGAL" | "OTHER"
    ) {
        return Err(Error::Validation);
    }
    let scope = auth::delegated(&ctx)?;
    auth::validate_scope(&s, &scope).await?;
    let sc = Scope::of(&scope);
    let target: Profile = s
        .store
        .by_public_id(&s.store.db, sc, &input.cf_meet_id)
        .await?;
    let txn = s.store.begin().await?;
    let row:Value=one(&txn,"INSERT INTO communication_operator_holds AS h (profile_id,reason,active) VALUES($1,$2,$3) ON CONFLICT(profile_id) DO UPDATE SET reason=EXCLUDED.reason,active=EXCLUDED.active,version=h.version+1,updated_at=now() RETURNING to_jsonb(h) doc",
        vec![target.id.into(),input.reason.clone().into(),input.active.into()]).await?.ok_or(Error::Unavailable)?;
    s.store
        .audit_system(
            &txn,
            sc,
            if input.active {
                "OPERATOR_HOLD_APPLIED"
            } else {
                "OPERATOR_HOLD_RELEASED"
            },
            Some(target.id),
            json!({"reason":input.reason,"operator":ctx.authenticated_service().as_str()}),
        )
        .await?;
    s.store
        .change(
            &txn,
            target.id,
            "PROFILE",
            Some(target.id),
            row["version"].as_i64(),
        )
        .await?;
    txn.commit().await?;
    Ok(Json(
        json!({"schemaVersion":1,"cfMeetId":input.cf_meet_id,"held":input.active,"version":row["version"]}),
    ))
}

/// Phone verification and recovery stay unavailable until a lawful provider exists.
pub async fn phone_deferred() -> Error {
    Error::DependencyNotConfigured
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ListReports {
    /// OPEN (default), DISMISSED, ACTIONED or ALL.
    #[serde(default)]
    status: Option<String>,
    #[serde(default)]
    limit: Option<i64>,
}
/// Gap audit S4: the operator queue of profile reports in the delegated scope, oldest
/// first: reason, both parties' public CF Meet IDs, how many reports the target has and
/// the review, if any. Reports carry no free text.
pub async fn list_reports(
    HttpState(s): HttpState<State>,
    Extension(ctx): Extension<InternalServiceContext>,
    Json(input): Json<ListReports>,
) -> Result<Json<Value>> {
    let status = input.status.as_deref().unwrap_or("OPEN");
    if !matches!(status, "OPEN" | "DISMISSED" | "ACTIONED" | "ALL") {
        return Err(Error::Validation);
    }
    let limit = input.limit.unwrap_or(50);
    if !(1..=200).contains(&limit) {
        return Err(Error::Validation);
    }
    let scope = auth::delegated(&ctx)?;
    auth::validate_scope(&s, &scope).await?;
    let sc = Scope::of(&scope);
    let mut values = crate::store::scoped(sc);
    values.push(status.into());
    values.push(limit.into());
    let items: Vec<Value> = crate::store::rows(&s.store.db,
        "SELECT jsonb_build_object('reportId',r.id,'reason',r.reason,'createdAt',r.created_at, \
           'reporterProfileId',r.reporter_profile_id,'targetProfileId',r.target_profile_id, \
           'targetReportCount',(SELECT count(*) FROM communication_profile_reports x WHERE x.target_profile_id=r.target_profile_id), \
           'review',CASE WHEN v.report_id IS NULL THEN NULL ELSE jsonb_build_object('decision',v.decision,'reason',v.reason,'held',v.held,'reviewedAt',v.reviewed_at) END) doc \
         FROM communication_profile_reports r JOIN communication_profiles t ON t.id=r.target_profile_id \
         LEFT JOIN communication_profile_report_reviews v ON v.report_id=r.id \
         WHERE t.tenant_id=$1 AND t.application_id=$2 AND t.environment_id=$3 \
           AND ($4='ALL' OR ($4='OPEN' AND v.report_id IS NULL) OR v.decision=$4) \
         ORDER BY r.created_at, r.id LIMIT $5",
        values).await?;
    let mut out = Vec::with_capacity(items.len());
    for mut item in items {
        for (field, key) in [
            ("reporterProfileId", "reporterCfMeetId"),
            ("targetProfileId", "targetCfMeetId"),
        ] {
            if let Some(id) = item[field].as_str().and_then(|v| Uuid::parse_str(v).ok()) {
                item[key] = json!(s.store.current_public_id(&s.store.db, id).await?);
            }
            if let Some(map) = item.as_object_mut() {
                map.remove(field);
            }
        }
        out.push(item);
    }
    Ok(Json(json!({"schemaVersion":1,"data":out})))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ReviewReport {
    report_id: Uuid,
    /// DISMISSED or ACTIONED.
    decision: String,
    /// NO_VIOLATION (dismissals), ABUSE, SECURITY, LEGAL or OTHER.
    reason: String,
    /// ACTIONED only: also place an operator hold on the reported profile.
    #[serde(default)]
    hold_target: bool,
}
/// Gap audit S4: records the operator's decision on one report (once; a second review is
/// a version conflict) and, for ACTIONED with holdTarget, applies the operator hold on the
/// reported profile in the same transaction. Audited.
pub async fn review_report(
    HttpState(s): HttpState<State>,
    Extension(ctx): Extension<InternalServiceContext>,
    Json(input): Json<ReviewReport>,
) -> Result<Json<Value>> {
    if !crate::operator_commands::valid_review(&input.decision, &input.reason, input.hold_target) {
        return Err(Error::Validation);
    }
    let scope = auth::delegated(&ctx)?;
    auth::validate_scope(&s, &scope).await?;
    let sc = Scope::of(&scope);
    let mut values = crate::store::scoped(sc);
    values.push(input.report_id.into());
    let report: Value = one(&s.store.db,
        "SELECT jsonb_build_object('target',r.target_profile_id) doc FROM communication_profile_reports r \
         JOIN communication_profiles t ON t.id=r.target_profile_id \
         WHERE t.tenant_id=$1 AND t.application_id=$2 AND t.environment_id=$3 AND r.id=$4",
        values).await?.ok_or(Error::NotFound)?;
    let target = report["target"]
        .as_str()
        .and_then(|v| Uuid::parse_str(v).ok())
        .ok_or(Error::Unavailable)?;
    let txn = s.store.begin().await?;
    let reviewer = ctx.authenticated_service().as_str().to_owned();
    let inserted = crate::store::execute(&txn,
        "INSERT INTO communication_profile_report_reviews(report_id,decision,reason,held,reviewed_by) VALUES($1,$2,$3,$4,$5) ON CONFLICT DO NOTHING",
        vec![input.report_id.into(), input.decision.clone().into(), input.reason.clone().into(), input.hold_target.into(), reviewer.clone().into()]).await?;
    if inserted == 0 {
        return Err(Error::Version);
    }
    // ADR 0068 A7: the reviewed report (version 2) to the admin bridge (when configured).
    crate::admin_events::enqueue_report(&s, &txn, input.report_id, Some(&input.decision), 2)
        .await?;
    if input.hold_target {
        let hold_reason = if input.reason == "SECURITY" || input.reason == "LEGAL" {
            input.reason.clone()
        } else if input.reason == "ABUSE" {
            "ABUSE".into()
        } else {
            "OTHER".into()
        };
        let row:Value=one(&txn,"INSERT INTO communication_operator_holds AS h (profile_id,reason,active) VALUES($1,$2,true) ON CONFLICT(profile_id) DO UPDATE SET reason=EXCLUDED.reason,active=true,version=h.version+1,updated_at=now() RETURNING to_jsonb(h) doc",
            vec![target.into(), hold_reason.into()]).await?.ok_or(Error::Unavailable)?;
        s.store
            .change(
                &txn,
                target,
                "PROFILE",
                Some(target),
                row["version"].as_i64(),
            )
            .await?;
    }
    s.store
        .audit_system(
            &txn,
            sc,
            "PROFILE_REPORT_REVIEWED",
            Some(target),
            json!({"reportId":input.report_id,"decision":input.decision,"reason":input.reason,"held":input.hold_target,"operator":reviewer}),
        )
        .await?;
    txn.commit().await?;
    Ok(Json(
        json!({"schemaVersion":1,"reportId":input.report_id,"decision":input.decision,"held":input.hold_target}),
    ))
}
