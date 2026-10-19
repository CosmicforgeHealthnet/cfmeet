//! Part V C.8 / C.9 / C.13: probes, authentication boundaries, internal routes, the
//! served contract, error hygiene and bounded authorization caching.

mod common;

use std::sync::Arc;

use axum::{
    body::Body,
    http::{Method, Request, StatusCode},
};
use common::*;
use messaging::identity::cache::{CachingAuthority, FeedPage, Freshness, IdentityFeed};
use serde_json::{json, Value};
use uuid::Uuid;

#[tokio::test]
async fn health_and_readiness_reflect_liveness_and_migrations() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let r = h
        .raw(Request::get("/health").body(Body::empty()).unwrap())
        .await;
    assert_eq!(r.status, StatusCode::OK);
    conforms("messaging/common/v1.schema.json#/$defs/Health", &r.json);
    let r = h
        .raw(Request::get("/ready").body(Body::empty()).unwrap())
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    conforms("messaging/common/v1.schema.json#/$defs/Readiness", &r.json);
    assert_eq!(r.json["migrations"], "current");
    assert!(!r.text.contains("postgres://") && !r.text.contains(&h.db.name));
    // An unmigrated database is NOT ready (and says why without details).
    let empty = Arc::new(TestDb::empty().await.unwrap());
    let unmigrated = Harness::on(empty, |_| {});
    let r = unmigrated
        .raw(Request::get("/ready").body(Body::empty()).unwrap())
        .await;
    assert_eq!(r.status, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(r.json["status"], "not_ready");
    conforms("messaging/common/v1.schema.json#/$defs/Readiness", &r.json);
}

#[tokio::test]
async fn customer_routes_fail_closed() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let c = h.customer(&["messages.send", "messages.read"]);
    let alice = h.principal(&c);
    let path = format!("/v1/messaging/principals/{alice}/devices");
    // No credential, unknown credential, wrong scheme, two Authorization headers.
    for headers in [
        vec![],
        vec![(
            "authorization",
            "Bearer cfmk_test_not_a_real_credential".to_string(),
        )],
        vec![("authorization", format!("Basic {}", c.token))],
        vec![
            ("authorization", format!("Bearer {}", c.token)),
            ("authorization", format!("Bearer {}", c.token)),
        ],
    ] {
        let mut b = Request::get(&path).header("cf-acting-principal", alice.to_string());
        for (k, v) in &headers {
            b = b.header(*k, v);
        }
        let r = h.raw(b.body(Body::empty()).unwrap()).await;
        assert_eq!(r.status, StatusCode::UNAUTHORIZED, "{headers:?}");
        assert_eq!(r.code(), "AUTHENTICATION_FAILED");
        conforms("messaging/common/v1.schema.json#/$defs/Error", &r.json);
        assert!(r.headers.get("www-authenticate").is_some());
    }
    // A client token is refused explicitly (no fake or permanent client tokens).
    let r = h
        .raw(
            Request::get(&path)
                .header("authorization", "CF-Client-Token abc.def.ghi")
                .body(Body::empty())
                .unwrap(),
        )
        .await;
    assert_eq!(
        (r.status, r.code()),
        (StatusCode::UNAUTHORIZED, "CLIENT_TOKENS_NOT_ENABLED")
    );
    // A service token never authenticates a customer route.
    let r = h
        .raw(
            Request::get(&path)
                .header("authorization", format!("Bearer {}", c.token))
                .header("x-cf-service-authorization", "Bearer x")
                .body(Body::empty())
                .unwrap(),
        )
        .await;
    assert_eq!(r.status, StatusCode::UNAUTHORIZED);
    // A suspended tenant / application / environment (SAFETY_ONLY authority).
    let mut suspended = c.context.clone();
    suspended.full_authority = false;
    let s = h.register_token(suspended);
    assert_eq!(
        h.get(&s, Some(alice), &path).await.code(),
        "AUTHORITY_SUSPENDED"
    );
    // Platform Access down: 503, never a cached or default authority.
    *h.authority.down.lock().unwrap() = true;
    assert_eq!(
        h.get(&c, Some(alice), &path).await.status,
        StatusCode::SERVICE_UNAVAILABLE
    );
    // Request ids are returned; error bodies never echo the credential.
    *h.authority.down.lock().unwrap() = false;
    let r = h.get(&c, Some(alice), "/v1/messaging/does-not-exist").await;
    assert_eq!(r.status, StatusCode::NOT_FOUND);
    assert!(r.headers.get("x-request-id").is_some());
    assert!(!r.text.contains(&c.token));
}

#[tokio::test]
async fn tenant_identity_never_comes_from_the_request() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let a = h.customer(&["messages.send"]);
    let b = h.customer(&["messages.send"]);
    let alice = h.principal(&a);
    // Body fields naming another tenant are refused (deny_unknown_fields) and never used.
    let r = h
        .post(
            &a,
            Some(alice),
            "/v1/messaging/devices",
            json!({ "challengeId": Uuid::new_v4(), "keyPackage": "AAAA", "tenantId": b.context.scope.tenant_id }),
        )
        .await;
    assert_eq!(r.code(), "VALIDATION_ERROR");
    // Headers claiming a tenant / environment are ignored: the challenge lands in A.
    let r = h
        .send(
            Method::POST,
            Some(&a.token),
            Some(alice),
            "/v1/messaging/devices/registration-challenges",
            Some(json!({})),
            &[
                ("x-tenant-id", &b.context.scope.tenant_id.to_string()),
                (
                    "x-environment-id",
                    &b.context.scope.environment_id.to_string(),
                ),
            ],
        )
        .await;
    assert_eq!(r.status, StatusCode::CREATED);
    assert_eq!(
        h.scalar(&format!(
            "SELECT count(*) FROM messaging_device_challenges WHERE tenant_id = '{}'",
            a.context.scope.tenant_id
        ))
        .await,
        1
    );
}

#[tokio::test]
async fn internal_routes_require_a_trusted_service_token() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let c = h.customer(&["messages.read"]);
    for path in ["/internal/v1/anything", "/internal/v1/devices"] {
        let r = h.raw(Request::get(path).body(Body::empty()).unwrap()).await;
        assert_eq!(
            (r.status, r.code()),
            (StatusCode::UNAUTHORIZED, "INTERNAL_AUTHENTICATION_FAILED"),
            "{path}"
        );
        // A customer credential is never an internal credential.
        let r = h
            .raw(
                Request::get(path)
                    .header("authorization", format!("Bearer {}", c.token))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await;
        assert_eq!(r.status, StatusCode::UNAUTHORIZED);
        // A correctly signing service the trust bundle does not list (forgery).
        let r = h.internal(&h.untrusted, path).await;
        assert_eq!(
            (r.status, r.code()),
            (StatusCode::UNAUTHORIZED, "INTERNAL_AUTHENTICATION_FAILED")
        );
    }
}

#[tokio::test]
async fn bodies_are_bounded_and_rejections_normalized() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let c = h.customer(&["messages.send"]);
    let alice = h.principal(&c);
    let big = "A".repeat(messaging::http::MAX_BODY_BYTES + 10);
    let r = h
        .raw(
            Request::post("/v1/messaging/devices")
                .header("authorization", format!("Bearer {}", c.token))
                .header("cf-acting-principal", alice.to_string())
                .header("content-type", "application/json")
                .body(Body::from(format!("{{\"keyPackage\":\"{big}\"}}")))
                .unwrap(),
        )
        .await;
    assert_eq!(r.code(), "PAYLOAD_TOO_LARGE");
    let r = h
        .raw(
            Request::post("/v1/messaging/devices")
                .header("authorization", format!("Bearer {}", c.token))
                .header("cf-acting-principal", alice.to_string())
                .header("content-type", "application/json")
                .body(Body::from("{not json"))
                .unwrap(),
        )
        .await;
    assert_eq!(r.code(), "VALIDATION_ERROR");
    assert!(!r.text.contains("not json"));
    let r = h
        .post(
            &c,
            Some(alice),
            "/v1/messaging/devices/not-a-uuid/revoke",
            json!({ "reason": "REPLACED" }),
        )
        .await;
    assert_eq!(r.code(), "VALIDATION_ERROR");
}

#[tokio::test]
async fn the_served_contract_lists_exactly_the_implemented_routes() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let r = h
        .raw(Request::get("/openapi.json").body(Body::empty()).unwrap())
        .await;
    assert_eq!(r.status, StatusCode::OK);
    let api: Value = serde_json::from_str(messaging::http::OPENAPI).unwrap();
    assert_eq!(r.json, api);
    let c = h.customer(&[]);
    let mut documented = 0;
    for (path, item) in api["paths"].as_object().unwrap() {
        for method in item.as_object().unwrap().keys() {
            documented += 1;
            let mut concrete = path.clone();
            while let (Some(a), Some(b)) = (concrete.find('{'), concrete.find('}')) {
                concrete.replace_range(a..=b, &Uuid::new_v4().to_string());
            }
            let m = Method::from_bytes(method.to_uppercase().as_bytes()).unwrap();
            let r = h
                .send(
                    m.clone(),
                    Some(&c.token),
                    None,
                    &concrete,
                    Some(json!({})),
                    &[],
                )
                .await;
            // Registered: never "no such route" / method-not-allowed.
            assert_ne!(r.status, StatusCode::METHOD_NOT_ALLOWED, "{method} {path}");
            assert!(
                !(r.status == StatusCode::NOT_FOUND
                    && r.json["error"]["message"] == messaging::http::NO_ROUTE_MESSAGE),
                "{method} {path} not routed"
            );
        }
    }
    assert!(documented >= 9);
    // And every route the sources register is documented (`:param` ~ `{param}`) — in
    // the served customer contract or the internal contract (`/internal/v1`).
    let internal: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../../packages/contracts/messaging/internal/v1.openapi.json"),
        )
        .unwrap(),
    )
    .unwrap();
    let normalized: Vec<String> = api["paths"]
        .as_object()
        .unwrap()
        .keys()
        .chain(internal["paths"].as_object().unwrap().keys())
        .map(|p| {
            p.split('/')
                .map(|s| if s.starts_with('{') { "*" } else { s })
                .collect::<Vec<_>>()
                .join("/")
        })
        .collect();
    fn walk(dir: &std::path::Path, out: &mut Vec<String>) {
        for e in std::fs::read_dir(dir).unwrap() {
            let p = e.unwrap().path();
            if p.is_dir() {
                walk(&p, out);
            } else if p.extension().is_some_and(|x| x == "rs") {
                out.push(std::fs::read_to_string(p).unwrap());
            }
        }
    }
    let mut sources = Vec::new();
    walk(
        &std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src"),
        &mut sources,
    );
    let mut routes = 0;
    for code in sources {
        for (i, _) in code.match_indices(".route(\"") {
            let rest = &code[i + 8..];
            let route = &rest[..rest.find('"').unwrap()];
            let route = route
                .split('/')
                .map(|s| if s.starts_with(':') { "*" } else { s })
                .collect::<Vec<_>>()
                .join("/");
            let route = route.trim_end_matches('/').to_string();
            routes += 1;
            // The nest prefix itself (`/v1/conversations`) and the separate operations
            // listener (`/metrics`, never on the API port) are not API routes.
            if route.is_empty() || route == "/metrics" {
                continue;
            }
            assert!(
                normalized.iter().any(|d| d.ends_with(&route)),
                "registered route {route} is not in the served contract"
            );
        }
    }
    assert!(routes >= 20, "walked the real route table ({routes})");
    // And an undocumented path is not routed.
    let r = h.get(&c, None, "/v1/messaging/admin").await;
    assert_eq!(
        r.json["error"]["message"],
        messaging::http::NO_ROUTE_MESSAGE
    );
}

/// A scripted change feed.
struct Feed {
    pages: std::sync::Mutex<Vec<Result<FeedPage, ()>>>,
}

#[async_trait::async_trait]
impl IdentityFeed for Feed {
    async fn page(
        &self,
        _after: i64,
    ) -> Result<FeedPage, messaging::identity::AuthorityUnavailable> {
        let mut pages = self.pages.lock().unwrap();
        match if pages.is_empty() {
            Ok(FeedPage {
                changes: 0,
                next_after: 10,
                has_more: false,
            })
        } else {
            pages.remove(0)
        } {
            Ok(p) => Ok(p),
            Err(()) => Err(messaging::identity::AuthorityUnavailable("down".into())),
        }
    }
}

#[tokio::test]
async fn cached_authorization_is_bounded_invalidated_and_never_used_for_mutations() {
    let authority = Arc::new(FakeAuthority::default());
    let token = "cfmk_test_cache".to_string();
    let ctx = messaging::identity::TrustedContext {
        scope: messaging::identity::OwnerScope {
            tenant_id: Uuid::new_v4(),
            application_id: Uuid::new_v4(),
            environment_id: Uuid::new_v4(),
            environment_kind: messaging::identity::EnvironmentKind::Test,
            service_account_id: Uuid::new_v4(),
        },
        credential_id: Uuid::new_v4(),
        first_party: false,
        full_authority: true,
        scopes: ["messages.read".to_string()].into(),
        principal_restriction: None,
    };
    authority.tokens.lock().unwrap().insert(token.clone(), ctx);
    let cache = CachingAuthority::new(authority.clone(), std::time::Duration::from_secs(30));
    let calls = || *authority.calls.lock().unwrap();
    // Not caching until the feed watcher has caught up.
    cache
        .authenticate_with(&token, Freshness::BoundedStale)
        .await
        .unwrap();
    cache
        .authenticate_with(&token, Freshness::BoundedStale)
        .await
        .unwrap();
    assert_eq!(calls(), 2);
    let feed = Feed {
        pages: std::sync::Mutex::new(vec![]),
    };
    let cursor = cache.poll(&feed, 0).await;
    assert_eq!(cursor, 10);
    assert!(cache.is_caching());
    cache
        .authenticate_with(&token, Freshness::BoundedStale)
        .await
        .unwrap();
    cache
        .authenticate_with(&token, Freshness::BoundedStale)
        .await
        .unwrap();
    assert_eq!(calls(), 3, "the second read is served from the cache");
    // Mutations always go to Platform Access.
    cache
        .authenticate_with(&token, Freshness::Fresh)
        .await
        .unwrap();
    assert_eq!(calls(), 4);
    // A revocation arrives: the credential is revoked in Platform Access AND the feed
    // reports a change: the cached positive answer is gone at the next poll.
    authority.tokens.lock().unwrap().clear();
    let feed = Feed {
        pages: std::sync::Mutex::new(vec![Ok(FeedPage {
            changes: 1,
            next_after: 11,
            has_more: false,
        })]),
    };
    cache.poll(&feed, cursor).await;
    let r = cache
        .authenticate_with(&token, Freshness::BoundedStale)
        .await
        .unwrap();
    assert!(matches!(
        r,
        messaging::identity::Authentication::Rejected { .. }
    ));
    // A feed outage disables caching entirely (no offline bypass).
    let feed = Feed {
        pages: std::sync::Mutex::new(vec![Err(())]),
    };
    cache.poll(&feed, 11).await;
    assert!(!cache.is_caching());
    assert!(cache.is_empty());
}
