//! Platform Access conforms to its canonical contracts in `packages/contracts`, in both
//! directions:
//!
//! * the internal OpenAPI lists exactly the routes the service registers;
//! * real responses (authentication outcomes, trusted context, credential status,
//!   evaluation, scope registry, change feed, principal operations, errors) validate
//!   against `cfmeet.platform-access.identity`;
//! * the Rust vocabularies (scopes and trust, failure reasons, error codes) equal the
//!   contract enumerations.

mod common;

use std::{collections::BTreeSet, path::PathBuf};

use axum::http::Method;
use common::*;
use jsonschema::{Registry, Resource, Validator};
use serde_json::{json, Value};

const BASE: &str = "https://schemas.cfmeet.com/";
const IDENTITY: &str = "platform-access/identity/v1.schema.json";

fn contracts() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../packages/contracts")
}

fn read(rel: &str) -> Value {
    serde_json::from_str(&std::fs::read_to_string(contracts().join(rel)).unwrap()).unwrap()
}

fn registry() -> Registry<'static> {
    let manifest = read("manifest.json");
    let mut builder = Registry::new();
    for c in manifest["contracts"].as_array().unwrap() {
        if c["kind"] == "json-schema" {
            let doc = read(c["path"].as_str().unwrap());
            let id = doc["$id"].as_str().unwrap().to_string();
            builder = builder.add(id, Resource::from_contents(doc)).unwrap();
        }
    }
    builder.prepare().unwrap()
}

fn validator(def: &str) -> Validator {
    let registry = registry();
    jsonschema::options()
        .with_registry(&registry)
        .should_validate_formats(true)
        .build(&json!({
            "$schema": "https://json-schema.org/draft/2020-12/schema",
            "$ref": format!("{BASE}{IDENTITY}#/$defs/{def}")
        }))
        .unwrap()
}

#[track_caller]
fn conforms(def: &str, value: &Value) {
    let v = validator(def);
    let errors: Vec<String> = v
        .iter_errors(value)
        .map(|e| format!("{e} at {}", e.instance_path()))
        .collect();
    assert!(errors.is_empty(), "{def} violated: {errors:?}\n{value:#}");
}

fn contract_enum(def: &str) -> BTreeSet<String> {
    read(IDENTITY)["$defs"][def]["enum"]
        .as_array()
        .unwrap_or_else(|| panic!("{def}"))
        .iter()
        .map(|v| v.as_str().unwrap().to_string())
        .collect()
}

/// `(method, path)` of every route the service registers, from the router sources.
fn implemented_routes() -> BTreeSet<(String, String)> {
    let src = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src/http");
    let mut out = BTreeSet::new();
    for (file, prefix) in [
        ("mod.rs", ""),
        ("internal.rs", "/internal/v1"),
        ("admin.rs", "/internal/v1"),
    ] {
        let code = std::fs::read_to_string(src.join(file)).unwrap();
        let code: String = code.split_whitespace().collect::<Vec<_>>().join(" ");
        for chunk in code.split(".route(").skip(1) {
            let Some(path) = chunk.split('"').nth(1) else {
                continue;
            };
            if path.starts_with("/internal/v1") && file == "mod.rs" {
                continue; // the nest itself
            }
            let handlers = chunk.split(')').take(4).collect::<Vec<_>>().join(")");
            for method in ["get", "post", "put", "delete"] {
                if handlers.contains(&format!("{method}(")) {
                    let openapi_path = path
                        .split('/')
                        .map(|s| match s.strip_prefix(':') {
                            Some(name) => format!("{{{}}}", camel(name)),
                            None => s.to_string(),
                        })
                        .collect::<Vec<_>>()
                        .join("/");
                    out.insert((method.to_string(), format!("{prefix}{openapi_path}")));
                }
            }
        }
    }
    out
}

fn camel(snake: &str) -> String {
    let mut out = String::new();
    let mut upper = false;
    for c in snake.chars() {
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

#[test]
fn the_internal_openapi_lists_exactly_the_implemented_routes() {
    let doc = read("platform-access/internal/v1.openapi.json");
    let mut documented = BTreeSet::new();
    for (path, item) in doc["paths"].as_object().unwrap() {
        for method in item.as_object().unwrap().keys() {
            documented.insert((method.clone(), path.clone()));
        }
    }
    let implemented = implemented_routes();
    assert!(
        implemented.len() > 30,
        "route extraction sanity: {implemented:?}"
    );
    assert_eq!(documented, implemented);
}

#[test]
fn vocabularies_equal_the_contract() {
    use platform_access::{http::error::ErrorCode, identity::scopes::ScopeRegistry};
    let registry = ScopeRegistry::standard();
    let trust: BTreeSet<String> = registry
        .definitions()
        .map(|d| {
            serde_json::to_value(d.trust)
                .unwrap()
                .as_str()
                .unwrap()
                .to_string()
        })
        .collect();
    assert!(trust.is_subset(&contract_enum("ScopeTrust")));
    for name in [
        "numbers.search",
        "numbers.read",
        "numbers.manage",
        "numbers.assign",
        "numbers.kyc.read",
        "numbers.kyc.manage",
        "numbers.portability",
        "numbers.admin",
        "sms.send",
        "sms.read",
    ] {
        assert!(
            registry.definition(name).is_some(),
            "{name} is not registered"
        );
    }
    let codes: BTreeSet<String> = ErrorCode::ALL
        .iter()
        .map(|c| c.as_str().to_string())
        .collect();
    assert_eq!(codes, contract_enum("ErrorCode"));
    let reasons: BTreeSet<String> =
        platform_access::identity::authentication::AuthFailureReason::ALL
            .iter()
            .map(|r| r.as_str().to_string())
            .collect();
    assert_eq!(reasons, contract_enum("AuthFailureReason"));
}

#[tokio::test]
async fn real_responses_conform_to_the_identity_contract() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let c = h
        .consumer("acme", "FIRST_PARTY", &["calls.read", "numbers.search"])
        .await;
    let (credential, secret) = h
        .issue(c.sa("PRODUCTION"), &["calls.read", "numbers.search"])
        .await;
    let credential_id = uuid(&credential["id"]);

    let ok = h.authenticate(&secret).await;
    conforms("AuthenticateResponse", &ok.body);
    let wrong = format!(
        "{}{}",
        &secret[..secret.len() - 1],
        if secret.ends_with('0') { "1" } else { "0" }
    );
    let rejected = h.authenticate(&wrong).await;
    conforms("AuthenticateResponse", &rejected.body);
    conforms(
        "AuthenticateResponse",
        &h.authenticate("garbage").await.body,
    );
    conforms("AuthenticateRequest", &json!({ "credential": secret }));

    let delegated = c.delegated("PRODUCTION");
    let ctx = h
        .as_calling(
            Method::GET,
            "/internal/v1/identity/context",
            None,
            Some(delegated.clone()),
        )
        .await;
    conforms("TrustedContext", &ctx.body);
    let status = h
        .as_calling(
            Method::GET,
            &format!("/internal/v1/credentials/{credential_id}"),
            None,
            Some(delegated.clone()),
        )
        .await;
    conforms("CredentialStatusResult", &status.body);
    let evaluate = json!({ "credentialId": credential_id, "requiredScopes": ["numbers.manage"] });
    conforms("EvaluateRequest", &evaluate);
    let decision = h
        .as_calling(
            Method::POST,
            "/internal/v1/authorization/evaluate",
            Some(evaluate),
            Some(delegated.clone()),
        )
        .await;
    conforms("EvaluateResponse", &decision.body);
    assert_eq!(decision.body["reason"], "MISSING_SCOPES");

    conforms(
        "ScopeRegistry",
        &h.as_calling(Method::GET, "/internal/v1/scopes", None, None)
            .await
            .body,
    );

    let registered = h
        .admin_ok(
            Method::POST,
            &format!(
                "/internal/v1/admin/environments/{}/principals",
                c.env("PRODUCTION")
            ),
            Some(json!({ "kind": "NATIVE_CF_USER", "externalReference": "user-1" })),
        )
        .await;
    let principal_id = uuid(&registered["principal"]["id"]);
    let resolve = json!({ "references": [
        { "principalId": principal_id },
        { "kind": "NATIVE_CF_USER", "externalReference": "nobody" },
        { "principalId": principal_id, "kind": "NATIVE_CF_USER" }
    ] });
    let resolved = h
        .as_calling(
            Method::POST,
            "/internal/v1/principals/resolve",
            Some(resolve),
            Some(delegated.clone()),
        )
        .await;
    conforms("ResolveResponse", &resolved.body);
    let endpoint = h
        .as_calling(
            Method::POST,
            "/internal/v1/principals/telephone-endpoints",
            Some(json!({ "contactReference": uuid::Uuid::new_v4() })),
            Some(delegated),
        )
        .await;
    conforms("PrincipalResult", &endpoint.body);
    let tomb = json!({ "operator": "ops.alice" });
    conforms("TombstoneRequest", &tomb);
    let tombstoned = h
        .as_calling(
            Method::POST,
            &format!("/internal/v1/principals/{principal_id}/tombstone"),
            Some(tomb),
            None,
        )
        .await;
    conforms("TombstoneResult", &tombstoned.body);

    // The change feed: every entity kind the scenario touched, each snapshot typed.
    let page = h
        .as_calling(
            Method::POST,
            "/internal/v1/identity/changes",
            Some(json!({ "after": 0, "limit": 500 })),
            None,
        )
        .await;
    conforms("ChangePage", &page.body);
    let kinds: BTreeSet<&str> = page.body["changes"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| c["entityKind"].as_str().unwrap())
        .collect();
    for kind in [
        "TENANT",
        "APPLICATION",
        "ENVIRONMENT",
        "SERVICE_ACCOUNT",
        "APPLICATION_SCOPE",
        "CREDENTIAL",
        "CREDENTIAL_SCOPE",
        "PRINCIPAL",
    ] {
        assert!(kinds.contains(kind), "{kind} missing from the feed");
    }

    // Errors: the envelope.
    for reply in [
        h.as_calling(Method::GET, "/internal/v1/admin/tenants/x", None, None)
            .await,
        h.raw(
            axum::http::Request::get("/internal/v1/scopes")
                .body(axum::body::Body::empty())
                .unwrap(),
        )
        .await,
        h.as_calling(
            Method::GET,
            &format!("/internal/v1/identity/imports/{}", "0".repeat(64)),
            None,
            None,
        )
        .await,
    ] {
        assert!(reply.status.is_client_error(), "{}", reply.body);
        conforms("Error", &reply.body);
    }
}
