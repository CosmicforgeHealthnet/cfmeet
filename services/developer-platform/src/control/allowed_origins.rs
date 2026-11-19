//! ADR 0062 — allowed websites (exact origins) of an environment (build contract §2.3).
//!
//! Platform Access holds the list; the Developer Console reads and replaces it through
//! the delegation API (`platform-identity.delegate`, actor header). The API Gateway and
//! Messaging read it from Platform Access's change feed.
//!
//! * Every member reads (`AllowedOriginsRead`; machine credentials: `projects.read`);
//!   OWNER / ADMIN replace (`AllowedOriginsManage`; machine: `credentials.manage`).
//! * Rules (checked here first, then by Platform Access): at most 20 origins, each an
//!   exact `scheme://host[:port]` with a lowercase host, no path, no wildcard, no
//!   explicit default port; `https` only, except `http://localhost[:port]` and
//!   `http://127.0.0.1[:port]` in SANDBOX / TEST environments.
//!
//! Flag `DEVELOPER_PLATFORM_ALLOWED_ORIGINS_ENABLED` (off: 404).

use sea_orm::TransactionTrait;
use serde::Deserialize;
use serde_json::{json, Value};
use uuid::Uuid;

use super::{
    activity::{self, Event},
    authorize,
    service_accounts::environment,
    users::console_delegate,
    Caller,
};
use crate::{
    domain::rbac::Action,
    http::error::{ApiError, ErrorCode},
    platform_access::DelegationCall,
    state::AppState,
};

pub const MAX_ORIGINS: usize = 20;

fn enabled(state: &AppState) -> Result<(), ApiError> {
    state
        .build
        .allowed_origins
        .then_some(())
        .ok_or(ApiError(ErrorCode::NotFound))
}

fn valid_host(host: &str) -> bool {
    (1..=253).contains(&host.len())
        && host.split('.').all(|label| {
            let b = label.as_bytes();
            (1..=63).contains(&b.len())
                && b.iter()
                    .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || *c == b'-')
                && b[0] != b'-'
                && b[b.len() - 1] != b'-'
        })
}

/// One origin, exactly as a browser sends it in `Origin`.
pub fn origin(value: &str, environment_kind: &str) -> Result<String, ApiError> {
    let invalid = ApiError(ErrorCode::ValidationError);
    let (scheme, rest) = value.split_once("://").ok_or(invalid)?;
    let (host, port) = match rest.rsplit_once(':') {
        Some((h, p)) => (h, Some(p)),
        None => (rest, None),
    };
    let port_ok = port.is_none_or(|p| {
        (1..=5).contains(&p.len())
            && p.bytes().all(|b| b.is_ascii_digit())
            && !p.starts_with('0')
            && p.parse::<u32>().is_ok_and(|n| (1..=65_535).contains(&n))
    });
    if !valid_host(host) || !port_ok {
        return Err(invalid);
    }
    let local = matches!(host, "localhost" | "127.0.0.1");
    let ok = match scheme {
        "https" => port != Some("443"),
        "http" => local && environment_kind != "PRODUCTION" && port != Some("80"),
        _ => false,
    };
    if ok {
        Ok(value.to_string())
    } else {
        Err(invalid)
    }
}

/// The validated list (duplicates removed, order kept).
pub fn origins(values: &[String], environment_kind: &str) -> Result<Vec<String>, ApiError> {
    if values.len() > MAX_ORIGINS {
        return Err(ApiError(ErrorCode::ValidationError));
    }
    let mut out: Vec<String> = Vec::new();
    for v in values {
        let o = origin(v, environment_kind)?;
        if !out.contains(&o) {
            out.push(o);
        }
    }
    Ok(out)
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PutAllowedOrigins {
    pub origins: Vec<String>,
}

fn view(pa: &Value, environment_id: Uuid) -> Result<Value, ApiError> {
    let consistent = pa
        .get("environmentId")
        .and_then(Value::as_str)
        .is_none_or(|e| Uuid::parse_str(e).ok() == Some(environment_id));
    let origins = pa.get("origins").and_then(Value::as_array).and_then(|o| {
        o.iter()
            .map(|v| v.as_str().map(str::to_string))
            .collect::<Option<Vec<_>>>()
    });
    match (consistent, origins) {
        (true, Some(origins)) => Ok(json!({ "environmentId": environment_id, "origins": origins })),
        _ => {
            tracing::warn!("platform access returned an inconsistent allowed-origins view");
            Err(ApiError(ErrorCode::ServiceUnavailable))
        }
    }
}

/// `GET /v1/environments/{environmentId}/allowed-origins`.
pub async fn get(
    state: &AppState,
    caller: &Caller,
    environment_id: Uuid,
) -> Result<Value, ApiError> {
    enabled(state)?;
    let env = environment(&state.db, environment_id).await?;
    authorize(state, caller, env.tenant_id, Action::AllowedOriginsRead).await?;
    let pa = console_delegate(
        state,
        caller,
        DelegationCall::GetAllowedOrigins { environment_id },
    )
    .await?;
    view(&pa, environment_id)
}

/// `PUT /v1/environments/{environmentId}/allowed-origins` — replaces the whole list.
pub async fn put(
    state: &AppState,
    caller: &Caller,
    environment_id: Uuid,
    body: PutAllowedOrigins,
) -> Result<Value, ApiError> {
    enabled(state)?;
    let env = environment(&state.db, environment_id).await?;
    authorize(state, caller, env.tenant_id, Action::AllowedOriginsManage).await?;
    let list = origins(&body.origins, &env.kind)?;
    let pa = console_delegate(
        state,
        caller,
        DelegationCall::PutAllowedOrigins {
            actor: caller.delegation_actor(),
            environment_id,
            origins: list.clone(),
        },
    )
    .await?;
    let v = view(&pa, environment_id)?;
    let txn = state.db.begin().await?;
    activity::record(
        &txn,
        caller,
        Event {
            tenant_id: env.tenant_id,
            action: "allowed_origins.updated",
            target_type: "ENVIRONMENT",
            target_id: environment_id.to_string(),
            summary: json!({
                "projectId": env.project_id, "environmentId": environment_id,
                "originCount": list.len(),
            }),
        },
    )
    .await?;
    txn.commit().await?;
    Ok(v)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn origins_are_exact_and_https_except_local_development() {
        for ok in [
            "https://app.example.com",
            "https://app.example.com:8443",
            "https://a-b.example",
        ] {
            assert!(origin(ok, "PRODUCTION").is_ok(), "{ok}");
        }
        for ok in [
            "http://localhost",
            "http://localhost:3000",
            "http://127.0.0.1:5173",
        ] {
            assert!(origin(ok, "SANDBOX").is_ok(), "{ok}");
            assert!(origin(ok, "TEST").is_ok(), "{ok}");
            assert!(origin(ok, "PRODUCTION").is_err(), "{ok}");
        }
        for bad in [
            "app.example.com",
            "http://app.example.com",
            "https://App.Example.com",
            "https://app.example.com/",
            "https://app.example.com/path",
            "https://*.example.com",
            "https://app.example.com:443",
            "https://app.example.com:0",
            "https://app.example.com:65536",
            "https://app.example.com:080",
            "https://-bad.example.com",
            "https://user@app.example.com",
            "https://app..example.com",
            "ftp://app.example.com",
            "http://localhost:80",
            "https://app.example.com?x=1",
        ] {
            assert!(origin(bad, "SANDBOX").is_err(), "{bad}");
        }
        let many: Vec<String> = (0..21)
            .map(|i| format!("https://a{i}.example.com"))
            .collect();
        assert!(origins(&many, "TEST").is_err());
        let dup = vec![
            "https://a.example".to_string(),
            "https://a.example".to_string(),
        ];
        assert_eq!(origins(&dup, "TEST").unwrap(), ["https://a.example"]);
    }
}
