//! Notification Service roadmap N10 (Document 5 §6, Phase 4.2; Document 7 F-21 / M-09):
//! the Hub side of the preference move.
//!
//! * the owner API the service copies from (`exportNotificationPreferences`,
//!   `listNotificationPreferenceChanges`; contract `communications/notification-owner/v1`,
//!   `communications-preferences.export`, Notification Service only, first-party scope);
//! * the explicit switch `communication_preference_authority` (HUB default / FREEZE /
//!   SERVICE): HUB is today's behaviour, FREEZE refuses preference writes with 503 +
//!   Retry-After, SERVICE turns the preference endpoints into facades over U6 / U7 with
//!   identical client responses, and a service failure is a clear error with nothing
//!   written locally.
//!
//! The Notification Service is the harness's in-memory I2 mock (U6 / U7 rules).
mod common;
use axum::{http::StatusCode, response::IntoResponse};
use cf_communications_policy::Action;
use common::*;
use communications::{
    config::Config,
    error::Error,
    model::{Profile, VerifiedAccount},
    store::one,
    State,
};
use serde_json::{json, Value};
use std::{collections::BTreeMap, sync::Arc};
use uuid::Uuid;

const EXPORT: &str = "/internal/v1/communications/notification-preferences/export";
const CHANGES: &str = "/internal/v1/communications/notification-preferences/changes";

struct User {
    account: VerifiedAccount,
    profile: Profile,
}
async fn user(h: &Harness) -> User {
    let account = h.account();
    let profile = h.profile(&account).await;
    User { account, profile }
}
fn conversation(h: &Harness, members: &[&User]) -> Uuid {
    let id = Uuid::new_v4();
    h.world.write().unwrap().conversations.insert(
        id,
        MockConversation {
            context: "DIRECT".into(),
            members: members.iter().map(|u| u.profile.principal_id).collect(),
            last_sequence: 1,
            messages: BTreeMap::new(),
        },
    );
    id
}
async fn contact_id(h: &Harness, owner: &Profile, other: &Profile) -> Uuid {
    let v: Value = one(&h.state.store.db,
        "SELECT jsonb_build_object('id',a.id) doc FROM communication_contact_aliases a JOIN communication_connections c ON c.id=a.connection_id WHERE a.owner_profile_id=$1 AND (c.left_profile_id=$2 OR c.right_profile_id=$2)",
        vec![owner.id.into(), other.id.into()]).await.unwrap().unwrap();
    Uuid::parse_str(v["id"].as_str().unwrap()).unwrap()
}
async fn doc(h: &Harness, sql: &str, values: Vec<sea_orm::Value>) -> Value {
    one(&h.state.store.db, sql, values)
        .await
        .unwrap()
        .unwrap_or(Value::Null)
}
fn prefs(version: i64, off: &[&str]) -> Value {
    let mut v = json!({ "expectedVersion": version });
    for k in [
        "directMessages",
        "groupMessages",
        "incomingCalls",
        "missedCalls",
        "contactRequests",
        "groupInvitations",
        "meetingInvitations",
    ] {
        v[k] = json!(!off.contains(&k));
    }
    v
}
fn ns_calls(h: &Harness) -> usize {
    h.world.read().unwrap().notifications.calls.len()
}
async fn export_all(h: &Harness, kind: &str) -> Vec<Value> {
    let mut items = Vec::new();
    let mut after = Value::Null;
    loop {
        let (status, v) = h
            .http(
                &h.notifications,
                EXPORT,
                json!({"kind":kind,"after":after,"limit":1}),
                h.scope.clone(),
            )
            .await;
        assert_eq!(status, StatusCode::OK, "{v}");
        assert_eq!(v["kind"], kind);
        assert!(v["feedPosition"].as_i64().unwrap() >= v["settledPosition"].as_i64().unwrap());
        items.extend(v["items"].as_array().unwrap().iter().cloned());
        if v["nextCursor"].is_null() {
            return items;
        }
        after = v["nextCursor"].clone();
    }
}

/// a: account switches (version 2), a muted conversation; b: a MENTIONS conversation;
/// a and b connected, a's alias of b MUTED.
async fn seeded(h: &Harness) -> (User, User, Uuid) {
    let (a, b) = (user(h).await, user(h).await);
    h.state
        .put_notification_preferences(&a.profile, prefs(0, &[]), Uuid::new_v4())
        .await
        .unwrap();
    h.state
        .put_notification_preferences(&a.profile, prefs(1, &["directMessages"]), Uuid::new_v4())
        .await
        .unwrap();
    let conv = conversation(h, &[&a, &b]);
    // "Mentions only" needs a MANAGED conversation (K11).
    h.world.write().unwrap().managed.insert(conv);
    let until = (chrono::Utc::now() + chrono::Duration::hours(8)).to_rfc3339();
    h.state.put_conversation_preferences(&h.delegated(&a.account), &a.profile, conv,
        json!({"expectedVersion":0,"notifications":"MUTED","mutedUntil":until,"mediaAutoDownload":"NEVER"}), Uuid::new_v4()).await.unwrap();
    h.state.put_conversation_preferences(&h.delegated(&b.account), &b.profile, conv,
        json!({"expectedVersion":0,"notifications":"MENTIONS","mutedUntil":null,"mediaAutoDownload":"WIFI"}), Uuid::new_v4()).await.unwrap();
    h.connected(&a.profile, &b.profile).await;
    let ab = contact_id(h, &a.profile, &b.profile).await;
    h.state
        .patch_contact(
            &a.profile,
            ab,
            json!({"expectedVersion":1,"notifications":"MUTED"}),
            Uuid::new_v4(),
        )
        .await
        .unwrap();
    (a, b, conv)
}

#[tokio::test]
async fn the_hub_exports_every_preference_row_with_its_version_to_the_service_only() {
    let h = Harness::new().await;
    let (a, b, conv) = seeded(&h).await;
    let body = json!({"kind":"ACCOUNT","after":null,"limit":10});
    // Only the Notification Service, with the permission and the first-party scope.
    for peer in [&h.account_signer, &h.calling_signer, &h.operator] {
        let (status, v) = h.http(peer, EXPORT, body.clone(), h.scope.clone()).await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{v}");
        let (status, _) = h
            .http(
                peer,
                CHANGES,
                json!({"after":0,"limit":10}),
                h.scope.clone(),
            )
            .await;
        assert_eq!(status, StatusCode::FORBIDDEN);
    }
    let (status, headers) = h
        .http_full(&h.notifications, EXPORT, body.clone(), h.scope.clone())
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(headers.get("cache-control").unwrap(), "no-store");
    for bad in [
        json!({"kind":"ACCOUNT","after":null,"limit":0}),
        json!({"kind":"ACCOUNT","after":null,"limit":501}),
        json!({"kind":"ACCOUNT","after":"not-a-cursor","limit":5}),
        json!({"kind":"CONVERSATION","after":Uuid::new_v4(),"limit":5}),
        json!({"kind":"SECURITY","after":null,"limit":5}),
    ] {
        let (status, v) = h
            .http(&h.notifications, EXPORT, bad.clone(), h.scope.clone())
            .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{bad} {v}");
    }

    let accounts = export_all(&h, "ACCOUNT").await;
    assert_eq!(
        accounts,
        vec![
            json!({"accountRef":a.account.account_reference,"version":2,"directMessages":false,"groupMessages":true,
            "incomingCalls":true,"missedCalls":true,"contactRequests":true,"groupInvitations":true,"meetingInvitations":true})
        ]
    );
    let conversations = export_all(&h, "CONVERSATION").await;
    assert_eq!(
        conversations.len(),
        2,
        "paged one by one with a composite cursor"
    );
    let mine = conversations
        .iter()
        .find(|c| c["accountRef"] == a.account.account_reference)
        .unwrap();
    assert_eq!(
        (
            mine["conversationId"].clone(),
            mine["notifications"].clone(),
            mine["version"].clone()
        ),
        (json!(conv), json!("MUTED"), json!(1))
    );
    assert!(mine["mutedUntil"].is_string());
    let theirs = conversations
        .iter()
        .find(|c| c["accountRef"] == b.account.account_reference)
        .unwrap();
    assert_eq!(
        (
            theirs["notifications"].clone(),
            theirs["mutedUntil"].clone()
        ),
        (json!("MENTIONS"), Value::Null)
    );
    let mut contacts = export_all(&h, "CONTACT").await;
    contacts.sort_by_key(|c| c["notifications"].as_str().unwrap().to_owned());
    assert_eq!(
        contacts,
        vec![
            json!({"accountRef":b.account.account_reference,"contactRef":a.account.account_reference,"notifications":"ALL","version":1}),
            json!({"accountRef":a.account.account_reference,"contactRef":b.account.account_reference,"notifications":"MUTED","version":2}),
        ]
    );
    // Another scope sees nothing.
    let other = cf_service_auth::DelegatedPlatformContext::new(
        Uuid::new_v4(),
        Uuid::new_v4(),
        Uuid::new_v4(),
        cf_service_auth::ApplicationEnvironmentKind::Test,
        Uuid::new_v4(),
    );
    let (status, v) = h.http(&h.notifications, EXPORT, body.clone(), other).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(v["items"], json!([]));

    // The change feed of the three domains, each entry resolved to the CURRENT value.
    let (status, v) = h
        .http(
            &h.notifications,
            CHANGES,
            json!({"after":0,"limit":500}),
            h.scope.clone(),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{v}");
    let items = v["items"].as_array().unwrap();
    let account_entries: Vec<&Value> = items.iter().filter(|i| i["kind"] == "ACCOUNT").collect();
    assert_eq!(account_entries.len(), 2, "both writes are listed");
    for e in &account_entries {
        assert_eq!(
            e["preference"]["version"], 2,
            "the current value, not the historical one"
        );
        assert_eq!(e["preference"]["directMessages"], false);
    }
    assert_eq!(
        items.iter().filter(|i| i["kind"] == "CONVERSATION").count(),
        2
    );
    let contact_entries: Vec<&Value> = items.iter().filter(|i| i["kind"] == "CONTACT").collect();
    assert!(contact_entries
        .iter()
        .any(|e| e["accountRef"] == a.account.account_reference
            && e["contactRef"] == b.account.account_reference
            && e["preference"]["notifications"] == "MUTED"));
    assert_eq!(v["more"], false);
    assert_eq!(v["nextAfter"], items.last().unwrap()["seq"]);
    // Positions: nothing is settled until it is older than the settle window.
    assert_eq!(v["settledPosition"], 0);
    h.state
        .store
        .db
        .execute_unprepared(
            "UPDATE communication_changes SET created_at = now() - interval '2 minutes'",
        )
        .await
        .unwrap();
    let (_, v) = h
        .http(
            &h.notifications,
            CHANGES,
            json!({"after":0,"limit":1}),
            h.scope.clone(),
        )
        .await;
    assert_eq!(v["settledPosition"], v["feedPosition"]);
    assert_eq!(v["more"], true);
    assert_eq!(v["items"].as_array().unwrap().len(), 1);

    // FREEZE (the final pass) still exports; SERVICE refuses: the Hub rows are a mirror.
    h.set_preference_authority("FREEZE").await;
    let (status, _) = h
        .http(&h.notifications, EXPORT, body.clone(), h.scope.clone())
        .await;
    assert_eq!(status, StatusCode::OK);
    h.set_preference_authority("SERVICE").await;
    for (path, b) in [
        (EXPORT, body.clone()),
        (CHANGES, json!({"after":0,"limit":5})),
    ] {
        let (status, v) = h.http(&h.notifications, path, b, h.scope.clone()).await;
        assert_eq!(status, StatusCode::CONFLICT, "{v}");
        assert_eq!(v["error"]["code"], "PREFERENCE_AUTHORITY_MOVED");
    }
    h.close().await;
}

use sea_orm::ConnectionTrait;

#[tokio::test]
async fn the_switch_defaults_to_hub_and_changes_nothing() {
    let h = Harness::new().await;
    let row = doc(&h, "SELECT jsonb_build_object('mode',mode,'version',version) doc FROM communication_preference_authority", vec![]).await;
    assert_eq!(row, json!({"mode":"HUB","version":1}));
    let (a, _b, conv) = seeded(&h).await;
    let got = h
        .state
        .get_notification_preferences(&a.profile)
        .await
        .unwrap();
    assert_eq!(got["version"], 2);
    assert_eq!(got["directMessages"], false);
    assert_eq!(got["securityEvents"], true);
    let c = h
        .state
        .get_conversation_preferences(&h.delegated(&a.account), &a.profile, conv)
        .await
        .unwrap();
    assert_eq!(c["notifications"], "MUTED");
    assert_eq!(
        ns_calls(&h),
        0,
        "HUB mode never calls the Notification Service"
    );
    h.close().await;
}

#[tokio::test]
async fn service_mode_reads_and_writes_through_the_service_with_identical_responses() {
    let h = Harness::new().await;
    let (a, b, conv) = seeded(&h).await;
    let sa = h.delegated(&a.account);
    let account_hub = h
        .state
        .get_notification_preferences(&a.profile)
        .await
        .unwrap();
    let conv_hub = h
        .state
        .get_conversation_preferences(&sa, &a.profile, conv)
        .await
        .unwrap();
    // The state the N10 copy produces: the Hub values with the Hub versions.
    let until = chrono::DateTime::parse_from_rfc3339(conv_hub["mutedUntil"].as_str().unwrap())
        .unwrap()
        .with_timezone(&chrono::Utc);
    h.world.write().unwrap().notifications.docs.insert(
        a.account.account_reference.clone(),
        NsDoc {
            version: 2,
            categories: BTreeMap::from([("directMessages".into(), false)]),
            conversations: BTreeMap::from([(
                conv.to_string(),
                json!({"notifications":"MUTED","mutedUntil":until}),
            )]),
            contacts: BTreeMap::from([(b.account.account_reference.clone(), "MUTED".into())]),
        },
    );
    h.set_preference_authority("SERVICE").await;

    // Reads: byte-identical client responses, now answered by the service.
    assert_eq!(
        h.state
            .get_notification_preferences(&a.profile)
            .await
            .unwrap(),
        account_hub
    );
    assert_eq!(
        h.state
            .get_conversation_preferences(&sa, &a.profile, conv)
            .await
            .unwrap(),
        conv_hub
    );
    assert!(ns_calls(&h) >= 2);
    // A value only the service holds (e.g. written through U7 directly) is what clients see.
    h.world
        .write()
        .unwrap()
        .notifications
        .docs
        .get_mut(&a.account.account_reference)
        .unwrap()
        .categories
        .insert("missedCalls".into(), false);
    assert_eq!(
        h.state
            .get_notification_preferences(&a.profile)
            .await
            .unwrap()["missedCalls"],
        false
    );
    // Embedded aggregates use the service too.
    let boot = h.state.bootstrap(&sa, &a.profile).await.unwrap();
    assert_eq!(boot["notificationPreferences"]["missedCalls"], false);

    // Account write: U7 decides with the client's version; the Hub mirrors it.
    let key = Uuid::new_v4();
    let calls = ns_calls(&h);
    let put = h
        .state
        .put_notification_preferences(
            &a.profile,
            prefs(2, &["directMessages", "groupMessages"]),
            key,
        )
        .await
        .unwrap();
    assert_eq!(
        put,
        json!({"schemaVersion":1,"version":3,"directMessages":false,"groupMessages":false,"incomingCalls":true,
        "missedCalls":true,"contactRequests":true,"groupInvitations":true,"meetingInvitations":true,"securityEvents":true,
        "payloadPolicy":"ROUTING_METADATA_ONLY"})
    );
    {
        let w = h.world.read().unwrap();
        let call = w.notifications.calls.last().unwrap();
        assert_eq!(call["method"], "PUT");
        assert_eq!(call["path"], "/preferences");
        assert_eq!(call["account"], a.account.account_reference);
        assert_eq!(call["idempotencyKey"], key.to_string());
        assert_eq!(call["body"]["expectedVersion"], 2);
        assert_eq!(call["body"]["categories"].as_object().unwrap().len(), 7);
        assert!(call["body"].get("securityEvents").is_none());
        assert!(!w.notifications.docs[&a.account.account_reference].categories["groupMessages"]);
    }
    assert_eq!(ns_calls(&h), calls + 1);
    let mirror = doc(&h, "SELECT jsonb_build_object('d',direct_messages,'g',group_messages,'v',version) doc FROM communication_notification_preferences WHERE profile_id=$1", vec![a.profile.id.into()]).await;
    assert_eq!(mirror, json!({"d":false,"g":false,"v":3}));
    // The exact replay is answered by the Hub without a second write.
    assert_eq!(
        h.state
            .put_notification_preferences(
                &a.profile,
                prefs(2, &["directMessages", "groupMessages"]),
                key
            )
            .await
            .unwrap(),
        put
    );
    assert_eq!(ns_calls(&h), calls + 1);
    // A stale version is the service's 409, and nothing changes in the Hub.
    assert!(matches!(
        h.state
            .put_notification_preferences(&a.profile, prefs(2, &[]), Uuid::new_v4())
            .await,
        Err(Error::Version)
    ));
    assert_eq!(doc(&h, "SELECT jsonb_build_object('v',version) doc FROM communication_notification_preferences WHERE profile_id=$1", vec![a.profile.id.into()]).await["v"], 3);

    // Conversation write: the Hub row version decides; the level goes to the service.
    let put = h.state.put_conversation_preferences(&sa, &a.profile, conv,
        json!({"expectedVersion":1,"notifications":"ALL","mutedUntil":null,"mediaAutoDownload":"ALWAYS"}), Uuid::new_v4()).await.unwrap();
    assert_eq!(
        (
            put["version"].clone(),
            put["notifications"].clone(),
            put["mediaAutoDownload"].clone()
        ),
        (json!(2), json!("ALL"), json!("ALWAYS"))
    );
    assert_eq!(
        h.world.read().unwrap().notifications.docs[&a.account.account_reference].conversations
            [&conv.to_string()]["notifications"],
        "ALL"
    );
    assert_eq!(
        h.state
            .get_conversation_preferences(&sa, &a.profile, conv)
            .await
            .unwrap(),
        put
    );
    let until = (chrono::Utc::now() + chrono::Duration::days(2)).to_rfc3339();
    let put = h.state.put_conversation_preferences(&sa, &a.profile, conv,
        json!({"expectedVersion":2,"notifications":"MUTED","mutedUntil":until,"mediaAutoDownload":"ALWAYS"}), Uuid::new_v4()).await.unwrap();
    assert_eq!(
        h.state
            .get_conversation_preferences(&sa, &a.profile, conv)
            .await
            .unwrap(),
        put,
        "mutedUntil is formatted exactly like the Hub row"
    );
    assert!(matches!(
        h.state.put_conversation_preferences(&sa, &a.profile, conv,
            json!({"expectedVersion":1,"notifications":"ALL","mutedUntil":null,"mediaAutoDownload":"WIFI"}), Uuid::new_v4()).await,
        Err(Error::Version)
    ));

    // Contact level: forwarded; alias-only edits stay in the Hub.
    let ab = contact_id(&h, &a.profile, &b.profile).await;
    let patched = h
        .state
        .patch_contact(
            &a.profile,
            ab,
            json!({"expectedVersion":2,"notifications":"ALL"}),
            Uuid::new_v4(),
        )
        .await
        .unwrap();
    assert_eq!(patched["notifications"], "ALL");
    assert_eq!(
        h.world.read().unwrap().notifications.docs[&a.account.account_reference].contacts
            [&b.account.account_reference],
        "ALL"
    );
    let calls = ns_calls(&h);
    h.state
        .patch_contact(
            &a.profile,
            ab,
            json!({"expectedVersion":3,"alias":"Bee"}),
            Uuid::new_v4(),
        )
        .await
        .unwrap();
    assert_eq!(ns_calls(&h), calls);

    // The Hub readers that have not moved see every change made through the facade:
    // the Messaging push policy (direct messages off for a) ...
    let decision = h
        .state
        .check_policy(
            &h.scope,
            &check(Action::MessageNotification, &b.profile, &a.profile),
        )
        .await
        .unwrap();
    assert!(!decision.allowed);
    // ... and a rollback to HUB loses nothing that went through these endpoints.
    h.set_preference_authority("HUB").await;
    let back = h
        .state
        .get_notification_preferences(&a.profile)
        .await
        .unwrap();
    assert_eq!(
        (
            back["directMessages"].clone(),
            back["groupMessages"].clone()
        ),
        (json!(false), json!(false))
    );
    let c = h
        .state
        .get_conversation_preferences(&sa, &a.profile, conv)
        .await
        .unwrap();
    assert_eq!(
        (c["notifications"].clone(), c["mutedUntil"].clone()),
        (put["notifications"].clone(), put["mutedUntil"].clone())
    );
    h.close().await;
}

#[tokio::test]
async fn the_write_freeze_answers_503_with_retry_after_and_keeps_reads() {
    let h = Harness::new().await;
    let (a, b, conv) = seeded(&h).await;
    h.set_preference_authority("FREEZE").await;
    let r = h
        .state
        .put_notification_preferences(&a.profile, prefs(2, &[]), Uuid::new_v4())
        .await;
    let e = r.unwrap_err();
    assert!(matches!(e, Error::PreferencesFrozen));
    let response = e.into_response();
    assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(response.headers().get("retry-after").unwrap(), "5");
    let body = axum::body::to_bytes(response.into_body(), 4096)
        .await
        .unwrap();
    let body: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(body["error"]["code"], "COMMUNICATIONS_UNAVAILABLE");
    let sa = h.delegated(&a.account);
    assert!(matches!(
        h.state.put_conversation_preferences(&sa, &a.profile, conv,
            json!({"expectedVersion":1,"notifications":"ALL","mutedUntil":null,"mediaAutoDownload":"WIFI"}), Uuid::new_v4()).await,
        Err(Error::PreferencesFrozen)
    ));
    let ab = contact_id(&h, &a.profile, &b.profile).await;
    assert!(matches!(
        h.state
            .patch_contact(
                &a.profile,
                ab,
                json!({"expectedVersion":2,"notifications":"ALL"}),
                Uuid::new_v4()
            )
            .await,
        Err(Error::PreferencesFrozen)
    ));
    // Not a notification preference: allowed during the freeze.
    h.state
        .patch_contact(
            &a.profile,
            ab,
            json!({"expectedVersion":2,"favorite":true}),
            Uuid::new_v4(),
        )
        .await
        .unwrap();
    // Reads stay local and unchanged.
    assert_eq!(
        h.state
            .get_notification_preferences(&a.profile)
            .await
            .unwrap()["version"],
        2
    );
    assert_eq!(
        h.state
            .get_conversation_preferences(&sa, &a.profile, conv)
            .await
            .unwrap()["notifications"],
        "MUTED"
    );
    assert_eq!(ns_calls(&h), 0);
    h.close().await;
}

#[tokio::test]
async fn an_unavailable_service_is_a_clear_error_and_nothing_is_written_locally() {
    let h = Harness::new().await;
    let (a, b, conv) = seeded(&h).await;
    let sa = h.delegated(&a.account);
    let before = doc(&h, "SELECT jsonb_build_object('n',(SELECT to_jsonb(n) FROM communication_notification_preferences n WHERE profile_id=$1),
        'c',(SELECT to_jsonb(c) FROM communication_conversation_preferences c WHERE owner_profile_id=$1),
        'a',(SELECT jsonb_agg(to_jsonb(x)) FROM communication_contact_aliases x WHERE owner_profile_id=$1),
        'feed',(SELECT count(*) FROM communication_changes WHERE profile_id=$1)) doc", vec![a.profile.id.into()]).await;
    h.world.write().unwrap().notifications.docs.insert(
        a.account.account_reference.clone(),
        NsDoc {
            version: 2,
            ..Default::default()
        },
    );
    h.set_preference_authority("SERVICE").await;
    h.world.write().unwrap().notifications.down = true;

    assert!(matches!(
        h.state.get_notification_preferences(&a.profile).await,
        Err(Error::Unavailable)
    ));
    assert!(matches!(
        h.state
            .get_conversation_preferences(&sa, &a.profile, conv)
            .await,
        Err(Error::Unavailable)
    ));
    let key = Uuid::new_v4();
    assert!(matches!(
        h.state
            .put_notification_preferences(&a.profile, prefs(2, &["incomingCalls"]), key)
            .await,
        Err(Error::Unavailable)
    ));
    assert!(matches!(
        h.state.put_conversation_preferences(&sa, &a.profile, conv,
            json!({"expectedVersion":1,"notifications":"ALL","mutedUntil":null,"mediaAutoDownload":"WIFI"}), Uuid::new_v4()).await,
        Err(Error::Unavailable)
    ));
    let ab = contact_id(&h, &a.profile, &b.profile).await;
    assert!(matches!(
        h.state
            .patch_contact(
                &a.profile,
                ab,
                json!({"expectedVersion":2,"notifications":"ALL"}),
                Uuid::new_v4()
            )
            .await,
        Err(Error::Unavailable)
    ));
    let after = doc(&h, "SELECT jsonb_build_object('n',(SELECT to_jsonb(n) FROM communication_notification_preferences n WHERE profile_id=$1),
        'c',(SELECT to_jsonb(c) FROM communication_conversation_preferences c WHERE owner_profile_id=$1),
        'a',(SELECT jsonb_agg(to_jsonb(x)) FROM communication_contact_aliases x WHERE owner_profile_id=$1),
        'feed',(SELECT count(*) FROM communication_changes WHERE profile_id=$1)) doc", vec![a.profile.id.into()]).await;
    assert_eq!(after, before, "no silent local write, no feed entry");
    // Aggregates degrade to the Hub mirror instead of failing the whole app start.
    let boot = h.state.bootstrap(&sa, &a.profile).await.unwrap();
    assert_eq!(boot["notificationPreferences"]["directMessages"], false);
    let ctx = h
        .state
        .conversation_context(&sa, &a.profile, conv)
        .await
        .unwrap();
    assert_eq!(ctx["preferences"]["notifications"], "MUTED");
    // The same request succeeds once the service is back (the key was not consumed).
    h.world.write().unwrap().notifications.down = false;
    let put = h
        .state
        .put_notification_preferences(&a.profile, prefs(2, &["incomingCalls"]), key)
        .await
        .unwrap();
    assert_eq!(
        (put["version"].clone(), put["incomingCalls"].clone()),
        (json!(3), json!(false))
    );

    // Without CF_NOTIFICATIONS_BASE_URL the facade is a 503 DEPENDENCY_NOT_CONFIGURED.
    let c = &h.state.config;
    let unconfigured = State {
        store: h.state.store.clone(),
        config: Arc::new(Config {
            bind: c.bind,
            database_url: c.database_url.clone(),
            public_app_url: c.public_app_url.clone(),
            invite_key: c.invite_key.clone(),
            verifier: c.verifier.clone(),
            client: c.client.clone(),
            calling_configured: true,
            messaging_configured: true,
            notifications_configured: false,
            notifications_publications: false,
        }),
        metrics: h.state.metrics.clone(),
    };
    let e = unconfigured
        .put_notification_preferences(&a.profile, prefs(3, &[]), Uuid::new_v4())
        .await
        .unwrap_err();
    assert!(matches!(e, Error::DependencyNotConfigured));
    assert_eq!(e.into_response().status(), StatusCode::SERVICE_UNAVAILABLE);
    h.close().await;
}

#[test]
fn the_owner_contract_names_the_export_permission() {
    let path = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../packages/contracts/communications/notification-owner/v1.openapi.json"
    );
    let doc: Value = serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
    for p in [EXPORT, CHANGES] {
        assert_eq!(
            doc["paths"][p]["post"]["x-cfmeet-internal-permission"],
            "communications-preferences.export",
            "{p}"
        );
    }
}
