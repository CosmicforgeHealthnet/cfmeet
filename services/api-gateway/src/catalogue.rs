//! The generated public route catalogue (`infra/api-gateway/route-manifest.json`),
//! embedded at compile time so the deployed binary and its documented routes cannot
//! diverge. `scripts/contracts/public_api.py --check` (CI) proves the manifest equals
//! what the authoritative contracts and the explicit allow-list produce.

use std::time::Duration;

use axum::http::Method;
use serde::Deserialize;

use crate::routing::{
    AuthMode, Protocol, RouteDefinition, RouteError, RouteTable, RoutingControls,
};

/// The embedded manifest.
pub const MANIFEST: &str = include_str!("../../../infra/api-gateway/route-manifest.json");

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ManifestRoute {
    id: String,
    domain: String,
    method: String,
    pattern: String,
    protocol: String,
    upstream: String,
    upstream_path: String,
    auth: Vec<String>,
    #[serde(default)]
    required_scope: Option<String>,
    #[serde(default)]
    forward_headers: Vec<String>,
    #[serde(default)]
    timeout_ms: Option<u64>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CatalogueEntry {
    pub domain: String,
    pub operation_id: String,
    pub method: String,
    pub path: String,
    pub class: String,
    #[serde(default)]
    pub reason: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Manifest {
    routes: Vec<ManifestRoute>,
    catalogue: Vec<CatalogueEntry>,
}

fn manifest() -> Manifest {
    serde_json::from_str(MANIFEST).expect("the embedded route manifest parses")
}

/// Every classified source-contract operation (public and non-public).
pub fn catalogue() -> Vec<CatalogueEntry> {
    manifest().catalogue
}

/// The public route definitions of the manifest, with the runtime controls applied
/// (disabled families removed; customer webhook routes re-pointed when central webhooks
/// own them).
pub fn definitions(controls: &RoutingControls) -> Result<Vec<RouteDefinition>, RouteError> {
    let mut out = Vec::new();
    for r in manifest().routes {
        if controls.disabled_domains.contains(&r.domain) {
            continue;
        }
        let method = Method::from_bytes(r.method.as_bytes())
            .map_err(|_| RouteError::ForbiddenPattern(r.id.clone()))?;
        let auth = r
            .auth
            .iter()
            .map(|a| AuthMode::parse(a).ok_or_else(|| RouteError::InvalidAuth(r.id.clone())))
            .collect::<Result<Vec<_>, _>>()?;
        let protocol = match r.protocol.as_str() {
            "REST" => Protocol::Rest,
            "WEBSOCKET" => Protocol::WebSocket,
            _ => return Err(RouteError::ForbiddenPattern(r.id.clone())),
        };
        let mut d = RouteDefinition {
            id: r.id,
            domain: r.domain,
            method,
            pattern: r.pattern,
            protocol,
            upstream: r.upstream,
            upstream_path: r.upstream_path,
            auth,
            required_scope: r.required_scope,
            forward_headers: r.forward_headers,
            timeout: r.timeout_ms.map(Duration::from_millis),
        };
        if (controls.central_webhooks && crate::routing::is_webhook_route(&d))
            || (controls.central_usage && crate::routing::is_usage_route(&d))
        {
            d.upstream = cf_service_auth::services::DEVELOPER_PLATFORM.to_string();
        }
        out.push(d);
    }
    Ok(out)
}

/// The table the flags select: deny-all unless public routing is enabled. WebSocket
/// routes are dropped (not refused) while WebSockets are off, so REST routing can be
/// enabled independently.
pub fn table_for(
    public_routing: bool,
    websockets: bool,
    controls: &RoutingControls,
) -> Result<RouteTable, RouteError> {
    if !public_routing {
        return Ok(RouteTable::deny_all());
    }
    let defs = definitions(controls)?
        .into_iter()
        .filter(|d| websockets || d.protocol != Protocol::WebSocket)
        .collect();
    RouteTable::new(defs, websockets)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_embedded_manifest_compiles_into_a_default_deny_table() {
        let t = table_for(true, true, &RoutingControls::default()).unwrap();
        assert!(t.len() > 400, "{}", t.len());
        assert_eq!(
            table_for(false, true, &RoutingControls::default())
                .unwrap()
                .len(),
            0
        );
        // No catalogued non-public operation is routable.
        for e in catalogue() {
            if e.class != "PUBLIC_CUSTOMER" {
                let m = Method::from_bytes(e.method.as_bytes()).unwrap();
                let path = e.path.replace(['{', '}'], "");
                assert!(
                    !matches!(t.lookup(&m, &path), crate::routing::Lookup::Matched(r) if r.definition.pattern == e.path),
                    "{} {} {}",
                    e.class,
                    e.method,
                    e.path
                );
            }
        }
    }

    #[test]
    fn controls_disable_families_and_move_webhooks() {
        let mut c = RoutingControls::default();
        c.disabled_domains.insert("sms".into());
        let defs = definitions(&c).unwrap();
        assert!(defs.iter().all(|d| d.domain != "sms"));
        assert!(defs
            .iter()
            .filter(|d| crate::routing::is_webhook_route(d))
            .all(|d| d.upstream == "cfmeet.calling"));
        c.central_webhooks = true;
        assert!(definitions(&c)
            .unwrap()
            .iter()
            .filter(|d| crate::routing::is_webhook_route(d))
            .all(|d| d.upstream == "cfmeet.developer-platform"));
        // §55: usage / limits stay Calling's until the flag, then move (and only them).
        let usage = |c: &RoutingControls| -> Vec<String> {
            definitions(c)
                .unwrap()
                .into_iter()
                .filter(crate::routing::is_usage_route)
                .map(|d| d.upstream)
                .collect()
        };
        assert_eq!(usage(&c), ["cfmeet.calling", "cfmeet.calling"]);
        c.central_usage = true;
        assert_eq!(
            usage(&c),
            ["cfmeet.developer-platform", "cfmeet.developer-platform"]
        );
    }
}
