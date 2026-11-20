//! Every owner command row (`contract::owner_commands`) through the real chain against a
//! fake owner: the owner path and camelCase body derived from the row, the strict
//! translation of the owner's `{status, version, resource}` answer, and the refusals
//! (unknown field, version rules, unknown status or resource field).

mod common;

use std::sync::{Arc, Mutex};

use admin_bridge::contract::catalog::catalog;
use admin_bridge::contract::model::{Owner, ParamFormat, Risk};
use admin_bridge::contract::owner_commands::{In, Out, OutField, OwnerCommand, Place, COMMANDS};
use admin_bridge::owners::{CommandCall, CommandOutcome, OwnerError, OwnerPort};
use admin_bridge::wire;
use async_trait::async_trait;
use chrono::{Duration, Utc};
use common::*;
use serde_json::{json, Map, Value};
use uuid::Uuid;

const ID: &str = "6f1c2a3b-4d5e-4f60-8a7b-9c0d1e2f3a4b";

#[derive(Default)]
struct FakeOwner {
    sent: Mutex<Vec<(Owner, String, Value)>>,
    answer: Mutex<Option<Value>>,
}

#[async_trait]
impl OwnerPort for FakeOwner {
    async fn read(
        &self,
        _: Owner,
        _: &str,
        _: &[(&str, String)],
        _: &str,
    ) -> Result<Value, OwnerError> {
        Err(OwnerError::Unavailable)
    }
    async fn command(&self, owner: Owner, call: CommandCall<'_>) -> CommandOutcome {
        self.sent
            .lock()
            .unwrap()
            .push((owner, call.path.to_string(), call.body.clone()));
        CommandOutcome::Applied(self.answer.lock().unwrap().clone().expect("answer"))
    }
}

fn sample_in(kind: In) -> Value {
    match kind {
        In::Enum(values) => json!(values[0]),
        In::Bool => json!(true),
        In::Int(min, _) => json!(min),
        In::Uuid | In::NullableUuid => json!(ID),
        In::Code => json!("ABUSE"),
        In::Str(min, _) | In::NullableStr(min, _) => json!("x".repeat(min.max(3))),
        In::UuidList(_) => json!([ID]),
        In::Object => json!({"section": {"enabled": true}}),
    }
}

fn sample_out(fields: &[OutField]) -> Value {
    let mut m = Map::new();
    for f in fields {
        let v = match f.kind {
            Out::Str => json!("id-1"),
            Out::Code => json!("ACTIVE"),
            Out::Uuid => json!(ID),
            Out::Int => json!(3),
            Out::Bool => json!(false),
            Out::Time => json!("2026-11-20T10:00:00Z"),
            Out::Text => json!("a reason"),
            Out::StrList => json!(["sharing"]),
            Out::Obj(inner) => sample_out(inner),
        };
        m.insert(f.owner.into(), v);
    }
    Value::Object(m)
}

fn status_of(row: &OwnerCommand) -> &'static str {
    row.statuses.first().copied().unwrap_or("READY")
}

fn approval(capability: &str, sub: &str) -> Value {
    let now = Utc::now();
    json!({
        "approval_id": Uuid::new_v4().to_string(),
        "capability": capability,
        "requester_ref": sub,
        "approver_ref": "staff-9999",
        "intent_digest": wire::sha256_hex(b"intent"),
        "decided_at": (now - Duration::minutes(1)).format("%Y-%m-%dT%H:%M:%SZ").to_string(),
        "expires_at": (now + Duration::hours(11)).format("%Y-%m-%dT%H:%M:%SZ").to_string(),
    })
}

/// A valid HQ call for the row, and the owner body the bridge must send.
fn hq_call(row: &OwnerCommand, sub: &str) -> (Call, Value, String) {
    let cap = catalog().into_iter().find(|c| c.id == row.id).unwrap();
    let mut path = cap.path.to_string();
    let mut owner_path = row.owner_path().to_string();
    for p in &cap.path_params {
        let v = match p.format {
            ParamFormat::Uuid => ID.to_string(),
            _ => "p-1".to_string(),
        };
        path = path.replace(&format!("{{{}}}", p.name), &v);
        owner_path = owner_path.replace(&format!("{{{}}}", p.name), &v);
    }
    let key = Uuid::new_v4().to_string();
    let mut body = json!({"operation_id": key});
    let mut owner = Map::new();
    if let Some(min) = row.version {
        body["expected_version"] = json!(min.max(1));
        owner.insert("expectedVersion".into(), json!(min.max(1)));
    }
    for f in row.body {
        let v = sample_in(f.kind);
        body[f.hq] = v.clone();
        match f.place {
            Place::Path => {
                owner_path = owner_path.replace(&format!("{{{}}}", f.hq), v.as_str().unwrap())
            }
            _ => {
                owner.insert(f.owner.into(), v);
            }
        }
    }
    let mut c = call(
        &path,
        &scoped(cap.purposes[0].as_str()),
        row.id,
        &[cap.roles[0].as_str()],
    );
    c.method = "POST";
    c.sub = sub.into();
    c.body = serde_json::to_vec(&body).unwrap();
    c.idempotency_key = Some(key);
    if cap.risk >= Risk::R3 {
        c.approval = Some(approval(row.id, sub));
    }
    (c, Value::Object(owner), owner_path)
}

#[tokio::test]
async fn owner_commands_follow_their_rows() {
    let owner = Arc::new(FakeOwner::default());
    let h = start(Options {
        owners: Some(owner.clone()),
        switches_all_on: true,
        ..Default::default()
    })
    .await;
    for (n, row) in COMMANDS.iter().enumerate() {
        let sub = format!("staff-{n:04}");
        let mut resource = sample_out(row.resource);
        resource["leaked_email"] = json!("someone@example.com");
        let version = row.version.map(|_| json!("4")).unwrap_or(Value::Null);
        *owner.answer.lock().unwrap() =
            Some(json!({"status": status_of(row), "version": version, "resource": resource}));
        let (c, owner_body, owner_path) = hq_call(row, &sub);
        let (status, v) = send(&h, &c).await;
        assert_eq!(status, 200, "{}: {v}", row.id);
        assert_eq!(
            v["result"]["operation"]["status"], "SUCCEEDED",
            "{}",
            row.id
        );
        assert_eq!(
            v["result"]["operation"]["source_status"],
            status_of(row),
            "{}",
            row.id
        );
        assert!(
            v["result"]["resource"].get("leaked_email").is_none(),
            "{}: closed resource",
            row.id
        );
        for f in row.resource {
            assert!(
                v["result"]["resource"].get(f.hq).is_some(),
                "{}: {}",
                row.id,
                f.hq
            );
        }
        let sent = owner.sent.lock().unwrap().last().cloned().unwrap();
        assert_eq!(sent.0, row.owner, "{}", row.id);
        assert_eq!(sent.1, owner_path, "{}", row.id);
        assert_eq!(sent.2, owner_body, "{}", row.id);
    }
}

#[tokio::test]
async fn owner_command_bodies_and_answers_are_strict() {
    let owner = Arc::new(FakeOwner::default());
    let h = start(Options {
        owners: Some(owner.clone()),
        switches_all_on: true,
        ..Default::default()
    })
    .await;
    let versioned = COMMANDS.iter().find(|r| r.version == Some(1)).unwrap();
    let unversioned = COMMANDS.iter().find(|r| r.version.is_none()).unwrap();
    let before = owner.sent.lock().unwrap().len();

    // An unknown field; a versioned command without its version; an unversioned one with one.
    let (mut c, _, _) = hq_call(versioned, "staff-1001");
    let mut body: Value = serde_json::from_slice(&c.body).unwrap();
    body["force"] = json!(true);
    c.body = serde_json::to_vec(&body).unwrap();
    assert_eq!(code(&send(&h, &c).await.1), "COMMAND_BODY_INVALID");
    let (mut c, _, _) = hq_call(versioned, "staff-1002");
    let mut body: Value = serde_json::from_slice(&c.body).unwrap();
    body.as_object_mut().unwrap().remove("expected_version");
    c.body = serde_json::to_vec(&body).unwrap();
    assert_eq!(code(&send(&h, &c).await.1), "COMMAND_BODY_INVALID");
    let (mut c, _, _) = hq_call(unversioned, "staff-1003");
    let mut body: Value = serde_json::from_slice(&c.body).unwrap();
    body["expected_version"] = json!(1);
    c.body = serde_json::to_vec(&body).unwrap();
    assert_eq!(code(&send(&h, &c).await.1), "COMMAND_BODY_INVALID");
    assert_eq!(
        owner.sent.lock().unwrap().len(),
        before,
        "nothing reached the owner"
    );

    // An answer outside the row (unknown status) leaves the outcome unknown.
    *owner.answer.lock().unwrap() = Some(
        json!({"status": "EXPLODED", "version": "2", "resource": sample_out(versioned.resource)}),
    );
    let (c, _, _) = hq_call(versioned, "staff-1004");
    assert_eq!(code(&send(&h, &c).await.1), "UNKNOWN_OUTCOME");
    // A resource missing a field too.
    let mut partial = sample_out(versioned.resource);
    partial
        .as_object_mut()
        .unwrap()
        .remove(versioned.resource[0].owner);
    *owner.answer.lock().unwrap() =
        Some(json!({"status": status_of(versioned), "version": "2", "resource": partial}));
    // (Another version: the unknown operation above still holds its slot.)
    let (mut c, _, _) = hq_call(versioned, "staff-1005");
    let mut body: Value = serde_json::from_slice(&c.body).unwrap();
    body["expected_version"] = json!(7);
    c.body = serde_json::to_vec(&body).unwrap();
    assert_eq!(code(&send(&h, &c).await.1), "UNKNOWN_OUTCOME");
}

#[test]
fn every_row_is_a_served_command_with_its_schemas() {
    let caps = catalog();
    let names = admin_bridge::contract::schemas::components();
    for row in COMMANDS {
        let cap = caps.iter().find(|c| c.id == row.id).unwrap();
        assert!(cap.served, "{}", row.id);
        assert_eq!(cap.body, Some(row.body_schema), "{}", row.id);
        assert!(
            names.contains_key(row.body_schema) && names.contains_key(row.result_schema),
            "{}",
            row.id
        );
        // Path placeholders are bridge parameters or path fields of the row.
        let mut path = row.owner_path().to_string();
        for p in &cap.path_params {
            path = path.replace(&format!("{{{}}}", p.name), "v");
        }
        for f in row.body.iter().filter(|f| f.place == Place::Path) {
            path = path.replace(&format!("{{{}}}", f.hq), "v");
        }
        assert!(
            !path.contains('{'),
            "{}: unresolved placeholder in {}",
            row.id,
            row.source
        );
    }
}
