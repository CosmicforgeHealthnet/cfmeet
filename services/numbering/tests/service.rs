//! Part IV C1 / C6: the service surface — probes, trusted vs untrusted internal callers,
//! customer authentication through a REAL Platform Access with scope enforcement and
//! tenant isolation, correlation / causation propagation, failure handling and the error
//! envelope (every response conforms to `cfmeet.numbering.internal-api`).
mod common;

use std::sync::Arc;

use axum::{
    body::Body,
    http::{Method, Request, StatusCode},
};
use cf_service_auth::{
    headers, CorrelationId, InternalPermission, OutboundServiceRequest, ServiceId, TrustPolicy,
};
use common::{actor, contracts::conforms, scope, Harness};
use numbering::{
    config::WorkerSettings,
    domain::RequestContext,
    http,
    identity::{Authentication, AuthorityUnavailable, CustomerAuthority, Throttle},
    numbers::{EnvironmentKind, InventorySource, OwnerScope, OwnershipClass},
    state::AppState,
};
use serde_json::json;
use uuid::Uuid;

const ERROR: &str = "numbering/common/v1.schema.json#/$defs/Error";

#[tokio::test]
async fn probes_report_liveness_and_readiness_without_data() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let health = h
        .raw(Request::get("/health").body(Body::empty()).unwrap())
        .await;
    assert_eq!(health.status, 200);
    assert_eq!(
        health.body,
        json!({ "status": "ok", "service": "numbering" })
    );
    assert!(health.headers.contains_key("x-request-id"));
    let ready = h
        .raw(Request::get("/ready").body(Body::empty()).unwrap())
        .await;
    assert_eq!(ready.status, 200);
    assert_eq!(
        (
            ready.body["status"].as_str(),
            ready.body["database"].as_str(),
            ready.body["migrations"].as_str()
        ),
        (Some("ready"), Some("ok"), Some("current"))
    );
    assert_eq!(ready.body["outboxPending"], 0);
    // Pending migrations: not ready.
    let Some(empty) = common::TestDb::empty().await else {
        return;
    };
    let not_migrated = Harness::build(
        Arc::new(empty),
        None,
        numbering::domain::ManualClock::new(common::t0()),
    );
    let ready = not_migrated
        .raw(Request::get("/ready").body(Body::empty()).unwrap())
        .await;
    assert_eq!(ready.status, 503);
    assert_eq!(ready.body["migrations"], "pending");
    // The probe schemas of the contract.
    let api = common::contracts::read("numbering/internal/v1.openapi.json");
    assert_eq!(
        api["components"]["schemas"]["Readiness"]["required"]
            .as_array()
            .unwrap()
            .len(),
        7
    );
}

#[tokio::test]
async fn internal_routes_admit_only_trusted_and_permitted_services() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let r = h.available_number(InventorySource::SyntheticTest).await;
    let path = format!("/internal/v1/number-resources/{}", r.id);
    for trusted in [&h.peers.calling, &h.peers.telephony] {
        let reply = h.internal(trusted, Method::GET, &path).await;
        assert_eq!(reply.status, 200, "{:?}", reply.body);
        conforms(
            "numbering/common/v1.schema.json#/$defs/NumberResource",
            &reply.body,
        );
        assert_eq!(reply.body["numberResourceId"], r.id.to_string());
    }
    // A correctly signing service Numbering does not trust, and a trusted name with a
    // key the bundle does not list: refused before any handler runs.
    for untrusted in [&h.peers.privacy_relay, &h.peers.rogue_calling] {
        let reply = h.internal(untrusted, Method::GET, &path).await;
        assert_eq!(
            (reply.status, reply.code()),
            (StatusCode::UNAUTHORIZED, "INTERNAL_AUTHENTICATION_FAILED")
        );
        conforms(ERROR, &reply.body);
    }
    // No token, a customer bearer, a malformed token, and unknown internal paths.
    for request in [
        Request::get(&path).body(Body::empty()).unwrap(),
        Request::get(&path)
            .header("authorization", "Bearer cfk_test_abc")
            .body(Body::empty())
            .unwrap(),
        Request::get(&path)
            .header(headers::SERVICE_AUTHORIZATION, "Bearer not.a.token")
            .body(Body::empty())
            .unwrap(),
        Request::get("/internal/v1/admin/number-resources")
            .body(Body::empty())
            .unwrap(),
    ] {
        let reply = h.raw(request).await;
        assert_eq!(
            (reply.status, reply.code()),
            (StatusCode::UNAUTHORIZED, "INTERNAL_AUTHENTICATION_FAILED")
        );
    }
    let reply = h
        .internal(&h.peers.calling, Method::GET, "/internal/v1/unknown")
        .await;
    assert_eq!(
        (reply.status, reply.code()),
        (StatusCode::NOT_FOUND, "NOT_FOUND")
    );
    let reply = h.internal(&h.peers.calling, Method::POST, &path).await;
    assert_eq!(reply.status, StatusCode::METHOD_NOT_ALLOWED);
    // A malformed id never echoes the input and uses the envelope.
    let reply = h
        .internal(
            &h.peers.calling,
            Method::GET,
            "/internal/v1/number-resources/not-a-uuid",
        )
        .await;
    assert_eq!(
        (reply.status, reply.code()),
        (StatusCode::BAD_REQUEST, "VALIDATION_ERROR")
    );
    assert!(!reply.body.to_string().contains("not-a-uuid"));
    conforms(ERROR, &reply.body);
    let reply = h
        .internal(
            &h.peers.calling,
            Method::GET,
            &format!("/internal/v1/number-resources/{}", Uuid::new_v4()),
        )
        .await;
    assert_eq!(
        (reply.status, reply.code()),
        (StatusCode::NOT_FOUND, "NOT_FOUND")
    );
    // Permissions come only from Numbering's bundle: number-resource.read cannot even be
    // granted to a service outside its registry (privacy-relay), so such a bundle fails.
    let bundle = common::trust_bundle_json(
        cf_service_auth::services::NUMBERING,
        &[(
            cf_service_auth::services::PRIVACY_RELAY,
            vec![h.peers.privacy_relay.public_jwk()],
            &["number-resource.read"],
        )],
    );
    assert!(TrustPolicy::from_json(&bundle, &ServiceId::numbering(), common::ENV).is_err());
    assert_eq!(
        InternalPermission::NumberResourceRead.definition().receiver,
        cf_service_auth::services::NUMBERING
    );
}

#[tokio::test]
async fn internal_requests_continue_the_signed_correlation_and_record_their_cause() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let r = h.available_number(InventorySource::SyntheticTest).await;
    let correlation = CorrelationId::generate();
    let request = OutboundServiceRequest::continuing(ServiceId::numbering(), &correlation);
    let signed = h.peers.calling.sign(&request).unwrap();
    let mut builder = Request::get(format!("/internal/v1/number-resources/{}", r.id));
    for (name, value) in signed.headers().iter() {
        builder = builder.header(name, value);
    }
    let reply = h.raw(builder.body(Body::empty()).unwrap()).await;
    assert_eq!(reply.status, 200);
    // The upstream request id is echoed (one hop = one request id).
    assert_eq!(
        reply.headers[headers::REQUEST_ID.as_str()],
        signed.request_id().as_str()
    );
    // Domain effects made under a context carry its correlation and cause into the outbox
    // and the lifecycle audit; follow-up work keeps the correlation.
    let upstream = RequestContext {
        request_id: cf_service_auth::RequestId::generate(),
        correlation_id: correlation.clone(),
        causation_id: Some(numbering::domain::CausationId::of_request(
            signed.request_id(),
        )),
    };
    let s = scope(EnvironmentKind::Sandbox);
    let res = h
        .numbers
        .reserve(
            &upstream,
            &s,
            actor(),
            r.id,
            chrono::Duration::minutes(5),
            None,
        )
        .await
        .unwrap();
    assert_eq!(res.correlation_id, correlation.as_str());
    let rows = common::strings(
        h.conn(),
        &format!(
            "SELECT DISTINCT correlation_id || '|' || causation_id FROM numbering_outbox_events WHERE correlation_id = '{}'",
            correlation.as_str()
        ),
    )
    .await;
    assert_eq!(
        rows,
        vec![format!(
            "{}|{}",
            correlation.as_str(),
            signed.request_id().as_str()
        )]
    );
    let events = h.numbers.lifecycle_events(r.id).await.unwrap();
    assert_eq!(events.last().unwrap().correlation_id, correlation.as_str());
    // An envelope carries causation only when the cause was an event.
    let envelope_causes = common::strings(
        h.conn(),
        "SELECT DISTINCT causation_id FROM numbering_outbox_events",
    )
    .await;
    assert!(envelope_causes.iter().all(|c| !c.starts_with("evt_")));
}

#[tokio::test]
async fn failures_are_explicit_and_never_leak_internals() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let r = h.available_number(InventorySource::SyntheticTest).await;
    // The database disappears (outage): readiness and data routes fail closed with 503.
    let admin = sea_orm::Database::connect(h.db.admin_url()).await.unwrap();
    let name = h.db.url.rsplit('/').next().unwrap().to_string();
    sea_orm::ConnectionTrait::execute_unprepared(
        &admin,
        &format!("DROP DATABASE {name} WITH (FORCE)"),
    )
    .await
    .unwrap();
    let ready = h
        .raw(Request::get("/ready").body(Body::empty()).unwrap())
        .await;
    assert_eq!(ready.status, 503);
    assert_eq!(ready.body["database"], "unavailable");
    let reply = h
        .internal(
            &h.peers.calling,
            Method::GET,
            &format!("/internal/v1/number-resources/{}", r.id),
        )
        .await;
    assert_eq!(
        (reply.status, reply.code()),
        (StatusCode::SERVICE_UNAVAILABLE, "SERVICE_UNAVAILABLE")
    );
    conforms(ERROR, &reply.body);
    let text = reply.body.to_string();
    for leak in ["sqlx", "postgres", "database \"", &name, &r.e164] {
        assert!(!text.contains(leak), "{leak} leaked: {text}");
    }
    let health = h
        .raw(Request::get("/health").body(Body::empty()).unwrap())
        .await;
    assert_eq!(
        health.status, 200,
        "liveness does not depend on the database"
    );
}

/// An authority stub for the outcomes a real Platform Access cannot be forced into
/// deterministically (throttling, SAFETY_ONLY).
struct Scripted(Authentication);

#[async_trait::async_trait]
impl CustomerAuthority for Scripted {
    async fn authenticate(&self, _presented: &str) -> Result<Authentication, AuthorityUnavailable> {
        Ok(self.0.clone())
    }
}

#[tokio::test]
async fn throttling_and_suspended_authority_are_mapped_without_detail() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let app = |auth: Authentication| {
        let state = AppState::new(
            h.conn().clone(),
            Arc::new(h.clock.clone()),
            Arc::new(Scripted(auth)),
            h.state.service_auth.clone(),
            WorkerSettings::default(),
        );
        http::app(state)
    };
    use tower::ServiceExt;
    let call = |router: axum::Router| async move {
        let response = router
            .oneshot(
                Request::get("/v1/numbers/assignments")
                    .header("authorization", "Bearer cfk_x")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        (
            response.status(),
            response.headers().get("retry-after").cloned(),
        )
    };
    let throttled = app(Authentication::Rejected {
        reason: "secret_mismatch".into(),
        throttle: Some(Throttle {
            retry_after_seconds: 42,
            threshold_crossed: true,
        }),
    });
    let (status, retry) = call(throttled).await;
    assert_eq!(status, StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(retry.unwrap(), "42");
    let mut ctx = numbering::identity::TrustedContext::from_authority(&json!({
        "authorityMode": "SAFETY_ONLY",
        "tenant": { "id": Uuid::new_v4(), "classification": "EXTERNAL" },
        "application": { "id": Uuid::new_v4() },
        "environment": { "id": Uuid::new_v4(), "kind": "SANDBOX" },
        "serviceAccount": { "id": Uuid::new_v4() },
        "credential": { "id": Uuid::new_v4() },
        "scopes": ["numbers.read"],
    }))
    .unwrap();
    let (status, _) = call(app(Authentication::Authenticated(Box::new(ctx.clone())))).await;
    assert_eq!(
        status,
        StatusCode::UNAUTHORIZED,
        "a suspended scope has no numbering operation"
    );
    ctx.full_authority = true;
    let (status, _) = call(app(Authentication::Authenticated(Box::new(ctx)))).await;
    assert_eq!(status, StatusCode::OK);
    // No authority configured: fail closed.
    let reply = h
        .customer(Some("cfk_anything"), Method::GET, "/v1/numbers/assignments")
        .await;
    assert_eq!(
        (reply.status, reply.code()),
        (StatusCode::SERVICE_UNAVAILABLE, "SERVICE_UNAVAILABLE")
    );
}

fn owner_scope(c: &common::platform_access::Customer, kind: EnvironmentKind) -> OwnerScope {
    OwnerScope {
        tenant_id: c.tenant_id,
        application_id: c.application_id,
        environment_id: c.environment_id,
        environment_kind: kind,
        service_account_id: c.service_account_id,
    }
}

#[tokio::test]
async fn customers_are_authenticated_by_platform_access_scoped_and_isolated() {
    let Some(h) = Harness::with_platform_access().await else {
        return;
    };
    let pa = h.pa.clone().unwrap();
    let alice = pa
        .customer(
            "SANDBOX",
            &["numbers.read", "calls.read"],
            &["numbers.read"],
        )
        .await;
    let bob = pa
        .customer("SANDBOX", &["numbers.read"], &["numbers.read"])
        .await;
    let alice_scope = owner_scope(&alice, EnvironmentKind::Sandbox);
    let j = h.active(&alice_scope, OwnershipClass::EmployeeDirect).await;

    // Alice reads her own records; every body conforms to the contract.
    let list = h
        .customer(Some(&alice.secret), Method::GET, "/v1/numbers/assignments")
        .await;
    assert_eq!(list.status, 200, "{:?}", list.body);
    conforms(
        "numbering/common/v1.schema.json#/$defs/DcnAssignmentList",
        &list.body,
    );
    assert_eq!(list.body["data"].as_array().unwrap().len(), 1);
    assert_eq!(
        list.body["data"][0]["ownership"]["ownerKind"],
        "ORGANIZATION"
    );
    let one = h
        .customer(
            Some(&alice.secret),
            Method::GET,
            &format!("/v1/numbers/assignments/{}", j.assignment.id),
        )
        .await;
    assert_eq!(one.status, 200);
    conforms(
        "numbering/common/v1.schema.json#/$defs/DcnAssignment",
        &one.body,
    );
    let history = h
        .customer(
            Some(&alice.secret),
            Method::GET,
            &format!("/v1/numbers/assignments/{}/history", j.assignment.id),
        )
        .await;
    assert_eq!(history.status, 200);
    conforms(
        "numbering/common/v1.schema.json#/$defs/AssignmentHistory",
        &history.body,
    );
    let reservation = h
        .customer(
            Some(&alice.secret),
            Method::GET,
            &format!("/v1/numbers/reservations/{}", j.reservation.id),
        )
        .await;
    assert_eq!(reservation.status, 200);
    conforms(
        "numbering/common/v1.schema.json#/$defs/Reservation",
        &reservation.body,
    );
    // Nothing provider- or number-shaped reaches a customer.
    for body in [&list.body, &one.body, &history.body, &reservation.body] {
        let t = body.to_string();
        assert!(
            !t.contains("+1555") && !t.contains("synthetic") && !t.contains("PN"),
            "{t}"
        );
    }

    // Bob (another tenant) sees nothing of Alice's, whatever he claims in the request.
    for path in [
        format!("/v1/numbers/assignments/{}", j.assignment.id),
        format!("/v1/numbers/assignments/{}/history", j.assignment.id),
        format!("/v1/numbers/reservations/{}", j.reservation.id),
    ] {
        let mut req = Request::get(&path).header("authorization", format!("Bearer {}", bob.secret));
        for (k, v) in [
            ("x-cf-tenant-id", alice.tenant_id.to_string()),
            ("x-tenant-id", alice.tenant_id.to_string()),
            ("x-cf-environment-id", alice.environment_id.to_string()),
        ] {
            req = req.header(k, v);
        }
        let reply = h.raw(req.body(Body::empty()).unwrap()).await;
        assert_eq!(
            (reply.status, reply.code()),
            (StatusCode::NOT_FOUND, "NOT_FOUND"),
            "{path}"
        );
    }
    let bobs = h
        .customer(Some(&bob.secret), Method::GET, "/v1/numbers/assignments")
        .await;
    assert_eq!(bobs.body["data"].as_array().unwrap().len(), 0);

    // Scope enforcement: a valid credential without numbers.read.
    let carol = pa
        .customer("SANDBOX", &["numbers.read", "calls.read"], &["calls.read"])
        .await;
    let reply = h
        .customer(Some(&carol.secret), Method::GET, "/v1/numbers/assignments")
        .await;
    assert_eq!(
        (reply.status, reply.code()),
        (StatusCode::FORBIDDEN, "INSUFFICIENT_SCOPE")
    );
    conforms(ERROR, &reply.body);

    // Every authentication failure is the same normalized 401.
    let mut garbage = alice.secret.clone();
    garbage.pop();
    garbage.push(if alice.secret.ends_with('a') {
        'b'
    } else {
        'a'
    });
    let signed = h
        .peers
        .calling
        .sign(&OutboundServiceRequest::new_operation(
            ServiceId::numbering(),
        ))
        .unwrap();
    let mut with_service_token = Request::get("/v1/numbers/assignments")
        .header("authorization", format!("Bearer {}", alice.secret));
    for (n, v) in signed.headers().iter() {
        with_service_token = with_service_token.header(n, v);
    }
    let replies = vec![
        h.customer(None, Method::GET, "/v1/numbers/assignments")
            .await,
        h.customer(
            Some("not-a-credential"),
            Method::GET,
            "/v1/numbers/assignments",
        )
        .await,
        h.customer(Some(&garbage), Method::GET, "/v1/numbers/assignments")
            .await,
        h.raw(
            Request::get("/v1/numbers/assignments")
                .header("authorization", format!("Basic {}", alice.secret))
                .body(Body::empty())
                .unwrap(),
        )
        .await,
        h.raw(with_service_token.body(Body::empty()).unwrap()).await,
        h.customer(None, Method::GET, "/v1/numbers/unknown-route")
            .await,
    ];
    for reply in &replies {
        assert_eq!(
            (reply.status, reply.code()),
            (StatusCode::UNAUTHORIZED, "AUTHENTICATION_FAILED"),
            "{:?}",
            reply.body
        );
        assert_eq!(
            reply.body["error"]["message"],
            replies[0].body["error"]["message"]
        );
        conforms(ERROR, &reply.body);
    }
    // Revocation takes effect on the next request (no caching of authority).
    pa.revoke(alice.credential_id).await;
    let reply = h
        .customer(Some(&alice.secret), Method::GET, "/v1/numbers/assignments")
        .await;
    assert_eq!(reply.status, StatusCode::UNAUTHORIZED);
    // Platform Access unreachable: fail closed (503), never an anonymous answer.
    pa.stop().await;
    let reply = h
        .customer(Some(&bob.secret), Method::GET, "/v1/numbers/assignments")
        .await;
    assert_eq!(
        (reply.status, reply.code()),
        (StatusCode::SERVICE_UNAVAILABLE, "SERVICE_UNAVAILABLE")
    );
}

#[tokio::test]
async fn every_implemented_route_is_in_the_contract_and_vice_versa() {
    let api = common::contracts::read("numbering/internal/v1.openapi.json");
    let manifest = common::contracts::read("manifest.json");
    let declared = manifest["contracts"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["id"] == "cfmeet.numbering.internal-api")
        .unwrap();
    assert_eq!(api["info"]["version"], declared["version"]);
    let mut contract: Vec<String> = Vec::new();
    for (path, ops) in api["paths"].as_object().unwrap() {
        for method in ops.as_object().unwrap().keys() {
            contract.push(format!("{} {}", method.to_uppercase(), path));
        }
    }
    contract.sort();
    let src = |p: &str| {
        std::fs::read_to_string(std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join(p)).unwrap()
    };
    let mut implemented = vec![
        "GET /health".to_string(),
        "GET /ready".to_string(),
        "GET /openapi.json".to_string(),
    ];
    for (file, prefix) in [
        ("src/http/customer.rs", "/v1"),
        ("src/http/numbers.rs", "/v1"),
        ("src/http/acquisitions.rs", "/v1"),
        ("src/http/catalog.rs", "/v1"),
        ("src/http/kyc.rs", "/v1"),
        ("src/http/callbacks.rs", "/provider-callbacks/v1"),
        ("src/http/internal.rs", "/internal/v1"),
    ] {
        for (method, path) in routes_in(&src(file)) {
            let path: String = path
                .split('/')
                .map(|seg| match seg.strip_prefix(':') {
                    Some(name) => format!("{{{}}}", to_camel(name)),
                    None => seg.to_string(),
                })
                .collect::<Vec<_>>()
                .join("/");
            implemented.push(format!("{method} {prefix}{path}"));
        }
    }
    implemented.sort();
    assert_eq!(implemented, contract);
}

/// Every `(METHOD, path)` of the `.route("path", get(..).post(..))` calls in `source`,
/// whatever the layout rustfmt chose.
fn routes_in(source: &str) -> Vec<(String, String)> {
    let mut out = Vec::new();
    for part in source.split(".route(").skip(1) {
        let Some(rest) = part.trim_start().strip_prefix('"') else {
            continue;
        };
        let (path, rest) = rest.split_once('"').unwrap();
        // The route's arguments: up to the parenthesis closing `.route(`.
        let mut depth = 1;
        let mut ident = String::new();
        for c in rest.chars() {
            match c {
                '(' => {
                    if depth == 1
                        && ["get", "post", "put", "patch", "delete"].contains(&ident.as_str())
                    {
                        out.push((ident.to_uppercase(), path.to_string()));
                    }
                    depth += 1;
                    ident.clear();
                }
                ')' => {
                    depth -= 1;
                    if depth == 0 {
                        break;
                    }
                }
                c if c.is_ascii_alphanumeric() || c == '_' => ident.push(c),
                _ => ident.clear(),
            }
        }
    }
    out
}

fn to_camel(s: &str) -> String {
    let mut out = String::new();
    let mut upper = false;
    for c in s.chars() {
        if c == '_' {
            upper = true;
        } else if upper {
            out.push(c.to_ascii_uppercase());
            upper = false;
        } else {
            out.push(c);
        }
    }
    out
}
