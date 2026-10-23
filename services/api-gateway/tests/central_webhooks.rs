//! Section 53 cut-over at the ingress: with `API_GATEWAY_CENTRAL_WEBHOOKS_ENABLED` the
//! generated customer webhook routes (`/v1/webhooks*`, `/v1/webhook-deliveries*`) reach
//! the Developer Platform at the SAME public path with the gateway token and the
//! delegated context (credential id); off (the default), they reach Calling. Nothing else
//! moves.

mod common;

use api_gateway::{catalogue, routing::RoutingControls};
use axum::http::{Method, StatusCode};
use cf_service_auth::services;
use common::*;

const CRED: &str = "cfmk_sbx_aaaaaaaaaaaaaaaaaaaaaaaa_bbbb";

/// The real generated webhook routes (plus one non-webhook Calling route as a control).
async fn gateway(central: bool) -> Gateway {
    let controls = RoutingControls {
        central_webhooks: central,
        ..RoutingControls::default()
    };
    let routes: Vec<_> = catalogue::definitions(&controls)
        .unwrap()
        .into_iter()
        .filter(|d| {
            api_gateway::routing::is_webhook_route(d)
                || (d.domain == "calling" && d.pattern == "/v1/calls" && d.method == Method::GET)
        })
        .collect();
    assert!(routes.len() >= 8, "{}", routes.len());
    Gateway::with(Options {
        routes: Some(routes),
        controls,
        ..Options::default()
    })
    .await
}

#[tokio::test]
async fn central_webhooks_move_only_the_webhook_routes_to_the_developer_platform() {
    for central in [false, true] {
        let g = gateway(central).await;
        let c = customer(&["webhooks.read", "webhooks.manage", "calls.read"]);
        g.authority.add(CRED, c.clone());
        let auth = format!("Bearer {CRED}");
        let headers = [
            ("authorization", auth.as_str()),
            ("content-type", "application/json"),
        ];
        let calls = [
            (Method::GET, "/v1/webhooks?limit=5".to_string(), None),
            (
                Method::POST,
                "/v1/webhooks".to_string(),
                Some(r#"{"url":"https://hooks.example.com/x","eventTypes":["call.ended"]}"#),
            ),
            (
                Method::GET,
                "/v1/webhooks/3c9e2b1a-5d4f-4a60-9b71-8c2d3e4f5a6b/deliveries".to_string(),
                None,
            ),
            (
                Method::POST,
                "/v1/webhook-deliveries/9b8a7c6d-5e4f-4a3b-8c2d-1e0f9a8b7c6d/replay".to_string(),
                None,
            ),
        ];
        for (method, path, body) in calls {
            let before = (g.calling().count(), g.dp().count());
            let r = g.call(method.clone(), &path, &headers, body).await;
            assert_eq!(
                r.status,
                StatusCode::OK,
                "{central} {method} {path}: {}",
                r.json
            );
            let (target, other) = if central {
                (g.dp(), g.calling())
            } else {
                (g.calling(), g.dp())
            };
            assert_eq!(
                (target.count(), other.count()),
                if central {
                    (before.1 + 1, before.0)
                } else {
                    (before.0 + 1, before.1)
                },
                "{central} {path}"
            );
            let got = target.last();
            // Same public path, the gateway's service token, the delegated context with
            // the gateway-authenticated credential id.
            assert_eq!(got.path, path.split('?').next().unwrap());
            assert_eq!(got.issuer.as_deref(), Some(services::API_GATEWAY));
            let d = got.delegated.expect("delegated context");
            assert_eq!(d.tenant_id(), c.tenant_id);
            assert_eq!(d.application_environment_id(), c.environment_id);
            assert_eq!(d.initiating_credential_id(), Some(c.credential_id));
            assert!(
                got.headers.get("authorization").is_none(),
                "never the credential"
            );
        }
        // Non-webhook Calling routes stay on Calling either way.
        let before = g.calling().count();
        let r = g.call(Method::GET, "/v1/calls", &headers, None).await;
        assert_eq!(r.status, StatusCode::OK);
        assert_eq!(g.calling().count(), before + 1);
        // Scopes are still enforced at the edge for the moved routes.
        let reader = customer(&["calls.read"]);
        g.authority
            .add("cfmk_sbx_cccccccccccccccccccccccc_dddd", reader);
        let r = g
            .call(
                Method::GET,
                "/v1/webhooks",
                &[(
                    "authorization",
                    "Bearer cfmk_sbx_cccccccccccccccccccccccc_dddd",
                )],
                None,
            )
            .await;
        assert_eq!(r.status, StatusCode::FORBIDDEN, "{}", r.json);
    }
}
