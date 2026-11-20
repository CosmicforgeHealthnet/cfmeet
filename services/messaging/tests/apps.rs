//! CF Meet Apps (ADR 0064; `docs/platform/integrate-internal-contracts.md` §7): OAuth
//! access / bot credentials act only as their restricted principal on a closed
//! MANAGED-only allowlist and record `viaApplicationId`; app bots are MANAGED-only and
//! mentions / direct messages publish ids-only bot events to the Developer Platform —
//! against the real router and a real PostgreSQL database, Platform Access scripted.

mod common;

use std::sync::Mutex;

use axum::http::{Method, StatusCode};
use common::{flows::*, *};
use messaging::{
    apps::AppsSettings,
    identity::{OAuthCredential, OAuthKind},
    webhook_relay::{self, EventDestination, PlatformEventSink, RelayFailure},
};
use serde_json::{json, Value};
use uuid::Uuid;

async fn apps_harness() -> Option<Harness> {
    Harness::build(|s| s.apps = AppsSettings::enabled()).await
}

/// An OAuth credential of `app` in `of`'s environment, restricted to `principals`.
fn oauth(h: &Harness, of: &Customer, kind: OAuthKind, app: Uuid, principals: &[Uuid]) -> Customer {
    let mut context = of.context.clone();
    context.credential_id = Uuid::new_v4();
    context.scopes = ["messages.read", "messages.send"]
        .iter()
        .map(|s| s.to_string())
        .collect();
    context.principal_restriction = Some(principals.iter().copied().collect());
    context.oauth = Some(OAuthCredential {
        kind,
        app_id: app,
        grant_id: Uuid::new_v4(),
    });
    h.register_token(context)
}

/// A CF Meet App's bot principal (`BOT_OR_SERVICE`, `app:<appId>`) of `c`'s environment.
fn bot(h: &Harness, c: &Customer, app: Uuid) -> Uuid {
    let id = h.principal_of_kind(c, "BOT_OR_SERVICE");
    h.principals
        .bots
        .lock()
        .unwrap()
        .insert((c.context.scope.environment_id, id), app);
    id
}

fn cid(conv: &Value) -> String {
    conv["conversationId"].as_str().unwrap().to_string()
}

#[tokio::test]
async fn an_oauth_user_token_acts_only_as_its_user() {
    let Some(h) = apps_harness().await else {
        return;
    };
    let c = h.customer(ALL_SCOPES);
    let (conv, people) = h.managed_group(&c, 2).await;
    let app = Uuid::new_v4();
    let token = oauth(&h, &c, OAuthKind::Access, app, &[people[1]]);
    let id = cid(&conv);

    // No CF-Acting-Principal: the restricted principal acts.
    let r = h.get(&token, None, "/v1/conversations").await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    assert_eq!(
        r.json["conversations"].as_array().unwrap().len(),
        1,
        "{}",
        r.text
    );
    let r = h
        .get(&token, Some(people[1]), &format!("/v1/conversations/{id}"))
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    // Another principal (even a member of the conversation) is refused.
    let r = h
        .get(&token, Some(people[0]), &format!("/v1/conversations/{id}"))
        .await;
    assert_eq!(r.status, StatusCode::FORBIDDEN, "{}", r.text);
    assert_eq!(r.reason(), "ACTING_PRINCIPAL_MISMATCH", "{}", r.text);
    // Outside the allowlist: conversation creation, membership, client tokens (and so
    // realtime), policy.
    for (method, path) in [
        (Method::POST, "/v1/conversations".to_string()),
        (Method::POST, format!("/v1/conversations/{id}/members")),
        (Method::POST, "/v1/messaging/client-tokens".to_string()),
        (Method::GET, "/v1/messaging/policy".to_string()),
    ] {
        let r = h
            .send(
                method,
                Some(&token.token),
                None,
                &path,
                Some(json!({})),
                &[("idempotency-key", &idem())],
            )
            .await;
        assert_eq!(r.status, StatusCode::FORBIDDEN, "{path}: {}", r.text);
        assert_eq!(r.code(), "INSUFFICIENT_SCOPE", "{path}");
        assert_eq!(r.reason(), "OAUTH_ROUTE_NOT_ALLOWED", "{path}");
    }
    // A restriction that does not name exactly one principal is refused.
    let wide = oauth(&h, &c, OAuthKind::Access, app, &[people[0], people[1]]);
    let r = h.get(&wide, None, "/v1/conversations").await;
    assert_eq!(r.status, StatusCode::FORBIDDEN, "{}", r.text);
    assert_eq!(r.reason(), "CREDENTIAL_RESTRICTION");
}

#[tokio::test]
async fn private_e2ee_is_invisible_to_apps() {
    let Some(h) = apps_harness().await else {
        return;
    };
    let c = h.customer(ALL_SCOPES);
    let (e2ee, a, _b) = h.e2ee_direct(&c).await;
    h.allow_managed(&c, "GROUP").await;
    let r = h
        .create_conversation(
            &c,
            a.principal,
            json!({ "contextClass": "GROUP", "securityProfile": "MANAGED" }),
        )
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text);
    let managed = r.json;
    assert_eq!(
        h.send_managed(&c, &managed, a.principal, "findme please")
            .await
            .status,
        StatusCode::CREATED
    );
    let token = oauth(&h, &c, OAuthKind::Access, Uuid::new_v4(), &[a.principal]);

    // The ordinary credential sees both; the app only the MANAGED one.
    let all = h.get(&c, Some(a.principal), "/v1/conversations").await;
    assert_eq!(
        all.json["conversations"].as_array().unwrap().len(),
        2,
        "{}",
        all.text
    );
    let r = h.get(&token, None, "/v1/conversations").await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    let listed: Vec<String> = r.json["conversations"]
        .as_array()
        .unwrap()
        .iter()
        .map(cid)
        .collect();
    assert_eq!(listed, vec![cid(&managed)]);

    let e = cid(&e2ee);
    for (method, path) in [
        (Method::GET, format!("/v1/conversations/{e}")),
        (Method::GET, format!("/v1/conversations/{e}/members")),
        (Method::GET, format!("/v1/conversations/{e}/messages")),
        (Method::POST, format!("/v1/conversations/{e}/messages")),
        (Method::GET, format!("/v1/conversations/{e}/events")),
        (Method::GET, format!("/v1/conversations/{e}/search?q=x")),
        (Method::POST, format!("/v1/conversations/{e}/attachments")),
        (
            Method::GET,
            format!("/v1/conversations/{e}/attachments/{}", Uuid::new_v4()),
        ),
    ] {
        let body = (method == Method::POST)
            .then(|| json!({ "clientMessageId": Uuid::new_v4(), "text": "x" }));
        let r = h
            .send(method, Some(&token.token), None, &path, body, &[])
            .await;
        assert_eq!(r.status, StatusCode::NOT_FOUND, "{path}: {}", r.text);
        assert_eq!(r.code(), "NOT_FOUND", "{path}");
    }
    // Search: MANAGED results, and E2EE conversations are not even counted.
    let r = h.get(&token, None, "/v1/messaging/search?q=findme").await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    assert_eq!(r.json["results"].as_array().unwrap().len(), 1, "{}", r.text);
    assert_eq!(r.json["localSearchOnlyConversations"], 0);
    let r = h
        .get(&c, Some(a.principal), "/v1/messaging/search?q=findme")
        .await;
    assert_eq!(r.json["localSearchOnlyConversations"], 1, "{}", r.text);
}

#[tokio::test]
async fn messages_record_the_sending_app() {
    let Some(h) = apps_harness().await else {
        return;
    };
    let c = h.customer(ALL_SCOPES);
    let (conv, people) = h.managed_group(&c, 2).await;
    let app = Uuid::new_v4();
    let token = oauth(&h, &c, OAuthKind::Access, app, &[people[1]]);
    let id = cid(&conv);
    let r = h
        .post(
            &token,
            None,
            &format!("/v1/conversations/{id}/messages"),
            json!({ "clientMessageId": Uuid::new_v4(), "text": "hello from the app" }),
        )
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text);
    assert_eq!(r.json["viaApplicationId"], json!(app));
    assert_eq!(r.json["senderPrincipalId"], json!(people[1]));
    conforms("messaging/common/v1.schema.json#/$defs/Message", &r.json);
    let sent = uuid(&r.json["messageId"]);
    assert_eq!(
        h.scalar(&format!(
            "SELECT count(*) FROM messaging_messages WHERE id = '{sent}' AND via_application_id = '{app}'"
        ))
        .await,
        1
    );
    // A message of the ordinary credential carries no viaApplicationId; readers see the
    // app on the app's message.
    let plain = h
        .send_managed(&c, &conv, people[0], "hello from a person")
        .await;
    assert_eq!(plain.status, StatusCode::CREATED, "{}", plain.text);
    assert!(
        plain.json.get("viaApplicationId").is_none(),
        "{}",
        plain.text
    );
    let r = h
        .get(
            &c,
            Some(people[0]),
            &format!("/v1/conversations/{id}/messages"),
        )
        .await;
    let msgs = r.json["messages"].as_array().unwrap();
    let mine = msgs.iter().find(|m| m["messageId"] == json!(sent)).unwrap();
    assert_eq!(mine["viaApplicationId"], json!(app));
}

#[tokio::test]
async fn an_app_bot_never_joins_a_private_e2ee_conversation() {
    let Some(h) = apps_harness().await else {
        return;
    };
    let c = h.customer(ALL_SCOPES);
    let app = Uuid::new_v4();
    let b = bot(&h, &c, app);
    // A PRIVATE_E2EE group (the default profile).
    let owner = h.member(&c, 1).await;
    let r = h
        .create_conversation(&c, owner.principal, json!({ "contextClass": "GROUP" }))
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text);
    assert_eq!(r.json["securityProfile"], "PRIVATE_E2EE");
    let add = |conv: Value| {
        let h = &h;
        let c = &c;
        let owner = owner.principal;
        async move {
            h.post(
                c,
                Some(owner),
                &format!("/v1/conversations/{}/members", cid(&conv)),
                json!({ "principalId": b, "role": "BOT_SERVICE" }),
            )
            .await
        }
    };
    let r = add(r.json).await;
    assert_eq!(r.status, StatusCode::FORBIDDEN, "{}", r.text);
    assert_eq!(r.reason(), "APP_BOT_MANAGED_ONLY");
    // Nor a DIRECT PRIVATE_E2EE conversation with it.
    let r = h
        .create_conversation(
            &c,
            owner.principal,
            json!({ "contextClass": "DIRECT", "counterpartPrincipalId": b }),
        )
        .await;
    assert_eq!(r.status, StatusCode::FORBIDDEN, "{}", r.text);
    assert_eq!(r.reason(), "APP_BOT_MANAGED_ONLY");
    // A MANAGED group admits it; a bot that is not an app's (another reference) is unchanged.
    h.allow_managed(&c, "GROUP").await;
    let r = h
        .create_conversation(
            &c,
            owner.principal,
            json!({ "contextClass": "GROUP", "securityProfile": "MANAGED" }),
        )
        .await;
    assert_eq!(add(r.json).await.status, StatusCode::CREATED);
    let system = h.principal_of_kind(&c, "BOT_OR_SERVICE");
    let r = h
        .create_conversation(
            &c,
            owner.principal,
            json!({ "contextClass": "GROUP", "securityProfile": "PRIVATE_E2EE" }),
        )
        .await;
    let r = h
        .post(
            &c,
            Some(owner.principal),
            &format!("/v1/conversations/{}/members", cid(&r.json)),
            json!({ "principalId": system, "role": "BOT_SERVICE" }),
        )
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text);
}

#[derive(Default)]
struct Sink {
    received: Mutex<Vec<Value>>,
}

#[async_trait::async_trait]
impl PlatformEventSink for Sink {
    async fn publish(&self, envelope: &Value) -> Result<(), RelayFailure> {
        self.received.lock().unwrap().push(envelope.clone());
        Ok(())
    }
}

#[tokio::test]
async fn bot_mentions_and_direct_messages_publish_ids_only_bot_events() {
    let Some(h) = apps_harness().await else {
        return;
    };
    let c = h.customer(ALL_SCOPES);
    let app = Uuid::new_v4();
    let b = bot(&h, &c, app);
    let (conv, people) = h.managed_group(&c, 2).await;
    let id = cid(&conv);
    let r = h
        .post(
            &c,
            Some(people[0]),
            &format!("/v1/conversations/{id}/members"),
            json!({ "principalId": b, "role": "BOT_SERVICE" }),
        )
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text);
    // A mention of the bot (and of a person: only the bot gets an event).
    let r = h
        .post(
            &c,
            Some(people[0]),
            &format!("/v1/conversations/{id}/messages"),
            json!({ "clientMessageId": Uuid::new_v4(), "text": "secret words for the bot",
                    "mentionedPrincipalIds": [b, people[1]] }),
        )
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text);
    let mentioned = uuid(&r.json["messageId"]);
    // A message without a mention: nothing.
    assert_eq!(
        h.send_managed(&c, &conv, people[1], "no mention")
            .await
            .status,
        StatusCode::CREATED
    );
    // A MANAGED DIRECT conversation with the bot.
    h.allow_managed(&c, "DIRECT").await;
    let r = h
        .create_conversation(&c, people[1], json!({ "contextClass": "DIRECT", "securityProfile": "MANAGED", "counterpartPrincipalId": b }))
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text);
    let dm = r.json;
    let r = h
        .send_managed(&c, &dm, people[1], "direct secret words")
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text);
    let direct = uuid(&r.json["messageId"]);
    // The bot answering (an app's bot token) is not an event about itself.
    let bot_token = oauth(&h, &c, OAuthKind::Bot, app, &[b]);
    let r = h
        .post(
            &bot_token,
            None,
            &format!("/v1/conversations/{}/messages", cid(&dm)),
            json!({ "clientMessageId": Uuid::new_v4(), "text": "bot reply" }),
        )
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text);
    assert_eq!(r.json["viaApplicationId"], json!(app));

    assert_eq!(
        h.scalar(
            "SELECT count(*) FROM messaging_outbox_events WHERE event_type LIKE 'messaging.bot.%'"
        )
        .await,
        2
    );
    // Every recorded event conforms to the event contract.
    let types = h.conforming_event_types().await;
    assert!(
        types.contains("messaging.bot.mentioned")
            && types.contains("messaging.bot.message-received")
    );

    // Relayed to the Developer Platform only, ids only, in the conversation's scope.
    let (calling, developer) = (Sink::default(), Sink::default());
    let sinks: [(EventDestination, &dyn PlatformEventSink); 2] = [
        (EventDestination::Calling, &calling),
        (EventDestination::DeveloperPlatform, &developer),
    ];
    webhook_relay::relay_due_to(&h.state, &sinks, 500)
        .await
        .unwrap();
    let is_bot = |e: &&Value| {
        e["event_type"]
            .as_str()
            .unwrap()
            .starts_with("messaging.bot.")
    };
    assert!(
        calling
            .received
            .lock()
            .unwrap()
            .iter()
            .filter(is_bot)
            .count()
            == 0
    );
    let received = developer.received.lock().unwrap().clone();
    let bots: Vec<&Value> = received.iter().filter(is_bot).collect();
    assert_eq!(bots.len(), 2, "{bots:?}");
    for e in &bots {
        let p = e["payload"].as_object().unwrap();
        let mut keys: Vec<&str> = p.keys().map(String::as_str).collect();
        keys.sort();
        assert_eq!(
            keys,
            [
                "conversation_id",
                "message_id",
                "sender_principal_id",
                "subject_principal_id"
            ]
        );
        assert_eq!(p["subject_principal_id"], json!(b));
        assert_eq!(
            e["ownership"]["application_environment_id"],
            json!(c.context.scope.environment_id)
        );
        assert!(!e.to_string().contains("secret words"));
    }
    let of = |t: &str| *bots.iter().find(|e| e["event_type"] == t).unwrap();
    assert_eq!(
        of("messaging.bot.mentioned")["payload"]["message_id"],
        json!(mentioned)
    );
    assert_eq!(
        of("messaging.bot.mentioned")["payload"]["sender_principal_id"],
        json!(people[0])
    );
    assert_eq!(
        of("messaging.bot.message-received")["payload"]["message_id"],
        json!(direct)
    );
    assert_eq!(
        h.scalar("SELECT count(*) FROM messaging_webhook_relay r JOIN messaging_outbox_events o ON o.id = r.event_id \
                  WHERE o.event_type LIKE 'messaging.bot.%' AND r.status = 'RELAYED' AND r.calling_outcome IS NULL").await,
        2
    );
}

#[tokio::test]
async fn with_the_flag_off_nothing_changes_and_oauth_is_refused() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let c = h.customer(ALL_SCOPES);
    let app = Uuid::new_v4();
    let b = bot(&h, &c, app);
    let (conv, people) = h.managed_group(&c, 2).await;
    let token = oauth(&h, &c, OAuthKind::Access, app, &[people[1]]);
    let r = h.get(&token, None, "/v1/conversations").await;
    assert_eq!(r.status, StatusCode::FORBIDDEN, "{}", r.text);
    assert_eq!(r.reason(), "OAUTH_NOT_ENABLED");
    // Bots join and are mentioned as before; no bot event, no viaApplicationId.
    let id = cid(&conv);
    let r = h
        .post(
            &c,
            Some(people[0]),
            &format!("/v1/conversations/{id}/members"),
            json!({ "principalId": b, "role": "BOT_SERVICE" }),
        )
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text);
    let r = h
        .post(&c, Some(people[0]), &format!("/v1/conversations/{id}/messages"),
            json!({ "clientMessageId": Uuid::new_v4(), "text": "hi", "mentionedPrincipalIds": [b] }))
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text);
    assert!(r.json.get("viaApplicationId").is_none());
    assert_eq!(
        h.scalar(
            "SELECT count(*) FROM messaging_outbox_events WHERE event_type LIKE 'messaging.bot.%'"
        )
        .await,
        0
    );
    let owner = h.member(&c, 1).await;
    let e2ee = h
        .create_conversation(&c, owner.principal, json!({ "contextClass": "GROUP" }))
        .await;
    let r = h
        .post(
            &c,
            Some(owner.principal),
            &format!("/v1/conversations/{}/members", cid(&e2ee.json)),
            json!({ "principalId": b, "role": "BOT_SERVICE" }),
        )
        .await;
    assert_eq!(
        r.status,
        StatusCode::CREATED,
        "unchanged with the flag off: {}",
        r.text
    );
}
