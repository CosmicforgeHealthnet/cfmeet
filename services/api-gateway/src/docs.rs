//! Customer-facing API documentation and discovery (PUBLIC_INFRASTRUCTURE).
//!
//! * `GET /openapi.json` — the aggregate public OpenAPI;
//! * `GET /openapi/{domain}.json` — the per-domain public OpenAPI;
//! * `GET /docs` — a read-only Swagger UI over those documents only (pinned, SRI-checked
//!   assets; no "try it out");
//! * `GET /discovery` — the API catalogue: families, their source contract and version,
//!   routable operations and WebSocket channels, and whether each family is enabled.
//!
//! The documents are the generated `packages/contracts/bundled/public/**` files embedded
//! at build time: only allow-listed public operations, no internal security scheme,
//! every reference resolved offline. The engineering Swagger selector of the Meeting
//! Service is unchanged.

use axum::{
    extract::{Path, State},
    http::{header, HeaderValue, StatusCode},
    response::{IntoResponse, Response},
    Json,
};
use base64::Engine;
use serde_json::{json, Value};
use sha2::Digest;

use crate::{
    http::error::{ApiError, ErrorCode},
    routing::Protocol,
    state::AppState,
};

macro_rules! public_doc {
    ($d:literal) => {
        include_str!(concat!(
            "../../../packages/contracts/bundled/public/",
            $d,
            "/v1.openapi.json"
        ))
    };
}

/// `(domain, display name, document)`.
pub const DOCS: &[(&str, &str, &str)] = &[
    ("meetings", "Meetings", public_doc!("meetings")),
    ("calling", "Calling", public_doc!("calling")),
    (
        "privacy-relay",
        "Privacy Relay",
        public_doc!("privacy-relay"),
    ),
    ("messaging", "Messaging", public_doc!("messaging")),
    ("numbering", "Numbers", public_doc!("numbering")),
    ("sms", "SMS", public_doc!("sms")),
    (
        "developer-platform",
        "Developer Platform",
        public_doc!("developer-platform"),
    ),
    ("gateway", "Gateway", public_doc!("gateway")),
];
pub const AGGREGATE: &str = public_doc!("aggregate");

fn json_doc(body: &'static str) -> Response {
    (
        [
            (header::CONTENT_TYPE, "application/json"),
            (header::CACHE_CONTROL, "public, max-age=300"),
        ],
        body,
    )
        .into_response()
}

pub async fn aggregate() -> Response {
    json_doc(AGGREGATE)
}

pub async fn domain_doc(Path(file): Path<String>) -> Response {
    let Some(domain) = file.strip_suffix(".json") else {
        return ApiError::Code(ErrorCode::NotFound).into_response();
    };
    match DOCS.iter().find(|(d, _, _)| *d == domain) {
        Some((_, _, body)) => json_doc(body),
        None => ApiError::Code(ErrorCode::NotFound).into_response(),
    }
}

pub async fn discovery(State(state): State<AppState>) -> Json<Value> {
    let families: Vec<Value> = DOCS
        .iter()
        .map(|(domain, name, body)| {
            let doc: Value = serde_json::from_str(body).unwrap_or(Value::Null);
            let routes: Vec<_> = state
                .routes
                .definitions()
                .filter(|r| r.domain == *domain)
                .collect();
            json!({
                "domain": domain,
                "name": name,
                "sourceContract": doc["info"]["x-cfmeet-source-contract"],
                "version": doc["info"]["x-cfmeet-source-version"],
                "enabled": !routes.is_empty(),
                "operations": routes.len(),
                "websockets": routes.iter().filter(|r| r.protocol == Protocol::WebSocket).map(|r| r.pattern.clone()).collect::<Vec<_>>(),
                "openapi": format!("/openapi/{domain}.json"),
            })
        })
        .collect();
    Json(json!({
        "service": "cfmeet.api-gateway",
        "documentation": { "aggregate": "/openapi.json", "reference": "/docs" },
        "families": families,
        "centralWebhooks": state.controls.central_webhooks,
        "centralUsage": state.controls.central_usage,
    }))
}

const SWAGGER_UI_VERSION: &str = "5.17.14";
const SWAGGER_UI_CSS_SRI: &str =
    "sha384-wxLW6kwyHktdDGr6Pv1zgm/VGJh99lfUbzSn6HNHBENZlCN7W602k9VkGdxuFvPn";
const SWAGGER_UI_JS_SRI: &str =
    "sha384-wmyclcVGX/WhUkdkATwhaK1X1JtiNrr2EoYJ+diV3vj4v6OC5yCeSu+yW13SYJep";
const SWAGGER_INIT: &str = r#"
window.addEventListener('load', function() {
  var selector = document.getElementById('api-family');
  function render(url) {
    window.ui = SwaggerUIBundle({ url: url, dom_id: '#swagger-ui', deepLinking: true,
      filter: true, supportedSubmitMethods: [], validatorUrl: null });
  }
  selector.addEventListener('change', function() { render(selector.value); });
  render(selector.value);
});
"#;

fn page() -> String {
    let options: String = std::iter::once(("aggregate", "All public APIs"))
        .chain(DOCS.iter().map(|(d, n, _)| (*d, *n)))
        .map(|(d, n)| {
            let url = if d == "aggregate" {
                "/openapi.json".to_string()
            } else {
                format!("/openapi/{d}.json")
            };
            format!(r#"<option value="{url}">{n}</option>"#)
        })
        .collect();
    format!(
        r#"<!doctype html>
<html lang="en">
<head>
  <meta charset="utf-8" />
  <meta name="viewport" content="width=device-width, initial-scale=1" />
  <title>CF Meet public API reference</title>
  <link rel="stylesheet" href="https://cdn.jsdelivr.net/npm/swagger-ui-dist@{v}/swagger-ui.css" integrity="{css}" crossorigin="anonymous" referrerpolicy="no-referrer" />
</head>
<body>
  <div style="padding:12px 20px;font:14px sans-serif"><label for="api-family">API: </label><select id="api-family">{options}</select></div>
  <div id="swagger-ui"></div>
  <script src="https://cdn.jsdelivr.net/npm/swagger-ui-dist@{v}/swagger-ui-bundle.js" integrity="{js}" crossorigin="anonymous" referrerpolicy="no-referrer"></script>
  <script>{init}</script>
</body>
</html>
"#,
        v = SWAGGER_UI_VERSION,
        css = SWAGGER_UI_CSS_SRI,
        js = SWAGGER_UI_JS_SRI,
        init = SWAGGER_INIT,
    )
}

pub async fn reference() -> Response {
    let hash = base64::engine::general_purpose::STANDARD.encode(sha2::Sha256::digest(SWAGGER_INIT));
    let csp = format!(
        "default-src 'none'; script-src https://cdn.jsdelivr.net 'sha256-{hash}'; \
         style-src https://cdn.jsdelivr.net 'unsafe-inline'; img-src 'self' data:; \
         connect-src 'self'; base-uri 'none'; form-action 'none'; frame-ancestors 'none'"
    );
    let mut r = (
        StatusCode::OK,
        [(header::CONTENT_TYPE, "text/html; charset=utf-8")],
        page(),
    )
        .into_response();
    if let Ok(v) = HeaderValue::from_str(&csp) {
        r.headers_mut().insert(header::CONTENT_SECURITY_POLICY, v);
    }
    r.headers_mut().insert(
        header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    );
    r
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn embedded_public_documents_contain_no_internal_surface() {
        for (d, _, body) in DOCS
            .iter()
            .chain(std::iter::once(&("aggregate", "", AGGREGATE)))
        {
            let v: Value = serde_json::from_str(body).unwrap();
            for p in v["paths"].as_object().unwrap().keys() {
                assert!(
                    !p.starts_with("/internal")
                        && !p.contains("/admin")
                        && !p.contains("provider-callbacks"),
                    "{d} {p}"
                );
            }
            let schemes = v["components"]["securitySchemes"]
                .as_object()
                .cloned()
                .unwrap_or_default();
            for s in schemes.keys() {
                assert!(
                    !s.contains("cfServiceAuth")
                        && !s.contains("serviceToken")
                        && !s.contains("providerSignature"),
                    "{d} {s}"
                );
            }
        }
        assert!(page().matches("integrity=\"sha384-").count() == 2);
    }

    /// Every routable public operation is documented in the aggregate document (and its
    /// domain document), and nothing undocumented is routable.
    #[test]
    fn documentation_is_in_parity_with_the_route_table() {
        let table =
            crate::catalogue::table_for(true, true, &crate::routing::RoutingControls::default())
                .unwrap();
        let agg: Value = serde_json::from_str(AGGREGATE).unwrap();
        let mut documented = 0;
        for (_, ops) in agg["paths"].as_object().unwrap() {
            documented += ops
                .as_object()
                .unwrap()
                .keys()
                .filter(|k| ["get", "post", "put", "patch", "delete"].contains(&k.as_str()))
                .count();
        }
        assert_eq!(documented, table.len());
        for r in table.definitions() {
            let op = &agg["paths"][r.pattern.as_str()][r.method.as_str().to_ascii_lowercase()];
            assert!(op.is_object(), "{} {} undocumented", r.method, r.pattern);
            let (_, _, body) = DOCS.iter().find(|(d, _, _)| *d == r.domain).unwrap();
            let doc: Value = serde_json::from_str(body).unwrap();
            assert!(
                doc["paths"][r.pattern.as_str()].is_object(),
                "{} missing from {}",
                r.pattern,
                r.domain
            );
        }
    }
}
