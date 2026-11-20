//! Contacts audit (owner request, 19 November 2026): K1-K11, KF1-KF3 PostgreSQL
//! acceptance. K5 (unblock keeps the contact) and K7 (re-opening a claimed invite) are
//! covered in `foundation.rs`; K8 (feed positions) in `migration_upgrade.rs`.
mod common;
use common::*;
use communications::{
    error::Error,
    model::{Profile, VerifiedAccount},
    store::one,
};
use serde_json::{json, Value};
use std::collections::BTreeMap;
use uuid::Uuid;

#[tokio::test]
async fn send_policy_overlaps_fresh_owner_reads_and_still_refuses_a_new_block() {
    use cf_communications_policy::{Action, PolicyCheck};
    let h = Harness::new().await;
    let a = h.profile(&h.account()).await;
    let b = h.profile(&h.account()).await;
    h.connected(&a, &b).await;
    h.world.write().unwrap().policy_read_barrier =
        Some(std::sync::Arc::new(tokio::sync::Barrier::new(2)));
    let request = PolicyCheck {
        action: Action::MessageSend,
        actor_principal_id: a.principal_id,
        target_principal_id: b.principal_id,
        credential_issued_at: None,
        credential_principal_id: None,
        conversation_id: None,
        mentioned: None,
    };
    assert!(
        h.state
            .check_policy(&h.scope, &request)
            .await
            .unwrap()
            .allowed
    );
    *h.blocked.write().unwrap() = true;
    assert!(
        !h.state
            .check_policy(&h.scope, &request)
            .await
            .unwrap()
            .allowed
    );
    h.world.write().unwrap().policy_read_barrier = None;
    h.close().await;
}

struct User {
    account: VerifiedAccount,
    profile: Profile,
}
async fn user(h: &Harness) -> User {
    let account = h.account();
    let profile = h.profile(&account).await;
    User { account, profile }
}
async fn contact_id(h: &Harness, owner: &Profile, other: &Profile) -> Uuid {
    let v: Value = one(&h.state.store.db,
        "SELECT jsonb_build_object('id',a.id) doc FROM communication_contact_aliases a JOIN communication_connections c ON c.id=a.connection_id WHERE a.owner_profile_id=$1 AND (c.left_profile_id=$2 OR c.right_profile_id=$2)",
        vec![owner.id.into(), other.id.into()]).await.unwrap().unwrap();
    Uuid::parse_str(v["id"].as_str().unwrap()).unwrap()
}
async fn request(h: &Harness, from: &Profile, to: &Profile) -> Result<Value, Error> {
    h.state
        .send_request(
            &h.scope,
            from,
            json!({"cfMeetId":h.public_id(to).await}),
            Uuid::new_v4(),
        )
        .await
}
async fn privacy(h: &Harness, p: &Profile, change: Value) {
    let current = h.state.get_privacy(p).await.unwrap();
    let mut settings = current["privacy"].clone();
    for (k, v) in change.as_object().unwrap() {
        settings[k] = v.clone();
    }
    h.state
        .set_privacy(
            p,
            json!({"expectedVersion":current["version"],"privacy":settings}),
            Uuid::new_v4(),
        )
        .await
        .unwrap();
}
fn id(v: &Value, key: &str) -> Uuid {
    Uuid::parse_str(v[key].as_str().unwrap()).unwrap()
}

#[tokio::test]
async fn k1_k10_contact_pages_come_from_one_query_and_counts_follow_connected_members() {
    let h = Harness::new().await;
    let a = user(&h).await;
    let mut others = Vec::new();
    for _ in 0..3 {
        let o = user(&h).await;
        h.connected(&a.profile, &o.profile).await;
        others.push(o);
    }
    // KF2: the About line is shown to contacts.
    let me = h.state.me(&others[0].profile).await.unwrap();
    h.state
        .patch_me(
            &others[0].profile,
            json!({"expectedVersion":me["version"],"about":"  At the clinic until 6  "}),
            Uuid::new_v4(),
        )
        .await
        .unwrap();
    let page = h
        .state
        .list_contacts(&a.profile, 2, Uuid::nil())
        .await
        .unwrap();
    assert_eq!(page["data"].as_array().unwrap().len(), 2);
    assert!(page["nextCursor"].is_string());
    let rest = h
        .state
        .list_contacts(&a.profile, 2, id(&page, "nextCursor"))
        .await
        .unwrap();
    assert_eq!(rest["data"].as_array().unwrap().len(), 1);
    assert!(rest["nextCursor"].is_null());
    let first = contact_id(&h, &a.profile, &others[0].profile).await;
    let view = h.state.contact_view(&a.profile, first).await.unwrap();
    assert_eq!(view["about"], "At the clinic until 6");
    assert_eq!(view["relationship"], "CONNECTED");
    assert_eq!(
        view["cfMeetId"],
        json!(h.public_id(&others[0].profile).await)
    );
    assert_eq!(view["principalId"], json!(others[0].profile.principal_id));
    assert!(view.get("favoriteRank").is_none());
    let all: Vec<Value> = page["data"]
        .as_array()
        .unwrap()
        .iter()
        .chain(rest["data"].as_array().unwrap())
        .cloned()
        .collect();
    assert!(all.contains(&view), "list and single views agree");

    // Favorites carry their rank.
    for (n, o) in others.iter().enumerate() {
        let c = contact_id(&h, &a.profile, &o.profile).await;
        h.state
            .set_favorite(
                &a.profile,
                c,
                true,
                json!({"rank":3 - n as i32}),
                Uuid::new_v4(),
            )
            .await
            .unwrap();
    }
    let favorites = h.state.list_favorites(&a.profile).await.unwrap();
    let ranks: Vec<i64> = favorites["data"]
        .as_array()
        .unwrap()
        .iter()
        .map(|f| f["favoriteRank"].as_i64().unwrap())
        .collect();
    assert_eq!(ranks, vec![1, 2, 3]);

    // K10: a list's count and limit follow the members still connected.
    let list = h
        .state
        .create_list(&a.profile, json!({"name":"Family"}), Uuid::new_v4())
        .await
        .unwrap();
    let list_id = id(&list, "listId");
    for o in &others {
        let c = contact_id(&h, &a.profile, &o.profile).await;
        h.state
            .set_list_member(&a.profile, list_id, c, true, Uuid::new_v4())
            .await
            .unwrap();
    }
    // One removes A, one is blocked by A.
    let removed = contact_id(&h, &others[1].profile, &a.profile).await;
    h.state
        .remove_contact(&others[1].profile, removed, Uuid::new_v4())
        .await
        .unwrap();
    h.state
        .set_block(&a.profile, &others[2].profile, true, Uuid::new_v4())
        .await
        .unwrap();
    let got = h
        .state
        .get_list(&a.profile, list_id, 50, Uuid::nil())
        .await
        .unwrap();
    assert_eq!(got["members"].as_array().unwrap().len(), 1);
    assert_eq!(got["memberCount"], 1);
    let lists = h.state.list_lists(&a.profile).await.unwrap();
    assert_eq!(lists["data"][0]["memberCount"], 1);
    // The ended contacts are hidden from the contact list, the blocked one too.
    let page = h
        .state
        .list_contacts(&a.profile, 50, Uuid::nil())
        .await
        .unwrap();
    assert_eq!(page["data"].as_array().unwrap().len(), 1);
    // A contact whose relationship ended no longer shows its About line.
    let ended = contact_id(&h, &a.profile, &others[2].profile).await;
    let view = h.state.contact_view(&a.profile, ended).await.unwrap();
    assert_eq!(view["relationship"], "BLOCKED");
    assert!(view["about"].is_null() && view["principalId"].is_null());
    h.close().await;
}

#[tokio::test]
async fn k2_adding_someone_who_already_asked_you_connects_both() {
    let h = Harness::new().await;
    let (a, b) = (user(&h).await, user(&h).await);
    let theirs = request(&h, &b.profile, &a.profile).await.unwrap();
    assert_eq!(theirs["state"], "PENDING");
    // B's discoverability and request settings do not stand in the way: A holds B's request.
    privacy(
        &h,
        &b.profile,
        json!({"publicIdDiscoverable":false,"contactRequestPermissions":"NOBODY"}),
    )
    .await;
    let mine = request(&h, &a.profile, &b.profile).await.unwrap();
    assert_eq!(mine["state"], "ACCEPTED");
    assert_eq!(mine["relationship"], "CONNECTED");
    assert_eq!(mine["requestId"], theirs["requestId"]);
    let incoming = h
        .state
        .list_requests(&a.profile, true, 50, Uuid::nil())
        .await
        .unwrap();
    assert_eq!(incoming["data"][0]["state"], "ACCEPTED");
    let c = contact_id(&h, &b.profile, &a.profile).await;
    assert_eq!(
        h.state.contact_view(&b.profile, c).await.unwrap()["relationship"],
        "CONNECTED"
    );
    // Asking again once connected is the existing "already connected" answer.
    privacy(&h, &b.profile, json!({"publicIdDiscoverable":true})).await;
    assert!(matches!(
        request(&h, &a.profile, &b.profile).await,
        Err(Error::Pending)
    ));
    h.close().await;
}

#[tokio::test]
async fn k3_the_wait_after_a_decline_applies_only_to_the_declined_person() {
    let h = Harness::new().await;
    let (a, b) = (user(&h).await, user(&h).await);
    let theirs = request(&h, &b.profile, &a.profile).await.unwrap();
    h.state
        .decide_request(
            &h.scope,
            &a.profile,
            id(&theirs, "requestId"),
            "decline",
            json!({"expectedVersion":1}),
            Uuid::new_v4(),
        )
        .await
        .unwrap();
    // B (declined) waits 24 hours.
    assert!(matches!(
        request(&h, &b.profile, &a.profile).await,
        Err(Error::RateLimited)
    ));
    // A (who declined) can change their mind at once.
    let mine = request(&h, &a.profile, &b.profile).await.unwrap();
    assert_eq!(mine["state"], "PENDING");
    // A cancels: now A is the one who waits, B may ask (and that is an ordinary request).
    h.state
        .decide_request(
            &h.scope,
            &a.profile,
            id(&mine, "requestId"),
            "cancel",
            json!({"expectedVersion":1}),
            Uuid::new_v4(),
        )
        .await
        .unwrap();
    assert!(matches!(
        request(&h, &a.profile, &b.profile).await,
        Err(Error::RateLimited)
    ));
    h.close().await;
}

#[tokio::test]
async fn k4_an_invite_from_the_person_works_even_with_requests_set_to_nobody() {
    let h = Harness::new().await;
    let (a, b, c) = (user(&h).await, user(&h).await, user(&h).await);
    privacy(
        &h,
        &a.profile,
        json!({"contactRequestPermissions":"NOBODY"}),
    )
    .await;
    let invite = h
        .state
        .create_invite(
            &a.profile,
            json!({"purpose":"CONNECT","expiresInSeconds":600,"maxUses":1}),
            Uuid::new_v4(),
            true,
        )
        .await
        .unwrap();
    let token = invite["payload"]
        .as_str()
        .unwrap()
        .rsplit('/')
        .next()
        .unwrap()
        .to_owned();
    h.state
        .resolve_invite(
            &h.scope,
            &b.profile,
            json!({"token":token,"purpose":"CONNECT"}),
            Uuid::new_v4(),
        )
        .await
        .unwrap();
    // B holds A's invite: the request goes through (A approves manually).
    let sent = request(&h, &b.profile, &a.profile).await.unwrap();
    assert_eq!(sent["state"], "PENDING");
    // Without an invite the setting still refuses.
    not_found(request(&h, &c.profile, &a.profile).await);

    // With direct contact (AUTOMATIC) the invite connects at once.
    let (d, e) = (user(&h).await, user(&h).await);
    privacy(
        &h,
        &d.profile,
        json!({"contactRequestPermissions":"NOBODY","contactApproval":"AUTOMATIC"}),
    )
    .await;
    let invite = h
        .state
        .create_invite(
            &d.profile,
            json!({"purpose":"CONNECT","expiresInSeconds":600,"maxUses":1}),
            Uuid::new_v4(),
            false,
        )
        .await
        .unwrap();
    let token = invite["payload"]
        .as_str()
        .unwrap()
        .rsplit('/')
        .next()
        .unwrap()
        .to_owned();
    h.state
        .resolve_invite(
            &h.scope,
            &e.profile,
            json!({"token":token,"purpose":"CONNECT"}),
            Uuid::new_v4(),
        )
        .await
        .unwrap();
    let added = request(&h, &e.profile, &d.profile).await.unwrap();
    assert_eq!(added["state"], "ACCEPTED");
    assert_eq!(added["relationship"], "CONNECTED");
    // An expired or revoked invite no longer helps.
    h.state
        .revoke_invite(&d.profile, id(&invite, "inviteId"), Uuid::new_v4())
        .await
        .unwrap();
    let f = user(&h).await;
    not_found(request(&h, &f.profile, &d.profile).await);
    h.close().await;
}

#[tokio::test]
async fn k9_saved_messages_are_checked_in_one_batch_and_failures_mark_items() {
    let h = Harness::new().await;
    let (a, b) = (user(&h).await, user(&h).await);
    let sa = h.delegated(&a.account);
    let mut ids = Vec::new();
    for _ in 0..3 {
        let conv = Uuid::new_v4();
        let m = Uuid::new_v4();
        let mut messages = BTreeMap::new();
        messages.insert(m, (5, "VISIBLE".to_string()));
        h.world.write().unwrap().conversations.insert(
            conv,
            MockConversation {
                context: "DIRECT".into(),
                members: vec![a.profile.principal_id, b.profile.principal_id],
                last_sequence: 5,
                messages,
            },
        );
        h.state
            .save_message(
                &sa,
                &a.profile,
                json!({"conversationId":conv,"messageId":m}),
                Uuid::new_v4(),
            )
            .await
            .unwrap();
        ids.push((conv, m));
    }
    // A leaves the first conversation; the second message is deleted.
    h.world
        .write()
        .unwrap()
        .conversations
        .get_mut(&ids[0].0)
        .unwrap()
        .members
        .retain(|p| *p != a.profile.principal_id);
    h.world
        .write()
        .unwrap()
        .conversations
        .get_mut(&ids[1].0)
        .unwrap()
        .messages
        .insert(ids[1].1, (5, "DELETED".into()));
    let before = h.world.read().unwrap().access_calls;
    let list = h
        .state
        .list_saved(&sa, &a.profile, 30, Uuid::nil())
        .await
        .unwrap();
    assert_eq!(h.world.read().unwrap().batch_access_calls, 1);
    assert_eq!(
        h.world.read().unwrap().access_calls,
        before,
        "no per-item calls"
    );
    let reason = |list: &Value, m: Uuid| {
        list["data"]
            .as_array()
            .unwrap()
            .iter()
            .find(|i| i["messageId"] == json!(m))
            .unwrap()["unavailableReason"]
            .clone()
    };
    assert_eq!(reason(&list, ids[0].1), "NOT_A_MEMBER");
    assert_eq!(reason(&list, ids[1].1), "DELETED");
    assert_eq!(reason(&list, ids[2].1), Value::Null);

    // Messaging failing marks items unavailable instead of failing the list.
    h.world.write().unwrap().batch_access_down = true;
    let list = h
        .state
        .list_saved(&sa, &a.profile, 30, Uuid::nil())
        .await
        .unwrap();
    for (_, m) in &ids {
        assert_eq!(reason(&list, *m), "TEMPORARILY_UNAVAILABLE");
    }
    // An older Messaging image without the batched route: one call per item, same answer.
    h.world.write().unwrap().batch_access_down = false;
    h.world.write().unwrap().no_batch_access = true;
    let list = h
        .state
        .list_saved(&sa, &a.profile, 30, Uuid::nil())
        .await
        .unwrap();
    assert_eq!(reason(&list, ids[0].1), "NOT_A_MEMBER");
    assert_eq!(reason(&list, ids[1].1), "DELETED");
    assert_eq!(reason(&list, ids[2].1), Value::Null);
    h.close().await;
}

#[tokio::test]
async fn k11_mentions_only_is_not_offered_for_private_chats() {
    let h = Harness::new().await;
    let (a, b) = (user(&h).await, user(&h).await);
    h.connected(&a.profile, &b.profile).await;
    let c = contact_id(&h, &a.profile, &b.profile).await;
    assert!(matches!(
        h.state
            .patch_contact(
                &a.profile,
                c,
                json!({"expectedVersion":1,"notifications":"MENTIONS"}),
                Uuid::new_v4()
            )
            .await,
        Err(Error::Validation)
    ));
    h.state
        .patch_contact(
            &a.profile,
            c,
            json!({"expectedVersion":1,"notifications":"MUTED"}),
            Uuid::new_v4(),
        )
        .await
        .unwrap();
    let sa = h.delegated(&a.account);
    let private = Uuid::new_v4();
    let managed = Uuid::new_v4();
    for conv in [private, managed] {
        h.world.write().unwrap().conversations.insert(
            conv,
            MockConversation {
                context: "GROUP".into(),
                members: vec![a.profile.principal_id, b.profile.principal_id],
                last_sequence: 1,
                messages: BTreeMap::new(),
            },
        );
    }
    h.world.write().unwrap().managed.insert(managed);
    let mentions = json!({"expectedVersion":0,"notifications":"MENTIONS","mutedUntil":null,"mediaAutoDownload":"WIFI"});
    assert!(matches!(
        h.state
            .put_conversation_preferences(
                &sa,
                &a.profile,
                private,
                mentions.clone(),
                Uuid::new_v4()
            )
            .await,
        Err(Error::Validation)
    ));
    let view = h
        .state
        .get_conversation_preferences(&sa, &a.profile, private)
        .await
        .unwrap();
    assert_eq!(view["mentionsAvailable"], false);
    let put = h
        .state
        .put_conversation_preferences(&sa, &a.profile, managed, mentions, Uuid::new_v4())
        .await
        .unwrap();
    assert_eq!(put["notifications"], "MENTIONS");
    assert_eq!(put["mentionsAvailable"], true);
    h.close().await;
}

#[tokio::test]
async fn kf1_people_who_opted_in_can_be_found_by_verified_email() {
    let h = Harness::new().await;
    let (a, b) = (user(&h).await, user(&h).await);
    h.world.write().unwrap().verified_emails.insert(
        "nurse@example.org".into(),
        b.account.account_reference.clone(),
    );
    let sa = h.delegated(&a.account);
    // Off by default: a match is the same 404 as no account.
    assert_eq!(
        h.state.get_privacy(&b.profile).await.unwrap()["privacy"]["emailDiscoverable"],
        false
    );
    not_found(
        h.state
            .resolve_email(&sa, &a.profile, json!({"email":"nurse@example.org"}))
            .await,
    );
    privacy(&h, &b.profile, json!({"emailDiscoverable":true})).await;
    let found = h
        .state
        .resolve_email(&sa, &a.profile, json!({"email":"  Nurse@Example.ORG "}))
        .await
        .unwrap();
    assert_eq!(found["cfMeetId"], json!(h.public_id(&b.profile).await));
    assert!(found.get("accountReference").is_none());
    // Then exactly the CF Meet ID rules: the found person can be added.
    let added = h
        .state
        .send_request(
            &h.scope,
            &a.profile,
            json!({"cfMeetId":found["cfMeetId"]}),
            Uuid::new_v4(),
        )
        .await
        .unwrap();
    assert_eq!(added["state"], "PENDING");
    not_found(
        h.state
            .resolve_email(&sa, &a.profile, json!({"email":"nobody@example.org"}))
            .await,
    );
    // A block hides them.
    h.state
        .set_block(&b.profile, &a.profile, true, Uuid::new_v4())
        .await
        .unwrap();
    not_found(
        h.state
            .resolve_email(&sa, &a.profile, json!({"email":"nurse@example.org"}))
            .await,
    );
    assert!(matches!(
        h.state
            .resolve_email(&sa, &a.profile, json!({"email":"not an email"}))
            .await,
        Err(Error::Validation)
    ));
    // Enumeration guard: 20 lookups per person and hour (4 used above).
    let mut limited = false;
    for _ in 0..20 {
        if matches!(
            h.state
                .resolve_email(&sa, &a.profile, json!({"email":"x@example.org"}))
                .await,
            Err(Error::RateLimited)
        ) {
            limited = true;
            break;
        }
    }
    assert!(limited);
    h.close().await;
}

#[tokio::test]
async fn kf2_the_about_line_is_validated_and_versioned() {
    let h = Harness::new().await;
    let a = user(&h).await;
    let me = h.state.me(&a.profile).await.unwrap();
    assert!(me["about"].is_null());
    let v = me["version"].clone();
    for bad in [
        json!({"expectedVersion":v,"about":"x".repeat(141)}),
        json!({"expectedVersion":v,"about":"line\nbreak"}),
        json!({"expectedVersion":v}),
        json!({"expectedVersion":v,"about":"ok","displayName":"no"}),
    ] {
        assert!(matches!(
            h.state.patch_me(&a.profile, bad, Uuid::new_v4()).await,
            Err(Error::Validation)
        ));
    }
    let ok = h
        .state
        .patch_me(
            &a.profile,
            json!({"expectedVersion":v,"about":"é".repeat(140)}),
            Uuid::new_v4(),
        )
        .await
        .unwrap();
    assert_eq!(ok["about"], json!("é".repeat(140)));
    assert_eq!(ok["version"], v.as_i64().unwrap() + 1);
    assert!(matches!(
        h.state
            .patch_me(
                &a.profile,
                json!({"expectedVersion":v,"about":null}),
                Uuid::new_v4()
            )
            .await,
        Err(Error::Version)
    ));
    let cleared = h
        .state
        .patch_me(
            &a.profile,
            json!({"expectedVersion":ok["version"],"about":""}),
            Uuid::new_v4(),
        )
        .await
        .unwrap();
    assert!(cleared["about"].is_null());
    h.close().await;
}
