//! Strict extraction from owner JSON into the bridge's closed result shapes. A missing
//! or mistyped field fails the whole answer (`OwnerError::Contract`): HQ never gets a
//! partial or guessed value.

use chrono::{DateTime, SecondsFormat, Utc};
use serde_json::{Map, Value};

use crate::owners::OwnerError;

pub type T<V> = Result<V, OwnerError>;

fn field<'a>(v: &'a Value, k: &str) -> T<&'a Value> {
    v.get(k).ok_or(OwnerError::Contract)
}

pub fn string(v: &Value, k: &str) -> T<Value> {
    field(v, k)?
        .as_str()
        .map(|s| Value::String(s.to_string()))
        .ok_or(OwnerError::Contract)
}

pub fn opt_string(v: &Value, k: &str) -> T<Value> {
    match v.get(k) {
        None | Some(Value::Null) => Ok(Value::Null),
        Some(Value::String(s)) => Ok(Value::String(s.clone())),
        _ => Err(OwnerError::Contract),
    }
}

pub fn uuid(v: &Value, k: &str) -> T<Value> {
    let s = field(v, k)?.as_str().ok_or(OwnerError::Contract)?;
    uuid::Uuid::parse_str(s)
        .map(|u| Value::String(u.hyphenated().to_string()))
        .map_err(|_| OwnerError::Contract)
}

pub fn opt_uuid(v: &Value, k: &str) -> T<Value> {
    match v.get(k) {
        None | Some(Value::Null) => Ok(Value::Null),
        Some(_) => uuid(v, k),
    }
}

pub fn int(v: &Value, k: &str) -> T<Value> {
    field(v, k)?
        .as_i64()
        .map(Value::from)
        .ok_or(OwnerError::Contract)
}

pub fn opt_int(v: &Value, k: &str) -> T<Value> {
    match v.get(k) {
        None | Some(Value::Null) => Ok(Value::Null),
        Some(x) => x.as_i64().map(Value::from).ok_or(OwnerError::Contract),
    }
}

pub fn boolean(v: &Value, k: &str) -> T<Value> {
    field(v, k)?
        .as_bool()
        .map(Value::Bool)
        .ok_or(OwnerError::Contract)
}

fn normalize_time(s: &str) -> T<Value> {
    DateTime::parse_from_rfc3339(s)
        .map(|t| {
            Value::String(
                t.with_timezone(&Utc)
                    .to_rfc3339_opts(SecondsFormat::Secs, true),
            )
        })
        .map_err(|_| OwnerError::Contract)
}

pub fn time(v: &Value, k: &str) -> T<Value> {
    normalize_time(field(v, k)?.as_str().ok_or(OwnerError::Contract)?)
}

pub fn opt_time(v: &Value, k: &str) -> T<Value> {
    match v.get(k) {
        None | Some(Value::Null) => Ok(Value::Null),
        Some(Value::String(s)) => normalize_time(s),
        _ => Err(OwnerError::Contract),
    }
}

/// `{items, nextCursor}` owner page -> (translated items, bridge `next_cursor`).
pub fn page(v: &Value, item: impl Fn(&Value) -> T<Value>) -> T<(Vec<Value>, Value)> {
    let items = field(v, "items")?.as_array().ok_or(OwnerError::Contract)?;
    if items.len() > 100 {
        return Err(OwnerError::Contract);
    }
    let out = items.iter().map(item).collect::<T<Vec<_>>>()?;
    let next = match v.get("nextCursor") {
        None | Some(Value::Null) => Value::Null,
        Some(Value::String(s)) => Value::String(s.clone()),
        Some(Value::Number(n)) => Value::String(n.to_string()),
        _ => return Err(OwnerError::Contract),
    };
    Ok((out, next))
}

pub fn obj(pairs: Vec<(&str, Value)>) -> Value {
    Value::Object(
        pairs
            .into_iter()
            .map(|(k, v)| (k.to_string(), v))
            .collect::<Map<_, _>>(),
    )
}
