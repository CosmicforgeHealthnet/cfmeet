//! Default-deny routing: the catalogue can never expose internal, provider-callback,
//! operator, Platform Access or Telephony surfaces; the flags default to deny-all.

mod common;

use api_gateway::routing::{
    foundation_catalogue, table_for, Lookup, Protocol, RouteError, RouteTable, CUSTOMER_UPSTREAMS,
};
use axum::http::Method;
use cf_service_auth::services;
use common::route;

#[test]
fn routing_is_deny_all_unless_enabled() {
    let table = table_for(false, false).unwrap();
    assert!(table.is_empty());
    for path in [
        "/v1/developer-platform/context",
        "/v1/anything",
        "/",
        "/internal/v1/x",
    ] {
        assert!(
            matches!(table.lookup(&Method::GET, path), Lookup::NoRoute),
            "{path}"
        );
    }
    let enabled = table_for(true, false).unwrap();
    assert_eq!(enabled.len(), foundation_catalogue().len());
    match enabled.lookup(&Method::GET, "/v1/developer-platform/context") {
        Lookup::Matched(m) => {
            assert_eq!(m.upstream.as_str(), services::DEVELOPER_PLATFORM);
            assert_eq!(m.upstream_path, "/internal/v1/gateway/context");
        }
        other => panic!("{other:?}"),
    }
    assert!(matches!(
        enabled.lookup(&Method::POST, "/v1/developer-platform/context"),
        Lookup::MethodNotAllowed
    ));
}

#[test]
fn the_production_catalogue_is_customer_facing_only() {
    for d in foundation_catalogue() {
        assert!(CUSTOMER_UPSTREAMS.contains(&d.upstream), "{}", d.id);
        assert!(d.pattern.starts_with("/v1/"));
        assert_eq!(d.protocol, Protocol::Rest);
    }
}

#[test]
fn forbidden_upstreams_are_refused() {
    for upstream in [
        services::PLATFORM_ACCESS,
        services::TELEPHONY,
        services::PRIVACY_RELAY,
        services::OPERATOR_CONSOLE,
        services::API_GATEWAY,
        "cfmeet.unknown-service",
    ] {
        let mut r = route("x", Method::GET, "/v1/x", "/internal/v1/x", None);
        r.upstream = upstream;
        assert!(
            matches!(
                RouteTable::new(vec![r], true),
                Err(RouteError::ForbiddenUpstream(..))
            ),
            "{upstream}"
        );
    }
    assert!(!CUSTOMER_UPSTREAMS.contains(&services::TELEPHONY));
    assert!(!CUSTOMER_UPSTREAMS.contains(&services::PLATFORM_ACCESS));
}

#[test]
fn internal_callback_and_operator_paths_are_never_public() {
    for pattern in [
        "/internal/v1/x",
        "/v1/internal/x",
        "/v1/admin/tenants",
        "/v1/operator/x",
        "/v1/provider-callbacks/sms",
        "/v1/telephony/callbacks",
        "/v1/metrics",
        "/provider-callbacks/v1/sms",
        "/health",
        "/v1/",
        "/v1/x/",
        "/v1/x//y",
        "/v1/../internal",
        "/v1/X",
    ] {
        let r = route("x", Method::GET, pattern, "/internal/v1/x", None);
        assert!(
            matches!(
                RouteTable::new(vec![r], true),
                Err(RouteError::ForbiddenPattern(_))
            ),
            "{pattern}"
        );
    }
    for upstream_path in [
        "/v1/x",
        "/internal/v1/admin/tenants",
        "/internal/v1/x/operator",
        "/internal/v2/x",
        "/internal/v1/{other}",
    ] {
        let r = route("x", Method::GET, "/v1/x", upstream_path, None);
        assert!(
            matches!(
                RouteTable::new(vec![r], true),
                Err(RouteError::ForbiddenUpstreamPath(_))
            ),
            "{upstream_path}"
        );
    }
}

#[test]
fn duplicates_and_disabled_websockets_are_refused() {
    let a = route("a", Method::GET, "/v1/x", "/internal/v1/x", None);
    let b = route("b", Method::GET, "/v1/x", "/internal/v1/y", None);
    assert!(matches!(
        RouteTable::new(vec![a.clone(), b], true),
        Err(RouteError::Duplicate(_))
    ));
    let c = route("a", Method::POST, "/v1/y", "/internal/v1/y", None);
    assert!(matches!(
        RouteTable::new(vec![a, c], true),
        Err(RouteError::Duplicate(_))
    ));
    let mut ws = route("ws", Method::GET, "/v1/stream", "/internal/v1/stream", None);
    ws.protocol = Protocol::WebSocket;
    assert!(matches!(
        RouteTable::new(vec![ws.clone()], false),
        Err(RouteError::WebSocketsDisabled(_))
    ));
    assert_eq!(RouteTable::new(vec![ws], true).unwrap().len(), 1);
}

#[test]
fn parameters_bind_safe_segments_only() {
    let t = RouteTable::new(
        vec![route(
            "p",
            Method::GET,
            "/v1/items/{item_id}",
            "/internal/v1/items/{item_id}",
            None,
        )],
        false,
    )
    .unwrap();
    match t.lookup(&Method::GET, "/v1/items/abc-123_X") {
        Lookup::Matched(m) => assert_eq!(m.upstream_path, "/internal/v1/items/abc-123_X"),
        other => panic!("{other:?}"),
    }
    for bad in [
        "/v1/items/a.b",
        "/v1/items/..",
        "/v1/items/a%2Fb",
        "/v1/items/",
        "/v1/items/a/b",
    ] {
        assert!(
            matches!(t.lookup(&Method::GET, bad), Lookup::NoRoute),
            "{bad}"
        );
    }
    let long = format!("/v1/items/{}", "a".repeat(129));
    assert!(matches!(t.lookup(&Method::GET, &long), Lookup::NoRoute));
}
