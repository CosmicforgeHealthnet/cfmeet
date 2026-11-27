//! ADR 0068 A4: the admin bridge's operator commands on the Communications Hub, through the
//! real router (CI: real PostgreSQL).
//!
//! * `POST /internal/v1/communications/operator/profiles/{cfMeetId}/suspend` (R3)
//! * `POST /internal/v1/communications/operator/profiles/{cfMeetId}/restore` (R3)
//! * `POST /internal/v1/communications/operator/reports/{reportId}/review` (R3)
//! * `POST /internal/v1/communications/operator/backfill` (R2)
//! * `POST /internal/v1/communications/operator/replay` (R2)
//! * `GET /internal/v1/operator/operations/{idempotencyKey}` (the settlement record)
//!
//! Each command needs `communications.administer` (admin bridge only), `X-CF-Operator`,
//! an `Idempotency-Key`, and for R3 the second person; versioned subjects change only at
//! `expectedVersion`; every applied command is in `communication_admin_operations`.
mod common;
use axum::http::{HeaderMap, HeaderValue, StatusCode};
use cf_service_auth::{OutboundServiceRequest, ServiceId, ServiceTokenSigner};
use common::*;
use communications::store::{execute, one};
use serde_json::{json, Value};
use tower::ServiceExt;
use uuid::Uuid;

const BACKFILL: &str = "/internal/v1/communications/operator/backfill";
const REPLAY: &str = "/internal/v1/communications/operator/replay";
const DIGEST: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";

fn suspend_path(cf: &str) -> String {
    format!("/internal/v1/communications/operator/profiles/{cf}/suspend")
}
fn restore_path(cf: &str) -> String {
    format!("/internal/v1/communications/operator/profiles/{cf}/restore")
}
fn review_path(report: Uuid) -> String {
    format!("/internal/v1/communications/operator/reports/{report}/review")
}
fn record_path(key: &str) -> String {
    format!("/internal/v1/operator/operations/{key}")
}

/// How a request is attributed.
#[derive(Clone, Copy)]
enum By {
    /// The bridge with operator and approver.
    Approved,
    /// The bridge with the operator alone.
    Operator,
    /// The bridge without attribution.
    BridgeBare,
    /// The operator console (no communications.administer).
    Console,
}

fn headers(h: &Harness, by: By, key: Option<&str>) -> HeaderMap {
    let signer: &ServiceTokenSigner = match by {
        By::Console => &h.operator,
        _ => &h.bridge,
    };
    let mut headers = signer
        .sign(&OutboundServiceRequest::new_operation(
            ServiceId::communications(),
        ))
        .unwrap()
        .headers();
    let mut put = |k: &'static str, v: &str| {
        headers.insert(k, HeaderValue::from_str(v).unwrap());
    };
    if matches!(by, By::Approved | By::Operator) {
        put("x-cf-operator", "hq:staff-0001");
    }
    if matches!(by, By::Approved) {
        put("x-cf-approver", "hq:staff-0002");
        put("x-cf-approval-ref", "appr-1");
        put("x-cf-intent-digest", DIGEST);
    }
    if let Some(key) = key {
        put("idempotency-key", key);
    }
    headers
}

async fn post(
    h: &Harness,
    path: &str,
    by: By,
    key: Option<&str>,
    body: Value,
) -> (StatusCode, Value) {
    h.http_headers(path, body, headers(h, by, key)).await
}

async fn get(h: &Harness, path: &str, by: By) -> (StatusCode, Value) {
    let mut r = axum::http::Request::builder()
        .method("GET")
        .uri(path)
        .body(axum::body::Body::empty())
        .unwrap();
    r.headers_mut().extend(headers(h, by, None));
    let response = communications::api::app(h.state.clone())
        .oneshot(r)
        .await
        .unwrap();
    let status = response.status();
    let bytes = axum::body::to_bytes(response.into_body(), 65536)
        .await
        .unwrap();
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    )
}

fn key() -> String {
    Uuid::new_v4().to_string()
}

async fn scalar(h: &Harness, sql: &str, values: Vec<sea_orm::Value>) -> Value {
    let v: Value = one(&h.state.store.db, sql, values).await.unwrap().unwrap();
    v["v"].clone()
}

async fn profile_version(h: &Harness, id: Uuid) -> i64 {
    scalar(
        h,
        "SELECT jsonb_build_object('v',version) doc FROM communication_profiles WHERE id=$1",
        vec![id.into()],
    )
    .await
    .as_i64()
    .unwrap()
}

async fn records(h: &Harness) -> i64 {
    scalar(
        h,
        "SELECT jsonb_build_object('v',count(*)) doc FROM communication_admin_operations",
        vec![],
    )
    .await
    .as_i64()
    .unwrap()
}

fn assert_no_personal_data(v: &Value) {
    let text = v.to_string();
    for forbidden in [
        "displayName",
        "avatar",
        "about",
        "accountReference",
        "principalId",
        "phone",
        "email",
        "Same chosen name",
    ] {
        assert!(!text.contains(forbidden), "{forbidden} in {text}");
    }
}

#[tokio::test]
async fn the_bridge_suspends_and_restores_a_profile_at_its_version() {
    let h = Harness::new().await;
    let p = h.profile(&h.account()).await;
    let cf = h.public_id(&p).await;
    let v1 = profile_version(&h, p.id).await;

    let k = key();
    let (status, body) = post(
        &h,
        &suspend_path(&cf),
        By::Approved,
        Some(&k),
        json!({"expectedVersion": v1, "reason": "ABUSE"}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["status"], "SUSPENDED");
    assert_eq!(body["version"], json!((v1 + 1).to_string()));
    assert_eq!(body["resource"]["cfMeetId"], json!(cf));
    assert_eq!(body["resource"]["held"], true);
    assert_eq!(body["resource"]["hold"]["reason"], "ABUSE");
    assert_eq!(body.as_object().unwrap().len(), 3, "{body}");
    assert_no_personal_data(&body);
    assert_eq!(profile_version(&h, p.id).await, v1 + 1);

    // The same key again: the current profile, nothing applied twice.
    let (status, again) = post(
        &h,
        &suspend_path(&cf),
        By::Approved,
        Some(&k),
        json!({"expectedVersion": v1, "reason": "ABUSE"}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{again}");
    assert_eq!(again["status"], "SUSPENDED");
    assert_eq!(again["version"], body["version"]);
    assert_eq!(profile_version(&h, p.id).await, v1 + 1);
    assert_eq!(records(&h).await, 1);

    // The settlement record answers what the command answered.
    let (status, rec) = get(&h, &record_path(&k), By::Operator).await;
    assert_eq!(status, StatusCode::OK, "{rec}");
    assert_eq!(
        rec,
        json!({"idempotency_key": k, "outcome": "APPLIED", "source_status": "SUSPENDED", "source_version": (v1 + 1).to_string()})
    );

    // A stale version: nothing done.
    let (status, stale) = post(
        &h,
        &restore_path(&cf),
        By::Approved,
        Some(&key()),
        json!({"expectedVersion": v1}),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "{stale}");
    assert_eq!(stale["error"]["code"], "VERSION_CONFLICT");

    // The same key for another command: a conflict.
    let (status, reused) = post(
        &h,
        &restore_path(&cf),
        By::Approved,
        Some(&k),
        json!({"expectedVersion": v1 + 1}),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "{reused}");
    assert_eq!(reused["error"]["code"], "IDEMPOTENCY_CONFLICT");

    // Restore at the current version.
    let k2 = key();
    let (status, restored) = post(
        &h,
        &restore_path(&cf),
        By::Approved,
        Some(&k2),
        json!({"expectedVersion": v1 + 1}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{restored}");
    assert_eq!(restored["status"], "RESTORED");
    assert_eq!(restored["version"], json!((v1 + 2).to_string()));
    assert_eq!(restored["resource"]["held"], false);
    let (_, rec) = get(&h, &record_path(&k2), By::Operator).await;
    assert_eq!(rec["source_status"], "RESTORED");

    // Nothing left to restore: refused, nothing done.
    let (status, _) = post(
        &h,
        &restore_path(&cf),
        By::Approved,
        Some(&key()),
        json!({"expectedVersion": v1 + 2}),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(profile_version(&h, p.id).await, v1 + 2);
    assert_eq!(records(&h).await, 2);

    // The person is recorded, in the ledger and in the Hub's audit.
    let row = scalar(
        &h,
        "SELECT jsonb_build_object('v',jsonb_build_object('op',operator_ref,'ap',approver_ref,'ref',approval_ref,'cmd',command,'kind',subject_kind,'id',subject_id)) doc \
         FROM communication_admin_operations WHERE idempotency_key=$1",
        vec![Uuid::parse_str(&k).unwrap().into()],
    )
    .await;
    assert_eq!(
        row,
        json!({"op":"hq:staff-0001","ap":"hq:staff-0002","ref":"appr-1","cmd":"PROFILE_SUSPEND","kind":"PROFILE","id":p.id})
    );
    let audit = scalar(
        &h,
        "SELECT jsonb_build_object('v',details) doc FROM communication_audit WHERE operation='OPERATOR_HOLD_APPLIED' AND resource_id=$1",
        vec![p.id.into()],
    )
    .await;
    assert_eq!(audit["operator"], "hq:staff-0001");
    assert_eq!(audit["approver"], "hq:staff-0002");
    assert_eq!(audit["reason"], "ABUSE");
    h.close().await;
}

#[tokio::test]
async fn profile_commands_refuse_before_doing_anything() {
    let h = Harness::new().await;
    let p = h.profile(&h.account()).await;
    let cf = h.public_id(&p).await;
    let v = profile_version(&h, p.id).await;
    let body = json!({"expectedVersion": v, "reason": "ABUSE"});
    for (path, by, k, body, expected) in [
        // No second person (R3).
        (
            suspend_path(&cf),
            By::Operator,
            Some(key()),
            body.clone(),
            StatusCode::FORBIDDEN,
        ),
        (
            restore_path(&cf),
            By::Operator,
            Some(key()),
            json!({"expectedVersion": v}),
            StatusCode::FORBIDDEN,
        ),
        // No Idempotency-Key, or not a UUID.
        (
            suspend_path(&cf),
            By::Approved,
            None,
            body.clone(),
            StatusCode::BAD_REQUEST,
        ),
        (
            suspend_path(&cf),
            By::Approved,
            Some("k-1".into()),
            body.clone(),
            StatusCode::BAD_REQUEST,
        ),
        // The bridge without attribution; the operator console.
        (
            suspend_path(&cf),
            By::BridgeBare,
            Some(key()),
            body.clone(),
            StatusCode::FORBIDDEN,
        ),
        (
            suspend_path(&cf),
            By::Console,
            Some(key()),
            body.clone(),
            StatusCode::FORBIDDEN,
        ),
        // Strict body.
        (
            suspend_path(&cf),
            By::Approved,
            Some(key()),
            json!({"expectedVersion": v, "reason": "ABUSE", "note": "x"}),
            StatusCode::BAD_REQUEST,
        ),
        (
            suspend_path(&cf),
            By::Approved,
            Some(key()),
            json!({"expectedVersion": 0, "reason": "ABUSE"}),
            StatusCode::BAD_REQUEST,
        ),
        (
            suspend_path(&cf),
            By::Approved,
            Some(key()),
            json!({"reason": "ABUSE"}),
            StatusCode::BAD_REQUEST,
        ),
        (
            suspend_path(&cf),
            By::Approved,
            Some(key()),
            json!({"expectedVersion": v, "reason": "BORED"}),
            StatusCode::BAD_REQUEST,
        ),
        // Unknown profile.
        (
            suspend_path("CF-ZZZZZZ"),
            By::Approved,
            Some(key()),
            body.clone(),
            StatusCode::NOT_FOUND,
        ),
    ] {
        let (status, answer) = post(&h, &path, by, k.as_deref(), body).await;
        assert_eq!(status, expected, "{path}: {answer}");
    }
    assert_eq!(profile_version(&h, p.id).await, v, "nothing changed");
    assert_eq!(records(&h).await, 0);
    h.close().await;
}

/// Two profiles and a report from the first about the second.
async fn reported(h: &Harness) -> (Uuid, communications::model::Profile) {
    let a = h.profile(&h.account()).await;
    let b = h.profile(&h.account()).await;
    let report = Uuid::new_v4();
    execute(
        &h.state.store.db,
        "INSERT INTO communication_profile_reports(id,reporter_profile_id,target_profile_id,reason) VALUES($1,$2,$3,'HARASSMENT')",
        vec![report.into(), a.id.into(), b.id.into()],
    )
    .await
    .unwrap();
    (report, b)
}

#[tokio::test]
async fn the_bridge_reviews_a_report_once_and_may_hold_the_target() {
    let h = Harness::new().await;
    let (report, target) = reported(&h).await;
    let target_version = profile_version(&h, target.id).await;

    // The report read carries the version the review sends back.
    let (status, page) = get(
        &h,
        "/internal/v1/communications/operator/reports",
        By::Operator,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{page}");
    assert_eq!(page["items"][0]["version"], 1);

    let review = json!({"expectedVersion": 1, "decision": "ACTIONED", "reason": "ABUSE", "holdTarget": true});
    // Refusals first: nothing done.
    let (status, _) = post(
        &h,
        &review_path(report),
        By::Operator,
        Some(&key()),
        review.clone(),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "R3 needs the second person");
    let (status, _) = post(&h, &review_path(report), By::Approved, None, review.clone()).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    let (status, _) = post(
        &h,
        &review_path(report),
        By::Console,
        Some(&key()),
        review.clone(),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    let (status, _) = post(
        &h,
        &review_path(report),
        By::Approved,
        Some(&key()),
        json!({"expectedVersion": 1, "decision": "DISMISSED", "reason": "NO_VIOLATION", "holdTarget": true}),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    let (status, _) = post(
        &h,
        &review_path(Uuid::new_v4()),
        By::Approved,
        Some(&key()),
        review.clone(),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let (status, _) = post(
        &h,
        &review_path(report),
        By::Approved,
        Some(&key()),
        json!({"expectedVersion": 2, "decision": "ACTIONED", "reason": "ABUSE"}),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "stale version");
    assert_eq!(records(&h).await, 0);

    let k = key();
    let (status, body) = post(
        &h,
        &review_path(report),
        By::Approved,
        Some(&k),
        review.clone(),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["status"], "ACTIONED");
    assert_eq!(body["version"], "2");
    assert_eq!(body["resource"]["reportId"], json!(report));
    assert_eq!(body["resource"]["held"], true);
    assert_eq!(
        body["resource"]["targetCfMeetId"],
        json!(h.public_id(&target).await)
    );
    assert_no_personal_data(&body);
    // The target is held and its version moved; the reviewer is the HQ staff member.
    let held = scalar(
        &h,
        "SELECT jsonb_build_object('v',active) doc FROM communication_operator_holds WHERE profile_id=$1",
        vec![target.id.into()],
    )
    .await;
    assert_eq!(held, true);
    assert_eq!(profile_version(&h, target.id).await, target_version + 1);
    let reviewer = scalar(
        &h,
        "SELECT jsonb_build_object('v',reviewed_by) doc FROM communication_profile_report_reviews WHERE report_id=$1",
        vec![report.into()],
    )
    .await;
    assert_eq!(reviewer, "hq:staff-0001");

    // Replay: the same answer, applied once.
    let (status, again) = post(
        &h,
        &review_path(report),
        By::Approved,
        Some(&k),
        review.clone(),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{again}");
    assert_eq!(again["status"], "ACTIONED");
    assert_eq!(profile_version(&h, target.id).await, target_version + 1);
    // A second review under a new key: reviewed already.
    for v in [1, 2] {
        let (status, body) = post(
            &h,
            &review_path(report),
            By::Approved,
            Some(&key()),
            json!({"expectedVersion": v, "decision": "DISMISSED", "reason": "NO_VIOLATION"}),
        )
        .await;
        assert_eq!(status, StatusCode::CONFLICT, "{body}");
    }
    let (status, rec) = get(&h, &record_path(&k), By::Operator).await;
    assert_eq!(status, StatusCode::OK, "{rec}");
    assert_eq!(rec["source_status"], "ACTIONED");
    assert_eq!(rec["source_version"], "2");
    assert_eq!(records(&h).await, 1);
    h.close().await;
}

#[tokio::test]
async fn the_bridge_runs_a_backfill_page_once_per_key() {
    let h = Harness::new().await;
    // A profile (the Hub then knows the environment's authority) and two accounts without.
    let existing = h.account();
    h.profile(&existing).await;
    h.account();
    h.account();

    // The console has no communications.administer; the bridge needs its key.
    let (status, _) = post(
        &h,
        BACKFILL,
        By::Console,
        Some(&key()),
        json!({"limit": 10}),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    let (status, _) = post(&h, BACKFILL, By::Operator, None, json!({"limit": 10})).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    for bad in [
        json!({"limit": 0}),
        json!({"limit": 101}),
        json!({"limit": 5, "tenantId": Uuid::new_v4()}),
        json!({}),
    ] {
        let (status, body) = post(&h, BACKFILL, By::Operator, Some(&key()), bad.clone()).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{bad}: {body}");
    }
    let (status, _) = post(
        &h,
        BACKFILL,
        By::Operator,
        Some(&key()),
        json!({"limit": 10, "environmentId": Uuid::new_v4()}),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND, "unknown environment");
    assert_eq!(records(&h).await, 0);

    // R2: no second person needed.
    let k = key();
    let (status, body) = post(&h, BACKFILL, By::Operator, Some(&k), json!({"limit": 100})).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["status"], "COMPLETED");
    assert!(body["version"].is_null());
    assert_eq!(body["resource"]["created"], 2);
    assert_eq!(
        body["resource"]["environmentId"],
        json!(h.scope.application_environment_id())
    );
    assert!(body["resource"]["nextCursor"].is_null());
    assert_no_personal_data(&body);

    // Replay: the stored answer; the Account is not asked again.
    h.account();
    let (status, again) = post(&h, BACKFILL, By::Operator, Some(&k), json!({"limit": 100})).await;
    assert_eq!(status, StatusCode::OK, "{again}");
    assert_eq!(again, body);
    let (status, rec) = get(&h, &record_path(&k), By::Operator).await;
    assert_eq!(status, StatusCode::OK, "{rec}");
    assert_eq!(rec["source_status"], "COMPLETED");
    assert!(rec["source_version"].is_null());
    // The bridge is recorded as the person on every created profile's audit.
    let audited = scalar(
        &h,
        "SELECT jsonb_build_object('v',count(*)) doc FROM communication_audit WHERE operation='PROFILE_BACKFILLED' AND details->>'operator'='hq:staff-0001'",
        vec![],
    )
    .await;
    assert_eq!(audited, 2);
    h.close().await;
}

#[tokio::test]
async fn the_bridge_requeues_a_dead_event() {
    let h = Harness::new().await;
    let event = Uuid::new_v4();
    let scope = &h.scope;
    execute(
        &h.state.store.db,
        "INSERT INTO communication_outbox(event_id,tenant_id,application_id,environment_id,destination,aggregate_id,aggregate_version,event_type,payload,state,attempts) \
         VALUES($1,$2,$3,$4,'CALLING',$5,1,'communications.test','{}','DEAD',12)",
        vec![
            event.into(),
            scope.tenant_id().into(),
            scope.application_id().into(),
            scope.application_environment_id().into(),
            Uuid::new_v4().into(),
        ],
    )
    .await
    .unwrap();

    let (status, _) = post(
        &h,
        REPLAY,
        By::Console,
        Some(&key()),
        json!({"eventId": event}),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    let (status, _) = post(&h, REPLAY, By::Operator, None, json!({"eventId": event})).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    let (status, _) = post(
        &h,
        REPLAY,
        By::Operator,
        Some(&key()),
        json!({"eventId": Uuid::new_v4()}),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    let k = key();
    let (status, body) = post(
        &h,
        REPLAY,
        By::Operator,
        Some(&k),
        json!({"eventId": event}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(
        body,
        json!({"status": "REQUEUED", "version": null, "resource": {"eventId": event, "requeued": 1}})
    );
    let state = scalar(
        &h,
        "SELECT jsonb_build_object('v',state) doc FROM communication_outbox WHERE event_id=$1",
        vec![event.into()],
    )
    .await;
    assert_eq!(state, "PENDING");
    // Replay: the stored answer. A new key: nothing DEAD any more, nothing done.
    let (status, again) = post(
        &h,
        REPLAY,
        By::Operator,
        Some(&k),
        json!({"eventId": event}),
    )
    .await;
    assert_eq!((status, &again), (StatusCode::OK, &body));
    let (status, conflict) = post(
        &h,
        REPLAY,
        By::Operator,
        Some(&key()),
        json!({"eventId": event}),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "{conflict}");
    let (_, rec) = get(&h, &record_path(&k), By::Operator).await;
    assert_eq!(rec["source_status"], "REQUEUED");
    assert_eq!(records(&h).await, 1);
    h.close().await;
}

#[tokio::test]
async fn the_operation_record_exists_only_for_the_bridge() {
    let h = Harness::new().await;
    let (status, _) = get(&h, &record_path(&key()), By::Operator).await;
    assert_eq!(status, StatusCode::NOT_FOUND, "unknown key");
    let (status, _) = get(&h, &record_path("not-a-key"), By::Operator).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let (status, _) = get(&h, &record_path(&key()), By::Console).await;
    assert_eq!(status, StatusCode::NOT_FOUND, "the console");
    let (status, _) = get(&h, &record_path(&key()), By::BridgeBare).await;
    assert_eq!(status, StatusCode::FORBIDDEN, "no operator named");
    h.close().await;
}

#[tokio::test]
async fn the_ledger_is_append_only_and_enforces_the_second_person() {
    let h = Harness::new().await;
    let db = &h.state.store.db;
    let insert = "INSERT INTO communication_admin_operations(idempotency_key,command,subject_kind,subject_id,operator_ref,approver_ref,approval_ref,resulting_state,resulting_version) \
                  VALUES($1,$2,$3,$4,'hq:staff-0001',$5,$6,'SUSPENDED',2)";
    let row = |command: &str, approver: Option<&str>| -> Vec<sea_orm::Value> {
        vec![
            Uuid::new_v4().into(),
            command.into(),
            "PROFILE".into(),
            Uuid::new_v4().into(),
            approver.map(str::to_owned).into(),
            approver.map(|_| "appr-1".to_owned()).into(),
        ]
    };
    // R3 without an approver, or approved by the operator: refused by the database.
    assert!(execute(db, insert, row("PROFILE_SUSPEND", None))
        .await
        .is_err());
    assert!(
        execute(db, insert, row("PROFILE_SUSPEND", Some("hq:staff-0001")))
            .await
            .is_err()
    );
    assert_eq!(
        execute(db, insert, row("PROFILE_SUSPEND", Some("hq:staff-0002")))
            .await
            .unwrap(),
        1
    );
    assert!(execute(
        db,
        "UPDATE communication_admin_operations SET resulting_state='RESTORED'",
        vec![]
    )
    .await
    .is_err());
    assert!(
        execute(db, "DELETE FROM communication_admin_operations", vec![])
            .await
            .is_err()
    );
    h.close().await;
}
