//! Calling's own API documentation page (Blueprint v1.0 Section 16, PLT-006).
//!
//! There is ONE source of truth for public API documents: the generated public bundles
//! `packages/contracts/bundled/public/**` (written by `scripts/contracts/public_api.py`
//! and `scripts/contracts/bundle_openapi.py` from the source contracts and the gateway
//! catalogue `infra/api-gateway/public-routes.json`). The API Gateway (`GET /docs`) and the
//! Meeting Service Swagger serve the same files; the private engineering catalogue is
//! `scripts/swagger/build-private-docs.py`. This module serves exactly two of those
//! bundles, unchanged apart from tag declarations:
//!
//! * Calling (`bundled/public/calling`): the gateway's PUBLIC_CUSTOMER Calling operations;
//! * Privacy Relay (`bundled/public/privacy-relay`): the relay ingress Calling hosts.
//!   Contact Vault private-contact operations (`/v1/privacy-relay/contacts*`, class
//!   `EXCLUDED` / `CONTACT_VAULT_PRIVATE`) are absent: they are not public and Calling
//!   does not serve them.
//!
//! Hardening (same as the Meeting Service and gateway renderers, plus access control):
//!
//! * **Off by default.** `CALLING_DOCS_ENABLED` (default `false`) mounts the routes;
//!   otherwise every docs path is an ordinary 404.
//! * **Authenticated.** With `CALLING_DOCS_ACCESS_TOKEN` (or `CALLING_DOCS_ACCESS_TOKEN_FILE`,
//!   preferred) every docs route requires HTTP Basic authentication whose password is that
//!   token (any user name). Without a token the page is served only in the `local` /
//!   `test` runtime environments (`CF_RUNTIME_ENVIRONMENT`); anywhere else an enabled page
//!   without a token is refused and stays unmounted (fail closed, logged).
//! * swagger-ui-dist pinned by version and Subresource Integrity, a restrictive CSP whose
//!   only inline script is allowed by hash, no "try it out" (`supportedSubmitMethods: []`),
//!   `persistAuthorization: false`, `no-store` caching, `nosniff`, no framing.

use std::sync::Arc;

use axum::{
    extract::{Request, State},
    http::{header, HeaderMap, HeaderValue, StatusCode},
    middleware::{from_fn_with_state, Next},
    response::{Html, IntoResponse, Redirect, Response},
    routing::get,
    Router,
};
use base64::Engine;
use secrecy::ExposeSecret;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

use crate::{
    config::{secret, ConfigError},
    state::AppState,
};

/// The generated public bundles (every `$ref` already resolved inside the document).
const CALLING_PUBLIC_OPENAPI: &str =
    include_str!("../../../packages/contracts/bundled/public/calling/v1.openapi.json");
const PRIVACY_RELAY_PUBLIC_OPENAPI: &str =
    include_str!("../../../packages/contracts/bundled/public/privacy-relay/v1.openapi.json");

/// (selector name, served path, bundle).
const DOCS: [(&str, &str, &str); 2] = [
    (
        "Calling",
        "/api-docs/calling/openapi.json",
        CALLING_PUBLIC_OPENAPI,
    ),
    (
        "Privacy Relay",
        "/api-docs/privacy-relay/openapi.json",
        PRIVACY_RELAY_PUBLIC_OPENAPI,
    ),
];

/// Same pinned swagger-ui-dist release and SRI digests as the Meeting Service
/// (`services/meeting-service/src/swagger/mod.rs`) and the gateway (`api-gateway/src/docs.rs`).
const SWAGGER_UI_VERSION: &str = "5.17.14";
const SWAGGER_UI_CSS_SRI: &str =
    "sha384-wxLW6kwyHktdDGr6Pv1zgm/VGJh99lfUbzSn6HNHBENZlCN7W602k9VkGdxuFvPn";
const SWAGGER_UI_JS_SRI: &str =
    "sha384-wmyclcVGX/WhUkdkATwhaK1X1JtiNrr2EoYJ+diV3vj4v6OC5yCeSu+yW13SYJep";

const SWAGGER_INIT: &str = r#"
window.addEventListener('load', function() {
  var selector = document.getElementById('api-document');
  function render(url) {
    window.ui = SwaggerUIBundle({ url: url, dom_id: '#swagger-ui', deepLinking: true,
      displayRequestDuration: true, filter: true, persistAuthorization: false,
      supportedSubmitMethods: [], validatorUrl: null });
  }
  selector.addEventListener('change', function() { render(selector.value); });
  render(selector.value);
});
"#;

const REALM: &str = "Basic realm=\"CF Meet Calling API documentation\", charset=\"UTF-8\"";

/// How the documentation routes are exposed.
#[derive(Clone)]
pub enum DocsAccess {
    /// Not mounted (the default).
    Disabled,
    /// Mounted without authentication (`local` / `test` runtime only).
    Open,
    /// Mounted behind HTTP Basic; holds the SHA-256 of the access token, never the token.
    Token([u8; 32]),
}

impl std::fmt::Debug for DocsAccess {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            DocsAccess::Disabled => "Disabled",
            DocsAccess::Open => "Open",
            DocsAccess::Token(_) => "Token([REDACTED])",
        })
    }
}

impl DocsAccess {
    pub fn from_env() -> Result<Self, ConfigError> {
        Self::from_lookup(|key| std::env::var(key).ok())
    }

    /// `CALLING_DOCS_ENABLED` (default `false`), `CALLING_DOCS_ACCESS_TOKEN[_FILE]`,
    /// `CF_RUNTIME_ENVIRONMENT`.
    pub fn from_lookup(get: impl Fn(&str) -> Option<String>) -> Result<Self, ConfigError> {
        let enabled = match get("CALLING_DOCS_ENABLED")
            .map(|v| v.trim().to_ascii_lowercase())
            .as_deref()
        {
            None | Some("") | Some("0") | Some("false") | Some("no") | Some("off") => false,
            Some("1") | Some("true") | Some("yes") | Some("on") => true,
            Some(_) => {
                return Err(ConfigError::invalid(
                    "CALLING_DOCS_ENABLED",
                    "must be true or false".to_string(),
                ))
            }
        };
        if !enabled {
            return Ok(DocsAccess::Disabled);
        }
        let token = secret::read_secret(
            &get,
            "CALLING_DOCS_ACCESS_TOKEN",
            "CALLING_DOCS_ACCESS_TOKEN_FILE",
        )?;
        match token {
            Some(token) => {
                let token = token.expose_secret();
                if token.len() < 32 {
                    return Err(ConfigError::invalid(
                        "CALLING_DOCS_ACCESS_TOKEN",
                        "must be at least 32 characters".to_string(),
                    ));
                }
                Ok(DocsAccess::Token(Sha256::digest(token.as_bytes()).into()))
            }
            None if secret::permissive_runtime(&get) => Ok(DocsAccess::Open),
            None => Err(ConfigError::Missing {
                var: "CALLING_DOCS_ACCESS_TOKEN",
            }),
        }
    }
}

/// Documentation routes from the environment. A configuration error keeps the routes
/// unmounted (fail closed) and is logged; it never stops the Calling API.
pub fn routes() -> Router<AppState> {
    let access = DocsAccess::from_env().unwrap_or_else(|error| {
        tracing::error!(%error, "Calling API documentation stays disabled");
        DocsAccess::Disabled
    });
    routes_with(access)
}

pub fn routes_with(access: DocsAccess) -> Router<AppState> {
    docs_router(access)
}

/// The documentation handlers never read the application state.
fn docs_router<S: Clone + Send + Sync + 'static>(access: DocsAccess) -> Router<S> {
    let router = Router::new()
        .route("/swagger-ui", get(swagger_redirect))
        .route("/swagger-ui/", get(swagger_ui));
    let router = DOCS.iter().fold(router, |router, (_, path, raw)| {
        let body = Arc::new(
            serde_json::to_string(&public_document(raw)).expect("public document serializes"),
        );
        router.route(
            path,
            get(move || {
                let body = body.clone();
                async move { json_response(body.as_str().to_owned()) }
            }),
        )
    });
    match access {
        DocsAccess::Disabled => Router::new(),
        DocsAccess::Open => router,
        DocsAccess::Token(digest) => {
            router.route_layer(from_fn_with_state(Arc::new(digest), require_docs_token))
        }
    }
}

async fn require_docs_token(
    State(digest): State<Arc<[u8; 32]>>,
    request: Request,
    next: Next,
) -> Response {
    if basic_password_matches(request.headers(), &digest) {
        return next.run(request).await;
    }
    let mut response = StatusCode::UNAUTHORIZED.into_response();
    let headers = response.headers_mut();
    headers.insert(header::WWW_AUTHENTICATE, HeaderValue::from_static(REALM));
    headers.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    response
}

/// `Authorization: Basic base64(user:password)` whose password hashes to `digest`
/// (compared in constant time over the fixed-length digests).
fn basic_password_matches(headers: &HeaderMap, digest: &[u8; 32]) -> bool {
    let Some(encoded) = headers
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Basic "))
    else {
        return false;
    };
    let Ok(decoded) = base64::engine::general_purpose::STANDARD.decode(encoded.trim()) else {
        return false;
    };
    let Some(colon) = decoded.iter().position(|b| *b == b':') else {
        return false;
    };
    let presented: [u8; 32] = Sha256::digest(&decoded[colon + 1..]).into();
    presented
        .iter()
        .zip(digest.iter())
        .fold(0u8, |acc, (a, b)| acc | (a ^ b))
        == 0
}

async fn swagger_redirect() -> Redirect {
    Redirect::permanent("/swagger-ui/")
}

fn swagger_page() -> String {
    let options: String = DOCS
        .iter()
        .map(|(name, path, _)| format!(r#"<option value="{path}">{name}</option>"#))
        .collect();
    format!(
        r#"<!doctype html>
<html lang="en">
<head>
  <meta charset="utf-8" />
  <meta name="viewport" content="width=device-width, initial-scale=1" />
  <meta name="robots" content="noindex, nofollow, noarchive" />
  <title>CF Meet Calling API documentation</title>
  <link rel="stylesheet" href="https://cdn.jsdelivr.net/npm/swagger-ui-dist@{v}/swagger-ui.css" integrity="{css}" crossorigin="anonymous" referrerpolicy="no-referrer" />
</head>
<body>
  <div style="padding:12px 20px;font:14px sans-serif"><label for="api-document">API: </label><select id="api-document" aria-label="Select API document">{options}</select></div>
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

fn swagger_csp() -> String {
    let hash = base64::engine::general_purpose::STANDARD.encode(Sha256::digest(SWAGGER_INIT));
    format!(
        "default-src 'none'; script-src https://cdn.jsdelivr.net 'sha256-{hash}'; \
         style-src https://cdn.jsdelivr.net 'unsafe-inline'; img-src 'self' data:; \
         connect-src 'self'; base-uri 'none'; form-action 'none'; frame-ancestors 'none'"
    )
}

async fn swagger_ui() -> Response {
    let mut response = Html(swagger_page()).into_response();
    let headers = response.headers_mut();
    if let Ok(csp) = HeaderValue::from_str(&swagger_csp()) {
        headers.insert(header::CONTENT_SECURITY_POLICY, csp);
    }
    hardening_headers(headers);
    response
}

fn json_response(body: String) -> Response {
    let mut response = body.into_response();
    let headers = response.headers_mut();
    headers.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("application/json; charset=utf-8"),
    );
    hardening_headers(headers);
    response
}

fn hardening_headers(headers: &mut HeaderMap) {
    headers.insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static("private, no-store"),
    );
    headers.insert(
        header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    );
    headers.insert(
        header::REFERRER_POLICY,
        HeaderValue::from_static("no-referrer"),
    );
}

/// The bundle as generated, plus top-level declarations for the operation tags it uses
/// (the public bundles carry operation tags but no `tags` array). No operation is added,
/// removed or rewritten.
fn public_document(raw: &str) -> Value {
    let mut document: Value =
        serde_json::from_str(raw).expect("generated public bundle must be valid JSON");
    let used: std::collections::BTreeSet<String> = document["paths"]
        .as_object()
        .into_iter()
        .flat_map(|paths| paths.values())
        .filter_map(Value::as_object)
        .flat_map(|item| item.values())
        .filter_map(|operation| operation["tags"].as_array())
        .flatten()
        .filter_map(|tag| tag.as_str().map(str::to_owned))
        .collect();
    let mut tags: Vec<Value> = document["tags"].as_array().cloned().unwrap_or_default();
    tags.retain(|tag| tag["name"].as_str().is_some_and(|name| used.contains(name)));
    let declared: std::collections::BTreeSet<String> = tags
        .iter()
        .filter_map(|tag| tag["name"].as_str().map(str::to_owned))
        .collect();
    for name in used.difference(&declared) {
        tags.push(json!({ "name": name }));
    }
    document["tags"] = Value::Array(tags);
    document
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use tower::ServiceExt;

    const METHODS: [&str; 8] = [
        "get", "post", "put", "patch", "delete", "options", "head", "trace",
    ];

    fn operations(document: &Value) -> std::collections::BTreeSet<(String, String)> {
        document["paths"]
            .as_object()
            .into_iter()
            .flatten()
            .flat_map(|(path, item)| {
                METHODS
                    .iter()
                    .filter(|m| item.get(**m).is_some())
                    .map(move |m| (m.to_uppercase(), path.clone()))
            })
            .collect()
    }

    fn references(value: &Value, output: &mut Vec<String>) {
        match value {
            Value::Array(values) => values.iter().for_each(|v| references(v, output)),
            Value::Object(object) => {
                if let Some(reference) = object.get("$ref").and_then(Value::as_str) {
                    output.push(reference.to_string());
                }
                object.values().for_each(|v| references(v, output));
            }
            _ => {}
        }
    }

    fn lookup(pairs: &[(&str, &str)]) -> impl Fn(&str) -> Option<String> {
        let pairs: Vec<(String, String)> = pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        move |key| pairs.iter().find(|(k, _)| k == key).map(|(_, v)| v.clone())
    }

    const TOKEN: &str = "docs-token-0123456789abcdef0123456789";

    async fn get_status(router: &Router, uri: &str, auth: Option<String>) -> Response {
        let mut request = axum::http::Request::builder().uri(uri);
        if let Some(auth) = auth {
            request = request.header(header::AUTHORIZATION, auth);
        }
        router
            .clone()
            .oneshot(request.body(Body::empty()).unwrap())
            .await
            .unwrap()
    }

    fn basic(user: &str, password: &str) -> String {
        format!(
            "Basic {}",
            base64::engine::general_purpose::STANDARD.encode(format!("{user}:{password}"))
        )
    }

    #[test]
    fn documentation_is_off_by_default_and_needs_a_token_outside_local() {
        assert!(matches!(
            DocsAccess::from_lookup(lookup(&[])).unwrap(),
            DocsAccess::Disabled
        ));
        assert!(matches!(
            DocsAccess::from_lookup(lookup(&[("CALLING_DOCS_ENABLED", "false")])).unwrap(),
            DocsAccess::Disabled
        ));
        assert!(DocsAccess::from_lookup(lookup(&[("CALLING_DOCS_ENABLED", "maybe")])).is_err());
        // Enabled without a token: refused in production / unset runtime, open locally.
        for runtime in [None, Some("production"), Some("staging")] {
            let mut pairs = vec![("CALLING_DOCS_ENABLED", "true")];
            if let Some(r) = runtime {
                pairs.push(("CF_RUNTIME_ENVIRONMENT", r));
            }
            assert!(
                matches!(
                    DocsAccess::from_lookup(lookup(&pairs)),
                    Err(ConfigError::Missing {
                        var: "CALLING_DOCS_ACCESS_TOKEN"
                    })
                ),
                "{runtime:?}"
            );
        }
        for runtime in ["local", "test"] {
            assert!(matches!(
                DocsAccess::from_lookup(lookup(&[
                    ("CALLING_DOCS_ENABLED", "true"),
                    ("CF_RUNTIME_ENVIRONMENT", runtime)
                ]))
                .unwrap(),
                DocsAccess::Open
            ));
        }
        assert!(DocsAccess::from_lookup(lookup(&[
            ("CALLING_DOCS_ENABLED", "true"),
            ("CALLING_DOCS_ACCESS_TOKEN", "short")
        ]))
        .is_err());
        let access = DocsAccess::from_lookup(lookup(&[
            ("CALLING_DOCS_ENABLED", "true"),
            ("CALLING_DOCS_ACCESS_TOKEN", TOKEN),
        ]))
        .unwrap();
        assert!(matches!(access, DocsAccess::Token(_)));
        assert!(!format!("{access:?}").contains(TOKEN));
    }

    #[tokio::test]
    async fn disabled_documentation_is_not_mounted() {
        let router = docs_router(DocsAccess::Disabled);
        for uri in [
            "/swagger-ui/",
            "/api-docs/calling/openapi.json",
            "/api-docs/privacy-relay/openapi.json",
        ] {
            assert_eq!(
                get_status(&router, uri, None).await.status(),
                StatusCode::NOT_FOUND,
                "{uri}"
            );
        }
    }

    #[tokio::test]
    async fn token_mode_requires_the_access_token_on_every_route() {
        let digest: [u8; 32] = Sha256::digest(TOKEN.as_bytes()).into();
        let router = docs_router(DocsAccess::Token(digest));
        for uri in [
            "/swagger-ui/",
            "/api-docs/calling/openapi.json",
            "/api-docs/privacy-relay/openapi.json",
        ] {
            let denied = get_status(&router, uri, None).await;
            assert_eq!(denied.status(), StatusCode::UNAUTHORIZED, "{uri}");
            assert!(denied.headers()[header::WWW_AUTHENTICATE]
                .to_str()
                .unwrap()
                .starts_with("Basic "));
            for wrong in [
                basic("docs", "wrong-token"),
                format!("Bearer {TOKEN}"),
                "Basic !!!".to_string(),
                basic("docs", &format!("{TOKEN}x")),
            ] {
                assert_eq!(
                    get_status(&router, uri, Some(wrong.clone())).await.status(),
                    StatusCode::UNAUTHORIZED,
                    "{uri} {wrong}"
                );
            }
            let ok = get_status(&router, uri, Some(basic("anyone", TOKEN))).await;
            assert_eq!(ok.status(), StatusCode::OK, "{uri}");
            assert_eq!(ok.headers()[header::CACHE_CONTROL], "private, no-store");
            assert_eq!(ok.headers()[header::X_CONTENT_TYPE_OPTIONS], "nosniff");
        }
    }

    #[tokio::test]
    async fn swagger_page_is_pinned_read_only_and_under_a_restrictive_csp() {
        let router = docs_router(DocsAccess::Open);
        let response = get_status(&router, "/swagger-ui/", None).await;
        assert_eq!(response.status(), StatusCode::OK);
        let csp = response.headers()[header::CONTENT_SECURITY_POLICY]
            .to_str()
            .unwrap()
            .to_string();
        assert!(csp.contains("default-src 'none'") && csp.contains("connect-src 'self'"));
        assert!(csp.contains("frame-ancestors 'none'") && !csp.contains("'unsafe-eval'"));
        let hash = base64::engine::general_purpose::STANDARD.encode(Sha256::digest(SWAGGER_INIT));
        assert!(csp.contains(&format!("'sha256-{hash}'")));

        let page = swagger_page();
        assert_eq!(
            page.matches("integrity=\"sha384-").count(),
            2,
            "SRI on CSS and JS"
        );
        assert_eq!(
            page.matches("<script").count(),
            2,
            "bundle + hashed init only"
        );
        assert!(page.contains(&format!("swagger-ui-dist@{SWAGGER_UI_VERSION}/")));
        assert!(!page.contains("standalone-preset"));
        assert!(SWAGGER_INIT.contains("supportedSubmitMethods: []"));
        assert!(SWAGGER_INIT.contains("persistAuthorization: false"));
        assert!(!SWAGGER_INIT.contains("tryItOutEnabled"));
        assert!(!SWAGGER_INIT.contains("requestInterceptor"));
    }

    /// The served documents ARE the generated public bundles: the same operations, every
    /// reference local, no Contact Vault private-contact operation, no internal surface.
    #[test]
    fn served_documents_are_the_generated_public_bundles() {
        for (name, _, raw) in DOCS {
            let bundle: Value = serde_json::from_str(raw).unwrap();
            let served = public_document(raw);
            assert_eq!(operations(&served), operations(&bundle), "{name}");
            assert!(!operations(&served).is_empty(), "{name}");
            for (_, path) in operations(&served) {
                assert!(!path.starts_with("/internal/"), "{name}: {path}");
                assert!(
                    !path.starts_with("/v1/privacy-relay/contacts"),
                    "{name}: {path}"
                );
            }
            let mut refs = Vec::new();
            references(&served, &mut refs);
            assert!(refs.iter().all(|r| r.starts_with("#/")), "{name}: {refs:?}");
            let declared: std::collections::BTreeSet<&str> = served["tags"]
                .as_array()
                .unwrap()
                .iter()
                .filter_map(|t| t["name"].as_str())
                .collect();
            for item in served["paths"].as_object().unwrap().values() {
                for method in METHODS {
                    for tag in item[method]["tags"].as_array().into_iter().flatten() {
                        assert!(declared.contains(tag.as_str().unwrap()), "{name}: {tag}");
                    }
                }
            }
            let schemes = served["components"]["securitySchemes"]
                .as_object()
                .map(|s| s.keys().cloned().collect::<Vec<_>>())
                .unwrap_or_default();
            assert!(!schemes.iter().any(|s| s == "cfServiceAuth"), "{name}");
        }
        let calling = public_document(CALLING_PUBLIC_OPENAPI);
        assert!(calling["paths"]["/v1/calls"].is_object());
        assert!(calling["paths"]
            .as_object()
            .unwrap()
            .keys()
            .any(|p| p.starts_with("/v1/enterprise/")));
        let relay = public_document(PRIVACY_RELAY_PUBLIC_OPENAPI);
        assert!(relay["paths"]["/v1/privacy-relay/contexts"].is_object());
        assert!(relay["paths"]["/v1/privacy-relay/contacts"].is_null());
    }
}
