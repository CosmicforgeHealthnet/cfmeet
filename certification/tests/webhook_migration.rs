//! Part VI Section 53 §9.1 / §19.3 — the webhook ownership migration from Calling to the
//! Developer Platform, certified on the REAL processes (no stub): customers subscribe at
//! Calling through the API Gateway (central webhooks OFF); the operator console runs the
//! dry-run inventory, the resumable import, reconcile, the shadow report and the cutover
//! check through the Developer Platform's operator interface; the configuration is
//! flipped step by step exactly as the runbook orders it (processes restarted with new
//! variables); an event is then delivered ONCE, by the Developer Platform, to a local
//! receiver that verifies it with the ORIGINAL signing secret; finally the cutover is
//! rolled back and Calling delivers again, signed with the secret rotated at the
//! Developer Platform in between.
//!
//! Runtime `local` (the only runtime that may deliver to a loopback receiver); the
//! migration transfer key is generated per stack and given to Calling and to the Developer
//! Platform under their OWN variable names. No production migration is performed.

use std::time::Duration;

use base64::Engine;
use cf_service_auth::RuntimeEnvironment;
use cfmeet_certification::{
    provision::{self, Auth, Gateway, Resp, Sandbox, SANDBOX_SCOPES},
    receiver::{Received, Receiver},
    stack::{self, Stack},
    StackConfig, StackOptions,
};
use rand::RngCore;
use reqwest::Method;
use serde_json::{json, Value};
use uuid::Uuid;

const WAIT: Duration = Duration::from_secs(90);
const TYPES: &[&str] = &["conversation.created", "message.created"];

fn random_key() -> String {
    let mut b = [0u8; 32];
    rand::thread_rng().fill_bytes(&mut b);
    base64::engine::general_purpose::STANDARD.encode(b)
}

#[track_caller]
fn status(r: &Resp, expected: u16, what: &str) {
    assert_eq!(r.status, expected, "{what}: {}", r.text);
}

fn options() -> StackOptions {
    let transfer = random_key();
    let sealing = random_key();
    let mut o = StackOptions {
        runtime: Some(RuntimeEnvironment::Local),
        ..StackOptions::default()
    };
    for p in [stack::DEVELOPER_PLATFORM, stack::DEVELOPER_PLATFORM_WORKER] {
        o = o
            .set(p, "DEVELOPER_PLATFORM_WEBHOOKS_ENABLED", "true")
            .set(p, "DEVELOPER_PLATFORM_WEBHOOK_DELIVERY_MODE", "shadow")
            .set(p, "DEVELOPER_PLATFORM_WEBHOOK_SECRET_KEY", sealing.clone())
            .set(p, "DEVELOPER_PLATFORM_WEBHOOK_SECRET_KEY_ID", "cert-dp-1")
            .set(
                p,
                "DEVELOPER_PLATFORM_WEBHOOK_ALLOW_PRIVATE_ENDPOINTS",
                "true",
            )
            .set(
                p,
                "DEVELOPER_PLATFORM_WEBHOOK_MIGRATION_TRANSFER_KEY",
                transfer.clone(),
            )
            .set(
                p,
                "DEVELOPER_PLATFORM_WEBHOOK_MIGRATION_TRANSFER_KEY_ID",
                "cert-transfer-1",
            );
    }
    o.set(
        stack::CALLING,
        "CALLING_WEBHOOK_ALLOW_PRIVATE_ENDPOINTS",
        "true",
    )
    .set(
        stack::CALLING,
        "CALLING_WEBHOOK_MIGRATION_TRANSFER_KEY",
        transfer,
    )
    .set(
        stack::CALLING,
        "CALLING_WEBHOOK_MIGRATION_TRANSFER_KEY_ID",
        "cert-transfer-1",
    )
    .set(stack::CALLING, "CALLING_PUBLIC_EVENTS_DESTINATION", "local")
    // Before the shadow window: Messaging publishes to Calling only.
    .set(
        stack::MESSAGING,
        "MESSAGING_PUBLIC_EVENTS_DESTINATION",
        "calling",
    )
    .set(
        stack::API_GATEWAY,
        "API_GATEWAY_CENTRAL_WEBHOOKS_ENABLED",
        "false",
    )
}

async fn allow_managed(gw: &Gateway, secret: &str) {
    let cur = gw
        .get("/v1/messaging/policy", Auth::Credential(secret))
        .await;
    status(&cur, 200, "GET /v1/messaging/policy");
    let r = gw
        .request(
            Method::PUT,
            "/v1/messaging/policy",
            Auth::Credential(secret),
            Some(json!({ "expectedVersion": cur.body["version"], "policy": { "contexts": {
                "GROUP": { "allowedProfiles": ["MANAGED", "PRIVATE_E2EE"], "defaultProfile": "MANAGED" } } } })),
            &[],
        )
        .await;
    status(&r, 200, "PUT /v1/messaging/policy");
}

async fn conversation(gw: &Gateway, secret: &str, owner: Uuid) -> String {
    let mut last = None;
    for _ in 0..50 {
        let r = gw
            .acting(
                Method::POST,
                "/v1/conversations",
                Auth::Credential(secret),
                owner,
                Some(json!({ "contextClass": "GROUP", "securityProfile": "MANAGED" })),
            )
            .await;
        if r.status == 201 {
            return r.str("/conversationId");
        }
        last = Some(r);
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    panic!("POST /v1/conversations: {:?}", last.map(|r| r.text));
}

async fn message(gw: &Gateway, secret: &str, owner: Uuid, cid: &str) {
    let r = gw
        .acting(
            Method::POST,
            &format!("/v1/conversations/{cid}/messages"),
            Auth::Credential(secret),
            owner,
            Some(json!({ "clientMessageId": Uuid::new_v4(), "text": "migration journey" })),
        )
        .await;
    status(&r, 201, "POST /v1/conversations/{id}/messages");
}

/// A Developer Platform operator command of the migration.
async fn op(stack: &Stack, method: Method, path: &str, body: Option<Value>) -> Resp {
    stack
        .operator()
        .dp(method, &format!("/operator/webhook-migration/{path}"), body)
        .await
        .expect("operator call")
}

/// Waits until `n` webhooks reached the receiver (and no more arrive for a moment).
async fn received(rx: &Receiver, n: usize) -> Vec<Received> {
    let got = rx
        .wait_for("/hook", n, WAIT)
        .await
        .expect("webhooks delivered");
    tokio::time::sleep(Duration::from_secs(4)).await;
    let got_after = rx.received("/hook");
    assert_eq!(
        got_after.len(),
        n,
        "exactly {n} webhooks: {:?}",
        got_after.iter().map(|r| r.event_type()).collect::<Vec<_>>()
    );
    let _ = got;
    got_after
}

async fn restart_dp(stack: &mut Stack, vars: &[(&str, &str)]) {
    for p in [stack::DEVELOPER_PLATFORM, stack::DEVELOPER_PLATFORM_WORKER] {
        stack
            .restart(p, vars)
            .await
            .expect("developer platform restarts");
    }
}

/// The whole migration on real processes: seed at Calling → dry run → import → shadow
/// window → freeze → import + reconcile + cutover check → delivery switch → final import
/// → takeover → gateway switch → producers → unfreeze → one delivery by the Developer
/// Platform verified with the ORIGINAL secret → rollback → Calling delivers again with the
/// secret rotated at the Developer Platform.
#[tokio::test]
async fn webhook_migration_journey() {
    let Some(config) = StackConfig::from_env().expect("certification prerequisites") else {
        return;
    };
    let mut stack = Stack::start_with(config, options())
        .await
        .expect("certification stack starts");
    let gw = stack.gateway();
    let rx = Receiver::start().await.unwrap();
    let sb: Sandbox = provision::developer_sandbox(
        &stack,
        &format!(
            "migration-{}@cert.example",
            &Uuid::new_v4().simple().to_string()[..10]
        ),
        SANDBOX_SCOPES,
    )
    .await
    .unwrap();
    let cred = sb.customer.secret.clone();

    // 1. Calling owns webhooks: the subscription is created through the gateway (central
    //    webhooks OFF) and Calling delivers.
    let r = gw
        .post(
            "/v1/webhooks",
            Auth::Credential(&cred),
            json!({ "url": rx.url("/hook"), "eventTypes": TYPES, "description": "migration" }),
        )
        .await;
    status(&r, 201, "POST /v1/webhooks (Calling)");
    let webhook = r.uuid("/webhook/webhookId");
    let original = r.str("/signingSecret");
    let owner = stack.operator().principal(sb.environment_id).await.unwrap();
    allow_managed(&gw, &cred).await;
    let cid = conversation(&gw, &cred, owner).await;
    message(&gw, &cred, owner, &cid).await;
    let got = received(&rx, 2).await;
    for r in &got {
        r.verify(&original)
            .expect("Calling signs with the original secret");
    }

    // 2. Dry run (writes nothing), import, reconcile.
    let r = op(&stack, Method::GET, "inventory", None).await;
    status(&r, 200, "inventory (dry run)");
    assert_eq!(
        r.body["wouldImport"]["subscriptions"]["inserted"], 1,
        "{}",
        r.text
    );
    assert_eq!(r.body["rotationRequired"], 0);
    let r = op(&stack, Method::GET, "status", None).await;
    assert_eq!(
        r.body["migratedSubscriptions"], 0,
        "the dry run wrote nothing"
    );
    assert_eq!(r.body["phase"], Value::Null);
    let r = op(
        &stack,
        Method::POST,
        "import",
        Some(json!({ "pageSize": 1 })),
    )
    .await;
    status(&r, 200, "import");
    assert_eq!(r.body["state"], "COMPLETED", "{}", r.text);
    assert_eq!(r.body["conflicts"], json!([]), "{}", r.text);
    let r = op(&stack, Method::GET, "reconcile", None).await;
    assert_eq!(r.body["clean"], true, "{}", r.text);
    assert!(!r.text.contains(&original) && !r.text.contains("whsec_"));

    // 3. The shadow window: Messaging publishes to both; Calling delivers, the Developer
    //    Platform only records SHADOW deliveries — exactly one delivery per event.
    let window = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
    stack
        .restart(
            stack::MESSAGING,
            &[("MESSAGING_PUBLIC_EVENTS_DESTINATION", "both")],
        )
        .await
        .unwrap();
    message(&gw, &cred, owner, &cid).await;
    let got = received(&rx, 3).await;
    got[2].verify(&original).unwrap();
    tokio::time::sleep(Duration::from_secs(3)).await;
    let r = op(
        &stack,
        Method::GET,
        &format!("shadow-report?since={window}"),
        None,
    )
    .await;
    status(&r, 200, "shadow report");
    assert_eq!(r.body["parity"], true, "{}", r.text);
    assert_eq!(r.body["eventsComparedBothSides"], 1, "{}", r.text);
    assert_eq!(r.body["developerPlatformDeliveriesSent"], 0);

    // 4. Freeze both sides' webhook mutations; a customer change is refused at Calling.
    stack
        .restart(
            stack::CALLING,
            &[("CALLING_WEBHOOK_MUTATIONS_FROZEN", "true")],
        )
        .await
        .unwrap();
    restart_dp(
        &mut stack,
        &[("DEVELOPER_PLATFORM_WEBHOOK_MUTATIONS_FROZEN", "true")],
    )
    .await;
    let r = gw
        .post(
            "/v1/webhooks",
            Auth::Credential(&cred),
            json!({ "url": rx.url("/hook"), "eventTypes": TYPES }),
        )
        .await;
    status(&r, 503, "frozen create at Calling");
    assert_eq!(r.code(), Some("SERVICE_UNAVAILABLE"));
    let r = gw
        .get(&format!("/v1/webhooks/{webhook}"), Auth::Credential(&cred))
        .await;
    status(&r, 200, "reads continue while frozen");
    let version = r.body["version"].as_i64().unwrap();

    // 5. Import (everything since), reconcile, cutover check.
    let r = op(&stack, Method::POST, "import", Some(json!({}))).await;
    assert_eq!(r.body["state"], "COMPLETED", "{}", r.text);
    let r = op(&stack, Method::GET, "cutover-check", None).await;
    status(&r, 200, "cutover-check");
    assert_eq!(r.body["ready"], true, "{}", r.text);

    // 6. The delivery switch: Calling stops sending (its deliveries stay recorded), the
    //    Developer Platform delivers — gated until the takeover proves Calling stopped.
    stack
        .restart(
            stack::CALLING,
            &[("CALLING_WEBHOOK_DELIVERY_ENABLED", "false")],
        )
        .await
        .unwrap();
    restart_dp(
        &mut stack,
        &[("DEVELOPER_PLATFORM_WEBHOOK_DELIVERY_MODE", "active")],
    )
    .await;
    let r = op(&stack, Method::POST, "takeover", None).await;
    status(&r, 409, "takeover before the final import");
    let r = op(&stack, Method::POST, "import", Some(json!({}))).await;
    assert_eq!(r.body["state"], "COMPLETED", "{}", r.text);
    assert_eq!(
        r.body["callingAtStart"]["deliveryEnabled"], false,
        "{}",
        r.text
    );
    let r = op(&stack, Method::POST, "takeover", None).await;
    status(&r, 200, "takeover");
    assert_eq!(r.body["phase"], "CUT_OVER");

    // 7. The gateway routes customer webhooks to the Developer Platform: same id, same
    //    version (expectedVersion continuity). Producers publish to it only; unfreeze.
    stack
        .restart(
            stack::API_GATEWAY,
            &[("API_GATEWAY_CENTRAL_WEBHOOKS_ENABLED", "true")],
        )
        .await
        .unwrap();
    stack
        .restart(
            stack::MESSAGING,
            &[("MESSAGING_PUBLIC_EVENTS_DESTINATION", "developer-platform")],
        )
        .await
        .unwrap();
    restart_dp(
        &mut stack,
        &[("DEVELOPER_PLATFORM_WEBHOOK_MUTATIONS_FROZEN", "false")],
    )
    .await;
    let r = gw
        .get(&format!("/v1/webhooks/{webhook}"), Auth::Credential(&cred))
        .await;
    status(&r, 200, "GET /v1/webhooks/{id} (Developer Platform)");
    assert_eq!(r.body["version"], version);
    assert_eq!(r.body["status"], "ACTIVE");

    // 8. One event, delivered ONCE by the Developer Platform, verified with the ORIGINAL
    //    secret.
    message(&gw, &cred, owner, &cid).await;
    let got = received(&rx, 4).await;
    let dp = &got[3];
    dp.verify(&original)
        .expect("the ORIGINAL secret verifies the Developer Platform's delivery");
    let r = gw
        .get(
            &format!("/v1/webhooks/{webhook}/deliveries?limit=100"),
            Auth::Credential(&cred),
        )
        .await;
    status(&r, 200, "deliveries (Developer Platform)");
    let ids: Vec<String> = r.body["data"]
        .as_array()
        .unwrap()
        .iter()
        .map(|d| d["deliveryId"].as_str().unwrap().to_string())
        .collect();
    assert!(ids.contains(&dp.delivery_id().to_string()), "{ids:?}");

    // 9. Rollback. A change made at the Developer Platform (a rotated secret) is pushed
    //    back to Calling; the delivery switch is reversed; Calling delivers with it.
    let r = gw
        .post(
            &format!("/v1/webhooks/{webhook}/rotate-secret"),
            Auth::Credential(&cred),
            json!({ "expectedVersion": version }),
        )
        .await;
    status(&r, 200, "rotate-secret (Developer Platform)");
    let rotated = r.str("/signingSecret");
    restart_dp(
        &mut stack,
        &[("DEVELOPER_PLATFORM_WEBHOOK_MUTATIONS_FROZEN", "true")],
    )
    .await;
    let r = op(&stack, Method::POST, "rollback-export", None).await;
    status(&r, 200, "rollback-export");
    assert_eq!(r.body["applied"], 1, "{}", r.text);
    assert!(!r.text.contains(&rotated));
    stack
        .restart(
            stack::API_GATEWAY,
            &[("API_GATEWAY_CENTRAL_WEBHOOKS_ENABLED", "false")],
        )
        .await
        .unwrap();
    stack
        .restart(
            stack::MESSAGING,
            &[("MESSAGING_PUBLIC_EVENTS_DESTINATION", "both")],
        )
        .await
        .unwrap();
    let r = op(&stack, Method::POST, "rollback", Some(json!({}))).await;
    status(&r, 200, "rollback");
    assert_eq!(r.body["completed"], true, "{}", r.text);
    stack
        .restart(
            stack::CALLING,
            &[
                ("CALLING_WEBHOOK_DELIVERY_ENABLED", "true"),
                ("CALLING_WEBHOOK_MUTATIONS_FROZEN", "false"),
            ],
        )
        .await
        .unwrap();
    restart_dp(
        &mut stack,
        &[
            ("DEVELOPER_PLATFORM_WEBHOOK_DELIVERY_MODE", "shadow"),
            ("DEVELOPER_PLATFORM_WEBHOOK_MUTATIONS_FROZEN", "false"),
        ],
    )
    .await;
    let r = gw
        .get(&format!("/v1/webhooks/{webhook}"), Auth::Credential(&cred))
        .await;
    status(&r, 200, "GET /v1/webhooks/{id} (Calling again)");
    assert_eq!(r.body["secretVersion"], 2, "{}", r.text);
    message(&gw, &cred, owner, &cid).await;
    let got = received(&rx, 5).await;
    got[4]
        .verify(&rotated)
        .expect("Calling signs with the secret rotated at the Developer Platform");
    assert!(got[4].verify(&original).is_err());
    let r = op(&stack, Method::GET, "status", None).await;
    assert_eq!(r.body["phase"], "ROLLED_BACK", "{}", r.text);
    stack.assert_alive().unwrap();
    stack.teardown().await;
}
