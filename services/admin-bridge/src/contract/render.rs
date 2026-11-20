//! Renders every file of `docs/contract/` from the catalog. Deterministic: same catalog,
//! same bytes (LF line endings, trailing newline, stable key order).

use std::collections::BTreeMap;

use base64::{engine::general_purpose::URL_SAFE_NO_PAD as B64, Engine};
use ed25519_dalek::{Signer, SigningKey};
use serde_json::{json, Map, Value};

use super::catalog::catalog;
use super::datasets::{COVERAGE_RULE, DATASETS};
use super::events::adapters_document;
use super::model::*;
use super::schemas::{components, error_envelope, result_schema, success_envelope};
use crate::errors::ErrorCode;
use crate::wire;

pub type Files = BTreeMap<String, Vec<u8>>;

fn json_bytes(v: &Value) -> Vec<u8> {
    let mut s = serde_json::to_string_pretty(v).expect("serializable");
    s.push('\n');
    s.into_bytes()
}

fn param_schema(p: &PathParam) -> Value {
    match p.format {
        ParamFormat::Uuid => json!({"type": "string", "format": "uuid"}),
        ParamFormat::Opaque => {
            json!({"type": "string", "pattern": OPAQUE_ID_PATTERN, "maxLength": 128})
        }
        ParamFormat::DatasetId => {
            json!({"type": "string", "pattern": DATASET_ID_PATTERN, "maxLength": 56})
        }
    }
}

fn query_param_schema(name: &str, cap: &Capability) -> Value {
    match name {
        "organization" => json!({"const": PRODUCT}),
        "zone" => json!({"enum": ["production", "sandbox"]}),
        "purpose" => json!({"enum": cap.purposes.iter().map(|p| p.as_str()).collect::<Vec<_>>()}),
        "limit" => json!({"type": "integer", "minimum": 1, "maximum": MAX_LIMIT}),
        "cursor" => json!({"type": "string", "pattern": CURSOR_PATTERN}),
        "start" | "end" => json!({"type": "string", "format": "date-time"}),
        "q" => json!({"type": "string", "pattern": SEARCH_PATTERN, "maxLength": 128}),
        _ => json!({"type": "string", "pattern": SCOPE_VALUE_PATTERN, "maxLength": 128}),
    }
}

fn openapi(caps: &[Capability]) -> Value {
    let mut paths: Map<String, Value> = Map::new();
    for cap in caps {
        let mut params: Vec<Value> = cap
            .path_params
            .iter()
            .map(|p| json!({"name": p.name, "in": "path", "required": true, "schema": param_schema(p)}))
            .collect();
        for name in cap.shape.params() {
            params.push(json!({
                "name": name,
                "in": "query",
                "required": cap.shape.required().contains(name),
                "schema": query_param_schema(name, cap),
            }));
        }
        if cap.idempotency_required() {
            params.push(json!({
                "name": "Idempotency-Key",
                "in": "header",
                "required": true,
                "schema": {"type": "string", "format": "uuid"}
            }));
        }
        let mut responses = Map::new();
        responses.insert(
            "200".into(),
            json!({"description": "Success", "content": {"application/json": {"schema": success_envelope(cap.result)}}}),
        );
        for code in [
            "400", "401", "403", "404", "409", "413", "422", "429", "500", "502", "503",
        ] {
            responses.insert(
                code.into(),
                json!({"description": "Error (signing-profile.md lists every code)", "content": {"application/json": {"schema": {"$ref": "#/components/schemas/ErrorEnvelope"}}}}),
            );
        }
        let mut op = json!({
            "operationId": cap.id,
            "summary": cap.id,
            "tags": [cap.module().as_str()],
            "parameters": params,
            "responses": responses,
            "x-cfmeet-kind": cap.kind,
            "x-cfmeet-query-shape": cap.shape,
            "x-cfmeet-permission": cap.permission,
            "x-cfmeet-risk": cap.risk,
            "x-cfmeet-approval": cap.approval(),
            "x-cfmeet-state": cap.state(),
            "x-cfmeet-owner": cap.owner,
        });
        if cap.method == Method::Post {
            op["requestBody"] = json!({
                "required": true,
                "content": {"application/json": {"schema": result_schema(cap.body)}}
            });
        }
        let entry = paths.entry(cap.full_path()).or_insert_with(|| json!({}));
        entry[cap.method.as_str().to_ascii_lowercase()] = op;
    }
    let mut schemas = components();
    schemas.insert("ErrorEnvelope".into(), error_envelope());
    json!({
        "openapi": "3.1.0",
        "info": {
            "title": "CF Meet Admin Bridge (HQ interface v1)",
            "version": CONTRACT_VERSION,
            "description": "Private HQ -> CF Meet admin bridge. Transport, signing, staff assertion and authorization rules: signing-profile.md. Artifact state never authorizes anything at runtime."
        },
        "servers": [{"url": "https://admin-bridge.cfmeet.internal"}],
        "paths": paths,
        "components": {"schemas": schemas}
    })
}

fn matrix(caps: &[Capability]) -> Value {
    Value::Array(
        caps.iter()
            .map(|c| {
                json!({
                    "id": c.id,
                    "version": c.version,
                    "owner": c.owner,
                    "method": c.method,
                    "bridge_endpoint": c.path,
                    "source_endpoint": c.source_endpoint,
                    "state": c.state(),
                    "permission": c.permission,
                    "risk": c.risk,
                    "privacy": c.privacy,
                    "scope": c.scope_rule(),
                    "source_version_required": c.source_version_required(),
                    "idempotency_required": c.idempotency_required(),
                    "approval": c.approval(),
                    "feature_flag": c.feature_flag(),
                    "blocker": c.blocker(),
                    "tests": c.tests,
                })
            })
            .collect(),
    )
}

fn by_state(caps: &[Capability]) -> Value {
    let mut m = Map::new();
    for s in State::ALL {
        m.insert(
            s.as_str().into(),
            json!(caps.iter().filter(|c| c.state() == *s).count()),
        );
    }
    Value::Object(m)
}

fn capabilities_doc(caps: &[Capability], openapi_sha: &str, matrix_sha: &str) -> Value {
    json!({
        "contract": CONTRACT_NAME,
        "contract_version": CONTRACT_VERSION,
        "envelope_version": ENVELOPE_VERSION,
        "product": PRODUCT,
        "hq_principal": HQ_PRINCIPAL,
        "bridge_base_path": BRIDGE_BASE,
        "source_artifacts": {"openapi_sha256": openapi_sha, "capability_matrix_sha256": matrix_sha},
        "query_shapes": QueryShape::ALL.iter().map(|s| (s.as_str().to_string(), json!(s.params()))).collect::<Map<_, _>>(),
        "totals": {"capabilities": caps.len(), "by_state": by_state(caps)},
        "capabilities": caps.iter().map(|c| json!({
            "id": c.id,
            "version": c.version,
            "module": c.module(),
            "kind": c.kind,
            "owner": c.owner,
            "owner_status": c.owner_status,
            "phase": c.phase,
            "method": c.method,
            "path": c.path,
            "source_endpoint": c.source_endpoint,
            "path_params": c.path_params.iter().map(|p| {
                let mut v = json!({"name": p.name});
                match p.format {
                    ParamFormat::Uuid => { v["format"] = json!("uuid"); }
                    ParamFormat::Opaque => { v["pattern"] = json!(OPAQUE_ID_PATTERN); v["max_length"] = json!(128); }
                    ParamFormat::DatasetId => { v["pattern"] = json!(DATASET_ID_PATTERN); v["max_length"] = json!(56); }
                }
                v
            }).collect::<Vec<_>>(),
            "query_shape": c.shape,
            "query_required": c.shape.required(),
            "purposes": c.purposes,
            "roles": c.roles,
            "request_body": if c.method == Method::Post { result_schema(c.body) } else { Value::Null },
            "success_status": 200,
            "success_media": "application/json",
            "result_schema": result_schema(c.result),
            "matrix_state": c.state(),
            "permission": c.permission,
            "risk": c.risk,
            "privacy": c.privacy,
            "scope": c.scope_rule(),
            "source_version_required": c.source_version_required(),
            "idempotency_required": c.idempotency_required(),
            "approval": c.approval(),
            "feature_flag": c.feature_flag(),
            "blocker": c.blocker(),
            "source_tests": c.tests,
        })).collect::<Vec<_>>(),
    })
}

/// The access-policy ceiling: the most any signed runtime policy may grant each role.
fn access_policy(caps: &[Capability]) -> Value {
    let mut roles = Map::new();
    for role in Role::ALL {
        // permission -> purposes, per role: a purpose is granted only with the
        // permissions whose capabilities allow it.
        let mut grants: BTreeMap<&str, Vec<&str>> = BTreeMap::new();
        for c in caps.iter().filter(|c| c.roles.contains(role)) {
            let purposes = grants.entry(c.permission.as_str()).or_default();
            purposes.extend(
                c.purposes
                    .iter()
                    .filter(|p| role.allows_purpose(**p))
                    .map(|p| p.as_str()),
            );
            purposes.sort_unstable();
            purposes.dedup();
        }
        roles.insert(
            role.as_str().into(),
            json!({"grants": grants, "scopes": {
                "market": "as enrolled for HQ (GLOBAL or listed countries)",
                "organization": [PRODUCT],
                "zone": ["production", "sandbox"],
                "tenant": "as enrolled for HQ"
            }}),
        );
    }
    json!({
        "policy": "cfmeet-admin-bridge.access-policy-ceiling.v1",
        "contract_version": CONTRACT_VERSION,
        "rule": "The runtime access policy is signed by CF Meet (Ed25519) and pinned by revision; every grant it makes must be inside this ceiling, or the bridge refuses to load it. A permission is effective only when the staff assertion lists it AND one of the asserted roles is granted it with the request's purpose. A role is never inferred from list order.",
        "roles": roles
    })
}

fn datasets_doc() -> Value {
    json!({
        "datasets": DATASETS.iter().map(|d| json!({
            "id": d.id,
            "description": d.description,
            "classification": "INTERNAL",
            "capability": "bridge.datasets.read",
            "metrics": d.metrics,
            "sources": d.sources.iter().map(|s| json!({"service": s.owner, "via": s.via})).collect::<Vec<_>>(),
            "coverage_rule": COVERAGE_RULE,
        })).collect::<Vec<_>>()
    })
}

// ---- Fixtures (test vectors produced by the runtime's own wire code) -------------

/// RFC 8032 §7.1 TEST 1 secret key: a public test key, never a real one.
const FIXTURE_SEED: &str = "9d61b19deffd5a60ba844af492ec2cc44449c5697b326919703bac031cae7f60";
const FIXTURE_HMAC_SECRET: &str = "fixture-only-hmac-secret-not-for-use-0123456789";

fn fixture_uri() -> String {
    format!("{BRIDGE_BASE}/datasets/cfmeet.engagement_daily?market=GLOBAL&organization=cfmeet&zone=production&tenant=cfmeet-first-party&purpose=metrics-read&limit=31&start=2026-11-01T00:00:00Z&end=2026-11-19T00:00:00Z")
}

fn compact_jws(header: &Value, claims: &Value) -> String {
    let seed: [u8; 32] = hex::decode(FIXTURE_SEED)
        .expect("hex")
        .try_into()
        .expect("32 bytes");
    let key = SigningKey::from_bytes(&seed);
    let input = format!(
        "{}.{}",
        B64.encode(serde_json::to_vec(header).expect("json")),
        B64.encode(serde_json::to_vec(claims).expect("json"))
    );
    let sig = key.sign(input.as_bytes());
    format!("{input}.{}", B64.encode(sig.to_bytes()))
}

fn fixtures() -> Files {
    let mut f = Files::new();
    let uri = fixture_uri();
    let (ts, nonce) = (1_795_000_000_i64, "00112233445566778899aabbccddeeff");
    f.insert(
        "fixtures/hmac-v2.json".into(),
        json_bytes(&json!({
            "description": "HMAC v2 and request_digest test vector (GET, empty body). The secret is a fixture value.",
            "secret_utf8": FIXTURE_HMAC_SECRET,
            "method": "GET",
            "uri": uri,
            "timestamp": ts,
            "nonce": nonce,
            "body_utf8": "",
            "canonical": wire::hmac_v2_canonical("GET", &uri, ts, nonce, b""),
            "signature": wire::hmac_v2_signature(FIXTURE_HMAC_SECRET.as_bytes(), "GET", &uri, ts, nonce, b""),
            "request_digest": wire::request_digest("GET", &uri, b""),
        })),
    );
    let seed: [u8; 32] = hex::decode(FIXTURE_SEED)
        .expect("hex")
        .try_into()
        .expect("32 bytes");
    let public = SigningKey::from_bytes(&seed).verifying_key();
    let header = json!({"alg": "EdDSA", "typ": "JWT", "kid": "hq-staff-fixture-1"});
    let claims = json!({
        "iss": "cosmicforge-hq",
        "sub": "staff-0001",
        "aud": "cfmeet-admin-bridge",
        "product": PRODUCT,
        "roles": ["cfmeet.viewer"],
        "permissions": ["bridge.datasets.read"],
        "market_scopes": ["GLOBAL"],
        "organization_scopes": [PRODUCT],
        "zone_scopes": ["production"],
        "tenant_scopes": ["cfmeet-first-party"],
        "session_id": "sess-0001",
        "auth_time": ts - 60,
        "acr": "urn:cosmicforge:acr:mfa",
        "jti": "6f1c2a3b-4d5e-4f60-8a7b-9c0d1e2f3a4b",
        "iat": ts,
        "nbf": ts,
        "exp": ts + 20,
        "request_digest": wire::request_digest("GET", &uri, b""),
    });
    f.insert(
        "fixtures/staff-assertion.json".into(),
        json_bytes(&json!({
            "description": "Ed25519 staff assertion test vector bound to fixtures/hmac-v2.json's request. Key: RFC 8032 section 7.1 TEST 1 (public test key).",
            "public_key_hex": hex::encode(public.to_bytes()),
            "header": header,
            "claims": claims,
            "jws": compact_jws(&header, &claims),
        })),
    );
    let cmd_uri = format!("{BRIDGE_BASE}/verification/cases/case-0001/partner-go-live/approve?market=GLOBAL&organization=cfmeet&zone=production&tenant=cfmeet-first-party&purpose=verification-review");
    let body = br#"{"expected_version":3,"note_ref":"evidence-0001"}"#;
    f.insert(
        "fixtures/approval-claim.json".into(),
        json_bytes(&json!({
            "description": "Approval claim carried in the staff assertion of a TWO_PERSON command. requester_ref and approver_ref are raw HQ subs; the bridge forwards them as X-CF-Operator: hq:<sub> and X-CF-Approver: hq:<sub>. Command bodies are defined per phase; this body is illustrative.",
            "method": "POST",
            "uri": cmd_uri,
            "body_utf8": std::str::from_utf8(body).expect("utf8"),
            "request_digest": wire::request_digest("POST", &cmd_uri, body),
            "approval": {
                "approval_id": "0b7e7f0e-2b9c-4c8e-9d7a-1f2e3d4c5b6a",
                "capability": "verification.partner-go-live.approve",
                "requester_ref": "staff-0001",
                "approver_ref": "staff-0002",
                "intent_digest": wire::sha256_hex(b"fixture intent"),
                "decided_at": "2026-11-19T10:00:00Z",
                "expires_at": "2026-11-19T22:00:00Z"
            }
        })),
    );
    f.insert(
        "fixtures/error.json".into(),
        json_bytes(&json!({
            "contract_version": ENVELOPE_VERSION,
            "product": PRODUCT,
            "correlation_id": "2f4e6a8c-0b1d-4e3f-9a5b-7c9d1e3f5a7b",
            "as_of": "2026-11-19T10:00:00Z",
            "freshness": "UNAVAILABLE",
            "error": {"code": ErrorCode::ScopeDenied.code(), "message": ErrorCode::ScopeDenied.meaning()}
        })),
    );
    f
}

// ---- signing-profile.md ----------------------------------------------------------

fn signing_profile(caps: &[Capability]) -> Vec<u8> {
    let mut s = String::new();
    s.push_str(&format!(
        "# CF Meet Admin Bridge signing profile (HQ interface v1, contract {CONTRACT_VERSION})\n\n\
Generated from `services/admin-bridge/src` by `admin-bridge-contract generate`; do not edit.\n\
Design: `docs/admin/cfmeet-admin-bridge.md` §3. Test vectors: `fixtures/`.\n\n\
## Transport\n\n\
- Private network only, dedicated port, TLS passed straight through. Never reachable through `api.cfmeet.com`.\n\
- TLS 1.3 only, mutual TLS. The client certificate must carry exactly one URI SAN equal to the enrolled HQ workload \
identity, and its SHA-256 fingerprint must match a live enrolled row for `{HQ_PRINCIPAL}`. Checked on every request.\n\
- Path at most 4096 bytes; no percent-encoded bytes and no `.` / `..` segments in the path. Body at most 32 KiB, JSON only.\n\n\
## HMAC v2\n\n\
Headers: `X-HQ-Key-ID`, `X-Service-Name: {HQ_PRINCIPAL}`, `X-Service-Timestamp` (unix seconds), `X-Service-Nonce` \
(32 lowercase hex), `X-Service-Signature-Version: 2`, `X-Service-Signature` (64 lowercase hex).\n\n\
```\nsignature = hex(HMAC-SHA256(key, \"2\" LF \"{HQ_PRINCIPAL}\" LF METHOD LF request-URI LF timestamp LF nonce LF hex(sha256(body))))\n```\n\n\
`request-URI` is the path and query exactly as sent. Clock skew at most ±30 s. Each HMAC key has its own expiry. \
The nonce is single-use (see Replay).\n\n\
## Staff assertion\n\n\
`Authorization: Bearer <compact JWS>`. Protected header exactly `{{\"alg\":\"EdDSA\",\"typ\":\"JWT\",\"kid\":<enrolled kid>}}` \
(no other member). Ed25519.\n\n\
| Claim | Rule |\n|---|---|\n\
| `iss`, `aud` | equal the enrolled issuer and audience |\n\
| `product` | `\"{PRODUCT}\"` |\n\
| `sub` | HQ staff id, 1–128 of `[A-Za-z0-9._:@-]` |\n\
| `roles[]` | bridge roles (below); none is preferred by position |\n\
| `permissions[]` | must contain the capability's permission |\n\
| `market_scopes[]`, `organization_scopes[]`, `zone_scopes[]`, `tenant_scopes[]` | all four always present; the request's values must be members |\n\
| `session_id`, `auth_time`, `acr` | revocation is checked per `sub` and per `session_id`; R2+ needs `now − auth_time ≤ 300`; R3/R4 need an enrolled strong `acr` |\n\
| `jti` | UUID, single-use |\n\
| `iat`, `nbf`, `exp` | `exp − iat ≤ 60`; `nbf ≤ now < exp` (±30 s skew) |\n\
| `request_digest` | `hex(sha256(METHOD LF request-URI LF \"{PRODUCT}\" LF hex(sha256(body))))` |\n\
| `approval` | required on every TWO_PERSON capability (below) |\n\n\
### Approval claim\n\n\
`approval = {{approval_id (uuid), capability, requester_ref, approver_ref, intent_digest (64 lowercase hex), decided_at, expires_at (RFC 3339 UTC)}}`.\n\
Refused when: missing on a TWO_PERSON capability; `capability` is not the route called; `requester_ref ≠ sub`; \
`approver_ref = sub`; `expires_at ≤ now`; `expires_at − decided_at > 43200 s`; `approval_id` used before (stored). \
The bridge forwards `X-CF-Operator: hq:<sub>`, `X-CF-Approver: hq:<approver_ref>`, `X-CF-Approval-Ref` and \
`X-CF-Intent-Digest`; the owning service repeats the approver ≠ operator check.\n\n\
## Replay\n\n\
The HMAC nonce and the assertion `jti` are stored together in one transaction and kept 11 minutes. A repeat of either is `409 REPLAY`.\n\n\
## Order of checks\n\n\
1 request shape → 2 client certificate → 3 HMAC v2 → 4 staff assertion → 5 request digest → 6 revocation → \
7 replay → 8 capability lookup (method + path; unknown is 404) → 9 authorization (pinned signed access policy; \
permission, role and purpose; all four scopes; MFA / acr by risk; approval) → 10 rate limit → 11 audit row written \
(failure: 503, no data, nothing done) → owner call.\n\n\
## Query shapes\n\n\
Exact ordered lists; a missing required, extra, repeated or out-of-order parameter is `400 QUERY_INVALID`.\n\n\
| Shape | Parameters (in order) | Required |\n|---|---|---|\n"
    ));
    for shape in QueryShape::ALL {
        s.push_str(&format!(
            "| `{}` | {} | {} |\n",
            shape,
            shape.params().join(", "),
            shape.required().join(", ")
        ));
    }
    s.push_str(&format!(
        "\n- `organization` is always `{PRODUCT}`; `zone` is `production` or `sandbox`; `market` and `tenant` match `{SCOPE_VALUE_PATTERN}`.\n\
- `limit` 1–{MAX_LIMIT}; `cursor` matches `{CURSOR_PATTERN}`; `start` / `end` are RFC 3339 UTC, `end > start`, window at most {MAX_WINDOW_DAYS} days, `end` not in the future by more than 60 s.\n\
- `q` matches `{SEARCH_PATTERN}`.\n\
- Path parameters: `format: uuid`, or a pattern with no `/`.\n\n\
## Purposes\n\n"
    ));
    for p in Purpose::ALL {
        s.push_str(&format!("- `{p}`\n"));
    }
    s.push_str("\n## Roles\n\nIdentical to HQ's product-grant roles (identity role map), plus the HQ worker's feed identity:\n\n");
    for r in Role::ALL {
        s.push_str(&format!("- `{r}`\n"));
    }
    s.push_str("\nThe most each role can be granted is `access-policy.v1.json`.\n\n## Commands\n\n\
Every command needs `Idempotency-Key` (UUID). HQ never retries; the bridge sends a command to its owner at most once. \
A timeout or broken owner answer is `502 UNKNOWN_OUTCOME`, settled only by reading back. \
The one read that uses `POST` is `pricing.charges.simulate`.\n\n## Envelope\n\n\
`{contract_version:\"1.0\", product:\"cfmeet\", correlation_id (uuid), as_of, freshness, result, error}`. \
On error `result` is absent. Missing data is `null` or `UNAVAILABLE`, never zero.\n\n## Error codes\n\n| Code | HTTP | Meaning |\n|---|---|---|\n");
    for e in ErrorCode::ALL {
        s.push_str(&format!(
            "| `{}` | {} | {} |\n",
            e.code(),
            e.status(),
            e.meaning()
        ));
    }
    s.push_str("
## Command outcomes

For a command, these codes guarantee the owning service was never called (nothing was done): every 4xx code, and these 5xx codes, which the bridge raises before any owner call:

");
    for e in ErrorCode::ALL
        .iter()
        .filter(|e| e.status() >= 500 && e.nothing_done())
    {
        s.push_str(&format!(
            "- `{}`
",
            e.code()
        ));
    }
    s.push_str("
Any other 5xx after a command was sent (`UNKNOWN_OUTCOME`, `INTERNAL`, `OWNER_UNAVAILABLE`) is an unknown outcome: settle it by reading back, never resend.
");
    let served: Vec<&str> = caps.iter().filter(|c| c.served).map(|c| c.id).collect();
    s.push_str(&format!(
        "\n## Served in this contract version\n\n{}\n",
        if served.is_empty() {
            "None: the bridge runtime ships in phase A1.".to_string()
        } else {
            served
                .iter()
                .map(|i| format!("- `{i}`"))
                .collect::<Vec<_>>()
                .join("\n")
        }
    ));
    s.into_bytes()
}

/// Every contract file, keyed by path relative to `docs/contract/`.
pub fn render() -> Files {
    let caps = catalog();
    let mut files = Files::new();
    let openapi_bytes = json_bytes(&openapi(&caps));
    let matrix_bytes = json_bytes(&matrix(&caps));
    let caps_doc = capabilities_doc(
        &caps,
        &wire::sha256_hex(&openapi_bytes),
        &wire::sha256_hex(&matrix_bytes),
    );
    files.insert("openapi.json".into(), openapi_bytes);
    files.insert("CFMEET_ADMIN_CAPABILITY_MATRIX.json".into(), matrix_bytes);
    files.insert("capabilities.v1.json".into(), json_bytes(&caps_doc));
    let mut schemas = components();
    schemas.insert("ErrorEnvelope".into(), error_envelope());
    for (name, schema) in &schemas {
        files.insert(format!("schemas/{name}.json"), json_bytes(schema));
    }
    files.insert(
        "bridge-schemas.v1.json".into(),
        json_bytes(&json!({"components": {"schemas": schemas}})),
    );
    files.insert("datasets.v1.json".into(), json_bytes(&datasets_doc()));
    files.insert(
        "access-policy.v1.json".into(),
        json_bytes(&access_policy(&caps)),
    );
    files.insert(
        "event-adapters.v1.json".into(),
        json_bytes(&adapters_document()),
    );
    files.extend(fixtures());
    files.insert("signing-profile.md".into(), signing_profile(&caps));
    let sums: String = files
        .iter()
        .map(|(path, bytes)| format!("{}  {}\n", wire::sha256_hex(bytes), path))
        .collect();
    files.insert("SHA256SUMS".into(), sums.into_bytes());
    files
}
