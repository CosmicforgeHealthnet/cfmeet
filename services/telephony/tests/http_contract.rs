//! Telephony's contracts are the implementation (Part E §181–§183): the internal OpenAPI
//! lists exactly the registered routes, and every real command, answer, error, usage
//! record and readiness body validates against `packages/contracts/telephony/`.

mod common;

use std::{collections::BTreeSet, path::PathBuf};

use axum::http::{Method, StatusCode};
use common::Harness;
use jsonschema::{Registry, Resource, Validator};
use serde_json::{json, Value};
use telephony::domain::NormalizedEvent;
use uuid::Uuid;

const B: &str = "https://schemas.cfmeet.com/";
const INTERNAL: &str = "telephony/internal/v1.openapi.json";

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
        let doc = read(c["path"].as_str().unwrap());
        let id = if c["kind"] == "json-schema" {
            doc["$id"].as_str().unwrap().to_string()
        } else {
            format!("{B}{}", c["path"].as_str().unwrap())
        };
        builder = builder.add(id, Resource::from_contents(doc)).unwrap();
    }
    builder.prepare().unwrap()
}

fn validator(reference: &str) -> Validator {
    let registry = registry();
    jsonschema::options()
        .with_registry(&registry)
        .should_validate_formats(true)
        .build(&json!({
            "$schema": "https://json-schema.org/draft/2020-12/schema",
            "$ref": format!("{B}{reference}"),
        }))
        .unwrap()
}

fn check(reference: &str, value: &Value) {
    let v = validator(reference);
    let errors: Vec<String> = v.iter_errors(value).map(|e| e.to_string()).collect();
    assert!(errors.is_empty(), "{reference}: {errors:?}\n{value:#}");
}

fn rust_files(dir: &std::path::Path, out: &mut Vec<PathBuf>) {
    for e in std::fs::read_dir(dir).unwrap() {
        let p = e.unwrap().path();
        if p.is_dir() {
            rust_files(&p, out);
        } else if p.extension().is_some_and(|x| x == "rs") {
            out.push(p);
        }
    }
}

#[test]
fn the_internal_openapi_lists_exactly_the_registered_routes() {
    let mut files = Vec::new();
    rust_files(
        &PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src"),
        &mut files,
    );
    let mut registered = BTreeSet::new();
    for file in files {
        let path = file.to_string_lossy().replace('\\', "/");
        // The metrics listener is a separate operational surface (own address).
        if path.ends_with("src/metrics.rs") {
            continue;
        }
        let code = std::fs::read_to_string(&file).unwrap();
        let prefix = if path.ends_with("src/internal/mod.rs") {
            "/internal/v1"
        } else {
            ""
        };
        for (i, _) in code.match_indices(".route(") {
            let rest = code[i + ".route(".len()..].trim_start();
            let literal = rest
                .strip_prefix('"')
                .and_then(|r| r.split('"').next())
                .unwrap_or_else(|| panic!("{}: non-literal route path", file.display()));
            let templated: Vec<String> = literal
                .split('/')
                .map(|seg| match seg.strip_prefix(':') {
                    Some(name) => format!("{{{name}}}"),
                    None => seg.to_string(),
                })
                .collect();
            registered.insert(format!("{prefix}{}", templated.join("/")));
        }
    }
    let documented: BTreeSet<String> = read(INTERNAL)["paths"]
        .as_object()
        .unwrap()
        .keys()
        .cloned()
        .collect();
    assert_eq!(
        registered, documented,
        "Telephony routes and its OpenAPI contract disagree"
    );
    // The metrics listener is never part of the service router.
    let routes =
        std::fs::read_to_string(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src/routes.rs"))
            .unwrap();
    assert!(!routes.contains("metrics"));
}

#[tokio::test]
async fn real_commands_answers_and_errors_follow_the_contracts() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let call = Uuid::new_v4();
    let mut legs = Vec::new();
    for _ in 0..2 {
        let contact = h.privacy.add_contact("+2348031234567", "NG");
        let mut body = h.originate_body(contact, Uuid::new_v4(), 1);
        body["call"]["callId"] = json!(call);
        check(
            "telephony/commands/v1.schema.json#/$defs/OriginateCommand",
            &body,
        );
        let r = h
            .calling(
                Method::POST,
                "/internal/v1/legs/originate",
                Some(body.clone()),
            )
            .await;
        assert_eq!(r.status, StatusCode::ACCEPTED);
        check(
            "telephony/commands/v1.schema.json#/$defs/LegAccepted",
            &r.json,
        );
        let replay = h
            .calling(Method::POST, "/internal/v1/legs/originate", Some(body))
            .await;
        assert_eq!(replay.status, StatusCode::OK);
        check(
            "telephony/commands/v1.schema.json#/$defs/LegAccepted",
            &replay.json,
        );
        legs.push(
            r.json["leg"]["telephonyLegId"]
                .as_str()
                .unwrap()
                .to_string(),
        );
    }
    h.run().await;
    let correlation = h.correlation_of(legs[0].parse().unwrap()).await;
    for event in [NormalizedEvent::LegRinging, NormalizedEvent::LegAnswered] {
        let e = h.provider_a.progress(&correlation, event, h.now());
        h.svc.ingest_provider_event("dp-a", e).await.unwrap();
    }
    let view = h
        .calling(Method::GET, &format!("/internal/v1/legs/{}", legs[0]), None)
        .await;
    check(
        "telephony/commands/v1.schema.json#/$defs/LegRead",
        &view.json,
    );
    let availability = h
        .calling(
            Method::GET,
            "/internal/v1/routes/availability?country=NG&capability=VOICE_OUTBOUND",
            None,
        )
        .await;
    check("telephony/routes/v1.schema.json", &availability.json);
    let bridge_body = json!({
        "schemaVersion": "1", "commandId": Uuid::new_v4(), "idempotencyKey": "bridge-contract-1",
        "correlationId": cf_service_auth::CorrelationId::generate().as_str(),
        "callId": call, "kind": "PUBLIC_LEGS", "telephonyLegIds": legs,
    });
    check(
        "telephony/commands/v1.schema.json#/$defs/BridgeCommand",
        &bridge_body,
    );
    let bridge = h
        .calling(Method::POST, "/internal/v1/bridges", Some(bridge_body))
        .await;
    check(
        "telephony/commands/v1.schema.json#/$defs/BridgeAccepted",
        &bridge.json,
    );
    let bridge_id = bridge.json["bridge"]["bridgeId"]
        .as_str()
        .unwrap()
        .to_string();
    let viewed = h
        .calling(
            Method::GET,
            &format!("/internal/v1/bridges/{bridge_id}"),
            None,
        )
        .await;
    check(
        "telephony/commands/v1.schema.json#/$defs/BridgeAccepted",
        &viewed.json,
    );
    let hold = json!({ "schemaVersion": "1", "commandId": Uuid::new_v4(), "idempotencyKey": "hold-contract-1",
                       "correlationId": cf_service_auth::CorrelationId::generate().as_str() });
    check(
        "telephony/commands/v1.schema.json#/$defs/ControlCommand",
        &hold,
    );
    let r = h
        .calling(
            Method::POST,
            &format!("/internal/v1/legs/{}/hold", legs[0]),
            Some(hold),
        )
        .await;
    assert!(r.status.is_success(), "{}", r.text);
    check(
        "telephony/commands/v1.schema.json#/$defs/LegAccepted",
        &r.json,
    );
    let ended = h
        .calling(
            Method::DELETE,
            &format!("/internal/v1/bridges/{bridge_id}"),
            None,
        )
        .await;
    check(
        "telephony/commands/v1.schema.json#/$defs/BridgeAccepted",
        &ended.json,
    );
    let term = json!({ "schemaVersion": "1", "commandId": Uuid::new_v4(), "idempotencyKey": "term-contract-1",
                       "correlationId": cf_service_auth::CorrelationId::generate().as_str(), "reason": "CALL_ENDED" });
    let r = h
        .calling(
            Method::POST,
            &format!("/internal/v1/legs/{}/terminate", legs[0]),
            Some(term),
        )
        .await;
    check(
        "telephony/commands/v1.schema.json#/$defs/LegAccepted",
        &r.json,
    );
    // Errors: not found, conflict, validation, unknown member.
    for (method, path, body) in [
        (
            Method::GET,
            format!("/internal/v1/legs/{}", Uuid::new_v4()),
            None,
        ),
        (
            Method::GET,
            "/internal/v1/routes/availability?country=ng&capability=VOICE_OUTBOUND".to_string(),
            None,
        ),
        (
            Method::POST,
            format!("/internal/v1/legs/{}/terminate", legs[1]),
            Some(
                json!({ "schemaVersion": "1", "commandId": Uuid::new_v4(), "idempotencyKey": "x",
                         "correlationId": "corr_00000000000000000000000000000000", "callerId": "+2342010000001" }),
            ),
        ),
    ] {
        let r = h.calling(method, &path, body).await;
        assert!(r.status.is_client_error(), "{path}: {}", r.text);
        check("telephony/commands/v1.schema.json#/$defs/Error", &r.json);
    }
    // Readiness and health.
    let ready = h.calling(Method::GET, "/internal/v1/readiness", None).await;
    check(
        &format!("{INTERNAL}#/components/schemas/Readiness"),
        &ready.json,
    );
    let health = h
        .raw(
            axum::http::Request::builder()
                .method(Method::GET)
                .uri("/health"),
            None,
        )
        .await;
    check(
        &format!("{INTERNAL}#/components/schemas/HealthStatus"),
        &health.json,
    );
    // The usage record of the ended leg (restricted, internal) follows its contract.
    for _ in 0..3 {
        h.run().await;
    }
    let row = h
        .text(&format!(
            "SELECT json_build_object('telephonyLegId', telephony_leg_id, 'callId', calling_call_id, \
               'callLegId', calling_call_leg_id, 'routeRef', route_id, 'destinationCountry', destination_country, \
               'connectedDurationMs', connected_duration_ms, 'billableSeconds', billable_seconds, \
               'providerCostMinor', provider_cost_minor, 'estimatedCostMinor', estimated_cost_minor, \
               'currency', currency, 'costKnown', cost_known, 'finalizationStatus', finalization_status, \
               'idempotencyKey', idempotency_key, \
               'finalizedAt', to_char(finalized_at AT TIME ZONE 'UTC', 'YYYY-MM-DD\"T\"HH24:MI:SS.US\"Z\"'))::text \
             FROM telephony_usage_records WHERE telephony_leg_id = '{}'",
            legs[0]
        ))
        .await
        .expect("usage finalized");
    check(
        "telephony/usage/v1.schema.json",
        &serde_json::from_str(&row).unwrap(),
    );
    // Every event delivered meanwhile follows the event contract.
    for e in h.calling.events() {
        check("telephony/events/v1.schema.json", &e);
    }
}
