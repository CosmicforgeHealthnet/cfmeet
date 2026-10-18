//! The Rust implementation conforms to the canonical contracts in
//! `packages/contracts` (the authority) — in both directions:
//! * what this crate serializes validates against the JSON Schemas;
//! * every contract example is accepted by the Rust types and every counter-example is
//!   rejected by them;
//! * enumerations, header names and limits are identical.

mod common;

use std::{fs, path::PathBuf};

use cf_service_auth::{
    claims::{ServiceTokenClaims, MAX_CLOCK_SKEW_SECONDS, MAX_TTL_SECONDS},
    headers, ApplicationEnvironmentKind, DelegatedPlatformContext, DelegatedPrincipalKind,
    InternalPermission, OutboundServiceRequest, RuntimeEnvironment, ServiceId, TrustPolicy,
};
use common::*;
use jsonschema::{Registry, Resource, Validator};
use serde_json::Value;
use uuid::Uuid;

fn contracts() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../contracts")
}

fn read(rel: &str) -> Value {
    serde_json::from_str(&fs::read_to_string(contracts().join(rel)).unwrap()).unwrap()
}

fn fixtures(dir: &str, kind: &str) -> Vec<(String, Value)> {
    let path = contracts().join(dir).join(kind);
    let mut out: Vec<(String, Value)> = fs::read_dir(&path)
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| p.extension().is_some_and(|e| e == "json"))
        .map(|p| {
            (
                p.file_name().unwrap().to_string_lossy().to_string(),
                serde_json::from_str(&fs::read_to_string(&p).unwrap()).unwrap(),
            )
        })
        .collect();
    out.sort_by(|a, b| a.0.cmp(&b.0));
    assert!(!out.is_empty(), "no {kind} in {dir}");
    out
}

/// Registry of every registered JSON Schema contract, keyed by `$id` (offline).
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

fn validator(rel: &str) -> Validator {
    let registry = registry();
    jsonschema::options()
        .with_registry(&registry)
        .should_validate_formats(true)
        .build(&read(rel))
        .unwrap()
}

fn assert_conforms(validator: &Validator, value: &Value, what: &str) {
    let errors: Vec<String> = validator
        .iter_errors(value)
        .map(|e| e.to_string())
        .collect();
    assert!(
        errors.is_empty(),
        "{what} violates its contract: {errors:?}\n{value:#}"
    );
}

const HEADER: &str = "platform/service-auth/token-header/v1.schema.json";
const CLAIMS: &str = "platform/service-auth/token-claims/v1.schema.json";
const CONTEXT: &str = "platform/service-auth/delegated-platform-context/v1.schema.json";
const BUNDLE: &str = "platform/service-auth/trust-bundle/v1.schema.json";

fn delegated(principal: bool) -> DelegatedPlatformContext {
    let c = DelegatedPlatformContext::new(
        Uuid::new_v4(),
        Uuid::new_v4(),
        Uuid::new_v4(),
        ApplicationEnvironmentKind::Production,
        Uuid::new_v4(),
    );
    if principal {
        c.with_initiating_principal(Uuid::new_v4(), DelegatedPrincipalKind::TelephoneEndpoint)
    } else {
        c
    }
}

#[test]
fn signed_tokens_conform_to_the_header_and_claims_contracts() {
    let fleet = Fleet::new(RuntimeEnvironment::Staging);
    let (header_v, claims_v) = (validator(HEADER), validator(CLAIMS));
    for context in [None, Some(delegated(false)), Some(delegated(true))] {
        let mut request = OutboundServiceRequest::new_operation(ServiceId::privacy_relay());
        if let Some(c) = context {
            request = request.with_delegated_context(c);
        }
        let token = token_of(&fleet.calling.sign(&request).unwrap());
        let mut parts = token.split('.');
        assert_conforms(
            &header_v,
            &decode_segment(parts.next().unwrap()),
            "token header",
        );
        assert_conforms(
            &claims_v,
            &decode_segment(parts.next().unwrap()),
            "token claims",
        );
    }
}

#[test]
fn delegated_context_serialization_conforms() {
    let v = validator(CONTEXT);
    for c in [delegated(false), delegated(true)] {
        assert_conforms(&v, &serde_json::to_value(&c).unwrap(), "delegated context");
    }
    for kind in DelegatedPrincipalKind::ALL {
        let c = delegated(false).with_initiating_principal(Uuid::new_v4(), *kind);
        assert_conforms(&v, &serde_json::to_value(&c).unwrap(), "delegated context");
    }
}

#[test]
fn contract_fixtures_agree_with_the_rust_types() {
    let dir = "platform/service-auth/token-claims";
    for (name, example) in fixtures(dir, "examples") {
        let parsed = ServiceTokenClaims::from_json(&example.to_string())
            .unwrap_or_else(|e| panic!("{name}: {e}"));
        assert_eq!(
            serde_json::to_value(&parsed).unwrap(),
            example,
            "{name} round trip"
        );
    }
    for (name, counter) in fixtures(dir, "counter-examples") {
        // Rejected by the Rust type, or (for well-formed but unsupported versions) by the
        // verifier's version rule.
        match ServiceTokenClaims::from_json(&counter.to_string()) {
            Err(_) => {}
            Ok(_) => assert_ne!(counter["ver"], 1, "{name} accepted by the Rust claims type"),
        }
    }

    let dir = "platform/service-auth/delegated-platform-context";
    for (name, example) in fixtures(dir, "examples") {
        let parsed: DelegatedPlatformContext =
            serde_json::from_value(example.clone()).unwrap_or_else(|e| panic!("{name}: {e}"));
        assert_eq!(
            serde_json::to_value(&parsed).unwrap(),
            example,
            "{name} round trip"
        );
    }
    for (name, counter) in fixtures(dir, "counter-examples") {
        assert!(
            serde_json::from_value::<DelegatedPlatformContext>(counter).is_err(),
            "{name} accepted by the Rust type"
        );
    }

    let dir = "platform/service-auth/trust-bundle";
    for (name, example) in fixtures(dir, "examples") {
        let receiver = ServiceId::parse(example["receiver"].as_str().unwrap()).unwrap();
        let env =
            RuntimeEnvironment::parse(example["runtime_environment"].as_str().unwrap()).unwrap();
        TrustPolicy::from_json(&example.to_string(), &receiver, env)
            .unwrap_or_else(|e| panic!("{name}: {e}"));
    }
    for (name, counter) in fixtures(dir, "counter-examples") {
        let receiver = ServiceId::privacy_relay();
        assert!(
            TrustPolicy::from_json(&counter.to_string(), &receiver, RuntimeEnvironment::Staging)
                .is_err(),
            "{name} accepted by the Rust trust policy"
        );
    }
}

#[test]
fn trust_bundles_built_by_rust_tooling_conform() {
    let v = validator(BUNDLE);
    let relay = ServiceId::privacy_relay();
    let b = bundle(
        &relay,
        RuntimeEnvironment::Production,
        &[Grant {
            issuer: &ServiceId::telephony(),
            keys: vec![
                key("telephony-production-1").public_jwk(),
                key("telephony-production-2").public_jwk(),
            ],
            permissions: &["protected-contact.resolve-for-execution"],
        }],
    );
    assert_conforms(&v, &b, "trust bundle");
}

fn enum_of(rel: &str, pointer: &str) -> Vec<String> {
    read(rel)
        .pointer(pointer)
        .and_then(Value::as_array)
        .unwrap_or_else(|| panic!("{rel}{pointer}"))
        .iter()
        .map(|v| v.as_str().unwrap().to_string())
        .collect()
}

#[test]
fn enumerations_are_identical() {
    const COMMON: &str = "platform/common/v1.schema.json";
    let names = |values: Vec<Value>| -> Vec<String> {
        values
            .into_iter()
            .map(|v| v.as_str().unwrap().to_string())
            .collect()
    };
    assert_eq!(
        enum_of(COMMON, "/$defs/InternalPermission/enum"),
        InternalPermission::ALL
            .iter()
            .map(|p| p.as_str().to_string())
            .collect::<Vec<_>>()
    );
    assert_eq!(
        enum_of(COMMON, "/$defs/RuntimeEnvironment/enum"),
        RuntimeEnvironment::ALL
            .iter()
            .map(|e| e.as_str().to_string())
            .collect::<Vec<_>>()
    );
    assert_eq!(
        enum_of(COMMON, "/$defs/ApplicationEnvironmentKind/enum"),
        names(
            ApplicationEnvironmentKind::ALL
                .iter()
                .map(|k| serde_json::to_value(k).unwrap())
                .collect()
        )
    );
    assert_eq!(
        enum_of(COMMON, "/$defs/PrincipalKind/enum"),
        names(
            DelegatedPrincipalKind::ALL
                .iter()
                .map(|k| serde_json::to_value(k).unwrap())
                .collect()
        )
    );
}

#[test]
fn service_identity_rules_are_identical() {
    let common = read("platform/common/v1.schema.json");
    let pattern = common.pointer("/$defs/ServiceId").unwrap().clone();
    let schema = serde_json::json!({ "$schema": "https://json-schema.org/draft/2020-12/schema" , "allOf": [pattern] });
    let v = jsonschema::validator_for(&schema).unwrap();
    for sample in [
        "cfmeet.calling",
        "cfmeet.privacy-relay",
        "cfmeet.telephony",
        "cfmeet.meeting-service",
        "cfmeet.a1",
        "*",
        "cfmeet.*",
        "cfmeet.",
        "calling",
        "cfmeet.Calling",
        "cfmeet.calling-",
        "cfmeet.calling--x",
        "cfmeet.1x",
    ] {
        assert_eq!(
            ServiceId::parse(sample).is_ok(),
            v.is_valid(&Value::String(sample.to_string())),
            "{sample}"
        );
    }
}

#[test]
fn transport_conventions_and_limits_match_the_contracts() {
    let http = read("platform/internal-http/v1.openapi.json");
    let scheme = http
        .pointer("/components/securitySchemes/cfServiceAuth")
        .unwrap();
    assert_eq!(scheme["in"], "header");
    assert!(scheme["name"]
        .as_str()
        .unwrap()
        .eq_ignore_ascii_case(headers::SERVICE_AUTHORIZATION.as_str()));
    let param = |n: &str| {
        http.pointer(&format!("/components/parameters/{n}/name"))
            .unwrap()
            .as_str()
            .unwrap()
            .to_ascii_lowercase()
    };
    assert_eq!(param("CfRequestId"), headers::REQUEST_ID.as_str());
    assert_eq!(param("CfCorrelationId"), headers::CORRELATION_ID.as_str());

    let claims = read(CLAIMS);
    let description = claims["description"].as_str().unwrap();
    assert!(description.contains(&format!("{MAX_TTL_SECONDS} seconds")));
    assert!(description.contains(&format!("{MAX_CLOCK_SKEW_SECONDS} seconds")));
    let header = read(HEADER);
    assert_eq!(
        header.pointer("/properties/alg/const").unwrap(),
        cf_service_auth::claims::ALGORITHM
    );
    assert_eq!(
        header.pointer("/properties/typ/const").unwrap(),
        cf_service_auth::claims::TOKEN_TYPE
    );
}

const GRANT: &str = "numbering/routing-grant/v1.schema.json";

#[test]
fn number_routing_grants_conform_both_ways() {
    use cf_service_auth::{
        number_grant::{
            GrantScope, GrantTarget, NumberGrantClaims, NumberGrantKind, NumberGrantRequest,
            NumberGrantSigner, NumberGrantTrust, NUMBER_GRANT_TOKEN_TYPE,
        },
        KeyId, ServiceSigningKey, SystemClock,
    };
    let v = validator(GRANT);
    let signer = NumberGrantSigner::new(
        ServiceId::numbering(),
        RuntimeEnvironment::Staging,
        ServiceSigningKey::generate(KeyId::parse("numbering-grant-staging-1").unwrap()),
        60,
        std::sync::Arc::new(SystemClock),
    )
    .unwrap();
    let scope = GrantScope {
        tenant_id: Uuid::new_v4(),
        application_id: Uuid::new_v4(),
        environment_id: Uuid::new_v4(),
    };
    for kind in NumberGrantKind::ALL {
        let request = NumberGrantRequest {
            kind,
            audience: ServiceId::telephony(),
            number_resource_id: Uuid::new_v4(),
            assignment_id: Uuid::new_v4(),
            routing_revision: 2,
            scope,
            principal_id: None,
            target: (!kind.is_outbound()).then(|| GrantTarget {
                kind: "ENTERPRISE_QUEUE".into(),
                target_ref: Uuid::new_v4(),
            }),
            operation_ref: kind.is_outbound().then(|| "call-0123456789".to_string()),
        };
        let grant = signer.sign(&request).unwrap();
        let mut parts = grant.expose_for_delivery().split('.');
        let header = decode_segment(parts.next().unwrap());
        assert_eq!(header["typ"], NUMBER_GRANT_TOKEN_TYPE);
        assert_conforms(&v, &decode_segment(parts.next().unwrap()), "grant claims");
    }
    // Every contract example is accepted by the Rust types, every counter-example of the
    // claims / trust bundle refused.
    for (name, value) in fixtures("numbering/routing-grant", "examples") {
        if name.starts_with("claims") {
            NumberGrantClaims::from_json(&value.to_string())
                .unwrap_or_else(|e| panic!("{name}: {e}"));
        }
        if name.starts_with("trust-bundle") {
            NumberGrantTrust::from_json(
                &value.to_string(),
                &ServiceId::telephony(),
                RuntimeEnvironment::Staging,
            )
            .unwrap_or_else(|e| panic!("{name}: {e}"));
        }
    }
    for (name, value) in fixtures("numbering/routing-grant", "counter-examples") {
        if name.starts_with("claims-with") {
            assert!(
                NumberGrantClaims::from_json(&value.to_string()).is_err(),
                "{name}"
            );
        }
        if name.starts_with("trust-bundle") {
            assert!(
                NumberGrantTrust::from_json(
                    &value.to_string(),
                    &ServiceId::telephony(),
                    RuntimeEnvironment::Staging,
                )
                .is_err(),
                "{name}"
            );
        }
    }
}
