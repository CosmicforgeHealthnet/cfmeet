//! Part G — search, reports, moderation, retention / purge, legal holds, exports, data
//! deletion, regulated contexts and the administrative audit — through the real router
//! and PostgreSQL, with real OpenMLS client devices for PRIVATE_E2EE conversations.

mod common;

use std::sync::Arc;

use axum::http::StatusCode;
use cf_object_storage::MemoryObjectStore;
use common::{flows::*, *};
use serde_json::{json, Value};
use uuid::Uuid;

fn cid(conv: &Value) -> String {
    conv["conversationId"].as_str().unwrap().to_string()
}

async fn harness() -> Option<(Harness, Arc<MemoryObjectStore>)> {
    let store = Arc::new(MemoryObjectStore::new());
    let s2 = store.clone();
    let h = Harness::build(move |s| s.storage = s2).await?;
    Some((h, store))
}

/// GROUP may be MANAGED (default) or PRIVATE_E2EE; `group` / `top` extend the policy.
async fn policy(h: &Harness, c: &Customer, group: Value, top: Value) {
    let mut g =
        json!({ "allowedProfiles": ["MANAGED", "PRIVATE_E2EE"], "defaultProfile": "MANAGED" });
    g.as_object_mut()
        .unwrap()
        .extend(group.as_object().unwrap().clone());
    let mut p = json!({ "contexts": { "GROUP": g } });
    p.as_object_mut()
        .unwrap()
        .extend(top.as_object().unwrap().clone());
    h.set_policy(c, p).await;
}

async fn managed_group(h: &Harness, c: &Customer, n: usize) -> (Value, Vec<Uuid>) {
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

async fn message(h: &Harness, c: &Customer, conv: &Value, who: Uuid, id: &str) -> Value {
    let r = h
        .get(
            c,
            Some(who),
            &format!("/v1/conversations/{}/messages/{id}", cid(conv)),
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    r.json["message"].clone()
}

#[tokio::test]
async fn managed_search_is_membership_aware_and_e2ee_is_local_only() {
    let Some((h, _)) = harness().await else {
        return;
    };
    let c = h.customer(ALL_SCOPES);
    policy(&h, &c, json!({}), json!({})).await;
    let (conv, people) = managed_group(&h, &c, 2).await;
    let (alice, bob) = (people[0], people[1]);
    let id = cid(&conv);
    let first = h
        .send_managed(&c, &conv, alice, "the quarterly budget draft is ready")
        .await;
    h.send_managed(&c, &conv, bob, "lunch at noon?").await;
    let gone = h
        .send_managed(&c, &conv, alice, "budget numbers v2 (wrong)")
        .await;

    let r = h
        .get(
            &c,
            Some(bob),
            &format!("/v1/conversations/{id}/search?q=budget"),
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    conforms(
        "messaging/common/v1.schema.json#/$defs/SearchResults",
        &r.json,
    );
    assert_eq!(r.json["results"].as_array().unwrap().len(), 2);
    assert_eq!(
        r.json["results"][0]["managed"]["text"], "budget numbers v2 (wrong)",
        "newest first"
    );

    // A deleted message leaves the index at once; "delete for me" hides it for that member.
    let mid = gone.json["messageId"].as_str().unwrap();
    assert!(h
        .delete(
            &c,
            Some(alice),
            &format!("/v1/conversations/{id}/messages/{mid}")
        )
        .await
        .status
        .is_success());
    let fid = first.json["messageId"].as_str().unwrap();
    assert!(h
        .delete(
            &c,
            Some(bob),
            &format!("/v1/conversations/{id}/messages/{fid}?scope=ME")
        )
        .await
        .status
        .is_success());
    let r = h
        .get(
            &c,
            Some(bob),
            &format!("/v1/conversations/{id}/search?q=budget"),
        )
        .await;
    assert_eq!(r.json["results"].as_array().unwrap().len(), 0, "{}", r.text);
    let r = h
        .get(
            &c,
            Some(alice),
            &format!("/v1/conversations/{id}/search?q=budget"),
        )
        .await;
    assert_eq!(r.json["results"].as_array().unwrap().len(), 1, "{}", r.text);

    // A new member sees nothing from before their history window.
    let carol = h.principal(&c);
    h.post(
        &c,
        Some(alice),
        &format!("/v1/conversations/{id}/members"),
        json!({ "principalId": carol }),
    )
    .await;
    let r = h
        .get(
            &c,
            Some(carol),
            &format!("/v1/conversations/{id}/search?q=budget"),
        )
        .await;
    assert_eq!(r.json["results"].as_array().unwrap().len(), 0, "{}", r.text);
    h.send_managed(&c, &conv, alice, "new budget thread").await;
    let r = h
        .get(&c, Some(carol), "/v1/messaging/search?q=budget")
        .await;
    assert_eq!(r.json["results"].as_array().unwrap().len(), 1, "{}", r.text);

    // Non-members, other tenants: nothing (no existence oracle).
    let outsider = h.principal(&c);
    assert_eq!(
        h.get(
            &c,
            Some(outsider),
            &format!("/v1/conversations/{id}/search?q=budget")
        )
        .await
        .status,
        StatusCode::NOT_FOUND
    );
    let r = h
        .get(&c, Some(outsider), "/v1/messaging/search?q=budget")
        .await;
    assert_eq!(r.json["results"].as_array().unwrap().len(), 0);
    let other = h.customer(ALL_SCOPES);
    let r = h
        .get(
            &other,
            Some(h.principal(&other)),
            &format!("/v1/conversations/{id}/search?q=budget"),
        )
        .await;
    assert_eq!(r.status, StatusCode::NOT_FOUND);
    // Bounds.
    for q in ["", "%00", &"x".repeat(201)] {
        let r = h
            .get(
                &c,
                Some(alice),
                &format!("/v1/conversations/{id}/search?q={q}"),
            )
            .await;
        assert_eq!(r.status, StatusCode::BAD_REQUEST, "{q:?}: {}", r.text);
    }

    // PRIVATE_E2EE: the server has no index and refuses — the device searches locally.
    let (e2ee, a, _b) = h.e2ee_direct(&c).await;
    let r = h
        .get(
            &c,
            Some(a.principal),
            &format!("/v1/conversations/{}/search?q=secret", cid(&e2ee)),
        )
        .await;
    assert_eq!(r.code(), "LOCAL_SEARCH_ONLY", "{}", r.text);
    let r = h
        .get(&c, Some(a.principal), "/v1/messaging/search?q=secret")
        .await;
    assert_eq!(r.json["localSearchOnlyConversations"], 1, "{}", r.text);
    assert_eq!(r.json["results"].as_array().unwrap().len(), 0);
}

#[tokio::test]
async fn e2ee_reports_carry_only_voluntary_disclosure_and_moderation_tombstones() {
    let Some((h, _)) = harness().await else {
        return;
    };
    let c = h.customer(ALL_SCOPES);
    let (conv, mut alice, mut bob) = h.e2ee_direct(&c).await;
    let id = cid(&conv);
    let sent = h
        .send_e2ee(
            &c,
            &conv,
            &mut alice,
            "you will regret this, unique-threat-7731",
        )
        .await;
    assert_eq!(sent.status, StatusCode::CREATED, "{}", sent.text);
    let mid = sent.json["messageId"].clone();
    let got = h.receive(&c, &conv, &mut bob).await;
    let decrypted = got.last().unwrap().1.clone();

    // Bob reports, voluntarily disclosing what HIS device decrypted.
    let r = h
        .post(&c, Some(bob.principal), &format!("/v1/conversations/{id}/reports"),
            json!({ "category": "HARASSMENT", "messageIds": [mid], "reportedPrincipalId": alice.principal,
                    "note": "repeated threats", "disclosure": [{ "messageId": mid, "text": decrypted }] }))
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text);
    conforms(
        "messaging/common/v1.schema.json#/$defs/ReportReceipt",
        &r.json,
    );
    assert_eq!(r.json["evidenceKind"], "REPORTER_DISCLOSURE");
    assert!(r.json.get("reporterPrincipalId").is_none());
    let rid = r.json["reportId"].as_str().unwrap().to_string();
    // Disclosed text and note are sealed at rest.
    let dump = h.dump().await;
    assert!(
        !dump.contains("unique-threat-7731") && !dump.contains("repeated threats"),
        "evidence stored in clear"
    );
    // Validation: self-report, unknown principal, disclosure for a message not reported.
    let r = h
        .post(
            &c,
            Some(bob.principal),
            &format!("/v1/conversations/{id}/reports"),
            json!({ "category": "SPAM", "reportedPrincipalId": bob.principal }),
        )
        .await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST);
    let r = h
        .post(
            &c,
            Some(bob.principal),
            &format!("/v1/conversations/{id}/reports"),
            json!({ "category": "SPAM", "reportedPrincipalId": Uuid::new_v4() }),
        )
        .await;
    assert_eq!(r.status, StatusCode::NOT_FOUND);
    let r = h
        .post(
            &c,
            Some(bob.principal),
            &format!("/v1/conversations/{id}/reports"),
            json!({ "category": "SPAM", "disclosure": [{ "messageId": mid, "text": "x" }] }),
        )
        .await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST);
    // A non-member cannot report.
    let outsider = h.principal(&c);
    let r = h
        .post(
            &c,
            Some(outsider),
            &format!("/v1/conversations/{id}/reports"),
            json!({ "category": "SPAM" }),
        )
        .await;
    assert_eq!(r.status, StatusCode::NOT_FOUND);

    // Moderators (administration only, moderation.manage).
    let r = h
        .get(&c, Some(bob.principal), "/v1/messaging/reports")
        .await;
    assert_eq!(r.status, StatusCode::FORBIDDEN, "{}", r.text);
    let weak = h.credential(&c, &["messages.read", "conversations.manage"]);
    assert_eq!(
        h.get(&weak, None, "/v1/messaging/reports").await.status,
        StatusCode::FORBIDDEN
    );
    let queue = h.get(&c, None, "/v1/messaging/reports?status=OPEN").await;
    assert_eq!(
        queue.json["reports"].as_array().unwrap().len(),
        1,
        "{}",
        queue.text
    );
    assert!(
        queue.json["reports"][0].get("messages").is_none(),
        "no evidence in the queue"
    );
    let detail = h
        .get(&c, None, &format!("/v1/messaging/reports/{rid}"))
        .await;
    assert_eq!(detail.status, StatusCode::OK, "{}", detail.text);
    conforms(
        "messaging/common/v1.schema.json#/$defs/ReportDetail",
        &detail.json,
    );
    let m = &detail.json["messages"][0];
    assert_eq!(m["e2ee"]["reporterDisclosure"], json!(decrypted));
    assert_eq!(m["e2ee"]["verification"], "REPORTER_ATTESTED_UNVERIFIABLE");
    assert_eq!(
        m["e2ee"]["ciphertextSha256"],
        sent.json["e2ee"]["ciphertextSha256"]
    );
    assert_eq!(detail.json["note"], "repeated threats");
    assert_eq!(detail.json["decryptionCapability"], "NONE");
    // Another tenant sees nothing.
    let other = h.customer(ALL_SCOPES);
    assert_eq!(
        h.get(&other, None, &format!("/v1/messaging/reports/{rid}"))
            .await
            .status,
        StatusCode::NOT_FOUND
    );

    // Remove the message (tombstone; the moderator never read the ciphertext).
    let act = h
        .post(&c, None, "/v1/messaging/moderation/actions",
            json!({ "kind": "REMOVE_MESSAGE", "conversationId": conv["conversationId"], "messageId": mid, "reportId": rid, "reason": "THREAT" }))
        .await;
    assert_eq!(act.status, StatusCode::CREATED, "{}", act.text);
    conforms(
        "messaging/common/v1.schema.json#/$defs/ModerationAction",
        &act.json,
    );
    let m = message(&h, &c, &conv, bob.principal, mid.as_str().unwrap()).await;
    assert_eq!(m["lifecycle"], "MODERATED");
    assert_eq!(m["e2ee"]["ciphertext"], Value::Null);
    let rep = h
        .get(&c, None, &format!("/v1/messaging/reports/{rid}"))
        .await;
    assert_eq!(rep.json["status"], "ACTIONED");
    assert_eq!(rep.json["resolution"], "REMOVE_MESSAGE");
    // A resolved report cannot be actioned twice; actions are append-only.
    let again = h
        .post(
            &c,
            None,
            "/v1/messaging/moderation/actions",
            json!({ "kind": "DISMISS_REPORT", "reportId": rid, "reason": "DUPLICATE" }),
        )
        .await;
    assert_eq!(again.reason(), "REPORT_NOT_OPEN", "{}", again.text);
    assert!(h
        .sql("UPDATE messaging_moderation_actions SET reason = 'X_X'")
        .await
        .is_err());
    assert!(h
        .sql("DELETE FROM messaging_moderation_actions")
        .await
        .is_err());
    // The purge (grace 0) removes the stored ciphertext.
    messaging::compliance::run_once(&h.state).await.unwrap();
    assert_eq!(
        h.scalar(&format!("SELECT count(*) FROM messaging_messages WHERE id = '{}' AND lifecycle = 'PURGED' AND mls_ciphertext IS NULL", mid.as_str().unwrap())).await,
        1
    );
    let list = h.get(&c, None, "/v1/messaging/moderation/actions").await;
    assert_eq!(list.json["actions"].as_array().unwrap().len(), 1);
}

#[tokio::test]
async fn moderation_mutes_restricts_and_respects_policy() {
    let Some((h, _)) = harness().await else {
        return;
    };
    let c = h.customer(ALL_SCOPES);
    policy(&h, &c, json!({}), json!({})).await;
    let (conv, people) = managed_group(&h, &c, 3).await;
    let (alice, bob, carol) = (people[0], people[1], people[2]);
    let id = cid(&conv);
    let mute = h
        .post(&c, None, "/v1/messaging/moderation/actions",
            json!({ "kind": "MUTE_MEMBER", "conversationId": id, "principalId": bob, "reason": "COOLING_OFF", "durationSeconds": 3600 }))
        .await;
    assert_eq!(mute.status, StatusCode::CREATED, "{}", mute.text);
    assert!(mute.json["effectiveUntil"].is_string());
    assert_eq!(
        h.send_managed(&c, &conv, bob, "hi").await.code(),
        "SEND_RESTRICTED"
    );
    let r = h.post(&c, None, "/v1/messaging/moderation/actions", json!({ "kind": "UNMUTE_MEMBER", "conversationId": id, "principalId": bob, "reason": "APPEAL_GRANTED" })).await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text);
    assert_eq!(
        h.send_managed(&c, &conv, bob, "hi again").await.status,
        StatusCode::CREATED
    );
    // An environment-wide restriction, then lifted.
    let r = h.post(&c, None, "/v1/messaging/moderation/actions", json!({ "kind": "RESTRICT_PRINCIPAL", "principalId": carol, "reason": "SPAM_WAVE", "durationSeconds": 600 })).await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text);
    assert_eq!(
        h.send_managed(&c, &conv, carol, "buy now").await.code(),
        "SEND_RESTRICTED"
    );
    let r = h
        .post(
            &c,
            None,
            "/v1/messaging/moderation/actions",
            json!({ "kind": "LIFT_RESTRICTION", "principalId": carol, "reason": "REVIEWED" }),
        )
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text);
    assert_eq!(
        h.send_managed(&c, &conv, carol, "sorry").await.status,
        StatusCode::CREATED
    );
    // Remove a member.
    let r = h.post(&c, None, "/v1/messaging/moderation/actions", json!({ "kind": "REMOVE_MEMBER", "conversationId": id, "principalId": carol, "reason": "TERMS_VIOLATION" })).await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text);
    assert_eq!(
        h.send_managed(&c, &conv, carol, "still here?").await.status,
        StatusCode::NOT_FOUND
    );
    // Validation and authority.
    for bad in [
        json!({ "kind": "MUTE_MEMBER", "conversationId": id, "principalId": bob, "reason": "X_Y" }),
        json!({ "kind": "MUTE_MEMBER", "conversationId": id, "principalId": bob, "reason": "lower", "durationSeconds": 60 }),
        json!({ "kind": "REMOVE_MESSAGE", "conversationId": id, "reason": "NO_TARGET" }),
        json!({ "kind": "EXPLODE", "reason": "NOPE" }),
    ] {
        let r = h
            .post(&c, None, "/v1/messaging/moderation/actions", bad.clone())
            .await;
        assert_eq!(r.status, StatusCode::BAD_REQUEST, "{bad}: {}", r.text);
    }
    let r = h.post(&c, Some(alice), "/v1/messaging/moderation/actions", json!({ "kind": "RESTRICT_PRINCIPAL", "principalId": bob, "reason": "X_Y", "durationSeconds": 60 })).await;
    assert_eq!(r.status, StatusCode::FORBIDDEN, "{}", r.text);

    // MANAGED reports reference messages; content is shown to moderators only by policy.
    let sent = h
        .send_managed(&c, &conv, alice, "managed spam text 4411")
        .await;
    let r = h
        .post(
            &c,
            Some(bob),
            &format!("/v1/conversations/{id}/reports"),
            json!({ "category": "SPAM", "messageIds": [sent.json["messageId"]] }),
        )
        .await;
    assert_eq!(r.json["evidenceKind"], "MANAGED_REFERENCE", "{}", r.text);
    let rid = r.json["reportId"].as_str().unwrap().to_string();
    let d = h
        .get(&c, None, &format!("/v1/messaging/reports/{rid}"))
        .await;
    assert_eq!(d.json["messages"][0]["managed"]["text"], Value::Null);
    assert_eq!(
        d.json["messages"][0]["managed"]["visibility"],
        "POLICY_WITHHOLDS_CONTENT"
    );
    policy(
        &h,
        &c,
        json!({}),
        json!({ "moderation": { "managedEvidenceAllowed": true } }),
    )
    .await;
    let d = h
        .get(&c, None, &format!("/v1/messaging/reports/{rid}"))
        .await;
    assert_eq!(
        d.json["messages"][0]["managed"]["text"],
        "managed spam text 4411"
    );
    let r = h
        .post(
            &c,
            None,
            "/v1/messaging/moderation/actions",
            json!({ "kind": "DISMISS_REPORT", "reportId": rid, "reason": "NOT_SPAM" }),
        )
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text);
    // Reporting can be disabled per context.
    policy(&h, &c, json!({ "reportingEnabled": false }), json!({})).await;
    let r = h
        .post(
            &c,
            Some(bob),
            &format!("/v1/conversations/{id}/reports"),
            json!({ "category": "SPAM" }),
        )
        .await;
    assert_eq!(
        (r.code(), r.reason()),
        ("FEATURE_DISABLED", "REPORTING_DISABLED"),
        "{}",
        r.text
    );
    // Moderation can be disabled.
    policy(
        &h,
        &c,
        json!({}),
        json!({ "moderation": { "enabled": false } }),
    )
    .await;
    let r = h.post(&c, None, "/v1/messaging/moderation/actions", json!({ "kind": "RESTRICT_PRINCIPAL", "principalId": bob, "reason": "X_Y", "durationSeconds": 60 })).await;
    assert_eq!(r.reason(), "MODERATION_DISABLED");
}

#[tokio::test]
async fn retention_expires_and_purges_but_legal_holds_preserve() {
    let Some((h, _)) = harness().await else {
        return;
    };
    let c = h.customer(ALL_SCOPES);
    policy(
        &h,
        &c,
        json!({}),
        json!({ "retention": { "messageDays": 30 } }),
    )
    .await;
    let (conv, people) = managed_group(&h, &c, 2).await;
    let (held_conv, held_people) = managed_group(&h, &c, 2).await;
    let old = h.send_managed(&c, &conv, people[0], "old message").await;
    let old_held = h
        .send_managed(&c, &held_conv, held_people[0], "old but held")
        .await;
    let hold = h
        .post(&c, None, "/v1/messaging/legal-holds", json!({ "scopeKind": "CONVERSATION", "conversationId": held_conv["conversationId"], "reasonCategory": "REGULATORY" }))
        .await;
    assert_eq!(hold.status, StatusCode::CREATED, "{}", hold.text);
    h.clock.advance(chrono::Duration::days(31));
    let fresh = h.send_managed(&c, &conv, people[0], "fresh message").await;
    let r = messaging::compliance::run_once(&h.state).await.unwrap();
    assert_eq!((r.expired, r.purged, r.held), (2, 1, 1), "{r:?}");
    let m = message(
        &h,
        &c,
        &conv,
        people[1],
        old.json["messageId"].as_str().unwrap(),
    )
    .await;
    assert_eq!(m["lifecycle"], "PURGED");
    let m = message(
        &h,
        &c,
        &conv,
        people[1],
        fresh.json["messageId"].as_str().unwrap(),
    )
    .await;
    assert_eq!(m["lifecycle"], "ACTIVE");
    // Held: hidden from members (EXPIRED) but the sealed text is preserved.
    let hid = old_held.json["messageId"].as_str().unwrap();
    let m = message(&h, &c, &held_conv, held_people[1], hid).await;
    assert_eq!(m["lifecycle"], "EXPIRED");
    assert_eq!(h.scalar(&format!("SELECT count(*) FROM messaging_messages WHERE id = '{hid}' AND managed_sealed IS NOT NULL")).await, 1);
    // A purge is irreversible (database trigger).
    let pid = old.json["messageId"].as_str().unwrap();
    assert!(h
        .sql(&format!(
            "UPDATE messaging_messages SET lifecycle = 'ACTIVE' WHERE id = '{pid}'"
        ))
        .await
        .is_err());
    // Release → purged on a later pass.
    let rel = h
        .post(
            &c,
            None,
            &format!(
                "/v1/messaging/legal-holds/{}/release",
                hold.json["holdId"].as_str().unwrap()
            ),
            json!({ "reason": "AUDIT_DONE" }),
        )
        .await;
    assert_eq!(rel.status, StatusCode::OK, "{}", rel.text);
    h.clock.advance(chrono::Duration::hours(2));
    let r = messaging::compliance::run_once(&h.state).await.unwrap();
    assert_eq!(r.purged, 1, "{r:?}");
    assert_eq!(
        h.scalar(&format!(
            "SELECT count(*) FROM messaging_messages WHERE id = '{hid}' AND lifecycle = 'PURGED'"
        ))
        .await,
        1
    );
}

#[tokio::test]
async fn exports_are_managed_only_authorized_and_expire() {
    let Some((h, store)) = harness().await else {
        return;
    };
    let c = h.customer(ALL_SCOPES);
    policy(&h, &c, json!({ "exportsEnabled": true }), json!({})).await;
    let (conv, people) = managed_group(&h, &c, 2).await;
    h.send_managed(&c, &conv, people[0], "export me 1").await;
    let deleted = h
        .send_managed(&c, &conv, people[1], "deleted before export")
        .await;
    h.delete(
        &c,
        Some(people[1]),
        &format!(
            "/v1/conversations/{}/messages/{}",
            cid(&conv),
            deleted.json["messageId"].as_str().unwrap()
        ),
    )
    .await;

    // E2EE conversations are never exported by the server.
    let (e2ee, _a, _b) = h.e2ee_direct(&c).await;
    let r = h
        .post(
            &c,
            None,
            "/v1/messaging/exports",
            json!({ "conversationId": e2ee["conversationId"] }),
        )
        .await;
    assert_eq!(r.code(), "E2EE_EXPORT_NOT_AVAILABLE", "{}", r.text);
    // Scope and administration.
    let weak = h.credential(&c, &["conversations.manage"]);
    assert_eq!(
        h.post(
            &weak,
            None,
            "/v1/messaging/exports",
            json!({ "conversationId": conv["conversationId"] })
        )
        .await
        .status,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        h.post(
            &c,
            Some(people[0]),
            "/v1/messaging/exports",
            json!({ "conversationId": conv["conversationId"] })
        )
        .await
        .status,
        StatusCode::FORBIDDEN
    );

    let r = h
        .post(
            &c,
            None,
            "/v1/messaging/exports",
            json!({ "conversationId": conv["conversationId"] }),
        )
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text);
    conforms("messaging/common/v1.schema.json#/$defs/Export", &r.json);
    assert_eq!(r.json["status"], "PENDING");
    let eid = r.json["exportId"].as_str().unwrap().to_string();
    let g = h
        .post(
            &c,
            None,
            &format!("/v1/messaging/exports/{eid}/download-grants"),
            json!({}),
        )
        .await;
    assert_eq!(g.reason(), "EXPORT_NOT_READY");
    messaging::compliance::run_once(&h.state).await.unwrap();
    let s = h
        .get(&c, None, &format!("/v1/messaging/exports/{eid}"))
        .await;
    assert_eq!(s.json["status"], "READY", "{}", s.text);
    assert_eq!(s.json["messageCount"], 2);
    let g = h
        .post(
            &c,
            None,
            &format!("/v1/messaging/exports/{eid}/download-grants"),
            json!({}),
        )
        .await;
    assert_eq!(g.status, StatusCode::OK, "{}", g.text);
    assert!(g.json["url"]
        .as_str()
        .unwrap()
        .contains("response-content-disposition=attachment"));
    let key = format!(
        "exp/{}/{}",
        c.context.scope.environment_id.simple(),
        Uuid::parse_str(&eid).unwrap().simple()
    );
    let doc: Value = serde_json::from_slice(&store.get_raw(&key).unwrap()).unwrap();
    assert_eq!(doc["format"], "CFMEET_MESSAGING_MANAGED_EXPORT_V1");
    assert_eq!(doc["messages"][0]["text"], "export me 1");
    assert_ne!(
        doc["messages"][1]["lifecycle"], "ACTIVE",
        "tombstone (or purged) only"
    );
    assert_eq!(
        doc["messages"][1]["text"],
        Value::Null,
        "deleted content is not exported"
    );
    // Another tenant cannot see it; audited.
    let other = h.customer(ALL_SCOPES);
    assert_eq!(
        h.get(&other, None, &format!("/v1/messaging/exports/{eid}"))
            .await
            .status,
        StatusCode::NOT_FOUND
    );
    let audit = h
        .get(&c, None, "/v1/messaging/audit?targetKind=EXPORT")
        .await;
    let actions: Vec<&str> = audit.json["entries"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["action"].as_str().unwrap())
        .collect();
    assert!(
        actions.contains(&"EXPORT_REQUESTED") && actions.contains(&"EXPORT_DOWNLOAD_GRANTED"),
        "{actions:?}"
    );
    // Expiry deletes the object.
    h.clock.advance(chrono::Duration::days(8));
    messaging::compliance::run_once(&h.state).await.unwrap();
    messaging::attachments::lifecycle::run_once(&h.state)
        .await
        .unwrap();
    assert!(store.get_raw(&key).is_none());
    assert_eq!(
        h.get(&c, None, &format!("/v1/messaging/exports/{eid}"))
            .await
            .json["status"],
        "EXPIRED"
    );
    // Exports disabled for the context.
    policy(&h, &c, json!({}), json!({})).await;
    let r = h
        .post(
            &c,
            None,
            "/v1/messaging/exports",
            json!({ "conversationId": conv["conversationId"] }),
        )
        .await;
    assert_eq!(r.reason(), "EXPORTS_DISABLED", "{}", r.text);
}

#[tokio::test]
async fn data_deletion_requests_respect_holds() {
    let Some((h, _)) = harness().await else {
        return;
    };
    let c = h.customer(ALL_SCOPES);
    policy(&h, &c, json!({}), json!({})).await;
    let (conv, people) = managed_group(&h, &c, 2).await;
    let (alice, bob) = (people[0], people[1]);
    let a1 = h.send_managed(&c, &conv, alice, "alice one").await;
    h.send_managed(&c, &conv, alice, "alice two").await;
    let b1 = h.send_managed(&c, &conv, bob, "bob one").await;
    let conv2 = h
        .create_conversation(
            &c,
            alice,
            json!({ "contextClass": "GROUP", "securityProfile": "MANAGED" }),
        )
        .await
        .json;
    let held_msg = h
        .send_managed(&c, &conv2, alice, "alice in a held conversation")
        .await;
    h.post(&c, None, "/v1/messaging/legal-holds", json!({ "scopeKind": "CONVERSATION", "conversationId": conv2["conversationId"], "reasonCategory": "LITIGATION" })).await;

    let r = h
        .post(
            &c,
            None,
            "/v1/messaging/deletion-requests",
            json!({ "kind": "PRINCIPAL_ERASURE", "principalId": alice }),
        )
        .await;
    assert_eq!(r.status, StatusCode::ACCEPTED, "{}", r.text);
    conforms(
        "messaging/common/v1.schema.json#/$defs/DeletionRequest",
        &r.json,
    );
    let rid = r.json["requestId"].as_str().unwrap().to_string();
    messaging::compliance::run_once(&h.state).await.unwrap();
    let s = h
        .get(&c, None, &format!("/v1/messaging/deletion-requests/{rid}"))
        .await;
    assert_eq!(s.json["status"], "PARTIALLY_HELD", "{}", s.text);
    assert_eq!(
        (
            s.json["messagesPurged"].as_i64(),
            s.json["heldItems"].as_i64()
        ),
        (Some(2), Some(1))
    );
    assert_eq!(
        message(&h, &c, &conv, bob, a1.json["messageId"].as_str().unwrap()).await["lifecycle"],
        "PURGED"
    );
    assert_eq!(
        message(&h, &c, &conv, bob, b1.json["messageId"].as_str().unwrap()).await["lifecycle"],
        "ACTIVE"
    );
    let hid = held_msg.json["messageId"].as_str().unwrap();
    assert_eq!(h.scalar(&format!("SELECT count(*) FROM messaging_messages WHERE id = '{hid}' AND lifecycle = 'DELETED' AND managed_sealed IS NOT NULL")).await, 1);
    // Conversation purge.
    let r = h
        .post(
            &c,
            None,
            "/v1/messaging/deletion-requests",
            json!({ "kind": "CONVERSATION_PURGE", "conversationId": conv["conversationId"] }),
        )
        .await;
    let rid = r.json["requestId"].as_str().unwrap().to_string();
    messaging::compliance::run_once(&h.state).await.unwrap();
    let s = h
        .get(&c, None, &format!("/v1/messaging/deletion-requests/{rid}"))
        .await;
    assert_eq!(s.json["status"], "COMPLETED", "{}", s.text);
    assert_eq!(h.scalar(&format!("SELECT count(*) FROM messaging_messages WHERE conversation_id = '{}' AND lifecycle <> 'PURGED'", cid(&conv))).await, 0);
    // Validation and authority.
    let r = h
        .post(
            &c,
            None,
            "/v1/messaging/deletion-requests",
            json!({ "kind": "PRINCIPAL_ERASURE" }),
        )
        .await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST);
    let r = h
        .post(
            &c,
            Some(alice),
            "/v1/messaging/deletion-requests",
            json!({ "kind": "PRINCIPAL_ERASURE", "principalId": alice }),
        )
        .await;
    assert_eq!(r.status, StatusCode::FORBIDDEN);
    let other = h.customer(ALL_SCOPES);
    let r = h
        .post(
            &other,
            None,
            "/v1/messaging/deletion-requests",
            json!({ "kind": "CONVERSATION_PURGE", "conversationId": conv["conversationId"] }),
        )
        .await;
    assert_eq!(r.status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn regulated_contexts_require_disclosure_and_audit_is_queryable() {
    let Some((h, _)) = harness().await else {
        return;
    };
    let c = h.customer(ALL_SCOPES);
    policy(&h, &c, json!({ "regulated": true }), json!({})).await;
    let p = h.principal(&c);
    let r = h
        .create_conversation(
            &c,
            p,
            json!({ "contextClass": "GROUP", "securityProfile": "MANAGED" }),
        )
        .await;
    assert_eq!(
        (r.code(), r.reason()),
        (
            "SECURITY_PROFILE_NOT_ALLOWED",
            "MANAGED_DISCLOSURE_REQUIRED"
        ),
        "{}",
        r.text
    );
    let r = h
        .create_conversation(&c, p, json!({ "contextClass": "GROUP", "securityProfile": "MANAGED", "managedDisclosureAcknowledged": true }))
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text);
    assert_eq!(r.json["encryption"]["serverCanReadContent"], true);
    // Central archive: only MANAGED may be configured.
    let v = h.get(&c, None, "/v1/messaging/policy").await;
    let bad = h
        .put(
            &c,
            None,
            "/v1/messaging/policy",
            json!({ "expectedVersion": v.json["version"], "policy": { "contexts": { "GROUP": {
            "allowedProfiles": ["MANAGED", "PRIVATE_E2EE"], "centralArchiveRequired": true } } } }),
        )
        .await;
    assert_eq!(bad.status, StatusCode::BAD_REQUEST, "{}", bad.text);

    // The audit API: tenant-scoped, filtered, paged; administration only.
    let hold = h
        .post(
            &c,
            None,
            "/v1/messaging/legal-holds",
            json!({ "scopeKind": "ENVIRONMENT", "reasonCategory": "INTERNAL_POLICY" }),
        )
        .await;
    assert_eq!(hold.status, StatusCode::CREATED);
    let a = h
        .get(&c, None, "/v1/messaging/audit?action=LEGAL_HOLD_CREATED")
        .await;
    assert_eq!(a.status, StatusCode::OK, "{}", a.text);
    conforms("messaging/common/v1.schema.json#/$defs/AuditPage", &a.json);
    assert_eq!(a.json["entries"].as_array().unwrap().len(), 1);
    assert_eq!(a.json["entries"][0]["targetId"], hold.json["holdId"]);
    let page = h.get(&c, None, "/v1/messaging/audit?limit=1").await;
    assert_eq!(page.json["entries"].as_array().unwrap().len(), 1);
    assert!(page.json["nextBefore"].is_string() || page.json["nextBefore"].is_null());
    assert_eq!(
        h.get(&c, Some(p), "/v1/messaging/audit").await.status,
        StatusCode::FORBIDDEN
    );
    let other = h.customer(ALL_SCOPES);
    let o = h.get(&other, None, "/v1/messaging/audit").await;
    assert_eq!(o.json["entries"].as_array().unwrap().len(), 0, "{}", o.text);
    assert_eq!(
        h.get(&c, None, "/v1/messaging/audit?action=lower")
            .await
            .status,
        StatusCode::BAD_REQUEST
    );
}

/// Race: a legal hold requested while a purge is in flight. The purge checked "not held"
/// inside its open transaction; the hold must NOT be acknowledged until that purge has
/// settled (else an acknowledged hold could lose its data). A purge that starts after the
/// acknowledgement sees the hold.
#[tokio::test]
async fn a_legal_hold_is_never_overtaken_by_an_in_flight_purge() {
    use sea_orm::TransactionTrait;
    let Some((h, _)) = harness().await else {
        return;
    };
    let c = h.customer(ALL_SCOPES);
    policy(
        &h,
        &c,
        json!({}),
        json!({ "retention": { "messageDays": 30 } }),
    )
    .await;
    let (conv, people) = managed_group(&h, &c, 2).await;
    let first = h.send_managed(&c, &conv, people[0], "purged first").await;
    let second = h.send_managed(&c, &conv, people[0], "held").await;
    h.clock.advance(chrono::Duration::days(31));
    let mut report = messaging::compliance::retention::RetentionReport::default();
    messaging::compliance::retention::expire_due(&h.state, 100, &mut report)
        .await
        .unwrap();
    assert_eq!(report.expired, 2, "{report:?}");
    let conv_id = Uuid::parse_str(&cid(&conv)).unwrap();
    let id1 = Uuid::parse_str(first.json["messageId"].as_str().unwrap()).unwrap();
    let id2 = Uuid::parse_str(second.json["messageId"].as_str().unwrap()).unwrap();
    let row1 = messaging::messages::load_message(&h.state.db, conv_id, id1, false)
        .await
        .unwrap()
        .unwrap();

    // 1. A purge is mid-transaction (checked, not yet committed) when the hold arrives.
    let purge = h.state.db.begin().await.unwrap();
    let outcome = messaging::compliance::retention::purge_message(&purge, &row1, h.state.now())
        .await
        .unwrap();
    assert!(outcome.is_some(), "not held when the purge checked");
    let hold_body = json!({ "scopeKind": "CONVERSATION", "conversationId": conv_id, "reasonCategory": "LITIGATION" });
    let finish_purge = async {
        tokio::time::sleep(std::time::Duration::from_millis(600)).await;
        // While the purge is open the hold is neither committed nor acknowledged.
        let visible = h
            .scalar("SELECT count(*) FROM messaging_legal_holds WHERE status = 'ACTIVE'")
            .await;
        purge.commit().await.unwrap();
        visible
    };
    let started = std::time::Instant::now();
    let (hold, visible_during_purge) = tokio::join!(
        async {
            let r = h
                .post(&c, None, "/v1/messaging/legal-holds", hold_body.clone())
                .await;
            (r, started.elapsed())
        },
        finish_purge
    );
    let (hold, waited) = hold;
    assert_eq!(hold.status, StatusCode::CREATED, "{}", hold.text);
    assert_eq!(
        visible_during_purge, 0,
        "the hold waited for the in-flight purge"
    );
    assert!(
        waited >= std::time::Duration::from_millis(550),
        "the hold was acknowledged only after the purge settled ({waited:?})"
    );
    // The purge that settled first stands (it preceded the hold) ...
    assert_eq!(
        h.scalar(&format!(
            "SELECT count(*) FROM messaging_messages WHERE id = '{id1}' AND lifecycle = 'PURGED'"
        ))
        .await,
        1
    );
    // 2. ... and every purge after the acknowledgement honours the hold.
    let r = messaging::compliance::run_once(&h.state).await.unwrap();
    assert_eq!((r.purged, r.held), (0, 1), "{r:?}");
    assert_eq!(
        h.scalar(&format!("SELECT count(*) FROM messaging_messages WHERE id = '{id2}' AND lifecycle = 'EXPIRED' AND managed_sealed IS NOT NULL")).await,
        1,
        "the held message keeps its content"
    );
    println!(
        "EVIDENCE hold_vs_purge hold_acknowledged_after_ms={}",
        waited.as_millis()
    );
}

/// §46 / §121 / §155: a MANAGED edit keeps the superseded version as a sealed revision
/// (never overwritten in place); concurrent edits resolve with a version check; members
/// see revisions only when the tenant's policy shows history; exports include them; a
/// legal hold preserves them and the purge removes them with the message.
#[tokio::test]
async fn managed_edits_keep_sealed_revisions_that_holds_preserve_and_purge_removes() {
    use sea_orm::ConnectionTrait;
    let Some((h, store)) = harness().await else {
        return;
    };
    let c = h.customer(ALL_SCOPES);
    policy(
        &h,
        &c,
        json!({ "exportsEnabled": true }),
        json!({ "retention": { "messageDays": 30 } }),
    )
    .await;
    let (conv, people) = managed_group(&h, &c, 2).await;
    let (alice, bob) = (people[0], people[1]);
    let id = cid(&conv);
    let sent = h
        .send_managed(&c, &conv, alice, "original wording zebra")
        .await;
    let mid = sent.json["messageId"].as_str().unwrap().to_string();
    let path = format!("/v1/conversations/{id}/messages/{mid}");

    // Two editors saw editCount 0: the first wins, the second is a version conflict.
    let first = h
        .patch(
            &c,
            Some(alice),
            &path,
            json!({ "text": "middle wording yak", "expectedEditCount": 0 }),
        )
        .await;
    assert_eq!(first.status, StatusCode::OK, "{}", first.text);
    let stale = h
        .patch(
            &c,
            Some(alice),
            &path,
            json!({ "text": "lost update", "expectedEditCount": 0 }),
        )
        .await;
    assert_eq!(stale.status, StatusCode::CONFLICT, "{}", stale.text);
    assert_eq!(
        (stale.code(), stale.reason()),
        ("VERSION_CONFLICT", "EDITED_CONCURRENTLY")
    );
    let last = h
        .patch(
            &c,
            Some(alice),
            &path,
            json!({ "text": "final wording xylophone", "expectedEditCount": 1 }),
        )
        .await;
    assert_eq!(last.json["editCount"], 2, "{}", last.text);
    assert_eq!(last.json["managed"]["text"], "final wording xylophone");

    // Both superseded versions are kept, sealed (no plaintext in the row).
    assert_eq!(
        h.scalar(&format!(
            "SELECT count(*) FROM messaging_message_revisions WHERE message_id = '{mid}'"
        ))
        .await,
        2
    );
    assert_eq!(
        h.scalar(&format!("SELECT count(*) FROM messaging_message_revisions WHERE message_id = '{mid}' \
             AND (position(convert_to('zebra', 'UTF8') in managed_sealed) > 0 OR position(convert_to('yak', 'UTF8') in managed_sealed) > 0)")).await,
        0,
        "revisions are sealed at rest"
    );
    // Search follows the current text only.
    let search = |q: &str| format!("/v1/conversations/{id}/search?q={q}");
    let old = h.get(&c, Some(bob), &search("zebra")).await;
    assert_eq!(old.status, StatusCode::OK, "{}", old.text);
    assert!(
        !old.text.contains(&mid),
        "a superseded word is not searchable: {}",
        old.text
    );
    assert!(h
        .get(&c, Some(bob), &search("xylophone"))
        .await
        .text
        .contains(&mid));

    // Members see history only when the policy shows it.
    let revisions = format!("{path}/revisions");
    let hidden = h.get(&c, Some(bob), &revisions).await;
    assert_eq!(
        hidden.reason(),
        "REVISION_HISTORY_HIDDEN",
        "{}",
        hidden.text
    );
    policy(
        &h,
        &c,
        json!({ "exportsEnabled": true }),
        json!({ "retention": { "messageDays": 30 }, "editing": { "revisionHistoryVisible": true } }),
    )
    .await;
    let shown = h.get(&c, Some(bob), &revisions).await;
    assert_eq!(shown.status, StatusCode::OK, "{}", shown.text);
    conforms(
        "messaging/common/v1.schema.json#/$defs/MessageRevisionPage",
        &shown.json,
    );
    let texts: Vec<&str> = shown.json["revisions"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r["text"].as_str().unwrap())
        .collect();
    assert_eq!(texts, ["original wording zebra", "middle wording yak"]);
    let outsider = h.principal(&c);
    assert_eq!(
        h.get(&c, Some(outsider), &revisions).await.status,
        StatusCode::NOT_FOUND
    );

    // Exports carry the revisions.
    let r = h
        .post(
            &c,
            None,
            "/v1/messaging/exports",
            json!({ "conversationId": conv["conversationId"] }),
        )
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text);
    let eid = r.json["exportId"].as_str().unwrap().to_string();
    messaging::compliance::run_once(&h.state).await.unwrap();
    let key = format!(
        "exp/{}/{}",
        c.context.scope.environment_id.simple(),
        Uuid::parse_str(&eid).unwrap().simple()
    );
    let doc: Value = serde_json::from_slice(&store.get_raw(&key).unwrap()).unwrap();
    assert_eq!(doc["messages"][0]["text"], "final wording xylophone");
    assert_eq!(
        doc["messages"][0]["revisions"][0]["text"],
        "original wording zebra"
    );
    assert_eq!(
        doc["messages"][0]["revisions"][1]["text"],
        "middle wording yak"
    );

    // The database keeps revisions immutable and bound to their message.
    assert!(h
        .state
        .db
        .execute_unprepared(&format!(
            "UPDATE messaging_message_revisions SET revision = 9 WHERE message_id = '{mid}'"
        ))
        .await
        .is_err());
    assert!(
        h.state
            .db
            .execute_unprepared(&format!(
                "DELETE FROM messaging_message_revisions WHERE message_id = '{mid}'"
            ))
            .await
            .is_err(),
        "not removable while the message exists"
    );

    // Under a legal hold, expiry keeps every version; after release the purge removes them.
    let hold = h
        .post(&c, None, "/v1/messaging/legal-holds", json!({ "scopeKind": "CONVERSATION", "conversationId": conv["conversationId"], "reasonCategory": "LITIGATION" }))
        .await;
    assert_eq!(hold.status, StatusCode::CREATED, "{}", hold.text);
    h.clock.advance(chrono::Duration::days(31));
    let r = messaging::compliance::run_once(&h.state).await.unwrap();
    assert!(r.held >= 1 && r.purged == 0, "{r:?}");
    assert_eq!(
        h.scalar(&format!(
            "SELECT count(*) FROM messaging_message_revisions WHERE message_id = '{mid}'"
        ))
        .await,
        2,
        "held revisions are preserved"
    );
    let rel = h
        .post(
            &c,
            None,
            &format!(
                "/v1/messaging/legal-holds/{}/release",
                hold.json["holdId"].as_str().unwrap()
            ),
            json!({ "reason": "AUDIT_DONE" }),
        )
        .await;
    assert_eq!(rel.status, StatusCode::OK, "{}", rel.text);
    h.clock.advance(chrono::Duration::hours(2));
    let r = messaging::compliance::run_once(&h.state).await.unwrap();
    assert!(r.purged >= 1, "{r:?}");
    assert_eq!(
        h.scalar(&format!(
            "SELECT count(*) FROM messaging_message_revisions WHERE message_id = '{mid}'"
        ))
        .await,
        0,
        "revisions are purged with the message"
    );
}
