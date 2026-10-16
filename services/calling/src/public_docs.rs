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
    extract::Path,
    http::{header, StatusCode},
    response::{Html, IntoResponse, Redirect, Response},
    routing::get,
    Router,
};
use serde_json::{json, Value};

use crate::state::AppState;

const SCHEMA_BASE: &str = "https://schemas.cfmeet.com/";

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
        .route("/api-docs/schemas/*path", get(schema))
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

async fn schema(Path(path): Path<String>) -> Response {
    let Some(raw) = schema_file(&path) else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let mut document: Value =
        serde_json::from_str(raw).expect("embedded public schema must be valid JSON");
    rewrite_schema_refs(&mut document);
    json_response(document)
}

fn json_response(document: Value) -> Response {
    let body = serde_json::to_string(&document).expect("embedded API document must serialize");
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
    rewrite_schema_refs(&mut document);

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
    rewrite_schema_refs(&mut document);

    document
}

/// Public docs must not depend on the separate schemas.cfmeet.com deployment. Rewrite
/// every authoritative schema URL to the allow-listed same-origin schema endpoint below.
fn rewrite_schema_refs(value: &mut Value) {
    match value {
        Value::Array(values) => {
            for value in values {
                rewrite_schema_refs(value);
            }
        }
        Value::Object(object) => {
            let replacement = object
                .get("$ref")
                .and_then(Value::as_str)
                .and_then(|reference| reference.strip_prefix(SCHEMA_BASE))
                .map(|path| format!("/api-docs/schemas/{path}"));
            if let Some(replacement) = replacement {
                object.insert("$ref".to_string(), Value::String(replacement));
            }
            for value in object.values_mut() {
                rewrite_schema_refs(value);
            }
        }
        _ => {}
    }
}

/// Only schema dependencies reachable from the PUBLIC Calling and Privacy Relay OpenAPI
/// documents are served. Internal Telephony/provider/service-auth API contracts are not.
fn schema_file(path: &str) -> Option<&'static str> {
    match path {
        "calling/common/v1.schema.json" => Some(include_str!(
            "../../../packages/contracts/calling/common/v1.schema.json"
        )),
        "calling/control/v1.schema.json" => Some(include_str!(
            "../../../packages/contracts/calling/control/v1.schema.json"
        )),
        "calling/enterprise/v1.schema.json" => Some(include_str!(
            "../../../packages/contracts/calling/enterprise/v1.schema.json"
        )),
        "platform/common/v1.schema.json" => Some(include_str!(
            "../../../packages/contracts/platform/common/v1.schema.json"
        )),
        "platform/usage/v1.schema.json" => Some(include_str!(
            "../../../packages/contracts/platform/usage/v1.schema.json"
        )),
        "platform/webhooks/v1.schema.json" => Some(include_str!(
            "../../../packages/contracts/platform/webhooks/v1.schema.json"
        )),
        "platform/service-auth/delegated-platform-context/v1.schema.json" => Some(include_str!(
            "../../../packages/contracts/platform/service-auth/delegated-platform-context/v1.schema.json"
        )),
        "platform/service-auth/trust-bundle/v1.schema.json" => Some(include_str!(
            "../../../packages/contracts/platform/service-auth/trust-bundle/v1.schema.json"
        )),
        "privacy-relay/common/v1.schema.json" => Some(include_str!(
            "../../../packages/contracts/privacy-relay/common/v1.schema.json"
        )),
        "privacy-relay/contact-delegation/v1.schema.json" => Some(include_str!(
            "../../../packages/contracts/privacy-relay/contact-delegation/v1.schema.json"
        )),
        "privacy-relay/contact/v1.schema.json" => Some(include_str!(
            "../../../packages/contracts/privacy-relay/contact/v1.schema.json"
        )),
        "privacy-relay/context/v1.schema.json" => Some(include_str!(
            "../../../packages/contracts/privacy-relay/context/v1.schema.json"
        )),
        "privacy-relay/evidence/v1.schema.json" => Some(include_str!(
            "../../../packages/contracts/privacy-relay/evidence/v1.schema.json"
        )),
        "privacy-relay/events/v1.schema.json" => Some(include_str!(
            "../../../packages/contracts/privacy-relay/events/v1.schema.json"
        )),
        "privacy-relay/session/v1.schema.json" => Some(include_str!(
            "../../../packages/contracts/privacy-relay/session/v1.schema.json"
        )),
        _ => None,
    }
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
    } else if path == "/v1/calls"
        || (path.matches('/').count() == 3 && path.starts_with("/v1/calls/"))
    {
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
            .map(|item| {
                METHODS
                    .iter()
                    .filter(|method| item.contains_key(*method))
                    .count()
            })
            .sum()
    }

    fn references(value: &Value, output: &mut Vec<String>) {
        match value {
            Value::Array(values) => {
                for value in values {
                    references(value, output);
                }
            }
            Value::Object(object) => {
                if let Some(reference) = object.get("$ref").and_then(Value::as_str) {
                    output.push(reference.to_string());
                }
                for value in object.values() {
                    references(value, output);
                }
            }
            _ => {}
        }
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

        let mut refs = Vec::new();
        references(&document, &mut refs);
        assert!(refs
            .iter()
            .all(|reference| !reference.starts_with(SCHEMA_BASE)));
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

        let mut refs = Vec::new();
        references(&document, &mut refs);
        assert!(refs
            .iter()
            .all(|reference| !reference.starts_with(SCHEMA_BASE)));
    }

    #[test]
    fn every_public_schema_dependency_is_same_origin_and_allow_listed() {
        for path in [
            "calling/common/v1.schema.json",
            "calling/control/v1.schema.json",
            "calling/enterprise/v1.schema.json",
            "platform/common/v1.schema.json",
            "platform/usage/v1.schema.json",
            "platform/webhooks/v1.schema.json",
            "platform/service-auth/delegated-platform-context/v1.schema.json",
            "platform/service-auth/trust-bundle/v1.schema.json",
            "privacy-relay/common/v1.schema.json",
            "privacy-relay/contact-delegation/v1.schema.json",
            "privacy-relay/contact/v1.schema.json",
            "privacy-relay/context/v1.schema.json",
            "privacy-relay/evidence/v1.schema.json",
            "privacy-relay/events/v1.schema.json",
            "privacy-relay/session/v1.schema.json",
        ] {
            let raw = schema_file(path).expect("public dependency is allow-listed");
            let mut document: Value = serde_json::from_str(raw).expect("valid schema");
            rewrite_schema_refs(&mut document);
            let mut refs = Vec::new();
            references(&document, &mut refs);
            assert!(
                refs.iter()
                    .all(|reference| !reference.starts_with(SCHEMA_BASE)),
                "{path} still depends on the external schema host"
            );
        }

        assert!(schema_file("telephony/internal/v1.openapi.json").is_none());
        assert!(schema_file("privacy-relay/internal/v1.openapi.json").is_none());
    }

    #[test]
    fn swagger_selector_exposes_calling_and_privacy_relay_but_not_internal_telephony() {
        assert!(SWAGGER_UI_HTML.contains("/api-docs/calling/openapi.json"));
        assert!(SWAGGER_UI_HTML.contains("/api-docs/privacy-relay/openapi.json"));
        assert!(!SWAGGER_UI_HTML.contains("telephony/internal"));
        assert!(!SWAGGER_UI_HTML.contains("privacy-relay/internal"));
    }
}
