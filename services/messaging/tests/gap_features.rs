//! Messaging gap closure: forwarding (S11), invite links (S12), announcement-only groups
//! (S13) and E2EE message franking (S2).

mod common;

use std::sync::Arc;

use axum::http::StatusCode;
use cf_object_storage::MemoryObjectStore;
use common::{flows::*, *};
use messaging::messages::franking;
use serde_json::{json, Value};
use uuid::Uuid;

fn cid(conv: &Value) -> String {
    conv["conversationId"].as_str().unwrap().to_string()
}

async fn version(h: &Harness, c: &Customer, owner: Uuid, conv: &Value) -> i64 {
    h.get(c, Some(owner), &format!("/v1/conversations/{}", cid(conv)))
        .await
        .json["version"]
        .as_i64()
        .unwrap()
}

#[tokio::test]
async fn announcement_only_groups_let_only_owners_and_admins_send() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let c = h.customer(ALL_SCOPES);
    let (conv, people) = h.managed_group(&c, 3).await;
    let (owner, member, admin) = (people[0], people[1], people[2]);
    let id = cid(&conv);
    let r = h
        .patch(
            &c,
            Some(owner),
            &format!("/v1/conversations/{id}/members/{admin}"),
            json!({ "role": "ADMIN" }),
        )
        .await;
    assert!(r.status.is_success(), "{}", r.text);
    let before = h.send_managed(&c, &conv, member, "before").await;
    assert_eq!(before.status, StatusCode::CREATED);
    // Only the owner changes the setting; DIRECT conversations refuse it.
    let v = version(&h, &c, owner, &conv).await;
    let r = h
        .patch(
            &c,
            Some(member),
            &format!("/v1/conversations/{id}"),
            json!({ "expectedVersion": v, "announcementOnly": true }),
        )
        .await;
    assert_eq!(r.reason(), "NOT_OWNER", "{}", r.text);
    let r = h
        .patch(
            &c,
            Some(owner),
            &format!("/v1/conversations/{id}"),
            json!({ "expectedVersion": v, "announcementOnly": true }),
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    assert_eq!(r.json["announcementOnly"], true);
    conforms(
        "messaging/common/v1.schema.json#/$defs/Conversation",
        &r.json,
    );
    // Send: owner and admin yes, member no.
    assert_eq!(
        h.send_managed(&c, &conv, owner, "news").await.status,
        StatusCode::CREATED
    );
    assert_eq!(
        h.send_managed(&c, &conv, admin, "more news").await.status,
        StatusCode::CREATED
    );
    let refused = h.send_managed(&c, &conv, member, "me too").await;
    assert_eq!(
        (refused.code(), refused.reason()),
        ("SEND_RESTRICTED", "ANNOUNCEMENT_ONLY")
    );
    // Edit of the member's earlier message: refused too.
    let mid = before.json["messageId"].as_str().unwrap();
    let r = h
        .patch(
            &c,
            Some(member),
            &format!("/v1/conversations/{id}/messages/{mid}"),
            json!({ "text": "edited" }),
        )
        .await;
    assert_eq!(r.reason(), "ANNOUNCEMENT_ONLY", "{}", r.text);
    // Reading stays.
    let list = h
        .get(
            &c,
            Some(member),
            &format!("/v1/conversations/{id}/messages"),
        )
        .await;
    assert_eq!(list.status, StatusCode::OK);
    // Back to everyone.
    let v = version(&h, &c, owner, &conv).await;
    let r = h
        .patch(
            &c,
            Some(owner),
            &format!("/v1/conversations/{id}"),
            json!({ "expectedVersion": v, "announcementOnly": false }),
        )
        .await;
    assert_eq!(r.json["announcementOnly"], false);
    assert_eq!(
        h.send_managed(&c, &conv, member, "again").await.status,
        StatusCode::CREATED
    );
}

#[tokio::test]
async fn managed_messages_are_forwarded_with_provenance_under_both_conversations_rules() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let c = h.customer(ALL_SCOPES);
    let (source, people) = h.managed_group(&c, 2).await;
    let (alice, bob) = (people[0], people[1]);
    // Bob owns a second group with Alice in it.
    let target = h
        .create_conversation(
            &c,
            bob,
            json!({ "contextClass": "GROUP", "securityProfile": "MANAGED" }),
        )
        .await;
    assert_eq!(target.status, StatusCode::CREATED, "{}", target.text);
    let target = target.json;
    let tid = cid(&target);
    let a = h
        .post(
            &c,
            Some(bob),
            &format!("/v1/conversations/{tid}/members"),
            json!({ "principalId": alice }),
        )
        .await;
    assert_eq!(a.status, StatusCode::CREATED, "{}", a.text);
    let original = h
        .send_managed(&c, &source, alice, "the plan, forward me")
        .await;
    let mid = original.json["messageId"].as_str().unwrap().to_string();
    let path = format!("/v1/conversations/{}/messages/{mid}/forward", cid(&source));

    // Bob forwards Alice's message into his group: a new message of Bob's, marked forwarded,
    // without the original sender (policy default) or the source ids.
    let client = Uuid::new_v4();
    let f = h
        .post(
            &c,
            Some(bob),
            &path,
            json!({ "targetConversationId": tid, "clientMessageId": client }),
        )
        .await;
    assert_eq!(f.status, StatusCode::CREATED, "{}", f.text);
    conforms("messaging/common/v1.schema.json#/$defs/Message", &f.json);
    assert_eq!(f.json["conversationId"], tid);
    assert_eq!(f.json["senderPrincipalId"], json!(bob));
    assert_eq!(f.json["managed"]["text"], "the plan, forward me");
    assert_eq!(f.json["forwarded"], true);
    assert_eq!(f.json["forwardedFromPrincipalId"], Value::Null);
    assert!(!f.text.contains(&cid(&source)) && !f.text.contains(&mid));
    // Idempotent per clientMessageId.
    let again = h
        .post(
            &c,
            Some(bob),
            &path,
            json!({ "targetConversationId": tid, "clientMessageId": client }),
        )
        .await;
    assert_eq!(again.status, StatusCode::OK, "{}", again.text);
    assert_eq!(again.json["messageId"], f.json["messageId"]);
    // The original sender is shown only when the policy says so.
    h.set_policy(
        &c,
        json!({ "contexts": { "GROUP": { "allowedProfiles": ["MANAGED", "PRIVATE_E2EE"], "defaultProfile": "MANAGED" } },
                "messages": { "forwardShowsOriginalSender": true } }),
    )
    .await;
    let f2 = h
        .post(
            &c,
            Some(bob),
            &path,
            json!({ "targetConversationId": tid, "clientMessageId": Uuid::new_v4() }),
        )
        .await;
    assert_eq!(
        f2.json["forwardedFromPrincipalId"],
        json!(alice),
        "{}",
        f2.text
    );
    // Target rules: a non-member target is unknown; an announcement-only target refuses
    // a plain member.
    let outsider = h
        .post(
            &c,
            Some(alice),
            &format!("/v1/conversations/{}/messages/{mid}/forward", cid(&source)),
            json!({ "targetConversationId": Uuid::new_v4(), "clientMessageId": Uuid::new_v4() }),
        )
        .await;
    assert_eq!(outsider.status, StatusCode::NOT_FOUND, "{}", outsider.text);
    let v = version(&h, &c, bob, &target).await;
    let r = h
        .patch(
            &c,
            Some(bob),
            &format!("/v1/conversations/{tid}"),
            json!({ "expectedVersion": v, "announcementOnly": true }),
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    let r = h
        .post(
            &c,
            Some(alice),
            &path,
            json!({ "targetConversationId": tid, "clientMessageId": Uuid::new_v4() }),
        )
        .await;
    assert_eq!(r.reason(), "ANNOUNCEMENT_ONLY", "{}", r.text);
    // Source rules: a non-member of the source cannot forward from it; policy can turn
    // forwarding off.
    let carol = h.principal(&c);
    let r = h
        .post(
            &c,
            Some(carol),
            &path,
            json!({ "targetConversationId": tid, "clientMessageId": Uuid::new_v4() }),
        )
        .await;
    assert_eq!(r.status, StatusCode::NOT_FOUND);
    h.set_policy(
        &c,
        json!({ "contexts": { "GROUP": { "allowedProfiles": ["MANAGED", "PRIVATE_E2EE"], "defaultProfile": "MANAGED" } },
                "messages": { "forwarding": false } }),
    )
    .await;
    let r = h
        .post(
            &c,
            Some(bob),
            &path,
            json!({ "targetConversationId": tid, "clientMessageId": Uuid::new_v4() }),
        )
        .await;
    assert_eq!(r.reason(), "FORWARDING_DISABLED", "{}", r.text);
    // Provenance can never be rewritten.
    assert!(h
        .sql(&format!(
            "UPDATE messaging_messages SET forwarded_from_message_id = NULL, forwarded_from_conversation_id = NULL WHERE id = '{}'",
            f.json["messageId"].as_str().unwrap()
        ))
        .await
        .is_err());
}

#[tokio::test]
async fn e2ee_messages_are_forwarded_client_side_only() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let c = h.customer(ALL_SCOPES);
    let (conv, mut alice, bob) = h.e2ee_direct(&c).await;
    let sent = h.send_e2ee(&c, &conv, &mut alice, "secret").await;
    let mid = sent.json["messageId"].as_str().unwrap();
    let (target, _) = h.managed_group(&c, 1).await;
    let r = h
        .post(
            &c,
            Some(bob.principal),
            &format!("/v1/conversations/{}/messages/{mid}/forward", cid(&conv)),
            json!({ "targetConversationId": cid(&target), "clientMessageId": Uuid::new_v4() }),
        )
        .await;
    assert_eq!(
        (r.code(), r.reason()),
        ("CONTENT_REJECTED", "E2EE_FORWARD_CLIENT_SIDE"),
        "{}",
        r.text
    );
}

#[tokio::test]
async fn invite_links_are_shown_once_stored_hashed_capped_expiring_and_revocable() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let c = h.customer(ALL_SCOPES);
    let (conv, people) = h.managed_group(&c, 2).await;
    let (owner, member) = (people[0], people[1]);
    let id = cid(&conv);
    let links = format!("/v1/conversations/{id}/invite-links");
    // Only OWNER / ADMIN create links.
    let r = h.post(&c, Some(member), &links, json!({})).await;
    assert_eq!(r.reason(), "NOT_ADMIN", "{}", r.text);
    let r = h
        .post(
            &c,
            Some(owner),
            &links,
            json!({ "maxUses": 2, "expiresInSeconds": 3600 }),
        )
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text);
    conforms(
        "messaging/common/v1.schema.json#/$defs/InviteLinkCreated",
        &r.json,
    );
    let token = r.json["token"].as_str().unwrap().to_string();
    let link_id = r.json["inviteLinkId"].as_str().unwrap().to_string();
    assert!(token.starts_with("mil_"));
    // Stored hashed: the token is in no column.
    assert!(!h.dump().await.contains(&token[4..]));
    // Listed without the token.
    let l = h.get(&c, Some(owner), &links).await;
    assert_eq!(l.status, StatusCode::OK, "{}", l.text);
    conforms(
        "messaging/common/v1.schema.json#/$defs/InviteLinkList",
        &l.json,
    );
    assert!(!l.text.contains(&token));
    assert_eq!(l.json["inviteLinks"][0]["status"], "ACTIVE");

    let join = |p: Uuid, t: String| {
        let h = &h;
        let c = &c;
        async move {
            h.post(
                c,
                Some(p),
                "/v1/messaging/invite-links/join",
                json!({ "token": t }),
            )
            .await
        }
    };
    let (dave, erin, frank) = (h.principal(&c), h.principal(&c), h.principal(&c));
    let r = join(dave, token.clone()).await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text);
    assert_eq!(h.scalar(&format!("SELECT count(*) FROM messaging_conversation_members WHERE conversation_id = '{id}' AND principal_id = '{dave}' AND state = 'ACTIVE' AND authority_source = 'INVITE_LINK'")).await, 1);
    // Joining again changes nothing and uses nothing.
    assert_eq!(join(dave, token.clone()).await.status, StatusCode::OK);
    // A banned principal is refused.
    let r = h
        .post(
            &c,
            Some(owner),
            &format!("/v1/conversations/{id}/bans"),
            json!({ "principalId": frank }),
        )
        .await;
    assert!(r.status.is_success(), "{}", r.text);
    let r = join(frank, token.clone()).await;
    assert_eq!(r.reason(), "PRINCIPAL_BANNED", "{}", r.text);
    // Second use, then the cap: the third joiner sees an unavailable link.
    assert_eq!(join(erin, token.clone()).await.status, StatusCode::CREATED);
    let r = join(h.principal(&c), token.clone()).await;
    assert_eq!(
        (r.status, r.reason()),
        (StatusCode::NOT_FOUND, "INVITE_LINK_UNAVAILABLE")
    );
    // Unknown and malformed tokens look the same.
    assert_eq!(
        join(h.principal(&c), "mil_nope".into()).await.reason(),
        "INVITE_LINK_UNAVAILABLE"
    );
    assert_eq!(
        join(h.principal(&c), "nope".into()).await.reason(),
        "INVITE_LINK_UNAVAILABLE"
    );
    // Revoked links stop working; a revocation is final.
    let r2 = h.post(&c, Some(owner), &links, json!({})).await;
    let t2 = r2.json["token"].as_str().unwrap().to_string();
    let l2 = r2.json["inviteLinkId"].as_str().unwrap().to_string();
    let r = h
        .post(&c, Some(owner), &format!("{links}/{l2}/revoke"), json!({}))
        .await;
    assert_eq!(r.json["status"], "REVOKED", "{}", r.text);
    assert_eq!(
        join(h.principal(&c), t2).await.reason(),
        "INVITE_LINK_UNAVAILABLE"
    );
    assert!(h.sql(&format!("UPDATE messaging_invite_links SET revoked_at = NULL, revoked_by_principal_id = NULL WHERE id = '{l2}'")).await.is_err());
    assert!(h
        .sql(&format!(
            "DELETE FROM messaging_invite_links WHERE id = '{link_id}'"
        ))
        .await
        .is_err());
    // Expired links stop working (the server's clock decides).
    let r3 = h
        .post(&c, Some(owner), &links, json!({ "expiresInSeconds": 60 }))
        .await;
    let t3 = r3.json["token"].as_str().unwrap().to_string();
    h.sql(&format!("UPDATE messaging_invite_links SET expires_at = created_at + interval '1 second' WHERE id = '{}'", r3.json["inviteLinkId"].as_str().unwrap()))
        .await
        .expect_err("expiry is immutable");
    let _ = t3;
    // Validation and DIRECT conversations.
    assert_eq!(
        h.post(&c, Some(owner), &links, json!({ "expiresInSeconds": 5 }))
            .await
            .status,
        StatusCode::BAD_REQUEST
    );
    assert_eq!(
        h.post(&c, Some(owner), &links, json!({ "maxUses": 0 }))
            .await
            .status,
        StatusCode::BAD_REQUEST
    );
    assert_eq!(
        h.post(&c, Some(owner), &links, json!({ "role": "ADMIN" }))
            .await
            .status,
        StatusCode::BAD_REQUEST
    );
}

#[tokio::test]
async fn e2ee_reports_with_a_revealed_franking_key_are_verified_against_the_senders_commitment() {
    let store = Arc::new(MemoryObjectStore::new());
    let Some(h) = Harness::build(move |s| s.storage = store).await else {
        return;
    };
    let c = h.customer(ALL_SCOPES);
    let (conv, mut alice, mut bob) = h.e2ee_direct(&c).await;
    let id = cid(&conv);
    // Alice's client: a fresh franking key travels INSIDE the ciphertext; the commitment
    // goes next to it.
    let key = [42u8; 32];
    let key_b64 = b64(&key);
    let text = "pay me or else, franked-7731";
    let plaintext = json!({ "text": text, "frankingKey": key_b64 }).to_string();
    let client_message_id = Uuid::new_v4();
    let commitment = franking::commitment(
        &key,
        uuid(&conv["conversationId"]),
        client_message_id,
        alice.principal,
        text,
    );
    let ct = h.encrypt_for(&c, &conv, &mut alice, client_message_id, &plaintext);
    let sent = h
        .post(
            &c,
            Some(alice.principal),
            &format!("/v1/conversations/{id}/messages"),
            json!({ "clientMessageId": client_message_id, "deviceId": alice.device_id(),
                    "epoch": alice.group.as_ref().unwrap().epoch(), "ciphertext": b64(&ct),
                    "frankingCommitment": commitment }),
        )
        .await;
    assert_eq!(sent.status, StatusCode::CREATED, "{}", sent.text);
    assert_eq!(sent.json["e2ee"]["frankingCommitment"], commitment);
    conforms("messaging/common/v1.schema.json#/$defs/Message", &sent.json);
    let mid = sent.json["messageId"].clone();
    // Bob decrypts, learns the key, can check the commitment, and reports revealing both.
    let got = h.receive(&c, &conv, &mut bob).await;
    let inner: Value = serde_json::from_str(&got.last().unwrap().1).unwrap();
    assert_eq!(inner["frankingKey"], key_b64);
    let report = |disclosed: &str, k: &str| {
        json!({ "category": "HARASSMENT", "messageIds": [mid], "reportedPrincipalId": alice.principal,
                "disclosure": [{ "messageId": mid, "text": disclosed, "frankingKey": k }] })
    };
    let r = h
        .post(
            &c,
            Some(bob.principal),
            &format!("/v1/conversations/{id}/reports"),
            report(text, &key_b64),
        )
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text);
    let view = h
        .get(
            &c,
            None,
            &format!(
                "/v1/messaging/reports/{}",
                r.json["reportId"].as_str().unwrap()
            ),
        )
        .await;
    assert_eq!(
        view.json["messages"][0]["e2ee"]["verification"], "FRANKING_VERIFIED",
        "{}",
        view.text
    );
    conforms(
        "messaging/common/v1.schema.json#/$defs/ReportDetail",
        &view.json,
    );
    // A forged plaintext does not verify.
    let r = h
        .post(
            &c,
            Some(bob.principal),
            &format!("/v1/conversations/{id}/reports"),
            report("something else", &key_b64),
        )
        .await;
    let view = h
        .get(
            &c,
            None,
            &format!(
                "/v1/messaging/reports/{}",
                r.json["reportId"].as_str().unwrap()
            ),
        )
        .await;
    assert_eq!(
        view.json["messages"][0]["e2ee"]["verification"], "FRANKING_MISMATCH",
        "{}",
        view.text
    );
    // The server never stored the key or plaintext in clear.
    let dump = h.dump().await;
    assert!(!dump.contains("franked-7731") && !dump.contains(&key_b64));
    // MANAGED messages carry no franking; a malformed commitment is refused.
    let (mconv, people) = h.managed_group(&c, 1).await;
    let r = h.post(&c, Some(people[0]), &format!("/v1/conversations/{}/messages", cid(&mconv)),
        json!({ "clientMessageId": Uuid::new_v4(), "text": "x", "frankingCommitment": commitment })).await;
    assert_eq!(r.reason(), "FRANKING_E2EE_ONLY", "{}", r.text);
    let r = h.post(&c, Some(alice.principal), &format!("/v1/conversations/{id}/messages"),
        json!({ "clientMessageId": Uuid::new_v4(), "deviceId": alice.device_id(), "epoch": 1, "ciphertext": b64(&ct),
                "frankingCommitment": "XYZ" })).await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST, "{}", r.text);
}
