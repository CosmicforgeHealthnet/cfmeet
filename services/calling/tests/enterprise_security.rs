//! Part F cross-cutting security (§149–§154): the right to make an enterprise Call grants
//! no administration, no voicemail or recording authority; each administrative family
//! needs its own scope; and the public enterprise contracts carry no telephone number,
//! provider / edge identifier, key sequence, object key or permanent URL.

mod common;

use std::path::PathBuf;

use axum::http::{Method, StatusCode};
use calling::platform::scopes::names;
use common::enterprise::{credential, EnterpriseWorld};
use serde_json::{json, Value};
use uuid::Uuid;

/// Every administrative / artifact route family (method, path, body).
fn admin_routes() -> Vec<(Method, String, Option<Value>)> {
    let id = Uuid::new_v4();
    let mut out = Vec::new();
    for family in [
        "extensions",
        "departments",
        "ring-groups",
        "queues",
        "ivr-flows",
        "business-hours",
        "forwarding-rules",
        "policies",
        "sip-devices",
        "public-contacts",
        "recording-policies",
    ] {
        out.push((Method::GET, format!("/v1/enterprise/{family}"), None));
        out.push((
            Method::POST,
            format!("/v1/enterprise/{family}"),
            Some(json!({})),
        ));
        out.push((Method::GET, format!("/v1/enterprise/{family}/{id}"), None));
    }
    out.push((Method::GET, "/v1/enterprise/audit".into(), None));
    out.push((
        Method::GET,
        "/v1/enterprise/voicemail/mailboxes".into(),
        None,
    ));
    out.push((
        Method::POST,
        "/v1/enterprise/voicemail/mailboxes".into(),
        Some(json!({})),
    ));
    out.push((
        Method::GET,
        "/v1/enterprise/voicemail/messages".into(),
        None,
    ));
    out.push((
        Method::POST,
        format!("/v1/enterprise/voicemail/messages/{id}/media-access"),
        Some(json!({})),
    ));
    out.push((Method::GET, format!("/v1/enterprise/recordings/{id}"), None));
    out.push((
        Method::POST,
        format!("/v1/enterprise/recordings/{id}/media-access"),
        Some(json!({})),
    ));
    out.push((
        Method::GET,
        format!("/v1/enterprise/recordings/{id}/access-audit"),
        None,
    ));
    out.push((
        Method::POST,
        format!("/v1/calls/{id}/recordings"),
        Some(json!({})),
    ));
    out
}

/// The scope a route family needs (the most specific one).
fn needs(path: &str, method: &Method) -> &'static str {
    if path.starts_with("/v1/enterprise/voicemail/mailboxes") {
        // Mailboxes are configuration to read; managing them is voicemail authority.
        if method == Method::GET {
            names::ENTERPRISE_CONFIG_READ
        } else {
            names::ENTERPRISE_VOICEMAIL_MANAGE
        }
    } else if path.contains("/voicemail/") {
        if method == Method::GET || path.ends_with("/media-access") {
            names::ENTERPRISE_VOICEMAIL_READ
        } else {
            names::ENTERPRISE_VOICEMAIL_MANAGE
        }
    } else if path.contains("/recordings") || path.contains("recording-policies") {
        if path.contains("recording-policies") && method == Method::GET {
            names::ENTERPRISE_CONFIG_READ
        } else if method == Method::GET && !path.ends_with("/access-audit")
            || path.ends_with("/media-access")
        {
            names::ENTERPRISE_RECORDING_READ
        } else {
            names::ENTERPRISE_RECORDING_MANAGE
        }
    } else if method == Method::GET {
        names::ENTERPRISE_CONFIG_READ
    } else {
        names::ENTERPRISE_CONFIG_WRITE
    }
}

/// §150: a credential that can make enterprise Calls (calls.* + enterprise.calls.use)
/// gets 403 INSUFFICIENT_SCOPE on every administrative, voicemail and recording route;
/// a configuration administrator gets 403 on voicemail and recording authority; each
/// family needs its own scope — never implied by another.
#[tokio::test]
async fn calling_authority_never_implies_administration_or_artifact_access() {
    let Some(e) = EnterpriseWorld::new().await else {
        return;
    };
    let config_admin = credential(
        e.h(),
        &e.a.w.consumer,
        e.a.w.kind,
        &[
            names::ENTERPRISE_CONFIG_READ,
            names::ENTERPRISE_CONFIG_WRITE,
        ],
    )
    .await;
    let voicemail_admin = credential(
        e.h(),
        &e.a.w.consumer,
        e.a.w.kind,
        &[
            names::ENTERPRISE_VOICEMAIL_READ,
            names::ENTERPRISE_VOICEMAIL_MANAGE,
        ],
    )
    .await;
    let holders: [(&str, &[&str], String); 3] = [
        (
            "enterprise caller",
            &[
                names::CALLS_READ,
                names::CALLS_WRITE,
                names::ENTERPRISE_CALLS_USE,
            ],
            e.caller_token().to_string(),
        ),
        (
            "configuration administrator",
            &[
                names::ENTERPRISE_CONFIG_READ,
                names::ENTERPRISE_CONFIG_WRITE,
            ],
            config_admin.secret.expose_once().to_string(),
        ),
        (
            "voicemail administrator",
            &[
                names::ENTERPRISE_VOICEMAIL_READ,
                names::ENTERPRISE_VOICEMAIL_MANAGE,
            ],
            voicemail_admin.secret.expose_once().to_string(),
        ),
    ];
    let mut refused = 0;
    for (who, scopes, token) in &holders {
        for (method, path, body) in admin_routes() {
            let required = needs(&path, &method);
            if scopes.contains(&required) {
                continue;
            }
            let r = e.send(token, method.clone(), &path, body).await;
            assert_eq!(
                r.status,
                StatusCode::FORBIDDEN,
                "{who}: {method} {path} needs {required}: {}",
                r.text
            );
            assert_eq!(
                r.json["error"]["code"], "INSUFFICIENT_SCOPE",
                "{who} {path}"
            );
            refused += 1;
        }
    }
    assert!(refused > 60, "the matrix exercised {refused} refusals");
}

fn contracts() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../packages/contracts")
}

fn read(rel: &str) -> Value {
    serde_json::from_str(&std::fs::read_to_string(contracts().join(rel)).unwrap()).unwrap()
}

/// Property names of every object schema under `value`, with the enclosing schema path.
fn property_names(value: &Value, path: &str, out: &mut Vec<(String, String)>) {
    match value {
        Value::Object(map) => {
            for (k, v) in map {
                if k == "properties" {
                    if let Some(props) = v.as_object() {
                        for name in props.keys() {
                            out.push((path.to_string(), name.clone()));
                        }
                    }
                }
                property_names(v, &format!("{path}/{k}"), out);
            }
        }
        Value::Array(items) => {
            for (i, v) in items.iter().enumerate() {
                property_names(v, &format!("{path}/{i}"), out);
            }
        }
        _ => {}
    }
}

/// §153 / §154 / §180: no public enterprise or history body has a field for a telephone
/// number, an edge channel or SIP Call-ID, a provider or carrier, a key sequence, an
/// object key or bucket; a URL exists only in the short-lived signed media-access grants
/// (minted after authorization), next to their expiry.
#[test]
fn public_enterprise_contracts_carry_no_numbers_provider_ids_or_storage_secrets() {
    let mut fields = Vec::new();
    property_names(
        &read("calling/enterprise/v1.schema.json"),
        "enterprise",
        &mut fields,
    );
    let api = read("calling/http/v1.openapi.json");
    for (path, item) in api["paths"].as_object().unwrap() {
        if path.starts_with("/v1/enterprise")
            || path.contains("recording")
            || path == "/v1/calling/history"
        {
            property_names(item, path, &mut fields);
        }
    }
    for schema in [
        "CallHistoryEntry",
        "CallHistoryList",
        "CallParticipation",
        "CallCounterpart",
        "CallHistoryEnterprise",
        "CallHistoryVoicemail",
    ] {
        property_names(&api["components"]["schemas"][schema], schema, &mut fields);
    }
    assert!(fields.len() > 150, "{} fields scanned", fields.len());
    let forbidden = [
        "phone",
        "msisdn",
        "e164",
        "telephone",
        "number",
        "callid",
        "asterisk",
        "provider",
        "carrier",
        "objectkey",
        "bucket",
        "spool",
        "dtmf",
        "digits",
        "sip",
        "uri",
        "url",
        "channel",
        "secret",
        "password",
        "token",
    ];
    let allowed = |path: &str, name: &str| match name {
        // CF Meet's own Call id, never a SIP Call-ID.
        "callId" => true,
        // The bounded notice channel (APPLICATION_SIGNAL / EDGE_TONE), not an edge channel.
        "channel" => true,
        // An opaque compliance-configuration reference ("jurisdiction" contains "uri").
        "jurisdictionRef" => true,
        // Internal extension short codes / ordinals, never telephone numbers.
        "extensionNumber"
        | "versionNumber"
        | "activeVersionNumber"
        | "latestVersionNumber"
        | "stepNumber"
        | "maxRingGroupFanout" => true,
        // CF Meet's own SIP device resource (an opaque id), never an edge or SIP identifier.
        "sipDeviceId" | "sipDevices" => true,
        // The short-lived signed read of ONE object, minted after authorization.
        "url" => path.contains("MediaAccess") || path.contains("media-access"),
        _ => false,
    };
    let mut violations: Vec<String> = fields
        .iter()
        .filter(|(path, name)| {
            let lower = name.to_ascii_lowercase();
            forbidden.iter().any(|f| lower.contains(f)) && !allowed(path, name)
        })
        .map(|(path, name)| format!("{path}: {name}"))
        .collect();
    violations.sort();
    violations.dedup();
    assert!(violations.is_empty(), "{violations:#?}");
    // Every signed grant states its expiry.
    let ent = read("calling/enterprise/v1.schema.json");
    for (name, def) in ent["$defs"].as_object().unwrap() {
        if def["properties"].get("url").is_some() {
            assert!(
                def["properties"].get("expiresAt").is_some(),
                "{name} has a url without expiresAt"
            );
        }
    }
}
