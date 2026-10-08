//! Privacy Relay conforms to its canonical contracts in `packages/contracts` (the
//! authority), in both directions:
//!
//! * the internal OpenAPI lists exactly the routes the service registers;
//! * real responses (contexts, sessions, mutation results, lists, evidence, decisions,
//!   errors) validate against the context / session / authorization / common schemas;
//! * every outbox row is a valid `cfmeet.platform.event-envelope` whose payload is a
//!   valid `cfmeet.privacy-relay.event`, and every enforcement command delivered to
//!   Calling is a valid `cfmeet.privacy-relay.enforcement-command`;
//! * the Rust vocabularies and error codes equal the contract enumerations.

mod common;

use std::{collections::BTreeSet, path::PathBuf};

use cf_service_auth::ApplicationEnvironmentKind as Kind;
use common::*;
use jsonschema::{Registry, Resource, Validator};
use privacy_relay::{error::ErrorCode, vocabulary::*};
use sea_orm::{ConnectionTrait, Statement};
use serde_json::{json, Value};

const BASE: &str = "https://schemas.cfmeet.com/";

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

fn validator(reference: &str) -> Validator {
    let registry = registry();
    jsonschema::options()
        .with_registry(&registry)
        .should_validate_formats(true)
        .build(&json!({ "$schema": "https://json-schema.org/draft/2020-12/schema", "$ref": format!("{BASE}{reference}") }))
        .unwrap()
}

fn assert_valid(v: &Validator, value: &Value, what: &str) {
    let errors: Vec<String> = v
        .iter_errors(value)
        .map(|e| format!("{e} at {}", e.instance_path()))
        .collect();
    assert!(
        errors.is_empty(),
        "{what} violates the contract: {errors:?}\n{value:#}"
    );
}

fn common_enum(name: &str) -> Vec<String> {
    read("privacy-relay/common/v1.schema.json")["$defs"][name]["enum"]
        .as_array()
        .unwrap_or_else(|| panic!("{name}"))
        .iter()
        .map(|v| v.as_str().unwrap().to_string())
        .collect()
}

fn names<T: Copy>(all: &[T], f: impl Fn(T) -> &'static str) -> Vec<String> {
    all.iter().map(|v| f(*v).to_string()).collect()
}

#[test]
fn vocabularies_and_error_codes_equal_the_contract() {
    assert_eq!(
        names(CommunicationContextStatus::ALL, |v| v.as_str()),
        common_enum("CommunicationContextStatus")
    );
    assert_eq!(
        names(PrivacyRelaySessionStatus::ALL, |v| v.as_str()),
        common_enum("PrivacyRelaySessionStatus")
    );
    assert_eq!(
        names(CommunicationChannel::ALL, |v| v.as_str()),
        common_enum("CommunicationChannel")
    );
    assert_eq!(
        names(DirectionPolicy::ALL, |v| v.as_str()),
        common_enum("DirectionPolicy")
    );
    assert_eq!(
        names(ParticipantSubjectKind::ALL, |v| v.as_str()),
        common_enum("ParticipantSubjectKind")
    );
    assert_eq!(
        names(RelayStrategy::ALL, |v| v.as_str()),
        common_enum("RelayStrategy")
    );
    assert_eq!(
        names(CommunicationRoute::ALL, |v| v.as_str()),
        common_enum("CommunicationRoute")
    );
    assert_eq!(
        names(AuthorizationPurpose::ALL, |v| v.as_str()),
        common_enum("AuthorizationPurpose")
    );
    assert_eq!(
        names(AuthorizationDecision::ALL, |v| v.as_str()),
        common_enum("AuthorizationDecision")
    );
    assert_eq!(
        names(AuthorizationReason::ALL, |v| v.as_str()),
        common_enum("AuthorizationReason")
    );
    assert_eq!(
        names(EnforcementState::ALL, |v| v.as_str()),
        common_enum("EnforcementState")
    );
    assert_eq!(
        names(EnforcementReason::ALL, |v| v.as_str()),
        common_enum("EnforcementReason")
    );
    assert_eq!(
        names(AuditEventType::ALL, |v| v.as_str()),
        common_enum("AuditEventType")
    );
    assert_eq!(
        names(SnapshotScope::ALL, |v| v.as_str()),
        common_enum("SnapshotScope")
    );
    // Every service error code is a contract code; the contract adds only the ingress's.
    let contract: BTreeSet<String> = common_enum("ErrorCode").into_iter().collect();
    let service: BTreeSet<String> = names(ErrorCode::ALL, |c| c.as_str()).into_iter().collect();
    let extra: Vec<_> = contract.difference(&service).cloned().collect();
    assert!(
        service.is_subset(&contract),
        "{:?}",
        service.difference(&contract).collect::<Vec<_>>()
    );
    assert_eq!(
        extra,
        [
            "AUTHENTICATION_FAILED",
            "INSUFFICIENT_SCOPE",
            "RATE_LIMITED"
        ]
    );
    // Envelope event types.
    let envelope: Vec<String> = read("privacy-relay/events/v1.schema.json")["$defs"]
        ["EnvelopeEventType"]["enum"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap().to_string())
        .collect();
    let mut ours: Vec<String> = AuditEventType::ALL
        .iter()
        .map(|e| e.envelope_type().to_string())
        .collect();
    ours.sort();
    assert_eq!(envelope, ours);
}

#[test]
fn internal_contract_paths_equal_the_registered_routes() {
    let code = std::fs::read_to_string(
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src/internal/mod.rs"),
    )
    .unwrap();
    let mut registered = BTreeSet::from(["/health".to_string()]);
    for (i, _) in code.match_indices(".route(") {
        let literal = code[i + ".route(".len()..]
            .trim_start()
            .strip_prefix('"')
            .unwrap()
            .split('"')
            .next()
            .unwrap();
        let templated: Vec<String> = literal
            .split('/')
            .map(|s| {
                s.strip_prefix(':')
                    .map(|n| format!("{{{n}}}"))
                    .unwrap_or_else(|| s.to_string())
            })
            .collect();
        registered.insert(format!("/internal/v1{}", templated.join("/")));
    }
    let documented: BTreeSet<String> = read("privacy-relay/internal/v1.openapi.json")["paths"]
        .as_object()
        .unwrap()
        .keys()
        .cloned()
        .collect();
    assert_eq!(registered, documented);
    // Manifest versions equal the documents'.
    let manifest = read("manifest.json");
    for (id, path) in [
        (
            "cfmeet.privacy-relay.internal-api",
            "privacy-relay/internal/v1.openapi.json",
        ),
        (
            "cfmeet.privacy-relay.http-api",
            "privacy-relay/http/v1.openapi.json",
        ),
    ] {
        let entry = manifest["contracts"]
            .as_array()
            .unwrap()
            .iter()
            .find(|c| c["id"] == id)
            .unwrap();
        assert_eq!(entry["path"], path);
        assert_eq!(read(path)["info"]["version"], entry["version"]);
    }
}

#[tokio::test]
async fn real_responses_events_and_commands_validate() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let c = Consumer::new(Kind::Production);
    let [a, b, cc] = principals(3).try_into().unwrap();
    let ctx_v = validator("privacy-relay/context/v1.schema.json#/$defs/CommunicationContext");
    let mutation_v = validator("privacy-relay/context/v1.schema.json#/$defs/ContextMutationResult");
    let list_v = validator("privacy-relay/context/v1.schema.json#/$defs/ContextList");
    let evidence_v = validator("privacy-relay/context/v1.schema.json#/$defs/ContextEvidence");
    let session_mutation_v =
        validator("privacy-relay/session/v1.schema.json#/$defs/SessionMutationResult");
    let session_v = validator("privacy-relay/session/v1.schema.json#/$defs/PrivacyRelaySession");
    let session_list_v = validator("privacy-relay/session/v1.schema.json#/$defs/SessionList");
    let decision_v =
        validator("privacy-relay/authorization/v1.schema.json#/$defs/AuthorizationDecisionResult");
    let request_v =
        validator("privacy-relay/authorization/v1.schema.json#/$defs/AuthorizationRequest");
    let create_v = validator("privacy-relay/context/v1.schema.json#/$defs/CreateContextRequest");
    let error_v = validator("privacy-relay/common/v1.schema.json#/$defs/ErrorEnvelope");
    let envelope_v = validator("platform/events/envelope/v1.schema.json");
    let payload_v = validator("privacy-relay/events/v1.schema.json");
    let command_v =
        validator("privacy-relay/internal/enforcement/v1.schema.json#/$defs/EnforcementCommand");

    let mut req = context_request(&[a, b], "EXPLICIT", &[(a, b), (b, a)]);
    req["graceExpiresAt"] = json!(iso(at(70)));
    assert_valid(&create_v, &req, "create request");
    let created = h.command("/internal/v1/contexts", &c, req).await;
    assert_valid(&mutation_v, &created.json, "create result");
    let id = created.json["context"]["contextId"]
        .as_str()
        .unwrap()
        .to_string();
    let session = h
        .command(
            &format!("/internal/v1/contexts/{id}/sessions"),
            &c,
            json!({ "relayStrategy": "APP_ONLY" }),
        )
        .await;
    assert_valid(&session_mutation_v, &session.json, "session result");
    let sid = session.json["session"]["relaySessionId"]
        .as_str()
        .unwrap()
        .to_string();
    let ask = json!({ "relaySessionId": sid, "source": { "principalId": a }, "targets": [{ "principalId": b }], "channel": "VOICE", "route": "APPLICATION", "purpose": "CALL_CREATE" });
    assert_valid(&request_v, &ask, "authorization request");
    let ok = h
        .post("/internal/v1/communication/authorize", &c, ask)
        .await;
    assert_valid(&decision_v, &ok.json, "authorized decision");
    let denied = h.authorize(&c, &sid, a, &[cc]).await;
    assert_valid(&decision_v, &denied.json, "denied decision");
    let updated = h.command(&format!("/internal/v1/contexts/{id}/update"), &c, json!({ "expectedVersion": 1, "participants": [{ "principalId": a }, { "principalId": cc }], "directions": [{ "from": { "principalId": a }, "to": { "principalId": cc } }] })).await;
    assert_valid(&mutation_v, &updated.json, "update result");
    let suspended = h
        .command(
            &format!("/internal/v1/sessions/{sid}/suspend"),
            &c,
            json!({ "expectedVersion": 2 }),
        )
        .await;
    assert_valid(&session_mutation_v, &suspended.json, "session suspend");
    for (action, version) in [("complete", 2), ("revoke", 3)] {
        let r = h.lifecycle(&c, &id, action, version).await;
        assert_valid(&mutation_v, &r.json, action);
    }
    assert_valid(
        &ctx_v,
        &h.get(&format!("/internal/v1/contexts/{id}"), &c).await.json,
        "context",
    );
    assert_valid(
        &session_v,
        &h.get(&format!("/internal/v1/sessions/{sid}"), &c)
            .await
            .json,
        "session",
    );
    assert_valid(
        &list_v,
        &h.post("/internal/v1/contexts/query", &c, json!({ "limit": 1 }))
            .await
            .json,
        "context list",
    );
    assert_valid(
        &session_list_v,
        &h.post("/internal/v1/sessions/query", &c, json!({}))
            .await
            .json,
        "session list",
    );
    assert_valid(
        &evidence_v,
        &h.get(&format!("/internal/v1/contexts/{id}/evidence"), &c)
            .await
            .json,
        "evidence",
    );
    for r in [
        h.get(
            &format!("/internal/v1/contexts/{}", uuid::Uuid::new_v4()),
            &c,
        )
        .await,
        h.lifecycle(&c, &id, "resume", 1).await,
        h.command(
            "/internal/v1/contexts",
            &c,
            json!({ "tenantId": c.tenant_id }),
        )
        .await,
    ] {
        assert_valid(&error_v, &r.json, "error");
    }
    // Expiry path and enforcement commands too.
    let other = h.bidirectional(&c, &[a, b]).await;
    h.app_session(&c, other["contextId"].as_str().unwrap())
        .await;
    h.clock.set(at(200));
    h.relay.run_sweep().await.unwrap();

    let rows = h
        .conn()
        .query_all(Statement::from_string(
            sea_orm::DbBackend::Postgres,
            "SELECT envelope FROM privacy_relay_outbox ORDER BY created_at".to_string(),
        ))
        .await
        .unwrap();
    assert!(rows.len() >= 15);
    for row in rows {
        let envelope: Value = row.try_get_by_index(0).unwrap();
        assert_valid(&envelope_v, &envelope, "outbox envelope");
        assert_valid(&payload_v, &envelope["payload"], "event payload");
    }
    let delivered = h.calling.delivered();
    assert!(delivered.len() >= 3);
    for d in delivered {
        assert_valid(&command_v, &d.payload, "enforcement command");
    }
}

/// The Contact Vault routes Privacy Relay serves itself are exactly the public paths
/// marked `x-cfmeet-served-by: cfmeet.privacy-relay`, and their real responses validate.
#[tokio::test]
async fn contact_vault_routes_and_responses_equal_the_contract() {
    let code = std::fs::read_to_string(
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src/contact_vault/http.rs"),
    )
    .unwrap();
    let mut registered = BTreeSet::new();
    for (i, _) in code.match_indices(".route(") {
        let literal = code[i + ".route(".len()..]
            .trim_start()
            .strip_prefix('"')
            .unwrap()
            .split('"')
            .next()
            .unwrap();
        let templated: Vec<String> = literal
            .split('/')
            .map(|s| {
                s.strip_prefix(':')
                    .map(|n| format!("{{{n}}}"))
                    .unwrap_or_else(|| s.to_string())
            })
            .collect();
        registered.insert(templated.join("/"));
    }
    let http = read("privacy-relay/http/v1.openapi.json");
    let mut served_here = BTreeSet::new();
    for (path, item) in http["paths"].as_object().unwrap() {
        for (method, op) in item.as_object().unwrap() {
            if op["x-cfmeet-served-by"] == "cfmeet.privacy-relay" {
                served_here.insert(path.clone());
                assert!(
                    op["security"][0]["contactDelegation"].is_array(),
                    "{method} {path}"
                );
                assert_eq!(
                    op["x-cfmeet-required-scope"], op["security"][0]["contactDelegation"][0],
                    "{method} {path}"
                );
            } else {
                assert!(
                    op["security"][0]["contactDelegation"].is_null(),
                    "{method} {path}"
                );
            }
        }
    }
    assert_eq!(registered, served_here);
    // `telephoneNumber` exists only in the Contact Vault raw-input bodies.
    for c in read("manifest.json")["contracts"].as_array().unwrap() {
        let path = c["path"].as_str().unwrap();
        let text = std::fs::read_to_string(contracts().join(path)).unwrap();
        if path != "privacy-relay/contact/v1.schema.json" && !path.ends_with("openapi.json") {
            assert!(!text.contains("\"telephoneNumber\""), "{path}");
        }
    }

    let Some(h) = Harness::new().await else {
        return;
    };
    let c = Consumer::new(Kind::Production);
    let result_v = validator("privacy-relay/contact/v1.schema.json#/$defs/ContactResult");
    let create_v = validator("privacy-relay/contact/v1.schema.json#/$defs/CreateContactRequest");
    let update_v = validator("privacy-relay/contact/v1.schema.json#/$defs/UpdateContactRequest");
    let error_v = validator("privacy-relay/common/v1.schema.json#/$defs/ErrorEnvelope");
    let token = h.writer(&c);
    let body = json!({ "telephoneNumber": "+39 02 1234 5678" });
    assert_valid(&create_v, &body, "create request");
    let created = h
        .contact_call(
            axum::http::Method::POST,
            "/v1/privacy-relay/contacts",
            Some(&token),
            Some(body),
        )
        .await;
    assert_valid(&result_v, &created.json, "create result");
    let id = created.json["contact"]["contactReference"]
        .as_str()
        .unwrap()
        .to_string();
    let update = json!({ "expectedVersion": 1, "telephoneNumber": "+44 20 7946 0958" });
    assert_valid(&update_v, &update, "update request");
    let updated = h
        .contact_call(
            axum::http::Method::PATCH,
            &format!("/v1/privacy-relay/contacts/{id}"),
            Some(&token),
            Some(update),
        )
        .await;
    assert_valid(&result_v, &updated.json, "update result");
    let disabled = h
        .contact_call(
            axum::http::Method::POST,
            &format!("/v1/privacy-relay/contacts/{id}/disable"),
            Some(&token),
            Some(json!({ "expectedVersion": 2 })),
        )
        .await;
    assert_valid(&result_v, &disabled.json, "disable result");
    for r in [
        h.contact_call(
            axum::http::Method::POST,
            "/v1/privacy-relay/contacts",
            None,
            Some(json!({})),
        )
        .await,
        h.contact_call(
            axum::http::Method::GET,
            &format!("/v1/privacy-relay/contacts/{}", uuid::Uuid::new_v4()),
            Some(&token),
            None,
        )
        .await,
        h.contact_call(
            axum::http::Method::POST,
            "/v1/privacy-relay/contacts",
            Some(&token),
            Some(json!({ "telephoneNumber": "+1 123 456 7890" })),
        )
        .await,
    ] {
        assert_valid(&error_v, &r.json, "error envelope");
    }
}
