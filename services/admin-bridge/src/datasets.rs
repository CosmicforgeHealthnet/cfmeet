//! The dataset collector (design §11 / §17.3): fills `admin_dataset_days`, which
//! `bridge.datasets.read` serves. Once per maintenance tick, for yesterday and each day of
//! the last 31 that a source has not reported yet:
//!
//! * **pull sources** — `GET /internal/v1/operator/datasets/{datasetId}/days/{yyyy-mm-dd}`
//!   on the owner, attributed to `hq:dataset-sync` (counts only, one UTC day);
//! * **event sources** — counts from the HQ feed: each subject's latest state as of the
//!   end of the day, per producer;
//! * **the bridge itself** — quarantined owner events of the day and the event inbox's
//!   open projection gaps (as observed when yesterday is collected).
//!
//! Each source merges its own metrics into the day and joins `sources_reported`
//! (`<service>:<via>`, as `contract::datasets` lists them) only when it could compute
//! every metric it owns: a metric nobody reported stays null, never a guessed 0, and the
//! read's coverage (COMPLETE / PARTIAL / NONE) follows. Merges are single idempotent
//! upserts, so replicas collecting at the same time are harmless; owner calls per tick are
//! bounded ([`MAX_PULLS_PER_TICK`]).

use chrono::{DateTime, Duration, NaiveDate, Utc};
use serde_json::{Map, Value};

use crate::chain::Bridge;
use crate::contract::datasets::{Dataset, DATASETS};
use crate::contract::model::Owner;
use crate::store::{DatasetDay, EventStateCount};

/// The operator the bridge names on its own dataset pulls.
pub const SYNC_OPERATOR: &str = "hq:dataset-sync";
/// Days looked back (yesterday included).
pub const LOOKBACK_DAYS: i64 = 31;
/// Owner calls per tick, across every pull source.
pub const MAX_PULLS_PER_TICK: usize = 64;
/// Missing days before yesterday one pull source fetches per tick (newest first).
pub const MAX_BACKFILL_PER_PULL: usize = 6;
/// Missing days recomputed per event source per tick (newest first).
pub const MAX_EVENT_DAYS_PER_TICK: usize = 8;

/// An owner's daily pull that exists (the owner serves the route): the dataset and the
/// metrics it reports. Pulls listed in `contract::datasets` but not here are not served by
/// their owner yet (their source never reports: coverage stays PARTIAL / NONE).
pub struct Pull {
    pub dataset: &'static str,
    pub owner: Owner,
    pub metrics: &'static [&'static str],
    /// Metrics the owner documents as not computable (always null): they do not keep the
    /// source from reporting.
    pub unmetered: &'static [&'static str],
}

pub const PULLS: &[Pull] = &[
    Pull {
        dataset: "cfmeet.engagement_daily",
        owner: Owner::MeetingService,
        metrics: &["dau", "mau", "signups"],
        unmetered: &[],
    },
    Pull {
        dataset: "cfmeet.meetings_daily",
        owner: Owner::MeetingService,
        metrics: &["meetings", "participant_minutes", "peak_live_meetings"],
        unmetered: &[],
    },
    Pull {
        dataset: "cfmeet.operations_health",
        owner: Owner::Notifications,
        metrics: &["failed_deliveries"],
        unmetered: &[],
    },
    Pull {
        dataset: "cfmeet.operations_health",
        owner: Owner::Messaging,
        metrics: &["failed_messages"],
        unmetered: &[],
    },
    Pull {
        dataset: "cfmeet.messaging_daily",
        owner: Owner::Messaging,
        metrics: &["messages_sent"],
        unmetered: &[],
    },
    Pull {
        dataset: "cfmeet.partners_daily",
        owner: Owner::DeveloperPlatform,
        metrics: &[
            "workspaces_active",
            "workspaces_restricted",
            "workspaces_suspended",
            "partners_live",
            "partner_mau",
        ],
        // Partner end users are not metered by the Developer Platform.
        unmetered: &["partner_mau"],
    },
    Pull {
        dataset: "cfmeet.apps_daily",
        owner: Owner::DeveloperPlatform,
        metrics: &[
            "apps_draft",
            "apps_in_review",
            "apps_published",
            "apps_suspended",
            "installs",
        ],
        unmetered: &[],
    },
];

/// The bridge's own source in `cfmeet.operations_health`.
const BRIDGE_METRICS: &[&str] = &["projection_gaps", "quarantined_events"];

/// What one tick did.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct CollectStats {
    pub merged: usize,
    pub pulls: usize,
}

fn source(owner: Owner, via: &str) -> String {
    format!("{}:{via}", owner.as_str())
}

fn day_start(d: NaiveDate) -> DateTime<Utc> {
    d.and_hms_opt(0, 0, 0).expect("midnight").and_utc()
}

fn count(states: &[EventStateCount], producer: Owner, wanted: &[&str]) -> i64 {
    states
        .iter()
        .filter(|s| s.producer == producer.as_str() && wanted.contains(&s.state.as_str()))
        .map(|s| s.subjects)
        .sum()
}

/// The metrics an event source owns, from the feed's latest states as of a day's end.
/// `None`: this (dataset, producer, type) yields no metric.
fn event_metrics(
    dataset: &str,
    producer: Owner,
    states: &[EventStateCount],
) -> Option<Vec<(&'static str, i64)>> {
    let c = |wanted: &[&str]| count(states, producer, wanted);
    Some(match dataset {
        "cfmeet.verification_queue" => vec![
            ("pending", c(&["OPEN"])),
            ("in_review", c(&["CLAIMED"])),
            ("approved", c(&["APPROVED", "PUBLISHED"])),
            ("rejected", c(&["REJECTED"])),
        ],
        "cfmeet.trust_safety_queue" => {
            let metric = match producer {
                Owner::Messaging => "open_messaging",
                Owner::Calling => "open_calling",
                Owner::Communications => "open_communications",
                Owner::MeetingService => "open_meetings",
                _ => return None,
            };
            vec![(metric, c(&["OPEN", "IN_REVIEW", "CLAIMED"]))]
        }
        "cfmeet.partners_daily" => vec![
            ("workspaces_active", c(&["ACTIVE"])),
            ("workspaces_restricted", c(&["RESTRICTED"])),
            ("workspaces_suspended", c(&["SUSPENDED"])),
        ],
        "cfmeet.apps_daily" => vec![
            ("apps_draft", c(&["DRAFT"])),
            ("apps_in_review", c(&["SUBMITTED", "IN_REVIEW"])),
            ("apps_published", c(&["PUBLISHED"])),
            ("apps_suspended", c(&["SUSPENDED"])),
        ],
        // Summed over producers by the caller; an immutable record (e.g. a partner
        // incident RECORDED) is not an open incident.
        "cfmeet.operations_health" => vec![(
            "open_incidents",
            c(&["OPEN", "ACKNOWLEDGED", "INVESTIGATING", "MITIGATING"]),
        )],
        _ => return None,
    })
}

fn missing(rows: &[DatasetDay], day: NaiveDate, src: &str) -> bool {
    !rows
        .iter()
        .find(|r| r.day == day)
        .is_some_and(|r| r.sources_reported.iter().any(|s| s == src))
}

/// The owner's day answer `{datasetId, date, metrics}`, strictly: every metric the pull
/// owns, an integer >= 0 or null. `None`: outside the contract.
pub fn pulled(pull: &Pull, day: NaiveDate, v: &Value) -> Option<(Map<String, Value>, bool)> {
    if v.get("datasetId")?.as_str()? != pull.dataset
        || v.get("date")?.as_str()? != day.format("%Y-%m-%d").to_string()
    {
        return None;
    }
    let reported = v.get("metrics")?.as_object()?;
    let mut out = Map::new();
    let mut complete = true;
    for m in pull.metrics {
        match reported.get(*m) {
            Some(Value::Null) | None if pull.unmetered.contains(m) => {}
            Some(Value::Null) | None => complete = false,
            Some(x) if x.as_i64().is_some_and(|n| n >= 0) => {
                out.insert((*m).to_string(), x.clone());
            }
            Some(_) => return None,
        }
    }
    Some((out, complete))
}

/// Days to look at, newest first: yesterday back to `LOOKBACK_DAYS` ago.
fn window(now: DateTime<Utc>) -> Vec<NaiveDate> {
    let today = now.date_naive();
    (1..=LOOKBACK_DAYS)
        .map(|n| today - Duration::days(n))
        .collect()
}

async fn merge(
    bridge: &Bridge,
    ds: &str,
    day: NaiveDate,
    metrics: Map<String, Value>,
    sources: Vec<String>,
    now: DateTime<Utc>,
    stats: &mut CollectStats,
) {
    if metrics.is_empty() && sources.is_empty() {
        return;
    }
    match bridge
        .store
        .merge_dataset_day(ds, day, &metrics, &sources, now)
        .await
    {
        Ok(()) => stats.merged += 1,
        Err(e) => tracing::warn!(dataset = ds, %day, error = %e, "dataset day not merged"),
    }
}

async fn collect_events(
    bridge: &Bridge,
    ds: &Dataset,
    rows: &[DatasetDay],
    days: &[NaiveDate],
    now: DateTime<Utc>,
    stats: &mut CollectStats,
) {
    // Event types of this dataset and their producers.
    let mut types: Vec<&str> = ds
        .sources
        .iter()
        .filter(|s| s.via != "PULL")
        .map(|s| s.via)
        .collect();
    types.sort_unstable();
    types.dedup();
    for t in types {
        let producers: Vec<Owner> = ds
            .sources
            .iter()
            .filter(|s| s.via == t)
            .map(|s| s.owner)
            .collect();
        // A producer that never sent this type has no stream yet: nothing to count.
        let live = match bridge.store.event_states(t, now).await {
            Ok(v) => v,
            Err(e) => {
                tracing::warn!(event_type = t, error = %e, "event states unavailable");
                return;
            }
        };
        let streaming: Vec<Owner> = producers
            .into_iter()
            .filter(|p| live.iter().any(|s| s.producer == p.as_str()))
            .collect();
        if streaming.is_empty() {
            continue;
        }
        let wanted: Vec<NaiveDate> = days
            .iter()
            .copied()
            .enumerate()
            .filter(|(i, d)| *i == 0 || streaming.iter().any(|p| missing(rows, *d, &source(*p, t))))
            .map(|(_, d)| d)
            .take(MAX_EVENT_DAYS_PER_TICK + 1)
            .collect();
        for day in wanted {
            let end = day_start(day + Duration::days(1));
            let states = match bridge.store.event_states(t, end).await {
                Ok(v) => v,
                Err(e) => {
                    tracing::warn!(event_type = t, error = %e, "event states unavailable");
                    return;
                }
            };
            let mut metrics = Map::new();
            let mut sources = Vec::new();
            let mut sums: Map<String, Value> = Map::new();
            for p in &streaming {
                // The producer's stream had started by the end of this day.
                if !states.iter().any(|s| s.producer == p.as_str()) {
                    continue;
                }
                let Some(values) = event_metrics(ds.id, *p, &states) else {
                    continue;
                };
                for (m, n) in values {
                    if ds.id == "cfmeet.operations_health" {
                        let prev = sums.get(m).and_then(Value::as_i64).unwrap_or(0);
                        sums.insert(m.to_string(), Value::from(prev + n));
                    } else {
                        metrics.insert(m.to_string(), Value::from(n));
                    }
                }
                sources.push(source(*p, t));
            }
            metrics.extend(sums);
            merge(bridge, ds.id, day, metrics, sources, now, stats).await;
        }
    }
}

async fn collect_bridge(
    bridge: &Bridge,
    rows: &[DatasetDay],
    days: &[NaiveDate],
    now: DateTime<Utc>,
    stats: &mut CollectStats,
) {
    let ds = "cfmeet.operations_health";
    let src = source(Owner::AdminBridge, "PULL");
    for (i, day) in days.iter().copied().enumerate() {
        let yesterday = i == 0;
        if !yesterday && !missing(rows, day, &src) {
            continue;
        }
        if !yesterday
            && rows
                .iter()
                .any(|r| r.day == day && r.metrics.contains_key("quarantined_events"))
        {
            continue;
        }
        let start = day_start(day);
        let Ok(quarantined) = bridge
            .store
            .quarantined_between(start, start + Duration::days(1))
            .await
        else {
            return;
        };
        let mut metrics = Map::new();
        metrics.insert("quarantined_events".into(), Value::from(quarantined));
        let mut sources = Vec::new();
        // Open gaps are a current figure: only yesterday's report can carry it.
        if yesterday {
            if let Ok(p) = bridge.store.projections().await {
                let gaps = p
                    .iter()
                    .find(|r| r.name == crate::routes::EVENT_INBOX_PROJECTION)
                    .map_or(0, |r| r.open_gaps);
                metrics.insert("projection_gaps".into(), Value::from(gaps));
                sources.push(src.clone());
            }
        }
        debug_assert!(metrics.keys().all(|k| BRIDGE_METRICS.contains(&k.as_str())));
        merge(bridge, ds, day, metrics, sources, now, stats).await;
    }
}

async fn collect_pulls(
    bridge: &Bridge,
    all: &[(&str, Vec<DatasetDay>)],
    days: &[NaiveDate],
    now: DateTime<Utc>,
    stats: &mut CollectStats,
) {
    // Every pull gets yesterday first, then at most MAX_BACKFILL_PER_PULL missing older
    // days: one source with a long gap never starves the others.
    for pull in PULLS {
        let rows = rows_in(all, pull.dataset);
        let src = source(pull.owner, "PULL");
        let mut backfilled = 0;
        for (i, day) in days.iter().copied().enumerate() {
            if stats.pulls >= MAX_PULLS_PER_TICK {
                return;
            }
            if i != 0 && (!missing(&rows, day, &src) || backfilled >= MAX_BACKFILL_PER_PULL) {
                continue;
            }
            if i != 0 {
                backfilled += 1;
            }
            stats.pulls += 1;
            let path = format!(
                "/internal/v1/operator/datasets/{}/days/{}",
                pull.dataset,
                day.format("%Y-%m-%d")
            );
            let v = match bridge
                .owners
                .read(pull.owner, &path, &[], SYNC_OPERATOR)
                .await
            {
                Ok(v) => v,
                Err(e) => {
                    tracing::debug!(owner = pull.owner.as_str(), %day, error = %e, "dataset pull failed");
                    // An owner that is down for one day is down for the others too.
                    break;
                }
            };
            let Some((metrics, complete)) = pulled(pull, day, &v) else {
                tracing::warn!(owner = pull.owner.as_str(), %day, "dataset answer outside the contract");
                break;
            };
            let sources = if complete {
                vec![src.clone()]
            } else {
                Vec::new()
            };
            merge(bridge, pull.dataset, day, metrics, sources, now, stats).await;
        }
    }
}

fn rows_in(all: &[(&str, Vec<DatasetDay>)], id: &str) -> Vec<DatasetDay> {
    all.iter()
        .find(|(d, _)| *d == id)
        .map(|(_, r)| r.clone())
        .unwrap_or_default()
}

/// One collection pass (called once per maintenance tick).
pub async fn collect(bridge: &Bridge) -> CollectStats {
    let now = (bridge.clock)();
    let days = window(now);
    let (Some(newest), Some(oldest)) = (days.first().copied(), days.last().copied()) else {
        return CollectStats::default();
    };
    let mut stats = CollectStats::default();
    let mut all: Vec<(&str, Vec<DatasetDay>)> = Vec::new();
    for ds in DATASETS {
        match bridge
            .store
            .dataset_days(ds.id, oldest, newest, LOOKBACK_DAYS as u32)
            .await
        {
            Ok(rows) => all.push((ds.id, rows)),
            Err(e) => {
                tracing::warn!(error = %e, "dataset days unavailable: collection skipped");
                return stats;
            }
        }
    }
    for ds in DATASETS {
        collect_events(bridge, ds, &rows_in(&all, ds.id), &days, now, &mut stats).await;
    }
    collect_bridge(
        bridge,
        &rows_in(&all, "cfmeet.operations_health"),
        &days,
        now,
        &mut stats,
    )
    .await;
    collect_pulls(bridge, &all, &days, now, &mut stats).await;
    stats
}
