//! Platform API request accounting of Calling's DIRECT routes (P6).
//!
//! The API Gateway counts every request it forwards (accepted daily totals and normalized
//! API_OPERATION counts) into the Developer Platform's usage system
//! (`POST /internal/v1/usage/api-requests`, `developer-usage.record`). Requests that reach
//! Calling directly — `Authorization: Bearer <credential>` on Calling's own origin — never
//! pass the gateway and were uncounted. This module counts exactly those, in the same
//! format and into the same endpoint:
//!
//! * only the DIRECT authentication path is counted: a gateway-forwarded request
//!   (service token, `calling-request.forward`) was already counted by the gateway, and an
//!   organization-forwarded one (Meeting Service) is first-party traffic the gateway does
//!   not meter either — so nothing is counted twice;
//! * a request is counted once it authenticated and passed Calling's request-rate limit
//!   (the gateway likewise counts accepted requests), as one daily entry and one
//!   API_OPERATION count (domain `calling`, the contract `operationId` of the matched
//!   route, the final status class, the credential / service account and environment
//!   kind);
//! * counts accumulate in memory and are flushed every [`FLUSH_INTERVAL`] as one batch
//!   with a fresh batch id; a batch whose outcome is unknown is retried with the SAME id
//!   (the Developer Platform applies a batch at most once), and counts arriving meanwhile
//!   wait for the next batch. Counts still in memory when the process stops are lost (as
//!   at the gateway); they are bounded by one flush interval.
//!
//! On by default when the Developer Platform is a configured target
//! (`CF_DEVELOPER_PLATFORM_BASE_URL`); `CALLING_DIRECT_USAGE_ACCOUNTING=off` disables it.
//! The Developer Platform must grant Calling `developer-usage.record` in its trust bundle.
//!
//! Not done here (owner decision): ENFORCING the platform API quota on direct routes.
//! Direct routes keep Calling's own request-rate limit; the gateway's shared quota
//! counters only see direct traffic through the durable daily total it re-seeds from.

use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
    time::Duration,
};

use cf_service_auth::{
    client::{InternalCallError, InternalServiceClient},
    OutboundServiceRequest, ServiceId,
};
use chrono::{DateTime, Utc};
use serde_json::{json, Value};
use uuid::Uuid;

/// How often accumulated counts are flushed.
pub const FLUSH_INTERVAL: Duration = Duration::from_secs(30);
/// The Developer Platform's accounting endpoint (same as the gateway's).
pub const USAGE_PATH: &str = "/internal/v1/usage/api-requests";
/// Owning domain of every count (the gateway's route-manifest domain for Calling).
pub const DOMAIN: &str = "calling";
/// Environment variable of the opt-out.
pub const ENV_SWITCH: &str = "CALLING_DIRECT_USAGE_ACCOUNTING";
/// Entries per batch (the Developer Platform accepts at most 10 000).
const MAX_BATCH_ENTRIES: usize = 5_000;

/// Whether direct-route accounting is on (`on` unless the variable says `off`).
pub fn enabled_from(value: Option<&str>) -> Result<bool, String> {
    match value.map(|v| v.trim().to_ascii_lowercase()) {
        None => Ok(true),
        Some(v) if v.is_empty() || v == "on" || v == "true" => Ok(true),
        Some(v) if v == "off" || v == "false" => Ok(false),
        Some(_) => Err(format!("{ENV_SWITCH} must be on or off")),
    }
}

/// The contract operation id of a matched route: `(METHOD, /v1/x/{param})` →
/// `operationId`, from Calling's served HTTP contract.
#[derive(Debug, Clone, Default)]
pub struct OperationIndex(HashMap<(String, String), String>);

const CONTRACT: &str = include_str!("../../../../packages/contracts/calling/http/v1.openapi.json");

impl OperationIndex {
    pub fn from_contract() -> Self {
        let mut map = HashMap::new();
        let doc: Value = serde_json::from_str(CONTRACT).unwrap_or(Value::Null);
        if let Some(paths) = doc["paths"].as_object() {
            for (path, item) in paths {
                // Only customer routes are counted (never internal / provider routes).
                if !path.starts_with("/v1/") {
                    continue;
                }
                let Some(item) = item.as_object() else {
                    continue;
                };
                for (method, op) in item {
                    if let Some(id) = op["operationId"].as_str() {
                        map.insert((method.to_ascii_uppercase(), path.clone()), id.to_string());
                    }
                }
            }
        }
        Self(map)
    }

    /// The operation id of `method` on the axum route template `matched`
    /// (`/v1/calls/:call_id` → `/v1/calls/{call_id}`).
    pub fn operation(&self, method: &str, matched: &str) -> Option<&str> {
        let template: Vec<String> = matched
            .split('/')
            .map(|seg| match seg.strip_prefix(':') {
                Some(name) => format!("{{{name}}}"),
                None => seg.to_string(),
            })
            .collect();
        self.0
            .get(&(method.to_ascii_uppercase(), template.join("/")))
            .map(String::as_str)
    }

    pub fn len(&self) -> usize {
        self.0.len()
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

/// One counted direct request.
#[derive(Debug, Clone)]
pub struct DirectRequest<'a> {
    pub environment_id: Uuid,
    pub environment_kind: &'static str,
    pub credential_id: Uuid,
    pub service_account_id: Uuid,
    pub method: &'a str,
    /// The matched route template (None: not a contract route, only the daily total).
    pub matched: Option<&'a str>,
    pub status: u16,
    pub at: DateTime<Utc>,
}

type OperationKey = (Uuid, String, String, &'static str, Uuid, Uuid, &'static str);

#[derive(Debug, Default)]
struct Pending {
    entries: HashMap<(Uuid, String), u64>,
    operations: HashMap<OperationKey, u64>,
}

/// The direct-route accountant (one per process).
pub struct DirectUsageAccounting {
    client: Option<Arc<InternalServiceClient>>,
    index: OperationIndex,
    pending: Mutex<Pending>,
    /// A built batch whose outcome is not known yet (retried with the same id).
    in_flight: tokio::sync::Mutex<Option<(Uuid, Value)>>,
}

fn status_class(status: u16) -> &'static str {
    match status {
        200..=299 => "2xx",
        300..=399 => "3xx",
        400..=499 => "4xx",
        _ => "5xx",
    }
}

impl DirectUsageAccounting {
    /// An accountant flushing through `client` (None: counts are kept for inspection
    /// only, e.g. in tests).
    pub fn new(client: Option<Arc<InternalServiceClient>>) -> Self {
        Self {
            client,
            index: OperationIndex::from_contract(),
            pending: Mutex::new(Pending::default()),
            in_flight: tokio::sync::Mutex::new(None),
        }
    }

    /// Counts one accepted direct request.
    pub fn record(&self, r: &DirectRequest<'_>) {
        let day = r.at.format("%Y-%m-%d").to_string();
        let operation = r
            .matched
            .and_then(|m| self.index.operation(r.method, m))
            .map(str::to_string);
        let mut p = self.pending.lock().unwrap();
        *p.entries
            .entry((r.environment_id, day.clone()))
            .or_default() += 1;
        if let Some(op) = operation {
            *p.operations
                .entry((
                    r.environment_id,
                    day,
                    op,
                    status_class(r.status),
                    r.credential_id,
                    r.service_account_id,
                    r.environment_kind,
                ))
                .or_default() += 1;
        }
        crate::metrics::inc("calling_direct_usage_requests_total", &[]);
    }

    /// The batch body of everything pending (None when nothing is pending). Takes the
    /// counts out of the pending set.
    pub fn take_batch(&self) -> Option<(Uuid, Value)> {
        let mut p = self.pending.lock().unwrap();
        if p.entries.is_empty() && p.operations.is_empty() {
            return None;
        }
        let mut entries = Vec::new();
        for ((env, day), count) in p.entries.drain() {
            entries.push(json!({ "environmentId": env, "day": day, "count": count }));
            if entries.len() >= MAX_BATCH_ENTRIES {
                break;
            }
        }
        let mut operations = Vec::new();
        let keys: Vec<OperationKey> = p
            .operations
            .keys()
            .take(MAX_BATCH_ENTRIES.saturating_sub(entries.len()))
            .cloned()
            .collect();
        for key in keys {
            let count = p.operations.remove(&key).unwrap_or(0);
            let (env, day, op, class, credential, service_account, kind) = key;
            operations.push(json!({
                "environmentId": env, "day": day, "domain": DOMAIN, "operationId": op,
                "statusClass": class, "count": count, "credentialId": credential,
                "serviceAccountId": service_account, "environmentKind": kind,
            }));
        }
        let batch_id = Uuid::new_v4();
        Some((
            batch_id,
            json!({ "batchId": batch_id, "entries": entries, "operations": operations }),
        ))
    }

    /// One flush: the in-flight batch (if any) first, else a new one. Returns whether a
    /// batch was acknowledged.
    pub async fn flush_once(&self) -> bool {
        let Some(client) = &self.client else {
            return false;
        };
        let mut in_flight = self.in_flight.lock().await;
        if in_flight.is_none() {
            *in_flight = self.take_batch();
        }
        let Some((_, body)) = in_flight.as_ref() else {
            return false;
        };
        let request = OutboundServiceRequest::new_operation(ServiceId::developer_platform());
        let outcome = match client.post_json(&request, USAGE_PATH, body).await {
            Ok(_) => "recorded",
            // A batch the Developer Platform refuses would be refused forever: drop it
            // (counted) instead of blocking every later batch.
            Err(InternalCallError::Status {
                status: 400..=499,
                code,
                ..
            }) if code.as_deref() != Some("RATE_LIMITED") => "rejected",
            Err(_) => "retry",
        };
        crate::metrics::inc(
            "calling_direct_usage_batches_total",
            &[("outcome", outcome)],
        );
        if outcome == "retry" {
            tracing::warn!("direct-route usage batch not acknowledged; retried with the same id");
            return false;
        }
        if outcome == "rejected" {
            tracing::error!("direct-route usage batch refused by the Developer Platform; dropped");
        }
        *in_flight = None;
        outcome == "recorded"
    }

    /// Flushes every [`FLUSH_INTERVAL`], forever.
    pub async fn run(self: Arc<Self>) {
        let mut ticker = tokio::time::interval(FLUSH_INTERVAL);
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            ticker.tick().await;
            self.flush_once().await;
        }
    }

    /// Pending `(daily total, operation count)` sums (tests, readiness detail).
    pub fn pending_totals(&self) -> (u64, u64) {
        let p = self.pending.lock().unwrap();
        (p.entries.values().sum(), p.operations.values().sum())
    }

    pub fn operation_index(&self) -> &OperationIndex {
        &self.index
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_contract_index_maps_route_templates_to_operation_ids() {
        let index = OperationIndex::from_contract();
        assert!(index.len() > 50);
        assert_eq!(index.operation("POST", "/v1/calls"), Some("createCall"));
        assert_eq!(
            index.operation("GET", "/v1/calls/:call_id"),
            Some("getCall")
        );
        assert_eq!(index.operation("GET", "/internal/v1/abuse-reports"), None);
        assert_eq!(index.operation("PUT", "/v1/calls"), None);
    }

    #[test]
    fn counts_aggregate_into_one_batch_and_are_taken_once() {
        let a = DirectUsageAccounting::new(None);
        let env = Uuid::new_v4();
        let at = DateTime::parse_from_rfc3339("2026-11-18T10:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        for status in [201, 201, 404] {
            a.record(&DirectRequest {
                environment_id: env,
                environment_kind: "SANDBOX",
                credential_id: Uuid::nil(),
                service_account_id: Uuid::nil(),
                method: "POST",
                matched: Some("/v1/calls"),
                status,
                at,
            });
        }
        a.record(&DirectRequest {
            environment_id: env,
            environment_kind: "SANDBOX",
            credential_id: Uuid::nil(),
            service_account_id: Uuid::nil(),
            method: "GET",
            matched: None,
            status: 200,
            at,
        });
        assert_eq!(a.pending_totals(), (4, 3));
        let (id, body) = a.take_batch().unwrap();
        assert_eq!(body["batchId"], id.to_string());
        assert_eq!(body["entries"][0]["count"], 4);
        assert_eq!(body["entries"][0]["day"], "2026-11-18");
        let ops = body["operations"].as_array().unwrap();
        assert_eq!(ops.len(), 2);
        let created = ops.iter().find(|o| o["statusClass"] == "2xx").unwrap();
        assert_eq!(created["operationId"], "createCall");
        assert_eq!(created["domain"], "calling");
        assert_eq!(created["count"], 2);
        assert!(a.take_batch().is_none());
    }

    #[test]
    fn the_switch_is_on_unless_off() {
        assert_eq!(enabled_from(None), Ok(true));
        assert_eq!(enabled_from(Some("off")), Ok(false));
        assert_eq!(enabled_from(Some("ON")), Ok(true));
        assert!(enabled_from(Some("maybe")).is_err());
    }
}
