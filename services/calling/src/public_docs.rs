//! Public API documentation for the Part III communications surface.
//!
//! The authoritative contracts stay in `packages/contracts`. This module embeds those
//! contracts at compile time and publishes ONLY the customer/developer-facing documents.
//! Internal service-authenticated routes, provider callbacks and provider-specific APIs
//! are deliberately excluded from public Swagger.
//!
//! Swagger UI presents two public documents:
//! - Calling (direct/app calling, enterprise calling, webhooks, usage/limits, safety);
//! - Privacy Relay (contexts, sessions, Contact Vault public ingress and evidence).
//!
//! Telephony and Privacy Relay internal APIs remain contract-documented in
//! `packages/contracts`, but are not exposed by this public documentation router.

use axum::{
    http::header,
    response::{Html, IntoResponse, Redirect, Response},
    routing::get,
    Router,
};
use serde_json::{json, Value};

use crate::state::AppState;

const CALLING_OPENAPI: &str =
    include_str!("../../../packages/contracts/calling/http/v1.openapi.json");
const PRIVACY_RELAY_OPENAPI: &str =
    include_str!("../../../packages/contracts/privacy-relay/http/v1.openapi.json");

const SWAGGER_UI_HTML: &str = r#"<!doctype html>
<html lang="en">
<head>
  <meta charset="utf-8" />
  <meta name="viewport" content="width=device-width, initial-scale=1" />
  <title>CF Meet Communications API</title>
  <link rel="stylesheet" href="https://cdn.jsdelivr.net/npm/swagger-ui-dist@5.17.14/swagger-ui.css" />
  <style>
    html { box-sizing: border-box; overflow-y: scroll; }
    *, *::before, *::after { box-sizing: inherit; }
    body { margin: 0; background: #fafafa; }
  </style>
</head>
<body>
  <div id="swagger-ui"></div>
  <script src="https://cdn.jsdelivr.net/npm/swagger-ui-dist@5.17.14/swagger-ui-bundle.js"></script>
  <script>
    window.onload = () => {
      window.ui = SwaggerUIBundle({
        dom_id: '#swagger-ui',
        deepLinking: true,
        displayRequestDuration: true,
        filter: true,
        persistAuthorization: true,
        urls: [
          { url: '/api-docs/calling/openapi.json', name: 'Calling' },
          { url: '/api-docs/privacy-relay/openapi.json', name: 'Privacy Relay' }
        ],
        'urls.primaryName': 'Calling'
      });
    };
  </script>
</body>
</html>
"#;

/// Public, unauthenticated documentation routes. They contain schemas and descriptions
/// only; protected API operations still require their normal credentials when "Try it
/// out" is used.
pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/swagger-ui", get(swagger_redirect))
        .route("/swagger-ui/", get(swagger_ui))
        .route("/api-docs/calling/openapi.json", get(calling_openapi))
        .route(
            "/api-docs/privacy-relay/openapi.json",
            get(privacy_relay_openapi),
        )
}

async fn swagger_redirect() -> Redirect {
    Redirect::permanent("/swagger-ui/")
}

async fn swagger_ui() -> Response {
    let mut response = Html(SWAGGER_UI_HTML).into_response();
    response.headers_mut().insert(
        header::CACHE_CONTROL,
        "public, max-age=300".parse().expect("static header"),
    );
    response
}

async fn calling_openapi() -> Response {
    json_response(public_calling_document())
}

async fn privacy_relay_openapi() -> Response {
    json_response(public_privacy_relay_document())
}

fn json_response(document: Value) -> Response {
    let body = serde_json::to_string(&document).expect("embedded OpenAPI must serialize");
    let mut response = body.into_response();
    response.headers_mut().insert(
        header::CONTENT_TYPE,
        "application/json; charset=utf-8"
            .parse()
            .expect("static header"),
    );
    response.headers_mut().insert(
        header::CACHE_CONTROL,
        "public, max-age=300".parse().expect("static header"),
    );
    response
}

fn public_calling_document() -> Value {
    let mut document: Value =
        serde_json::from_str(CALLING_OPENAPI).expect("Calling OpenAPI contract must be valid JSON");

    if let Some(paths) = document.get_mut("paths").and_then(Value::as_object_mut) {
        // Calling's authoritative contract also contains service-to-service and provider
        // callbacks. Those remain available to CI/operators from packages/contracts, but
        // are not a public developer surface.
        paths.retain(|path, _| !path.starts_with("/internal/"));
        tag_operations(paths, calling_tag);
    }

    if let Some(security) = document
        .get_mut("components")
        .and_then(Value::as_object_mut)
        .and_then(|components| components.get_mut("securitySchemes"))
        .and_then(Value::as_object_mut)
    {
        security.remove("cfServiceAuth");
        security.remove("mediaProviderSignature");
    }

    document["info"]["title"] = json!("CF Meet Part III — Calling API");
    document["info"]["description"] = json!(
        "Public CF Meet Calling API for direct app calling, ringing and device signaling,          call control and history, enterprise calling, recordings/voicemail, customer          webhooks, usage/limits and abuse controls. Internal service, provider and          telephony execution endpoints are intentionally excluded."
    );
    document["servers"] = json!([{
        "url": "/",
        "description": "Current CF Meet public API host"
    }]);
    document["tags"] = json!([
        {"name": "Calling", "description": "Create, list and inspect direct calls."},
        {"name": "Call Control", "description": "Accept, decline, cancel, end, hold, resume and transfer calls."},
        {"name": "Realtime Calling", "description": "Devices, invitations, signaling and media-session access."},
        {"name": "Call History", "description": "Privacy-safe call history."},
        {"name": "Enterprise Calling", "description": "Extensions, departments, ring groups, queues, IVR, business hours, forwarding and voicemail."},
        {"name": "Recordings", "description": "Recording policy, consent, lifecycle and authorized media access."},
        {"name": "Webhooks", "description": "Customer webhook subscriptions, delivery history and replay."},
        {"name": "Usage & Limits", "description": "Usage facts and application limits."},
        {"name": "Safety & Abuse", "description": "Blocks and abuse-report workflows."},
        {"name": "Service", "description": "Public service health."}
    ]);

    document
}

fn public_privacy_relay_document() -> Value {
    let mut document: Value = serde_json::from_str(PRIVACY_RELAY_OPENAPI)
        .expect("Privacy Relay public OpenAPI contract must be valid JSON");

    if let Some(paths) = document.get_mut("paths").and_then(Value::as_object_mut) {
        // The source contract is already the public Privacy Relay surface; keep the
        // defensive filter so a future accidental internal path cannot become public docs.
        paths.retain(|path, _| !path.starts_with("/internal/"));
        tag_operations(paths, privacy_relay_tag);
    }

    document["info"]["title"] = json!("CF Meet Part III — Privacy Relay API");
    document["info"]["description"] = json!(
        "Public Privacy Relay API for Communication Contexts, relay sessions, Contact          Vault access, verification and privacy-safe evidence. Raw private contact values          are accepted only by the explicitly authorized Contact Vault write flow and are          never returned by the API."
    );
    document["servers"] = json!([{
        "url": "/",
        "description": "Current CF Meet public API host"
    }]);
    document["tags"] = json!([
        {"name": "Privacy Relay", "description": "Communication Context and relay-session lifecycle."},
        {"name": "Contact Vault", "description": "Protected-contact grants, creation, metadata and verification."},
        {"name": "Privacy Evidence", "description": "Privacy-safe authorization and call evidence."}
    ]);

    document
}

fn tag_operations(
    paths: &mut serde_json::Map<String, Value>,
    tag_for_path: fn(&str) -> &'static str,
) {
    const METHODS: [&str; 7] = ["get", "post", "put", "patch", "delete", "options", "head"];

    for (path, item) in paths {
        let Some(item) = item.as_object_mut() else {
            continue;
        };
        let tag = tag_for_path(path);
        for method in METHODS {
            if let Some(operation) = item.get_mut(method).and_then(Value::as_object_mut) {
                operation.insert("tags".to_string(), json!([tag]));
            }
        }
    }
}

fn calling_tag(path: &str) -> &'static str {
    if path == "/health" {
        "Service"
    } else if path == "/v1/calling/history" {
        "Call History"
    } else if path.starts_with("/v1/calling/") {
        "Realtime Calling"
    } else if path.starts_with("/v1/webhooks") || path.starts_with("/v1/webhook-deliveries") {
        "Webhooks"
    } else if path == "/v1/usage" || path == "/v1/limits" {
        "Usage & Limits"
    } else if path.starts_with("/v1/blocks")
        || path == "/v1/abuse-reports"
        || path.ends_with("/abuse-reports")
    {
        "Safety & Abuse"
    } else if path.contains("/recording") {
        "Recordings"
    } else if path.starts_with("/v1/enterprise/") || path.ends_with("/ivr/input") {
        "Enterprise Calling"
    } else if path == "/v1/calls" || path.matches('/').count() == 3 && path.starts_with("/v1/calls/") {
        "Calling"
    } else {
        "Call Control"
    }
}

fn privacy_relay_tag(path: &str) -> &'static str {
    if path.contains("/evidence") {
        "Privacy Evidence"
    } else if path.contains("/contacts") || path.contains("/contact-access-grants") {
        "Contact Vault"
    } else {
        "Privacy Relay"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn operation_count(document: &Value) -> usize {
        const METHODS: [&str; 7] = ["get", "post", "put", "patch", "delete", "options", "head"];
        document
            .get("paths")
            .and_then(Value::as_object)
            .into_iter()
            .flat_map(|paths| paths.values())
            .filter_map(Value::as_object)
            .map(|item| METHODS.iter().filter(|method| item.contains_key(*method)).count())
            .sum()
    }

    #[test]
    fn public_calling_swagger_contains_only_the_public_surface() {
        let document = public_calling_document();
        let paths = document["paths"].as_object().expect("paths");

        assert_eq!(paths.len(), 83);
        assert_eq!(operation_count(&document), 127);
        assert!(paths.contains_key("/v1/calls"));
        assert!(paths.contains_key("/v1/calling/endpoints"));
        assert!(paths.contains_key("/v1/enterprise/extensions"));
        assert!(paths.contains_key("/v1/webhooks"));
        assert!(paths.contains_key("/v1/usage"));
        assert!(paths.keys().all(|path| !path.starts_with("/internal/")));

        let security = document["components"]["securitySchemes"]
            .as_object()
            .expect("security schemes");
        assert!(!security.contains_key("cfServiceAuth"));
        assert!(!security.contains_key("mediaProviderSignature"));
        assert!(security.contains_key("platformCredential"));
    }

    #[test]
    fn public_privacy_relay_swagger_is_the_public_contract_only() {
        let document = public_privacy_relay_document();
        let paths = document["paths"].as_object().expect("paths");

        assert_eq!(paths.len(), 19);
        assert_eq!(operation_count(&document), 22);
        assert!(paths.contains_key("/v1/privacy-relay/contexts"));
        assert!(paths.contains_key("/v1/privacy-relay/sessions"));
        assert!(paths.contains_key("/v1/privacy-relay/contacts"));
        assert!(paths.keys().all(|path| !path.starts_with("/internal/")));
    }

    #[test]
    fn swagger_selector_exposes_calling_and_privacy_relay_but_not_internal_telephony() {
        assert!(SWAGGER_UI_HTML.contains("/api-docs/calling/openapi.json"));
        assert!(SWAGGER_UI_HTML.contains("/api-docs/privacy-relay/openapi.json"));
        assert!(!SWAGGER_UI_HTML.contains("telephony/internal"));
        assert!(!SWAGGER_UI_HTML.contains("privacy-relay/internal"));
    }
}
