//! Part VI §21 — controlled rollout and independent rollback rehearsal on the REAL
//! processes (isolated certification stack; nothing external is routed or cut over).
//!
//! The stack starts DARK — every Part VI control off — and the documented release
//! sequence is applied one configuration change at a time, each verified before the
//! next (process restarts with new variables, exactly as a deployment would):
//!
//!  1. prerequisites: every service ready, migrations current;
//!  2. Developer Platform API on (`DEVELOPER_PLATFORM_GATEWAY_API_ENABLED`) — still
//!     unreachable from outside while the gateway is dark;
//!  3. public routing on (`API_GATEWAY_PUBLIC_ROUTING_ENABLED` / `_WEBSOCKETS_ENABLED`):
//!     sandbox smoke through the gateway;
//!  4. production-access workflow on (`DEVELOPER_PLATFORM_PRODUCTION_ACCESS_WORKFLOW_ENABLED`);
//!  5. central webhooks (`DEVELOPER_PLATFORM_WEBHOOKS_ENABLED` in shadow, then the
//!     gateway's `API_GATEWAY_CENTRAL_WEBHOOKS_ENABLED`) and central usage
//!     (`API_GATEWAY_CENTRAL_USAGE_ENABLED`);
//!
//! then every cutover is rolled back INDEPENDENTLY — usage, webhooks, one API family,
//! the production-access workflow, the Developer Platform API, public routing — each
//! time checking that the other controls and the domain services are untouched.
//!
//! Same prerequisites and skip rules as `tests/journeys.rs`.

use std::{
    sync::atomic::{AtomicBool, Ordering},
    time::Duration,
};

use base64::Engine;
use cf_service_auth::RuntimeEnvironment;
use cfmeet_certification::{
    provision::{self, Auth, Gateway, Resp, SANDBOX_SCOPES},
    receiver::Receiver,
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

fn status(r: &Resp, expected: u16, what: &str) {
    assert_eq!(r.status, expected, "{what}: {}", r.text);
}

/// Every Part VI control OFF (the gateway is deny-all), webhooks keyed but disabled.
fn dark() -> StackOptions {
    let mut b = [0u8; 32];
    rand::thread_rng().fill_bytes(&mut b);
    let key = base64::engine::general_purpose::STANDARD.encode(b);
    let mut o = StackOptions {
        runtime: Some(RuntimeEnvironment::Local),
        ..StackOptions::default()
    };
    for p in [stack::DEVELOPER_PLATFORM, stack::DEVELOPER_PLATFORM_WORKER] {
        o = o
            .set(p, "DEVELOPER_PLATFORM_GATEWAY_API_ENABLED", "false")
            .set(
                p,
                "DEVELOPER_PLATFORM_PRODUCTION_ACCESS_WORKFLOW_ENABLED",
                "false",
            )
            .set(p, "DEVELOPER_PLATFORM_WEBHOOKS_ENABLED", "false")
            .set(p, "DEVELOPER_PLATFORM_WEBHOOK_DELIVERY_MODE", "shadow")
            .set(p, "DEVELOPER_PLATFORM_WEBHOOK_SECRET_KEY", key.clone())
            .set(p, "DEVELOPER_PLATFORM_WEBHOOK_SECRET_KEY_ID", "rollout-k1")
            .set(
                p,
                "DEVELOPER_PLATFORM_WEBHOOK_ALLOW_PRIVATE_ENDPOINTS",
                "true",
            );
    }
    o.set(
        stack::CALLING,
        "CALLING_WEBHOOK_ALLOW_PRIVATE_ENDPOINTS",
        "true",
    )
    .set(
        stack::API_GATEWAY,
        "API_GATEWAY_PUBLIC_ROUTING_ENABLED",
        "false",
    )
    .set(
        stack::API_GATEWAY,
        "API_GATEWAY_WEBSOCKETS_ENABLED",
        "false",
    )
    .set(
        stack::API_GATEWAY,
        "API_GATEWAY_CENTRAL_WEBHOOKS_ENABLED",
        "false",
    )
    .set(
        stack::API_GATEWAY,
        "API_GATEWAY_CENTRAL_USAGE_ENABLED",
        "false",
    )
}

async fn ready(stack: &Stack, service: &str) -> Resp {
    Resp::from(
        stack
            .http
            .get(format!("{}/ready", stack.url(service)))
            .send()
            .await
            .unwrap(),
    )
    .await
}

async fn restart(stack: &mut Stack, process: &str, vars: &[(&str, &str)]) {
    stack
        .restart(process, vars)
        .await
        .unwrap_or_else(|e| panic!("{process} restart: {e:#}"));
}

/// Restarts the Developer Platform API and worker together (one deployment unit).
async fn restart_dp(stack: &mut Stack, vars: &[(&str, &str)]) {
    restart(stack, stack::DEVELOPER_PLATFORM, vars).await;
    restart(stack, stack::DEVELOPER_PLATFORM_WORKER, vars).await;
}

/// The families every certification credential may read without side effects.
const FAMILIES: &[&str] = &[
    "/v1/calls",
    "/v1/messaging/policy",
    "/v1/numbers/countries",
    "/v1/sms/messages",
    "/v1/developer-platform/context",
    "/v1/privacy-relay/contexts",
];

async fn families(gw: &Gateway, secret: &str) -> Vec<(&'static str, u16)> {
    let mut out = Vec::new();
    for p in FAMILIES {
        out.push((*p, gw.get(p, Auth::Credential(secret)).await.status));
    }
    out
}

fn limits_of(r: &Resp) -> Vec<String> {
    r.body["limits"]
        .as_array()
        .unwrap()
        .iter()
        .map(|l| l["limit"].as_str().unwrap().to_string())
        .collect()
}

#[tokio::test]
async fn rollout_and_independent_rollback_rehearsal() {
    let Some(config) = StackConfig::from_env().expect("certification prerequisites") else {
        return;
    };
    let _g = Serial::acquire().await;
    let mut stack = Stack::start_with(config, dark()).await.expect("dark stack");
    let operator = stack.operator();

    // 1. Prerequisites: every service ready (the Developer Platform: migrations current).
    for s in stack::STARTED_PROCESSES {
        assert_eq!(stack.readiness(s).await.unwrap(), 200, "{s} ready");
    }
    assert_eq!(
        ready(&stack, stack::DEVELOPER_PLATFORM).await.body["migrations"],
        "current"
    );
    let gw_ready = ready(&stack, stack::API_GATEWAY).await;
    assert_eq!(gw_ready.body["publicRouting"], "disabled");
    assert_eq!(gw_ready.body["routes"], 0, "dark gateway: deny-all");
    // A valid machine credential (operator-issued) reaches nothing while dark.
    let probe = operator
        .customer("EXTERNAL", "SANDBOX", SANDBOX_SCOPES)
        .await
        .unwrap();
    let gw = stack.gateway();
    assert!(
        families(&gw, &probe.secret)
            .await
            .iter()
            .all(|(_, s)| *s == 404),
        "dark gateway answers 404 for every family"
    );

    // 2. Developer Platform API on — still dark from outside.
    restart_dp(
        &mut stack,
        &[("DEVELOPER_PLATFORM_GATEWAY_API_ENABLED", "true")],
    )
    .await;
    assert_eq!(
        ready(&stack, stack::DEVELOPER_PLATFORM).await.body["gatewayApi"],
        "enabled"
    );
    assert_eq!(
        stack
            .gateway()
            .get(
                "/v1/developer-platform/context",
                Auth::Credential(&probe.secret)
            )
            .await
            .status,
        404
    );

    // 3. Public routing on: sandbox smoke through the gateway.
    restart(
        &mut stack,
        stack::API_GATEWAY,
        &[
            ("API_GATEWAY_PUBLIC_ROUTING_ENABLED", "true"),
            ("API_GATEWAY_WEBSOCKETS_ENABLED", "true"),
        ],
    )
    .await;
    let gw = stack.gateway();
    let r = ready(&stack, stack::API_GATEWAY).await;
    assert!(r.body["routes"].as_u64().unwrap() > 400, "{}", r.text);
    assert_eq!(r.body["platformAccess"], "reachable");
    let sb = provision::developer_sandbox(
        &stack,
        &format!("rollout-{}@cert.cfmeet.test", Uuid::new_v4().simple()),
        SANDBOX_SCOPES,
    )
    .await
    .expect("sandbox onboarding through the gateway");
    let cred = sb.customer.secret.clone();
    let session = Auth::Session(&sb.user.session);
    for (p, s) in families(&gw, &cred).await {
        assert_eq!(s, 200, "smoke {p}");
    }

    // 4. Production-access workflow: off -> 503, on -> the request is accepted.
    let submit = |gw: Gateway| {
        let (project, session_token) = (sb.project_id, sb.user.session.clone());
        async move {
            gw.post(
                &format!("/v1/projects/{project}/production-access-requests"),
                Auth::Session(&session_token),
                json!({ "capabilities": ["MEETINGS"], "useCaseSummary": "Rollout rehearsal" }),
            )
            .await
        }
    };
    let off = submit(gw.clone()).await;
    status(&off, 503, "production-access workflow disabled");
    assert_eq!(off.code(), Some("CAPABILITY_UNAVAILABLE"));
    restart_dp(
        &mut stack,
        &[(
            "DEVELOPER_PLATFORM_PRODUCTION_ACCESS_WORKFLOW_ENABLED",
            "true",
        )],
    )
    .await;
    let gw = stack.gateway();
    status(
        &submit(gw.clone()).await,
        201,
        "production-access workflow enabled",
    );

    // 5a. Central webhooks: the Developer Platform in shadow, then the gateway switch.
    let rx = Receiver::start().await.unwrap();
    let calling_hook = gw
        .post(
            "/v1/webhooks",
            Auth::Credential(&cred),
            json!({ "url": rx.url("/calling"), "eventTypes": ["conversation.created"] }),
        )
        .await;
    status(&calling_hook, 201, "Calling subscription (central off)");
    let calling_hook = calling_hook.str("/webhook/webhookId");
    restart_dp(
        &mut stack,
        &[("DEVELOPER_PLATFORM_WEBHOOKS_ENABLED", "true")],
    )
    .await;
    let gw = stack.gateway();
    let console = gw
        .post(
            &format!("/v1/environments/{}/webhooks", sb.environment_id),
            session,
            json!({ "url": rx.url("/dp"), "eventTypes": ["conversation.created"] }),
        )
        .await;
    status(&console, 201, "Developer Platform subscription (shadow)");
    let listed = |gw: Gateway, cred: String| async move {
        let r = gw.get("/v1/webhooks", Auth::Credential(&cred)).await;
        status(&r, 200, "GET /v1/webhooks");
        r.text
    };
    assert!(
        listed(gw.clone(), cred.clone())
            .await
            .contains(&calling_hook),
        "still Calling's"
    );
    restart(
        &mut stack,
        stack::API_GATEWAY,
        &[("API_GATEWAY_CENTRAL_WEBHOOKS_ENABLED", "true")],
    )
    .await;
    let gw = stack.gateway();
    assert!(
        !listed(gw.clone(), cred.clone())
            .await
            .contains(&calling_hook),
        "central: served by the Developer Platform"
    );
    // 5b. Central usage.
    let calling_limits = limits_of(&gw.get("/v1/limits", Auth::Credential(&cred)).await);
    assert!(
        calling_limits.contains(&"CONCURRENT_CALLS".to_string()),
        "{calling_limits:?}"
    );
    restart(
        &mut stack,
        stack::API_GATEWAY,
        &[("API_GATEWAY_CENTRAL_USAGE_ENABLED", "true")],
    )
    .await;
    let gw = stack.gateway();
    assert_eq!(
        limits_of(&gw.get("/v1/limits", Auth::Credential(&cred)).await),
        ["REQUEST_RATE"]
    );
    let r = ready(&stack, stack::API_GATEWAY).await;
    assert_eq!(
        (
            r.body["centralWebhooks"].as_bool(),
            r.body["centralUsage"].as_bool()
        ),
        (Some(true), Some(true))
    );

    // ---- Independent rollbacks ---------------------------------------------------
    // R1: central usage only.
    restart(
        &mut stack,
        stack::API_GATEWAY,
        &[("API_GATEWAY_CENTRAL_USAGE_ENABLED", "false")],
    )
    .await;
    let gw = stack.gateway();
    assert_eq!(
        limits_of(&gw.get("/v1/limits", Auth::Credential(&cred)).await),
        calling_limits
    );
    assert!(
        !listed(gw.clone(), cred.clone())
            .await
            .contains(&calling_hook),
        "webhooks stay central when usage rolls back"
    );
    // R2: central webhooks only (usage stays Calling's, the Developer Platform keeps its
    // subscription).
    restart(
        &mut stack,
        stack::API_GATEWAY,
        &[("API_GATEWAY_CENTRAL_WEBHOOKS_ENABLED", "false")],
    )
    .await;
    let gw = stack.gateway();
    assert!(
        listed(gw.clone(), cred.clone())
            .await
            .contains(&calling_hook),
        "back to Calling"
    );
    assert_eq!(
        limits_of(&gw.get("/v1/limits", Auth::Credential(&cred)).await),
        calling_limits
    );
    let console_list = gw
        .get(
            &format!("/v1/environments/{}/webhooks", sb.environment_id),
            session,
        )
        .await;
    status(&console_list, 200, "console webhooks after rollback");
    assert!(
        console_list.text.contains(&rx.url("/dp")),
        "no Developer Platform record lost"
    );
    // R3: one API family (SMS) only.
    restart(
        &mut stack,
        stack::API_GATEWAY,
        &[("API_GATEWAY_DISABLED_DOMAINS", "sms")],
    )
    .await;
    let gw = stack.gateway();
    for (p, s) in families(&gw, &cred).await {
        assert_eq!(
            s,
            if p.starts_with("/v1/sms") { 404 } else { 200 },
            "family rollback {p}"
        );
    }
    restart(
        &mut stack,
        stack::API_GATEWAY,
        &[("API_GATEWAY_DISABLED_DOMAINS", "")],
    )
    .await;
    let gw = stack.gateway();
    assert!(
        families(&gw, &cred).await.iter().all(|(_, s)| *s == 200),
        "family restored"
    );
    // R4: production-access workflow only — existing requests stay readable.
    restart_dp(
        &mut stack,
        &[(
            "DEVELOPER_PLATFORM_PRODUCTION_ACCESS_WORKFLOW_ENABLED",
            "false",
        )],
    )
    .await;
    let gw = stack.gateway();
    status(&submit(gw.clone()).await, 503, "workflow rolled back");
    let list = gw
        .get(
            &format!("/v1/projects/{}/production-access-requests", sb.project_id),
            session,
        )
        .await;
    status(&list, 200, "requests still readable");
    assert_eq!(
        list.body["items"].as_array().map(Vec::len),
        Some(1),
        "{}",
        list.text
    );
    // R5: the Developer Platform API only — the domain families keep working.
    restart_dp(
        &mut stack,
        &[("DEVELOPER_PLATFORM_GATEWAY_API_ENABLED", "false")],
    )
    .await;
    let gw = stack.gateway();
    for (p, s) in families(&gw, &cred).await {
        let expect = if p.starts_with("/v1/developer-platform") {
            404
        } else {
            200
        };
        assert_eq!(s, expect, "Developer Platform rollback {p}");
    }
    // R6: public routing — the gateway goes dark, every service stays healthy.
    restart(
        &mut stack,
        stack::API_GATEWAY,
        &[("API_GATEWAY_PUBLIC_ROUTING_ENABLED", "false")],
    )
    .await;
    let gw = stack.gateway();
    assert!(
        families(&gw, &cred).await.iter().all(|(_, s)| *s == 404),
        "dark again"
    );
    for s in stack::STARTED_PROCESSES {
        assert_eq!(
            stack.readiness(s).await.unwrap(),
            200,
            "{s} healthy after the rollbacks"
        );
    }
    // Re-enabling restores service without data loss (the subscription and request).
    restart_dp(
        &mut stack,
        &[("DEVELOPER_PLATFORM_GATEWAY_API_ENABLED", "true")],
    )
    .await;
    restart(
        &mut stack,
        stack::API_GATEWAY,
        &[("API_GATEWAY_PUBLIC_ROUTING_ENABLED", "true")],
    )
    .await;
    let gw = stack.gateway();
    let list = gw
        .get(
            &format!("/v1/projects/{}/production-access-requests", sb.project_id),
            session,
        )
        .await;
    status(&list, 200, "requests after re-enable");
    assert_eq!(list.body["items"].as_array().map(Vec::len), Some(1));
    let calls = gw.get("/v1/calls", Auth::Credential(&cred)).await;
    status(&calls, 200, "domain service after re-enable");
    let _: Value = calls.body;
    stack.teardown().await;
}
