//! Runs the owner commands of `contract::owner_commands` through the command core: the
//! body is checked against the row (snake_case in, camelCase out), the owner path is built
//! from the bridge path parameters, and the owner's `{status, version, resource}` answer is
//! re-extracted strictly (an unknown status, a malformed version or a resource outside the
//! row's fields leaves the outcome unknown, settled from the owner's record).

use serde_json::{Map, Value};

use super::commands::{self, Applied, CommandSpec};
use super::translate as t;
use crate::chain::{Answer, Authorized, Bridge};
use crate::contract::owner_commands::{command, In, Out, OutField, OwnerCommand, Place, Reason};
use crate::errors::ErrorCode;
use crate::owners::OwnerError;

pub async fn dispatch(bridge: &Bridge, auth: &Authorized) -> Option<Result<Answer, ErrorCode>> {
    let spec = command(auth.capability.id)?;
    if !auth.capability.served {
        return None;
    }
    Some(run(bridge, auth, spec).await)
}

fn valid(kind: In, v: &Value) -> bool {
    match kind {
        In::Enum(allowed) => v.as_str().is_some_and(|s| allowed.contains(&s)),
        In::Bool => v.is_boolean(),
        In::Int(min, max) => v.as_i64().is_some_and(|n| (min..=max).contains(&n)),
        In::Uuid => v
            .as_str()
            .and_then(|s| {
                uuid::Uuid::parse_str(s)
                    .ok()
                    .map(|u| u.hyphenated().to_string() == s)
            })
            .unwrap_or(false),
        In::NullableUuid => v.is_null() || valid(In::Uuid, v),
        In::Code => v.as_str().is_some_and(|s| {
            (1..=64).contains(&s.len())
                && s.as_bytes()[0].is_ascii_uppercase()
                && s.bytes()
                    .all(|b| b.is_ascii_uppercase() || b.is_ascii_digit() || b == b'_')
        }),
        In::Str(min, max) => v.as_str().is_some_and(|s| {
            let n = s.chars().count();
            (min..=max).contains(&n) && !s.chars().any(char::is_control)
        }),
        In::NullableStr(min, max) => v.is_null() || valid(In::Str(min, max), v),
        In::UuidList(max) => v.as_array().is_some_and(|a| {
            a.len() <= max
                && a.iter().all(|x| valid(In::Uuid, x))
                && a.iter().collect::<std::collections::HashSet<_>>().len() == a.len()
        }),
        In::Object => v.is_object(),
        In::EnumList(values, max) => v.as_array().is_some_and(|a| {
            a.len() <= max
                && a.iter()
                    .all(|x| x.as_str().is_some_and(|s| values.contains(&s)))
                && a.iter().collect::<std::collections::HashSet<_>>().len() == a.len()
        }),
        In::Time => v.as_str().is_some_and(|s| {
            s.len() <= 40 && s.ends_with('Z') && chrono::DateTime::parse_from_rfc3339(s).is_ok()
        }),
        In::Date => v.as_str().is_some_and(|s| {
            s.len() == 10 && chrono::NaiveDate::parse_from_str(s, "%Y-%m-%d").is_ok()
        }),
        In::NullableObject => v.is_null() || v.is_object(),
    }
}

/// HQ's body → (ledger version, owner body in camelCase, path values, target parts),
/// validated against the row.
pub struct Mapped {
    pub version: String,
    pub body: Value,
    pub path: Vec<(&'static str, String)>,
    pub target: Vec<String>,
}

/// `tenant`: the request's tenant scope (filled into [`Place::Tenant`] fields).
pub fn owner_body(spec: &OwnerCommand, body: &Value, tenant: &str) -> Result<Mapped, ErrorCode> {
    let bad = ErrorCode::CommandBodyInvalid;
    let mut out = Map::new();
    let version = match spec.version {
        Some(min) => {
            let v = body
                .get("expected_version")
                .and_then(Value::as_i64)
                .filter(|v| *v >= min)
                .ok_or(bad)?;
            out.insert("expectedVersion".into(), Value::from(v));
            v.to_string()
        }
        None => {
            if body.get("expected_version").is_some() {
                return Err(bad);
            }
            "0".to_string()
        }
    };
    let (mut path, mut target) = (Vec::new(), Vec::new());
    for f in spec.body {
        if let Place::Fixed(value) = f.place {
            if body.get(f.hq).is_some() {
                return Err(bad);
            }
            out.insert(f.owner.into(), Value::from(value));
            continue;
        }
        if f.place == Place::Tenant {
            if body.get(f.hq).is_some() {
                return Err(bad);
            }
            // A tenant scope that is not a UUID names no owner tenant.
            let t = uuid::Uuid::parse_str(tenant)
                .map(|u| u.hyphenated().to_string())
                .map_err(|_| ErrorCode::ResourceNotFound)?;
            out.insert(f.owner.into(), Value::from(t.clone()));
            target.push(t);
            continue;
        }
        match body.get(f.hq) {
            None if f.required => return Err(bad),
            None => {}
            Some(v) if valid(f.kind, v) => match f.place {
                Place::Body => {
                    out.insert(f.owner.into(), v.clone());
                }
                Place::Path => {
                    let s = v.as_str().filter(|s| string_ok(s)).ok_or(bad)?;
                    path.push((f.hq, s.to_string()));
                    target.push(s.to_string());
                }
                Place::BodyAndTarget => {
                    out.insert(f.owner.into(), v.clone());
                    target.push(
                        v.as_str()
                            .map(str::to_string)
                            .unwrap_or_else(|| v.to_string()),
                    );
                }
                Place::Fixed(_) | Place::Tenant => {
                    unreachable!("bridge-filled values are handled above")
                }
            },
            Some(_) => return Err(bad),
        }
    }
    match (spec.reason, body.get("reason_code")) {
        (Reason::Required, None | Some(Value::Null)) => return Err(bad),
        (Reason::Forward | Reason::Required, Some(r)) if !r.is_null() => {
            out.insert("reasonCode".into(), r.clone());
        }
        _ => {}
    }
    Ok(Mapped {
        version,
        body: Value::Object(out),
        path,
        target,
    })
}

fn string_ok(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 128
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b':' | b'@' | b'-'))
}

fn extract(fields: &[OutField], v: &Value) -> Result<Value, OwnerError> {
    let mut out = Map::new();
    for f in fields {
        let raw = v.get(f.owner);
        let value = match raw {
            None | Some(Value::Null) if f.nullable => Value::Null,
            None | Some(Value::Null) => return Err(OwnerError::Contract),
            Some(x) => match f.kind {
                Out::Str => match x.as_str() {
                    Some(s) if string_ok(s) => x.clone(),
                    _ => return Err(OwnerError::Contract),
                },
                Out::Code => {
                    if valid(In::Code, x) {
                        x.clone()
                    } else {
                        return Err(OwnerError::Contract);
                    }
                }
                Out::Uuid => t::uuid(v, f.owner)?,
                Out::Int => t::int(v, f.owner)?,
                Out::Bool => t::boolean(v, f.owner)?,
                Out::Time => t::time(v, f.owner)?,
                Out::Text => match x.as_str() {
                    Some(s) if s.chars().count() <= 500 && !s.chars().any(char::is_control) => {
                        x.clone()
                    }
                    _ => return Err(OwnerError::Contract),
                },
                Out::StrList => match x.as_array() {
                    Some(a)
                        if a.len() <= 64 && a.iter().all(|i| i.as_str().is_some_and(string_ok)) =>
                    {
                        x.clone()
                    }
                    _ => return Err(OwnerError::Contract),
                },
                Out::Obj(inner) => extract(inner, x)?,
                Out::ObjList(inner) => match x.as_array() {
                    Some(a) if a.len() <= 100 => Value::Array(
                        a.iter()
                            .map(|i| extract(inner, i))
                            .collect::<Result<Vec<_>, _>>()?,
                    ),
                    _ => return Err(OwnerError::Contract),
                },
            },
        };
        out.insert(f.hq.into(), value);
    }
    Ok(Value::Object(out))
}

/// The owner's `{status, version, resource}` answer, strictly.
pub fn applied(spec: &OwnerCommand, v: &Value) -> Option<Applied> {
    let status = v.get("status")?.as_str()?;
    let known = if spec.statuses.is_empty() {
        valid(In::Code, &Value::from(status))
    } else {
        spec.statuses.contains(&status)
    };
    if !known {
        return None;
    }
    let version = match v.get("version") {
        Some(Value::String(s))
            if !s.is_empty() && s.len() <= 19 && s.bytes().all(|b| b.is_ascii_digit()) =>
        {
            Some(s.clone())
        }
        None | Some(Value::Null) => None,
        _ => return None,
    };
    let view = extract(spec.resource, v.get("resource")?).ok()?;
    Some(Applied {
        source_status: status.to_string(),
        source_version: version,
        view,
    })
}

async fn run(
    bridge: &Bridge,
    auth: &Authorized,
    spec: &'static OwnerCommand,
) -> Result<Answer, ErrorCode> {
    let extra: Vec<&str> = spec
        .body
        .iter()
        .filter(|f| !matches!(f.place, Place::Fixed(_) | Place::Tenant))
        .map(|f| f.hq)
        .collect();
    let (base, body) = commands::parse_command_body(&auth.body, &extra, spec.version.is_some())?;
    let mapped = owner_body(spec, &body, &auth.query.tenant)?;
    let mut path = spec.owner_path().to_string();
    let mut target = format!("{}:{}", spec.owner.as_str(), spec.id);
    for (name, value) in &auth.params {
        path = path.replace(&format!("{{{name}}}"), value);
        target.push(':');
        target.push_str(value);
    }
    for (name, value) in &mapped.path {
        path = path.replace(&format!("{{{name}}}"), value);
    }
    for part in &mapped.target {
        target.push(':');
        target.push_str(part);
    }
    if path.contains('{') {
        return Err(ErrorCode::Internal);
    }
    if spec.version.is_none() {
        // Every unversioned command is its own slot.
        target.push(':');
        target.push_str(&base.operation_id.to_string());
    }
    commands::execute(
        bridge,
        auth,
        CommandSpec {
            owner: spec.owner,
            owner_path: path,
            owner_body: mapped.body,
            target,
            expected_version: mapped.version,
            operation_id: base.operation_id,
        },
        move |v: &Value| applied(spec, v),
    )
    .await
}
