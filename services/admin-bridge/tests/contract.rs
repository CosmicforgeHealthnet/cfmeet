//! Contract tests: drift between the catalog and `docs/contract/`, the HQ interface v1
//! rules HQ relies on, the breaking-change gate and the published test vectors.

use std::{collections::HashSet, fs, path::PathBuf};

use admin_bridge::contract::{
    catalog::catalog, compat::breaking_changes, datasets::DATASETS, events::ADAPTERS, model::*,
    render::render,
};
use admin_bridge::wire;
use base64::{engine::general_purpose::URL_SAFE_NO_PAD as B64, Engine};
use ed25519_dalek::{Signature, Verifier, VerifyingKey};
use serde_json::{json, Value};

fn dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("docs/contract")
}

fn rendered(path: &str) -> Value {
    serde_json::from_slice(&render()[path]).unwrap()
}

#[test]
fn committed_contract_has_no_drift() {
    let files = render();
    for (path, bytes) in &files {
        let disk = fs::read(dir().join(path))
            .unwrap_or_else(|_| panic!("{path} missing: run admin-bridge-contract generate"));
        assert!(
            disk == *bytes,
            "{path} drifted from the catalog: run admin-bridge-contract generate"
        );
    }
}

#[test]
fn rendering_is_deterministic() {
    assert_eq!(render(), render());
}

#[test]
fn sha256sums_cover_every_other_file() {
    let files = render();
    let sums = String::from_utf8(files["SHA256SUMS"].clone()).unwrap();
    let mut listed = HashSet::new();
    for line in sums.lines() {
        let (hash, path) = line.split_once("  ").unwrap();
        assert_eq!(hash, wire::sha256_hex(&files[path]), "{path}");
        listed.insert(path.to_string());
    }
    for path in files.keys().filter(|p| *p != "SHA256SUMS") {
        assert!(listed.contains(path), "{path} not in SHA256SUMS");
    }
}

#[test]
fn ids_routes_and_modules_follow_hq_interface_v1() {
    let caps = catalog();
    let mut ids = HashSet::new();
    let mut routes = HashSet::new();
    for c in &caps {
        assert!(ids.insert(c.id), "duplicate id {}", c.id);
        assert!(
            routes.insert((c.method, c.path)),
            "duplicate route {} {}",
            c.method,
            c.path
        );
        assert!(
            Module::parse(c.id.split('.').next().unwrap()).is_some(),
            "{}: module",
            c.id
        );
        assert!(
            c.path.starts_with('/') && !c.path.contains("//"),
            "{}: path",
            c.id
        );
        assert!(!c.roles.is_empty(), "{}: no role", c.id);
        assert!(!c.purposes.is_empty(), "{}: no purpose", c.id);
        if c.method == Method::Get {
            assert_ne!(c.kind, Kind::Command, "{}: GET command", c.id);
        }
        if c.method == Method::Post && c.kind == Kind::Read {
            assert_eq!(
                c.id, "pricing.charges.simulate",
                "only listed reads may be POST"
            );
        }
        let in_path: Vec<&str> = c
            .path
            .split('/')
            .filter_map(|s| s.strip_prefix('{')?.strip_suffix('}'))
            .collect();
        assert_eq!(
            in_path,
            c.path_params.iter().map(|p| p.name).collect::<Vec<_>>(),
            "{}: path params",
            c.id
        );
    }
}

#[test]
fn risky_commands_need_two_people_versions_and_idempotency() {
    for c in catalog() {
        let risky = c.kind == Kind::Command && c.risk >= Risk::R3;
        assert_eq!(c.approval() == Approval::TwoPerson, risky, "{}", c.id);
        assert_eq!(c.source_version_required(), risky, "{}", c.id);
        if c.kind == Kind::Command {
            assert!(c.idempotency_required(), "{}", c.id);
        }
    }
}

#[test]
fn served_capabilities_are_exactly_the_ones_built() {
    let served: Vec<&str> = catalog()
        .iter()
        .filter(|c| c.served)
        .map(|c| c.id)
        .collect();
    assert_eq!(
        served,
        [
            "bridge.capabilities.read",
            "bridge.health.read",
            "bridge.datasets.read",
            "hq.projections.status",
            "organizations.organizations.search",
            "organizations.organizations.read",
            "organizations.bindings.list",
            "organizations.audit.list",
            "organizations.global-audit.list",
            "organizations.members.list",
            "organizations.teams.list",
            "organizations.policies.read",
            "notifications.deliveries.list",
            "notifications.failed-deliveries.list",
            "notifications.suppressions.list",
            "notifications.routing.read",
            "notifications.providers.read"
        ]
    );
    for c in catalog() {
        if c.served {
            assert_eq!(c.state(), State::ReadOnly, "{}", c.id);
            assert!(c.blocker().is_none() && !c.tests.is_empty(), "{}", c.id);
        } else {
            assert!(
                c.blocker().is_some(),
                "{}: unserved capability needs a blocker",
                c.id
            );
            assert!(
                matches!(c.state(), State::Disabled | State::BlockedDependency),
                "{}",
                c.id
            );
        }
    }
}

#[test]
fn matrix_entries_have_exactly_the_agreed_fields() {
    let fields: Vec<&str> = vec![
        "id",
        "version",
        "owner",
        "method",
        "bridge_endpoint",
        "source_endpoint",
        "state",
        "permission",
        "risk",
        "privacy",
        "scope",
        "source_version_required",
        "idempotency_required",
        "approval",
        "feature_flag",
        "blocker",
        "tests",
    ];
    let matrix = rendered("CFMEET_ADMIN_CAPABILITY_MATRIX.json");
    let entries = matrix.as_array().expect("matrix is an array");
    assert_eq!(entries.len(), catalog().len());
    for e in entries {
        let keys: Vec<&str> = e.as_object().unwrap().keys().map(String::as_str).collect();
        assert_eq!(keys, fields, "{}", e["id"]);
    }
}

#[test]
fn openapi_operations_match_the_matrix() {
    let openapi = rendered("openapi.json");
    let matrix = rendered("CFMEET_ADMIN_CAPABILITY_MATRIX.json");
    let mut operations = 0;
    for (path, item) in openapi["paths"].as_object().unwrap() {
        assert!(path.starts_with(BRIDGE_BASE));
        for (method, op) in item.as_object().unwrap() {
            operations += 1;
            let id = op["operationId"].as_str().unwrap();
            let entry = matrix
                .as_array()
                .unwrap()
                .iter()
                .find(|e| e["id"] == id)
                .expect("operation in matrix");
            assert_eq!(
                entry["method"].as_str().unwrap().to_ascii_lowercase(),
                *method
            );
            assert_eq!(
                format!(
                    "{BRIDGE_BASE}{}",
                    entry["bridge_endpoint"].as_str().unwrap()
                ),
                *path
            );
            let ok = &op["responses"]["200"]["content"]["application/json"]["schema"];
            assert!(
                ok["properties"].get("result").is_some(),
                "{id}: 2xx must declare properties.result"
            );
            for p in op["parameters"]
                .as_array()
                .unwrap()
                .iter()
                .filter(|p| p["in"] == "path")
            {
                let s = &p["schema"];
                assert!(
                    s["format"] == "uuid" || s["pattern"].is_string(),
                    "{id}: path param needs uuid or pattern"
                );
                if let Some(pattern) = s["pattern"].as_str() {
                    assert!(
                        !pattern.contains('/'),
                        "{id}: '/' must never match a path value"
                    );
                }
            }
            let query: Vec<&str> = op["parameters"]
                .as_array()
                .unwrap()
                .iter()
                .filter(|p| p["in"] == "query")
                .map(|p| p["name"].as_str().unwrap())
                .collect();
            let shape = QueryShape::parse(op["x-cfmeet-query-shape"].as_str().unwrap()).unwrap();
            assert_eq!(query, shape.params(), "{id}: query order");
        }
    }
    assert_eq!(operations, catalog().len());
}

#[test]
fn result_refs_resolve_and_unbuilt_results_cannot_succeed() {
    let caps = rendered("capabilities.v1.json");
    let schemas = rendered("bridge-schemas.v1.json");
    for c in caps["capabilities"].as_array().unwrap() {
        let r = &c["result_schema"];
        if let Some(reference) = r["$ref"].as_str() {
            let name = reference.strip_prefix("#/components/schemas/").unwrap();
            assert!(
                schemas["components"]["schemas"].get(name).is_some(),
                "{}: {name}",
                c["id"]
            );
        } else {
            assert_eq!(*r, json!({"not": {}}), "{}", c["id"]);
        }
    }
}

#[test]
fn platform_capabilities_are_as_agreed_with_hq() {
    let caps = catalog();
    let find = |id: &str| {
        caps.iter()
            .find(|c| c.id == id)
            .unwrap_or_else(|| panic!("{id}"))
    };
    for (id, method, path, shape) in [
        (
            "bridge.capabilities.read",
            Method::Get,
            "/capabilities",
            QueryShape::Scoped,
        ),
        (
            "bridge.health.read",
            Method::Get,
            "/health/aggregate",
            QueryShape::Scoped,
        ),
        (
            "bridge.datasets.read",
            Method::Get,
            "/datasets/{dataset_id}",
            QueryShape::ScopedWindow,
        ),
        (
            "hq.events.feed",
            Method::Get,
            "/hq/events",
            QueryShape::ScopedWindow,
        ),
        (
            "hq.events.ack",
            Method::Post,
            "/hq/event-acknowledgements",
            QueryShape::Scoped,
        ),
        (
            "hq.projections.status",
            Method::Get,
            "/hq/projections/status",
            QueryShape::Scoped,
        ),
    ] {
        let c = find(id);
        assert_eq!((c.method, c.path, c.shape), (method, path, shape), "{id}");
    }
    assert_eq!(
        find("bridge.capabilities.read").purposes,
        vec![Purpose::AdminUiCapabilities]
    );
    // The HQ worker's feed identity reaches the event feed and projection status only.
    for c in &caps {
        if c.roles.contains(&Role::SourceSync) {
            assert!(
                matches!(
                    c.id,
                    "hq.events.feed"
                        | "hq.events.ack"
                        | "hq.projections.status"
                        | "bridge.datasets.read"
                ),
                "{}",
                c.id
            );
        }
    }
    assert_eq!(
        find("operations.activation.request").roles,
        vec![Role::ReleaseManager]
    );
}

#[test]
fn datasets_and_event_adapters_are_as_agreed_with_hq() {
    let ids: Vec<&str> = DATASETS.iter().map(|d| d.id).collect();
    assert_eq!(
        ids,
        [
            "cfmeet.engagement_daily",
            "cfmeet.meetings_daily",
            "cfmeet.messaging_daily",
            "cfmeet.partners_daily",
            "cfmeet.apps_daily",
            "cfmeet.verification_queue",
            "cfmeet.trust_safety_queue",
            "cfmeet.operations_health",
        ]
    );
    assert_eq!(ADAPTERS.len(), 13);
    let mut keys = HashSet::new();
    for a in ADAPTERS {
        assert!(keys.insert((a.source, a.export_type)), "duplicate adapter");
    }
    let count = |t: &str| ADAPTERS.iter().filter(|a| a.export_type == t).count();
    assert_eq!(count("abuse.report.changed.v1"), 4);
    assert_eq!(count("incident.changed.v1"), 5);
    assert_eq!(count("pricing.version.changed.v1"), 1);
}

#[test]
fn compat_gate_accepts_additive_and_refuses_breaking_changes() {
    let caps = rendered("capabilities.v1.json");
    let schemas = rendered("bridge-schemas.v1.json");
    assert!(breaking_changes(&caps, &schemas, &caps, &schemas).is_empty());

    let mut removed = caps.clone();
    removed["capabilities"].as_array_mut().unwrap().remove(0);
    assert!(!breaking_changes(&caps, &schemas, &removed, &schemas).is_empty());

    let mut changed = caps.clone();
    changed["capabilities"][0]["permission"] = json!("something.else");
    assert!(!breaking_changes(&caps, &schemas, &changed, &schemas).is_empty());

    // Additive: a state change and a first-time result schema are fine.
    let mut additive = caps.clone();
    let idx = additive["capabilities"]
        .as_array()
        .unwrap()
        .iter()
        .position(|c| c["result_schema"] == json!({"not": {}}))
        .unwrap();
    additive["capabilities"][idx]["matrix_state"] = json!("READ_ONLY");
    additive["capabilities"][idx]["result_schema"] = json!({"$ref": "#/components/schemas/Health"});
    assert!(breaking_changes(&caps, &schemas, &additive, &schemas).is_empty());
    // ...but once published it is frozen.
    let mut later = additive.clone();
    later["capabilities"][idx]["result_schema"] = json!({"$ref": "#/components/schemas/Dataset"});
    assert!(!breaking_changes(&additive, &schemas, &later, &schemas).is_empty());

    let mut schema_changed = schemas.clone();
    schema_changed["components"]["schemas"]["Health"]["description"] = json!("changed");
    assert!(!breaking_changes(&caps, &schemas, &caps, &schema_changed).is_empty());

    // An announced major bump allows breaking changes.
    let mut major = removed.clone();
    major["contract_version"] = json!("2.0.0");
    assert!(breaking_changes(&caps, &schemas, &major, &schemas).is_empty());
}

#[test]
fn published_test_vectors_verify() {
    let h = rendered("fixtures/hmac-v2.json");
    let (method, uri, nonce) = (
        h["method"].as_str().unwrap(),
        h["uri"].as_str().unwrap(),
        h["nonce"].as_str().unwrap(),
    );
    let ts = h["timestamp"].as_i64().unwrap();
    let secret = h["secret_utf8"].as_str().unwrap().as_bytes();
    assert!(wire::hmac_v2_verify(
        secret,
        method,
        uri,
        ts,
        nonce,
        b"",
        h["signature"].as_str().unwrap()
    ));
    assert!(!wire::hmac_v2_verify(
        secret,
        method,
        &format!("{uri}&x=1"),
        ts,
        nonce,
        b"",
        h["signature"].as_str().unwrap()
    ));
    assert!(!wire::hmac_v2_verify(
        b"other",
        method,
        uri,
        ts,
        nonce,
        b"",
        h["signature"].as_str().unwrap()
    ));
    assert!(!wire::hmac_v2_verify(
        secret,
        method,
        uri,
        ts,
        nonce,
        b"",
        &h["signature"].as_str().unwrap().to_ascii_uppercase()
    ));
    assert_eq!(wire::request_digest(method, uri, b""), h["request_digest"]);

    let s = rendered("fixtures/staff-assertion.json");
    let key = VerifyingKey::from_bytes(
        &hex::decode(s["public_key_hex"].as_str().unwrap())
            .unwrap()
            .try_into()
            .unwrap(),
    )
    .unwrap();
    let jws = s["jws"].as_str().unwrap();
    let (input, sig) = jws.rsplit_once('.').unwrap();
    let sig = Signature::from_slice(&B64.decode(sig).unwrap()).unwrap();
    key.verify(input.as_bytes(), &sig)
        .expect("fixture assertion verifies");
    assert_eq!(s["claims"]["request_digest"], h["request_digest"]);
}

#[test]
fn source_sync_ceiling_is_exactly_feed_status_and_datasets() {
    let policy = rendered("access-policy.v1.json");
    assert_eq!(
        policy["roles"]["cfmeet.source_sync"]["grants"],
        json!({
            "bridge.datasets.read": ["metrics-read"],
            "hq.events.ack": ["event-sync"],
            "hq.events.read": ["event-sync"],
            "hq.projections.read": ["event-sync"]
        })
    );
}

/// HQ classifies reads by id suffix and shape (agreed with HQ Track B): collections end in
/// `.list` / `.search` and are paged or searched; nothing else may end that way.
#[test]
fn collections_and_only_collections_end_in_list_or_search() {
    for c in catalog() {
        if c.kind == Kind::Command {
            assert!(
                !c.id.ends_with(".list") && !c.id.ends_with(".search"),
                "{}: a command named like a collection",
                c.id
            );
            continue;
        }
        let collection = c.id.ends_with(".list") || c.id.ends_with(".search");
        let paged = matches!(
            c.shape,
            QueryShape::ScopedPage | QueryShape::ScopedWindow | QueryShape::ScopedSearch
        );
        if collection {
            assert!(
                paged,
                "{}: a collection must be paged, windowed or searched",
                c.id
            );
            assert_eq!(c.method, Method::Get, "{}", c.id);
        }
        if c.id.ends_with(".search") {
            assert_eq!(c.shape, QueryShape::ScopedSearch, "{}", c.id);
        }
    }
}

/// HQ treats every non-GET as a COMMAND unless it is on its reviewed POST-read list. Adding a
/// read-type POST here requires telling HQ first.
#[test]
fn read_type_posts_are_exactly_the_ones_hq_reviewed() {
    let posts: Vec<&str> = catalog()
        .into_iter()
        .filter(|c| c.method == Method::Post && c.kind != Kind::Command)
        .map(|c| c.id)
        .collect();
    assert_eq!(posts, ["hq.events.ack", "pricing.charges.simulate"]);
}

/// HQ's read path refuses any result whose object KEY matches its personal-data list
/// (case-insensitive, exact match), whatever the value, even masked or hashed. No
/// published schema may use one of these names as a property anywhere (agreed with HQ
/// Track B): name masked values after what they are (`address_masked`, `recipient_hash`).
const HQ_BANNED_KEYS: &[&str] = &[
    "nin",
    "bvn",
    "ssn",
    "national_id",
    "passport",
    "passport_number",
    "document_number",
    "id_number",
    "password",
    "passcode",
    "pin",
    "otp",
    "secret",
    "token",
    "api_key",
    "private_key",
    "phone",
    "phone_number",
    "msisdn",
    "email",
    "email_address",
    "latitude",
    "longitude",
    "lat",
    "lng",
    "lon",
    "gps",
    "coordinates",
    "location_trail",
    "account_number",
    "iban",
    "card_number",
    "pan",
    "cvv",
    "message_body",
    "body_text",
    "raw_payload",
];

fn collect_property_names(v: &Value, path: &str, out: &mut Vec<String>) {
    match v {
        Value::Object(m) => {
            if let Some(Value::Object(props)) = m.get("properties") {
                for (k, sub) in props {
                    out.push(format!("{path}.{k}"));
                    collect_property_names(sub, &format!("{path}.{k}"), out);
                }
            }
            for (k, sub) in m {
                if k != "properties" {
                    collect_property_names(sub, path, out);
                }
            }
        }
        Value::Array(a) => a.iter().for_each(|x| collect_property_names(x, path, out)),
        _ => {}
    }
}

#[test]
fn no_schema_uses_a_key_hq_treats_as_personal_data() {
    let schemas = rendered("bridge-schemas.v1.json");
    let mut names = Vec::new();
    for (name, schema) in schemas["components"]["schemas"].as_object().unwrap() {
        collect_property_names(schema, name, &mut names);
    }
    assert!(!names.is_empty());
    let banned: Vec<&String> = names
        .iter()
        .filter(|p| {
            HQ_BANNED_KEYS.contains(&p.rsplit('.').next().unwrap().to_ascii_lowercase().as_str())
        })
        .collect();
    assert!(banned.is_empty(), "properties HQ would refuse: {banned:?}");
}
