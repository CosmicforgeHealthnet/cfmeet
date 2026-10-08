//! §27: Communication Context creation and validation.

mod common;

use axum::http::StatusCode;
use cf_service_auth::ApplicationEnvironmentKind as Kind;
use common::*;
use serde_json::{json, Value};

const CONTEXTS: &str = "/internal/v1/contexts";

#[tokio::test]
async fn valid_contexts_are_created_at_version_one() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let c = Consumer::new(Kind::Production);
    let [a, b] = principals(2).try_into().unwrap();

    let ctx = h.bidirectional(&c, &[a, b]).await;
    assert_eq!(ctx["version"], 1);
    assert_eq!(ctx["status"], "ACTIVE");
    assert_eq!(ctx["directionPolicy"], "BIDIRECTIONAL");
    assert_eq!(ctx["allowedChannels"], json!(["VOICE"]));
    assert_eq!(ctx["allowedRelayStrategies"], json!(["APP_ONLY"]));
    assert_eq!(ctx["participants"].as_array().unwrap().len(), 2);
    assert_eq!(ctx["policy"]["terminateActiveCallsOnRevoke"], true);
    assert_eq!(ctx["policy"]["graceAllowsNewCalls"], false);

    // Explicit directions, plus a declared (future) MESSAGING channel.
    let mut req = context_request(&[a, b], "EXPLICIT", &[(a, b)]);
    req["allowedChannels"] = json!(["VOICE", "MESSAGING"]);
    let explicit = h.create_context(&c, req).await;
    assert_eq!(explicit["directionPolicy"], "EXPLICIT");
    assert_eq!(explicit["directions"].as_array().unwrap().len(), 1);
    assert_eq!(explicit["allowedChannels"], json!(["MESSAGING", "VOICE"]));

    // ContactReference participants are stored as opaque references only.
    let contact = uuid::Uuid::new_v4();
    let mut req = context_request(&[a], "BIDIRECTIONAL", &[]);
    req["participants"] = json!([{ "principalId": a }, { "contactReferenceId": contact }]);
    let with_contact = h.create_context(&c, req).await;
    let p = with_contact["participants"]
        .as_array()
        .unwrap()
        .iter()
        .find(|p| p["subjectKind"] == "CONTACT_REFERENCE")
        .unwrap();
    assert_eq!(p["subjectKind"], "CONTACT_REFERENCE");
    assert_eq!(p["contactReferenceId"], json!(contact));
    assert!(p.get("principalId").is_none());

    // Exactly one context version / audit event / outbox row per creation.
    assert_eq!(
        h.count("SELECT count(*) FROM communication_contexts").await,
        3
    );
    assert_eq!(
        h.count("SELECT count(*) FROM relay_audit_events WHERE event_type = 'CONTEXT_CREATED'")
            .await,
        3
    );
    assert_eq!(h.count("SELECT count(*) FROM privacy_relay_outbox WHERE event_type = 'privacy-relay.context.created'").await, 3);
}

async fn rejected(h: &Harness, c: &Consumer, req: Value) -> (StatusCode, String) {
    let r = h.command(CONTEXTS, c, req).await;
    (r.status, r.code().to_string())
}

#[tokio::test]
async fn invalid_contexts_are_rejected() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let c = Consumer::new(Kind::Sandbox);
    let [a, b] = principals(2).try_into().unwrap();
    let base = context_request(&[a, b], "BIDIRECTIONAL", &[]);
    let bad = |f: &dyn Fn(&mut Value)| {
        let mut v = base.clone();
        f(&mut v);
        v
    };
    let validation = (StatusCode::BAD_REQUEST, "VALIDATION_ERROR".to_string());
    for (what, req) in [
        (
            "empty participant set",
            bad(&|v| v["participants"] = json!([])),
        ),
        (
            "single participant",
            bad(&|v| v["participants"] = json!([{ "principalId": a }])),
        ),
        (
            "duplicate participant",
            bad(&|v| v["participants"] = json!([{ "principalId": a }, { "principalId": a }])),
        ),
        (
            "both references",
            bad(&|v| v["participants"][0] = json!({ "principalId": a, "contactReferenceId": b })),
        ),
        (
            "starts after expires",
            bad(&|v| v["startsAt"] = json!(iso(at(90)))),
        ),
        (
            "starts equals expires",
            bad(&|v| v["startsAt"] = json!(iso(at(60)))),
        ),
        (
            "grace before expiry",
            bad(&|v| v["graceExpiresAt"] = json!(iso(at(30)))),
        ),
        (
            "expired at creation",
            bad(&|v| {
                v["startsAt"] = json!(iso(at(-120)));
                v["expiresAt"] = json!(iso(at(-60)));
            }),
        ),
        ("no channel", bad(&|v| v["allowedChannels"] = json!([]))),
        (
            "unknown channel",
            bad(&|v| v["allowedChannels"] = json!(["SMS"])),
        ),
        (
            "explicit without directions",
            bad(&|v| v["directionPolicy"] = json!("EXPLICIT")),
        ),
        (
            "directions with bidirectional",
            bad(&|v| {
                v["directions"] =
                    json!([{ "from": { "principalId": a }, "to": { "principalId": b } }])
            }),
        ),
        (
            "self direction",
            bad(&|v| {
                v["directionPolicy"] = json!("EXPLICIT");
                v["directions"] =
                    json!([{ "from": { "principalId": a }, "to": { "principalId": a } }]);
            }),
        ),
        (
            "arbitrary policy blob",
            bad(&|v| {
                v["policy"] = json!({ "anything": { "deeply": { "nested": [1, { "x": true }] } } })
            }),
        ),
        (
            "policy of the wrong type",
            bad(&|v| v["policy"] = json!({ "graceAllowsNewCalls": "always" })),
        ),
        (
            "phone-number reference",
            bad(&|v| v["externalContextReference"] = json!("+2348012345678")),
        ),
        (
            "digits-only phone reference",
            bad(&|v| v["externalContextReference"] = json!("2348012345678")),
        ),
        (
            "e-mail reference",
            bad(&|v| v["externalContextReference"] = json!("alice@example.com")),
        ),
        (
            "sip reference",
            bad(&|v| v["externalContextReference"] = json!("sip:alice")),
        ),
        (
            "unbounded reference",
            bad(&|v| v["externalContextReference"] = json!("x".repeat(200))),
        ),
        (
            "phone in contact reference",
            bad(&|v| v["participants"][1] = json!({ "contactReferenceId": "+2348012345678" })),
        ),
        (
            "telephone participant",
            bad(&|v| v["participants"][1] = json!({ "phoneNumber": "+2348012345678" })),
        ),
        (
            "client-supplied tenant",
            bad(&|v| v["tenantId"] = json!(uuid::Uuid::new_v4())),
        ),
        (
            "client-supplied application",
            bad(&|v| v["applicationId"] = json!(uuid::Uuid::new_v4())),
        ),
        (
            "client-supplied environment",
            bad(&|v| v["environmentId"] = json!(uuid::Uuid::new_v4())),
        ),
        (
            "client-supplied status",
            bad(&|v| v["status"] = json!("ACTIVE")),
        ),
        ("client-supplied version", bad(&|v| v["version"] = json!(7))),
        (
            "client-supplied authorization",
            bad(&|v| v["authorizationResult"] = json!("AUTHORIZED")),
        ),
        ("provider", bad(&|v| v["provider"] = json!("carrier-x"))),
        (
            "relay number",
            bad(&|v| v["relayNumber"] = json!("+2348012345678")),
        ),
        (
            "sip uri",
            bad(&|v| v["sipUri"] = json!("sip:alice@example.com")),
        ),
    ] {
        assert_eq!(rejected(&h, &c, req).await, validation, "{what}");
    }
    // Nothing was persisted by any refused request.
    assert_eq!(
        h.count("SELECT count(*) FROM communication_contexts").await,
        0
    );
    assert_eq!(
        h.count("SELECT count(*) FROM privacy_relay_commands").await,
        0
    );
}

#[tokio::test]
async fn one_context_per_external_reference() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let c = Consumer::new(Kind::Production);
    let [a, b] = principals(2).try_into().unwrap();
    let mut req = context_request(&[a, b], "BIDIRECTIONAL", &[]);
    req["externalContextReference"] = json!("delivery-77");
    h.create_context(&c, req.clone()).await;
    let r = h.command(CONTEXTS, &c, req.clone()).await;
    assert_eq!(
        (r.status, r.code()),
        (StatusCode::CONFLICT, "CONTEXT_REFERENCE_CONFLICT")
    );
    // The same reference in another environment is a different context.
    h.create_context(&c.sibling_environment(), req.clone())
        .await;
    // Concurrent creations of one reference (different keys): exactly one wins.
    req["externalContextReference"] = json!("delivery-race");
    let statuses = futures::future::join_all((0..8).map(|_| {
        let (req, c, h) = (req.clone(), c.clone(), &h);
        async move { h.command(CONTEXTS, &c, req).await.status }
    }))
    .await;
    assert_eq!(
        statuses
            .iter()
            .filter(|s| **s == StatusCode::CREATED)
            .count(),
        1,
        "{statuses:?}"
    );
    assert!(statuses
        .iter()
        .all(|s| *s == StatusCode::CREATED || *s == StatusCode::CONFLICT));
}
