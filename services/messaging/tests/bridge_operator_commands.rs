//! The admin bridge's operator COMMANDS (ADR 0068, design §6.3 / §17.6; A4 / A5 / A7):
//! `POST /internal/v1/operator/*` answer only `cfmeet.admin-bridge` holding
//! `messaging-firstparty.administer` with a named HQ operator, an `Idempotency-Key`, the
//! resource's tenant and version, and (R3 / R4) the second person; the change and its
//! `messaging_admin_operations` record commit together; a repeated key answers the first
//! call; `GET /internal/v1/operator/operations/{key}` settles it. No content is returned.

mod common;

use axum::http::{Method, StatusCode};
use common::{flows::*, *};
use serde_json::{json, Value};
use uuid::Uuid;

const OP: (&str, &str) = ("x-cf-operator", "hq:staff-0001");
const APPROVAL: [(&str, &str); 3] = [
    ("x-cf-approver", "hq:staff-0002"),
    ("x-cf-approval-ref", "appr-0001"),
    (
        "x-cf-intent-digest",
        "aa11bb22cc33dd44ee55ff6600778899aa11bb22cc33dd44ee55ff6600778899",
    ),
];

/// A bridge command with an operator, a key and (when `approved`) the second person.
async fn command(h: &Harness, path: &str, body: Value, key: Uuid, approved: bool) -> Response {
    let peers = peers();
    let key = key.to_string();
    let mut headers: Vec<(&str, &str)> = vec![OP, ("idempotency-key", key.as_str())];
    if approved {
        headers.extend(APPROVAL);
    }
    h.call_internal(
        &peers.admin_bridge,
        Method::POST,
        path,
        Some(body),
        None,
        &headers,
    )
    .await
}

async fn read(h: &Harness, path: &str) -> Response {
    let peers = peers();
    h.call_internal(&peers.admin_bridge, Method::GET, path, None, None, &[OP])
        .await
}

async fn record(h: &Harness, key: Uuid) -> Response {
    read(h, &format!("/internal/v1/operator/operations/{key}")).await
}

fn cid(conv: &Value) -> Uuid {
    uuid(&conv["conversationId"])
}

/// A MANAGED group of `n` principals (the first is the owner).
async fn managed_group(h: &Harness, c: &Customer, n: usize) -> (Value, Vec<Uuid>) {
    h.set_policy(
        c,
        json!({ "contexts": { "GROUP": { "allowedProfiles": ["MANAGED", "PRIVATE_E2EE"], "defaultProfile": "MANAGED" } } }),
    )
    .await;
    let people: Vec<Uuid> = (0..n).map(|_| h.principal(c)).collect();
    let r = h
        .create_conversation(
            c,
            people[0],
            json!({ "contextClass": "GROUP", "securityProfile": "MANAGED" }),
        )
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text);
    for p in &people[1..] {
        let a = h
            .post(
                c,
                Some(people[0]),
                &format!("/v1/conversations/{}/members", cid(&r.json)),
                json!({ "principalId": p }),
            )
            .await;
        assert_eq!(a.status, StatusCode::CREATED, "{}", a.text);
    }
    (r.json, people)
}

/// A report by `reporter` of `message` in `conv`.
async fn report(h: &Harness, c: &Customer, conv: &Value, reporter: Uuid, message: Uuid) -> Uuid {
    let r = h
        .post(
            c,
            Some(reporter),
            &format!("/v1/conversations/{}/reports", cid(conv)),
            json!({ "category": "HARASSMENT", "messageIds": [message] }),
        )
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text);
    uuid(&r.json["reportId"])
}

async fn ledger_rows(h: &Harness) -> i64 {
    h.scalar("SELECT count(*) FROM messaging_admin_operations")
        .await
}

#[tokio::test]
async fn removing_a_reported_message_tombstones_it_once_and_settles() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let c = h.customer(ALL_SCOPES);
    let tenant = c.context.scope.tenant_id;
    let (conv, people) = managed_group(&h, &c, 2).await;
    let sent = h
        .send_managed(&c, &conv, people[0], "an abusive message")
        .await;
    let message = uuid(&sent.json["messageId"]);
    let rid = report(&h, &c, &conv, people[1], message).await;
    let detail = read(&h, &format!("/internal/v1/operator/reports/{rid}")).await;
    assert_eq!(detail.json["version"], 1, "{}", detail.text);
    let path = format!("/internal/v1/operator/reports/{rid}/remove-message");
    let body = json!({ "tenantId": tenant, "expectedVersion": 1, "messageId": message, "reasonCode": "HARASSMENT" });
    let key = Uuid::new_v4();

    // No second person: refused, nothing done.
    let r = command(&h, &path, body.clone(), key, false).await;
    assert_eq!(
        (r.status, r.code(), r.reason()),
        (
            StatusCode::FORBIDDEN,
            "INTERNAL_PERMISSION_DENIED",
            "SECOND_PERSON_REQUIRED"
        ),
        "{}",
        r.text
    );
    // No Idempotency-Key.
    let r = h
        .call_internal(
            &peers().admin_bridge,
            Method::POST,
            &path,
            Some(body.clone()),
            None,
            &[OP, APPROVAL[0], APPROVAL[1], APPROVAL[2]],
        )
        .await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST, "{}", r.text);
    // A stale version, another tenant, an unknown field.
    let stale = json!({ "tenantId": tenant, "expectedVersion": 2, "messageId": message, "reasonCode": "HARASSMENT" });
    let r = command(&h, &path, stale, Uuid::new_v4(), true).await;
    assert_eq!(
        (r.status, r.code()),
        (StatusCode::CONFLICT, "VERSION_CONFLICT")
    );
    let foreign = json!({ "tenantId": Uuid::new_v4(), "expectedVersion": 1, "messageId": message, "reasonCode": "HARASSMENT" });
    let r = command(&h, &path, foreign, Uuid::new_v4(), true).await;
    assert_eq!(r.status, StatusCode::NOT_FOUND);
    let mut extra = body.clone();
    extra["note"] = json!("free text is not accepted");
    let r = command(&h, &path, extra, Uuid::new_v4(), true).await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST);
    assert_eq!(ledger_rows(&h).await, 0, "refusals record nothing");
    assert_eq!(record(&h, key).await.status, StatusCode::NOT_FOUND);

    // Applied.
    let r = command(&h, &path, body.clone(), key, true).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    assert_eq!(r.json["status"], "REMOVED");
    assert_eq!(r.json["version"], "2");
    assert_eq!(r.json["resource"]["messageId"], json!(message));
    assert_eq!(r.json["resource"]["reportStatus"], "ACTIONED");
    assert!(
        !r.text.contains("abusive"),
        "the bridge never sees message content"
    );
    let m = h
        .get(
            &c,
            Some(people[1]),
            &format!("/v1/conversations/{}/messages/{message}", cid(&conv)),
        )
        .await;
    assert_eq!(m.json["lifecycle"], "MODERATED", "{}", m.text);

    // A replay answers the first call and applies nothing again.
    let again = command(&h, &path, body.clone(), key, true).await;
    assert_eq!(again.status, StatusCode::OK, "{}", again.text);
    assert_eq!(again.json, r.json);
    assert_eq!(
        h.scalar("SELECT count(*) FROM messaging_moderation_actions")
            .await,
        1
    );
    assert_eq!(ledger_rows(&h).await, 1);
    // The same key for another command is a conflict.
    let other = command(
        &h,
        &format!("/internal/v1/operator/reports/{rid}/dismiss"),
        json!({ "tenantId": tenant, "expectedVersion": 2, "reasonCode": "NO_VIOLATION" }),
        key,
        true,
    )
    .await;
    assert_eq!(
        (other.status, other.reason()),
        (StatusCode::CONFLICT, "IDEMPOTENCY_KEY_REUSED")
    );

    // The settlement record says what the command answered.
    let rec = record(&h, key).await;
    assert_eq!(rec.status, StatusCode::OK, "{}", rec.text);
    assert_eq!(
        rec.json,
        json!({ "idempotency_key": key, "outcome": "APPLIED", "source_status": "REMOVED", "source_version": "2" })
    );
    // Only the bridge with a named operator can read it.
    let r = h
        .call_internal(
            &peers().meeting,
            Method::GET,
            &format!("/internal/v1/operator/operations/{key}"),
            None,
            None,
            &[],
        )
        .await;
    assert_eq!(r.status, StatusCode::NOT_FOUND);
    let row = h
        .scalar(&format!(
            "SELECT count(*) FROM messaging_admin_operations WHERE idempotency_key = '{key}' \
             AND operator_ref = 'hq:staff-0001' AND approver_ref = 'hq:staff-0002' AND approval_ref = 'appr-0001' \
             AND command = 'messaging.message.remove' AND subject_kind = 'REPORT' AND subject_id = '{rid}'"
        ))
        .await;
    assert_eq!(row, 1);
    // A resolved report cannot be dismissed.
    let r = command(
        &h,
        &format!("/internal/v1/operator/reports/{rid}/dismiss"),
        json!({ "tenantId": tenant, "expectedVersion": 2, "reasonCode": "NO_VIOLATION" }),
        Uuid::new_v4(),
        true,
    )
    .await;
    assert_eq!(
        (r.status, r.reason()),
        (StatusCode::CONFLICT, "REPORT_NOT_OPEN")
    );
}

#[tokio::test]
async fn reports_are_dismissed_at_their_version() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let c = h.customer(ALL_SCOPES);
    let tenant = c.context.scope.tenant_id;
    let (conv, people) = managed_group(&h, &c, 2).await;
    let sent = h.send_managed(&c, &conv, people[0], "hello").await;
    let rid = report(&h, &c, &conv, people[1], uuid(&sent.json["messageId"])).await;
    let r = command(
        &h,
        &format!("/internal/v1/operator/reports/{rid}/dismiss"),
        json!({ "tenantId": tenant, "expectedVersion": 1, "reasonCode": "NO_VIOLATION" }),
        Uuid::new_v4(),
        true,
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    assert_eq!(r.json["status"], "DISMISSED");
    assert_eq!(r.json["version"], "2");
    assert_eq!(r.json["resource"]["resolution"], "NO_VIOLATION");
    let detail = read(&h, &format!("/internal/v1/operator/reports/{rid}")).await;
    assert_eq!(detail.json["status"], "DISMISSED");
    assert_eq!(detail.json["version"], 2);
}

#[tokio::test]
async fn members_are_muted_and_removed_and_conversations_restricted() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let c = h.customer(ALL_SCOPES);
    let tenant = c.context.scope.tenant_id;
    let (conv, people) = managed_group(&h, &c, 3).await;
    let id = cid(&conv);
    let member_path = |p: Uuid| format!("/internal/v1/operator/conversations/{id}/members/{p}");
    let m = read(&h, &member_path(people[1])).await;
    assert_eq!(m.status, StatusCode::OK, "{}", m.text);
    let v = m.json["version"].as_i64().unwrap();

    // Mute.
    let mute = format!("{}/mute", member_path(people[1]));
    let key = Uuid::new_v4();
    let body = json!({ "tenantId": tenant, "expectedVersion": v, "reasonCode": "COOLING_OFF", "durationSeconds": 3600 });
    assert_eq!(
        command(&h, &mute, body.clone(), key, false).await.status,
        StatusCode::FORBIDDEN
    );
    let r = command(&h, &mute, body.clone(), key, true).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    assert_eq!(r.json["status"], "MUTED");
    assert_eq!(r.json["version"], (v + 1).to_string());
    assert!(r.json["resource"]["sendRestrictedUntil"].is_string());
    assert_eq!(
        h.send_managed(&c, &conv, people[1], "hi").await.code(),
        "SEND_RESTRICTED"
    );
    assert_eq!(command(&h, &mute, body, key, true).await.json, r.json);
    // A stale version.
    let r = command(
        &h,
        &mute,
        json!({ "tenantId": tenant, "expectedVersion": v, "reasonCode": "COOLING_OFF", "durationSeconds": 3600 }),
        Uuid::new_v4(),
        true,
    )
    .await;
    assert_eq!(r.code(), "VERSION_CONFLICT");
    // A bad duration.
    let r = command(
        &h,
        &mute,
        json!({ "tenantId": tenant, "expectedVersion": v + 1, "reasonCode": "COOLING_OFF", "durationSeconds": 5 }),
        Uuid::new_v4(),
        true,
    )
    .await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST);

    // Remove (never the owner).
    let owner = read(&h, &member_path(people[0])).await;
    let r = command(
        &h,
        &format!("{}/remove", member_path(people[0])),
        json!({ "tenantId": tenant, "expectedVersion": owner.json["version"], "reasonCode": "TERMS_VIOLATION" }),
        Uuid::new_v4(),
        true,
    )
    .await;
    assert_eq!(
        (r.status, r.reason()),
        (StatusCode::CONFLICT, "OWNER_CANNOT_BE_REMOVED")
    );
    let carol = read(&h, &member_path(people[2])).await;
    let r = command(
        &h,
        &format!("{}/remove", member_path(people[2])),
        json!({ "tenantId": tenant, "expectedVersion": carol.json["version"], "reasonCode": "TERMS_VIOLATION" }),
        Uuid::new_v4(),
        true,
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    assert_eq!(r.json["status"], "REMOVED");
    assert_eq!(r.json["resource"]["memberState"], "REMOVED");

    // Restrict the conversation.
    let before = read(&h, &format!("/internal/v1/operator/conversations/{id}")).await;
    assert_eq!(before.status, StatusCode::OK, "{}", before.text);
    assert_eq!(before.json["lifecycleState"], "ACTIVE");
    assert!(before.json.get("title").is_none());
    let version = before.json["version"].as_i64().unwrap();
    let restrict = format!("/internal/v1/operator/conversations/{id}/restrict");
    let r = command(
        &h,
        &restrict,
        json!({ "tenantId": tenant, "expectedVersion": version + 5, "reasonCode": "ABUSE_WAVE" }),
        Uuid::new_v4(),
        true,
    )
    .await;
    assert_eq!(r.code(), "VERSION_CONFLICT");
    let r = command(
        &h,
        &restrict,
        json!({ "tenantId": tenant, "expectedVersion": version, "reasonCode": "ABUSE_WAVE" }),
        Uuid::new_v4(),
        true,
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    assert_eq!(r.json["status"], "LOCKED");
    assert_eq!(r.json["version"], (version + 1).to_string());
    assert_eq!(
        h.send_managed(&c, &conv, people[0], "still open?")
            .await
            .status,
        StatusCode::CONFLICT
    );
    let r = command(
        &h,
        &restrict,
        json!({ "tenantId": tenant, "expectedVersion": version + 1, "reasonCode": "ABUSE_WAVE" }),
        Uuid::new_v4(),
        true,
    )
    .await;
    assert_eq!(r.reason(), "CONVERSATION_NOT_ACTIVE");
}

#[tokio::test]
async fn an_engaged_moderation_switch_is_a_definite_refusal() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let c = h.customer(ALL_SCOPES);
    let tenant = c.context.scope.tenant_id;
    let (conv, people) = managed_group(&h, &c, 2).await;
    let id = cid(&conv);
    let engage = command(
        &h,
        "/internal/v1/operator/switches/MODERATION_ACTIONS",
        json!({ "engaged": true, "expectedVersion": 0, "reasonCode": "INCIDENT_RESPONSE" }),
        Uuid::new_v4(),
        true,
    )
    .await;
    assert_eq!(engage.status, StatusCode::OK, "{}", engage.text);
    let m = read(
        &h,
        &format!(
            "/internal/v1/operator/conversations/{id}/members/{}",
            people[1]
        ),
    )
    .await;
    let r = command(
        &h,
        &format!("/internal/v1/operator/conversations/{id}/members/{}/mute", people[1]),
        json!({ "tenantId": tenant, "expectedVersion": m.json["version"], "reasonCode": "COOLING_OFF", "durationSeconds": 600 }),
        Uuid::new_v4(),
        true,
    )
    .await;
    assert_eq!(
        (r.status, r.code(), r.reason()),
        (StatusCode::CONFLICT, "CONFLICT", "KILL_SWITCH_ENGAGED"),
        "{}",
        r.text
    );
    assert_eq!(ledger_rows(&h).await, 1, "only the switch change");
}

#[tokio::test]
async fn policy_is_read_and_changed_but_never_its_security_posture() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let c = h.customer(ALL_SCOPES);
    let (tenant, env) = (c.context.scope.tenant_id, c.context.scope.environment_id);
    managed_group(&h, &c, 2).await;
    let path = format!("/internal/v1/operator/policy?tenantId={tenant}&environmentId={env}");
    let p = read(&h, &path).await;
    assert_eq!(p.status, StatusCode::OK, "{}", p.text);
    let version = p.json["version"].as_i64().unwrap();
    assert!(version >= 1);
    assert!(p.json["effective"]["attachments"]["enabled"].is_boolean());
    // Unknown tenant / environment: 404; missing parameters: 400.
    let r = read(
        &h,
        &format!(
            "/internal/v1/operator/policy?tenantId={}&environmentId={env}",
            Uuid::new_v4()
        ),
    )
    .await;
    assert_eq!(r.status, StatusCode::NOT_FOUND);
    let r = read(
        &h,
        &format!("/internal/v1/operator/policy?tenantId={tenant}"),
    )
    .await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST);

    let mut doc = p.json["policy"].clone();
    doc["attachments"] = json!({ "enabled": false });
    let change = |doc: Value, v: i64| json!({ "tenantId": tenant, "environmentId": env, "expectedVersion": v, "reasonCode": "ATTACHMENT_ABUSE", "policy": doc });
    let r = command(
        &h,
        "/internal/v1/operator/policy",
        change(doc.clone(), version + 1),
        Uuid::new_v4(),
        true,
    )
    .await;
    assert_eq!(r.code(), "POLICY_VERSION_CONFLICT");
    let r = command(
        &h,
        "/internal/v1/operator/policy",
        change(doc.clone(), version),
        Uuid::new_v4(),
        true,
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    assert_eq!(r.json["status"], "UPDATED");
    assert_eq!(r.json["version"], (version + 1).to_string());
    let after = read(&h, &path).await;
    assert_eq!(after.json["effective"]["attachments"]["enabled"], false);
    // The security posture stays the application's decision.
    let mut weaker = after.json["policy"].clone();
    weaker["contexts"]["GROUP"]["allowedProfiles"] = json!(["MANAGED"]);
    let r = command(
        &h,
        "/internal/v1/operator/policy",
        change(weaker, version + 1),
        Uuid::new_v4(),
        true,
    )
    .await;
    assert_eq!(
        (r.status, r.reason()),
        (
            StatusCode::UNPROCESSABLE_ENTITY,
            "SECURITY_POSTURE_OWNER_ONLY"
        ),
        "{}",
        r.text
    );
}

#[tokio::test]
async fn holds_are_placed_and_released_without_the_case_reference() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let c = h.customer(ALL_SCOPES);
    let (tenant, env) = (c.context.scope.tenant_id, c.context.scope.environment_id);
    let (conv, _) = managed_group(&h, &c, 2).await;
    let key = Uuid::new_v4();
    let place = json!({ "tenantId": tenant, "environmentId": env, "scopeKind": "CONVERSATION",
        "conversationId": cid(&conv), "reasonCategory": "LITIGATION", "caseReference": "CASE-SECRET-7" });
    let r = command(&h, "/internal/v1/operator/holds", place.clone(), key, true).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    assert_eq!(r.json["status"], "ACTIVE");
    assert_eq!(r.json["version"], "1");
    assert_eq!(r.json["resource"]["holdId"], json!(key));
    assert!(!r.text.contains("CASE-SECRET"));
    assert_eq!(
        command(&h, "/internal/v1/operator/holds", place, key, true)
            .await
            .json,
        r.json
    );
    assert_eq!(
        h.scalar("SELECT count(*) FROM messaging_legal_holds").await,
        1
    );
    // Another tenant's conversation is not found.
    let r = command(
        &h,
        "/internal/v1/operator/holds",
        json!({ "tenantId": Uuid::new_v4(), "environmentId": env, "scopeKind": "CONVERSATION",
            "conversationId": cid(&conv), "reasonCategory": "LITIGATION" }),
        Uuid::new_v4(),
        true,
    )
    .await;
    assert_eq!(r.status, StatusCode::NOT_FOUND);

    let release = format!("/internal/v1/operator/holds/{key}/release");
    let r = command(
        &h,
        &release,
        json!({ "tenantId": tenant, "expectedVersion": 2, "reasonCode": "CASE_CLOSED" }),
        Uuid::new_v4(),
        true,
    )
    .await;
    assert_eq!(r.code(), "VERSION_CONFLICT");
    let r = command(
        &h,
        &release,
        json!({ "tenantId": tenant, "expectedVersion": 1, "reasonCode": "CASE_CLOSED" }),
        Uuid::new_v4(),
        true,
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    assert_eq!(r.json["status"], "RELEASED");
    assert_eq!(r.json["version"], "2");
    let holds = read(&h, "/internal/v1/operator/holds?status=RELEASED").await;
    assert_eq!(holds.json["items"][0]["version"], 2);
    let r = command(
        &h,
        &release,
        json!({ "tenantId": tenant, "expectedVersion": 2, "reasonCode": "CASE_CLOSED" }),
        Uuid::new_v4(),
        true,
    )
    .await;
    assert_eq!(r.reason(), "HOLD_NOT_ACTIVE");
}

#[tokio::test]
async fn deletion_requests_are_registered_only_with_two_people() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let c = h.customer(ALL_SCOPES);
    let (tenant, env) = (c.context.scope.tenant_id, c.context.scope.environment_id);
    let (conv, people) = managed_group(&h, &c, 2).await;
    let body = json!({ "tenantId": tenant, "environmentId": env, "kind": "PRINCIPAL_ERASURE",
        "principalId": people[1], "reasonCode": "PRIVACY_REQUEST" });
    let key = Uuid::new_v4();
    let r = command(
        &h,
        "/internal/v1/operator/deletion-requests",
        body.clone(),
        key,
        false,
    )
    .await;
    assert_eq!(r.reason(), "SECOND_PERSON_REQUIRED");
    let r = command(
        &h,
        "/internal/v1/operator/deletion-requests",
        body,
        key,
        true,
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    assert_eq!(r.json["status"], "PENDING");
    assert!(r.json["version"].is_null());
    assert_eq!(r.json["resource"]["requestId"], json!(key));
    let rec = record(&h, key).await;
    assert_eq!(rec.json["source_status"], "PENDING");
    assert!(rec.json["source_version"].is_null());
    // A purge of a conversation outside the named environment is not found.
    let r = command(
        &h,
        "/internal/v1/operator/deletion-requests",
        json!({ "tenantId": tenant, "environmentId": Uuid::new_v4(), "kind": "CONVERSATION_PURGE",
            "conversationId": cid(&conv), "reasonCode": "PRIVACY_REQUEST" }),
        Uuid::new_v4(),
        true,
    )
    .await;
    assert_eq!(r.status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn switches_incidents_and_failed_messages_are_cli_operations_as_commands() {
    let Some(h) = Harness::new().await else {
        return;
    };
    // Switch: engage at version 0 (never set), release at its version.
    let path = "/internal/v1/operator/switches/ATTACHMENT_UPLOAD";
    let r = command(
        &h,
        path,
        json!({ "engaged": true, "expectedVersion": 0, "reasonCode": "STORAGE_INCIDENT" }),
        Uuid::new_v4(),
        false,
    )
    .await;
    assert_eq!(r.reason(), "SECOND_PERSON_REQUIRED");
    let r = command(
        &h,
        path,
        json!({ "engaged": true, "expectedVersion": 0, "reasonCode": "STORAGE_INCIDENT" }),
        Uuid::new_v4(),
        true,
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    assert_eq!(
        (r.json["status"].as_str(), r.json["version"].as_str()),
        (Some("ENGAGED"), Some("1"))
    );
    let r = command(
        &h,
        path,
        json!({ "engaged": true, "expectedVersion": 1, "reasonCode": "STORAGE_INCIDENT" }),
        Uuid::new_v4(),
        true,
    )
    .await;
    assert_eq!(r.reason(), "SWITCH_ALREADY_IN_STATE");
    let r = command(
        &h,
        path,
        json!({ "engaged": false, "expectedVersion": 0, "reasonCode": "RECOVERED" }),
        Uuid::new_v4(),
        true,
    )
    .await;
    assert_eq!(r.code(), "VERSION_CONFLICT");
    let r = command(
        &h,
        path,
        json!({ "engaged": false, "expectedVersion": 1, "reasonCode": "RECOVERED" }),
        Uuid::new_v4(),
        true,
    )
    .await;
    assert_eq!(r.json["status"], "RELEASED", "{}", r.text);
    assert_eq!(
        command(
            &h,
            "/internal/v1/operator/switches/NO_SUCH_SWITCH",
            json!({ "engaged": true, "expectedVersion": 0, "reasonCode": "X_Y" }),
            Uuid::new_v4(),
            true
        )
        .await
        .status,
        StatusCode::BAD_REQUEST
    );
    // A switch the key-transparency monitor engaged stays with the server's operators.
    h.sql(
        "INSERT INTO messaging_kill_switches (switch_name, scope_key, engaged, reason, changed_by, changed_at) \
         VALUES ('KT_PROOFS', 'GLOBAL', true, 'KT_CHECKPOINT_UNVERIFIABLE', 'messaging-kt-monitor', now())",
    )
    .await
    .unwrap();
    let r = command(
        &h,
        "/internal/v1/operator/switches/KT_PROOFS",
        json!({ "engaged": false, "expectedVersion": 1, "reasonCode": "RECOVERED" }),
        Uuid::new_v4(),
        true,
    )
    .await;
    assert_eq!(r.reason(), "KT_INTEGRITY_CLI_ONLY");

    // Incident resolution (R2: no second person).
    let incident = Uuid::new_v4();
    h.sql(&format!(
        "INSERT INTO messaging_incidents (id, category, severity, dedupe_key, summary, first_seen_at, last_seen_at) \
         VALUES ('{incident}', 'OUTBOX_DEAD_LETTER', 'MEDIUM', 'OUTBOX_DEAD_LETTER:x', 'summary-never-relayed', now(), now())"
    ))
    .await
    .unwrap();
    let resolve = format!("/internal/v1/operator/incidents/{incident}/resolve");
    let key = Uuid::new_v4();
    let r = command(
        &h,
        &resolve,
        json!({ "reasonCode": "REPLAYED" }),
        key,
        false,
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    assert_eq!(r.json["status"], "RESOLVED");
    assert!(r.json["version"].is_null());
    assert!(!r.text.contains("summary-never-relayed"));
    let r = command(
        &h,
        &resolve,
        json!({ "reasonCode": "REPLAYED" }),
        Uuid::new_v4(),
        false,
    )
    .await;
    assert_eq!(r.reason(), "INCIDENT_NOT_OPEN");
    let kt = Uuid::new_v4();
    h.sql(&format!(
        "INSERT INTO messaging_incidents (id, category, severity, dedupe_key, summary, first_seen_at, last_seen_at) \
         VALUES ('{kt}', 'KT_ROLLBACK_DETECTED', 'CRITICAL', 'KT_ROLLBACK_DETECTED:x', 's', now(), now())"
    ))
    .await
    .unwrap();
    let r = command(
        &h,
        &format!("/internal/v1/operator/incidents/{kt}/resolve"),
        json!({ "reasonCode": "RESTORED" }),
        Uuid::new_v4(),
        false,
    )
    .await;
    assert_eq!(r.reason(), "KT_INTEGRITY_CLI_ONLY");

    // Failed-message retry.
    let event = Uuid::new_v4();
    h.sql(&format!(
        "INSERT INTO messaging_outbox_events (id, event_type, aggregate_type, aggregate_id, tenant_id, application_id, \
         environment_id, payload, correlation_id, occurred_at, next_attempt_at, status, attempts) \
         VALUES ('{event}', 'messaging.policy.updated', 'MESSAGING_POLICY', '{event}', '{event}', '{event}', '{event}', \
         '{{}}'::jsonb, 'corr_12345678', now(), now(), 'DEAD_LETTER', 9)"
    ))
    .await
    .unwrap();
    let retry = format!("/internal/v1/operator/failed-messages/{event}/retry");
    let r = command(
        &h,
        &retry,
        json!({ "reasonCode": "PEER_RECOVERED" }),
        Uuid::new_v4(),
        false,
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    assert_eq!(r.json["status"], "RETRIED");
    assert_eq!(r.json["resource"]["queueState"], "PENDING");
    let r = command(
        &h,
        &retry,
        json!({ "reasonCode": "PEER_RECOVERED" }),
        Uuid::new_v4(),
        false,
    )
    .await;
    assert_eq!(r.reason(), "NOT_DEAD_LETTERED");

    // Key-transparency checkpoint without a configured checkpoint file: refused.
    let r = command(
        &h,
        "/internal/v1/operator/key-transparency/checkpoint",
        json!({ "reasonCode": "QUARTERLY_AUDIT" }),
        Uuid::new_v4(),
        true,
    )
    .await;
    assert_eq!(
        (r.status, r.reason()),
        (StatusCode::CONFLICT, "KT_CHECKPOINT_NOT_CONFIGURED")
    );
}

#[tokio::test]
async fn a_key_transparency_checkpoint_advances_the_operator_checkpoint() {
    let dir = std::env::temp_dir().join(format!("msg-kt-{}", Uuid::new_v4().simple()));
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("checkpoint.json");
    let f2 = file.clone();
    let Some(h) = Harness::build(move |s| s.kt_checkpoint = Some(f2)).await else {
        return;
    };
    let r = command(
        &h,
        "/internal/v1/operator/key-transparency/checkpoint",
        json!({ "reasonCode": "QUARTERLY_AUDIT" }),
        Uuid::new_v4(),
        false,
    )
    .await;
    assert_eq!(r.reason(), "SECOND_PERSON_REQUIRED");
    let r = command(
        &h,
        "/internal/v1/operator/key-transparency/checkpoint",
        json!({ "reasonCode": "QUARTERLY_AUDIT" }),
        Uuid::new_v4(),
        true,
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    assert_eq!(r.json["status"], "CHECKPOINTED");
    assert!(file.exists());
    std::fs::remove_dir_all(&dir).ok();
}

#[tokio::test]
async fn commands_answer_only_the_bridge_with_a_named_operator() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let peers = peers();
    let (c, r, p) = (Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4());
    let routes = [
        format!("/internal/v1/operator/reports/{r}/remove-message"),
        format!("/internal/v1/operator/reports/{r}/dismiss"),
        format!("/internal/v1/operator/conversations/{c}/members/{p}/mute"),
        format!("/internal/v1/operator/conversations/{c}/members/{p}/remove"),
        format!("/internal/v1/operator/conversations/{c}/restrict"),
        "/internal/v1/operator/policy".to_string(),
        "/internal/v1/operator/holds".to_string(),
        format!("/internal/v1/operator/holds/{p}/release"),
        "/internal/v1/operator/deletion-requests".to_string(),
        "/internal/v1/operator/switches/MESSAGE_SEND".to_string(),
        format!("/internal/v1/operator/incidents/{p}/resolve"),
        format!("/internal/v1/operator/failed-messages/{p}/retry"),
        "/internal/v1/operator/key-transparency/checkpoint".to_string(),
    ];
    let key = Uuid::new_v4().to_string();
    for route in &routes {
        // The Meeting Service (the console's relay) does not hold the permission.
        let x = h
            .call_internal(
                &peers.meeting,
                Method::POST,
                route,
                Some(json!({})),
                None,
                &[("idempotency-key", key.as_str())],
            )
            .await;
        assert_eq!(x.code(), "INTERNAL_PERMISSION_DENIED", "{route}");
        // The bridge without X-CF-Operator.
        let x = h
            .call_internal(
                &peers.admin_bridge,
                Method::POST,
                route,
                Some(json!({})),
                None,
                &[("idempotency-key", key.as_str())],
            )
            .await;
        assert_eq!(x.code(), "INTERNAL_PERMISSION_DENIED", "{route}");
    }
    for route in [
        format!("/internal/v1/operator/conversations/{c}"),
        format!("/internal/v1/operator/conversations/{c}/members/{p}"),
        format!("/internal/v1/operator/policy?tenantId={c}&environmentId={p}"),
    ] {
        let x = h
            .call_internal(&peers.meeting, Method::GET, &route, None, None, &[])
            .await;
        assert_eq!(x.code(), "INTERNAL_PERMISSION_DENIED", "{route}");
        let x = read(&h, &route).await;
        assert_eq!(x.status, StatusCode::NOT_FOUND, "{route}");
    }
    assert_eq!(ledger_rows(&h).await, 0);
}
