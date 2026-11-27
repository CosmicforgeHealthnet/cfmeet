//! Chat organization and MANAGED polls: a member's own Favourites and chat lists, polls in
//! MANAGED conversations (votes, live tallies through the conversation events, close) and
//! a MANAGED group's description.

mod common;

use axum::http::StatusCode;
use common::{flows::ALL_SCOPES, *};
use serde_json::{json, Value};
use uuid::Uuid;

fn cid(conv: &Value) -> String {
    conv["conversationId"].as_str().unwrap().to_string()
}

async fn inbox_of(h: &Harness, c: &Customer, who: Uuid, id: &str) -> Value {
    let list = h
        .get(c, Some(who), "/v1/conversations?orderBy=lastActivity")
        .await;
    assert_eq!(list.status, StatusCode::OK, "{}", list.text);
    list.json["conversations"]
        .as_array()
        .unwrap()
        .iter()
        .find(|v| v["conversationId"] == id)
        .map(|v| v["inbox"].clone())
        .expect("listed")
}

/// Favourites are the member's own, independent of archive and pin.
#[tokio::test]
async fn members_mark_favourite_chats_for_themselves() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let c = h.customer(ALL_SCOPES);
    let (conv, people) = h.managed_group(&c, 2).await;
    let (me, other) = (people[0], people[1]);
    let id = cid(&conv);
    let settings = format!("/v1/conversations/{id}/inbox-settings");
    assert_eq!(inbox_of(&h, &c, me, &id).await["favourite"], false);
    let r = h
        .put(&c, Some(me), &settings, json!({ "favourite": true }))
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    conforms(
        "messaging/common/v1.schema.json#/$defs/ConversationInboxSettings",
        &r.json,
    );
    assert_eq!(r.json["favourite"], true);
    assert_eq!(inbox_of(&h, &c, me, &id).await["favourite"], true);
    assert_eq!(inbox_of(&h, &c, other, &id).await["favourite"], false);
    // Pinning and archiving leave it a favourite.
    let r = h
        .put(&c, Some(me), &settings, json!({ "archived": true }))
        .await;
    assert_eq!(
        (r.json["archived"].clone(), r.json["favourite"].clone()),
        (json!(true), json!(true))
    );
    let r = h
        .put(&c, Some(me), &settings, json!({ "favourite": false }))
        .await;
    assert_eq!(
        (r.json["archived"].clone(), r.json["favourite"].clone()),
        (json!(true), json!(false))
    );
    let outsider = h.principal(&c);
    let r = h
        .put(&c, Some(outsider), &settings, json!({ "favourite": true }))
        .await;
    assert_eq!(r.code(), "NOT_FOUND", "{}", r.text);
}

/// Chat lists: named, ordered, the member's own; several lists may hold one chat; only
/// chats the member is in; removing an item or a list never touches the conversation.
#[tokio::test]
async fn members_organize_chats_into_their_own_lists() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let c = h.customer(ALL_SCOPES);
    let (conv, people) = h.managed_group(&c, 2).await;
    let (me, other) = (people[0], people[1]);
    let id = cid(&conv);
    let second = h
        .create_conversation(
            &c,
            me,
            json!({ "contextClass": "GROUP", "securityProfile": "MANAGED" }),
        )
        .await;
    assert_eq!(second.status, StatusCode::CREATED, "{}", second.text);
    let second = cid(&second.json);

    let empty = h.get(&c, Some(me), "/v1/messaging/chat-lists").await;
    assert_eq!(empty.status, StatusCode::OK, "{}", empty.text);
    conforms(
        "messaging/common/v1.schema.json#/$defs/ChatListPage",
        &empty.json,
    );
    assert_eq!(empty.json["chatLists"], json!([]));

    let family = h
        .post(
            &c,
            Some(me),
            "/v1/messaging/chat-lists",
            json!({ "name": " Family ", "conversationIds": [id] }),
        )
        .await;
    assert_eq!(family.status, StatusCode::CREATED, "{}", family.text);
    conforms(
        "messaging/common/v1.schema.json#/$defs/ChatList",
        &family.json,
    );
    assert_eq!(family.json["name"], "Family");
    assert_eq!(family.json["position"], 0);
    assert_eq!(family.json["conversationIds"], json!([id]));
    let family_id = family.json["listId"].as_str().unwrap().to_string();

    // Names are unique per member (case-insensitively); a chat the member is not in is
    // refused without saying whether it exists.
    let dup = h
        .post(
            &c,
            Some(me),
            "/v1/messaging/chat-lists",
            json!({ "name": "family" }),
        )
        .await;
    assert_eq!(
        (dup.code(), dup.reason()),
        ("CONFLICT", "CHAT_LIST_NAME_TAKEN")
    );
    let foreign = h
        .post(
            &c,
            Some(me),
            "/v1/messaging/chat-lists",
            json!({ "name": "Work", "conversationIds": [Uuid::new_v4()] }),
        )
        .await;
    assert_eq!(foreign.code(), "NOT_FOUND", "{}", foreign.text);
    let blank = h
        .post(
            &c,
            Some(me),
            "/v1/messaging/chat-lists",
            json!({ "name": "  " }),
        )
        .await;
    assert_eq!(blank.code(), "VALIDATION_ERROR", "{}", blank.text);

    let work = h
        .post(
            &c,
            Some(me),
            "/v1/messaging/chat-lists",
            json!({ "name": "Work", "conversationIds": [id, second] }),
        )
        .await;
    assert_eq!(work.status, StatusCode::CREATED, "{}", work.text);
    assert_eq!(work.json["position"], 1);
    let work_id = work.json["listId"].as_str().unwrap().to_string();

    // Add (idempotent) and remove items; one chat may be in several lists.
    let item = |list: &str, conversation: &str| {
        format!("/v1/messaging/chat-lists/{list}/conversations/{conversation}")
    };
    for _ in 0..2 {
        let r = h
            .put(&c, Some(me), &item(&family_id, &second), json!({}))
            .await;
        assert_eq!(r.status, StatusCode::OK, "{}", r.text);
        conforms("messaging/common/v1.schema.json#/$defs/ChatList", &r.json);
        let mut ids: Vec<String> =
            serde_json::from_value(r.json["conversationIds"].clone()).unwrap();
        ids.sort();
        let mut want = vec![id.clone(), second.clone()];
        want.sort();
        assert_eq!(ids, want);
    }
    let r = h
        .put(
            &c,
            Some(me),
            &item(&family_id, &Uuid::new_v4().to_string()),
            json!({}),
        )
        .await;
    assert_eq!(r.code(), "NOT_FOUND", "{}", r.text);
    let r = h.delete(&c, Some(me), &item(&family_id, &id)).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    assert_eq!(r.json["conversationIds"], json!([second]));
    // Removing it from a list left the conversation alone.
    assert_eq!(
        h.get(&c, Some(me), &format!("/v1/conversations/{id}"))
            .await
            .status,
        StatusCode::OK
    );

    // Rename and reorder.
    let r = h
        .patch(
            &c,
            Some(me),
            &format!("/v1/messaging/chat-lists/{work_id}"),
            json!({ "name": "Office", "position": 0 }),
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    assert_eq!(
        (r.json["name"].clone(), r.json["position"].clone()),
        (json!("Office"), json!(0))
    );
    let r = h
        .patch(
            &c,
            Some(me),
            &format!("/v1/messaging/chat-lists/{work_id}"),
            json!({}),
        )
        .await;
    assert_eq!(r.code(), "VALIDATION_ERROR", "{}", r.text);
    let page = h.get(&c, Some(me), "/v1/messaging/chat-lists").await;
    let names: Vec<&str> = page.json["chatLists"]
        .as_array()
        .unwrap()
        .iter()
        .map(|l| l["name"].as_str().unwrap())
        .collect();
    assert_eq!(names, vec!["Office", "Family"]);

    // Another member never sees or changes my lists.
    assert_eq!(
        h.get(&c, Some(other), "/v1/messaging/chat-lists")
            .await
            .json["chatLists"],
        json!([])
    );
    for r in [
        h.patch(
            &c,
            Some(other),
            &format!("/v1/messaging/chat-lists/{work_id}"),
            json!({ "name": "Mine" }),
        )
        .await,
        h.put(&c, Some(other), &item(&work_id, &id), json!({}))
            .await,
        h.delete(&c, Some(other), &item(&work_id, &id)).await,
        h.delete(
            &c,
            Some(other),
            &format!("/v1/messaging/chat-lists/{work_id}"),
        )
        .await,
    ] {
        assert_eq!(r.code(), "NOT_FOUND", "{}", r.text);
    }

    // A conversation I leave drops out of my lists (nothing is deleted).
    let r = h
        .delete(
            &c,
            Some(other),
            &format!("/v1/conversations/{id}/members/{other}"),
        )
        .await;
    assert!(r.status.is_success(), "{}", r.text);
    let r = h
        .post(
            &c,
            Some(other),
            "/v1/messaging/chat-lists",
            json!({ "name": "Gone", "conversationIds": [id] }),
        )
        .await;
    assert_eq!(r.code(), "NOT_FOUND", "{}", r.text);

    // At most twenty lists.
    for i in 2..20 {
        let r = h
            .post(
                &c,
                Some(me),
                "/v1/messaging/chat-lists",
                json!({ "name": format!("List {i}") }),
            )
            .await;
        assert_eq!(r.status, StatusCode::CREATED, "{}", r.text);
    }
    let r = h
        .post(
            &c,
            Some(me),
            "/v1/messaging/chat-lists",
            json!({ "name": "One more" }),
        )
        .await;
    assert_eq!(
        (r.code(), r.reason()),
        ("CONFLICT", "CHAT_LIST_LIMIT_REACHED")
    );

    // Deleting a list keeps its conversations.
    let r = h
        .delete(&c, Some(me), &format!("/v1/messaging/chat-lists/{work_id}"))
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    conforms(
        "messaging/common/v1.schema.json#/$defs/ChatListDeleted",
        &r.json,
    );
    let r = h
        .delete(&c, Some(me), &format!("/v1/messaging/chat-lists/{work_id}"))
        .await;
    assert_eq!(r.code(), "NOT_FOUND", "{}", r.text);
    assert_eq!(
        h.get(&c, Some(me), &format!("/v1/conversations/{second}"))
            .await
            .status,
        StatusCode::OK
    );
    let page = h.get(&c, Some(me), "/v1/messaging/chat-lists").await;
    let positions: Vec<i64> = page.json["chatLists"]
        .as_array()
        .unwrap()
        .iter()
        .map(|l| l["position"].as_i64().unwrap())
        .collect();
    assert_eq!(positions, (0..19).collect::<Vec<i64>>());
}

fn poll_of(message: &Value) -> Value {
    message["managed"]["poll"].clone()
}

/// MANAGED polls: a poll is a message; members vote (and change or retract their vote),
/// see who voted for what, follow the tally through the conversation events, and the
/// poll's sender or an admin closes it.
#[tokio::test]
async fn members_vote_on_managed_polls_and_follow_the_tally() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let c = h.customer(ALL_SCOPES);
    let (conv, people) = h.managed_group(&c, 3).await;
    let (owner, a, b) = (people[0], people[1], people[2]);
    let id = cid(&conv);
    let messages = format!("/v1/conversations/{id}/messages");

    let bad = h
        .post(
            &c,
            Some(a),
            &messages,
            json!({ "clientMessageId": Uuid::new_v4(), "text": "Lunch?", "poll": { "options": ["Only one"] } }),
        )
        .await;
    assert_eq!(bad.code(), "VALIDATION_ERROR", "{}", bad.text);
    let created = h
        .post(
            &c,
            Some(a),
            &messages,
            json!({ "clientMessageId": Uuid::new_v4(), "text": "Lunch?",
                    "poll": { "options": ["Pizza", "Sushi", "Salad"], "allowsMultipleAnswers": false } }),
        )
        .await;
    assert_eq!(created.status, StatusCode::CREATED, "{}", created.text);
    conforms(
        "messaging/common/v1.schema.json#/$defs/Message",
        &created.json,
    );
    assert_eq!(created.json["managed"]["presentation"], "POLL");
    assert_eq!(created.json["managed"]["text"], "Lunch?");
    let poll = poll_of(&created.json);
    assert_eq!(poll["options"][1]["text"], "Sushi");
    assert_eq!(
        (poll["voterCount"].clone(), poll["closed"].clone()),
        (json!(0), json!(false))
    );
    let mid = created.json["messageId"].as_str().unwrap().to_string();
    let vote = format!("/v1/conversations/{id}/messages/{mid}/poll/vote");
    let close = format!("/v1/conversations/{id}/messages/{mid}/poll/close");

    // The chat list previews the question, marked as a poll.
    let inbox = inbox_of(&h, &c, owner, &id).await;
    assert_eq!(inbox["lastMessage"]["previewText"], "Lunch?");
    assert_eq!(inbox["lastMessage"]["presentation"], "POLL");

    let r = h
        .put(&c, Some(b), &vote, json!({ "optionIndexes": [1] }))
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    conforms(
        "messaging/common/v1.schema.json#/$defs/PollChangeResult",
        &r.json,
    );
    assert_eq!(r.json["changed"], true);
    assert_eq!(
        r.json["poll"]["options"][1]["voterPrincipalIds"],
        json!([b])
    );
    // The same vote again changes nothing; one answer only.
    let r = h
        .put(&c, Some(b), &vote, json!({ "optionIndexes": [1] }))
        .await;
    assert_eq!(r.json["changed"], false);
    let r = h
        .put(&c, Some(owner), &vote, json!({ "optionIndexes": [0, 2] }))
        .await;
    assert_eq!(r.code(), "VALIDATION_ERROR", "{}", r.text);
    let r = h
        .put(&c, Some(owner), &vote, json!({ "optionIndexes": [7] }))
        .await;
    assert_eq!(r.code(), "VALIDATION_ERROR", "{}", r.text);
    // Change, then retract.
    let r = h
        .put(&c, Some(owner), &vote, json!({ "optionIndexes": [0] }))
        .await;
    assert_eq!(r.json["poll"]["voterCount"], 2);
    let r = h
        .put(&c, Some(b), &vote, json!({ "optionIndexes": [2] }))
        .await;
    assert_eq!(r.json["poll"]["options"][1]["voteCount"], 0);
    assert_eq!(
        r.json["poll"]["options"][2]["voterPrincipalIds"],
        json!([b])
    );
    let r = h
        .put(&c, Some(b), &vote, json!({ "optionIndexes": [] }))
        .await;
    assert_eq!(r.json["poll"]["voterCount"], 1);

    // Reading the message shows the current tally; the events carry it too.
    let read = h.get(&c, Some(b), &format!("{messages}/{mid}")).await;
    assert_eq!(
        poll_of(&read.json["message"])["options"][0]["voterPrincipalIds"],
        json!([owner])
    );
    let events = h
        .get(
            &c,
            Some(b),
            &format!("/v1/conversations/{id}/events?after=0"),
        )
        .await;
    let voted: Vec<&Value> = events.json["events"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|e| e["type"] == "poll.voted")
        .collect();
    assert_eq!(voted.len(), 4);
    assert!(voted[0]["data"]["message"]["managed"]["poll"].is_object());

    // Only the sender or an admin closes it; then no vote is accepted.
    let r = h.post(&c, Some(b), &close, json!({})).await;
    assert_eq!(
        (r.code(), r.reason()),
        ("PRINCIPAL_NOT_AUTHORIZED", "NOT_POLL_CREATOR")
    );
    let r = h.post(&c, Some(a), &close, json!({})).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    assert_eq!(
        (r.json["changed"].clone(), r.json["poll"]["closed"].clone()),
        (json!(true), json!(true))
    );
    assert_eq!(r.json["poll"]["closedByPrincipalId"], json!(a));
    let again = h.post(&c, Some(owner), &close, json!({})).await;
    assert_eq!(again.json["changed"], false);
    let r = h
        .put(&c, Some(b), &vote, json!({ "optionIndexes": [1] }))
        .await;
    assert_eq!((r.code(), r.reason()), ("CONFLICT", "POLL_CLOSED"));

    // A poll's question is not edited; a plain message is not a poll.
    let r = h
        .patch(
            &c,
            Some(a),
            &format!("{messages}/{mid}"),
            json!({ "text": "Dinner?" }),
        )
        .await;
    assert_eq!(r.reason(), "POLL_NOT_EDITABLE", "{}", r.text);
    let plain = h.send_managed(&c, &conv, a, "not a poll").await;
    let plain_id = plain.json["messageId"].as_str().unwrap();
    let r = h
        .put(
            &c,
            Some(b),
            &format!("/v1/conversations/{id}/messages/{plain_id}/poll/vote"),
            json!({ "optionIndexes": [0] }),
        )
        .await;
    assert_eq!(r.code(), "NOT_FOUND", "{}", r.text);
    let outsider = h.principal(&c);
    let r = h
        .put(&c, Some(outsider), &vote, json!({ "optionIndexes": [0] }))
        .await;
    assert_eq!(r.code(), "NOT_FOUND", "{}", r.text);

    // A deleted poll takes no more votes or closing.
    let r = h.delete(&c, Some(a), &format!("{messages}/{mid}")).await;
    assert!(r.status.is_success(), "{}", r.text);
    let r = h.post(&c, Some(a), &close, json!({})).await;
    assert_eq!(r.code(), "MESSAGE_NOT_MODIFIABLE", "{}", r.text);

    let types = h.conforming_event_types().await;
    assert!(types.contains("messaging.poll.voted") && types.contains("messaging.poll.closed"));
}

/// A MANAGED group's description: set on creation or by the owner / a group admin,
/// bounded, cleared with null; DIRECT conversations have none.
#[tokio::test]
async fn group_admins_describe_their_groups() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let c = h.customer(ALL_SCOPES);
    h.set_policy(
        &c,
        json!({ "contexts": {
            "GROUP": { "allowedProfiles": ["MANAGED", "PRIVATE_E2EE"], "defaultProfile": "MANAGED" },
            "DIRECT": { "allowedProfiles": ["MANAGED", "PRIVATE_E2EE"], "defaultProfile": "MANAGED" } } }),
    )
    .await;
    let (owner, admin, member) = (h.principal(&c), h.principal(&c), h.principal(&c));
    let r = h
        .create_conversation(
            &c,
            owner,
            json!({ "contextClass": "GROUP", "securityProfile": "MANAGED", "title": "Book club",
                    "description": "Monthly reads.\nBring snacks." }),
        )
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text);
    conforms(
        "messaging/common/v1.schema.json#/$defs/Conversation",
        &r.json,
    );
    assert_eq!(r.json["description"], "Monthly reads.\nBring snacks.");
    let id = cid(&r.json);
    for (p, role) in [(admin, Some("ADMIN")), (member, None)] {
        let a = h
            .post(
                &c,
                Some(owner),
                &format!("/v1/conversations/{id}/members"),
                json!({ "principalId": p }),
            )
            .await;
        assert_eq!(a.status, StatusCode::CREATED, "{}", a.text);
        if let Some(role) = role {
            let r = h
                .patch(
                    &c,
                    Some(owner),
                    &format!("/v1/conversations/{id}/members/{p}"),
                    json!({ "role": role }),
                )
                .await;
            assert!(r.status.is_success(), "{}", r.text);
        }
    }
    let version = |v: &Value| v["version"].as_i64().unwrap();
    let current = h
        .get(&c, Some(owner), &format!("/v1/conversations/{id}"))
        .await;
    let r = h
        .patch(
            &c,
            Some(admin),
            &format!("/v1/conversations/{id}"),
            json!({ "expectedVersion": version(&current.json), "description": "Weekly now", "title": "Book club+" }),
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    assert_eq!(
        (r.json["description"].clone(), r.json["title"].clone()),
        (json!("Weekly now"), json!("Book club+"))
    );
    // A plain member does not; an admin does not change what only the owner administers.
    let r2 = h
        .patch(
            &c,
            Some(member),
            &format!("/v1/conversations/{id}"),
            json!({ "expectedVersion": version(&r.json), "description": "Mine" }),
        )
        .await;
    assert_eq!(r2.reason(), "NOT_ADMIN", "{}", r2.text);
    let r2 = h
        .patch(
            &c,
            Some(admin),
            &format!("/v1/conversations/{id}"),
            json!({ "expectedVersion": version(&r.json), "description": "x", "announcementOnly": true }),
        )
        .await;
    assert_eq!(r2.reason(), "NOT_OWNER", "{}", r2.text);
    let r2 = h
        .patch(
            &c,
            Some(admin),
            &format!("/v1/conversations/{id}"),
            json!({ "expectedVersion": version(&r.json), "description": "d".repeat(513) }),
        )
        .await;
    assert_eq!(r2.code(), "VALIDATION_ERROR", "{}", r2.text);
    let cleared = h
        .patch(
            &c,
            Some(owner),
            &format!("/v1/conversations/{id}"),
            json!({ "expectedVersion": version(&r.json), "description": null }),
        )
        .await;
    assert_eq!(cleared.status, StatusCode::OK, "{}", cleared.text);
    assert_eq!(cleared.json["description"], Value::Null);

    let direct = h
        .create_conversation(
            &c,
            owner,
            json!({ "contextClass": "DIRECT", "securityProfile": "MANAGED",
                    "counterpartPrincipalId": member, "description": "no" }),
        )
        .await;
    assert_eq!(direct.code(), "VALIDATION_ERROR", "{}", direct.text);
}
