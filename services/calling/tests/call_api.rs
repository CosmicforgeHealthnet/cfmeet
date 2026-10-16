//! Safe REST retrieval / listing (Part C, Workstream D): `GET /v1/calls` and
//! `GET /v1/calls/{call_id}`.
//!
//! * `calls.read` required (403 INSUFFICIENT_SCOPE otherwise, before any lookup);
//!   invalid credentials get the normalized 401;
//! * scope comes only from the credential: foreign tenant / application / environment
//!   Calls answer exactly like missing ones (404), and listings only contain the caller's
//!   Calls;
//! * keyset pagination is stable (no duplicates, no gaps); filters are bounded and strict;
//! * no protected, provider or internal fields; no mutation routes.

mod common;

use std::collections::BTreeSet;

use axum::http::{Method, StatusCode};
use calling::{
    calls::{kinds::CallKind, service::NewCall, CallAction, CallState},
    domain::{Clock, EnvironmentKind},
    platform::scopes::names,
};
use chrono::Duration;
use common::{calls::CallWorld, Harness};
use serde_json::Value;

fn ids(page: &Value) -> Vec<String> {
    page["data"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| c["callId"].as_str().unwrap().to_string())
        .collect()
}

fn keys(value: &Value, out: &mut BTreeSet<String>) {
    match value {
        Value::Object(map) => {
            for (k, v) in map {
                out.insert(k.clone());
                keys(v, out);
            }
        }
        Value::Array(items) => items.iter().for_each(|i| keys(i, out)),
        _ => {}
    }
}

#[tokio::test]
async fn call_detail_requires_calls_read_and_returns_the_safe_read_model() {
    let Some(w) = CallWorld::new().await else {
        return;
    };
    let call = w.call_in(CallState::Connected).await;
    let path = format!("/v1/calls/{}", call.id);
    let ok =
        w.h.request(Method::GET, &path, Some(w.secret()), None)
            .await;
    assert_eq!(ok.status, StatusCode::OK, "{}", ok.text);
    let body = &ok.json;
    assert_eq!(body["callId"], call.id.to_string());
    assert_eq!(body["state"], "CONNECTED");
    assert_eq!(body["version"], call.version);
    assert_eq!(body["kind"], "VOICE");
    assert_eq!(body["initiatorPrincipalId"], w.initiator.id().to_string());
    assert_eq!(body["participants"].as_array().unwrap().len(), 2);
    assert_eq!(body["sessions"].as_array().unwrap().len(), 2);
    assert!(body["connectedAt"].is_string());
    assert!(body["endedAt"].is_null());
    assert_eq!(ok.headers["cache-control"], "no-store");

    // No protected, provider or internal values.
    let mut all = BTreeSet::new();
    keys(body, &mut all);
    for key in &all {
        let lower = key.to_lowercase();
        for forbidden in [
            "mediaidentity",
            "identity",
            "binding",
            "provider",
            "carrier",
            "phone",
            "contact",
            "credential",
            "secret",
            "token",
            "tenant",
            "application",
            "environment",
            "payload",
            "room",
            "sip",
        ] {
            assert!(!lower.contains(forbidden), "field {key}");
        }
    }
    for session in w.calls().sessions(w.scope(), call.id).await.unwrap() {
        assert!(!ok.text.contains(session.media_identity.as_str()));
    }
    assert!(!ok.text.contains(w.secret()));
    assert!(
        !ok.text.contains("initiator-1"),
        "principal references are not exposed"
    );

    // Without calls.read: 403 before any lookup (also for unknown ids).
    let no_scope =
        w.h.issue(
            w.consumer.sa(EnvironmentKind::Production),
            &[names::PRINCIPALS_READ],
        )
        .await;
    for p in [
        path.clone(),
        format!("/v1/calls/{}", uuid::Uuid::new_v4()),
        "/v1/calls".into(),
    ] {
        let r =
            w.h.request(Method::GET, &p, Some(no_scope.secret.expose_once()), None)
                .await;
        assert_eq!(r.status, StatusCode::FORBIDDEN, "{p}");
        assert_eq!(r.json["error"]["code"], "INSUFFICIENT_SCOPE");
        assert!(r.headers["www-authenticate"]
            .to_str()
            .unwrap()
            .contains("scope=\"calls.read\""));
    }
    // calls.write alone is not read access either.
    let write_only =
        w.h.issue(
            w.consumer.sa(EnvironmentKind::Production),
            &[names::CALLS_WRITE],
        )
        .await;
    assert_eq!(
        w.h.request(
            Method::GET,
            &path,
            Some(write_only.secret.expose_once()),
            None
        )
        .await
        .status,
        StatusCode::FORBIDDEN
    );
    // Invalid / missing credentials: the normalized 401.
    let missing = w.h.request(Method::GET, &path, None, None).await;
    let invalid =
        w.h.request(
            Method::GET,
            &path,
            Some("cfmk_live_000000000000000000000000_00"),
            None,
        )
        .await;
    let mut tampered = w.secret().to_string();
    let last = tampered.pop().unwrap();
    tampered.push(if last == '0' { '1' } else { '0' });
    let tampered = w.h.request(Method::GET, &path, Some(&tampered), None).await;
    for r in [&missing, &invalid, &tampered] {
        assert_eq!(r.status, StatusCode::UNAUTHORIZED);
        assert_eq!(r.json["error"]["code"], "AUTHENTICATION_FAILED");
        assert_eq!(r.normalized_error(), missing.normalized_error());
    }
}

#[tokio::test]
async fn foreign_calls_are_indistinguishable_from_missing_ones() {
    let Some(w) = CallWorld::new().await else {
        return;
    };
    let mine = w.create().await.call;
    let shared = |h: &Harness| Harness::on(h.db.clone(), common::TEST_PEPPER, h.clock.clone());
    let other_tenant =
        CallWorld::in_harness(shared(&w.h), "beta", EnvironmentKind::Production).await;
    let other_app = CallWorld::for_consumer(
        shared(&w.h),
        w.h.application_in(&w.consumer.tenant, "second").await,
        EnvironmentKind::Production,
    )
    .await;
    let sandbox = CallWorld::for_consumer(
        shared(&w.h),
        w.h.application_in(&w.consumer.tenant, "third").await,
        EnvironmentKind::Sandbox,
    )
    .await;
    // Same application, SANDBOX vs PRODUCTION.
    let same_app_sandbox = {
        let token =
            w.h.issue(
                w.consumer.sa(EnvironmentKind::Sandbox),
                &[names::CALLS_READ],
            )
            .await;
        token.secret.expose_once().to_string()
    };
    let missing =
        w.h.request(
            Method::GET,
            &format!("/v1/calls/{}", uuid::Uuid::new_v4()),
            Some(w.secret()),
            None,
        )
        .await;
    let malformed =
        w.h.request(Method::GET, "/v1/calls/not-a-call", Some(w.secret()), None)
            .await;
    assert_eq!(missing.status, StatusCode::NOT_FOUND);
    assert_eq!(malformed.normalized_error(), missing.normalized_error());
    for (who, secret) in [
        ("other tenant", other_tenant.secret().to_string()),
        ("other application", other_app.secret().to_string()),
        ("other application sandbox", sandbox.secret().to_string()),
        ("same application sandbox", same_app_sandbox),
    ] {
        let r =
            w.h.request(
                Method::GET,
                &format!("/v1/calls/{}", mine.id),
                Some(&secret),
                None,
            )
            .await;
        assert_eq!(r.status, StatusCode::NOT_FOUND, "{who}");
        assert_eq!(r.normalized_error(), missing.normalized_error(), "{who}");
        let list =
            w.h.request(Method::GET, "/v1/calls", Some(&secret), None)
                .await;
        assert_eq!(list.status, StatusCode::OK);
        assert!(
            !ids(&list.json).contains(&mine.id.to_string()),
            "{who} lists it"
        );
    }
    // And the owner still sees it.
    assert_eq!(
        w.h.request(
            Method::GET,
            &format!("/v1/calls/{}", mine.id),
            Some(w.secret()),
            None
        )
        .await
        .status,
        StatusCode::OK
    );
}

#[tokio::test]
async fn listings_contain_only_the_callers_calls() {
    let Some(w) = CallWorld::new().await else {
        return;
    };
    let mut mine = BTreeSet::new();
    for _ in 0..3 {
        mine.insert(w.create().await.call.id.to_string());
    }
    let shared = Harness::on(w.h.db.clone(), common::TEST_PEPPER, w.h.clock.clone());
    let other = CallWorld::in_harness(shared, "gamma", EnvironmentKind::Production).await;
    for _ in 0..2 {
        other.create().await;
    }
    let sandbox = CallWorld::for_consumer(
        Harness::on(w.h.db.clone(), common::TEST_PEPPER, w.h.clock.clone()),
        w.h.application_in(&w.consumer.tenant, "sbx-app").await,
        EnvironmentKind::Sandbox,
    )
    .await;
    sandbox.create().await;
    let r =
        w.h.request(Method::GET, "/v1/calls", Some(w.secret()), None)
            .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    assert_eq!(ids(&r.json).into_iter().collect::<BTreeSet<_>>(), mine);
    assert!(r.json["nextCursor"].is_null());
    // Query parameters can never widen the scope.
    for q in ["tenantId", "applicationId", "environmentId", "tenant_id"] {
        let r =
            w.h.request(
                Method::GET,
                &format!("/v1/calls?{q}={}", other.consumer.tenant.id),
                Some(w.secret()),
                None,
            )
            .await;
        assert_eq!(r.status, StatusCode::BAD_REQUEST, "{q}");
        assert_eq!(r.json["error"]["code"], "VALIDATION_ERROR");
    }
}

#[tokio::test]
async fn keyset_pagination_is_stable_without_duplicates_or_gaps() {
    let Some(w) = CallWorld::new().await else {
        return;
    };
    let mut created = Vec::new();
    for i in 0..23 {
        // Several calls share a timestamp: the id breaks ties.
        if i % 3 == 0 {
            w.h.clock.advance(Duration::seconds(1));
        }
        let c = w.create().await.call;
        created.push((c.created_at, c.id.to_string()));
    }
    created.sort();
    created.reverse();
    let expected: Vec<String> = created.into_iter().map(|(_, id)| id).collect();
    let mut seen = Vec::new();
    let mut cursor: Option<String> = None;
    let mut pages = 0;
    loop {
        let path = match &cursor {
            Some(c) => format!("/v1/calls?limit=5&cursor={c}"),
            None => "/v1/calls?limit=5".to_string(),
        };
        let r =
            w.h.request(Method::GET, &path, Some(w.secret()), None)
                .await;
        assert_eq!(r.status, StatusCode::OK, "{}", r.text);
        let page = ids(&r.json);
        assert!(page.len() <= 5);
        seen.extend(page);
        pages += 1;
        match r.json["nextCursor"].as_str() {
            Some(c) => cursor = Some(c.to_string()),
            None => break,
        }
    }
    assert_eq!(pages, 5);
    assert_eq!(seen, expected, "stable order, no duplicates, no gaps");
    // The same cursor yields the same page.
    let first =
        w.h.request(Method::GET, "/v1/calls?limit=7", Some(w.secret()), None)
            .await;
    let c = first.json["nextCursor"].as_str().unwrap();
    let a =
        w.h.request(
            Method::GET,
            &format!("/v1/calls?limit=7&cursor={c}"),
            Some(w.secret()),
            None,
        )
        .await;
    let b =
        w.h.request(
            Method::GET,
            &format!("/v1/calls?limit=7&cursor={c}"),
            Some(w.secret()),
            None,
        )
        .await;
    assert_eq!(ids(&a.json), ids(&b.json));
    assert_eq!(ids(&a.json), expected[7..14]);
}

#[tokio::test]
async fn list_filters_are_bounded_and_strict() {
    let Some(w) = CallWorld::new().await else {
        return;
    };
    let t0 = w.h.clock.now();
    let cancelled = w.create().await.call;
    w.act(cancelled.id, CallAction::Cancel).await.unwrap();
    w.h.clock.advance(Duration::seconds(10));
    let other_initiator = w.principal("someone-else").await;
    let theirs = w
        .calls()
        .create_requested_call(&w.ctx, &other_initiator, NewCall::of_kind(CallKind::Video))
        .await
        .unwrap()
        .call;
    let get = |q: String| {
        let w = &w;
        async move {
            w.h.request(
                Method::GET,
                &format!("/v1/calls?{q}"),
                Some(w.secret()),
                None,
            )
            .await
        }
    };
    assert_eq!(
        ids(&get("state=CANCELLED".into()).await.json),
        [cancelled.id.to_string()]
    );
    assert_eq!(
        ids(&get("state=REQUESTED".into()).await.json),
        [theirs.id.to_string()]
    );
    assert_eq!(
        ids(
            &get(format!("initiatorPrincipalId={}", other_initiator.id()))
                .await
                .json
        ),
        [theirs.id.to_string()]
    );
    let after = (t0 + Duration::seconds(5)).to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
    assert_eq!(
        ids(&get(format!("createdAfter={after}")).await.json),
        [theirs.id.to_string()]
    );
    assert_eq!(
        ids(&get(format!("createdBefore={after}")).await.json),
        [cancelled.id.to_string()]
    );
    assert_eq!(get("limit=100".into()).await.status, StatusCode::OK);
    for q in [
        "limit=101",
        "limit=0",
        "limit=-1",
        "limit=abc",
        "state=LIVEKIT_JOINED",
        "state=connected",
        "cursor=zzz",
        "createdAfter=yesterday",
        "initiatorPrincipalId=x",
        "limit=5&limit=6",
        "filter=state%3DCONNECTED",
        "orderBy=id",
    ] {
        let r = get(q.into()).await;
        assert_eq!(r.status, StatusCode::BAD_REQUEST, "{q}");
        assert_eq!(r.json["error"]["code"], "VALIDATION_ERROR", "{q}");
    }
}

/// Calls are created and controlled only through explicit commands (Part C §55): no
/// route sets a state, and no command body accepts one.
#[tokio::test]
async fn there_is_no_generic_call_state_mutation() {
    let Some(w) = CallWorld::new().await else {
        return;
    };
    let call = w.create().await.call;
    for (method, path, expected) in [
        (
            Method::POST,
            "/v1/calls".to_string(),
            StatusCode::BAD_REQUEST,
        ),
        (
            Method::PATCH,
            format!("/v1/calls/{}", call.id),
            StatusCode::METHOD_NOT_ALLOWED,
        ),
        (
            Method::PUT,
            format!("/v1/calls/{}", call.id),
            StatusCode::METHOD_NOT_ALLOWED,
        ),
        (
            Method::DELETE,
            format!("/v1/calls/{}", call.id),
            StatusCode::METHOD_NOT_ALLOWED,
        ),
        (
            Method::POST,
            format!("/v1/calls/{}/answer", call.id),
            StatusCode::NOT_FOUND,
        ),
        (
            Method::POST,
            format!("/v1/calls/{}/state", call.id),
            StatusCode::NOT_FOUND,
        ),
        (
            Method::POST,
            format!("/v1/calls/{}/cancel", call.id),
            StatusCode::BAD_REQUEST,
        ),
        (
            Method::POST,
            format!("/v1/calls/{}/end", call.id),
            StatusCode::BAD_REQUEST,
        ),
        (
            Method::POST,
            format!("/v1/calls/{}/hold", call.id),
            StatusCode::BAD_REQUEST,
        ),
    ] {
        let r =
            w.h.request(
                method.clone(),
                &path,
                Some(w.secret()),
                Some(serde_json::json!({"state": "CONNECTED"})),
            )
            .await;
        assert_eq!(r.status, expected, "{method} {path}");
    }
    assert_eq!(w.call(call.id).await.state, CallState::Requested);
}
