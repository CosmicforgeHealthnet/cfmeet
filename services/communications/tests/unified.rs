//! Sections 10–16 PostgreSQL acceptance: favorites/lists, chat controls, call launch and
//! timeline, devices, meetings links, the change feed and private operator routes.
mod common;
use cf_communications_policy::Action;
use common::*;
use communications::{
    error::Error,
    model::{Profile, VerifiedAccount},
    store::{execute, one},
};
use serde_json::{json, Value};
use std::collections::BTreeMap;
use uuid::Uuid;

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
fn conversation(h: &Harness, context: &str, members: &[&User], last: i64) -> Uuid {
    let id = Uuid::new_v4();
    h.world.write().unwrap().conversations.insert(
        id,
        MockConversation {
            context: context.into(),
            members: members.iter().map(|u| u.profile.principal_id).collect(),
            last_sequence: last,
            messages: BTreeMap::new(),
        },
    );
    id
}
fn endpoint(h: &Harness, u: &User) -> Uuid {
    let id = Uuid::new_v4();
    h.world
        .write()
        .unwrap()
        .endpoints
        .insert(id, (u.profile.principal_id, "ACTIVE".into()));
    id
}
fn q(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
    pairs
        .iter()
        .map(|(k, v)| ((*k).into(), (*v).into()))
        .collect()
}

#[tokio::test]
async fn favorites_and_lists_are_private_bounded_and_never_grant_permission() {
    let h = Harness::new().await;
    let (a, b, c) = (user(&h).await, user(&h).await, user(&h).await);
    h.connected(&a.profile, &b.profile).await;
    h.connected(&c.profile, &b.profile).await;
    let ab = contact_id(&h, &a.profile, &b.profile).await;
    let cb = contact_id(&h, &c.profile, &b.profile).await;
    let key = Uuid::new_v4();
    let fav = h
        .state
        .set_favorite(&a.profile, ab, true, json!({"rank":3}), key)
        .await
        .unwrap();
    assert_eq!(fav["favorite"], true);
    // Exact replay returns the stored result; a different body with the same key conflicts.
    assert_eq!(
        h.state
            .set_favorite(&a.profile, ab, true, json!({"rank":3}), key)
            .await
            .unwrap(),
        fav
    );
    assert!(matches!(
        h.state
            .set_favorite(&a.profile, ab, true, json!({"rank":4}), key)
            .await,
        Err(Error::Idempotency)
    ));
    // Another owner's contact id is indistinguishable from a missing one.
    not_found(
        h.state
            .set_favorite(&a.profile, cb, true, json!({}), Uuid::new_v4())
            .await,
    );
    let list = h.state.list_favorites(&a.profile).await.unwrap();
    assert_eq!(list["data"].as_array().unwrap().len(), 1);
    assert_eq!(list["data"][0]["favoriteRank"], 3);
    assert_eq!(
        h.state.list_favorites(&b.profile).await.unwrap()["data"]
            .as_array()
            .unwrap()
            .len(),
        0
    );

    let created = h
        .state
        .create_list(&a.profile, json!({"name":"Family"}), Uuid::new_v4())
        .await
        .unwrap();
    let list_id = Uuid::parse_str(created["listId"].as_str().unwrap()).unwrap();
    assert!(matches!(
        h.state
            .create_list(&a.profile, json!({"name":"family "}), Uuid::new_v4())
            .await,
        Err(Error::Duplicate)
    ));
    assert!(matches!(
        h.state
            .create_list(&a.profile, json!({"name":"  "}), Uuid::new_v4())
            .await,
        Err(Error::Validation)
    ));
    let added = h
        .state
        .set_list_member(&a.profile, list_id, ab, true, Uuid::new_v4())
        .await
        .unwrap();
    assert_eq!(added["memberCount"], 1);
    // Re-adding is idempotent and does not bump the version.
    let again = h
        .state
        .set_list_member(&a.profile, list_id, ab, true, Uuid::new_v4())
        .await
        .unwrap();
    assert_eq!(again["version"], added["version"]);
    not_found(
        h.state
            .set_list_member(&a.profile, list_id, cb, true, Uuid::new_v4())
            .await,
    );
    not_found(h.state.get_list(&c.profile, list_id, 10, Uuid::nil()).await);
    // The owner-scoped trigger rejects a direct insert of a foreign contact.
    assert!(execute(&h.state.store.db,"INSERT INTO communication_contact_list_members(list_id,owner_profile_id,contact_id) VALUES($1,$2,$3)",vec![list_id.into(),a.profile.id.into(),cb.into()]).await.is_err());
    let stale = h
        .state
        .patch_list(
            &a.profile,
            list_id,
            json!({"expectedVersion":1,"name":"Kin"}),
            Uuid::new_v4(),
        )
        .await;
    assert!(matches!(stale, Err(Error::Version)));
    let version = h
        .state
        .get_list(&a.profile, list_id, 10, Uuid::nil())
        .await
        .unwrap()["version"]
        .as_i64()
        .unwrap();
    h.state
        .patch_list(
            &a.profile,
            list_id,
            json!({"expectedVersion":version,"name":"Kin"}),
            Uuid::new_v4(),
        )
        .await
        .unwrap();
    let deleted = h
        .state
        .delete_list(
            &a.profile,
            list_id,
            json!({"expectedVersion":version+1}),
            Uuid::new_v4(),
        )
        .await
        .unwrap();
    assert_eq!(deleted["deleted"], true);
    // Deleting a list never deletes the contact or its relationship.
    assert_eq!(
        h.state.contact_view(&a.profile, ab).await.unwrap()["relationship"],
        "CONNECTED"
    );
    // Favorites and lists grant nothing: B's privacy still decides.
    execute(&h.state.store.db, "UPDATE communication_privacy SET incoming_call_policy='NOBODY',version=version+1 WHERE profile_id=$1", vec![b.profile.id.into()]).await.unwrap();
    assert!(
        !h.state
            .check_policy(&h.scope, &check(Action::DirectCall, &a.profile, &b.profile))
            .await
            .unwrap()
            .allowed
    );
    // Limits are enforced.
    for i in 0..49 {
        h.state
            .create_list(&a.profile, json!({"name":format!("L{i}")}), Uuid::new_v4())
            .await
            .unwrap();
    }
    h.state
        .create_list(&a.profile, json!({"name":"L49"}), Uuid::new_v4())
        .await
        .unwrap();
    assert!(matches!(
        h.state
            .create_list(&a.profile, json!({"name":"over"}), Uuid::new_v4())
            .await,
        Err(Error::PolicyValidation)
    ));
    h.close().await;
}

#[tokio::test]
async fn chat_controls_require_membership_and_clear_for_me_is_monotonic() {
    let h = Harness::new().await;
    let (a, b, outsider) = (user(&h).await, user(&h).await, user(&h).await);
    let conv = conversation(&h, "DIRECT", &[&a, &b], 10);
    let m = Uuid::new_v4();
    let gone = Uuid::new_v4();
    {
        let mut w = h.world.write().unwrap();
        let c = w.conversations.get_mut(&conv).unwrap();
        c.messages.insert(m, (8, "VISIBLE".into()));
        c.messages.insert(gone, (9, "DELETED".into()));
    }
    let sa = h.delegated(&a.account);
    let so = h.delegated(&outsider.account);
    not_found(
        h.state
            .get_conversation_preferences(&so, &outsider.profile, conv)
            .await,
    );
    let defaults = h
        .state
        .get_conversation_preferences(&sa, &a.profile, conv)
        .await
        .unwrap();
    assert_eq!(
        (
            defaults["version"].clone(),
            defaults["notifications"].clone()
        ),
        (json!(0), json!("ALL"))
    );
    let put = h.state.put_conversation_preferences(&sa, &a.profile, conv, json!({"expectedVersion":0,"notifications":"MUTED","mutedUntil":(chrono::Utc::now()+chrono::Duration::hours(8)).to_rfc3339(),"mediaAutoDownload":"NEVER"}), Uuid::new_v4()).await.unwrap();
    assert_eq!(put["version"], 1);
    assert!(matches!(h.state.put_conversation_preferences(&sa, &a.profile, conv, json!({"expectedVersion":0,"notifications":"ALL","mutedUntil":null,"mediaAutoDownload":"WIFI"}), Uuid::new_v4()).await, Err(Error::Version)));
    assert!(matches!(h.state.put_conversation_preferences(&sa, &a.profile, conv, json!({"expectedVersion":1,"notifications":"ALL","mutedUntil":(chrono::Utc::now()+chrono::Duration::hours(1)).to_rfc3339(),"mediaAutoDownload":"WIFI"}), Uuid::new_v4()).await, Err(Error::Validation)));
    // Another member's preferences are independent.
    assert_eq!(
        h.state
            .get_conversation_preferences(&h.delegated(&b.account), &b.profile, conv)
            .await
            .unwrap()["notifications"],
        "ALL"
    );

    // Saved messages: references only; deleted/foreign messages are refused.
    let saved = h
        .state
        .save_message(
            &sa,
            &a.profile,
            json!({"conversationId":conv,"messageId":m}),
            Uuid::new_v4(),
        )
        .await
        .unwrap();
    not_found(
        h.state
            .save_message(
                &sa,
                &a.profile,
                json!({"conversationId":conv,"messageId":gone}),
                Uuid::new_v4(),
            )
            .await,
    );
    not_found(
        h.state
            .save_message(
                &so,
                &outsider.profile,
                json!({"conversationId":conv,"messageId":m}),
                Uuid::new_v4(),
            )
            .await,
    );
    let again = h
        .state
        .save_message(
            &sa,
            &a.profile,
            json!({"conversationId":conv,"messageId":m}),
            Uuid::new_v4(),
        )
        .await
        .unwrap();
    assert_eq!(again["savedId"], saved["savedId"]);
    let row: Value = one(
        &h.state.store.db,
        "SELECT to_jsonb(s) doc FROM communication_saved_messages s WHERE owner_profile_id=$1",
        vec![a.profile.id.into()],
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(
        row.as_object()
            .unwrap()
            .keys()
            .filter(|k| k.contains("text") || k.contains("body") || k.contains("cipher"))
            .count(),
        0
    );

    // Clear for me: bounded by the canonical sequence, monotonic and per user.
    assert!(matches!(
        h.state
            .clear_for_me(
                &sa,
                &a.profile,
                conv,
                json!({"throughSequence":11}),
                Uuid::new_v4()
            )
            .await,
        Err(Error::Validation)
    ));
    let cleared = h
        .state
        .clear_for_me(
            &sa,
            &a.profile,
            conv,
            json!({"throughSequence":9}),
            Uuid::new_v4(),
        )
        .await
        .unwrap();
    assert_eq!(cleared["clearedThroughSequence"], 9);
    let older = h
        .state
        .clear_for_me(
            &sa,
            &a.profile,
            conv,
            json!({"throughSequence":3}),
            Uuid::new_v4(),
        )
        .await
        .unwrap();
    assert_eq!(
        older["clearedThroughSequence"], 9,
        "an older request never moves the watermark back"
    );
    assert!(execute(&h.state.store.db, "UPDATE communication_conversation_watermarks SET cleared_through_sequence=1,version=version+1 WHERE owner_profile_id=$1", vec![a.profile.id.into()]).await.is_err());
    assert_eq!(h.state.watermark(&b.profile, conv).await.unwrap(), 0);
    // The saved item is now hidden for A only (reason given, reference kept).
    let list = h
        .state
        .list_saved(&sa, &a.profile, 30, Uuid::nil())
        .await
        .unwrap();
    assert_eq!(list["data"][0]["available"], false);
    assert_eq!(list["data"][0]["unavailableReason"], "CLEARED_FOR_ME");
    // Media index omits items at or before the watermark and never exposes uploader ids.
    let media = h
        .state
        .conversation_media(&sa, &a.profile, conv, 30, None)
        .await
        .unwrap();
    assert_eq!(media["data"].as_array().unwrap().len(), 0);
    assert!(!media
        .to_string()
        .contains(&a.profile.principal_id.to_string()));
    h.state
        .delete_saved(
            &a.profile,
            Uuid::parse_str(saved["savedId"].as_str().unwrap()).unwrap(),
            Uuid::new_v4(),
        )
        .await
        .unwrap();
    // Removal from the conversation revokes every control.
    h.world
        .write()
        .unwrap()
        .conversations
        .get_mut(&conv)
        .unwrap()
        .members
        .retain(|p| *p != a.profile.principal_id);
    not_found(
        h.state
            .clear_for_me(&sa, &a.profile, conv, json!({}), Uuid::new_v4())
            .await,
    );
    not_found(h.state.conversation_context(&sa, &a.profile, conv).await);
    h.close().await;
}

#[tokio::test]
async fn calls_launch_through_policy_once_and_project_safe_timeline_cards() {
    let h = Harness::new().await;
    let (a, b, stranger) = (user(&h).await, user(&h).await, user(&h).await);
    h.connected(&a.profile, &b.profile).await;
    let ab = contact_id(&h, &a.profile, &b.profile).await;
    let sa = h.delegated(&a.account);
    let ea = endpoint(&h, &a);
    let request = Uuid::new_v4();
    let launch = json!({"kind":"VIDEO","clientRequestId":request,"endpointId":ea});
    let first = h
        .state
        .call_contact(&sa, &a.profile, ab, launch.clone())
        .await
        .unwrap();
    let replay = h
        .state
        .call_contact(&sa, &a.profile, ab, launch.clone())
        .await
        .unwrap();
    assert_eq!(
        first["callId"], replay["callId"],
        "duplicate launches return the canonical call"
    );
    assert_eq!(replay["replayed"], true);
    assert_eq!(h.world.read().unwrap().launches.len(), 1);
    let links: Value = one(
        &h.state.store.db,
        "SELECT jsonb_build_object('n',count(*)) doc FROM communication_call_links",
        vec![],
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(links["n"], 1);
    // A contact-origin call creates no conversation and no timeline card.
    let cards: Value = one(
        &h.state.store.db,
        "SELECT jsonb_build_object('n',count(*)) doc FROM communication_timeline_entries",
        vec![],
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(cards["n"], 0);
    assert!(matches!(
        h.state
            .call_contact(
                &sa,
                &a.profile,
                ab,
                json!({"kind":"FAX","clientRequestId":Uuid::new_v4(),"endpointId":ea})
            )
            .await,
        Err(Error::Validation)
    ));
    // Blocks deny before Calling is asked.
    *h.blocked.write().unwrap() = true;
    assert!(matches!(
        h.state
            .call_contact(
                &sa,
                &a.profile,
                ab,
                json!({"kind":"VOICE","clientRequestId":Uuid::new_v4(),"endpointId":ea})
            )
            .await,
        Err(Error::ActionNotAllowed)
    ));
    *h.blocked.write().unwrap() = false;
    assert_eq!(h.world.read().unwrap().launches.len(), 1);

    // Conversation call: one card per member with a profile; outsiders see nothing.
    let conv = conversation(&h, "DIRECT", &[&a, &b], 4);
    let call = h
        .state
        .call_conversation(
            &sa,
            &a.profile,
            conv,
            json!({"kind":"VOICE","clientRequestId":Uuid::new_v4(),"endpointId":ea}),
        )
        .await
        .unwrap();
    let call_id = Uuid::parse_str(call["callId"].as_str().unwrap()).unwrap();
    not_found(h.state.call_conversation(&h.delegated(&stranger.account), &stranger.profile, conv, json!({"kind":"VOICE","clientRequestId":Uuid::new_v4(),"endpointId":endpoint(&h,&stranger)})).await);
    // The recipient's card resolves the canonical per-participant outcome from Calling.
    h.world
        .write()
        .unwrap()
        .calls
        .insert(call_id, ("NO_ANSWER".into(), true, 3, "MISSED".into()));
    let sb = h.delegated(&b.account);
    let tb = h
        .state
        .timeline(&sb, &b.profile, conv, &q(&[]))
        .await
        .unwrap();
    assert_eq!(tb["data"].as_array().unwrap().len(), 1);
    assert_eq!(tb["data"][0]["call"]["outcome"], "MISSED");
    assert_eq!(tb["data"][0]["call"]["missed"], true);
    assert_eq!(tb["stale"], false);
    let ta = h
        .state
        .timeline(&sa, &a.profile, conv, &q(&[]))
        .await
        .unwrap();
    assert_eq!(
        ta["data"][0]["call"]["missed"], false,
        "the caller's card is not a missed call"
    );
    // An out-of-order older version never overwrites the newer outcome.
    h.world
        .write()
        .unwrap()
        .calls
        .insert(call_id, ("RINGING".into(), false, 2, "RINGING".into()));
    execute(&h.state.store.db, "UPDATE communication_timeline_entries SET refreshed_at='epoch',data=data||'{\"terminal\":false}' WHERE viewer_profile_id=$1", vec![b.profile.id.into()]).await.unwrap();
    let tb = h
        .state
        .timeline(&sb, &b.profile, conv, &q(&[]))
        .await
        .unwrap();
    assert_eq!(tb["data"][0]["call"]["outcome"], "MISSED");
    // Calling outage: a card due for refresh is served and marked stale, never fabricated.
    execute(
        &h.state.store.db,
        "UPDATE communication_timeline_entries SET refreshed_at='epoch' WHERE viewer_profile_id=$1",
        vec![b.profile.id.into()],
    )
    .await
    .unwrap();
    h.world.write().unwrap().calling_down = true;
    let tb = h
        .state
        .timeline(&sb, &b.profile, conv, &q(&[]))
        .await
        .unwrap();
    assert_eq!(tb["stale"], true);
    h.world.write().unwrap().calling_down = false;
    // Calling's latest state for the conversation call is the missed outcome again.
    h.world
        .write()
        .unwrap()
        .calls
        .insert(call_id, ("NO_ANSWER".into(), true, 3, "MISSED".into()));
    // History exposes public identities only, never principal ids.
    let history = h
        .state
        .call_history(&sb, &b.profile, &q(&[("missed", "true")]))
        .await
        .unwrap();
    let text = history.to_string();
    assert!(
        !text.contains(&a.profile.principal_id.to_string())
            && !text.contains(&b.profile.principal_id.to_string())
    );
    assert_eq!(
        history["data"][0]["counterparts"][0]["cfMeetId"],
        json!(h.public_id(&a.profile).await)
    );
    // Call context is participant-only.
    not_found(
        h.state
            .call_context(&h.delegated(&stranger.account), &stranger.profile, call_id)
            .await,
    );
    // Clear for me hides the card for B only.
    h.state
        .clear_for_me(&sb, &b.profile, conv, json!({}), Uuid::new_v4())
        .await
        .unwrap();
    assert_eq!(
        h.state
            .timeline(&sb, &b.profile, conv, &q(&[]))
            .await
            .unwrap()["data"]
            .as_array()
            .unwrap()
            .len(),
        0
    );
    assert_eq!(
        h.state
            .timeline(&sa, &a.profile, conv, &q(&[]))
            .await
            .unwrap()["data"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    // Leaving the conversation removes timeline access.
    h.world
        .write()
        .unwrap()
        .conversations
        .get_mut(&conv)
        .unwrap()
        .members
        .retain(|p| *p != b.profile.principal_id);
    not_found(h.state.timeline(&sb, &b.profile, conv, &q(&[])).await);
    assert_eq!(
        h.state.activity(&sb, &b.profile, &q(&[])).await.unwrap()["data"]
            .as_array()
            .unwrap()
            .len(),
        0
    );
    h.close().await;
}

#[tokio::test]
async fn devices_link_only_owned_references_and_revocation_is_terminal() {
    let h = Harness::new().await;
    let (a, b) = (user(&h).await, user(&h).await);
    let sa = h.delegated(&a.account);
    let own = Uuid::new_v4();
    let foreign = Uuid::new_v4();
    {
        let mut w = h.world.write().unwrap();
        w.devices
            .insert(own, (a.profile.principal_id, "ACTIVE".into()));
        w.devices
            .insert(foreign, (b.profile.principal_id, "ACTIVE".into()));
    }
    let ep = endpoint(&h, &a);
    not_found(
        h.state
            .link_device(
                &sa,
                &a.profile,
                json!({"platform":"ANDROID","chatDeviceId":foreign}),
                Uuid::new_v4(),
            )
            .await,
    );
    let linked = h
        .state
        .link_device(
            &sa,
            &a.profile,
            json!({"platform":"ANDROID","chatDeviceId":own,"callEndpointId":ep}),
            Uuid::new_v4(),
        )
        .await
        .unwrap();
    assert_eq!(linked["linked"], true);
    let device = Uuid::parse_str(linked["device"]["deviceId"].as_str().unwrap()).unwrap();
    let again = h
        .state
        .link_device(
            &sa,
            &a.profile,
            json!({"platform":"ANDROID","chatDeviceId":own}),
            Uuid::new_v4(),
        )
        .await
        .unwrap();
    assert_eq!(again["linked"], false);
    let push = h
        .state
        .device_push(
            &sa,
            &a.profile,
            device,
            Some(json!({"platform":"FCM","token":"opaque-test-token"})),
            Uuid::new_v4(),
        )
        .await
        .unwrap();
    assert_eq!(push["device"]["push"]["registered"], true);
    let stored: Value = one(
        &h.state.store.db,
        "SELECT to_jsonb(d) doc FROM communication_devices d WHERE id=$1",
        vec![device.into()],
    )
    .await
    .unwrap()
    .unwrap();
    assert!(
        !stored.to_string().contains("opaque-test-token"),
        "push tokens are never stored by the Hub"
    );
    not_found(h.state.device_sync_state(&b.profile, device, None).await);
    let head = h.state.sync_head(&a.profile).await.unwrap().0;
    assert!(matches!(
        h.state
            .device_sync_state(
                &a.profile,
                device,
                Some((json!({"acknowledgedPosition":head+1}), Uuid::new_v4()))
            )
            .await,
        Err(Error::Validation)
    ));
    let acked = h
        .state
        .device_sync_state(
            &a.profile,
            device,
            Some((json!({"acknowledgedPosition":head}), Uuid::new_v4())),
        )
        .await
        .unwrap();
    assert_eq!(acked["acknowledgedPosition"], head);
    let back = h
        .state
        .device_sync_state(
            &a.profile,
            device,
            Some((json!({"acknowledgedPosition":0}), Uuid::new_v4())),
        )
        .await
        .unwrap();
    assert_eq!(
        back["acknowledgedPosition"], head,
        "acknowledgements never move backwards"
    );
    let revoked = h
        .state
        .revoke_device(
            &sa,
            &a.profile,
            device,
            json!({"reason":"COMPROMISED"}),
            Uuid::new_v4(),
        )
        .await
        .unwrap();
    assert_eq!(revoked["device"]["state"], "REVOKED");
    {
        let w = h.world.read().unwrap();
        assert_eq!(
            (w.revoked_devices.clone(), w.revoked_endpoints.clone()),
            (vec![own], vec![ep])
        );
    }
    assert!(execute(&h.state.store.db, "UPDATE communication_devices SET state='ACTIVE',revoked_at=NULL,version=version+1 WHERE id=$1", vec![device.into()]).await.is_err());
    not_found(
        h.state
            .device_push(&sa, &a.profile, device, None, Uuid::new_v4())
            .await,
    );
    h.close().await;
}

#[tokio::test]
async fn meetings_are_linked_by_reference_and_only_shared_by_owners() {
    let h = Harness::new().await;
    let (a, b, outsider) = (user(&h).await, user(&h).await, user(&h).await);
    let conv = conversation(&h, "GROUP", &[&a, &b], 2);
    let sa = h.delegated(&a.account);
    let request = Uuid::new_v4();
    let instant = h
        .state
        .instant_meeting(
            &sa,
            &a.profile,
            conv,
            json!({"clientRequestId":request,"title":"Standup"}),
        )
        .await
        .unwrap();
    let replay = h
        .state
        .instant_meeting(
            &sa,
            &a.profile,
            conv,
            json!({"clientRequestId":request,"title":"Standup"}),
        )
        .await
        .unwrap();
    assert_eq!(instant["linkId"], replay["linkId"]);
    not_found(
        h.state
            .instant_meeting(
                &h.delegated(&outsider.account),
                &outsider.profile,
                conv,
                json!({"clientRequestId":Uuid::new_v4()}),
            )
            .await,
    );
    // B cannot re-share A's meeting; A can share it into another chat.
    let code = instant["meetingCode"].as_str().unwrap().to_owned();
    let sb = h.delegated(&b.account);
    assert!(matches!(
        h.state
            .share_meeting(
                &sb,
                &b.profile,
                conv,
                json!({"clientRequestId":Uuid::new_v4(),"meetingCode":code})
            )
            .await,
        Err(Error::ActionNotAllowed)
    ));
    let other = conversation(&h, "DIRECT", &[&a, &b], 0);
    h.state
        .share_meeting(
            &sa,
            &a.profile,
            other,
            json!({"clientRequestId":Uuid::new_v4(),"meetingCode":code}),
        )
        .await
        .unwrap();
    not_found(
        h.state
            .share_meeting(
                &sa,
                &a.profile,
                other,
                json!({"clientRequestId":Uuid::new_v4(),"meetingCode":"zzz-unknown"}),
            )
            .await,
    );
    let listed = h
        .state
        .conversation_meetings(&sb, &b.profile, conv, 10, Uuid::nil())
        .await
        .unwrap();
    assert_eq!(listed["data"].as_array().unwrap().len(), 1);
    assert_eq!(listed["data"][0]["linkedByViewer"], false);
    let tb = h
        .state
        .timeline(&sb, &b.profile, conv, &q(&[]))
        .await
        .unwrap();
    assert_eq!(tb["data"][0]["kind"], "MEETING_INVITATION");
    h.close().await;
}

#[tokio::test]
async fn change_feed_is_monotonic_bounded_and_reports_resync_instead_of_gaps() {
    let h = Harness::new().await;
    let (a, b) = (user(&h).await, user(&h).await);
    h.connected(&a.profile, &b.profile).await;
    let ab = contact_id(&h, &a.profile, &b.profile).await;
    for _ in 0..3 {
        h.state
            .create_list(
                &a.profile,
                json!({"name":format!("x{}",Uuid::new_v4().simple())}),
                Uuid::new_v4(),
            )
            .await
            .unwrap();
    }
    h.state
        .set_favorite(&a.profile, ab, true, json!({}), Uuid::new_v4())
        .await
        .unwrap();
    let all = h.state.changes(&a.profile, &q(&[]), false).await.unwrap();
    let positions: Vec<i64> = all["data"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["position"].as_i64().unwrap())
        .collect();
    assert_eq!(positions.len(), 4);
    assert!(positions.windows(2).all(|w| w[0] < w[1]));
    assert_eq!(
        h.state.changes(&b.profile, &q(&[]), false).await.unwrap()["data"]
            .as_array()
            .unwrap()
            .len(),
        0
    );
    let filtered = h
        .state
        .changes(&a.profile, &q(&[("domains", "CONTACT")]), true)
        .await
        .unwrap();
    assert_eq!(filtered["data"].as_array().unwrap().len(), 1);
    assert!(matches!(
        h.state
            .changes(&a.profile, &q(&[("domains", "SECRETS")]), true)
            .await,
        Err(Error::Validation)
    ));
    assert!(matches!(
        h.state
            .changes(&a.profile, &q(&[("after", "999999999")]), false)
            .await,
        Err(Error::Validation)
    ));
    let page = h
        .state
        .changes(&a.profile, &q(&[("limit", "2")]), false)
        .await
        .unwrap();
    assert_eq!(page["hasMore"], true);
    assert_eq!(page["nextPosition"], positions[1]);
    // Pruned history makes an old cursor resynchronize instead of silently skipping.
    execute(
        &h.state.store.db,
        "DELETE FROM communication_changes WHERE profile_id=$1 AND seq<=$2",
        vec![a.profile.id.into(), positions[2].into()],
    )
    .await
    .unwrap();
    let resync = h
        .state
        .changes(
            &a.profile,
            &q(&[("after", &positions[0].to_string())]),
            false,
        )
        .await
        .unwrap();
    assert_eq!(resync["resyncRequired"], true);
    let boot = h
        .state
        .bootstrap(&h.delegated(&a.account), &a.profile)
        .await
        .unwrap();
    assert_eq!(boot["counts"]["contactLists"], 3);
    assert_eq!(boot["counts"]["favorites"], 1);
    h.close().await;
}

#[tokio::test]
async fn private_owner_and_operator_routes_are_scoped_deduplicated_and_audited() {
    let h = Harness::new().await;
    let (a, b) = (user(&h).await, user(&h).await);
    h.connected(&a.profile, &b.profile).await;
    let event = json!({"eventId":Uuid::new_v4(),"eventType":"call.ended","resourceId":Uuid::new_v4(),"sourceVersion":4});
    let (status, body) = h
        .http(
            &h.calling_signer,
            "/internal/v1/communications/events/calling",
            event.clone(),
            h.scope.clone(),
        )
        .await;
    assert_eq!(
        (status.as_u16(), body["applied"].clone()),
        (200, json!(true))
    );
    let (_, dup) = h
        .http(
            &h.calling_signer,
            "/internal/v1/communications/events/calling",
            event.clone(),
            h.scope.clone(),
        )
        .await;
    assert_eq!(dup["duplicate"], true);
    let mut changed = event.clone();
    changed["sourceVersion"] = json!(5);
    let (status, _) = h
        .http(
            &h.calling_signer,
            "/internal/v1/communications/events/calling",
            changed,
            h.scope.clone(),
        )
        .await;
    assert_eq!(
        status.as_u16(),
        409,
        "a reused event id with a different payload is refused"
    );
    let (status, _) = h.http(&h.calling_signer, "/internal/v1/communications/events/meetings", json!({"eventId":Uuid::new_v4(),"eventType":"meeting.ended","resourceId":Uuid::new_v4(),"sourceVersion":1}), h.scope.clone()).await;
    assert_eq!(
        status.as_u16(),
        403,
        "Calling cannot publish Meetings events"
    );
    let (status, _) = h.http(&h.calling_signer, "/internal/v1/communications/events/calling", json!({"eventId":Uuid::new_v4(),"eventType":"message.created","resourceId":Uuid::new_v4(),"sourceVersion":1}), h.scope.clone()).await;
    assert_eq!(status.as_u16(), 400);
    // Account (the user-forwarding caller) holds no publish or operator permission.
    let (status, _) = h
        .http(
            &h.account_signer,
            "/internal/v1/communications/events/calling",
            event,
            h.scope.clone(),
        )
        .await;
    assert_eq!(status.as_u16(), 403);
    let (status, _) = h
        .http(
            &h.account_signer,
            "/internal/v1/communications/admin/suspend",
            json!({"cfMeetId":h.public_id(&b.profile).await,"active":true,"reason":"ABUSE"}),
            h.scope.clone(),
        )
        .await;
    assert_eq!(status.as_u16(), 403);
    // Operator hold denies personal communication until released.
    let (status, _) = h
        .http(
            &h.operator,
            "/internal/v1/communications/admin/suspend",
            json!({"cfMeetId":h.public_id(&b.profile).await,"active":true,"reason":"ABUSE"}),
            h.scope.clone(),
        )
        .await;
    assert_eq!(status.as_u16(), 200);
    assert!(
        !h.state
            .check_policy(&h.scope, &check(Action::DirectCall, &a.profile, &b.profile))
            .await
            .unwrap()
            .allowed
    );
    h.http(
        &h.operator,
        "/internal/v1/communications/admin/suspend",
        json!({"cfMeetId":h.public_id(&b.profile).await,"active":false,"reason":"ABUSE"}),
        h.scope.clone(),
    )
    .await;
    assert!(
        h.state
            .check_policy(&h.scope, &check(Action::DirectCall, &a.profile, &b.profile))
            .await
            .unwrap()
            .allowed
    );
    let audit: Value = one(&h.state.store.db, "SELECT jsonb_build_object('n',count(*)) doc FROM communication_audit WHERE operation LIKE 'OPERATOR_HOLD_%'", vec![]).await.unwrap().unwrap();
    assert_eq!(audit["n"], 2);
    // Phone verification/recovery stay unavailable; metrics carry no personal data.
    let (status, body) = h
        .http(
            &h.operator,
            "/internal/v1/communications/admin/phone-recovery",
            json!({}),
            h.scope.clone(),
        )
        .await;
    assert_eq!(
        (status.as_u16(), body["error"]["code"].clone()),
        (503, json!("DEPENDENCY_NOT_CONFIGURED"))
    );
    let (status, body) = h
        .http(
            &h.operator,
            "/internal/v1/communications/reconciliation/range",
            json!({"from":"2026-11-01T00:00:00Z","to":"2026-11-06T00:00:00Z"}),
            h.scope.clone(),
        )
        .await;
    assert_eq!(
        (status.as_u16(), body["requeuedEvents"].clone()),
        (200, json!(0))
    );
    // Global message-notification switch belongs to the recipient.
    h.state.put_notification_preferences(&b.profile, json!({"expectedVersion":0,"directMessages":false,"groupMessages":true,"incomingCalls":true,"missedCalls":true,"contactRequests":true,"groupInvitations":true,"meetingInvitations":true}), Uuid::new_v4()).await.unwrap();
    assert!(
        !h.state
            .check_policy(
                &h.scope,
                &check(Action::MessageNotification, &a.profile, &b.profile)
            )
            .await
            .unwrap()
            .allowed
    );
    assert!(
        h.state
            .check_policy(
                &h.scope,
                &check(Action::GroupNotification, &a.profile, &b.profile)
            )
            .await
            .unwrap()
            .allowed
    );
    assert!(
        h.state
            .check_policy(
                &h.scope,
                &check(Action::MessageNotification, &b.profile, &a.profile)
            )
            .await
            .unwrap()
            .allowed
    );
    // Per-conversation mute belongs to the recipient and applies only to that chat.
    h.state.put_notification_preferences(&b.profile, json!({"expectedVersion":1,"directMessages":true,"groupMessages":true,"incomingCalls":true,"missedCalls":true,"contactRequests":true,"groupInvitations":true,"meetingInvitations":true}), Uuid::new_v4()).await.unwrap();
    let chat = conversation(&h, "DIRECT", &[&a, &b], 1);
    let other_chat = conversation(&h, "DIRECT", &[&a, &b], 1);
    h.state.put_conversation_preferences(&h.delegated(&b.account), &b.profile, chat, json!({"expectedVersion":0,"notifications":"MUTED","mutedUntil":null,"mediaAutoDownload":"WIFI"}), Uuid::new_v4()).await.unwrap();
    let mut in_chat = check(Action::MessageNotification, &a.profile, &b.profile);
    in_chat.conversation_id = Some(chat);
    assert!(
        !h.state
            .check_policy(&h.scope, &in_chat)
            .await
            .unwrap()
            .allowed
    );
    in_chat.conversation_id = Some(other_chat);
    assert!(
        h.state
            .check_policy(&h.scope, &in_chat)
            .await
            .unwrap()
            .allowed
    );
    assert!(
        h.state
            .check_policy(
                &h.scope,
                &check(Action::MessageNotification, &a.profile, &b.profile)
            )
            .await
            .unwrap()
            .allowed
    );
    h.close().await;
}
