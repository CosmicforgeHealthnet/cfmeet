//! Owner event ingestion (design §11): the optional third listener
//! (`ADMIN_BRIDGE_EVENTS_BIND_ADDRESS`, private network only) serving
//! `POST /internal/v1/hq-bridge/events`.
//!
//! * The caller is a CF Meet service with a `cf-service+jwt` for `cfmeet.admin-bridge`,
//!   verified against the bridge's trust bundle (`CF_SERVICE_TRUST_BUNDLE_FILE`), granted
//!   `hq-bridge-events.publish`. Each token is single use (the bridge's claim table, keyed by
//!   issuer and token id, shared by every replica).
//! * The producer is the authenticated service, never a body field.
//! * Body (closed): `{event_id, type, subject: {kind, id}, state, version, occurred_at}`.
//!   Ids, states, versions and times only: no personal data is ever stored.
//! * Dedupe on `(producer, event_id)`; a version not newer than the subject's last one is
//!   accepted and not appended (STALE); a type the bridge does not accept from that producer
//!   (`event-adapters.v1.json`) is quarantined: counted on the `events.inbox` projection,
//!   never appended.
//!
//! Answers: `202 {"status": "APPENDED"|"DUPLICATE"|"STALE"|"QUARANTINED", "sequence"}`;
//! `400 EVENT_INVALID`, `401 UNAUTHENTICATED`, `403 FORBIDDEN`, `404`, `405`, `413`, `503`.
//! These codes are between CF Meet services only (not part of the HQ contract).

use std::{convert::Infallible, sync::Arc};

use chrono::{DateTime, Duration, Utc};
use http::{HeaderMap, Method};
use http_body_util::{BodyExt, Full, Limited};
use hyper::{body::Incoming, Request, Response};
use hyper_util::rt::TokioIo;
use serde_json::{json, Value};
use tokio::net::TcpListener;

use cf_service_auth::{InternalPermission, ServiceTokenVerifier};

use crate::chain::Bridge;
use crate::contract::events::{ADAPTERS, MAX_EVENT_BYTES};
use crate::contract::model::Owner;
use crate::store::{Claim, ClaimKind, ClaimOutcome, IngestOutcome, OwnerEvent, EVENT_TYPES};

pub const EVENTS_PATH: &str = "/internal/v1/hq-bridge/events";
/// An event may claim to have happened at most this far in the future (clock skew).
pub const MAX_FUTURE_SECONDS: i64 = 300;

/// The verifier of owner service tokens (receiver `cfmeet.admin-bridge`). Single use is
/// enforced by the bridge's store, not the verifier's in-process guard.
pub struct EventIngress {
    verifier: Arc<ServiceTokenVerifier>,
}

impl EventIngress {
    pub fn new(verifier: ServiceTokenVerifier) -> Self {
        EventIngress {
            verifier: Arc::new(verifier.without_replay_guard()),
        }
    }
}

fn answer(status: u16, body: Value) -> (u16, Value) {
    (status, body)
}

fn refused(status: u16, code: &str) -> (u16, Value) {
    answer(status, json!({"error": {"code": code}}))
}

fn opaque_ok(v: &str) -> bool {
    !v.is_empty()
        && v.len() <= 128
        && v.as_bytes()[0].is_ascii_alphanumeric()
        && v.bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b':' | b'-'))
}

fn kind_ok(v: &str) -> bool {
    !v.is_empty()
        && v.len() <= 48
        && v.as_bytes()[0].is_ascii_lowercase()
        && v.bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_')
}

fn state_ok(v: &str) -> bool {
    !v.is_empty()
        && v.len() <= 64
        && v.as_bytes()[0].is_ascii_uppercase()
        && v.bytes()
            .all(|b| b.is_ascii_uppercase() || b.is_ascii_digit() || b == b'_')
}

fn type_ok(v: &str) -> bool {
    !v.is_empty()
        && v.len() <= 96
        && v.as_bytes()[0].is_ascii_lowercase()
        && v.bytes().all(|b| {
            b.is_ascii_lowercase() || b.is_ascii_digit() || matches!(b, b'.' | b'_' | b'-')
        })
}

/// Parses the closed body. `None`: malformed (nothing is recorded).
pub fn parse_event(producer: &str, body: &[u8], now: DateTime<Utc>) -> Option<OwnerEvent> {
    let v: Value = serde_json::from_slice(body).ok()?;
    let o = v.as_object()?;
    let fields = [
        "event_id",
        "type",
        "subject",
        "state",
        "version",
        "occurred_at",
    ];
    if o.len() != fields.len() || !fields.iter().all(|f| o.contains_key(*f)) {
        return None;
    }
    let event_id = o
        .get("event_id")?
        .as_str()
        .filter(|s| crate::chain::assertion::uuid_ok(s))
        .and_then(|s| uuid::Uuid::parse_str(s).ok())?;
    let event_type = o.get("type")?.as_str().filter(|t| type_ok(t))?.to_string();
    let subject = o.get("subject")?.as_object()?;
    if subject.len() != 2 {
        return None;
    }
    let subject_kind = subject
        .get("kind")?
        .as_str()
        .filter(|k| kind_ok(k))?
        .to_string();
    let subject_id = subject
        .get("id")?
        .as_str()
        .filter(|i| opaque_ok(i))?
        .to_string();
    let state = o
        .get("state")?
        .as_str()
        .filter(|s| state_ok(s))?
        .to_string();
    let version = o.get("version")?.as_i64().filter(|v| *v >= 1)?;
    let occurred_at = DateTime::parse_from_rfc3339(o.get("occurred_at")?.as_str()?)
        .ok()?
        .with_timezone(&Utc);
    if occurred_at > now + Duration::seconds(MAX_FUTURE_SECONDS) {
        return None;
    }
    Some(OwnerEvent {
        producer: producer.to_string(),
        event_id,
        event_type,
        subject_kind,
        subject_id,
        state,
        version,
        occurred_at,
    })
}

/// The owner name of an authenticated CF Meet service.
fn producer_of(service: &str) -> Option<Owner> {
    Owner::ALL
        .iter()
        .copied()
        .find(|o| crate::owners::service_id(*o).is_some_and(|id| id.as_str() == service))
}

/// True when `producer` may publish `event_type` (one of the six types, from its adapter's
/// source service).
pub fn accepted(producer: Owner, event_type: &str) -> bool {
    EVENT_TYPES.contains(&event_type)
        && ADAPTERS
            .iter()
            .any(|a| a.source == producer && a.export_type == event_type)
}

/// One event request, after the HTTP layer. Returns the status and JSON answer.
pub async fn ingest(
    bridge: &Bridge,
    ingress: &EventIngress,
    method: &Method,
    path: &str,
    headers: &HeaderMap,
    body: &[u8],
) -> (u16, Value) {
    if path != EVENTS_PATH {
        return refused(404, "NOT_FOUND");
    }
    if method != Method::POST {
        return refused(405, "METHOD_NOT_ALLOWED");
    }
    if !bridge.switches.enabled {
        return refused(503, "BRIDGE_DISABLED");
    }
    let ctx = match ingress.verifier.verify_request(method, path, headers).await {
        Ok(ctx) => ctx,
        Err(e) => {
            let d = &e.diagnostics;
            tracing::warn!(
                reason = e.failure.as_str(),
                issuer = d.issuer.as_ref().map(|i| i.as_str()).unwrap_or(""),
                "owner event authentication rejected"
            );
            return refused(401, "UNAUTHENTICATED");
        }
    };
    let service = ctx.authenticated_service().as_str().to_string();
    if !ctx.has_permission(InternalPermission::HqBridgeEventsPublish) {
        tracing::warn!(issuer = %service, "owner event refused: hq-bridge-events.publish not granted");
        return refused(403, "FORBIDDEN");
    }
    let Some(producer) = producer_of(&service) else {
        return refused(403, "FORBIDDEN");
    };
    // Single use, across replicas: the token id is claimed in the bridge's own store.
    let claim_value = format!("svc:{service}:{}", ctx.token_id().as_str());
    if claim_value.len() > 128 {
        return refused(401, "UNAUTHENTICATED");
    }
    let keep_until = DateTime::from_timestamp(ctx.expires_at(), 0).unwrap_or_else(Utc::now)
        + Duration::seconds(60);
    match bridge
        .store
        .claim_once(&[Claim {
            kind: ClaimKind::Jti,
            value: claim_value,
            keep_until,
        }])
        .await
    {
        Ok(ClaimOutcome::Recorded) => {}
        Ok(ClaimOutcome::Replay(_)) => {
            tracing::warn!(issuer = %service, "replayed owner event token refused");
            return refused(401, "UNAUTHENTICATED");
        }
        Err(_) => return refused(503, "UNAVAILABLE"),
    }
    let now = (bridge.clock)();
    let Some(event) = parse_event(producer.as_str(), body, now) else {
        return refused(400, "EVENT_INVALID");
    };
    let ok = accepted(producer, &event.event_type);
    match bridge.store.ingest_event(&event, ok, now).await {
        Ok(outcome) => {
            if outcome == IngestOutcome::Quarantined {
                tracing::warn!(producer = producer.as_str(), event_type = %event.event_type, "owner event quarantined");
            }
            let sequence = match outcome {
                IngestOutcome::Appended(s) => json!(s),
                _ => Value::Null,
            };
            answer(
                202,
                json!({"status": outcome.as_str(), "sequence": sequence}),
            )
        }
        Err(_) => refused(503, "UNAVAILABLE"),
    }
}

fn response(status: u16, body: &Value) -> Response<Full<bytes::Bytes>> {
    Response::builder()
        .status(status)
        .header("content-type", "application/json")
        .header("cache-control", "no-store")
        .body(Full::new(bytes::Bytes::from(
            serde_json::to_vec(body).expect("json"),
        )))
        .expect("valid response")
}

async fn serve(
    bridge: Arc<Bridge>,
    ingress: Arc<EventIngress>,
    req: Request<Incoming>,
) -> Result<Response<Full<bytes::Bytes>>, Infallible> {
    let (parts, body) = req.into_parts();
    let max = MAX_EVENT_BYTES as usize;
    let declared = parts
        .headers
        .get("content-length")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.parse::<usize>().ok());
    if declared.is_some_and(|n| n > max) {
        return Ok(response(413, &json!({"error": {"code": "TOO_LARGE"}})));
    }
    let body = match Limited::new(body, max).collect().await {
        Ok(b) => b.to_bytes(),
        Err(_) => return Ok(response(413, &json!({"error": {"code": "TOO_LARGE"}}))),
    };
    let (status, v) = ingest(
        &bridge,
        &ingress,
        &parts.method,
        parts.uri.path(),
        &parts.headers,
        &body,
    )
    .await;
    Ok(response(status, &v))
}

/// Accept loop of the owner event listener (plain HTTP on the private network; every
/// request carries a verified service token).
pub async fn run_events(bridge: Arc<Bridge>, listener: TcpListener, ingress: Arc<EventIngress>) {
    loop {
        let Ok((tcp, _)) = listener.accept().await else {
            continue;
        };
        let (bridge, ingress) = (bridge.clone(), ingress.clone());
        tokio::spawn(async move {
            let service =
                hyper::service::service_fn(move |req| serve(bridge.clone(), ingress.clone(), req));
            let _ = hyper::server::conn::http1::Builder::new()
                .max_buf_size(MAX_EVENT_BYTES as usize + 16 * 1024)
                .serve_connection(TokioIo::new(tcp), service)
                .await;
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn body(v: Value) -> Vec<u8> {
        serde_json::to_vec(&v).unwrap()
    }

    fn good() -> Value {
        json!({
            "event_id": "6f1e8c2a-3b4d-4e5f-8a6b-7c8d9e0f1a2b", "type": "verification.case.changed.v1",
            "subject": {"kind": "verification_case", "id": "case-1"}, "state": "APPROVED",
            "version": 3, "occurred_at": "2026-11-20T10:00:00Z"
        })
    }

    #[test]
    fn the_body_is_closed() {
        let now = DateTime::parse_from_rfc3339("2026-11-20T10:00:10Z")
            .unwrap()
            .with_timezone(&Utc);
        let e = parse_event("developer-platform", &body(good()), now).unwrap();
        assert_eq!((e.producer.as_str(), e.version), ("developer-platform", 3));
        for (k, v) in [
            ("email", json!("x@example.com")),
            ("version", json!(0)),
            ("state", json!("approved")),
            ("event_id", json!("not-a-uuid")),
            ("occurred_at", json!("2026-11-21T10:00:00Z")),
            ("subject", json!({"kind": "case", "id": "a", "name": "n"})),
        ] {
            let mut b = good();
            b[k] = v;
            assert!(
                parse_event("developer-platform", &body(b), now).is_none(),
                "{k}"
            );
        }
    }

    #[test]
    fn producers_publish_only_their_adapters_types() {
        assert!(accepted(
            Owner::DeveloperPlatform,
            "verification.case.changed.v1"
        ));
        assert!(accepted(Owner::Calling, "abuse.report.changed.v1"));
        assert!(!accepted(Owner::Calling, "verification.case.changed.v1"));
        assert!(!accepted(Owner::DeveloperPlatform, "unknown.thing.v1"));
        assert_eq!(producer_of("cfmeet.calling"), Some(Owner::Calling));
    }
}
