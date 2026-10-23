//! Part VI §51 / §53 — meetings through the public API Gateway with platform credentials,
//! and `meeting.*` webhooks, on the REAL processes.
//!
//! * [`sandbox_credential_runs_environment_meetings_through_the_gateway`] (default `test`
//!   stack): an external developer's SANDBOX credential creates, lists, reads and ends
//!   meetings through the gateway (`PLATFORM_CREDENTIAL`, `meetings.read` /
//!   `meetings.write`); the gateway forwards with its signed delegated context and the
//!   Meeting Service has Platform Access re-verify the credential live. Another tenant's
//!   credential gets 404 for those meetings, a read-only credential is refused writes at
//!   the edge, the developer's own CF Meet session never sees them, and a revoked
//!   credential is refused on the next request.
//! * [`meeting_webhooks_are_delivered_signed_with_safe_fields`] (`local` stack: the
//!   Developer Platform delivers to a loopback receiver only there, as in
//!   `tests/webhooks.rs`; Meeting Service publishes to the Developer Platform, delivery
//!   mode active): the meeting's `meeting.started` webhook reaches the subscribing
//!   environment signed with its secret, carrying ids and lifecycle codes only — and never
//!   reaches another tenant.
//!
//! Prerequisites and skip rules are those of `tests/journeys.rs`. One journey at a time.

use std::{
    collections::BTreeSet,
    sync::atomic::{AtomicBool, Ordering},
    time::Duration,
};

use base64::Engine;
use cf_service_auth::RuntimeEnvironment;
use cfmeet_certification::{
    provision::{self, Auth, Gateway, Resp, SANDBOX_SCOPES},
    receiver::{Received, Receiver},
    stack::{self, Stack},
    StackConfig, StackOptions,
};
use rand::RngCore;
use serde_json::{json, Value};
use uuid::Uuid;

static BUSY: AtomicBool = AtomicBool::new(false);

struct Serial;

impl Serial {
    async fn acquire() -> Serial {
        while BUSY
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_err()
        {
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        Serial
    }
}

impl Drop for Serial {
    fn drop(&mut self) {
        BUSY.store(false, Ordering::Release);
    }
}

fn tag() -> String {
    Uuid::new_v4().simple().to_string()[..10].to_string()
}

fn email(role: &str) -> String {
    format!("{role}-{}@cert.example", tag())
}

#[track_caller]
fn status(r: &Resp, expected: u16, what: &str) {
    assert_eq!(r.status, expected, "{what}: {}", r.text);
}

const WAIT: Duration = Duration::from_secs(90);

async fn start(options: StackOptions) -> Option<(Serial, Stack)> {
    let config = StackConfig::from_env().expect("certification prerequisites")?;
    let guard = Serial::acquire().await;
    let stack = Stack::start_with(config, options)
        .await
        .expect("certification stack starts");
    Some((guard, stack))
}

async fn instant_meeting(gw: &Gateway, secret: &str, title: &str) -> (Uuid, Uuid, String) {
    let r = gw
        .post(
            "/api/v2/instant-meetings",
            Auth::Credential(secret),
            json!({ "title": title, "accessPolicy": "PUBLIC" }),
        )
        .await;
    status(
        &r,
        201,
        "POST /api/v2/instant-meetings (platform credential)",
    );
    (
        r.uuid("/meetingSpace/id"),
        r.uuid("/conference/id"),
        r.str("/meetingSpace/meetingCode"),
    )
}

#[tokio::test]
async fn sandbox_credential_runs_environment_meetings_through_the_gateway() {
    let Some((_g, stack)) = start(StackOptions::default()).await else {
        return;
    };
    let gw = stack.gateway();
    let operator = stack.operator();
    let a = provision::developer_sandbox(&stack, &email("meet-a"), SANDBOX_SCOPES)
        .await
        .unwrap();
    let b = provision::developer_sandbox(&stack, &email("meet-b"), SANDBOX_SCOPES)
        .await
        .unwrap();
    let cred_a = Auth::Credential(&a.customer.secret);
    let cred_b = Auth::Credential(&b.customer.secret);

    // 1. Create (instant), list, read, schedule and end — all through the gateway.
    let (space, conference, code) = instant_meeting(&gw, &a.customer.secret, "Support").await;
    assert!(
        gw.get(&format!("/api/v2/meeting-spaces/{space}"), cred_a)
            .await
            .body["ownerUserId"]
            .is_null(),
        "an environment's meeting has no CF Meet owner"
    );
    let list = gw.get("/api/v2/meeting-spaces", cred_a).await;
    status(&list, 200, "GET /api/v2/meeting-spaces");
    let items = list.body["items"].as_array().unwrap();
    assert_eq!(items.len(), 1, "{}", list.text);
    assert_eq!(items[0]["id"], json!(space));
    assert_eq!(items[0]["viewerRole"], "OWNER");
    let one = gw
        .get(&format!("/api/v2/meeting-spaces/{code}"), cred_a)
        .await;
    status(&one, 200, "GET /api/v2/meeting-spaces/{code}");
    let c = gw
        .get(&format!("/api/v2/conferences/{conference}"), cred_a)
        .await;
    status(&c, 200, "GET /api/v2/conferences/{id}");
    assert_eq!(c.body["status"], "LIVE");
    let p = gw
        .get(
            &format!("/api/v2/conferences/{conference}/participants"),
            cred_a,
        )
        .await;
    status(&p, 200, "GET /api/v2/conferences/{id}/participants");
    let scheduled = gw
        .post(
            "/api/v2/meeting-spaces",
            cred_a,
            json!({ "title": "Weekly", "accessPolicy": "PUBLIC" }),
        )
        .await;
    status(&scheduled, 201, "POST /api/v2/meeting-spaces");
    let scheduled = scheduled.uuid("/id");
    let occurrence = gw
        .post(
            &format!("/api/v2/meeting-spaces/{scheduled}/conferences"),
            cred_a,
            json!({}),
        )
        .await;
    status(
        &occurrence,
        201,
        "POST /api/v2/meeting-spaces/{id}/conferences",
    );
    let end = gw
        .post(
            &format!("/api/v2/conferences/{conference}/end"),
            cred_a,
            json!({}),
        )
        .await;
    status(&end, 200, "POST /api/v2/conferences/{id}/end");

    // The meeting is owned by A's LIVE chain (Meeting Service evidence).
    let db = stack.db(stack::MEETING_SERVICE).await.unwrap();
    let owned = cfmeet_certification::db::scalar_i64(
        &db,
        &format!(
            "SELECT count(*) FROM meeting_spaces WHERE id = '{space}' AND owner_user_id IS NULL \
             AND platform_tenant_id = '{}' AND platform_application_id = '{}' \
             AND platform_environment_id = '{}' AND created_by_service_account_id = '{}'",
            a.customer.tenant_id,
            a.customer.application_id,
            a.customer.environment_id,
            a.customer.service_account_id
        ),
    )
    .await
    .unwrap();
    assert_eq!(owned, 1);

    // 2. Another tenant: the meetings do not exist for it.
    for path in [
        format!("/api/v2/meeting-spaces/{space}"),
        format!("/api/v2/meeting-spaces/{code}"),
        format!("/api/v2/conferences/{conference}"),
        format!("/api/v2/conferences/{conference}/participants"),
    ] {
        let r = gw.get(&path, cred_b).await;
        status(&r, 404, &format!("tenant B GET {path}"));
    }
    let r = gw
        .post(
            &format!("/api/v2/conferences/{conference}/end"),
            cred_b,
            json!({}),
        )
        .await;
    status(&r, 404, "tenant B ends A's conference");
    let r = gw.get("/api/v2/meeting-spaces", cred_b).await;
    status(&r, 200, "tenant B lists");
    assert_eq!(r.body["items"], json!([]), "{}", r.text);

    // 3. Scopes are enforced at the edge: a read-only credential of A's environment.
    let (_, reader) = operator
        .credential(a.customer.service_account_id, &["meetings.read"])
        .await
        .unwrap();
    let r = gw
        .post(
            "/api/v2/instant-meetings",
            Auth::Credential(&reader),
            json!({}),
        )
        .await;
    status(&r, 403, "read-only credential writes");
    assert_eq!(r.code(), Some("INSUFFICIENT_SCOPE"), "{}", r.text);
    let r = gw
        .get("/api/v2/meeting-spaces", Auth::Credential(&reader))
        .await;
    status(&r, 200, "read-only credential lists its environment");
    assert_eq!(r.body["items"].as_array().unwrap().len(), 2);

    // 4. The developer's own CF Meet session is a human: it never sees the environment's
    //    meetings (and the platform credential never reaches the Meeting Service).
    let mine = gw
        .get("/api/v2/meeting-spaces", Auth::Session(&a.user.session))
        .await;
    status(&mine, 200, "CF Meet session lists its meetings");
    assert!(!mine.text.contains(&space.to_string()), "{}", mine.text);

    // 5. Revocation applies on the very next request (live re-verification).
    operator.revoke(a.customer.credential_id).await.unwrap();
    let r = gw.get("/api/v2/meeting-spaces", cred_a).await;
    status(&r, 401, "revoked credential");
    stack.teardown().await;
}

fn sealing_key() -> String {
    let mut b = [0u8; 32];
    rand::thread_rng().fill_bytes(&mut b);
    base64::engine::general_purpose::STANDARD.encode(b)
}

fn webhook_options() -> StackOptions {
    let mut o = StackOptions {
        runtime: Some(RuntimeEnvironment::Local),
        ..StackOptions::default()
    };
    let key = sealing_key();
    for p in [stack::DEVELOPER_PLATFORM, stack::DEVELOPER_PLATFORM_WORKER] {
        o = o
            .set(p, "DEVELOPER_PLATFORM_WEBHOOKS_ENABLED", "true")
            .set(p, "DEVELOPER_PLATFORM_WEBHOOK_DELIVERY_MODE", "active")
            .set(p, "DEVELOPER_PLATFORM_WEBHOOK_SECRET_KEY", key.clone())
            .set(p, "DEVELOPER_PLATFORM_WEBHOOK_SECRET_KEY_ID", "cert-k1")
            .set(p, "DEVELOPER_PLATFORM_WEBHOOK_REQUEST_TIMEOUT_MS", "5000")
            .set(
                p,
                "DEVELOPER_PLATFORM_WEBHOOK_ALLOW_PRIVATE_ENDPOINTS",
                "true",
            );
    }
    o.set(
        stack::MEETING_SERVICE,
        "MEETING_SERVICE_PUBLIC_EVENTS_DESTINATION",
        "developer-platform",
    )
    .set(
        stack::API_GATEWAY,
        "API_GATEWAY_CENTRAL_WEBHOOKS_ENABLED",
        "true",
    )
}

fn is_uuid(s: &str) -> bool {
    s.len() == 36 && Uuid::parse_str(s).is_ok()
}

fn is_code(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 64
        && s.bytes()
            .all(|b| b.is_ascii_uppercase() || b.is_ascii_digit() || b == b'_')
}

async fn subscribe(gw: &Gateway, secret: &str, url: &str, types: &[&str]) -> (Uuid, String) {
    let r = gw
        .post(
            "/v1/webhooks",
            Auth::Credential(secret),
            json!({ "url": url, "eventTypes": types, "description": "meetings certification" }),
        )
        .await;
    status(&r, 201, &format!("POST /v1/webhooks {url}"));
    let signing = r.str("/signingSecret");
    assert!(signing.starts_with("whsec_"), "{}", r.text);
    (r.uuid("/webhook/webhookId"), signing)
}

#[tokio::test]
async fn meeting_webhooks_are_delivered_signed_with_safe_fields() {
    let Some((_g, stack)) = start(webhook_options()).await else {
        return;
    };
    assert_eq!(stack.runtime, RuntimeEnvironment::Local);
    let gw = stack.gateway();
    let rx = Receiver::start().await.unwrap();
    let a = provision::developer_sandbox(&stack, &email("meethook-a"), SANDBOX_SCOPES)
        .await
        .unwrap();
    let b = provision::developer_sandbox(&stack, &email("meethook-b"), SANDBOX_SCOPES)
        .await
        .unwrap();
    let types = ["meeting.started", "meeting.ended", "participant.joined"];
    let (_wa, secret_a) = subscribe(&gw, &a.customer.secret, &rx.url("/a"), &types).await;
    let (_wb, secret_b) = subscribe(&gw, &b.customer.secret, &rx.url("/b"), &types).await;

    let title = format!("Confidential board review {}", tag());
    let (space, conference, code) = instant_meeting(&gw, &a.customer.secret, &title).await;
    // A guest joins the public meeting (its display name must never reach a webhook).
    let guest_name = format!("Guest Ada {}", tag());
    let join = reqwest::Client::builder()
        .no_proxy()
        .build()
        .unwrap()
        .post(format!(
            "{}/api/v2/conferences/{conference}/join",
            stack.url(stack::MEETING_SERVICE)
        ))
        .json(&json!({ "displayName": guest_name }))
        .send()
        .await
        .unwrap();
    assert!(join.status().is_success(), "guest join: {}", join.status());

    rx.wait_until("/a", WAIT, "meeting.started of A's meeting", |g| {
        g.iter().any(|r| {
            r.event_type() == "meeting.started" && r.json()["data"]["meetingId"] == json!(space)
        })
    })
    .await
    .unwrap();
    // Let stragglers (and any duplicate) arrive before checking.
    tokio::time::sleep(Duration::from_secs(3)).await;
    let got: Vec<Received> = rx.received("/a");
    assert!(!got.is_empty());
    let forbidden = [
        title.as_str(),
        guest_name.as_str(),
        code.as_str(),
        a.customer.secret.as_str(),
        secret_a.as_str(),
        a.user.session.as_str(),
        "cfmk_",
        "whsec_",
        "livekit",
        "wss://",
        "http",
    ];
    let mut event_ids = BTreeSet::new();
    for r in &got {
        r.verify(&secret_a)
            .unwrap_or_else(|e| panic!("{}: {e}", r.event_type()));
        assert!(r.verify(&secret_b).is_err(), "B's secret must not verify");
        assert!(r.event_id().starts_with("whe_"), "{}", r.event_id());
        let v = r.json();
        for k in v.as_object().unwrap().keys() {
            assert!(
                [
                    "id",
                    "type",
                    "apiVersion",
                    "occurredAt",
                    "data",
                    "correlationId"
                ]
                .contains(&k.as_str()),
                "unexpected top-level key {k}: {v}"
            );
        }
        for (k, val) in v["data"].as_object().unwrap() {
            let safe = match val {
                Value::String(s) => is_uuid(s) || is_code(s),
                Value::Number(n) => n.as_u64().is_some(),
                _ => false,
            };
            assert!(safe, "unsafe data {k}={val} in {v}");
        }
        let text = String::from_utf8_lossy(&r.body).to_string();
        for f in forbidden {
            assert!(!text.contains(f), "{f} leaked in {text}");
        }
        assert!(
            event_ids.insert(r.event_id().to_string()),
            "event {} delivered twice",
            r.event_id()
        );
    }
    let started = got
        .iter()
        .find(|r| r.event_type() == "meeting.started")
        .unwrap();
    let data = started.json()["data"].clone();
    assert_eq!(data["meetingId"], json!(space), "{data}");
    assert_eq!(data["status"], "LIVE", "{data}");
    // Tenant B never receives A's meeting events.
    let ids_b: Vec<String> = rx
        .received("/b")
        .iter()
        .map(|r| r.json()["data"].to_string())
        .collect();
    assert!(
        ids_b.iter().all(|d| !d.contains(&space.to_string())),
        "A's meeting reached B: {ids_b:?}"
    );
    // A CF Meet user's meeting never produces a platform event (nothing new for A).
    let before = rx.received("/a").len();
    let user_meeting = gw
        .post(
            "/api/v2/instant-meetings",
            Auth::Session(&a.user.session),
            json!({ "title": "my own" }),
        )
        .await;
    status(&user_meeting, 201, "CF Meet user instant meeting");
    tokio::time::sleep(Duration::from_secs(3)).await;
    assert_eq!(rx.received("/a").len(), before);
    stack.teardown().await;
}
