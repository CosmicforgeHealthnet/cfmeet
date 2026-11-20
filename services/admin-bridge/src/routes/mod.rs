//! Routes: discovery, aggregate health, datasets and projection status (A1), and the
//! owner reads by module (`organizations`, A2). Every other capability is refused before it gets here (`served == false`).

pub mod calling;
pub mod communications;
pub mod identity;
pub mod meetings;
pub mod messaging;
pub mod notifications;
pub mod organizations;
pub mod partners;
pub mod translate;

use std::collections::BTreeMap;

use chrono::{DateTime, Duration, NaiveDate, Utc};
use serde_json::{json, Value};

use crate::chain::{Answer, Authorized, Bridge};
use crate::contract::datasets::{dataset, DATASETS};
use crate::contract::model::{Kind, Owner, State};
use crate::errors::ErrorCode;

pub async fn dispatch(bridge: &Bridge, auth: &Authorized) -> Result<Answer, ErrorCode> {
    if let Some(answer) = organizations::dispatch(bridge, auth).await {
        return answer;
    }
    if let Some(answer) = notifications::dispatch(bridge, auth).await {
        return answer;
    }
    if let Some(answer) = partners::dispatch(bridge, auth).await {
        return answer;
    }
    if let Some(answer) = identity::dispatch(bridge, auth).await {
        return answer;
    }
    if let Some(answer) = calling::dispatch(bridge, auth).await {
        return answer;
    }
    if let Some(answer) = communications::dispatch(bridge, auth).await {
        return answer;
    }
    if let Some(answer) = meetings::dispatch(bridge, auth).await {
        return answer;
    }
    if let Some(answer) = messaging::dispatch(bridge, auth).await {
        return answer;
    }
    match auth.capability.id {
        "bridge.capabilities.read" => Ok(discovery(bridge)),
        "bridge.health.read" => Ok(health(bridge).await),
        "bridge.datasets.read" => datasets(bridge, auth).await,
        "hq.projections.status" => projections(bridge, auth.now).await,
        _ => Err(ErrorCode::CapabilityDisabled),
    }
}

/// Runtime state: the artifact state, lowered by the switches; never higher.
pub fn runtime_state(
    bridge: &Bridge,
    cap: &crate::contract::model::Capability,
) -> (State, Option<String>) {
    match cap.state() {
        State::ReadOnly | State::Available => {
            if let Some(flag) = cap
                .module()
                .flag()
                .filter(|_| !bridge.switches.module_on(cap.module()))
            {
                (State::Disabled, Some(format!("{flag} is off")))
            } else if cap.kind == Kind::Command && !bridge.switches.writes_enabled {
                (
                    State::Disabled,
                    Some("ADMIN_BRIDGE_WRITES_ENABLED is off".into()),
                )
            } else {
                (cap.state(), None)
            }
        }
        s => (s, cap.blocker()),
    }
}

fn discovery(bridge: &Bridge) -> Answer {
    let policy_revision = bridge
        .policy
        .read()
        .expect("policy lock")
        .as_ref()
        .map(|p| p.revision);
    let capabilities: Vec<Value> = bridge
        .routes
        .capabilities()
        .map(|c| {
            let (state, blocker) = runtime_state(bridge, c);
            json!({"id": c.id, "state": state.as_str(), "blocker": blocker})
        })
        .collect();
    Answer {
        result: json!({
            "contract_version": crate::contract::model::CONTRACT_VERSION,
            "openapi_sha256": bridge.openapi_sha256,
            "policy_revision": policy_revision,
            "capabilities": capabilities,
        }),
        freshness: "LIVE",
    }
}

// ---- Health ----------------------------------------------------------------------

/// Probes owner health URLs (GET, no redirects, 2 s timeout).
pub struct HealthProbe {
    urls: BTreeMap<Owner, String>,
    client: reqwest::Client,
}

impl HealthProbe {
    pub fn new(urls: BTreeMap<Owner, String>) -> Self {
        let client = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .no_proxy()
            .timeout(std::time::Duration::from_secs(2))
            .build()
            .expect("http client");
        HealthProbe { urls, client }
    }
}

async fn health(bridge: &Bridge) -> Answer {
    let now = (bridge.clock)();
    let mut set = tokio::task::JoinSet::new();
    for (owner, url) in &bridge.health.urls {
        let (owner, url, client) = (*owner, url.clone(), bridge.health.client.clone());
        set.spawn(async move {
            let status = match client.get(&url).send().await {
                Ok(r) if r.status().is_success() => "OK",
                Ok(r) if r.status().is_server_error() => "DOWN",
                Ok(_) => "DEGRADED",
                Err(_) => "DOWN",
            };
            (owner, status)
        });
    }
    let mut probed: BTreeMap<Owner, &'static str> = BTreeMap::new();
    while let Some(Ok((owner, status))) = set.join_next().await {
        probed.insert(owner, status);
    }
    let db_ok = bridge.store.ping().await.is_ok();
    let checked_at = now.to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
    let mut services = vec![json!({
        "service": Owner::AdminBridge.as_str(),
        "status": if db_ok { "OK" } else { "DOWN" },
        "checked_at": checked_at,
    })];
    let mut degraded = false;
    // Only probed owners are listed: an owner without a probe is omitted, never OK.
    for owner in Owner::ALL
        .iter()
        .filter(|o| bridge.health.urls.contains_key(o))
    {
        let status = probed.get(owner).copied().unwrap_or("UNKNOWN");
        degraded |= status != "OK";
        services
            .push(json!({"service": owner.as_str(), "status": status, "checked_at": checked_at}));
    }
    let status = if !db_ok {
        "DOWN"
    } else if degraded {
        "DEGRADED"
    } else if bridge.health.urls.is_empty() {
        "UNKNOWN"
    } else {
        "OK"
    };
    Answer {
        result: json!({"as_of": checked_at, "status": status, "services": services}),
        freshness: "LIVE",
    }
}

// ---- Datasets --------------------------------------------------------------------

fn day_cursor(d: NaiveDate) -> String {
    d.format("%Y-%m-%d").to_string()
}

async fn datasets(bridge: &Bridge, auth: &Authorized) -> Result<Answer, ErrorCode> {
    let id = auth
        .params
        .get("dataset_id")
        .map(String::as_str)
        .unwrap_or_default();
    let ds = dataset(id).ok_or(ErrorCode::DatasetNotFound)?;
    let q = &auth.query;
    let (start, end) = (
        q.start.ok_or(ErrorCode::QueryInvalid)?,
        q.end.ok_or(ErrorCode::QueryInvalid)?,
    );
    let first = start.date_naive();
    // The window is [start, end): a day is in it when it starts before `end`.
    let last = (end - Duration::nanoseconds(1)).date_naive();
    let from = match &q.cursor {
        None => first,
        Some(c) => {
            let d =
                NaiveDate::parse_from_str(c, "%Y-%m-%d").map_err(|_| ErrorCode::QueryInvalid)?;
            if d < first || d > last {
                return Err(ErrorCode::QueryInvalid);
            }
            d
        }
    };
    let limit = q.limit.unwrap_or(31);
    let mut days = Vec::new();
    let mut d = from;
    while d <= last && days.len() < limit as usize {
        days.push(d);
        d += Duration::days(1);
    }
    let next_cursor = (d <= last).then(|| day_cursor(d));
    let page_end = *days.last().unwrap_or(&from);
    let rows = bridge
        .store
        .dataset_days(ds.id, from, page_end, limit)
        .await
        .map_err(|_| ErrorCode::BridgeUnavailable)?;
    let expected: Vec<String> = ds
        .sources
        .iter()
        .map(|s| format!("{}:{}", s.owner.as_str(), s.via))
        .collect();
    let (mut complete, mut any) = (0usize, false);
    let series: Vec<Value> = days
        .iter()
        .map(|day| {
            let row = rows.iter().find(|r| r.day == *day);
            let reported = row
                .map(|r| r.sources_reported.as_slice())
                .unwrap_or_default();
            if expected.iter().all(|e| reported.contains(e)) {
                complete += 1;
            }
            any |= !reported.is_empty();
            let metrics: serde_json::Map<String, Value> = ds
                .metrics
                .iter()
                .map(|m| {
                    let v = row
                        .and_then(|r| r.metrics.get(*m))
                        .filter(|v| v.as_i64().is_some_and(|n| n >= 0))
                        .cloned()
                        .unwrap_or(Value::Null);
                    (m.to_string(), v)
                })
                .collect();
            json!({"date": day_cursor(*day), "metrics": metrics})
        })
        .collect();
    let (coverage, freshness) = if !days.is_empty() && complete == days.len() {
        ("COMPLETE", "RECENT")
    } else if any {
        ("PARTIAL", "PARTIAL")
    } else {
        ("NONE", "UNAVAILABLE")
    };
    let fmt = |t: DateTime<Utc>| t.to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
    Ok(Answer {
        result: json!({
            "dataset_id": ds.id,
            "classification": "INTERNAL",
            "coverage": coverage,
            "window": {"start": fmt(start), "end": fmt(end)},
            "series": series,
            "next_cursor": next_cursor,
        }),
        freshness,
    })
}

// ---- Projections -----------------------------------------------------------------

pub const EVENT_INBOX_PROJECTION: &str = "events.inbox";

async fn projections(bridge: &Bridge, now: DateTime<Utc>) -> Result<Answer, ErrorCode> {
    let rows = bridge
        .store
        .projections()
        .await
        .map_err(|_| ErrorCode::BridgeUnavailable)?;
    let names = DATASETS
        .iter()
        .map(|d| d.id)
        .chain(std::iter::once(EVENT_INBOX_PROJECTION));
    let rank = |f: &str| {
        [
            "LIVE",
            "RECENT",
            "STALE",
            "PARTIAL",
            "UNKNOWN",
            "UNAVAILABLE",
        ]
        .iter()
        .position(|x| *x == f)
        .unwrap_or(5)
    };
    let mut worst = "LIVE";
    let list: Vec<Value> = names
        .map(|name| match rows.iter().find(|r| r.name == name) {
            Some(r) if r.last_event_at.is_some() => {
                let last = r.last_event_at.expect("checked");
                let lag = (now - last).num_seconds().max(0);
                let freshness = if lag < 300 {
                    "LIVE"
                } else if lag < 3600 {
                    "RECENT"
                } else {
                    "STALE"
                };
                if rank(freshness) > rank(worst) {
                    worst = freshness;
                }
                json!({
                    "name": name, "freshness": freshness,
                    "coverage": if r.open_gaps == 0 { "COMPLETE" } else { "PARTIAL" },
                    "last_event_at": last.to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
                    "lag_seconds": lag, "open_gaps": r.open_gaps, "quarantined": r.quarantined,
                })
            }
            _ => {
                worst = "UNAVAILABLE";
                json!({"name": name, "freshness": "UNAVAILABLE", "coverage": "NONE", "last_event_at": null, "lag_seconds": null, "open_gaps": null, "quarantined": null})
            }
        })
        .collect();
    Ok(Answer {
        result: json!({"projections": list}),
        freshness: worst,
    })
}
