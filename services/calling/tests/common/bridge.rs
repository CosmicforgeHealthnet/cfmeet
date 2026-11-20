//! ADR 0068: requests as the HQ admin bridge (`cfmeet.admin-bridge`) and as other callers
//! of the operator routes.

use axum::http::Method;
use cf_service_auth::{OutboundServiceRequest, ServiceId, ServiceTokenSigner};
use serde_json::Value;
use uuid::Uuid;

use super::{Harness, TestResponse};

pub const OPERATOR: &str = "hq:staff-0001";
pub const APPROVER: &str = "hq:staff-0002";
pub const APPROVAL_REF: &str = "apr_0001";

/// How the admin bridge attributes one request.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Attribution {
    /// `X-CF-Operator` only (an R2 command, or a two-person command refused).
    Operator,
    /// `X-CF-Operator` and the second person's approval headers.
    Approved,
    /// No attribution at all.
    None,
}

/// A signed internal request by `signer` with the given attribution and key.
pub async fn signed(
    h: &Harness,
    signer: &ServiceTokenSigner,
    method: Method,
    path: &str,
    body: Option<Value>,
    attribution: Attribution,
    key: Option<Uuid>,
) -> TestResponse {
    let token = signer
        .sign(&OutboundServiceRequest::new_operation(ServiceId::calling()))
        .unwrap();
    let mut headers: Vec<(String, String)> = token
        .headers()
        .iter()
        .map(|(k, v)| (k.as_str().to_string(), v.to_str().unwrap().to_string()))
        .collect();
    if attribution != Attribution::None {
        headers.push(("x-cf-operator".into(), OPERATOR.into()));
    }
    if attribution == Attribution::Approved {
        headers.push(("x-cf-approver".into(), APPROVER.into()));
        headers.push(("x-cf-approval-ref".into(), APPROVAL_REF.into()));
        headers.push(("x-cf-intent-digest".into(), "ab".repeat(32)));
    }
    if let Some(key) = key {
        headers.push(("idempotency-key".into(), key.to_string()));
    }
    let refs: Vec<(&str, &str)> = headers
        .iter()
        .map(|(k, v)| (k.as_str(), v.as_str()))
        .collect();
    h.request_with(method, path, None, body, &refs).await
}

/// A command of the admin bridge under `key`.
pub async fn command(
    h: &Harness,
    path: &str,
    body: Value,
    attribution: Attribution,
    key: Uuid,
) -> TestResponse {
    let signer = h.peers.admin_bridge.clone();
    signed(
        h,
        &signer,
        Method::POST,
        path,
        Some(body),
        attribution,
        Some(key),
    )
    .await
}

/// The bridge's settlement read of `key`.
pub async fn operation(h: &Harness, key: Uuid) -> TestResponse {
    let signer = h.peers.admin_bridge.clone();
    signed(
        h,
        &signer,
        Method::GET,
        &format!("/internal/v1/operator/operations/{key}"),
        None,
        Attribution::Operator,
        None,
    )
    .await
}
