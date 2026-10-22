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
    // §129 (G-18): a MANAGED search query is metered as one QUERIES fact.
    let s = h
        .get(
            &c,
            Some(people[0]),
            &format!("/v1/conversations/{cid}/search?q=metered"),
        )
        .await;
    assert_eq!(s.status, StatusCode::OK, "{}", s.text);
    let u = h
        .get(&c, None, &format!("/v1/messaging/usage?{range}"))
        .await;
    conforms(
        "messaging/common/v1.schema.json#/$defs/UsageSummary",
        &u.json,
    );
    let search = u.json["totals"]
        .as_array()
        .unwrap()
        .iter()
        .find(|t| t["usageType"] == "SEARCH_QUERY")
        .unwrap_or_else(|| panic!("{}", u.text));
    assert_eq!(
        (search["quantity"].as_i64(), search["unit"].as_str()),
        (Some(1), Some("QUERIES"))
    );
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

/// §28 / §47 / §49: the effective policy governs threads, reactions and pins. With threads
/// off, replies are still recorded but carry no thread root; with reactions or pins off,
/// new ones are refused (existing reactions can still be removed).
#[tokio::test]
async fn the_policy_snapshot_governs_threads_reactions_and_pins() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let c = h.first_party(false);
    let (conv, people) = h.managed_group(&c, 2).await;
    let (owner, member) = (people[0], people[1]);
    let cid = conv["conversationId"].as_str().unwrap().to_string();
    let root = h.send_managed(&c, &conv, owner, "agenda?").await;
    let root_id = root.json["messageId"].as_str().unwrap().to_string();
    let rpath = format!("/v1/conversations/{cid}/messages/{root_id}/reactions/:thumbsup:");
    assert_eq!(
        h.put(&c, Some(member), &rpath, json!({})).await.status,
        StatusCode::OK
    );

    let effective = h.get(&c, None, "/v1/messaging/policy").await;
    assert_eq!(
        effective.json["effective"]["messages"],
        json!({ "replies": true, "threads": true, "reactions": true, "pins": true }),
        "{}",
        effective.text
    );
    h.set_policy(
        &c,
        json!({ "contexts": { "GROUP": { "allowedProfiles": ["MANAGED", "PRIVATE_E2EE"], "defaultProfile": "MANAGED" } },
                "messages": { "threads": false, "reactions": false, "pins": false } }),
    )
    .await;
    let effective = h.get(&c, None, "/v1/messaging/policy").await;
    assert_eq!(
        effective.json["effective"]["messages"],
        json!({ "replies": true, "threads": false, "reactions": false, "pins": false })
    );

    let reply = h
        .post(&c, Some(member), &format!("/v1/conversations/{cid}/messages"),
              json!({ "clientMessageId": Uuid::new_v4(), "text": "item 1", "replyToMessageId": root_id }))
        .await;
    assert_eq!(reply.status, StatusCode::CREATED, "{}", reply.text);
    assert_eq!(
        reply.json["replyToMessageId"].as_str(),
        Some(root_id.as_str())
    );
    assert_eq!(
        reply.json["threadRootId"],
        Value::Null,
        "threads off: a reply, not a thread"
    );

    let refused = h
        .put(
            &c,
            Some(owner),
            &format!("/v1/conversations/{cid}/messages/{root_id}/reactions/:tada:"),
            json!({}),
        )
        .await;
    assert_eq!(refused.reason(), "REACTIONS_DISABLED", "{}", refused.text);
    let removed = h.delete(&c, Some(member), &rpath).await;
    assert_eq!(removed.status, StatusCode::OK, "{}", removed.text);
    let pin = h
        .put(
            &c,
            Some(owner),
            &format!("/v1/conversations/{cid}/pins/{root_id}"),
            json!({}),
        )
        .await;
    assert_eq!(pin.reason(), "PINS_DISABLED", "{}", pin.text);
}

/// §30: a large group's members page through `GET …/members` (complete, in join order,
/// without duplicates) and the conversation view embeds a bounded first page and the count.
#[tokio::test]
async fn large_group_membership_is_paginated_and_the_view_is_bounded() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let c = h.customer(ALL_SCOPES);
    let (conv, people) = h.managed_group(&c, 1).await;
    let owner = people[0];
    let cid = conv["conversationId"].as_str().unwrap().to_string();
    let mut all = std::collections::BTreeSet::from([owner]);
    for _ in 0..150 {
        let p = h.principal(&c);
        let r = h
            .post(
                &c,
                Some(owner),
                &format!("/v1/conversations/{cid}/members"),
                json!({ "principalId": p }),
            )
            .await;
        assert_eq!(r.status, StatusCode::CREATED, "{}", r.text);
        all.insert(p);
    }
    let mut seen = Vec::new();
    let mut cursor: Option<String> = None;
    let mut pages = 0;
    loop {
        let path = match &cursor {
            Some(c) => format!("/v1/conversations/{cid}/members?limit=60&cursor={c}"),
            None => format!("/v1/conversations/{cid}/members?limit=60"),
        };
        let page = h.get(&c, Some(owner), &path).await;
        assert_eq!(page.status, StatusCode::OK, "{}", page.text);
        conforms(
            "messaging/common/v1.schema.json#/$defs/MemberPage",
            &page.json,
        );
        pages += 1;
        for m in page.json["members"].as_array().unwrap() {
            seen.push(Uuid::parse_str(m["principalId"].as_str().unwrap()).unwrap());
        }
        match page.json["nextCursor"].as_str() {
            Some(next) => cursor = Some(next.to_string()),
            None => break,
        }
    }
    assert_eq!(pages, 3);
    assert_eq!(seen.len(), 151, "complete");
    assert_eq!(
        seen.iter().collect::<std::collections::BTreeSet<_>>().len(),
        151,
        "no duplicates"
    );
    assert_eq!(
        seen.iter()
            .copied()
            .collect::<std::collections::BTreeSet<_>>(),
        all
    );
    // (joined_at, principal_id) order; under the fixed test clock every join time ties,
    // so the keyset must order (and resume) by principal id alone.
    let mut sorted = seen.clone();
    sorted.sort();
    assert_eq!(seen, sorted, "keyset order across equal join times");
    let bad = h
        .get(
            &c,
            Some(owner),
            &format!("/v1/conversations/{cid}/members?limit=501"),
        )
        .await;
    assert_eq!(bad.status, StatusCode::BAD_REQUEST);
    let bad = h
        .get(
            &c,
            Some(owner),
            &format!("/v1/conversations/{cid}/members?cursor=bm90LWEtY3Vyc29y"),
        )
        .await;
    assert_eq!(bad.status, StatusCode::BAD_REQUEST);

    let view = h
        .get(&c, Some(owner), &format!("/v1/conversations/{cid}"))
        .await;
    conforms(
        "messaging/common/v1.schema.json#/$defs/Conversation",
        &view.json,
    );
    assert_eq!(view.json["members"].as_array().unwrap().len(), 100);
    assert_eq!(view.json["memberCount"], 151);
    assert_eq!(view.json["membersTruncated"], true);
    assert_eq!(view.json["members"][0]["role"], "OWNER");
}

/// §90 / Appendix K (G-14): a conversation-specific retention is only ever stricter than
/// the context's policy, only ever tightens (the database refuses loosening too), is
/// audited, and the retention worker expires that conversation's messages while a sibling
/// conversation under the same policy keeps its own.
#[tokio::test]
async fn a_conversation_retention_only_tightens_and_expires_its_own_messages() {
    use sea_orm::ConnectionTrait;
    let Some(h) = Harness::new().await else {
        return;
    };
    let c = h.customer(ALL_SCOPES);
    let (conv, people) = h.managed_group(&c, 2).await;
    let (sibling, _) = {
        let r = h
            .create_conversation(
                &c,
                people[0],
                json!({ "contextClass": "GROUP", "securityProfile": "MANAGED" }),
            )
            .await;
        assert_eq!(r.status, StatusCode::CREATED, "{}", r.text);
        (r.json, ())
    };
    h.set_policy(
        &c,
        json!({ "contexts": { "GROUP": { "allowedProfiles": ["MANAGED", "PRIVATE_E2EE"], "defaultProfile": "MANAGED", "retentionDays": 1 } } }),
    )
    .await;
    let cid = conv["conversationId"].as_str().unwrap().to_string();
    let path = format!("/v1/conversations/{cid}");
    let version = |v: &Value| v["version"].as_i64().unwrap();
    let current = h.get(&c, None, &path).await;
    assert_eq!(current.json["retentionSeconds"], Value::Null);
    let looser = h
        .patch(
            &c,
            None,
            &path,
            json!({ "expectedVersion": version(&current.json), "retentionSeconds": 2 * 86_400 }),
        )
        .await;
    assert_eq!(
        looser.reason(),
        "RETENTION_LOOSER_THAN_POLICY",
        "{}",
        looser.text
    );
    let too_short = h
        .patch(
            &c,
            None,
            &path,
            json!({ "expectedVersion": version(&current.json), "retentionSeconds": 59 }),
        )
        .await;
    assert_eq!(too_short.code(), "VALIDATION_ERROR", "{}", too_short.text);
    let set = h
        .patch(
            &c,
            None,
            &path,
            json!({ "expectedVersion": version(&current.json), "retentionSeconds": 3600 }),
        )
        .await;
    assert_eq!(set.status, StatusCode::OK, "{}", set.text);
    conforms(
        "messaging/common/v1.schema.json#/$defs/Conversation",
        &set.json,
    );
    assert_eq!(set.json["retentionSeconds"], 3600);
    let loosen = h
        .patch(
            &c,
            None,
            &path,
            json!({ "expectedVersion": version(&set.json), "retentionSeconds": 7200 }),
        )
        .await;
    assert_eq!(
        loosen.reason(),
        "RETENTION_ONLY_TIGHTENS",
        "{}",
        loosen.text
    );
    let refused = h
        .state
        .db
        .execute_unprepared(&format!(
            "UPDATE messaging_conversations SET retention_seconds = NULL WHERE id = '{cid}'"
        ))
        .await;
    assert!(
        refused.is_err(),
        "the database refuses removing a retention"
    );
    assert_eq!(
        h.scalar(&format!("SELECT count(*) FROM messaging_audit WHERE action = 'CONVERSATION_RETENTION_SET' AND target_id = '{cid}'")).await,
        1
    );

    let mine = h.send_managed(&c, &conv, people[0], "disappears").await;
    let theirs = h.send_managed(&c, &sibling, people[0], "stays a day").await;
    assert_eq!(
        (mine.status, theirs.status),
        (StatusCode::CREATED, StatusCode::CREATED)
    );
    h.clock.advance(chrono::Duration::hours(2));
    let mut report = messaging::compliance::retention::RetentionReport::default();
    messaging::compliance::retention::expire_due(&h.state, 100, &mut report)
        .await
        .unwrap();
    assert_eq!(report.expired, 1, "{report:?}");
    let lifecycle = |id: &Value| {
        format!(
            "SELECT count(*) FROM messaging_messages WHERE id = '{}' AND lifecycle = 'ACTIVE'",
            id.as_str().unwrap()
        )
    };
    assert_eq!(h.scalar(&lifecycle(&mine.json["messageId"])).await, 0);
    assert_eq!(h.scalar(&lifecycle(&theirs.json["messageId"])).await, 1);
    // A second pass finds nothing more (idempotent).
    let mut again = messaging::compliance::retention::RetentionReport::default();
    messaging::compliance::retention::expire_due(&h.state, 100, &mut again)
        .await
        .unwrap();
    assert_eq!(again.expired, 0);
    // Creation may set a (stricter) retention directly; a looser one is refused.
    let r = h
        .create_conversation(
            &c,
            people[0],
            json!({ "contextClass": "GROUP", "securityProfile": "MANAGED", "retentionSeconds": 600 }),
        )
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text);
    assert_eq!(r.json["retentionSeconds"], 600);
    let r = h
        .create_conversation(
            &c,
            people[0],
            json!({ "contextClass": "GROUP", "securityProfile": "MANAGED", "retentionSeconds": 3 * 86_400 }),
        )
        .await;
    assert_eq!(r.reason(), "RETENTION_LOOSER_THAN_POLICY", "{}", r.text);
}

/// §49 (G-08): pins may expire; an expired pin is neither listed nor counted, and pinning
/// the message again renews it.
#[tokio::test]
async fn pins_can_expire_and_be_renewed() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let c = h.customer(ALL_SCOPES);
    let (conv, people) = h.managed_group(&c, 2).await;
    let owner = people[0];
    let cid = conv["conversationId"].as_str().unwrap().to_string();
    let m = h.send_managed(&c, &conv, owner, "pin me").await;
    let mid = m.json["messageId"].as_str().unwrap().to_string();
    let pin = format!("/v1/conversations/{cid}/pins/{mid}");
    let list = format!("/v1/conversations/{cid}/pins");
    let past = h
        .put(
            &c,
            Some(owner),
            &pin,
            json!({ "expiresAt": (h.now() - chrono::Duration::minutes(1)).to_rfc3339() }),
        )
        .await;
    assert_eq!(past.code(), "VALIDATION_ERROR", "{}", past.text);
    let unknown = h.put(&c, Some(owner), &pin, json!({ "until": "x" })).await;
    assert_eq!(unknown.code(), "VALIDATION_ERROR", "{}", unknown.text);
    let at = h.now() + chrono::Duration::hours(1);
    let r = h
        .put(
            &c,
            Some(owner),
            &pin,
            json!({ "expiresAt": at.to_rfc3339() }),
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    conforms("messaging/common/v1.schema.json#/$defs/PinChange", &r.json);
    assert_eq!(r.json["changed"], true);
    let l = h.get(&c, Some(people[1]), &list).await;
    conforms("messaging/common/v1.schema.json#/$defs/PinList", &l.json);
    assert_eq!(l.json["pins"].as_array().unwrap().len(), 1);
    assert!(l.json["pins"][0]["expiresAt"].is_string());
    h.clock.advance(chrono::Duration::hours(2));
    assert_eq!(
        h.get(&c, Some(people[1]), &list).await.json["pins"],
        json!([]),
        "an expired pin is not listed"
    );
    let renewed = h.put(&c, Some(owner), &pin, json!({})).await;
    assert_eq!(renewed.status, StatusCode::OK, "{}", renewed.text);
    assert_eq!(renewed.json["changed"], true);
    assert_eq!(renewed.json["expiresAt"], Value::Null);
    let l = h.get(&c, Some(people[1]), &list).await;
    assert_eq!(l.json["pins"].as_array().unwrap().len(), 1);
    assert_eq!(l.json["pins"][0]["expiresAt"], Value::Null);
    // Re-pinning an unexpiring pin is a no-op.
    assert_eq!(
        h.put(&c, Some(owner), &pin, json!({})).await.json["changed"],
        false
    );
}

/// §47 (G-07): a thread summary — reply count, last reply and the caller's unread replies
/// (from their read cursor) — inside the caller's history window only.
#[tokio::test]
async fn thread_summaries_count_replies_and_unread_replies() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let c = h.customer(ALL_SCOPES);
    let (conv, people) = h.managed_group(&c, 2).await;
    let (owner, member) = (people[0], people[1]);
    let cid = conv["conversationId"].as_str().unwrap().to_string();
    let root = h.send_managed(&c, &conv, owner, "agenda?").await;
    let root_id = root.json["messageId"].as_str().unwrap().to_string();
    let path = format!("/v1/conversations/{cid}/messages");
    let summary = format!("{path}/{root_id}/thread");
    let empty = h.get(&c, Some(owner), &summary).await;
    assert_eq!(empty.status, StatusCode::OK, "{}", empty.text);
    conforms(
        "messaging/common/v1.schema.json#/$defs/ThreadSummary",
        &empty.json,
    );
    assert_eq!(
        (
            empty.json["replyCount"].as_i64(),
            empty.json["lastReplySequence"].clone()
        ),
        (Some(0), Value::Null)
    );
    let mut last = 0;
    for (who, text) in [(member, "one"), (member, "two"), (owner, "three")] {
        let r = h
            .post(&c, Some(who), &path, json!({ "clientMessageId": Uuid::new_v4(), "text": text, "replyToMessageId": root_id }))
            .await;
        assert_eq!(r.status, StatusCode::CREATED, "{}", r.text);
        last = r.json["sequence"].as_i64().unwrap();
    }
    // A message outside the thread does not count.
    h.send_managed(&c, &conv, member, "unrelated").await;
    let s = h.get(&c, Some(owner), &summary).await;
    conforms(
        "messaging/common/v1.schema.json#/$defs/ThreadSummary",
        &s.json,
    );
    assert_eq!(s.json["replyCount"], 3, "{}", s.text);
    assert_eq!(s.json["lastReplySequence"], last);
    assert_eq!(s.json["unreadReplyCount"], 2, "the member's two replies");
    let r = h
        .put(
            &c,
            Some(owner),
            &format!("/v1/conversations/{cid}/read-cursor"),
            json!({ "sequence": last }),
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    assert_eq!(
        h.get(&c, Some(owner), &summary).await.json["unreadReplyCount"],
        0
    );
    // Non-members and unknown roots see nothing.
    let outsider = h.principal(&c);
    assert_eq!(
        h.get(&c, Some(outsider), &summary).await.status,
        StatusCode::NOT_FOUND
    );
    let unknown = format!("{path}/{}/thread", Uuid::new_v4());
    assert_eq!(
        h.get(&c, Some(owner), &unknown).await.status,
        StatusCode::NOT_FOUND
    );
}

/// §30 / §113 (G-01): MODERATOR moderates members without administering roles or
/// outranking administrators; READ_ONLY reads but sends nothing; BOT_SERVICE is for bot
/// principals only and bots never administer.
#[tokio::test]
async fn moderator_read_only_and_bot_service_roles_are_enforced() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let c = h.first_party(false);
    let (conv, people) = h.managed_group(&c, 5).await;
    let (owner, moderator, reader, plain, admin) =
        (people[0], people[1], people[2], people[3], people[4]);
    let cid = conv["conversationId"].as_str().unwrap().to_string();
    let member = |p: Uuid| format!("/v1/conversations/{cid}/members/{p}");
    for (p, role) in [
        (moderator, "MODERATOR"),
        (reader, "READ_ONLY"),
        (admin, "ADMIN"),
    ] {
        let r = h
            .patch(&c, Some(owner), &member(p), json!({ "role": role }))
            .await;
        assert_eq!(r.status, StatusCode::OK, "{role}: {}", r.text);
        conforms("messaging/common/v1.schema.json#/$defs/RoleChange", &r.json);
    }
    let list = h
        .get(&c, Some(owner), &format!("/v1/conversations/{cid}/members"))
        .await;
    conforms(
        "messaging/common/v1.schema.json#/$defs/MemberPage",
        &list.json,
    );
    let view = h.get(&c, None, &format!("/v1/conversations/{cid}")).await;
    conforms(
        "messaging/common/v1.schema.json#/$defs/Conversation",
        &view.json,
    );

    // READ_ONLY: reads its window, sends nothing (messages, attachments), still reacts.
    let m = h.send_managed(&c, &conv, owner, "announcement").await;
    let r = h.send_managed(&c, &conv, reader, "may I?").await;
    assert_eq!(
        (r.code(), r.reason()),
        ("PRINCIPAL_NOT_AUTHORIZED", "READ_ONLY_MEMBER"),
        "{}",
        r.text
    );
    let read = h
        .get(
            &c,
            Some(reader),
            &format!("/v1/conversations/{cid}/messages"),
        )
        .await;
    assert!(read.text.contains("announcement"), "{}", read.text);
    let mid = m.json["messageId"].as_str().unwrap();
    let r = h
        .put(
            &c,
            Some(reader),
            &format!("/v1/conversations/{cid}/messages/{mid}/reactions/:thumbsup:"),
            json!({}),
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);

    // MODERATOR: removes and bans plain members, never administrators or moderators, and
    // administers no roles or members.
    let r = h
        .patch(
            &c,
            Some(moderator),
            &member(plain),
            json!({ "role": "ADMIN" }),
        )
        .await;
    assert_eq!(r.reason(), "NOT_ADMIN", "{}", r.text);
    let r = h
        .post(
            &c,
            Some(moderator),
            &format!("/v1/conversations/{cid}/members"),
            json!({ "principalId": h.principal(&c) }),
        )
        .await;
    assert_eq!(r.reason(), "NOT_ADMIN", "{}", r.text);
    let r = h.delete(&c, Some(moderator), &member(admin)).await;
    assert_eq!(r.reason(), "NOT_ADMIN", "{}", r.text);
    let r = h
        .post(
            &c,
            Some(moderator),
            &format!("/v1/conversations/{cid}/bans"),
            json!({ "principalId": owner }),
        )
        .await;
    assert_eq!(r.reason(), "NOT_ADMIN", "{}", r.text);
    let r = h
        .post(
            &c,
            Some(moderator),
            &format!("/v1/conversations/{cid}/bans"),
            json!({ "principalId": plain }),
        )
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text);
    let r = h
        .get(
            &c,
            Some(moderator),
            &format!("/v1/conversations/{cid}/bans"),
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    let r = h
        .delete(
            &c,
            Some(moderator),
            &format!("/v1/conversations/{cid}/bans/{plain}"),
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    let r = h.delete(&c, Some(moderator), &member(reader)).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);

    // BOT_SERVICE: only for bot principals; bots never administer.
    let bot = h.principal_of_kind(&c, "BOT_OR_SERVICE");
    let human = h.principal(&c);
    let add = format!("/v1/conversations/{cid}/members");
    let r = h
        .post(
            &c,
            Some(owner),
            &add,
            json!({ "principalId": human, "role": "BOT_SERVICE" }),
        )
        .await;
    assert_eq!(
        r.reason(),
        "BOT_SERVICE_REQUIRES_BOT_PRINCIPAL",
        "{}",
        r.text
    );
    let r = h
        .post(
            &c,
            Some(owner),
            &add,
            json!({ "principalId": bot, "role": "ADMIN" }),
        )
        .await;
    assert_eq!(r.reason(), "BOT_CANNOT_ADMINISTER", "{}", r.text);
    let r = h
        .post(
            &c,
            Some(owner),
            &add,
            json!({ "principalId": bot, "role": "BOT_SERVICE" }),
        )
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text);
    conforms("messaging/common/v1.schema.json#/$defs/Member", &r.json);
    assert_eq!(
        (r.json["role"].as_str(), r.json["authoritySource"].as_str()),
        (Some("BOT_SERVICE"), Some("API"))
    );
    let r = h.send_managed(&c, &conv, bot, "build passed").await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text);
    let r = h
        .patch(
            &c,
            Some(owner),
            &member(bot),
            json!({ "role": "MODERATOR" }),
        )
        .await;
    assert_eq!(r.reason(), "BOT_CANNOT_ADMINISTER", "{}", r.text);
    let r = h
        .patch(&c, Some(owner), &member(admin), json!({ "role": "OWNER" }))
        .await;
    assert_eq!(r.code(), "VALIDATION_ERROR", "{}", r.text);
}

/// §35 (G-01): an invited principal is not a member until it accepts (no history before,
/// none of its invitation's window); invitations can be declined, revoked or expire.
#[tokio::test]
async fn invitations_are_accepted_declined_revoked_or_expire() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let c = h.first_party(false);
    let (conv, people) = h.managed_group(&c, 2).await;
    let (owner, plain) = (people[0], people[1]);
    let cid = conv["conversationId"].as_str().unwrap().to_string();
    let add = format!("/v1/conversations/{cid}/members");
    let (guest, other, late) = (h.principal(&c), h.principal(&c), h.principal(&c));
    h.send_managed(&c, &conv, owner, "before the invitation")
        .await;

    let r = h
        .post(
            &c,
            Some(plain),
            &add,
            json!({ "principalId": guest, "invite": true }),
        )
        .await;
    assert_eq!(r.reason(), "NOT_ADMIN", "{}", r.text);
    let r = h
        .post(
            &c,
            Some(owner),
            &add,
            json!({ "principalId": guest, "invite": true, "role": "READ_ONLY" }),
        )
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text);
    conforms("messaging/common/v1.schema.json#/$defs/Member", &r.json);
    assert_eq!(r.json["state"], "INVITED");
    assert!(r.json["invitationExpiresAt"].is_string());
    // Invited is not a member: nothing to read, nothing to send.
    let msgs = format!("/v1/conversations/{cid}/messages");
    assert_eq!(
        h.get(&c, Some(guest), &msgs).await.status,
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        h.send_managed(&c, &conv, guest, "hi").await.status,
        StatusCode::NOT_FOUND
    );
    let list = h.get(&c, Some(guest), "/v1/messaging/invitations").await;
    assert_eq!(list.status, StatusCode::OK, "{}", list.text);
    conforms(
        "messaging/common/v1.schema.json#/$defs/InvitationList",
        &list.json,
    );
    assert_eq!(
        list.json["invitations"][0]["conversationId"].as_str(),
        Some(cid.as_str())
    );
    assert_eq!(list.json["invitations"][0]["role"], "READ_ONLY");
    // Only the invitee answers.
    let accept = format!("/v1/conversations/{cid}/invitation/accept");
    assert_eq!(
        h.post(&c, Some(other), &accept, json!({})).await.status,
        StatusCode::NOT_FOUND
    );
    h.send_managed(&c, &conv, owner, "while invited").await;
    let a = h.post(&c, Some(guest), &accept, json!({})).await;
    assert_eq!(a.status, StatusCode::OK, "{}", a.text);
    conforms("messaging/common/v1.schema.json#/$defs/Member", &a.json);
    assert_eq!(
        (
            a.json["state"].as_str(),
            a.json["role"].as_str(),
            a.json["authoritySource"].as_str()
        ),
        (Some("ACTIVE"), Some("READ_ONLY"), Some("INVITATION"))
    );
    h.send_managed(&c, &conv, owner, "after joining").await;
    let read = h.get(&c, Some(guest), &msgs).await;
    assert_eq!(read.status, StatusCode::OK, "{}", read.text);
    assert!(read.text.contains("after joining"));
    assert!(!read.text.contains("before the invitation") && !read.text.contains("while invited"));
    assert_eq!(
        h.post(&c, Some(guest), &accept, json!({})).await.status,
        StatusCode::NOT_FOUND,
        "answered once"
    );

    // Declined by the invitee; revoked by an administrator.
    h.post(
        &c,
        Some(owner),
        &add,
        json!({ "principalId": other, "invite": true }),
    )
    .await;
    let d = h
        .post(
            &c,
            Some(other),
            &format!("/v1/conversations/{cid}/invitation/decline"),
            json!({}),
        )
        .await;
    assert_eq!(
        (d.status, d.json["state"].as_str()),
        (StatusCode::OK, Some("LEFT")),
        "{}",
        d.text
    );
    h.post(
        &c,
        Some(owner),
        &add,
        json!({ "principalId": other, "invite": true }),
    )
    .await;
    let r = h
        .delete(
            &c,
            Some(owner),
            &format!("/v1/conversations/{cid}/members/{other}"),
        )
        .await;
    assert_eq!(
        (r.status, r.json["state"].as_str()),
        (StatusCode::OK, Some("REMOVED")),
        "{}",
        r.text
    );
    assert_eq!(
        h.post(&c, Some(other), &accept, json!({})).await.status,
        StatusCode::NOT_FOUND
    );

    // Expiry: an invitation answered too late is closed and refused.
    h.post(
        &c,
        Some(owner),
        &add,
        json!({ "principalId": late, "invite": true }),
    )
    .await;
    h.clock.advance(chrono::Duration::days(8));
    assert_eq!(
        h.get(&c, Some(late), "/v1/messaging/invitations")
            .await
            .json["invitations"],
        json!([])
    );
    let r = h.post(&c, Some(late), &accept, json!({})).await;
    assert_eq!(
        (r.code(), r.reason()),
        ("CONFLICT", "INVITATION_EXPIRED"),
        "{}",
        r.text
    );
    assert_eq!(
        h.scalar(&format!("SELECT count(*) FROM messaging_conversation_members WHERE principal_id = '{late}' AND state = 'REMOVED' AND removed_reason = 'INVITATION_EXPIRED'")).await,
        1
    );
    let types = h.conforming_event_types().await;
    assert!(
        types.contains("messaging.member.invited")
            && types.contains("messaging.member.invitation-closed"),
        "{types:?}"
    );
}
