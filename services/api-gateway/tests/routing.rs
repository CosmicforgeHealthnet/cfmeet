//! Default-deny routing: the route table refuses non-customer upstreams, internal /
//! administrative / callback patterns and foreign upstream paths at construction, and
//! the generated catalogue is in parity with the contracts (every public operation
//! resolves to exactly one route; nothing non-public resolves at all).

mod common;

use api_gateway::{
    catalogue,
    routing::{
        AuthMode, Lookup, Protocol, RouteError, RouteTable, RoutingControls, CUSTOMER_UPSTREAMS,
    },
};
use axum::http::Method;
use cf_service_auth::services;
use common::*;
use serde_json::Value;

fn one(pattern: &str, upstream: &str, upstream_path: &str) -> Result<RouteTable, RouteError> {
    let mut r = route(
        "x",
        "calling",
        Method::GET,
        pattern,
        upstream,
        &[AuthMode::PlatformCredential],
        None,
    );
    r.upstream_path = upstream_path.into();
    RouteTable::new(vec![r], true)
}

#[test]
fn construction_refuses_every_non_customer_surface() {
    for upstream in [
        services::PLATFORM_ACCESS,
        services::TELEPHONY,
        services::PRIVACY_RELAY,
        services::OPERATOR_CONSOLE,
        services::API_GATEWAY,
    ] {
        assert!(
            matches!(
                one("/v1/x", upstream, "/v1/x"),
                Err(RouteError::ForbiddenUpstream(..))
            ),
            "{upstream}"
        );
    }
    for pattern in [
        "/internal/v1/x",
        "/v1/admin/x",
        "/v1/x/admin",
        "/v1/operator/x",
        "/v1/provider-callbacks/x",
        "/v1/x/callbacks",
        "/v1/metrics",
        "/v2/x",
        "/v1/",
        "/v1/x/",
        "/v1/../x",
        "/v1/X",
    ] {
        assert!(
            matches!(
                one(pattern, services::CALLING, pattern),
                Err(RouteError::ForbiddenPattern(_))
            ),
            "{pattern}"
        );
    }
    // An upstream path must be the public pattern itself (receivers authorize their own
    // public routes) — only the Developer Platform has /internal/v1 gateway routes.
    assert!(matches!(
        one("/v1/x", services::CALLING, "/internal/v1/x"),
        Err(RouteError::ForbiddenUpstreamPath(_))
    ));
    assert!(matches!(
        one("/v1/x", services::CALLING, "/v1/y"),
        Err(RouteError::ForbiddenUpstreamPath(_))
    ));
    assert!(matches!(
        one(
            "/v1/x",
            services::DEVELOPER_PLATFORM,
            "/internal/v1/admin/x"
        ),
        Err(RouteError::ForbiddenUpstreamPath(_))
    ));
    assert!(one(
        "/v1/x",
        services::DEVELOPER_PLATFORM,
        "/internal/v1/gateway/x"
    )
    .is_ok());
    assert!(one(
        "/api/v2/x/{id}",
        services::MEETING_SERVICE,
        "/api/v2/x/{id}"
    )
    .is_ok());
    // Duplicates, missing auth and disabled WebSockets.
    let r = route(
        "a",
        "calling",
        Method::GET,
        "/v1/a",
        services::CALLING,
        &[AuthMode::PlatformCredential],
        None,
    );
    assert!(matches!(
        RouteTable::new(vec![r.clone(), r.clone()], true),
        Err(RouteError::Duplicate(_))
    ));
    let mut no_auth = r.clone();
    no_auth.auth.clear();
    assert!(matches!(
        RouteTable::new(vec![no_auth], true),
        Err(RouteError::InvalidAuth(_))
    ));
    let mut ws = r;
    ws.protocol = Protocol::WebSocket;
    assert!(matches!(
        RouteTable::new(vec![ws], false),
        Err(RouteError::WebSocketsDisabled(_))
    ));
    assert_eq!(CUSTOMER_UPSTREAMS.len(), 6);
}

#[test]
fn lookups_bind_safe_parameters_and_prefer_literals() {
    let t = RouteTable::new(test_routes(), true).unwrap();
    match t.lookup(&Method::GET, "/v1/test/items/history") {
        Lookup::Matched(m) => assert_eq!(m.definition.id, "calling.history"),
        other => panic!("{other:?}"),
    }
    match t.lookup(&Method::GET, "/v1/test/items/abc-123") {
        Lookup::Matched(m) => assert_eq!(m.upstream_path, "/v1/test/items/abc-123"),
        other => panic!("{other:?}"),
    }
    match t.lookup(&Method::GET, "/v1/developer-platform/context") {
        Lookup::Matched(m) => assert_eq!(m.upstream_path, "/internal/v1/gateway/context"),
        other => panic!("{other:?}"),
    }
    for bad in [
        "/v1/test/items/a%2Fb",
        "/v1/test/items/..",
        "/v1/test/items/%2e%2e",
        "/v1/test/items/a%00",
        "/v1/test/items/a b",
    ] {
        assert!(
            matches!(t.lookup(&Method::GET, bad), Lookup::NoRoute),
            "{bad}"
        );
    }
    assert!(
        matches!(t.lookup(&Method::DELETE, "/v1/test/items"), Lookup::MethodNotAllowed(ref m) if m == &[Method::POST])
    );
    assert!(matches!(
        t.lookup(&Method::GET, "/internal/v1/gateway/context"),
        Lookup::NoRoute
    ));
    assert!(matches!(
        t.lookup(&Method::GET, "/v1/test"),
        Lookup::NoRoute
    ));
}

#[test]
fn the_generated_catalogue_is_in_parity_with_the_contracts() {
    let manifest: Value = serde_json::from_str(catalogue::MANIFEST).unwrap();
    let table = catalogue::table_for(true, true, &RoutingControls::default()).unwrap();
    // Every classified operation of the source contracts has exactly one class, and
    // only PUBLIC_CUSTOMER operations are routable.
    let mut public = 0;
    for e in catalogue::catalogue() {
        let m = Method::from_bytes(e.method.as_bytes()).unwrap();
        let sample = e
            .path
            .split('/')
            .map(|s| if s.starts_with('{') { "abc" } else { s })
            .collect::<Vec<_>>()
            .join("/");
        let routed = matches!(table.lookup(&m, &sample), Lookup::Matched(ref r) if r.definition.pattern == e.path);
        match e.class.as_str() {
            "PUBLIC_CUSTOMER" => {
                public += 1;
                assert!(
                    routed,
                    "public operation not routable: {} {}",
                    e.method, e.path
                );
            }
            "INTERNAL_SERVICE"
            | "OPERATOR_ADMIN"
            | "PROVIDER_CALLBACK"
            | "PUBLIC_INFRASTRUCTURE"
            | "EXCLUDED" => {
                assert!(
                    !routed,
                    "{} operation routable: {} {}",
                    e.class, e.method, e.path
                );
            }
            other => panic!("unknown class {other}"),
        }
    }
    assert_eq!(public, table.len());
    assert_eq!(public, manifest["routes"].as_array().unwrap().len());
    // Platform Access, Telephony and internal / admin / provider paths never appear.
    for r in table.definitions() {
        assert!(
            CUSTOMER_UPSTREAMS.contains(&r.upstream.as_str()),
            "{}",
            r.id
        );
        assert!(
            !r.pattern.contains("/internal")
                && !r.pattern.contains("admin")
                && !r.pattern.contains("provider"),
            "{}",
            r.pattern
        );
    }
    // The approved families are all present.
    for (domain, sample) in [
        ("meetings", "/api/v2/meeting-spaces"),
        ("calling", "/v1/calls"),
        ("privacy-relay", "/v1/privacy-relay/contexts"),
        ("messaging", "/v1/conversations"),
        ("numbering", "/v1/numbers/search"),
        ("sms", "/v1/sms/messages"),
        ("developer-platform", "/v1/organizations"),
    ] {
        assert!(
            table
                .definitions()
                .any(|d| d.domain == domain && d.pattern == sample),
            "{domain} {sample}"
        );
    }
    // Contact Vault operations stay private; numbering administration is never public.
    assert!(!table.definitions().any(|d| d.pattern.contains("/contacts")));
    assert!(!table
        .definitions()
        .any(|d| d.pattern.starts_with("/v1/admin")));
    // The three approved realtime channels are WebSocket routes.
    let ws: Vec<_> = table
        .definitions()
        .filter(|d| d.protocol == Protocol::WebSocket)
        .map(|d| d.pattern.as_str())
        .collect();
    assert_eq!(ws.len(), 3, "{ws:?}");
    for p in [
        "/api/v2/conferences/{id}/realtime",
        "/v1/calling/signaling",
        "/v1/realtime",
    ] {
        assert!(ws.contains(&p), "{p}");
    }
    // WebSockets off: REST routing still works, realtime routes are absent.
    let rest = catalogue::table_for(true, false, &RoutingControls::default()).unwrap();
    assert_eq!(rest.len(), table.len() - 3);
}
