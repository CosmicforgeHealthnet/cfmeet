//! Part V I §113 — member roles and bans, replies and threads, reactions and pins, and
//! unread counts.

mod common;

use axum::http::StatusCode;
use common::{flows::ALL_SCOPES, *};
use serde_json::{json, Value};
use uuid::Uuid;

#[tokio::test]
async fn roles_and_bans_are_administrator_only_and_bans_block_re_adding() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let c = h.first_party(false);
    let (conv, people) = h.managed_group(&c, 3).await;
    let (owner, a, b) = (people[0], people[1], people[2]);
    let cid = conv["conversationId"].as_str().unwrap().to_string();

    // A member cannot promote; the owner can; the owner's role is fixed.
    let r = h
        .patch(
            &c,
            Some(a),
            &format!("/v1/conversations/{cid}/members/{b}"),
            json!({ "role": "ADMIN" }),
        )
        .await;
    assert_eq!(r.reason(), "NOT_ADMIN", "{}", r.text);
    let r = h
        .patch(
            &c,
            Some(owner),
            &format!("/v1/conversations/{cid}/members/{a}"),
            json!({ "role": "ADMIN" }),
        )
        .await;
    assert_eq!(
        (r.status, r.json["changed"].as_bool()),
        (StatusCode::OK, Some(true)),
        "{}",
        r.text
    );
    conforms("messaging/common/v1.schema.json#/$defs/RoleChange", &r.json);
    let r = h
        .patch(
            &c,
            Some(a),
            &format!("/v1/conversations/{cid}/members/{owner}"),
            json!({ "role": "MEMBER" }),
        )
        .await;
    assert_eq!(r.reason(), "OWNER_ROLE_FIXED", "{}", r.text);
    let r = h
        .patch(
            &c,
            Some(owner),
            &format!("/v1/conversations/{cid}/members/{a}"),
            json!({ "role": "OWNER" }),
        )
        .await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST, "{}", r.text);

    // The new admin bans b: b is removed and cannot be re-added until unbanned.
    let r = h
        .post(
            &c,
            Some(a),
            &format!("/v1/conversations/{cid}/bans"),
            json!({ "principalId": b, "reason": "SPAM" }),
        )
        .await;
    assert!(r.status.is_success(), "{}", r.text);
    let sent = h.send_managed(&c, &conv, b, "still here?").await;
    assert!(
        !sent.status.is_success(),
        "a banned principal cannot send: {}",
        sent.text
    );
    let bans = h
        .get(&c, Some(owner), &format!("/v1/conversations/{cid}/bans"))
        .await;
    assert_eq!(bans.status, StatusCode::OK, "{}", bans.text);
    assert!(bans.text.contains(&b.to_string()), "{}", bans.text);
    conforms("messaging/common/v1.schema.json#/$defs/BanList", &bans.json);
    let r = h
        .post(
            &c,
            Some(owner),
            &format!("/v1/conversations/{cid}/members"),
            json!({ "principalId": b }),
        )
        .await;
    assert!(
        !r.status.is_success(),
        "a banned principal is not re-added: {}",
        r.text
    );
    let r = h
        .get(&c, Some(b), &format!("/v1/conversations/{cid}/bans"))
        .await;
    assert!(!r.status.is_success(), "{}", r.text);
    let r = h
        .delete(
            &c,
            Some(owner),
            &format!("/v1/conversations/{cid}/bans/{b}"),
        )
        .await;
    assert!(r.status.is_success(), "{}", r.text);
    let r = h
        .post(
            &c,
            Some(owner),
            &format!("/v1/conversations/{cid}/members"),
            json!({ "principalId": b }),
        )
        .await;
    assert!(r.status.is_success(), "{}", r.text);
    assert!(
        h.scalar("SELECT count(*) FROM messaging_audit WHERE action IN ('MEMBER_ROLE_CHANGED','MEMBER_BANNED','MEMBER_UNBANNED')")
            .await
            >= 3
    );
}

#[tokio::test]
async fn replies_threads_reactions_pins_and_unread_counts() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let c = h.first_party(false);
    let (conv, people) = h.managed_group(&c, 2).await;
    let (owner, member) = (people[0], people[1]);
    let cid = conv["conversationId"].as_str().unwrap().to_string();
    let root = h.send_managed(&c, &conv, owner, "agenda?").await;
    let root_id = root.json["messageId"].as_str().unwrap().to_string();

    // Replies carry the root as their thread; a reply to a reply keeps the same root.
    let path = format!("/v1/conversations/{cid}/messages");
    let r1 = h
        .post(&c, Some(member), &path, json!({ "clientMessageId": Uuid::new_v4(), "text": "item 1", "replyToMessageId": root_id }))
        .await;
    assert_eq!(r1.status, StatusCode::CREATED, "{}", r1.text);
    assert_eq!(
        (
            r1.json["replyToMessageId"].as_str(),
            r1.json["threadRootId"].as_str()
        ),
        (Some(root_id.as_str()), Some(root_id.as_str()))
    );
    let r1_id = r1.json["messageId"].as_str().unwrap().to_string();
    let r2 = h
        .post(
            &c,
            Some(owner),
            &path,
            json!({ "clientMessageId": Uuid::new_v4(), "text": "ok", "replyToMessageId": r1_id }),
        )
        .await;
    assert_eq!(
        r2.json["threadRootId"].as_str(),
        Some(root_id.as_str()),
        "{}",
        r2.text
    );
    let missing = h
        .post(&c, Some(owner), &path, json!({ "clientMessageId": Uuid::new_v4(), "text": "?", "replyToMessageId": Uuid::new_v4() }))
        .await;
    assert_eq!(
        missing.reason(),
        "REPLY_TARGET_NOT_FOUND",
        "{}",
        missing.text
    );
    let thread = h
        .get(&c, Some(member), &format!("{path}?threadRootId={root_id}"))
        .await;
    assert_eq!(thread.status, StatusCode::OK, "{}", thread.text);
    assert_eq!(
        thread.json["messages"].as_array().map(|m| m.len()),
        Some(3),
        "{}",
        thread.text
    );

    // Reactions: idempotent per principal, counted, removable; invalid reactions refused.
    let rpath = format!("{path}/{root_id}/reactions");
    for who in [owner, member, member] {
        let r = h
            .put(&c, Some(who), &format!("{rpath}/:thumbsup:"), json!({}))
            .await;
        assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    }
    let r = h
        .put(&c, Some(owner), &format!("{rpath}/%F0%9F%91%8D"), json!({}))
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    let r = h
        .put(&c, Some(owner), &format!("{rpath}/hello"), json!({}))
        .await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST, "{}", r.text);
    let of = |list: &Value, r: &str| {
        list["reactions"]
            .as_array()
            .unwrap()
            .iter()
            .find(|x| x["reaction"] == r)
            .cloned()
            .unwrap_or(Value::Null)
    };
    let list = h.get(&c, Some(member), &rpath).await;
    let thumbs = of(&list.json, ":thumbsup:");
    assert_eq!(
        (thumbs["count"].as_i64(), thumbs["reactedByMe"].as_bool()),
        (Some(2), Some(true)),
        "{}",
        list.text
    );
    let emoji = of(&list.json, "\u{1F44D}");
    assert_eq!(
        (emoji["count"].as_i64(), emoji["reactedByMe"].as_bool()),
        (Some(1), Some(false)),
        "{}",
        list.text
    );
    conforms(
        "messaging/common/v1.schema.json#/$defs/ReactionSummary",
        &list.json,
    );
    h.delete(&c, Some(member), &format!("{rpath}/:thumbsup:"))
        .await;
    let list = h.get(&c, Some(member), &rpath).await;
    assert_eq!(of(&list.json, ":thumbsup:")["count"], 1, "{}", list.text);

    // Pins: administrators only; members see them.
    let ppath = format!("/v1/conversations/{cid}/pins");
    let r = h
        .put(&c, Some(member), &format!("{ppath}/{root_id}"), json!({}))
        .await;
    assert_eq!(r.reason(), "NOT_ADMIN", "{}", r.text);
    let r = h
        .put(&c, Some(owner), &format!("{ppath}/{root_id}"), json!({}))
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    let pins = h.get(&c, Some(member), &ppath).await;
    assert_eq!(
        pins.json["pins"][0]["messageId"].as_str(),
        Some(root_id.as_str()),
        "{}",
        pins.text
    );
    conforms("messaging/common/v1.schema.json#/$defs/PinList", &pins.json);
    h.delete(&c, Some(owner), &format!("{ppath}/{root_id}"))
        .await;
    assert_eq!(
        h.get(&c, Some(member), &ppath).await.json["pins"],
        json!([])
    );

    // Unread: the member has not read the owner's two messages; after reading, none.
    let u = h
        .get(&c, Some(member), &format!("/v1/conversations/{cid}/unread"))
        .await;
    assert_eq!(u.status, StatusCode::OK, "{}", u.text);
    assert_eq!(u.json["unreadCount"], 2, "{}", u.text);
    conforms(
        "messaging/common/v1.schema.json#/$defs/UnreadCount",
        &u.json,
    );
    let last = u.json["lastSequence"].as_i64().unwrap();
    let r = h
        .put(
            &c,
            Some(member),
            &format!("/v1/conversations/{cid}/read-cursor"),
            json!({ "sequence": last }),
        )
        .await;
    assert!(r.status.is_success(), "{}", r.text);
    let u = h
        .get(&c, Some(member), &format!("/v1/conversations/{cid}/unread"))
        .await;
    assert_eq!(u.json["unreadCount"], 0, "{}", u.text);

    // Every change is a domain event for realtime and webhooks.
    for t in [
        "messaging.reaction.added",
        "messaging.reaction.removed",
        "messaging.pin.added",
        "messaging.pin.removed",
    ] {
        assert!(
            h.scalar(&format!(
                "SELECT count(*) FROM messaging_outbox_events WHERE event_type = '{t}'"
            ))
            .await
                >= 1,
            "{t}"
        );
    }
    // Non-members see nothing.
    let outsider = h.principal(&c);
    assert_eq!(
        h.get(&c, Some(outsider), &rpath).await.status,
        StatusCode::NOT_FOUND
    );
}

#[tokio::test]
async fn e2ee_reactions_are_messages_not_server_rows() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let c = h.customer(ALL_SCOPES);
    let (conv, mut a, _b) = h.e2ee_direct(&c).await;
    let sent = h.send_e2ee(&c, &conv, &mut a, "hi").await;
    assert_eq!(sent.status, StatusCode::CREATED, "{}", sent.text);
    let cid = conv["conversationId"].as_str().unwrap();
    let mid = sent.json["messageId"].as_str().unwrap();
    let r = h
        .put(
            &c,
            Some(a.principal),
            &format!("/v1/conversations/{cid}/messages/{mid}/reactions/:thumbsup:"),
            json!({}),
        )
        .await;
    assert_eq!(r.reason(), "E2EE_REACTIONS_ARE_MESSAGES", "{}", r.text);
    assert_eq!(
        h.scalar("SELECT count(*) FROM messaging_message_reactions")
            .await,
        0
    );
}

#[tokio::test]
async fn usage_is_metered_as_facts_without_prices_and_scoped_to_the_environment() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let c = h.first_party(false);
    let (conv, people) = h.managed_group(&c, 2).await;
    for i in 0..3 {
        let r = h.send_managed(&c, &conv, people[i % 2], "metered").await;
        assert_eq!(r.status, StatusCode::CREATED, "{}", r.text);
    }
    // An idempotent replay of a send is not metered twice.
    let cid = conv["conversationId"].as_str().unwrap();
    let body = json!({ "clientMessageId": Uuid::new_v4(), "text": "once" });
    let path = format!("/v1/conversations/{cid}/messages");
    h.post(&c, Some(people[0]), &path, body.clone()).await;
    h.post(&c, Some(people[0]), &path, body).await;

    let window = "from=2000-01-01T00:00:00Z&to=2000-03-01T00:00:00Z";
    let now = chrono::Utc::now();
    let range = format!(
        "from={}&to={}",
        (now - chrono::Duration::days(1)).format("%Y-%m-%dT%H:%M:%SZ"),
        (now + chrono::Duration::days(1)).format("%Y-%m-%dT%H:%M:%SZ")
    );
    let u = h
        .get(&c, None, &format!("/v1/messaging/usage?{range}"))
        .await;
    assert_eq!(u.status, StatusCode::OK, "{}", u.text);
    let totals = u.json["totals"].as_array().unwrap();
    let messages = totals
        .iter()
        .find(|t| t["usageType"] == "MESSAGE_ACCEPTED")
        .unwrap_or_else(|| panic!("{}", u.text));
    assert_eq!(
        (messages["quantity"].as_i64(), messages["unit"].as_str()),
        (Some(4), Some("MESSAGES")),
        "{}",
        u.text
    );
    assert_eq!(messages["securityProfile"], "MANAGED");
    assert_eq!(u.json["pricing"], "NONE");
    conforms(
        "messaging/common/v1.schema.json#/$defs/UsageSummary",
        &u.json,
    );
    for word in ["price", "cost", "amount", "currency"] {
        assert!(
            !u.text.contains(&format!("\"{word}")),
            "no {word}: {}",
            u.text
        );
    }
    // Empty window; bad windows refused.
    let empty = h
        .get(&c, None, &format!("/v1/messaging/usage?{window}"))
        .await;
    assert_eq!(empty.json["totals"], json!([]), "{}", empty.text);
    let r = h
        .get(
            &c,
            None,
            "/v1/messaging/usage?from=2026-01-01T00:00:00Z&to=2026-12-31T00:00:00Z",
        )
        .await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST, "{}", r.text);
    // Another environment sees none of it; the scope is required.
    let other = h.first_party(false);
    let u = h
        .get(&other, None, &format!("/v1/messaging/usage?{range}"))
        .await;
    assert_eq!(u.json["totals"], json!([]), "{}", u.text);
    let narrow = h.customer(&["messages.read"]);
    let r = h
        .get(&narrow, None, &format!("/v1/messaging/usage?{range}"))
        .await;
    assert_eq!(r.status, StatusCode::FORBIDDEN, "{}", r.text);
    // Usage facts are immutable.
    assert!(h
        .sql("UPDATE messaging_usage_records SET quantity = 0")
        .await
        .is_err());
}
